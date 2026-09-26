//! `snd-aloop` 回环验证 —— **今天第一个能回答「声音对不对」的判据**。
//!
//! 在此之前，整条音频链路只有「时长对不对」和「xrun 是不是 0」两条指标，
//! 而这两条**全都不看样本本身**：采样率错了、声道反了、格式转换写错了、
//! 重采样算法是线性插值 —— 它们照样全绿。这个示例补上那一条：
//!
//! ```text
//! 已知信号 ──▶ AudioEngine（解码侧同一条路径：ring → 回调 → 格式转换） ──▶ hw:Loopback,0
//!                                                                          │
//!                                            内核模块 snd-aloop（内存回环） │
//!                                                                          ▼
//!                       逐位比对（或声明的容差内） ◀── 采集 hw:Loopback,1
//! ```
//!
//! # 它**能**验证什么
//!
//! - **格式转换**：i16 → 设备格式（f32/i32/I24）→ 回来，逐位一致；
//! - **声道处理**：L/R 有没有错位（左用 440+1500Hz、右用 600+1200Hz，两声道不同）；
//! - **重采样路径**：源采样率设备不支持时，频率与幅度有没有被保住；
//! - **我们确实是把数据写进设备的那一方**：录回来的就是我们写进去的那串
//!   （伪随机序列，不可能碰巧相同）。
//!
//! # 它**不能**验证什么（不许夸大）
//!
//! - 验证的是**我们这条路径**，**不是「到扬声器的最终路径」**。真实声卡内部
//!   可能还有 DSP / 硬件混音，那一步在 `hw:` **之后**，从进程内看不见。
//! - 所以它**不能**证明「你听到的是逐位精确的」。那需要**硬件级采集**
//!   （一根线把输出接回输入，或用带数字回读的声卡）。
//! - 回环设备自己也在做事：它的播放侧与采集侧是两个独立 PCM，中间由内核模块
//!   的内存拷贝连接。这一层不是我们的代码，它**自己出问题**本示例看不出来
//!   （但两遍交叉比对能看出来：见「回环自检」）。
//!
//! # 用法
//!
//! ```text
//! sudo modprobe snd-aloop            # 本示例不会自己加载内核模块
//! cargo run -p dsls-audio --example loopback_verify
//! ```
//!
//! 没有 `snd-aloop` 时**优雅跳过**（打印怎么加载它），退出码仍是 0 ——
//! 「没有回环设备」不是「声音是错的」。
//!
//! 其它模式：
//!
//! ```text
//! cargo run -p dsls-audio --example loopback_verify -- --mapping
//!     只打印能力模型：AudioEngine::new() 实际会拿到 Exclusive 还是 Shared。
//!
//! cargo run -p dsls-audio --example loopback_verify -- --hold hw:CARD=1,DEV=0
//!     占位进程：死占住某台设备，用来实测「hw: 被占用时优雅退回 Shared」。
//! ```
//!
//! # 实时约束（设计文档 §16.2）
//!
//! 采集回调里**只做**「T → i16 转换 + 推进 ring」，无分配、无锁、无 IO。
//! 取环由另一个普通线程做（那里允许分配）。见 [`build_capture`] 与 [`spawn_drain`]。

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dsls_audio::output_path::{self, OutputPath};
use dsls_audio::{AudioEngine, AudioRing, AudioRingConsumer, AudioRingProducer};

/// 直通那一档用的源采样率。snd-aloop 支持 8000..768000，所以这一档必然是**直通**。
const SRC_RATE: u32 = 44100;

/// 重采样那一档的源采样率。**刻意选在 snd-aloop 的下限（8000）之外** ——
/// 引擎报告不了这个率，于是必然走「设备默认率 + 重采样」那条路
/// （`AudioEngine::build_on_device` 的 ②）。
const RESAMPLE_SRC_RATE: u32 = 4000;

/// 采集环容量（秒）。信号只有 1 秒，留足余量装下前后的静音。
const CAPTURE_SECONDS: usize = 6;

/// 对齐用的前导码长度（帧）。伪随机序列的 64 帧（128 个样本）在别处**不可能**
/// 再次出现 —— 用它找起点比「找第一个非零样本」稳得多（回调开始时会有一段
/// 饥饿造成的静音，那段的长度不确定）。
const PREAMBLE_FRAMES: usize = 64;

/// 幅度容差：重采样那一档允许的幅度偏差（占满幅比例）。
///
/// 为什么是「声明的容差」而不是逐位一致：重采样是**滤波**，输出与输入的解析值
/// 本来就不会逐位相等（rubato 的 Fft 是同步重采样器，通带内近似单位增益，
/// 但存在远低于本容差的纹波）。本容差只用来判「信号有没有被毁掉」：
/// 频率错、线性插值、混叠、丢样本都会**远超**它。实测值会在输出里打出来。
const RESAMPLE_AMP_TOLERANCE: f64 = 0.05;

/// 正弦幅度（每个分量）。两个分量相加 20000 < 32767，不会削顶。
const SINE_AMP: f32 = 10_000.0;

