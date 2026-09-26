//! 手动冒烟程序：**在真实设备上**验证「建流成功 → 回调真的在取数 → xrun 计数可观测」。
//!
//! 它不是自动化测试（机器上没声卡时会失败），所以刻意放在 `examples/` 而不是
//! `#[test]` 里。跑法：
//!
//! ```text
//! cargo run -p dsls-audio --example smoke
//! ```
//!
//! 期望：先看到「回调取走了样本」（`ring_available` 下降），再看到环被抽干后
//! `xrun` 从 0 开始上涨。这两件事一起证明了**回调体真的在硬件上跑**，
//! 而不只是编译通过。
//!
//! ⚠️ 它**不能**证明「声音本身是对的」——那需要回环采集，见 Task 9。
//! 采样率对齐（Task 10）也不是它验的：**时长的判据是掐表**，不是这个冒烟程序。
//! 它在这里的用处是把设备**实际报出来的能力**打出来，让「走的哪条路」在真机上
//! 可见 —— `AudioEngine::new` 的选择就是在这张表上做的，表打出来，选择才可复核。

use cpal::traits::{DeviceTrait, HostTrait};
use dsls_audio::AudioEngine;
use std::time::Duration;

/// 冒烟用的采样率。设备支持它就验「直通」那条路，不支持就验「重采样」那条路 ——
/// 两条路都会被真机跑一遍，**走的是哪条看下面打出来的 `passthrough=`**。
const DESIRED_RATE: u32 = 44100;

fn main() -> anyhow::Result<()> {
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

    let mut engine = AudioEngine::new(DESIRED_RATE)?;
    println!(
        "STREAM BUILT + PLAY OK | desired={DESIRED_RATE}Hz output={}Hz {}ch \
         passthrough={} | ring_capacity={}",
        engine.output_rate(),
        engine.output_channels(),
        engine.is_passthrough(),
        engine.ring_capacity()
    );

    // 1 秒 440Hz 正弦，立体声 i16，按**期望的（源）采样率**生成 ——
    // `push_samples` 收的就是源率样本，换算由引擎自己做。
    let rate = DESIRED_RATE as f32;
    let n = DESIRED_RATE as usize;
    let mut buf = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v = (i as f32 / rate * 440.0 * std::f32::consts::TAU).sin() * 8000.0;
        buf.push(v as i16);
        buf.push(v as i16);
    }
    engine.push_samples(&buf);
    println!(
        "PUSHED {} samples, ring_available={}",
        buf.len(),
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

    // 排空重采样器的尾巴（直通时是空操作）。放在最后，用来确认它不会 panic，
    // 也不会在环里凭空多塞数据。
    engine.finish_stream();
    println!(
        "AFTER finish_stream: ring_available={}",
        engine.ring_available()
    );

    if after_play < buf.len() {
        println!("RESULT: 回调确实从环里取走了样本（管线是活的）");
    } else {
        println!("RESULT: 警告 —— 回调一个样本都没取");
    }
    println!("starved_at={starved_at:?}");
    Ok(())
}
