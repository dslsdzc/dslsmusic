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

use cpal::traits::{DeviceTrait, HostTrait};
use dsls_audio::AudioEngine;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("没有输出设备");
    let supported = device.default_output_config()?;
    println!(
        "DEVICE: {} | fmt={:?} ch={} rate={}",
        device.description()?.name(),
        supported.sample_format(),
        supported.channels(),
        supported.sample_rate()
    );

    let mut engine = AudioEngine::new()?;
    println!("STREAM BUILT + PLAY OK");

    // 1 秒 440Hz 正弦，立体声 i16，按**设备自己的**采样率生成
    let rate = supported.sample_rate() as f32;
    let n = supported.sample_rate() as usize;
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

    if after_play < buf.len() {
        println!("RESULT: 回调确实从环里取走了样本（管线是活的）");
    } else {
        println!("RESULT: 警告 —— 回调一个样本都没取");
    }
    println!("starved_at={starved_at:?}");
    Ok(())
}