fn main() -> ExitCode {
    // 引擎的 log 是「如实报告」的一部分（独占尝试、可选的 hw: 设备列表），
    // 不装 logger 就什么都看不见。
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--hold") => hold(args.get(1).map(String::as_str)),
        Some("--mapping") => {
            mapping();
            ExitCode::SUCCESS
        }
        _ => verify(),
    }
}

/// 只报告能力模型：`AudioEngine::new()` 会拿到哪条路，以及本机可选哪些 `hw:` 设备。
fn mapping() {
    let host = cpal::default_host();
    println!("== 能力模型 ==");
    report_auto_path();
    let options = output_path::hardware_output_devices(&host);
    println!("本机可选的物理输出设备（`hw:`，独占的**唯一**来源）：");
    if options.is_empty() {
        println!("  <一个都没有>");
    }
    for (id, name) in options {
        println!("  {id}  ({name})");
    }
    println!(
        "\n指定其一做独占：设 {} 或加 CLI 的 --device（后者是硬性的：打不开就报错）。",
        output_path::EXCLUSIVE_DEVICE_ENV
    );
}

/// 占位进程：死占住一台设备，用来实测「被占用时怎么办」。
fn hold(pcm_id: Option<&str>) -> ExitCode {
    let Some(pcm_id) = pcm_id else {
        eprintln!("用法：--hold <pcm_id>，例如 --hold hw:CARD=1,DEV=0");
        return ExitCode::FAILURE;
    };
    // `new_on` 是**硬**语义：占不到就直接报错，不会偷偷换设备。
    let engine = match AudioEngine::new_on(pcm_id, SRC_RATE) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("占位失败：{pcm_id} 打不开（{e}）");
            return ExitCode::FAILURE;
        }
    };
    println!("已占住 {pcm_id}（{}）。Ctrl-C 结束。", engine.output_path());
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// 跑一条判据；**只在「测量链路被证明是脏的」时重测一次**，并如实打印两次的数字。
///
/// ⚠️ **这不是「重试到通过」。** 它有两个硬边界：
///
/// - **只有设备侧 xrun 非零**（= 测量本身被干扰，snd-aloop 没有流控）才重测；
///   一次**干净**的跑失败就是失败，一次都不重试。
/// - 重测时换伪随机种子（`attempt`），否则回环缓冲里的残留会和期望一模一样，
///   前导码会匹配到残留上 —— 那种「通过」是假的。
///
/// 为什么需要它：实测 14 次里 2 次判据 1 失败，两次都伴随**播放侧 2 次设备 xrun**，
/// 且 dump 分析显示「前 21504 帧逐位一致，之后是与本信号无关的缓冲残留」
/// （既不是位移也不是重启）—— 也就是 snd-aloop 的环形缓冲在设备 xrun 后
/// 读写位置失配，这是**测量器件（内核模块）的行为**，不是样本处理错了。
/// 相关分析见 `.superpowers/sdd/task-11-report.md`。
fn measure(
    name: &str,
    lb: &Loopback,
    run: fn(&Loopback, u64) -> anyhow::Result<Outcome>,
) -> (Option<Outcome>, bool) {
    let mut attempt = 0u64;
    let mut outcome = match run(lb, attempt) {
        Ok(o) => o,
        Err(e) => {
            println!("  ✗ 跑不起来：{e:#}");
            return (None, true);
        }
    };
    if !outcome.pass && outcome.disturbed() > 0 {
        println!(
            "  ↻ {name}：这次两侧设备侧 xrun = 采集 {} / 播放 {} —— **测量本身被干扰**了\
             （snd-aloop 没有流控），重测一次。\n                  说明：干净的一次跑失败**不会**重测；这次重测换了一套伪随机数据。",
            outcome.capture_errors, outcome.playback_errors
        );
        attempt += 1;
        outcome = match run(lb, attempt) {
            Ok(o) => o,
            Err(e) => {
                println!("  ✗ 重测跑不起来：{e:#}");
                return (None, true);
            }
        };
    }
    let failed = !outcome.pass;
    (Some(outcome), failed)
}

