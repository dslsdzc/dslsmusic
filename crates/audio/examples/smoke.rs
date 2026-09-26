//! 手动冒烟程序：**在真实设备上**验证三件事。
//!
//! 1. 建流成功、回调真的在取数（`ring_available` 下降）
//! 2. 环被抽干后 `xrun` 从 0 开始上涨（指标是活的）
//! 3. **时长对得上**：喂进去 N 秒源率样本，环里就该出 N 秒
//!
//! 它不是自动化测试（机器上没声卡时会失败），所以刻意放在 `examples/` 而不是
//! `#[test]` 里。跑法：
//!
//! ```text
//! cargo run -p dsls-audio --example smoke
//! DSLS_SMOKE_RATE=500000 cargo run -p dsls-audio --example smoke   # 强制走重采样
//! ```
//!
//! ⚠️ 它**不能**证明「声音本身是对的」——那需要回环采集，见 Task 9。
//! 它验的是**时长**（客观、可复现、不依赖听感），以及把设备**实际报出来的能力**
//! 打出来 —— `AudioEngine::new` 的选择就是在这张表上做的，表打出来，选择才可复核。
//!
//! ## 为什么要能指定源采样率
//!
//! 本机（PipeWire）的设备声称支持 **1..384000Hz 的任意配置**，于是**永远**走直通
//! 那条路 —— 重采样分支在 CLI 上根本跑不到。把 `DSLS_SMOKE_RATE` 设成一个超出
//! 设备所报范围的值，才会落进兜底分支，重采样那条路才能在**真机**上被走一遍。

use cpal::traits::{DeviceTrait, HostTrait};
use dsls_audio::AudioEngine;
use std::time::{Duration, Instant};

/// 源采样率。默认 44100（多数音乐）；用 `DSLS_SMOKE_RATE` 覆盖。
const DEFAULT_SOURCE_RATE: u32 = 44100;
/// 喂进去的正弦有多长（秒）。**这就是时长判据的基准**：环里也该出这么多秒。
const TONE_SECONDS: usize = 3;

fn source_rate() -> u32 {
    match std::env::var("DSLS_SMOKE_RATE") {
        Ok(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("DSLS_SMOKE_RATE={v:?} 不是合法的 u32，退回 {DEFAULT_SOURCE_RATE}");
            DEFAULT_SOURCE_RATE
        }),
        Err(_) => DEFAULT_SOURCE_RATE,
    }
}

/// 按**源采样率**生成 `seconds` 秒的 440Hz 立体声正弦。
/// `push_samples` 收的就是源率样本，换算由引擎自己做。
fn tone(rate: u32, seconds: usize) -> Vec<i16> {
    let frames = rate as usize * seconds;
    let mut buf = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = (i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin() * 8000.0;
        buf.push(v as i16);
        buf.push(v as i16);
    }
    buf
}

fn main() -> anyhow::Result<()> {
    let rate = source_rate();
    let host = cpal::default_host();
    let device = host.default_output_device().expect("没有输出设备");
    let supported = device.default_output_config()?;
    println!(
        "DEVICE: {} | default: fmt={:?} ch={} rate={}",
        device.description()?.name(),
        supported.sample_format(),
        supported.channels(),
        supported.sample_rate()
    );

    // ⚠️ **这张表必须是可见的。** 早先只看 `default_output_config()`（ch=2），
    //    而 `supported_output_configs()` 里同时有 **1ch** 的 48000Hz 配置 ——
    //    只看前者永远发现不了这个坑。采样率对齐的选择正是在这张表上做的。
    println!("SUPPORTED OUTPUT CONFIGS:");
    for range in device.supported_output_configs()? {
        println!(
            "  {}ch {:?} {:?}..{:?}",
            range.channels(),
            range.sample_format(),
            range.min_sample_rate(),
            range.max_sample_rate()
        );
    }

    let mut engine = AudioEngine::new(rate)?;
    println!(
        "STREAM BUILT + PLAY OK | source={rate}Hz output={}Hz {}ch passthrough={} | \
         ring_capacity={}",
        engine.output_rate(),
        engine.output_channels(),
        engine.is_passthrough(),
        engine.ring_capacity()
    );

    // ── 1) 回调真的在取数 + xrun 可观测 ──
    //
    // ⚠️ 这一段**故意不加背压**（下面第 2 段才加）：它的目的是把环喂爆、
    //    好让「饿死之后 xrun 会涨」这条可观测。源率远高于输出率时这里会溢出丢样本
    //    （`push_samples` 的既定语义），所以 `ring_available` / `xrun` 在这两行里
    //    不可当性能指标读 —— 时长的判据在第 2 段。
    let one_sec = tone(rate, 1);
    engine.push_samples(&one_sec);
    println!(
        "PUSHED {} samples, ring_available={}",
        one_sec.len(),
        engine.ring_available()
    );
    std::thread::sleep(Duration::from_millis(400));
    let after_play = engine.ring_available();
    println!(
        "AFTER 400ms: ring_available={after_play} xrun={}",
        engine.xrun_count()
    );

    // 饿死它：不再喂数据，一直等到环被抽干，xrun 必须开始上涨
    let mut starved_at = None;
    for step in 0..300 {
        std::thread::sleep(Duration::from_millis(10));
        if engine.xrun_count() > 0 && starved_at.is_none() {
            starved_at = Some(step * 10);
            println!(
                "XRUN WENT NON-ZERO after ~{}ms of starvation: xrun={}",
                step * 10,
                engine.xrun_count()
            );
        }
    }
    println!(
        "AFTER STARVING 3000ms: ring_available={} xrun={}",
        engine.ring_available(),
        engine.xrun_count()
    );

    // ── 2) 时长判据：喂 N 秒 ⇒ 环里出 N 秒 ──
    //
    // ⚠️ **必须带背压喂**：`push_samples` 在环满时是**丢数据**（只记一次 overflow），
    //    一次全塞进去会丢掉大半，量出来的时长就没有意义了。CLI 里也是同一条规则。
    let audio = tone(rate, TONE_SECONDS);
    let started = Instant::now();
    let mut fed = 0usize;
    while fed < audio.len() / 2 {
        let take = (4096usize).min(audio.len() / 2 - fed);
        engine.push_samples(&audio[fed * 2..(fed + take) * 2]);
        fed += take;
        while engine.ring_available() > engine.ring_capacity() / 4 * 3 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    // ⚠️ **排空重采样器的尾巴。** 不调它，结尾会少一截（重采样那条路上尤其明显）。
    engine.finish_stream();
    while engine.ring_available() > 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let measured = started.elapsed().as_secs_f64();

    println!(
        "DURATION: 源 {TONE_SECONDS}s（{rate}Hz）→ 实测环排空用时 {measured:.3} s，\
         比值 {:.4}",
        measured / TONE_SECONDS as f64
    );
    println!(
        "          （重采样那条路上，不调 finish_stream 就会明显偏短；\
         直通那条路上它应当是 {TONE_SECONDS} 秒）"
    );
    println!("xrun={}", engine.xrun_count());

    if after_play < one_sec.len() {
        println!("RESULT: 回调确实从环里取走了样本（管线是活的）");
    } else {
        println!("RESULT: 警告 —— 回调一个样本都没取");
    }
    println!("starved_at={starved_at:?}");
    Ok(())
}
