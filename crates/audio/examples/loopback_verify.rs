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
//!   的内存拷贝连接。这一层不是我们的代码，它**自己出问题**本示例看不出来。
//!   （原文在这里指了一个叫「回环自检」的小节做两遍交叉比对 —— **全文没有那一节**，
//!   是一句够不着的能力承诺，已删。真要做，得让同一次运行比对两遍独立的采集。）
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
/// 频率错、线性插值、丢样本都会**远超**它。实测值会在输出里打出来。
///
/// # 0.05 这个数**凭什么**定在这里（不记下来，将来没人敢动它）
///
/// 它**不是**从重采样器推出来的，是从**实测分布**定的（2026-09-26 本机 snd-aloop，
/// 原始记录见 `.superpowers/sdd/task-11-report.md` §3.2b）：
///
/// | 场景 | 幅度偏差 |
/// |---|---|
/// | 干净环境 | 0.18% |
/// | 系统负载上来了 | 2.52% |
/// | 满载（8 个忙循环） | 22%~65%（失败） |
///
/// ⇒ 它是给**测量环境**留的余量，不是给重采样器留的：干净环境下重采样路径的
/// 实测偏差是上表第一行那个零头。想收紧它的人要先知道 —— **2.5% 那个量级是
/// 真实的环境产物**（snd-aloop 没有流控，机器一忙设备就丢样本），收紧了会在
/// 机器一忙时开始误报，而误报会把这条判据变成「看两次就忽略」的东西。
/// （口径提醒：上表是**任务 11 那次**的记录；两轮修复重跑实测 **0.00~0.03%**
/// （判据 2 两侧的设备侧 xrun 都是 0），与第一行的 **0.18%** 对不上 ——
/// **原因未查明**，见 `.superpowers/sdd/task-11-fix-report.md` §7（复现不出、
/// 无法定论）。方向上也不支持「机器状态不同」这种解释：0.18% 那次记的是
/// **安静机器（load ~1）**上的读数，而修复重跑时的机器更忙、读数反而更低。
/// 本容差的依据是第二行那个 **2.5%** 的环境量级（见上面一段），不是第一行；
/// 而第二、三行那两档的**前提**（设备侧 xrun）在判据 2 的重跑里没再出现 ——
/// 两轮修复的判据 2 都是「采集 0 / 播放 0」
/// （第一轮那张表见 `.superpowers/sdd/task-11-fix-report.md` §1 —— 那两列是按判据
/// 分开记 xrun 的；第三行那个场景 —— 8 个忙循环 —— 也压过、判据 2 同样 0 / 0，
/// 凭据是**当次原始日志** `/tmp/t11-loaded.log:29`，不是复盘文字）
/// ⇒ 那两档既没被复现、也没被推翻，这个差不动它。）
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
) -> Measured {
    let mut attempt = 0u64;
    let mut outcome = match run(lb, attempt) {
        Ok(o) => o,
        Err(e) => {
            println!("  ✗ 跑不起来：{e:#}");
            return Measured {
                outcome: None,
                failed: true,
                retried: false,
            };
        }
    };
    let mut retried = false;
    if !outcome.pass && outcome.disturbed() > 0 {
        println!(
            "  ↻ {name}：这次两侧设备侧 xrun = 采集 {} / 播放 {} —— **测量本身被干扰**了\
             （snd-aloop 没有流控），重测一次。\n                  说明：干净的一次跑失败**不会**重测；这次重测换了一套信号\
             （判据 1 换伪随机种子，判据 2 换频率集）—— 否则回环缓冲里的残留会让\
             「重测通过」变成一句空话。",
            outcome.capture_errors, outcome.playback_errors
        );
        attempt += 1;
        retried = true;
        outcome = match run(lb, attempt) {
            Ok(o) => o,
            Err(e) => {
                println!("  ✗ 重测跑不起来：{e:#}");
                return Measured {
                    outcome: None,
                    failed: true,
                    retried,
                };
            }
        };
    }
    Measured {
        failed: !outcome.pass,
        outcome: Some(outcome),
        retried,
    }
}