fn verify() -> ExitCode {
    let host = cpal::default_host();

    println!("== 能力模型：AudioEngine::new() 实际拿到哪条路 ==");
    report_auto_path();

    let Some(lb) = find_loopback(&host) else {
        println!("\n== SKIPPED：本机没有 snd-aloop 回环设备，回环验证没跑 ==");
        println!("它需要内核模块 snd-aloop（GPL，主线内核自带）。加载：");
        println!("    sudo modprobe snd-aloop");
        println!("然后确认 /proc/asound/cards 里出现 `Loopback`，再跑一次本示例。");
        println!(
            "⚠️ **这不是失败**：没有回环设备就没法验证「声音对不对」—— 如实跳过，不假装验过。"
        );
        return ExitCode::SUCCESS;
    };
    println!("\n回环设备：写 {} → 读 {}", lb.play_id, lb.cap_id);

    let mut failed = 0usize;
    let mut disturbances = 0u64;
    for (name, run) in [
        (
            "判据 1/2",
            test_bit_exact as fn(&Loopback, u64) -> anyhow::Result<Outcome>,
        ),
        ("判据 2/2", test_resample),
    ] {
        let (outcome, bad) = measure(name, &lb, run);
        if let Some(o) = outcome {
            disturbances += o.disturbed();
        }
        if bad {
            failed += 1;
        }
    }

    println!("\n== 结论 ==");
    if failed == 0 {
        println!("回环验证通过。⚠️ 它证明的是「到 hw: 为止、我们这条路径」没问题，");
        println!("   **不是**「你听到的是逐位精确的」—— 那需要硬件级采集（见本文件顶部）。");
        if disturbances > 0 {
            println!(
                "   ⚠️ 但这次运行两侧设备侧 xrun 合计 {disturbances} 次 —— 结论要打个折扣，建议重跑。"
            );
        }
        ExitCode::SUCCESS
    } else {
        println!("有 {failed} 项没过 —— 上面每项的实测数字就是线索。");
        if disturbances > 0 {
            println!(
                "   （本次两侧设备侧 xrun 合计 {disturbances} 次；先按上面的提示重跑一次，\
                 排除「测量链路被饿到」再查代码。）"
            );
        }
        ExitCode::FAILURE
    }
}

/// 打印 `AudioEngine::new()`（**自动选路**）实际拿到的那条路径。
///
/// CLI 打印的就是这几行：路径 + 它保证什么/不保证什么。**两行必须一起出现** ——
/// 只打「Exclusive」而不打边界，就是在给人一个假的 bit-perfect 承诺。
fn report_auto_path() {
    if let Some(note) = output_path::platform_note() {
        println!("  {note}");
    }
    match AudioEngine::new(SRC_RATE) {
        Ok(engine) => print_path(engine.output_path(), &engine),
        Err(e) => println!("  建不出流：{e:#}"),
    }
}

fn print_path(path: &OutputPath, engine: &AudioEngine) {
    println!("  输出路径：{path}");
    println!("    ⚠️ {}", path.caveat());
    println!(
        "    设备侧：{} 声道 / 格式 {:?} / {}Hz；我们这一层{}重采样。",
        engine.output_channels(),
        engine.output_format(),
        engine.output_rate(),
        if engine.is_passthrough() {
            "未"
        } else {
            "已"
        }
    );
}

// ─────────────────────────── 回环设备发现 ───────────────────────────

struct Loopback {
    /// 写进去的那一侧（`hw:CARD=<loopback>,DEV=0`）。
    play_id: String,
    /// 录回来的那一侧（`DEV=1`）。
    cap_id: String,
    cap: cpal::Device,
}

/// snd-aloop 的两侧：**同一个模块的 DEV=0 与 DEV=1**。
///
/// 为什么按 DEV 号而不是按枚举顺序：cpal 的枚举顺序不是接口契约，
/// 而 DEV=0 → DEV=1 这个配对是 snd-aloop 的既定行为（写 DEV=0 的子流，
/// 从 DEV=1 的同号子流读出来）。
fn find_loopback(host: &cpal::Host) -> Option<Loopback> {
    let mut play = None;
    let mut cap = None;
    for d in host.devices().ok()? {
        let id = output_path::pcm_id_of(&d);
        if !output_path::is_hardware_pcm_id(&id) {
            continue;
        }
        // 卡名由内核给（snd-aloop 恒为 "Loopback"），不随语言变。
        let name = d
            .description()
            .map(|x| x.name().to_owned())
            .unwrap_or_default();
        if !name.contains("Loopback") {
            continue;
        }
        match dev_index(&id) {
            Some(0) if d.supports_output() => play = Some((id, d)),
            Some(1) if d.supports_input() => cap = Some((id, d)),
            _ => {}
        }
    }
    let (play_id, _) = play?;
    let (cap_id, cap) = cap?;
    Some(Loopback {
        play_id,
        cap_id,
        cap,
    })
}

/// `hw:CARD=3,DEV=1` → `Some(1)`。解析不出来就 `None`（宁可找不到设备，也不猜）。
fn dev_index(pcm_id: &str) -> Option<u32> {
    pcm_id
        .split(',')
        .find_map(|p| p.trim().strip_prefix("DEV="))?
        .parse()
        .ok()
}

// ─────────────────────────── 判据一：逐位一致 ───────────────────────────

