use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// 无锁 SPSC 环形缓冲，用 `AudioRing::split()` 拆成生产端与消费端。
///
/// **消费端在音频回调线程里使用：无锁、无分配、无 IO**（设计文档 §16.2）。
/// 原计划用 `Mutex<Vec<i16>>`，那会在实时线程里加锁 —— 已废弃。
///
/// # 帧对齐不变量（**丢样本时左右声道会永久互换**）
///
/// 环里承载的是**2 声道交错**的立体声：`L,R,L,R,…`。样本被**丢弃**时，只要丢掉的
/// 个数是**奇数**，此后所有样本的奇偶位置就翻一位 —— 每个「帧」都变成 `(R,L)`，
/// **左右声道永久互换**，而采样率、`xrun`、波形统计**全部照常**。
/// （本项目早先修过的 `got / 2` 截断是同一个坑，只是长在消费端。）
///
/// 所以本类型的硬约束是：**`push_samples` 每次调用丢掉的样本数必须是偶数。**
///
/// 两侧各守一半：
/// - 消费端 `fill_from_ring`：把 pop 长度**向下取到偶数**，那半个样本留在环里
///   （见 `lib.rs` 里它的注释）。环里因此**允许**出现奇数个样本。
/// - 生产端 [`AudioRingProducer::push_samples`]：对**丢弃数**取偶。注意不是对
///   空闲空间取偶 —— `data` 的长度**允许是奇数**（见那个方法的文档），
///   只在偶数长度时两者才等价。
pub struct AudioRing {
    capacity: usize,
}

impl AudioRing {
    /// `capacity` 是**样本个数**（不是字节数、不是帧数）。
    ///
    /// # Panics
    ///
    /// `capacity == 0` 时 panic（ringbuf 的既定行为，已核实源码
    /// `ringbuf-0.4.8/src/rb/macros.rs:20`）。
    pub fn new(capacity: usize) -> Self {
        Self { capacity }
    }

    pub fn split(self) -> (AudioRingProducer, AudioRingConsumer) {
        let (prod, cons) = HeapRb::<i16>::new(self.capacity).split();
        let overflows = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(AtomicU64::new(0));
        (
            AudioRingProducer {
                inner: prod,
                overflows: Arc::clone(&overflows),
                underruns: Arc::clone(&underruns),
                carry: None,
            },
            AudioRingConsumer {
                inner: cons,
                underruns,
            },
        )
    }
}

/// 生产端 —— 解码线程使用。`!Sync` 但 `Send`。
pub struct AudioRingProducer {
    inner: HeapProd<i16>,
    overflows: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    /// 上一次推送时**扣下**的那一个样本（只有环满才可能发生，见 `push_samples`）。
    /// 它下次排在最前面推出去，所以**流序不变**；最多积压 1 个，不会随调用增长。
    ///
    /// 它不在环里，因此 `available()` 数不到它 —— 这是「环内可读样本数」，不是
    /// 「还有多少样本没进环」，两者的差额最多就是这 1 个。
    carry: Option<i16>,
}