/// 一条判据的测量结果（见 [`measure`]）。
struct Measured {
    /// `None` = 根本没跑起来（配置/建流失败），不是「内容不对」。
    outcome: Option<Outcome>,
    failed: bool,
    /// 是否**因为被干扰而重测过**。要一路带到结论里 —— 见 [`verify`]。
    retried: bool,
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
    // ⚠️ **重测历史要带进结论。** 只累加**最终那次**的计数，会在
    //    「第一次被干扰且失败、第二次干净通过」之后打印一句**不带任何折扣**的
    //    「回环验证通过」——唯一痕迹是中段那行提示，而读结论的人往往只看最后一段。
    let mut retried: Vec<&str> = Vec::new();
    for (name, run) in [
        (
            "判据 1/2",
            test_bit_exact as fn(&Loopback, u64) -> anyhow::Result<Outcome>,
        ),
        ("判据 2/2", test_resample),
    ] {
        let measured = measure(name, &lb, run);
        if let Some(o) = measured.outcome {
            // 重测过的话，第一次的扰动计数已经不在手里了（`measure` 只回传最终那次），
            // 所以这里记账用的是「重测过」这个事实本身 —— 那也是结论里缺的那一句。
            disturbances += o.disturbed();
        }
        if measured.retried {
            retried.push(name);
        }
        if measured.failed {
            failed += 1;
        }
    }