/// 直通那一档：**逐位一致**（源采样率建流 ⇒ 我们一个样本都没动，
/// 设备格式转换是无损的 ⇒ 录回来的必须与期望**逐位相同**）。
///
/// ⚠️ `attempt` 只用来换伪随机种子（见 [`measure`] 里的说明）：**重测时绝不能用
/// 同一串数据** —— 回环缓冲里还留着上一次的波形，同种子会让「前导码唯一」这条
/// 假设失效，比对就失去意义。
fn test_bit_exact(lb: &Loopback, attempt: u64) -> anyhow::Result<Outcome> {
    println!("\n== 判据 1/2：直通（源采样率建流）逐位一致 ==");
    if attempt > 0 {
        println!("  （第 {} 次重测）", attempt + 1);
    }
    let signal = pseudo_random(SRC_RATE as usize, 0x5EED_1234_ABCD_0001 ^ attempt);

    let mut engine = AudioEngine::new_on(&lb.play_id, SRC_RATE)?;
    print_path(engine.output_path(), &engine);
    anyhow::ensure!(
        engine.is_passthrough(),
        "这一档要验的就是「我们没重采样」，但引擎报告它重采样了"
    );

    let capture = start_capture(
        &lb.cap,
        engine.output_rate(),
        engine.output_channels(),
        engine.output_format(),
    )?;
    engine.push_samples(&signal);
    engine.finish_stream();
    wait_drained(&engine)?;
    let cap = capture.finish();
    dump("bitexact", &cap.samples);
    let captured = &cap.samples[..];
    let playback_errors = engine.device_error_count();
    report_measurement(cap.device_errors, playback_errors);

    println!(
        "  写出 {} 个样本，录回 {} 个（含前后静音）",
        signal.len(),
        captured.len()
    );
    let Some(at) = find_onset(captured, &signal) else {
        report_no_onset(captured, &signal);
        return Ok(Outcome {
            pass: false,
            capture_errors: cap.device_errors,
            playback_errors,
        });
    };
    println!(
        "  前导码在第 {at} 个样本处对上（≈{:.1}ms 的静音头）",
        at as f64 / 2.0 / SRC_RATE as f64 * 1000.0
    );

    let d = compare(&captured[at..], &signal);
    let outcome = Outcome {
        pass: d.mismatches == 0,
        capture_errors: cap.device_errors,
        playback_errors,
    };
    if d.mismatches == 0 {
        println!(
            "  ✓ 逐位一致：{} 个样本全等（最大偏差 {} LSB）",
            d.compared, d.worst
        );
        Ok(outcome)
    } else {
        println!(
            "  ✗ {} / {} 个样本不一致，最大偏差 {} LSB",
            d.mismatches, d.compared, d.worst
        );
        if let Some((i, want, got)) = d.first {
            let ch = if i % 2 == 0 { "L" } else { "R" };
            println!(
                "    第一个不一致：样本 {i}（第 {} 帧，{ch} 声道）期望 {want}，实得 {got}",
                i / 2
            );
        }
        if d.worst_channel_swapped() {
            println!(
                "    ⚠️ 左右声道**互换**了 —— 见 crates/audio/src/lib.rs 里「半个帧」那段注释。"
            );
        }
        if outcome.disturbed() > 0 {
            println!(
                "    ⚠️ 这次两侧设备侧 xrun = 采集 {} / 播放 {} —— 设备被饿到时会在信号里\
                 插静音或丢样本，**那可能正是这些不一致的来源**（不是样本处理错了）。\
                 先重跑一次；重跑仍然不一致才说明是我们的问题。",
                outcome.capture_errors, outcome.playback_errors
            );
        }
        Ok(outcome)
    }
}

// ─────────────────────────── 判据二：重采样路径 ───────────────────────────

