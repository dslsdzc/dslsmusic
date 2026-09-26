//! 采样率转换：把**源采样率**的样本流搬到**设备采样率**。
//!
//! **位置是硬性的**（设计文档 §6.1 / §16.2）：
//!
//! ```text
//! 解码 → [PcmGraph：在源率上做 DSP] → [本模块：源率 → 设备率] → ring → 音频回调
//! ```
//!
//! - ring 里装的**永远是设备率**样本 ⇒ 音频回调仍然只是 `pop_slice` + 采样格式
//!   转换，也就是 §16.2 要的「memcpy + 转换」。
//! - ⚠️ **绝不要把这一层搬进回调。** 回调线程禁止分配 / 加锁 / IO，而重采样既要
//!   分配又要算 FFT —— 放进去就是把实时路径变重，正是 §16.2 禁止的方向。
//! - ⚠️ **`PcmGraph` 跑在源率上，本模块是「速率边界」而不是链上的一环。**
//!   为什么不让它当 `PcmStage`：见 `lib.rs` 的 `AudioEngine` 文档。
//!
//! # 为什么是 `Fft`，不是别的
//!
//! 依据 rubato 自己的选型指引（`rubato-5.0.0/README.md` 的 "Choosing a resampler"）：
//!
//! - 「把一个音频文件从一个固定采样率换到另一个……用 `Fft`」——
//!   我们的场景正是**整首歌固定比率**（两个采样率在播放期间都不变）。
//! - 「**同步重采样器没有质量档位，它始终给最好的质量**」；而 `Async::new_sinc`
//!   只能「与 `Fft` 的质量**持平**」却更吃 CPU。
//! - ⚠️ 由此否掉了 `Async::new_poly` 与一切多项式 / 线性插值：它们**不做抗混叠**，
//!   那是「最省事」的做法，不是用户裁定的「最好」。
//!
//! # 时长的精确性
//!
//! 重采样器有**启动延迟**（`output_delay()` 个输出帧）。本模块把它**补掉**：
//! 开头丢掉那么多帧，结尾再喂静音把它们挤出来 ⇒
//! **总输出帧数 == `ceil(输入帧数 × 比率)`**，与输入时长一致。
//! 不补的后果是「开头多一小段静音、结尾少一小段音乐」——量很小（本模块默认配置
//! 下约 12 ms），但它是**时长误差**，而这个任务的验收标准恰恰是掐表验时长。

use anyhow::Context;
// ⚠️ 走 rubato 的**再导出**，不把 `audioadapter-buffers` 加成直接依赖。
//    rubato 在根上 `pub use audioadapter_buffers;`（已核实·源码
//    `rubato-5.0.0/src/lib.rs:8`），它自己的公开签名收的就是这个 crate 的类型，
//    所以这条路径是 rubato 公开 API 的一部分，不是内部实现细节。
//    Cargo.toml 里因此仍然只有**新增 rubato 一个**直接依赖。
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

/// 管线的声道数。**与 `decode::decode_to_i16` 的「恰好 2 声道交错」契约绑定**
/// （见那个函数的文档）：ring、音频回调、本模块三处都无条件按 2 声道理解样本。
pub const CHANNELS: usize = 2;

/// 重采样器一次处理的**目标**块长（帧）。
///
/// 太小 ⇒ 抗混叠滤波器的截止频率被压低，高频会滚降（rubato README 的
/// "Resampling quality" 一节）；太大 ⇒ 启动延迟变大。rubato 的建议是
/// 「每块约 100–1000 帧」，1024 落在区间上沿。
///
/// 实际块长不一定等于它：`FixedSync::Both` 下这只是一个**参考值**，会被向上取到
/// 「两个采样率的最大公约数所允许的最近整数倍」。44100→48000 时最小块是
/// 147/160 帧，所以实际块长是 **1029/1120** 帧。这个换算由 rubato 自己做，
/// 我们只读 `input_frames_next()` / `output_frames_max()`，不自己算 ——
/// 自己算就等于把 rubato 的内部取整规则抄一遍，抄错了是静默的。
const CHUNK_FRAMES: usize = 1024;

