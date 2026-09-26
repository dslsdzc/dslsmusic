use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::conv::IntoSample;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// 解码为**恰好 2 声道交错**的 i16（重采样留给后续 DSP 阶段）。
///
/// ⚠️ **契约：输出恰好 2 声道交错。** 下游 [`crate::output`] 与
/// `fill_from_ring` **无条件**把相邻两个样本当成一帧的 L/R，所以声道数必须
/// 在这里归一化。早先只写了「立体声」而代码按**源**声道数摊平 —— 于是 mono
/// 源会被下游当成 L/R 两样本 ⇒ **2 倍速 + 升调**（6ch 源 3 倍速），
/// **而 xrun 恒 0、不报错**。这类「听起来不对但指标全正常」的 bug 最难查，
/// 所以它是**契约**，不是实现细节。
///
/// 归一则：
/// - **1 声道 → 复制到 L/R**（无损、平凡正确）
/// - **2 声道 → 直通**
/// - **> 2 声道 → 取前两个声道**（⚠️ **朴素做法，不是 BS.775 降混** ——
///   那个系数表我们拿不到，**不许编系数**）
///
/// 发生复制/降混时记一条 `log::warn!`，把源声道数打出来 —— 否则用户只会觉得
/// 「这首歌听起来怪怪的」而没有任何线索。日志在**解码线程**上，不在音频回调里
/// （§16.2 只约束回调线程）。整轨只记一次，不是每个 packet 一条。
///
/// 回调取 `&mut [i16]` 而**不是** `&[i16]` —— 因为 PCM graph 的 DSP
/// 要**原地**处理样本（设计文档 §6.1）。
pub fn decode_to_i16(
    stream: Box<dyn symphonia::core::io::MediaSource>,
    ext_hint: Option<&str>,
    on_samples: &mut dyn FnMut(&mut [i16]),
) -> anyhow::Result<u32> {
    let mss = MediaSourceStream::new(stream, Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext_hint {
        hint.with_extension(e);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("文件中没有可解码的音轨"))?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut scratch: Vec<i16> = Vec::new();
    // 声道归一化的告警整轨只打一次（不是每个 packet 一条，否则 4 分钟的歌
    // 会刷出几千行同样的日志）。
    let mut warned_remix = false;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet)?;
        scratch.clear();
        push_interleaved_i16(&decoded, &mut scratch, &mut warned_remix);
        on_samples(&mut scratch);
    }
    Ok(sample_rate)
}