/// 重采样那一档：源采样率设备不支持（[`RESAMPLE_SRC_RATE`]）⇒ 引擎退回设备默认率
/// 并重采样。这里验的是**信号有没有被毁掉**：两个已知频率的幅度还在不在。
///
/// 为什么不是逐位：重采样是滤波，输出与输入的解析值本来就不逐位相等。
/// 为什么幅度就够：频率错了（比率错）、线性插值（高频滚降）、混叠、丢样本，
/// 都会让某个分量**塌掉**，而通带内的好重采样器两处都在 1% 以内。
fn test_resample(lb: &Loopback, attempt: u64) -> anyhow::Result<Outcome> {
    println!("\n== 判据 2/2：重采样路径（{RESAMPLE_SRC_RATE}Hz → 设备默认率）==");
    if attempt > 0 {
        println!("  （第 {} 次重测）", attempt + 1);
    }
    let _ = attempt; // 这一档用正弦，没有「前导码唯一」那类问题
    // 左右**用不同的频率对**：这一档就能顺带验声道有没有接反。
    // 每个频率在 0.9 秒的分析窗里都是整数周期（440×0.9=396、1500×0.9=1350、
    // 600×0.9=540、1200×0.9=1080）⇒ 矩形窗下互不泄漏，幅度读数干净。
    const LEFT: [f64; 2] = [440.0, 1500.0];
    const RIGHT: [f64; 2] = [600.0, 1200.0];
    let signal = dual_sine(RESAMPLE_SRC_RATE as usize, RESAMPLE_SRC_RATE, &LEFT, &RIGHT);

    let mut engine = AudioEngine::new_on(&lb.play_id, RESAMPLE_SRC_RATE)?;
    print_path(engine.output_path(), &engine);
    anyhow::ensure!(
        !engine.is_passthrough(),
        "{RESAMPLE_SRC_RATE}Hz 不该被设备支持，但引擎报告它是直通 —— 这一档就测不到了"
    );
    let out_rate = engine.output_rate();
    println!(
        "  重采样 {RESAMPLE_SRC_RATE}Hz → {out_rate}Hz（比率 {:.3}）",
        out_rate as f64 / RESAMPLE_SRC_RATE as f64
    );

    let capture = start_capture(
        &lb.cap,
        out_rate,
        engine.output_channels(),
        engine.output_format(),
    )?;
    engine.push_samples(&signal);
    engine.finish_stream();
    wait_drained(&engine)?;
    let cap = capture.finish();
    dump("resample", &cap.samples);
    let captured = &cap.samples[..];
    let playback_errors = engine.device_error_count();
    report_measurement(cap.device_errors, playback_errors);

    // ⚠️ **单位：`find_onset_by_level` 给的是【样本】下标，下面全都按【帧】算。**
    //    （这里第一版就是混了单位：把「样本差」当成「帧数」，于是 1 秒的信号
    //     被量成 2 秒 —— 恰是本项目反复踩的那种「数字看着正常但口径不对」。
    //     它自己是靠 dump 出来的 0.1 秒一段的幅度包络定位到的。）
    let Some(start_sample) = find_onset_by_level(captured, (SINE_AMP * 0.25) as i16) else {
        println!(
            "  ✗ 录回来的信号里找不到任何超过阈值的样本（{} 个样本全是静音？）",
            captured.len()
        );
        return Ok(Outcome {
            pass: false,
            capture_errors: cap.device_errors,
            playback_errors,
        });
    };
    let start = start_sample / 2;
    // 跳过起振（滤波器瞬态 + 检测误差），取 0.9 秒做分析。
    let skip = out_rate as usize / 20;
    let span = out_rate as usize * 9 / 10;
    let total_frames = captured.len() / 2;
    let end = (start + skip + span).min(total_frames);
    if end < start + skip + span / 2 {
        println!(
            "  ✗ 录回来的信号太短（只有 {} 帧可用）",
            end.saturating_sub(start + skip)
        );
        return Ok(Outcome {
            pass: false,
            capture_errors: cap.device_errors,
            playback_errors,
        });
    }
    let window = &captured[start + skip..end * 2];
    let left: Vec<f32> = window.iter().step_by(2).map(|&s| s as f32).collect();
    let right: Vec<f32> = window
        .iter()
        .skip(1)
        .step_by(2)
        .map(|&s| s as f32)
        .collect();

    let mut ok = true;
    for (ch, samples, freqs) in [("L", &left, LEFT), ("R", &right, RIGHT)] {
        for f in freqs {
            let got = amplitude_at(samples, out_rate, f);
            let dev = (got - SINE_AMP as f64).abs() / SINE_AMP as f64;
            let mark = if dev <= RESAMPLE_AMP_TOLERANCE {
                "✓"
            } else {
                "✗"
            };
            println!(
                "  {mark} {ch} 声道 {f:.0}Hz：幅度 {got:.0} / 期望 {SINE_AMP:.0}\
                 （偏差 {:.2}%，容差 {:.0}%）",
                dev * 100.0,
                RESAMPLE_AMP_TOLERANCE * 100.0
            );
            if dev > RESAMPLE_AMP_TOLERANCE {
                ok = false;
            }
        }
    }

    // 时长：信号的有声区间 ≈ 输入的时长。**±10%** —— 这一档只用来抓「丢了一大截」
    // （例如背压把数据扔了、尾巴被吞了）。精确的时长保证在 resample.rs 的单元测试里。
    let expected_span = signal.len() / 2 * out_rate as usize / RESAMPLE_SRC_RATE as usize;
    if let Some(last_sample) = captured
        .iter()
        .rposition(|&s| s.abs() > (SINE_AMP * 0.25) as i16)
    {
        // 同样换成帧（`rposition` 给的是样本下标）。
        let measured = (last_sample / 2).saturating_sub(start);
        let dev = (measured as f64 - expected_span as f64).abs() / expected_span as f64;
        let mark = if dev <= 0.10 { "✓" } else { "✗" };
        println!(
            "  {mark} 有声区间 {measured} 帧 / 期望 {expected_span} 帧（偏差 {:.2}%，容差 10%）",
            dev * 100.0
        );
        if dev > 0.10 {
            ok = false;
        }
    }
    println!("  （这一档是**滤波**，不可能逐位一致；逐位那条由判据 1 负责。）");
    Ok(Outcome {
        pass: ok,
        capture_errors: cap.device_errors,
        playback_errors,
    })
}

// ─────────────────────────── 采集侧 ───────────────────────────

/// 采集端：一路流 + 一个把 ring 抽干的普通线程。
struct Capture {
    stream: cpal::Stream,
    stop: Arc<AtomicBool>,
    /// 设备侧错误（ALSA 的 overrun/underrun）次数。**必须报出来**：
    /// 它非零时这次比对就**不能当判据**（样本可能被设备丢过），而不是「路径错了」。
    errors: Arc<AtomicU64>,
    drain: std::thread::JoinHandle<Vec<i16>>,
}

/// 一次采集的产物。
struct Captured {
    samples: Vec<i16>,
    /// 设备侧错误次数。见 [`Capture::errors`]。
    device_errors: u64,
}