/// `process_into_buffer` 的失败**不可能发生**：输入缓冲每次按 `input_frames_next()`
/// 精确凑满、输出缓冲按 `output_frames_max()` 一次性分配、声道数两处都写死
/// `CHANNELS`。rubato 只在这三件事对不上时报错，而它们全由本模块自己算出来。
///
/// ⇒ 所以这里用 `expect` 而不是把 `Result` 一路上抛：真的触发就是**实现错误**，
/// 必须响。把它做成 `Result` 再让调用方「记条日志继续」才是错的 ——
/// 那会把「音频流已经错位」变成一个静默的日志。
const INVARIANT: &str = "重采样缓冲尺寸由本模块自己算出，对不上即是实现错误";

/// i16 满量程。**必须是 32768 而不是 `i16::MAX`(32767)** —— 与
/// `decode::push_interleaved_i16` 用同一个系数，两者互为逆变换，所以
/// **i16 → f32 → i16 是逐位精确的**（有测试钉住）。用 32767 会让整条路径
/// 衰减 1 LSB，而且往返不再精确。
const SCALE: f32 = 32768.0;

#[inline]
fn to_f32(s: i16) -> f32 {
    s as f32 / SCALE
}

#[inline]
fn to_i16(x: f32) -> i16 {
    // `.round()` 是必须的：浮点→整数的 `as` 是**截断**，直接用会引入
    // -0.5 LSB 的直流偏置。
    // 不需要额外的 `clamp`：重采样会过冲（Gibbs），而 Rust 的浮点→整数 `as`
    // **本身就是饱和**转换（`push_interleaved_i16` 的注释里已核实过这条）。
    (x * SCALE).round() as i16
}

/// 流式重采样器：源率 i16 样本进，设备率 i16 样本出。
///
/// 有状态：跨 `process` 调用保留**不足一整块的尾巴**与重采样器内部的延迟线。
/// 因此**整首歌必须共用一个实例**，且结尾必须调 [`StreamResampler::flush`]。
pub struct StreamResampler {
    inner: Fft<f32>,
    /// 输入侧交错 f32 暂存（**按样本数**计，不是帧数 —— 见 `process` 的注释）。
    /// 长度在 `[0, input_frames_next() * CHANNELS]` 之间。
    staged: Vec<f32>,
    /// 输出侧交错缓冲，按 `output_frames_max()` **一次分配**。逐次 push 时不再分配
    /// （本模块跑在解码线程上、允许分配，但没有必要每次都分配）。
    out_buf: Vec<f32>,
    /// 启动延迟（输出帧）。开头丢掉这么多帧。
    delay: usize,
    /// 已产出的输出帧总数（含被丢掉的 `delay` 帧）。
    produced: usize,
    /// 已交付给调用方的输出帧总数。
    emitted: usize,
    /// 累计喂进来的**样本**总数（含不足一帧的那半个，见 `flush`）。
    total_samples: usize,
    /// `flush` 只能生效一次（重复调用若再喂静音，会凭空多出一段静音）。
    flushed: bool,
}

