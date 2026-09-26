use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::conv::IntoSample;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// 解码为交错的 i16 立体声（重采样留给后续 DSP 阶段）。
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
        push_interleaved_i16(&decoded, &mut scratch);
        on_samples(&mut scratch);
    }
    Ok(sample_rate)
}

/// 把任意采样格式的解码结果摊成**交错的 i16**。
///
/// ⚠️ 这里**不能**照抄「所有格式都当浮点处理」的写法：`AudioBufferRef`
/// 的 10 个变体里有一半的样本类型是整数（`i16`/`i32`/`u8`/`i24`…），
/// 对它们写 `.clamp(-1.0, 1.0)` 是类型错误。统一先 `into_sample::<f32>()`
/// 归一化到 `[-1, 1]`，再乘满量程。
fn push_interleaved_i16(buf: &AudioBufferRef<'_>, out: &mut Vec<i16>) {
    macro_rules! conv {
        ($b:expr) => {{
            let channels = $b.spec().channels.count();
            let frames = $b.frames();
            out.reserve(frames * channels);
            for f in 0..frames {
                for c in 0..channels {
                    let v: f32 = $b.chan(c)[f].into_sample();
                    out.push((v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
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
        for (i, (l, r)) in frames.iter().enumerate() {
            let (dl, dr) = (
                got[i * 2] as i32 - *l as i32,
                got[i * 2 + 1] as i32 - *r as i32,
            );
            assert!(
                dl.abs() <= 1 && dr.abs() <= 1,
                "第 {i} 帧回环误差 ({dl}, {dr}) 超过 1 LSB（i16→f32→i16 的固有衰减）：{:?}",
                &got[i * 2..i * 2 + 2]
            );
        }
        // 满量程必须映射到满量程附近 —— 缩放系数写错（例如把 [-1,1] 当 i16 直接存）
        // 会在这里暴露，而不是等到用户听见声音小得像蚊子叫。
        assert!(got[6] > 32000, "正满幅得到 {}", got[6]);
        assert!(got[7] < -32000, "负满幅得到 {}", got[7]);
    }

    #[test]
    fn decodes_wav_pcm_u8_in_the_integer_domain() {
        // 8 位 WAV 是**无符号偏移二进制**：128 是静音、0 是负满幅、255 是正满幅。
        // 这一条专门盯住「把整数样本当浮点处理」的写法：那样 128 会变成 -1.0，
        // 也就是把静音解成**满幅直流**。
        let data = [0u8, 128, 255, 128];
        // hint 传 None：容器识别必须靠 RIFF 头自己完成
        let (rate, got) = decode_bytes(wav_pcm(8, 1, 8000, &data), None).expect("解码失败");

        assert_eq!(rate, 8000);
        assert_eq!(got.len(), 4);
        assert_eq!(got[1], 0, "u8 的 128 是静音，必须解成 0");
        assert_eq!(got[3], 0);
        // 回调整体在 i16 域里补静音，靠 `from_sample` 把它变成各格式的静音值。
        // 这一步在 u8 上必须是 128 而不是 0，否则补的仍是满幅直流。
        use cpal::Sample as _;
        assert_eq!(
            u8::from_sample(0i16),
            128,
            "i16 域的 0 在 u8 上应映射成 128"
        );
        // 满幅两端。u8 的 0 与 255 分别对应 i16 域的 -32767 与 32511
        // （0 → (0/128)-1 = -1.0 → ×32767；255 → (255/128)-1 = 0.9921875 → ×32767）。
        assert_eq!(got[0], -32767, "u8 的 0 是负满幅");
        assert_eq!(got[2], 32511, "u8 的 255 是正满幅");
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