impl Capture {
    /// 停流 → 等设备把最后一点交付完 → 停抽干线程 → 取回样本。
    fn finish(self) -> Captured {
        self.stream.pause().ok();
        drop(self.stream);
        // 设备侧缓冲里可能还有没交上来的：本机采集缓冲配的是 1/8 秒
        // （见 start_capture），所以这里等得比它长。
        std::thread::sleep(Duration::from_millis(500));
        self.stop.store(true, Ordering::Relaxed);
        Captured {
            samples: self.drain.join().unwrap_or_default(),
            device_errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

fn start_capture(
    device: &cpal::Device,
    rate: u32,
    channels: u16,
    format: cpal::SampleFormat,
) -> anyhow::Result<Capture> {
    let config = cpal::StreamConfig {
        channels,
        sample_rate: rate,
        // ⚠️ **刻意要一个比默认大得多的设备侧缓冲（1/8 秒）。**
        //    snd-aloop 的设备侧缓冲只有 2 个周期（本机实测约 15ms），而这一档
        //    测的是**样本内容**、不是实时性：系统一忙（编译、别的音频进程），
        //    采集端就可能被饿到、设备侧 overrun 丢样本 —— 那时比对会失败，
        //    但**错的是测量链路，不是我们的音频路径**。实测踩过一次（9 次里 1 次）。
        //    注意：**只放大采集这一侧**。播放侧（`AudioEngine`）保持它自己的默认
        //    缓冲 —— 那是产品行为，不为测试改。
        buffer_size: cpal::BufferSize::Fixed(rate / 8),
    };
    // 环容量按采集率算（同 `AudioEngine` 的理由：按秒定，不按样本数定）。
    let ring = AudioRing::new(rate as usize * channels as usize * CAPTURE_SECONDS);
    let (producer, consumer) = ring.split();
    let errors = Arc::new(AtomicU64::new(0));
    let stream = build_capture(device, config, format, producer, Arc::clone(&errors))?;
    stream.play()?;
    let stop = Arc::new(AtomicBool::new(false));
    let drain = spawn_drain(consumer, Arc::clone(&stop));
    Ok(Capture {
        stream,
        stop,
        errors,
        drain,
    })
}

/// 按设备报的格式分发采集流。**与 `build_stream_for` 同一套臂** ——
/// 设备的格式是运行时才知道的，硬写一种就等于在别的设备上直接失败。
fn build_capture(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    producer: AudioRingProducer,
    errors: Arc<AtomicU64>,
) -> anyhow::Result<cpal::Stream> {
    use cpal::SampleFormat as F;
    match format {
        F::F32 => capture_stream::<f32>(device, config, producer, errors),
        F::I16 => capture_stream::<i16>(device, config, producer, errors),
        F::I32 => capture_stream::<i32>(device, config, producer, errors),
        F::I24 => capture_stream::<cpal::I24>(device, config, producer, errors),
        F::U8 => capture_stream::<u8>(device, config, producer, errors),
        other => anyhow::bail!("采集侧不支持格式 {other:?}（与 build_stream_for 的臂保持一致）"),
    }
}

fn capture_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut producer: AudioRingProducer,
    errors: Arc<AtomicU64>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + ToI16 + Send + 'static,
{
    // 回调内**不分配**：暂存一次性分配好再 move 进闭包（与 audio 回调同一个理由）。
    let mut stage: Vec<i16> = vec![0; 8192];
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut off = 0;
                while off < data.len() {
                    let n = (data.len() - off).min(stage.len());
                    for (dst, &src) in stage[..n].iter_mut().zip(&data[off..off + n]) {
                        *dst = src.to_i16();
                    }
                    producer.push_samples(&stage[..n]);
                    off += n;
                }
            },
            move |err| {
                errors.fetch_add(1, Ordering::Relaxed);
                log::error!("采集流错误: {err}");
            },
            None,
        )
        .map_err(Into::into)
}

/// 抽环线程（普通线程，允许分配）。回调只推，这里只取。
fn spawn_drain(
    mut consumer: AudioRingConsumer,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<i16>> {
    std::thread::spawn(move || {
        let mut out: Vec<i16> = Vec::new();
        let mut buf = vec![0i16; 8192];
        while !stop.load(Ordering::Relaxed) {
            let n = consumer.available().min(buf.len());
            if n == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            let got = consumer.pop_raw(&mut buf[..n]);
            out.extend_from_slice(&buf[..got]);
        }
        // 收尾：停流之前已经交付的样本还在环里。
        loop {
            let n = consumer.available().min(buf.len());
            if n == 0 {
                break;
            }
            let got = consumer.pop_raw(&mut buf[..n]);
            out.extend_from_slice(&buf[..got]);
        }
        out
    })
}

/// 设备域的样本 → i16。
///
/// ⚠️ **每个实现都手写，不走 cpal 的 `from_sample`。** 用 `from_sample` 等于用
/// 实现里那同一个函数去证明实现 —— 尺度写错（例如 `*32767` 而不是 `*32768`）
/// 会在两边同时出现而相互抵消。这里写的是**逆映射**，与
/// `crates/audio/src/lib.rs` 里 `I24` 那条测试同一个理由。
trait ToI16 {
    fn to_i16(self) -> i16;
}

impl ToI16 for i16 {
    fn to_i16(self) -> i16 {
        self
    }
}

impl ToI16 for f32 {
    fn to_i16(self) -> i16 {
        // 逆映射：i16 → f32 是 `k / 32768.0`，所以回来是 `x * 32768.0`。
        // `.round()` 必须有（截断会引入 -0.5 LSB 的偏置）。
        (self * 32768.0).round() as i16
    }
}

impl ToI16 for i32 {
    fn to_i16(self) -> i16 {
        // i16 → i32 是 `<< 16`（dasp 的转换），逆回来是算术右移。
        (self >> 16) as i16
    }
}

impl ToI16 for cpal::I24 {
    fn to_i16(self) -> i16 {
        // i16 → I24 是 `<< 8`（已核实 dasp_sample-0.11.0/src/conv.rs:186）。
        (self.inner() >> 8) as i16
    }
}

impl ToI16 for u8 {
    fn to_i16(self) -> i16 {
        // i16 → u8 是 `((k >> 8) + 128) as u8`（**有损**：8 位只有 256 级）。
        // 采样设备真给 u8 时这条只能验「大致对」，验不了逐位 —— 那种情况下
        // 判据 1 会在报告里显示偏差量级。
        ((self as i16) - 128) << 8
    }
}

// ─────────────────────────── 信号与比对 ───────────────────────────

/// 确定性伪随机 i16 立体声（xorshift64）。
///
/// 为什么用伪随机而不是正弦：它是**宽带**信号，任何插值/重采样/丢样本/声道错位
/// 都会立刻显形，而且只要种子相同，期望值就可以在本进程里重新算出来。
fn pseudo_random(frames: usize, seed: u64) -> Vec<i16> {
    let mut s = seed | 1;
    let mut out = Vec::with_capacity(frames * 2);
    for _ in 0..frames * 2 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        // 取高位：xorshift 的低位随机性差。
        out.push(((s >> 48) as u16) as i16);
    }
    out
}

/// 左右声道各两个正弦的叠加（**两声道内容不同** ⇒ 声道接反会被抓到）。
fn dual_sine(frames: usize, rate: u32, left: &[f64; 2], right: &[f64; 2]) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * 2);
    for n in 0..frames {
        let t = n as f64 / rate as f64;
        let l: f64 = left
            .iter()
            .map(|f| (2.0 * std::f64::consts::PI * f * t).sin())
            .sum();
        let r: f64 = right
            .iter()
            .map(|f| (2.0 * std::f64::consts::PI * f * t).sin())
            .sum();
        out.push((l * SINE_AMP as f64) as i16);
        out.push((r * SINE_AMP as f64) as i16);
    }
    out
}