impl StreamResampler {
    /// `src_rate` 与 `dst_rate` **必须不同** —— 相同就该走直通，让调用方去建
    /// 直通流，而不是在这里做一次「比率 1.0 的重采样」（那仍是一次滤波，
    /// 白白引入失真与延迟）。
    pub fn new(src_rate: u32, dst_rate: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            src_rate > 0 && dst_rate > 0,
            "采样率必须为正：src={src_rate} dst={dst_rate}"
        );
        anyhow::ensure!(
            src_rate != dst_rate,
            "源率与设备率相同（都是 {src_rate}Hz）时不该建重采样器：请走直通"
        );
        let inner = Fft::<f32>::new(
            src_rate as usize,
            dst_rate as usize,
            CHUNK_FRAMES,
            CHANNELS,
            FixedSync::Both,
        )
        .with_context(|| {
            format!("建重采样器失败（{src_rate}Hz → {dst_rate}Hz）：rubato 拒绝了这组参数")
        })?;
        let delay = inner.output_delay();
        let in_max = inner.input_frames_max() * CHANNELS;
        let out_max = inner.output_frames_max() * CHANNELS;
        Ok(Self {
            staged: Vec::with_capacity(in_max),
            out_buf: vec![0.0; out_max],
            delay,
            produced: 0,
            emitted: 0,
            total_samples: 0,
            flushed: false,
            inner,
        })
    }

    /// 启动延迟（输出帧）。**开头必须丢掉这么多帧**，否则整首歌会晚 `delay`
    /// 个输出帧开始（听感上是「开头多一小段静音」+「结尾少同样长的一段」）。
    pub fn delay_frames(&self) -> usize {
        self.delay
    }

    /// 喂一段**源率**样本（2 声道交错），把产出的**设备率**样本追加上去。
    ///
    /// 暂存是**按样本**而不是按帧记的：这样调用方传进来的长度是奇数时，多出来的
    /// 那半个样本会**留在暂存里**等下一次的右声道，而不会被丢掉。
    /// 丢掉它不只是少一个样本 —— 它后面的样本会被当成 L/R 配对，**此后整条流
    /// 声道互换且不可恢复**（`fill_from_ring` 的注释里记的是同一类坑）。
    pub fn process(&mut self, input: &[i16], out: &mut Vec<i16>) {
        debug_assert!(!self.flushed, "flush 之后不该再喂数据");
        self.total_samples += input.len();
        let mut done = 0usize;
        while done < input.len() {
            let block = self.inner.input_frames_next() * CHANNELS;
            let take = (block - self.staged.len()).min(input.len() - done);
            let base = self.staged.len();
            self.staged.resize(base + take, 0.0);
            for (dst, &src) in self.staged[base..].iter_mut().zip(&input[done..]) {
                *dst = to_f32(src);
            }
            done += take;
            // 凑满一整块才交给重采样器。**不足一块时绝不能补静音** ——
            // 那会在歌中间插进一段空白，而且是每来一个小包就插一次。
            if self.staged.len() == block {
                self.pump(out, usize::MAX);
            }
        }
    }

    /// 排空：把重采样器内部压着的尾巴推出来。
    ///
    /// ⚠️ **整首歌播完必须调它**，否则结尾会被吞掉一截 —— 重采样器内部的延迟线里
    /// 还压着 `delay` 个输出帧，暂存里还可能剩不足一整块的输入。
    ///
    /// 它同时把开头为对齐而丢掉的 `delay` 帧补回来，所以**总输出帧数**
    /// （`process` 产出的 + `flush` 产出的）== `ceil(输入帧数 × 比率)`。
    /// 这正是「掐表验时长」能成立的原因。
    pub fn flush(&mut self, out: &mut Vec<i16>) {
        if self.flushed || self.total_samples == 0 {
            return;
        }
        self.flushed = true;
        // 目标：rubato 自己对「处理完整段」的口径也是这个式子
        // （`process_all_into_buffer` 里 `expected_output_len = ceil(ratio * input_len)`），
        // 用同一个式子才能保证我们和它算出的时长一致。
        // 除以 `CHANNELS` 是整的：不足一帧的那半个样本（若真出现）在这里被舍掉 ——
        // 它是 1/88200 秒，且没有右声道可配，留着也没有意义。
        let frames = self.total_samples / CHANNELS;
        let expected = (self.inner.resample_ratio() * frames as f64).ceil() as usize;
        let block = self.inner.input_frames_next() * CHANNELS;

        // ① 暂存里剩下的真实样本：**文件到此为止**，后面没有数据了，所以补静音是对的。
        //    （与 `process` 中途补静音是两回事，见那里的注释。）
        self.staged.resize(block, 0.0);
        self.pump(out, expected.saturating_sub(self.emitted));

        // ② 一直喂静音，直到交付帧数达到目标。每轮至少产出 `output_frames_next()` 帧，
        //    所以循环必然终止。
        while self.emitted < expected {
            self.staged.clear();
            self.staged.resize(block, 0.0);
            self.pump(out, expected - self.emitted);
        }
    }

    /// 跑**一次**重采样。前置条件：`staged` 恰好装满一整块。
    ///
    /// `cap` 是本次最多交付多少帧：正常播放传 `usize::MAX`；排空的最后一轮用它
    /// 截到精确长度（不截的话末尾会多出不到一块的静音，时长就不准了）。
    fn pump(&mut self, out: &mut Vec<i16>, cap: usize) {
        let block = self.inner.input_frames_next() * CHANNELS;
        debug_assert_eq!(self.staged.len(), block);
        let out_max = self.inner.output_frames_max();
        let (consumed, produced) = {
            let in_view =
                InterleavedSlice::new(&self.staged, CHANNELS, block / CHANNELS).expect(INVARIANT);
            let mut out_view =
                InterleavedSlice::new_mut(&mut self.out_buf, CHANNELS, out_max).expect(INVARIANT);
            self.inner
                .process_into_buffer(&in_view, &mut out_view, None)
                .expect(INVARIANT)
        };
        debug_assert_eq!(consumed * CHANNELS, block, "定长输入模式必须恰好吃掉一整块");
        debug_assert!(produced <= out_max);
        self.staged.clear();

        // 交付规则：**全局第 `delay` 帧之前的一律丢掉**。
        // `emitted` 数的是「已交付多少帧」，它们对应的全局帧号是
        // `[delay, delay + emitted)`，所以下一个要交付的是全局第
        // `delay + emitted` 帧。本次调用产出的是全局 `[before, before + produced)`。
        let before = self.produced;
        self.produced += produced;
        let start = (self.delay + self.emitted)
            .saturating_sub(before)
            .min(produced);
        let end = produced.min(start.saturating_add(cap));
        if end > start {
            let (s, e) = (start * CHANNELS, end * CHANNELS);
            out.reserve(e - s);
            for &x in &self.out_buf[s..e] {
                out.push(to_i16(x));
            }
            self.emitted += end - start;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 交错立体声：生成 `frames` 帧、每帧 `(l, r)`。
    fn interleave(frames: &[(i16, i16)]) -> Vec<i16> {
        let mut v = Vec::with_capacity(frames.len() * 2);
        for &(l, r) in frames {
            v.push(l);
            v.push(r);
        }
        v
    }

    fn sine(frames: usize, rate: u32, freq: f32, amp: f32) -> Vec<i16> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let x = (i as f32 / rate as f32 * freq * std::f32::consts::TAU).sin() * amp;
            let s = x as i16;
            v.push(s);
            v.push(s);
        }
        v
    }

    /// 整段喂进去（不分块），拿全部输出。
    fn resample_all(src: u32, dst: u32, input: &[i16]) -> Vec<i16> {
        let mut rs = StreamResampler::new(src, dst).expect("建重采样器");
        let mut out = Vec::new();
        rs.process(input, &mut out);
        rs.flush(&mut out);
        out
    }

    // ── 换算函数的往返精确性 ──

    /// i16 → f32 → i16 **逐位精确**，全 65536 个取值都验一遍。
    ///
    /// 这条是 `SCALE = 32768.0` 而不是 `32767.0` 的证明：换成后者，
    /// `to_i16(to_f32(s))` 会整体衰减 1 LSB，下面的断言立刻失败。
    /// 它同时保证「直通」与「重采样」两条路的**幅度口径一致** ——
    /// 不一致的话，切歌时音量会跳一下。
    #[test]
    fn i16_f32_round_trip_is_bit_exact_for_every_value() {
        for s in i16::MIN..=i16::MAX {
            assert_eq!(to_i16(to_f32(s)), s, "i16 {s} 往返必须逐位精确");
        }
    }

    /// 过冲必须**饱和**而不是回绕。回绕是音频里最刺耳的 bug 之一。
    #[test]
    fn overshoot_saturates_instead_of_wrapping() {
        assert_eq!(to_i16(1.5), i16::MAX, "+1.5 倍满幅要饱和");
        assert_eq!(to_i16(-1.5), i16::MIN, "-1.5 倍满幅要饱和");
        assert_eq!(to_i16(1.0), i16::MAX);
    }

    // ── ⭐ 本任务的核心回归测试：时长 ──

    /// **44.1kHz 的 42.893 秒素材，在 48kHz 输出上必须还是 42.893 秒。**
    ///
    /// 这是 Task 9 量出来的那个缺陷的直接回归测试：
    /// 不重采样时 42.893 秒会变成 39.4 秒（1.0884 倍速、+1.46 半音）。
    /// 这里不需要声卡、不需要耳朵 —— **帧数就是时长**。
    #[test]
    fn forty_two_second_clip_keeps_its_duration_across_44100_to_48000() {
        let frames = (42.893_f64 * 44100.0).round() as usize; // ≈ 1_891_581 帧
        let input = sine(frames, 44100, 440.0, 8000.0);
        let out = resample_all(44100, 48000, &input);

        let got_frames = out.len() / CHANNELS;
        let want_frames = (frames as f64 * 48000.0 / 44100.0).ceil() as usize;
        let got_secs = got_frames as f64 / 48000.0;

        assert_eq!(
            got_frames, want_frames,
            "输出帧数必须是 ceil(输入帧数 × 48000/44100)"
        );
        assert!(
            (got_secs - 42.893).abs() < 0.001,
            "输出时长必须是 42.893 秒，实测 {got_secs:.4} 秒\
             （不重采样会是 39.4 秒 —— 那正是本任务要修的缺陷）"
        );
    }

    /// 同上，换一组常见的低源率（22050 → 48000，比率 2.177）。
    #[test]
    fn duration_is_preserved_for_22050_to_48000() {
        let frames = 22050 * 3;
        let input = sine(frames, 22050, 440.0, 8000.0);
        let out = resample_all(22050, 48000, &input);
        assert_eq!(
            out.len() / CHANNELS,
            (frames as f64 * 48000.0 / 22050.0).ceil() as usize
        );
    }

    /// 降采样方向（96000 → 44100）也要成立：比率 < 1。
    #[test]
    fn duration_is_preserved_when_downsampling() {
        let frames = 96000 * 2;
        let input = sine(frames, 96000, 440.0, 8000.0);
        let out = resample_all(96000, 44100, &input);
        assert_eq!(
            out.len() / CHANNELS,
            (frames as f64 * 44100.0 / 96000.0).ceil() as usize
        );
    }

    // ── 分块与暂存 ──

    /// **分块喂与整段喂必须逐位一致。** 解码器给的是任意边界的小包
    /// （mp3 一个包 1152 帧），暂存逻辑错了这里立刻红。
    #[test]
    fn chunked_feeding_matches_single_shot_bit_for_bit() {
        let input = sine(5000, 44100, 440.0, 8000.0);
        let want = resample_all(44100, 48000, &input);

        for chunk in [1usize, 3, 7, 100, 1024, 4096] {
            let mut rs = StreamResampler::new(44100, 48000).expect("建重采样器");
            let mut got = Vec::new();
            for c in input.chunks(chunk) {
                rs.process(c, &mut got);
            }
            rs.flush(&mut got);
            assert_eq!(
                got, want,
                "按 {chunk} 个样本一块喂，结果必须与整段喂逐位相同"
            );
        }
    }

    /// **奇数长度**的输入不能把流拧歪：多出来的那半个样本要留到下一次。
    ///
    /// 若实现按帧截断（丢掉那半个），后续所有 L/R 配对整体错位 ——
    /// 听感是声道互换且不可恢复，而指标全正常。
    #[test]
    fn an_odd_length_push_does_not_desynchronize_the_channels() {
        let frames: Vec<(i16, i16)> = (0..200)
            .map(|i| (i as i16 * 100, -(i as i16) * 100))
            .collect();
        let input = interleave(&frames);
        let want = resample_all(44100, 48000, &input);

        // 先推 1 个样本（半个帧），再推剩下的
        let mut rs = StreamResampler::new(44100, 48000).expect("建重采样器");
        let mut got = Vec::new();
        rs.process(&input[..1], &mut got);
        rs.process(&input[1..], &mut got);
        rs.flush(&mut got);
        assert_eq!(got, want, "半个帧必须留在暂存里等它的右声道，不能被丢掉");
    }

    /// `flush` 前拿到的帧数**必须少于**最终帧数 —— 否则说明尾巴是在 `process`
    /// 阶段就出来了，那 `flush` 就是空操作，「必须调 flush」这条约束也就没意义了。
    #[test]
    fn flush_is_what_recovers_the_tail() {
        let frames = 3000;
        let input = sine(frames, 44100, 440.0, 8000.0);
        let mut rs = StreamResampler::new(44100, 48000).expect("建重采样器");
        let mut out = Vec::new();
        rs.process(&input, &mut out);
        let before_flush = out.len() / CHANNELS;
        rs.flush(&mut out);
        let after_flush = out.len() / CHANNELS;

        assert!(
            before_flush < after_flush,
            "flush 必须真的产出样本（{before_flush} → {after_flush}）"
        );
        assert!(
            after_flush - before_flush >= rs.delay_frames() - 2,
            "flush 至少要补回启动延迟那么多帧"
        );
    }

    /// `flush` 必须**幂等**：调两次不能凭空多出一段静音。
    #[test]
    fn flush_twice_does_not_add_extra_silence() {
        let input = sine(2000, 44100, 440.0, 8000.0);
        let mut rs = StreamResampler::new(44100, 48000).expect("建重采样器");
        let mut out = Vec::new();
        rs.process(&input, &mut out);
        rs.flush(&mut out);
        let once = out.len();
        rs.flush(&mut out);
        assert_eq!(out.len(), once, "重复 flush 必须什么都不做");
    }

    // ── 波形本身对不对 ──

    /// 直流信号重采样之后**还是直流**（去掉两端各一小段瞬态）。
    ///
    /// 这条抓的是「接错线」类错误（数据没进重采样器、或输出缓冲没读对）——
    /// 那会让输出全是 0 或全是噪声。
    #[test]
    fn a_dc_signal_stays_dc() {
        let input = interleave(&vec![(1000i16, -1000i16); 8000]);
        let out = resample_all(44100, 48000, &input);
        // 跳过开头/结尾各 1000 帧的滤波器瞬态
        let mid = &out[1000 * 2..(out.len() / 2 - 1000) * 2];
        for (i, s) in mid.iter().enumerate() {
            let want = if i % 2 == 0 { 1000 } else { -1000 };
            assert!(
                (*s - want).abs() <= 1,
                "第 {i} 个样本是 {s}，直流应保持 {want}（±1 LSB）"
            );
        }
    }

    /// **频率必须不变。** 440Hz 的正弦进，440Hz 的正弦出 —— 数过零点。
    ///
    /// 这条与「时长」是同一个缺陷的两面：不重采样时 44.1k 的 440Hz 在 48k 设备上
    /// 会变成 478.9Hz（+1.46 半音）。时长那条测的是「放多久」，这条测的是「音高」。
    #[test]
    fn a_440hz_tone_stays_440hz() {
        let frames = 44100; // 1 秒
        let input = sine(frames, 44100, 440.0, 16000.0);
        let out = resample_all(44100, 48000, &input);
        let mono: Vec<i16> = out.chunks(2).map(|c| c[0]).collect();

        // 数正→负的穿越次数（跳过两端瞬态），去抖：必须真的跨过 -1000 才算
        let mut crossings = 0usize;
        let mut armed = false;
        for &s in &mono[500..mono.len() - 500] {
            if s < -1000 {
                armed = true;
            } else if s > 1000 && armed {
                armed = false;
                crossings += 1;
            }
        }
        // 440Hz × ~0.979 秒有效长度 ≈ 430
        assert!(
            (425..=435).contains(&crossings),
            "440Hz 的 1 秒素材在 48kHz 输出上应仍是 440Hz（约 430 个周期），实测 {crossings}\
             —— 不重采样会数出约 479 个"
        );
    }

    // ── 参数校验与退化输入 ──

    /// 相同采样率**必须报错**，不能悄悄做一次「比率 1.0 的重采样」
    /// （那是一次白白的滤波 + 延迟）。调用方此时应该走直通。
    #[test]
    fn equal_rates_are_rejected() {
        let e = StreamResampler::new(48000, 48000)
            .err()
            .expect("相同采样率必须报错");
        assert!(
            e.to_string().contains("直通"),
            "错误信息要指出正确做法：{e}"
        );
    }

    /// 0 采样率是调用方的实现错误，必须报错而不是拿去做除法/建 FFT。
    #[test]
    fn zero_rate_is_rejected() {
        assert!(StreamResampler::new(0, 48000).is_err());
        assert!(StreamResampler::new(44100, 0).is_err());
    }

    /// 空输入：`flush` 不该产出任何东西，也不该 panic。
    #[test]
    fn empty_input_produces_nothing() {
        let out = resample_all(44100, 48000, &[]);
        assert!(out.is_empty(), "没有输入就没有输出");
    }

    /// 长度**远小于一块**的输入（比 `input_frames_next()` 还短）：
    /// 补静音凑块、再产出完整时长。这是「很短的音效」那条路。
    #[test]
    fn input_shorter_than_one_block_still_yields_the_right_duration() {
        let frames = 50;
        let input = sine(frames, 44100, 440.0, 8000.0);
        let out = resample_all(44100, 48000, &input);
        let want = (frames as f64 * 48000.0 / 44100.0).ceil() as usize;
        assert_eq!(out.len() / CHANNELS, want);
    }

    /// 两个采样率的**最大公约数是 1** 的极端组合（44100 → 44101）。
    ///
    /// 这时 FFT 块长会被撑到 44100 帧（rubato 的最小块 = `rate / gcd`），
    /// 延迟约 0.5 秒 —— 慢，但**必须仍然正确**。这条钉住的是「我们没有给
    /// 任何采样率组合留一个会静默出错的洞」。
    #[test]
    fn a_coprime_rate_pair_still_works() {
        let frames = 300;
        let input = sine(frames, 44100, 440.0, 8000.0);
        let out = resample_all(44100, 44101, &input);
        assert_eq!(
            out.len() / CHANNELS,
            (frames as f64 * 44101.0 / 44100.0).ceil() as usize,
            "互素的采样率对也必须给出正确时长"
        );
    }
}