/// 把任意采样格式的解码结果摊成**恰好 2 声道交错的 i16**（见 `decode_to_i16`
/// 的契约）。`warned_remix` 是「整轨只告警一次」的开关，由调用方跨 packet 持有。
///
/// ⚠️ 这里**不能**照抄「所有格式都当浮点处理」的写法：`AudioBufferRef`
/// 的 10 个变体里有一半的样本类型是整数（`i16`/`i32`/`u8`/`i24`…），
/// 对它们写 `.clamp(-1.0, 1.0)` 是类型错误。统一先 `into_sample::<f32>()`
/// 归一化到 `[-1, 1]`，再乘满量程。
fn push_interleaved_i16(buf: &AudioBufferRef<'_>, out: &mut Vec<i16>, warned_remix: &mut bool) {
    macro_rules! conv {
        ($b:expr) => {{
            let src_channels = $b.spec().channels.count();
            let frames = $b.frames();
            // ⚠️ 0 声道**不需要**特判分支（加了也测不到，是死代码）：带容量的
            //    0 声道 `AudioBuffer` 构造不出来 —— `AudioBuffer::new` 会在
            //    `duration / channels.count()` 上除零 panic，`render_reserved`
            //    又被 `n_frames + n <= n_capacity == 0` 卡住。所以 0 声道的
            //    buffer 只可能是 `AudioBuffer::unused()`：`frames()` 必为 0、
            //    `chan(0)` 返回空切片（已核实·源码 symphonia-core-0.5.5/
            //    src/audio.rs:586-593 的断言在 `n_capacity == 0` 时成立），
            //    下面的循环一次都不跑 ⇒ 不产出样本、也不越界。
            // 1 声道：右声道读同一个平面 ⇒ 复制到 L/R。>2 声道：只看前两个平面。
            let left = $b.chan(0);
            let right = $b.chan(if src_channels > 1 { 1 } else { 0 });
            // 只有「复制」（1 声道）与「丢声道」（>2 声道）才告警。2 声道直通、
            // 0 声道没有任何样本可摊 —— 给它们报「已降混」是假话，只会让
            // 「这首歌听起来不对」的线索变成噪音。
            if src_channels != 2 && src_channels != 0 && !*warned_remix {
                *warned_remix = true;
                if src_channels == 1 {
                    log::warn!("源音频是 1 声道：已复制到 L/R（下游按 2 声道交错消费）");
                } else {
                    log::warn!(
                        "源音频是 {src_channels} 声道：只取前两个声道（朴素做法，\
                         不是 BS.775 降混）"
                    );
                }
            }
            out.reserve(frames * 2);
            for f in 0..frames {
                let (l, r): (f32, f32) = (left[f].into_sample(), right[f].into_sample());
                // ⚠️ 乘 **32768.0**，不是 `i16::MAX`(32767)。乘 32767 会让整条
                //    路径**衰减 1 LSB**（1000→999、32767→32766、−32768→−32767）；
                //    乘 32768.0 之后 i16 源**逐位精确**，且**不需要**额外的正端
                //    饱和：`(1.0 * 32768.0) as i16 == 32767`（Rust 的浮点→整数
                //    `as` 是**饱和**转换，symphonia 自己也依赖这条）。
                out.push((l.clamp(-1.0, 1.0) * 32768.0) as i16);
                out.push((r.clamp(-1.0, 1.0) * 32768.0) as i16);
            }
        }};
    }
    match buf {
        AudioBufferRef::U8(b) => conv!(b),
        AudioBufferRef::U16(b) => conv!(b),
        AudioBufferRef::U24(b) => conv!(b),
        AudioBufferRef::U32(b) => conv!(b),
        // ⚠️ `S8` 必须留着：`AudioBufferRef` 一共 10 个变体，漏一个就不是穷尽 match。
        AudioBufferRef::S8(b) => conv!(b),
        AudioBufferRef::S16(b) => conv!(b),
        AudioBufferRef::S24(b) => conv!(b),
        AudioBufferRef::S32(b) => conv!(b),
        AudioBufferRef::F32(b) => conv!(b),
        AudioBufferRef::F64(b) => conv!(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 以下测试是**实现时补的**，brief 的 Step 1 里没有。 ──
    //
    // brief 自己就点明了「`pcm` feature 掉了 WAV 会**静默失效**」。
    // 静默失效意味着没有测试就永远发现不了，所以这里**手搓 WAV 字节**
    // 做端到端解码验证，不依赖任何外部音频文件。

    /// 手搓一个最小 WAV（RIFF / 格式 1 = PCM）。
    fn wav_pcm(bits: u16, channels: u16, sample_rate: u32, data: &[u8]) -> Vec<u8> {
        let block_align = channels * bits / 8;
        let byte_rate = sample_rate * block_align as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    fn decode_bytes(bytes: Vec<u8>, hint: Option<&str>) -> anyhow::Result<(u32, Vec<i16>)> {
        let mut got = Vec::new();
        let rate = decode_to_i16(
            Box::new(std::io::Cursor::new(bytes)),
            hint,
            &mut |s: &mut [i16]| got.extend_from_slice(s),
        )?;
        Ok((rate, got))
    }

    #[test]
    fn decodes_wav_pcm16_keeping_rate_order_and_scale() {
        let frames: [(i16, i16); 4] =
            [(1000, -1000), (2000, -2000), (3000, -3000), (32767, -32768)];
        let mut data = Vec::new();
        for (l, r) in frames {
            data.extend_from_slice(&l.to_le_bytes());
            data.extend_from_slice(&r.to_le_bytes());
        }
        let (rate, got) =
            decode_bytes(wav_pcm(16, 2, 44100, &data), Some("wav")).expect("解码失败");

        assert_eq!(rate, 44100, "采样率要从容器里读出来");
        assert_eq!(got.len(), 8, "4 帧 × 2 声道，且必须是交错的");
        // **逐位精确**（不是「±1 LSB」）：i16 → f32 → ×32768.0 → i16 是无损回环。
        // 乘 32767（`i16::MAX`）会让整条路径衰减 1 LSB，那时这里会失败 —— 这正是
        // 这条断言存在的意义。它同时覆盖了「缩放系数写错」（例如把 [-1,1] 当 i16
        // 直接存）：那会让 32767 变成 1 之类的值，而不是等值回环。
        for (i, (l, r)) in frames.iter().enumerate() {
            assert_eq!(
                &got[i * 2..i * 2 + 2],
                &[*l, *r],
                "第 {i} 帧必须逐位精确回环（i16→f32→i16）"
            );
        }
    }

    /// **mono 源**必须被归一化成 2 声道交错，且样本**在 L/R 上各出现一次**。
    ///
    /// 只断长度是抓不到这个 bug 的：按源声道数摊平同样能让 `len() == 4` 成立，
    /// 而下游 `fill_from_ring` 会把 `[s0, s1]` 当成一帧的 L/R ⇒ 2 倍速 + 升调。
    #[test]
    fn mono_source_is_copied_to_both_channels_not_passed_through() {
        let data: [i16; 4] = [1000, -2000, 3000, -32768];
        let mut bytes = Vec::new();
        for s in data {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let (rate, got) =
            decode_bytes(wav_pcm(16, 1, 22050, &bytes), Some("wav")).expect("解码失败");

        assert_eq!(rate, 22050);
        assert_eq!(got.len(), 8, "4 帧 mono ⇒ 4 帧立体声（8 个样本）");
        for (i, s) in data.iter().enumerate() {
            assert_eq!(
                &got[i * 2..i * 2 + 2],
                &[*s, *s],
                "mono 第 {i} 帧必须复制到 L 和 R；原样透传会让下游按 L/R 两样本消费"
            );
        }
    }

    /// **> 2 声道源**取前两个声道（⚠️ 不是 BS.775 降混，系数表拿不到、不编系数）。
    #[test]
    fn more_than_two_channels_keeps_only_the_first_two() {
        // 4 声道 × 2 帧，每帧顺序是 (L, R, C, LFE)
        let frames: [[i16; 4]; 2] = [[100, 200, 300, 400], [500, 600, 700, 800]];
        let mut data = Vec::new();
        for fr in frames {
            for s in fr {
                data.extend_from_slice(&s.to_le_bytes());
            }
        }
        let (_, got) = decode_bytes(wav_pcm(16, 4, 8000, &data), Some("wav")).expect("解码失败");

        assert_eq!(
            got,
            [100, 200, 500, 600],
            "4 声道必须只取前两个声道，摊成 2 声道交错（第 3/4 声道被丢掉）"
        );
    }

    /// 0 声道 / 0 帧的退化输入（`AudioBuffer::unused()`，解码器不会产生它）：
    /// 不 panic、不产出样本，也**不该**报「已降混」——那是假话。
    ///
    /// 这条钉住的是 `push_interleaved_i16` 里 `src_channels != 0` 那个条件：
    /// 若有人把 0 声道也归到「非 2 声道 ⇒ 告警」，日志会开始说
    /// 「0 声道已取前两个声道」。
    #[test]
    fn zero_channel_buffer_is_a_no_op_not_a_panic() {
        use symphonia::core::audio::AudioBuffer;
        let unused: AudioBuffer<i16> = AudioBuffer::unused();
        let buf = AudioBufferRef::S16(std::borrow::Cow::Borrowed(&unused));
        assert_eq!(
            buf.spec().channels.count(),
            0,
            "前提：这个 buffer 是 0 声道"
        );
        assert_eq!(buf.frames(), 0, "前提：它也是 0 帧");

        let mut got = Vec::new();
        let mut warned = false;
        push_interleaved_i16(&buf, &mut got, &mut warned);
        assert!(got.is_empty(), "0 声道不该产出任何样本");
        assert!(!warned, "0 声道不是降混/复制，不该报降混告警");
    }

    #[test]
    fn decodes_wav_pcm_u8_in_the_integer_domain() {
        // 8 位 WAV 是**无符号偏移二进制**：128 是静音、0 是负满幅、255 是正满幅。
        // 这一条专门盯住「把整数样本当浮点处理」的写法：那样 128 会变成 -1.0，
        // 也就是把静音解成**满幅直流**。
        //
        // 这个源是 **mono**：样本排列（复制到 L/R）由
        // `mono_source_is_copied_to_both_channels_not_passed_through` 专门钉住，
        // 这里顺带确认一遍。
        let data = [0u8, 128, 255, 128];
        // hint 传 None：容器识别必须靠 RIFF 头自己完成
        let (rate, got) = decode_bytes(wav_pcm(8, 1, 8000, &data), None).expect("解码失败");

        assert_eq!(rate, 8000);
        assert_eq!(got.len(), 8, "4 帧 mono ⇒ 4 帧立体声");
        // 回调整体在 i16 域里补静音，靠 `from_sample` 把它变成各格式的静音值。
        // 这一步在 u8 上必须是 128 而不是 0，否则补的仍是满幅直流。
        use cpal::Sample as _;
        assert_eq!(
            u8::from_sample(0i16),
            128,
            "i16 域的 0 在 u8 上应映射成 128"
        );
        // 满幅两端。u8 的 0 与 255 分别对应 i16 域的 -32768 与 32512
        // （0 → (0/128)-1 = -1.0 → ×32768；255 → (255/128)-1 = 0.9921875 → ×32768）。
        assert_eq!(&got[..2], &[-32768, -32768], "u8 的 0 是负满幅");
        assert_eq!(&got[4..6], &[32512, 32512], "u8 的 255 是正满幅");
        // 静音两侧都要看：只查一个下标会漏掉「L/R 有一路没被复制」。
        assert_eq!(&got[2..4], &[0, 0], "u8 的 128 是静音，两路都必须是 0");
        assert_eq!(&got[6..8], &[0, 0]);
    }

    #[test]
    fn non_audio_input_is_an_error_not_a_panic() {
        let mut got = Vec::new();
        let r = decode_to_i16(
            Box::new(std::io::Cursor::new(
                b"this is definitely not audio".to_vec(),
            )),
            None,
            &mut |s: &mut [i16]| got.extend_from_slice(s),
        );
        assert!(
            r.is_err(),
            "非音频输入必须返回 Err，不能 panic 也不能谎报成功"
        );
        assert!(got.is_empty(), "失败时不该吐出任何样本");
    }
}