impl AudioRingProducer {
    /// 推入样本。**环满时新数据被丢弃并计一次溢出**，绝不阻塞、绝不分配。
    ///
    /// ⚠️ **本方法每次调用丢掉的样本数恒为偶数** —— 丢奇数个会**永久翻转
    /// 左右声道**，理由见 [`AudioRing`] 的文档。这是本方法的唯一硬约束。
    ///
    /// # `data` 的长度允许是**奇数**
    ///
    /// 调用方按任意边界切分同一个立体声流是合法的：先推 `[L0,R0,L1]`、
    /// 再推 `[R1,L2,R2]`，那半个帧留在环里等它的另一半。这条不是本方法的宽容，
    /// 是**既有约定**：消费端 `fill_from_ring`（`lib.rs`）、
    /// [`crate::resample::StreamResampler::process`] 的文档都写明了它，
    /// `tests/ring_phase.rs` 也正是靠「先推 7 个」造出奇数占用来钉这条不变量的。
    ///
    /// 所以「把空闲空间向下取到偶数」**只在 `data` 为偶数长度时够用**
    /// （空闲 4、推 7 个，取偶后仍会丢掉 3 个 = 奇数）。这里对**丢弃数**取偶，
    /// 它对两种长度都成立。代价是**最多浪费 1 个空闲槽位** ——
    /// 环容量是「采样率 × 声道数 × 秒数」（`lib.rs` 的 `RING_SECONDS`），
    /// 上万个样本里少用 1 个可以忽略。
    pub fn push_samples(&mut self, data: &[i16]) {
        // 概念上要推的是 `[carry] ++ data`：`carry` 是上次扣下的样本，它在前。
        let lead = self.carry;
        let total = data.len() + usize::from(lead.is_some());
        let room = self.inner.vacant_len();
        let mut take = total.min(room);
        let mut dropped = total - take;

        if dropped % 2 == 1 {
            if take > 0 {
                // 少收一个 ⇒ 多丢一个 ⇒ 丢弃数变偶。
                take -= 1;
                dropped += 1;
            } else {
                // 环**满**（`room == 0`）而 `total` 是奇数：一个样本都收不下。
                // 整段丢掉会让丢弃数变成奇数 ⇒ **扣下 1 个**，丢弃数因此是 `total - 1`（偶）。
                //
                // 扣下的必须是**最后一个**样本（`lead` 已在手上时就是它本人，不动）：
                // 这样丢掉的是一段**连续前缀**，而扣下的那个下次排在最前面推出去，
                // 与后面的流依然连续 —— 换个扣法就会在流中间挖掉一个样本。
                if lead.is_none() {
                    self.carry = data.last().copied();
                }
                dropped -= 1;
            }
        }
        debug_assert_eq!(
            dropped % 2,
            0,
            "丢弃数必须是偶数：丢奇数个会永久翻转左右声道（见 AudioRing 的文档）"
        );

        // `lead` 先走：它在流里排在 `data` 前面，且 `take >= 1` 时必定推得进去
        // （`take <= room`）。`take == 0` 时它必须**留在** `carry` 里，不能清掉。
        // （不写 let-chain：本仓 MSRV 是 1.87，let-chain 要 1.88。）
        let mut pushed = 0usize;
        if take >= 1 {
            if let Some(c) = lead {
                pushed = self.inner.push_slice(&[c]);
                self.carry = None;
            }
        }
        let from_data = take - pushed;
        if from_data > 0 {
            pushed += self.inner.push_slice(&data[..from_data]);
        }
        debug_assert_eq!(pushed, take, "定长推送：能收多少就收多少");

        if dropped > 0 {
            self.overflows.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 当前环内可读样本数（供背压判断）。
    ///
    /// ⚠️ 它数的是**环里的**样本，不含 `carry` 里可能扣着的那 1 个 ——
    /// 「还有多少样本没进环」最多比它大 1（见那个字段的文档）。
    /// 背压按它算是对的：扣着的那个样本本来就没打算立刻进环。
    pub fn available(&self) -> usize {
        self.inner.occupied_len()
    }

    pub fn overflows(&self) -> u64 {
        self.overflows.load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

/// 消费端 —— **音频回调线程使用**。`!Sync` 但 `Send`。
pub struct AudioRingConsumer {
    inner: HeapCons<i16>,
    underruns: Arc<AtomicU64>,
}

impl AudioRingConsumer {
    /// 取样本填满 `out`。**取不满时尾部补静音并计一次 underrun**。
    /// 返回实际取到的样本数。永不阻塞、永不分配。
    pub fn pop_samples(&mut self, out: &mut [i16]) -> usize {
        let n = self.inner.pop_slice(out);
        if n < out.len() {
            out[n..].fill(0);
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
        n
    }

    /// **不补静音、不计数**的原始取值。永不阻塞、永不分配。
    ///
    /// 音频回调用它，而不用 `pop_samples`——因为**静音必须补在目标采样格式
    /// `T` 的域里**：`f32` 的静音是 `0.0`、`i16` 是 `0`、**`u8` 是 `128`**。
    /// 在 i16 域里补 0 再转换是对的，但直接往 `T` 上写 `0` 在 u8 设备上
    /// 是满幅直流噪声。回调侧的补静音见 `fill_from_ring` 的 `T::EQUILIBRIUM`。
    pub fn pop_raw(&mut self, out: &mut [i16]) -> usize {
        self.inner.pop_slice(out)
    }

    /// 当前环内可读样本数。**无锁**（两次原子读 + 本地缓存），回调里可用。
    ///
    /// 回调靠它把 pop 长度**向下取到偶数**（整帧立体声），见 `fill_from_ring`：
    /// 环里可能装着奇数个样本（push 端按任意边界切分同一个立体声流是合法的），
    /// 那半个样本必须留在环里，不能取出来丢掉。
    pub fn available(&self) -> usize {
        self.inner.occupied_len()
    }

    /// 记一次欠载。**按「事件」计，不按缺失样本数计**——与 `pop_samples`
    /// 的口径一致，这样「xrun 必须为 0」这条硬指标才有可比性。
    pub fn count_underruns(&self, n: u64) {
        self.underruns.fetch_add(n, Ordering::Relaxed);
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_order() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3]);
        let mut out = [0i16; 3];
        assert_eq!(cons.pop_samples(&mut out), 3);
        assert_eq!(out, [1, 2, 3]);
    }

    #[test]
    fn counts_underrun_when_empty() {
        let ring = AudioRing::new(8);
        let (_prod, mut cons) = ring.split();
        // ⚠️ 预填 99、**不是** 0：`out` 若初值就是 0，下面那条断言**恒真**
        //    （不管 `pop_samples` 有没有补静音都通过），等于没测。
        //    这是「空环」那一路；「取到一部分」那一路在
        //    `partial_underrun_still_fills_silence`。
        let mut out = [99i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 0);
        assert_eq!(cons.underruns(), 1);
        // 回调必须填满整块缓冲 —— 没数据时填静音，绝不能留垃圾值（那是爆音）
        assert_eq!(out, [0i16; 4]);
    }

    #[test]
    fn counts_overflow_and_keeps_oldest() {
        // ringbuf 的语义：环满之后**新数据被丢弃**（不是挤掉最旧的）
        let ring = AudioRing::new(4);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(prod.overflows(), 1, "6 个样本推进容量 4 的环，应记一次溢出");
        let mut out = [0i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 4);
        assert_eq!(out, [1, 2, 3, 4], "ringbuf 保留先到的数据");
    }

    /// 构造交错立体声流：**L 恒为非负、R 恒为负**，于是帧对齐可以逐帧断言。
    /// 与 `tests/ring_phase.rs` 里的同名函数是同一个构造。
    fn stereo(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| if i % 2 == 0 { i as i16 } else { -(i as i16) })
            .collect()
    }

    /// 把环里成对的样本抽干、追加到 `out`。**丢奇数个样本就会在这里露馅**：
    /// 此后每对的第一个样本都会变成负的。
    fn drain(c: &mut AudioRingConsumer, out: &mut Vec<i16>) {
        let mut buf = [0i16; 2];
        while c.available() >= 2 {
            c.pop_samples(&mut buf);
            out.extend_from_slice(&buf);
        }
    }

    /// **奇数长度的推送被部分丢弃时，丢弃数也必须是偶数。**
    ///
    /// `tests/ring_phase.rs` 钉的是**偶数长度**推送那一路（那时「把空闲空间向下
    /// 取到偶数」就够）。这一路钉的是奇数长度：空闲 4（偶）、推 7 个，
    /// 只看空闲空间会丢 3 个（奇）⇒ 环里剩下的样本与下一批之间错开一个样本。
    #[test]
    fn odd_length_push_never_drops_an_odd_count() {
        let src = stereo(64);
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();

        prod.push_samples(&src[0..4]); // 先占住 4 个（2 整帧）
        prod.push_samples(&src[4..11]); // 长度 7（奇）：空闲 4 ⇒ 只能收 3 个
        assert_eq!(prod.overflows(), 1, "前提：这一次应记一次溢出");
        assert_eq!(prod.available(), 7, "收 3 个、丢 4 个（偶）才对");

        drain(&mut cons, &mut heard);
        for chunk in src[11..].chunks(4) {
            prod.push_samples(chunk);
            drain(&mut cons, &mut heard);
        }
        drain(&mut cons, &mut heard);

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        for (k, pair) in heard.chunks_exact(2).enumerate() {
            assert!(
                pair[0] >= 0 && pair[1] < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。\
                 说明有一次奇数长度的丢弃把 L/R 翻了位",
                pair[0],
                pair[1]
            );
        }
    }

    /// **环满时推奇数个样本**：整段丢掉就是奇数个 ⇒ 同样会翻转声道。
    /// 这里一个样本都收不下，所以只能**扣下**最后一个留到下次（丢弃数因此为偶）。
    #[test]
    fn full_ring_defers_one_sample_instead_of_flipping_channels() {
        let src = stereo(64);
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();

        prod.push_samples(&src[0..8]); // 8 = 容量，环满
        assert_eq!(prod.available(), 8, "前提：环应被占满");
        prod.push_samples(&src[8..11]); // 长度 3（奇），一个都进不去
        assert_eq!(prod.overflows(), 1, "前提：这一次应记一次溢出");
        assert_eq!(prod.available(), 8, "环满时占用数不该变");

        drain(&mut cons, &mut heard); // 听掉这 4 帧，环空出来
        for chunk in src[11..].chunks(4) {
            prod.push_samples(chunk);
            drain(&mut cons, &mut heard);
        }
        drain(&mut cons, &mut heard);

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        for (k, pair) in heard.chunks_exact(2).enumerate() {
            assert!(
                pair[0] >= 0 && pair[1] < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。\
                 环满时丢奇数个样本把 L/R 翻了位",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn partial_underrun_still_fills_silence() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[7, 8]);
        let mut out = [99i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 2);
        assert_eq!(out, [7, 8, 0, 0], "尾部必须补静音，不能保留 out 原值");
        assert_eq!(cons.underruns(), 1, "取不满也算一次 underrun");
    }
}
