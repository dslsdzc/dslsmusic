//! 诊断工具：把 cpal 在本机枚举到的设备**原样打出来**。
//!
//! 为什么需要它：`OutputPath` 要把「默认设备」映射到一个物理 `hw:` 设备
//! （见 `crates/audio/src/output_path.rs`），而 cpal 的 `Device` 除了
//! `description()` 之外**不暴露 ALSA 的 PCM 名字** —— 映射规则必须按各后端
//! 实际报出来的东西写，不能照文档猜。换一台机器（不同后端、不同 ALSA 配置）
//! 先跑这个再看映射结果。
//!
//! `cargo run -p dsls-audio --example probe_devices`
//!
//! 带一个参数时，只打印名字里含该子串的设备**支持的全部配置**（采样率 / 声道 /
//! 格式），用来判断某张卡能拿什么参数开流：
//!
//! `cargo run -p dsls-audio --example probe_devices -- Loopback`

use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let filter = std::env::args().nth(1);
    let host = cpal::default_host();
    println!("host = {:?}", host.id());
    if let Some(f) = filter {
        list_configs(&host, &f);
        return;
    }

    let default = host.default_output_device();
    match &default {
        Some(d) => print_device("默认输出设备", d),
        None => println!("默认输出设备: <无>"),
    }

    let devices = match host.devices() {
        Ok(d) => d,
        Err(e) => {
            println!("枚举设备失败: {e}");
            return;
        }
    };
    for (i, d) in devices.enumerate() {
        println!("--- [{i}] ---");
        print_device("", &d);
    }
}

/// 打印名字含 `filter` 的设备的全部输出配置。
fn list_configs(host: &cpal::Host, filter: &str) {
    let devices = match host.devices() {
        Ok(d) => d,
        Err(e) => {
            println!("枚举设备失败: {e}");
            return;
        }
    };
    for d in devices {
        let id = d.id().map(|x| x.id().to_owned()).unwrap_or_default();
        let name = d
            .description()
            .map(|x| x.name().to_owned())
            .unwrap_or_default();
        if !id.contains(filter) && !name.contains(filter) {
            continue;
        }
        println!("=== {id} ({name}) out={} ===", d.supports_output());
        match d.default_output_config() {
            Ok(c) => println!(
                "  default: {}Hz {}ch {:?}",
                c.sample_rate(),
                c.channels(),
                c.sample_format()
            ),
            Err(e) => println!("  default: <{e}>"),
        }
        match d.supported_output_configs() {
            Ok(cfgs) => {
                for c in cfgs {
                    println!(
                        "  {}ch {:?} {}..{}Hz",
                        c.channels(),
                        c.sample_format(),
                        c.min_sample_rate(),
                        c.max_sample_rate()
                    );
                }
            }
            Err(e) => println!("  supported_output_configs: <{e}>"),
        }
    }
}

fn print_device(label: &str, d: &cpal::Device) {
    let name = d
        .description()
        .map(|x| x.name().to_owned())
        .unwrap_or_else(|e| format!("<description 失败: {e}>"));
    // ⚠️ `driver()` 在 ALSA 后端里装的是**原始 PCM 名字**（cpal 源码
    //    `host/alsa/mod.rs:448` 的 `.driver(self.pcm_id.as_str())`），
    //    这也是唯一能拿到 `hw:CARD=..,DEV=..` 的地方。
    let id = d.id().map(|x| x.id().to_owned()).unwrap_or_default();
    let driver = d
        .description()
        .ok()
        .and_then(|x| x.driver().map(str::to_owned))
        .unwrap_or_default();
    println!(
        "{label} name={name:?} pcm_id={id:?} driver={driver:?} out={} in={}",
        d.supports_output(),
        d.supports_input()
    );
}