    println!("\n== 结论 ==");
    if failed == 0 {
        println!("回环验证通过。⚠️ 它证明的是「到 hw: 为止、我们这条路径」没问题，");
        println!("   **不是**「你听到的是逐位精确的」—— 那需要硬件级采集（见本文件顶部）。");
        if !retried.is_empty() {
            println!(
                "   ⚠️ 而且这是**重测后**才通过的（{}）：第一次因测量链路被干扰而失败，\
                 你看到的数字来自第二次。",
                retried.join("、")
            );
        }
        if disturbances > 0 {
            println!(
                "   ⚠️ 且这次运行两侧设备侧 xrun 合计 {disturbances} 次 —— 结论要再打个折扣，建议重跑。"
            );
        }
        ExitCode::SUCCESS
    } else {
        println!("有 {failed} 项没过 —— 上面每项的实测数字就是线索。");
        // ⚠️ **失败这一支也要带重测历史。** 与上面成功那一支同理：重测过的话，
        //    打印出来的数字是**第二次**的，而「第一次是什么样」已经不在手里了 ——
        //    不说这一句，读结论的人会以为那就是第一次的读数。
        if !retried.is_empty() {
            println!(
                "   （{}**已经重测过一次**（换了信号：判据 1 换伪随机种子、判据 2 换频率集，\
                 见 measure()）仍然没过 —— 上面那一项的实测数字是**第二次**的。）",
                retried.join("、")
            );
        }
        if disturbances > 0 {
            println!(
                "   （本次两侧设备侧 xrun 合计 {disturbances} 次（重测过的那一项只算了\
                 **最后一次**）；先按上面的提示重跑一次，排除「测量链路被饿到」再查代码。）"
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
    // ⚠️ **「逐位一致」的判据里必须带上「全都比到了」。** `compare` 取
    //    `captured.len().min(expected.len())` ⇒ 采集提前结束时它**只比前缀**，
    //    而裁决原来只有 `mismatches == 0` ⇒ 会照样打印「✓ 逐位一致：N 个样本全等」
    //    并以 0 退出。触发很窄（采集环 6 秒、尾部是静音；信号**内部**丢失会变成
    //    mismatch，头部丢失会被 `find_onset` 抓成 None）——但这是本项目第一条
    //    音频正确性判据，它的 pass **不该立在一个它从不检查的长度上**。
    let covered = d.compared == signal.len();
    let outcome = Outcome {
        pass: d.mismatches == 0 && covered,
        capture_errors: cap.device_errors,
        playback_errors,
    };
    if !covered {
        println!(
            "  ✗ 采集**没有盖住整个信号**：比到 {} / {} 个样本就没了 \
             （后面的 {} 个样本根本没录回来）——「逐位一致」这句话不成立，判失败。",
            d.compared,
            signal.len(),
            signal.len() - d.compared
        );
        println!(
            "    这通常意味着采集侧被饿到、或停流时尾巴没交付完；不是样本处理错了。\
             重跑一次；反复出现就看系统负载。"
        );
        return Ok(outcome);
    }
    if d.mismatches == 0 {
        println!(
            "  ✓ 逐位一致：{} / {} 个样本全等（**信号被完整覆盖**，最大偏差 {} LSB）",
            d.compared,
            signal.len(),
            d.worst
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

/// 本档第 `attempt` 次尝试用的频率集（左、右各两个分量）。
///
/// 两条硬约束，换频率时**都要满足**：
///
/// 1. **每个频率在 0.9 秒的分析窗里是整数周期** —— 即频率是 10 的倍数
///    （`f × 0.9 = f × 9/10`，整数 `f` 要让它为整数就得 `f ≡ 0 (mod 10)`）。
///    满足它，矩形窗下四个分量互不泄漏，幅度读数才干净。
/// 2. **远低于源采样率的奈奎斯特**（4000Hz 的奈奎斯特是 2000Hz）。
///    贴着 2000 会退化成恒零（`sin(2π·2000·n/4000) = sin(πn) = 0`），
///    所以本档最大只用到 1480。
///
/// 左右用**不同**的频率对，这一档就顺带验声道有没有接反。
///
/// ⚠️ **换频率只挡住了一个方向。** 它挡住的是「重测拿上次的残留报**假绿**」
/// （见 [`test_resample`] 里那段）；挡不住的是**反方向**：`find_onset_by_level`
/// 从下标 0 开始扫、取第一个过阈值的样本，而残留也是同一量级的正弦 ——
/// 起点可能落进**上一次留下的残留**里，于是分析窗从旧数据开始，
/// **被测内容本身没问题也会判红**。方向是安全的（宁可判红，不放假绿），
/// 但重测失败时要先把这一条排掉再怀疑代码。
fn resample_freqs(attempt: u64) -> ([f64; 2], [f64; 2]) {
    match attempt {
        0 => ([440.0, 1500.0], [600.0, 1200.0]),
        // 重测换一套 —— 理由见 `test_resample` 里那段（残留会让重测拿旧波形报绿）。
        _ => ([520.0, 1480.0], [880.0, 1160.0]),
    }
}

/// 重采样那一档：源采样率设备不支持（[`RESAMPLE_SRC_RATE`]）⇒ 引擎退回设备默认率
/// 并重采样。这里验的是**信号有没有被毁掉**：两个已知频率的幅度还在不在。
///
/// 为什么不是逐位：重采样是滤波，输出与输入的解析值本来就不逐位相等。
/// 为什么幅度就够：**频率错了**（比率错）、**线性插值**（高频滚降）、**丢样本**
/// 都会让某个分量**塌掉**，而通带内的好重采样器在 1% 以内。
///
/// ⚠️ **混叠不在这一档的覆盖范围内 —— 而且是构造上覆盖不到。** 本档唯一的配置是
/// 4000Hz → 48000Hz（12 倍**上**采样），上采样不可能产生混叠；要验它得配一个
/// **降采样**用例（源率高于设备率），本档没配。（以前这里把「混叠」列进能抓到的东西，
/// 那是一句由构造决定测不到的能力断言。）
fn test_resample(lb: &Loopback, attempt: u64) -> anyhow::Result<Outcome> {
    println!("\n== 判据 2/2：重采样路径（{RESAMPLE_SRC_RATE}Hz → 设备默认率）==");
    if attempt > 0 {
        println!("  （第 {} 次重测）", attempt + 1);
    }
    // ⚠️ **每次尝试换一套频率**，不是「用正弦就没问题」。
    //    重测存在的场景正是「上一次跑失败了」，而失败那次留下的残留**就是同一个正弦**；
    //    `find_onset_by_level` 从下标 0 开始扫、取第一个过阈值的样本 ⇒ 那个样本落在
    //    **陈旧残留**里（snd-aloop 保留未读数据，报告自己的 dump 分析在 xrun 后
    //    发现约 0.5 秒无关残留）。于是分析窗口从陈旧数据开始，**若第二次实际什么都
    //    没产出，harness 会拿第一次的波形报「通过」** —— 恰在重试存在的那个场景里
    //    给出假绿。换一套频率之后，残留不可能与本次的期望对上。
    let (freqs_l, freqs_r) = resample_freqs(attempt);
    let signal = dual_sine(
        RESAMPLE_SRC_RATE as usize,
        RESAMPLE_SRC_RATE,
        &freqs_l,
        &freqs_r,
    );

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
    // ⚠️ **起点也要乘 2。** 它上面三行刚承诺「下面全都按帧算」，这里原来写的是
    //    `captured[start + skip..end * 2]` —— 终点按帧乘了 2，起点没有。三个后果：
    //    - 窗口长度变成 `2*span + start + skip` 个样本（实测 0.9309 秒，不是 0.9 秒）；
    //      于是 440Hz 落在 409.2 个周期（`find_onset_by_level` 的起点因运行而异，
    //      所以这个小数每次都不一样）⇒ 矩形窗下**只有它**的幅度读数会动。
    //      量级很小：负频率镜像的泄漏是 `|sin(2πfN/fs)| / (N·|sin(2πf/fs)|)`
    //      = 16.5 / 44640 ≈ **0.037%**（对全部相位取上界，已用 numpy 复核），
    //      远在 5% 容差内 —— 但它是**唯一**会让四个频率读数不一致的项，不是零。
    //    - 窗口起点落在帧 `(start+skip)/2`（≈27ms）而不是 `start+skip`（≈55ms）
    //      ⇒ 保留了 `skip` 本来要避开的**一半**起振瞬态。
    //    - **奇偶**：`start + skip` 为奇数时 `window[0]` 是**右**声道样本，而下面的
    //      `step_by(2)` 把它当 `"L"` 喂进估计器 ⇒ 左右整条对调，四个频率全塌到 ~0
    //      （实测：在真 dump 上把起点挪一个样本，四个读数从 ~10000 掉到 ~7）。
    //      乘 2 之后起点恒为偶数，这条依赖**结构上**消失，不再靠运气。
    let window = &captured[(start + skip) * 2..end * 2];
    let left: Vec<f32> = window.iter().step_by(2).map(|&s| s as f32).collect();
    let right: Vec<f32> = window
        .iter()
        .skip(1)
        .step_by(2)
        .map(|&s| s as f32)
        .collect();

    let mut ok = true;
    for (ch, samples, freqs) in [("L", &left, freqs_l), ("R", &right, freqs_r)] {
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
            // ⚠️ **这里只做一次原子加，不打印。**
            //    错误回调跑在**与数据回调同一个音频线程**上（§16.2 三样全禁：
            //    分配、加锁、IO —— `log::error!` 格式化要分配、要取 logger 的全局锁、
            //    还要写 stderr）。这里以前有一行 `log::error!("采集流错误: {err}")`，
            //    它与本文件顶部「采集回调里只做 T→i16 + 推进 ring」的自述**直接矛盾**，
            //    而且报告里那次 **17 次 xrun 的突发，正是这一行在挨饿的线程上
            //    反复触发的时候** —— 它会给正在饿肚子的线程再添一笔。
            //    次数已经经 `report_measurement` 到用户手里了，解释该放在那里。
            move |_err| {
                errors.fetch_add(1, Ordering::Relaxed);
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
///   信号被插了静音，判据 1 失败、判据 2 的幅度掉到 35%~78%。
///   ⇒ 那次失败是**真的**（实时路径被饿到），只是原因不在样本处理上，而在这里。
///
/// ⚠️ **别去日志里找那次错误的文本。** 那一轮的日志里确实有 17 行
/// `音频流错误: A buffer underrun or overrun occurred`（原始记录：
/// `.superpowers/sdd/task-11-report.md` §3.3），但**当时**引擎的错误回调还会打印它；
/// **后来那次修复（提交 `a53ceb8`）把那行日志删掉了**（见
/// `.superpowers/sdd/task-11-fix-report.md` 的「§1.4 的代价」那段；设计文档 §16.2：
/// 那条回调与数据回调同源，格式化 + 取锁 + 写 stderr 三样都犯，见 `build_output`
/// 里错误回调那段）⇒ 日志里**不会再出现**这一行，回调里只剩一次原子加，能看到的
/// 只有 `device_error_count()` 这个数（见 `AudioEngine::device_error_count` 的文档）。
/// 判「有没有被饿到」看那个数，不看日志。
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
    /// 多数**帧**是不是正好左右对调了 —— 声道互换是一条**会静默发生**的错
    /// （见 `lib.rs` 里「半个帧」那段），值得单独认出来。
    ///
    /// ⚠️ 判据按**帧**数、且与 mismatch 无关。旧写法是
    /// `swapped * 2 > mismatches`，而 `swapped` 只在**不一致的奇数下标**上递增
    /// ⇒ `swapped ≤ mismatches / 2` ⇒ **等号恒成立** ⇒ 严格 `>` 永假
    /// ⇒ 「⚠️ 左右声道**互换**了」这句提示**永远不会为真正的互换打印**。
    /// 反方向它也错：两声道内容相同时（R 里是 L）它会误触发，打印一个对该 bug
    /// 而言是错的诊断。⇒ 现在数的是「**全体帧里有多少帧恰好是期望那帧的对调**」，
    /// 并要求它过半。L==R 的帧对调前后一样，不构成证据，不计。
    ///
    /// （判据本身不依赖这句诊断：真正的全互换会让每个样本都 mismatch，
    /// `pass` 照样是 false。坏的是**诊断**，不是判据 —— 但错的诊断会把查错方向带偏。）
    fn worst_channel_swapped(&self) -> bool {
        let frames = self.compared / 2;
        frames > 0 && self.swapped * 2 > frames
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
        }
    }
    // 第二遍**按帧**数「这一帧是不是正好是期望那一帧的左右对调」。
    // 与上面那遍分开是因为口径不同：上面数**样本**，这里数**帧** ——
    // 混在一个循环里正是旧写法出错的地方（见 `worst_channel_swapped` 的文档）。
    for f in 0..n / 2 {
        let (i, j) = (f * 2, f * 2 + 1);
        let (wl, wr) = (expected[i], expected[j]);
        let (gl, gr) = (captured[i], captured[j]);
        // L == R 的帧对调前后一样 —— 不构成「互换」的证据，别拿来凑数。
        if wl != wr && gl == wr && gr == wl {
            d.swapped += 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **真正的全 L/R 互换必须被判出来。** 这条是回归测试：旧写法
    /// （`swapped * 2 > mismatches`，而 `swapped` 只在不一致的奇数下标上递增）
    /// 恒推出 `swapped ≤ mismatches / 2` ⇒ 严格 `>` 永假 ⇒ 提示永远不打印。
    #[test]
    fn detects_a_real_channel_swap() {
        // 4 帧，L/R 内容不同 —— 交换之后每一帧都看得出来。
        let expected: Vec<i16> = vec![10, 20, 30, 40, 50, 60, 70, 80];
        let swapped: Vec<i16> = vec![20, 10, 40, 30, 60, 50, 80, 70];
        let d = compare(&swapped, &expected);
        assert_eq!(d.mismatches, 8, "全互换时每个样本都不一致");
        assert!(
            d.worst_channel_swapped(),
            "全 L/R 互换必须被认出来（这是旧写法抓不到的那一档）"
        );
    }

    /// **反方向也要对**：两声道内容相同时，交换与不交换一模一样 ——
    /// 那不是「互换」的证据，不该打印那句诊断（旧写法在这里会误触发）。
    #[test]
    fn does_not_report_a_swap_when_channels_are_identical() {
        let expected: Vec<i16> = vec![7, 7, 9, 9, 11, 11, 13, 13];
        let d = compare(&expected, &expected);
        assert_eq!(d.mismatches, 0);
        assert!(
            !d.worst_channel_swapped(),
            "两声道内容相同（R 里是 L）不是互换，不能打印互换诊断"
        );
    }

    /// 采集短了的时候 `compare` **只比前缀** —— 判据必须自己要求「全都比到了」。
    /// 这条钉住 `test_bit_exact` 里那个 `covered` 依据的事实。
    #[test]
    fn compare_only_covers_what_it_was_given() {
        let expected: Vec<i16> = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let short: Vec<i16> = vec![1, 2, 3, 4];
        let d = compare(&short, &expected);
        assert_eq!(d.compared, 4, "只比到能给出来的长度");
        assert_eq!(
            d.mismatches, 0,
            "前缀一致 ⇒ mismatches 是 0（这正是旧裁决的漏洞）"
        );
        assert!(
            d.compared < expected.len(),
            "⇒ `covered` 必须为假，否则「逐位一致」会立在 4/8 个样本上"
        );
    }

    /// 重测换的那套频率必须同时满足两条硬约束，否则重测本身会变成假绿/假红：
    /// ① 0.9 秒窗口里整数周期（⇒ 10 的倍数）；② 远低于源率的奈奎斯特（2000Hz）。
    #[test]
    fn retry_frequency_sets_are_usable_and_different() {
        let a = resample_freqs(0);
        let b = resample_freqs(1);
        assert_ne!(a, b, "重测必须换一套频率，否则残留会让「重测通过」变成空话");
        for set in [a, b] {
            for f in set.0.iter().chain(set.1.iter()) {
                assert_eq!(
                    f % 10.0,
                    0.0,
                    "{f}Hz 在 0.9 秒里不是整数周期（f×0.9 要是整数 ⇒ f 得是 10 的倍数）"
                );
                assert!(
                    *f < 2000.0,
                    "{f}Hz 达到或超过源率 {RESAMPLE_SRC_RATE}Hz 的奈奎斯特（2000Hz）—— \
                     贴到 2000 会退化成恒零信号"
                );
            }
            assert_ne!(
                set.0, set.1,
                "左右必须用不同的频率对，否则这一档验不了声道接反"
            );
        }
    }
}