/// 报出**两侧**的设备侧错误：采集侧（本示例的流）与播放侧（`AudioEngine` 的流）。
///
/// 为什么要单独说这件事：**它非零时这次比对不能当判据。** snd-aloop 没有流控，
/// 系统一忙（编译、别的音频进程）任一侧被饿到，设备就会丢样本或插静音 ——
/// 那时比对会失败，但**错的是实时链路，不是样本内容**。
///
/// 实测（2026-09-26，本机）：
/// - 正常 6 次连跑：两侧都是 0，逐位一致 88200/88200；
/// - **CPU 满载**（8 个忙循环）跑一次：播放侧数到 **17** 次设备 xrun，采集侧 0 ——
///   信号被插了静音，判据 1 失败、判据 2 的幅度掉到 35%~78%。同一次日志里
///   17 行 `音频流错误: A buffer underrun or overrun occurred`。
///   ⇒ 那次失败是**真的**（实时路径被饿到），只是原因不在样本处理上，而在这里。
fn report_measurement(capture_errors: u64, playback_errors: u64) {
    if capture_errors == 0 && playback_errors == 0 {
        println!("  测量链路：设备侧错误 0（采集侧 0 / 播放侧 0）—— 这次比对有效");
    } else {
        println!(
            "  ⚠️ 测量链路：设备侧错误 **采集侧 {capture_errors} / 播放侧 {playback_errors}** —— \
             snd-aloop 没有流控，被饿到时设备会丢样本或插静音，甚至让环形缓冲的读写位置失配。\n     \
             内容指标照样算数，但**这次的结论要打个折扣**：真失败的话会自动重测一次（见 measure()）；\
             反复出现就看系统负载（或换台机器跑）。"
        );
    }
}

/// 一次判据的结果。
///
/// ⚠️ **设备侧 xrun 的次数不决定通过与否，只作为「这次结论可不可信」的标注。**
/// 为什么这么分工（这是实现时踩出来的）：
///
/// - 判据的**内容**（样本是否逐位一致、幅度在不在容差内）就是判据本身，
///   它失败就是失败 —— 那个 xrun 计数只是**解释**（设备被饿到会往信号里插静音，
///   不一致正是从那里来的）；
/// - 反过来，把「xrun 非零」也当成失败会让判据**变得不可信**：一次 6 连跑里就有
///   1 次因为播放侧 2 次 xrun 而报失败，而**所有内容指标全部通过**（幅度 9987/10000、
///   时长偏差 0.01%）。一个会随机报红、又和被测内容无关的判据，看两次就会被忽略。
/// - 所以：**内容决定通过与否，干扰只被如实标出来**（含汇总里那一行）。
struct Outcome {
    pass: bool,
    capture_errors: u64,
    playback_errors: u64,
}

impl Outcome {
    fn disturbed(&self) -> u64 {
        self.capture_errors + self.playback_errors
    }
}

/// 把采集到的原始样本落盘（`DSLS_LB_DUMP=/tmp/x.raw`）—— 出问题时能拿去离线看。
/// 出问题时的第一条命令就是它，所以留在代码里而不是临时加。
fn dump(tag: &str, samples: &[i16]) {
    let Ok(path) = std::env::var("DSLS_LB_DUMP") else {
        return;
    };
    let path = format!("{path}.{tag}.raw");
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    match std::fs::write(&path, &bytes) {
        Ok(()) => println!("  （已把 {} 个样本 dump 到 {path}）", samples.len()),
        Err(e) => println!("  （dump 到 {path} 失败：{e}）"),
    }
}

/// 在采集里找期望信号的起点：**逐样本匹配前导码**。
fn find_onset(captured: &[i16], expected: &[i16]) -> Option<usize> {
    let pat = &expected[..PREAMBLE_FRAMES * 2];
    if captured.len() < pat.len() {
        return None;
    }
    // 先比第一个样本再比整段：绝大多数位置在第一次比较就被排除。
    (0..=captured.len() - pat.len())
        .find(|&i| captured[i] == pat[0] && captured[i..i + pat.len()] == *pat)
}

/// 重采样那一档不能靠前导码对齐（前导码本身也被重采样了）——
/// 改用「第一个幅度超过阈值的样本」。
fn find_onset_by_level(captured: &[i16], level: i16) -> Option<usize> {
    captured.iter().position(|&s| s.abs() > level)
}

struct Diff {
    compared: usize,
    mismatches: usize,
    worst: i32,
    first: Option<(usize, i16, i16)>,
    swapped: usize,
}

impl Diff {
    /// 把「实得」当成左右互换的样本来比，看是不是就一致了 ——
    /// 声道互换是一条**会静默发生**的错（见 `lib.rs` 里「半个帧」那段），
    /// 值得单独认出来。
    fn worst_channel_swapped(&self) -> bool {
        self.swapped * 2 > self.mismatches && self.mismatches > 0
    }
}

fn compare(captured: &[i16], expected: &[i16]) -> Diff {
    let n = captured.len().min(expected.len());
    let mut d = Diff {
        compared: n,
        mismatches: 0,
        worst: 0,
        first: None,
        swapped: 0,
    };
    for i in 0..n {
        let (want, got) = (expected[i], captured[i]);
        if want != got {
            d.mismatches += 1;
            let dev = (want as i32 - got as i32).abs();
            d.worst = d.worst.max(dev);
            if d.first.is_none() {
                d.first = Some((i, want, got));
            }
            // 同一帧里左右对调后是否相等？
            if i % 2 == 1 && i >= 1 && expected[i - 1] == got && expected[i] == captured[i - 1] {
                d.swapped += 1;
            }
        }
    }
    d
}

/// 找不到起点时把**诊断数据**打出来 —— 这条判据的价值有一半在这里：
/// 出问题时，用户手里要有能拿去查的东西，而不是一句「不一致」。
fn report_no_onset(captured: &[i16], expected: &[i16]) {
    let nonzero = captured.iter().filter(|&&s| s != 0).count();
    let first_nz = captured.iter().position(|&s| s != 0);
    println!(
        "  ✗ 在录回来的 {} 个样本里找不到期望信号的前导码。",
        captured.len()
    );
    println!("    非零样本 {nonzero} 个，第一个非零在 {first_nz:?}");
    println!("    期望信号前 8 个样本：{:?}", &expected[..8]);
    if let Some(i) = first_nz {
        let end = (i + 8).min(captured.len());
        println!("    录回来第一个非零起的 8 个：{:?}", &captured[i..end]);
    }
}

/// 等 ring 排空（带超时 —— 卡住时要有结论，不能挂死）。
fn wait_drained(engine: &AudioEngine) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while engine.ring_available() > 0 {
        anyhow::ensure!(Instant::now() < deadline, "等了 20 秒环还没排空");
        std::thread::sleep(Duration::from_millis(10));
    }
    // 设备侧缓冲（snd-aloop 是 2 个周期）还没播完。
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// 用 Goertzel/DFT 直接算某个频率上的幅度：`|Σ x[n]·e^{-j2πfn/fs}| · 2/N`。
///
/// 相位无关（取模），所以不需要和信号对齐起点 —— 那正是这一档想要的：
/// 只问「这个频率上还有多少能量」。
fn amplitude_at(samples: &[f32], rate: u32, freq: f64) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, &x) in samples.iter().enumerate() {
        let t = 2.0 * std::f64::consts::PI * freq * n as f64 / rate as f64;
        re += x as f64 * t.cos();
        im -= x as f64 * t.sin();
    }
    2.0 * (re * re + im * im).sqrt() / samples.len() as f64
}
