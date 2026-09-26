//! dslsmusic 的 CLI 入口。两个职责：
//!
//! - **M0 路径**（不带子命令，或显式 `render`）：SDL3 窗口 + wgpu 三角形 ——
//!   验证渲染链路与帧时间统计。
//! - **M1 路径**（`login` / `play`）：扫码登录 → 搜索 → 取流 → 解码 → 出声。
//!
//! ⚠️ **两条路径都在这里，不是漏删 M0 那段。** M0/M1 的验收标准里
//! **同时**有「`cargo run -p dslsmusic` 弹出窗口」与
//! 「`cargo run -p dslsmusic -- play "<歌名>"` 能出声」两条。把 main.rs
//! 换成纯 CLI 会让前者**直接失效**（clap 会报「需要子命令」并以退出码 2 结束）。
//! 所以子命令是**可选**的，缺省即渲染。

use clap::{Parser, Subcommand};
use dsls_audio::AudioEngine;
use dsls_platform::{PlatformEvent, PlatformWindow, init_gpu};
use dsls_provider_api::{MusicSource, Quality};
use dsls_provider_netease::{NeteaseClient, QrState};
use dslsmusic::metrics;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(about = "dslsmusic —— 渲染链路（M0）+ 音乐链路（M1）")]
struct Cli {
    /// **可选**：不带子命令时走 `render`（见模块文档）。
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 渲染 1280×720 窗口 + 三角形（M0 的验收路径）
    Render,
    /// 扫码登录，把会话存到 ~/.config/dslsmusic/session
    Login,
    /// 搜索并播放第一首结果
    Play {
        keywords: String,
        /// 音质档位：low | standard | high | lossless | hires
        #[arg(long, default_value = "high")]
        quality: String,
        /// 指定输出设备（ALSA 的 PCM 名字，如 `hw:CARD=1,DEV=0`）。
        ///
        /// ⚠️ **硬性**：打不开就报错，不会偷偷换一台设备播放。不给这个参数时走
        /// 自动选路：先试设备独占（拿不到就**如实退回**共享路径，不会静音）。
        /// 可选值见 `cargo run -p dsls-audio --example probe_devices`。
        #[arg(long)]
        device: Option<String>,
        /// 把**推进 ring 的样本**另存一份 16-bit PCM WAV 到这个路径。
        ///
        /// 采样率写**源采样率**、2 声道交错 —— 即**解码器的输出**
        /// （重采样在 `AudioEngine` 内部，是这条链路之后的事）。
        ///
        /// 为什么要有这个开关：**VNC 的 RFB 协议没有音频通道**，
        /// 「用耳朵当场验收」在那类环境下做不通 —— 而这个文件可以带走、
        /// 可以反复验。⚠️ 它产出的是**证据**，不是判决：文件能打开、时长对，
        /// 不等于「声音是对的」，后者仍要人听。
        #[arg(long, value_name = "PATH")]
        dump_wav: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ⚠️ **必须显式给默认过滤级别**：裸的 `env_logger::init()` 在 `RUST_LOG`
    // 未设时会退到 `env_filter` 的内建 directive `LevelFilter::Error`
    // （`env_filter-2.0.0/src/filter.rs:151`）⇒ **`warn!` 与 `info!` 全被过滤掉**。
    //
    // 这不是「日志多一点少一点」的偏好问题：provider 里那些诊断日志
    // （如 `provider-netease/src/parse.rs` 的试听片段告警）**只在第一次真实
    // 触发时有价值**，而它们的触发路径今天不可达 —— 默认配置下看不见，
    // 等于那条诊断不存在，出问题时用户手里没有任何线索。
    // `warn` 是「默认就该被看见」的下限。要更啰嗦仍可 `RUST_LOG=info`。
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match Cli::parse().cmd {
        // M0 的渲染循环是**同步**的，缺省臂必须与 `Render` 走同一条路径。
        None | Some(Cmd::Render) => render_triangle(),
        Some(Cmd::Login) => login().await,
        Some(Cmd::Play {
            keywords,
            quality,
            device,
            dump_wav,
        }) => play(&keywords, &quality, device.as_deref(), dump_wav.as_deref()).await,
    }
}

/// 会话目录：`~/.config/dslsmusic`（平台差异交给 `dirs`）。
fn session_path() -> std::path::PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| ".".into());
    p.push("dslsmusic");
    p
}

/// 读回登录时落盘的会话 cookie 串。没登录过就是 `None`（以游客身份跑）。
fn load_cookie() -> Option<String> {
    std::fs::read_to_string(session_path().join("session")).ok()
}

/// CLI 的档位字符串 → [`Quality`]。
///
/// ⚠️ **未知档位返回 `Err`，不是悄悄当成 `High`。** 计划的草稿写的是
/// `_ => Quality::High`，那会让 `--quality lossles`（少一个 `s`）**静默播
/// 320k** —— 用户以为拿到了无损，拿到的是有损，而没有任何信号。
/// 这与本项目反复禁止的「静默的谎言」同类；对照
/// `provider-netease/src/client.rs::level_for`：那里遇到未知变体也是
/// `Err(Protocol)` 而不是随便挑一档。
fn quality_from_str(s: &str) -> anyhow::Result<Quality> {
    Ok(match s {
        "low" => Quality::Low,
        "standard" => Quality::Standard,
        "high" => Quality::High,
        "lossless" => Quality::Lossless,
        "hires" => Quality::HiRes,
        other => {
            anyhow::bail!("未知音质档位 {other:?}；可用：low | standard | high | lossless | hires")
        }
    })
}

/// `login`：扫码 → 轮询 → 把会话写盘。
///
/// **只做扫码，不做任何写操作**（签到 / 打卡 / 云盘上传）—— 设计文档 §4.4 硬约束。
async fn login() -> anyhow::Result<()> {
    let client = NeteaseClient::new(None);
    let key = client.qr_key().await?;
    let png = client.qr_image(&key).await?;

    let dir = session_path();
    std::fs::create_dir_all(&dir)?;
    let qr_path = dir.join("qrcode.png");
    std::fs::write(&qr_path, &png)?;

    println!("二维码已保存到: {}", qr_path.display());
    println!("用网易云音乐 App 扫描它，然后在手机上确认。");
    println!("（在图形界面下可以直接打开这个 PNG）");

    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match client.qr_check(&key).await? {
            QrState::WaitingScan => print!("."),
            QrState::WaitingConfirm => print!("*"),
            QrState::Expired => anyhow::bail!("二维码已过期，请重新运行 login"),
            QrState::Confirmed { cookie } => {
                // **不需要判空**：`from_code` 已改成返回 `Result`，
                // 拿不到会话会是 `Err(Protocol)`，在上一行的 `?` 就返回了。
                // 这里再判一次就是不可能到达的死代码。
                std::fs::write(dir.join("session"), &cookie)?;
                println!("\n登录成功，会话已保存。");
                return Ok(());
            }
        }
        // 轮询进度是 `.` / `*` 逐字符打的，不 flush 就看不到（行缓冲）。
        let _ = std::io::stdout().flush();
    }
}

/// `play`：搜索 → 取流 → 下载 → 解码 → 出声（可选：同时 dump 一份 WAV）。
async fn play(
    keywords: &str,
    quality: &str,
    device: Option<&str>,
    dump_wav: Option<&Path>,
) -> anyhow::Result<()> {
    let cookie = load_cookie();
    if cookie.is_none() {
        log::warn!("未找到会话，将以游客身份播放（可能受限）。先跑 `login` 更稳。");
    }
    let client = NeteaseClient::new(cookie);

    let q = quality_from_str(quality)?;

    // 经**抽象**调用，不碰具体类型 —— M1 存在的理由就是验证这条链路。
    // `Ok(None)` 表示「本音源不处理搜索」，与「没搜到」是两回事（§4.5）：
    // 前者该换音源，后者就是答案本身。所以下面的 `.ok_or_else` 是对的，
    // 而 `tracks.is_empty()` 走的是「查到了但没结果」。
    let src: &dyn MusicSource = &client;

    println!("搜索: {keywords}");
    let tracks = src
        .search(keywords, 5)
        .await?
        .ok_or_else(|| anyhow::anyhow!("本音源不支持搜索（Unsupported）"))?;
    if tracks.is_empty() {
        println!("没有结果。");
        return Ok(());
    }
    for (i, t) in tracks.iter().enumerate() {
        println!(
            "  [{}] {} — {}  ({}ms)",
            i + 1,
            t.title,
            t.artist,
            t.duration_ms
        );
    }

    let track = &tracks[0];
    println!("\n解析播放地址: {} — {}", track.title, track.artist);
    let url = src
        .stream_url(&track.id, q)
        .await?
        .ok_or_else(|| anyhow::anyhow!("本音源不支持取流（Unsupported）"))?;
    println!("得到地址（{} 字符）", url.len());

    // ⚠️ **全量下载到内存再解码，这不是偷懒。** `decode_to_i16` 收的是
    // `Box<dyn MediaSource>`，即 `Read + Seek`；HTTP 流式响应**不可 seek**，
    // 而 symphonia 的格式探测与 mp3 回退都要 seek。所以 `Cursor<Vec<u8>>`
    // （symphonia 为它实现了 `MediaSource`）是当前接口下唯一正确的喂法。
    let resp = reqwest::get(&url).await?;
    // ⚠️ reqwest 对 4xx/5xx **不报错**。不查状态码的话，CDN 返回的 403
    // 错误页会被原样喂给解码器，报出来的是「解码失败」——
    // 而真实原因是「拿不到文件」，两者差着一次排查。
    anyhow::ensure!(
        resp.status().is_success(),
        "下载播放地址失败：HTTP {}",
        resp.status()
    );
    let bytes = resp.bytes().await?;
    println!("已下载 {} KB", bytes.len() / 1024);

    // ⚠️ **先 probe，再建引擎。** 引擎必须按**源采样率**建流才不用重采样
    // （见 `dsls_audio::AudioEngine::new`），而建流不可事后修改 —— 所以源率要在
    // 解码**之前**知道。`decode_to_i16` 的返回值来得太晚，Task 9 就是把它丢掉、
    // 按设备率建的流，于是 44.1kHz 的歌在 48kHz 设备上变成了 1.0884 倍速，
    // **而 xrun 恒 0、不报任何错**（实测 42.893 秒的歌 39.4 秒放完）。
    //
    // 做两次 `Cursor` 是**故意**的：`bytes` 是引用计数的 `Bytes`，`clone()` 只是
    // 加一次计数，不会把整首歌复制一遍。symphonia 的 `MediaSource` 对
    // `Cursor<T: AsRef<[u8]>>` 有实现（已核实·源码
    // `symphonia-core-0.5.5/src/io/mod.rs:77`），所以两次都能喂。
    let probed =
        dsls_audio::decode::probe_sample_rate(Box::new(std::io::Cursor::new(bytes.clone())))?;
    // ⚠️ 容器**没写**采样率时不要装作知道：44100 只是 `decode_to_i16` 的同一个兜底
    // 值，猜错了就是变速变调。至少把「我在猜」说出来。
    let src_rate = probed.unwrap_or_else(|| {
        log::warn!(
            "容器未声明采样率，按 44100Hz 建输出流（猜测）。若实际不是 44100，\
             这首歌会变速变调 —— 解码时会再报一次实际值。"
        );
        44100
    });

    // `--device` 是**硬**语义（用户点名要这台设备，打不开就报错，不偷偷换一台）；
    // 不给就走自动选路：先试独占，拿不到就如实退回共享 —— 见 AudioEngine 的文档。
    let mut engine = match device {
        Some(id) => AudioEngine::new_on(id, src_rate)?,
        None => AudioEngine::new(src_rate)?,
    };
    // **两条边界必须一起说**，它们的口径不同：
    //   ① is_passthrough：「**我们**没重采样」；
    //   ② output_path：「**系统混音器**在不在我们与设备之间」。
    // Task 10 的教训就是只知道 ① 会得出「已经 bit-perfect」的错觉 ——
    // 那时 `is_passthrough() == true`，而 44.1kHz 仍是 PipeWire 在重采样。
    println!("输出路径：{}", engine.output_path());
    println!("  ⚠️ {}", engine.output_path().caveat());
    println!(
        "  设备侧：{}Hz {}ch 格式 {:?}；我们这一层{}重采样{}",
        engine.output_rate(),
        engine.output_channels(),
        engine.output_format(),
        if engine.is_passthrough() {
            "未"
        } else {
            "已"
        },
        if engine.is_passthrough() {
            ""
        } else {
            "（rubato FFT 同步重采样器，抗混叠；不是线性插值）"
        }
    );
    if let Some(note) = dsls_audio::output_path::platform_note() {
        println!("  {note}");
    }

    // ── `--dump-wav` ──────────────────────────────────────────────────────
    // ⚠️ **建文件失败不中断播放**（dump 是辅助手段，不是主路径），
    // 但**必须出声**：用户以为文件在写、跑完才发现什么都没有，比当场报错更糟。
    // 用 `log::error!` 而不是 `eprintln!`，与文件里其它非致命错误同一口径
    // （默认过滤级别是 `info`，所以它一定会被看见）。
    let mut dump = match dump_wav {
        Some(path) => match WavDump::create(path, src_rate) {
            Ok(d) => {
                println!(
                    "WAV dump：{}（源率 {src_rate}Hz、{WAV_CHANNELS} 声道、16-bit PCM）",
                    path.display()
                );
                Some(d)
            }
            Err(e) => {
                log::error!(
                    "打不开 dump 路径 {}（{e}）—— **这次不写 WAV，播放继续**。\
                     检查父目录是否存在、有没有写权限。",
                    path.display()
                );
                None
            }
        },
        None => None,
    };

    println!("开始播放…（Ctrl-C 停止）");

    let cursor = std::io::Cursor::new(bytes);

    // 闭包借用 engine 与 pcm（`push_samples` 要 `&mut`），闭包结束借用即释放，
    // 所以后面的 `ring_available_frames()` / `xrun_count()` 可以直接用。
    const CHUNK: usize = 4096;
    // v1 是空图（零开销）；v2 的 EQ / 响度 / 变速都挂在这里。
    // ⚠️ **它跑在【源采样率】上** —— 重采样是图**之后**的速率边界，
    // 不是链上的一环（见 `dsls_audio::AudioEngine` 的文档）。v2 写 stage 时
    // 按源率算系数，不要按设备率。
    let mut pcm = dsls_audio::PcmGraph::new();
    let decoded_rate =
        dsls_audio::decode::decode_to_i16(Box::new(cursor), Some("mp3"), &mut |samples| {
            for chunk in samples.chunks_mut(CHUNK) {
                pcm.process(chunk); // ← 解码与重采样之间：DSP 的位置（源率）
                engine.push_samples(chunk); // ← 引擎内部换算到设备率后才进 ring
                // ⚠️ **与 `push_samples` 同一批字节**，不是「ring 取出的样本」：
                // 回环判据已经证明推进去的东西原样从 `hw:` 出来，两件事合起来
                // 覆盖整条路径。而这一批正是**解码器的输出**（重采样在
                // `AudioEngine` 内部，是这条链路之后的事）—— 要验的就是它。
                if let Some(d) = dump.as_mut() {
                    d.write(chunk);
                }
            }
            // 背压：`push_samples` 在环满时**丢数据**（计入 `dropped_frames()`，
            // 见 crates/audio/src/output.rs 的既定语义），而 `AudioEngine` 没有把
            // 丢弃暴露出来 ⇒ 这里不压住就是**静默丢样本**。
            // 阈值取环容量的 3/4 —— 环容量按**输出率**算出来的，所以这里不能写死
            // `44100 * 3`：48kHz 设备上那个数字不是 3 秒。
            // ⚠️ **分子分母必须同单位**：两个方法现在都是**帧**（改名带 `_frames`
            //    就是为了让这件事写不错 —— 帧化之前它们是样本）。
            while engine.ring_available_frames() > engine.ring_capacity_frames() / 4 * 3 {
                std::thread::sleep(Duration::from_millis(20));
            }
        })?;

    // ⚠️ **必须排空重采样器的尾巴**：它内部还压着启动延迟那么多的输出帧，
    // 不排空的话结尾会被吞掉一截（听不出来，但**时长会短**）。
    engine.finish_stream();

    // ⚠️ **dump 在「解码结束」这一刻就收尾，不等缓冲播完。**
    // 头的两个长度字段要 seek 回去补，补完文件才算数；而下面那个排空循环还要
    // 几秒，期间 Ctrl-C 会直接杀掉进程 —— 那正好落在「内容已写满、头还是占位值」
    // 的窗口里。收尾越早，这个窗口越小。
    if let Some(d) = dump.as_mut() {
        match d.finish() {
            Ok(()) => {
                let secs = d.seconds();
                println!(
                    "WAV 已写出：{}（{secs:.3} 秒，{src_rate}Hz，{WAV_CHANNELS} 声道，16-bit PCM{}）",
                    d.path().display(),
                    if d.incomplete() {
                        "；⚠️ 写的过程中出过错，这个文件**不完整**"
                    } else {
                        ""
                    }
                );
                // 与曲目元数据里的时长对一下 —— 这是 Task 10 那条「掐表」判据的
                // 离线版。⚠️ 元数据本身常有取整误差（普遍在零点几秒），所以
                // 差在 1 秒内说明不了什么；超过 1 秒才值得查。**这里不下结论**，
                // 只把两个数和差值摆出来。
                let meta = track.duration_ms as f64 / 1000.0;
                if (secs - meta).abs() > 1.0 {
                    log::warn!(
                        "dump 时长 {secs:.3}s 与曲目元数据 {meta:.3}s 差 {:+.3}s —— \
                         值得查（常见原因：解码被截断、容器声明与实际采样率不符）。\
                         注意元数据本身也有取整误差。",
                        secs - meta
                    );
                }
            }
            // 收尾失败 = 头还是占位值（长度写 0）。那种文件多数播放器照放，
            // 但它是坏的 —— 必须说清，不能让它冒充证据。
            Err(e) => log::error!(
                "WAV 收尾失败（{}）：{e} —— 头里的两个长度字段仍是占位值 0，\
                 **这个文件不能当证据用**。播放未受影响。",
                d.path().display()
            ),
        }
    } else if dump_wav.is_some() {
        println!("WAV dump：**没有产出**（建文件时就失败了，见上方的错误）—— 播放未受影响。");
    }

    // 解码报出来的实际采样率。**它必须与建流用的那个一致**，否则就是变速变调。
    // `decode_to_i16` 内部已经在两者不一致时告警了（容器声明 vs 解码器实际），
    // 这里是最后一道：万一 probe 与 decode 分叉，至少不让它静默通过。
    if decoded_rate != src_rate {
        log::error!(
            "probe 报 {src_rate}Hz 但解码实际是 {decoded_rate}Hz —— \
             输出流是按前者建的，这首歌会以 {:.4} 倍速播放（音高也会变）。",
            decoded_rate as f64 / src_rate as f64
        );
    }

    // ⚠️ **xrun 必须在「解码结束」这一刻采一次，不能只看收尾的总数。**
    // 环被**自然读空**（歌放完了）时回调必然记一次欠载 —— 那是流的终点，
    // 不是故障。真正有意义的口径是「还有数据可推的时候，音频线程有没有被
    // 抽干」，也就是解码结束这一刻的计数：此时背压刚把环维持在 ~3 秒，
    // 这一刻的读数是干净的。
    let xrun_during_playback = engine.xrun_count();

    // 等缓冲播完（否则 main 一返回，AudioEngine 析构 = 立刻断声）。
    while engine.ring_available_frames() > 0 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let xrun_total = engine.xrun_count();
    let xrun_tail = xrun_total.saturating_sub(xrun_during_playback);
    // 反方向的那一半：环被**填满**、生产者丢帧。**单调累加**，所以取收尾后的
    // 总数即可 —— 任何一次非零都是缺陷，不像 xrun 要避开「歌放完、环自然读空」那一次。
    let dropped = engine.dropped_frames();

    println!("\n播放结束。");
    println!(
        "采样率：源 {src_rate}Hz → 输出 {}Hz（{}）；输出路径：{}",
        engine.output_rate(),
        if engine.is_passthrough() {
            "直通，未重采样"
        } else {
            "已重采样"
        },
        engine.output_path()
    );
    // ⚠️ **路径后面必须跟着边界。** `OutputPath` 的 `Display` 只打类型与设备
    //    （见 `crates/audio/src/output_path.rs` 里那个 impl），而收尾这几行才是
    //    用户拿来判断「我到底拿到了什么」的地方 —— 单独读「直通 + Exclusive」
    //    正是本任务要防的那个虚假逐位精确印象。
    //    启动时打过一次不代表这里能省（读结论的人常常只看最后几行）；
    //    这里**调用** `engine.output_path().caveat()` 而不是抄一份文本，
    //    免得造出第二处会漂移的事实。
    println!("  ⚠️ {}", engine.output_path().caveat());
    println!(
        "xrun：播放期间 = {xrun_during_playback}（这条必须为 0）；\
         总计 = {xrun_total}（其中收尾抽干 {xrun_tail} 次 —— 歌放完、环读空，\
         回调必然记一次，不是故障。口径见 crates/audio/src/lib.rs 与设计文档 §16.2）"
    );
    // ⚠️ 上面那个数字**只覆盖「环被抽干」这一个方向**。反方向的另一半是
    //    「环被填满、生产者丢帧」—— 它此前**没有任何指标覆盖**：
    //    WAV 文件完整、xrun 为 0、波形统计正常，都**不代表你听到的等于我们产出的**。
    //    `dropped_frames() == 0` 才是这条的唯一证据（设计文档 §16.2）——
    //    ⚠️ 但「为 0」不等于「一个样本都没少」：流的末尾还可能有**半个帧**
    //    （总样本数为奇数时最后那个样本），它不算丢弃、不在任何计数器里，
    //    也播不出来（`AudioEngine::finish_stream` 会为它记一条 warn —— 不是 `dropped_frames`）。
    //    下面那行打印出来的话因此**只覆盖丢弃这一个方向**，不要读成「零即无损」。
    // ⚠️ **单位是帧，不是「次数」**（帧化时一起改的，理由见 `AudioEngine::dropped_frames`）。
    //    非零时它可以直接换算成「丢了多少秒音频」——「丢了一次」和「丢了一千帧」
    //    是两个量级的事故，标签必须说得出这个区别。
    println!(
        "ring 丢弃（生产者丢帧）：{dropped}（这条也必须为 0 —— 非零说明\
         你听到的**不等于我们产出的**。单位是**帧**：{dropped} ÷ {}Hz ≈ {:.3} 秒的音频\
         没播出来。它和上面的 xrun 是**相反方向**的一对：那个数环被抽干\
         （消费者饿、我们喂不上），这个数环被填满（生产者丢、我们不喂了）。\
         两个方向都要看。⚠️ 两者**单位不同**（那个按事件计），数字之间没有\
         可比性 —— 各自判是否为 0）",
        engine.output_rate(),
        dropped as f64 / engine.output_rate() as f64
    );
    // ⚠️ 上面的数字**只覆盖「我们自己的 ring 被抽干」**：设备卡顿而我方缓冲有货时，
    // 它照样是 0（设计文档 §16.2 的口径）。设备侧那一半单独报 —— 它以前只进日志，
    // 听感上是「断了一下」，而指标上完全隐形。
    // ⚠️ **标签要跟计数器一样宽。** 这个数不是「ALSA 的 xrun 次数」，是「cpal 的错误
    //    回调被调了几次」——ALSA 的 underrun/overrun 走这条，设备拔出等也走同一条
    //    （见 `AudioEngine::device_error_count` 的文档）。标签写窄了，用户会把别的
    //    设备侧错误读成 xrun，然后往错的方向查。
    println!(
        "设备侧报错（cpal 错误回调被调的次数 —— ALSA 的 underrun/overrun 走这条，\
         设备拔出等也走同一条）：{}（这条也必须为 0 —— 非零说明设备侧出过错；\
         最常见的原因是实时路径被饿到，而上面的 xrun 计数看不见它）",
        engine.device_error_count()
    );
    Ok(())
}

// ───────────────────────── `--dump-wav` 的落盘器 ─────────────────────────
//
// **自己写这 44 字节，零新依赖**（设计文档 §18.8 已论证过这条路：
// `data` 偏移 36、payload 起始 44、RIFF size = 全长 − 8）。

/// WAV 头固定 44 字节 —— 因为 16-bit PCM 的 `fmt ` 块是 16 字节，没有附加字段。
const WAV_HEADER_LEN: usize = 44;
/// **恰好 2 声道**：与 `dsls_audio::decode::decode_to_i16` 的契约一致
/// （那个函数无条件把相邻两个样本当成一帧的 L/R）。
const WAV_CHANNELS: u16 = 2;
const WAV_BITS: u16 = 16;
/// 1 帧的字节数 = `block_align`。
const WAV_FRAME_BYTES: u16 = WAV_CHANNELS * WAV_BITS / 8;
/// `RIFF` 的长度字段 = **本字段之后**的字节数 = 4("WAVE") + 8+16("fmt ") + 8+data。
const WAV_RIFF_OVERHEAD: u32 = 36;

/// `--dump-wav` 的落盘器：16-bit PCM、2 声道，**采样率写源率**。
///
/// **dump 的是「推进 ring 的那一批样本」**，不是「ring 取出的样本」。
/// ⚠️ **采样率必须写源率**：`push_samples` 收的就是源率样本（重采样在
/// `AudioEngine` 内部，是这条链路之后的事），写设备率会把一个 44.1kHz 的文件
/// 标成 48kHz —— 时长与音高跟着错 8.84%，而那**不是格式小瑕疵，是把证据做假**。
///
/// ⚠️ **两个长度字段收尾时必须回填**（[`WavDump::finish`]）。不补的话多数
/// 播放器仍能放（它们容忍），**但那是个坏文件** —— 而一个「能放」的坏文件
/// 比一个打不开的文件更危险：它看起来像证据。
struct WavDump {
    file: std::fs::File,
    /// 出错时要能说清是**哪个文件**，所以路径要留着。
    path: PathBuf,
    /// 源采样率（Hz），直接写进头。
    rate: u32,
    /// 已写出的**帧**数（1 帧 = L、R 两个 i16）。收尾时用它和磁盘上的真实
    /// 字节数对账 —— 两者不等就说明中途写失败过。
    frames: u64,
    /// 头里 `data` 字段的**最终**值（收尾时按真实长度定），也是算秒数的依据。
    data_len: u32,
    /// i16 → 小端字节的中转缓冲。复用以免每 4096 个样本就分配一次。
    buf: Vec<u8>,
    /// **中间出过事**（写失败、或磁盘上的长度与预期对不上）—— 此时文件不能
    /// 冒充「完整的证据」。名字不叫 truncated：写失败也可能一个字节都没写进去，
    /// 说「截断」是**过度断言**，说「不完整」才是真的。
    incomplete: bool,
}

impl WavDump {
    /// 建文件 + 先写**占位**的头：`data` 有多长此刻还不知道，见 `finish`。
    fn create(path: &Path, rate: u32) -> std::io::Result<Self> {
        let mut file = std::fs::File::create(path)?;
        file.write_all(&Self::header(rate, 0))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            rate,
            frames: 0,
            data_len: 0,
            buf: Vec::new(),
            incomplete: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// 44 字节的头。`data_len` 是 `data` 段的**字节**数，不是帧数。
    fn header(rate: u32, data_len: u32) -> [u8; WAV_HEADER_LEN] {
        let mut h = [0u8; WAV_HEADER_LEN];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&(WAV_RIFF_OVERHEAD + data_len).to_le_bytes());
        h[8..12].copy_from_slice(b"WAVE");
        // ⚠️ 是 "fmt "（**4 字节，带尾空格**），不是 "fmt"。
        h[12..16].copy_from_slice(b"fmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes()); // fmt 块长度：PCM 固定 16
        h[20..22].copy_from_slice(&1u16.to_le_bytes()); // 1 = 整数 PCM（不是 IEEE float）
        h[22..24].copy_from_slice(&WAV_CHANNELS.to_le_bytes());
        h[24..28].copy_from_slice(&rate.to_le_bytes());
        h[28..32].copy_from_slice(&(rate * u32::from(WAV_FRAME_BYTES)).to_le_bytes()); // 字节率
        h[32..34].copy_from_slice(&WAV_FRAME_BYTES.to_le_bytes());
        h[34..36].copy_from_slice(&WAV_BITS.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&data_len.to_le_bytes());
        h
    }

    /// 写一批**源率、2 声道交错**的 i16。
    ///
    /// ⚠️ **失败只报一次、且不向上传播**：dump 是辅助手段不是主路径，
    /// 不能因为它写不进去就让整首歌播不成。重复报也不行 —— 失败会在**每个**
    /// chunk（4096 个样本 ≈ 46ms）重演，不设闸就是每 46ms 刷一条同样的错误，
    /// 把真正该看的输出淹掉。
    fn write(&mut self, samples: &[i16]) {
        if self.incomplete {
            return;
        }
        self.buf.clear();
        // 复用缓冲：满了就不再分配（第一批之后 reserve 都是 no-op）。
        self.buf.reserve(samples.len() * 2);
        for s in samples {
            self.buf.extend_from_slice(&s.to_le_bytes());
        }
        // 先记账再写：`finish` 用**磁盘上的真实长度**兜底，所以即使这里写到
        // 一半失败，账错了也不会让头撒谎。⚠️ 顺带一提，`samples.len() / 2` 的
        // 向下取整也是「解码输出必须是偶数个 i16」这条契约的哨兵：万一哪天它
        // 被破坏（单声道源没归一化），账就会和真实长度对不上，`finish` 会报出来
        // —— 而不是悄悄产出一个错位的 WAV。
        self.frames += (samples.len() / 2) as u64;
        if let Err(e) = self.file.write_all(&self.buf) {
            self.incomplete = true;
            log::error!(
                "写 WAV 失败（{}）：{e} —— **dump 到此中断，播放继续**。\
                 已写进去的部分收尾时会按真实长度补头，但它不再是完整的证据。",
                self.path.display()
            );
        }
    }

    /// 回填 `RIFF` 与 `data` 两个长度字段。
    ///
    /// ⚠️ **用磁盘上的真实长度，不用帧计数。** 写失败可能停在某个 chunk 中间，
    /// 那时计数比文件里真有的字节多；拿计数去回填会把 `data` 写得比 payload 长
    /// —— 那正是「头撒谎」的坏文件，而它**照样能被多数播放器打开**。
    /// 真实长度还顺带校验了计数：两者不等就说明中途出过事，一并标出来。
    fn finish(&mut self) -> anyhow::Result<()> {
        let bytes = self
            .file
            .metadata()?
            .len()
            .saturating_sub(WAV_HEADER_LEN as u64);
        let expected = self.frames * u64::from(WAV_FRAME_BYTES);
        if bytes != expected {
            self.incomplete = true;
            log::warn!(
                "dump 记的是 {expected} 字节、文件里实际是 {bytes} 字节 —— 中间出过事。\
                 头按**实际**长度补（这样文件自身是自洽的、能打开），\
                 但它的内容不完整，**别拿它当证据**。"
            );
        }
        // RIFF 的长度字段是 u32：能表达的 payload 上限是 4GiB − 44B
        // （≈ 6.7 小时的 44.1kHz 立体声）。超了就没有诚实的头可写 —— 报错，
        // 不写一个截断冒充的数字。
        anyhow::ensure!(
            bytes <= u64::from(u32::MAX) - u64::from(WAV_RIFF_OVERHEAD),
            "dump 数据段 {bytes} 字节，超出 RIFF 的上限（4GiB − 44B）—— 写不出诚实的头，\
             这个文件不可用",
        );
        self.data_len = u32::try_from(bytes)?;
        // 两个字段在头里不挨着：RIFF size 在偏移 4，data size 在偏移 40。
        self.file.seek(SeekFrom::Start(4))?;
        self.file
            .write_all(&(WAV_RIFF_OVERHEAD + self.data_len).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(40))?;
        self.file.write_all(&self.data_len.to_le_bytes())?;
        Ok(())
    }

    /// 已写出的**实际**秒数 —— 按头里那个 `data` 长度算，不按「以为写了多少」。
    fn seconds(&self) -> f64 {
        let frames = f64::from(self.data_len) / f64::from(WAV_FRAME_BYTES);
        frames / f64::from(self.rate)
    }

    /// 文件是不是**不完整**（写失败过 / 长度对不上）。
    fn incomplete(&self) -> bool {
        self.incomplete
    }
}

/// M0 的渲染循环。
///
/// **函数体与 Task 4 验收过的版本逐字相同**，只是从 `main` 里搬进来 ——
/// 不重排、不「顺便清理」，避免动到已经跑过的渲染路径。
fn render_triangle() -> anyhow::Result<()> {
    // 声明顺序有意义：`gpu` 借用 `window`，后声明者先析构 → gpu 先于 window 释放。
    // 因此 `window` 不需要（也不能）是 mut：轮询走内部可变性。
    let window = PlatformWindow::new("dslsmusic", 1280, 720)?;
    let mut gpu = init_gpu(&window)?;
    log::info!("RenderTier = {:?}", gpu.tier);

    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("triangle"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

    let bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[],
        });
    let layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("triangle"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: gpu.config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

    let mut metrics = metrics::FrameMetrics::new();

    // 推迟的 surface 重配请求。为什么要推迟见下面 Suboptimal 分支的说明。
    let mut needs_reconfigure = false;

    'running: loop {
        match window.poll_events() {
            PlatformEvent::CloseRequested => break 'running,
            PlatformEvent::Resized(w, h) if w > 0 && h > 0 => {
                gpu.config.width = w;
                gpu.config.height = h;
                gpu.surface.configure(&gpu.device, &gpu.config);
            }
            _ => {}
        }

        // ⚠️ 重配必须在**上一次 present 之后、这一次 acquire 之前**。
        // 上一帧已 present，此刻没有存活的 SurfaceTexture，configure 安全。
        if needs_reconfigure {
            needs_reconfigure = false;
            gpu.surface.configure(&gpu.device, &gpu.config);
        }

        metrics.begin_frame();

        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                continue;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                // surface 失效 → 重新配置后跳过本帧。Android 前后台切换走这条路径。
                gpu.surface.configure(&gpu.device, &gpu.config);
                continue;
            }
            // ⚠️ Suboptimal 携带一个**完全可用**的纹理，在 Linux/Vulkan 上
            // 直接来自 VK_SUBOPTIMAL_KHR。把它当致命错误会让「任何使
            // swapchain extent 变陈旧的窗口/缩放变化」直接杀掉进程。
            //
            // ⚠️⚠️ **但不能在这里 configure**：`Surface::configure` 在
            // 有存活的 `SurfaceTexture` 时会 **panic**（wgpu 30.0.1
            // `api/surface.rs:143-144` 明载；实现见 wgpu-core
            // `device/resource.rs` 的 `E::PreviousOutputExists`）。
            // 而 `f` 正是活着的那个纹理。
            //
            // 对照：上面的 `Outdated | Lost` 分支可以就地 configure，
            // 是因为**那条路径上没有纹理**。
            //
            // 正确做法：照常渲染并 present 这一帧，把重配**推迟到下一轮
            // 取下帧之前**（见循环顶部消费 needs_reconfigure 的地方）。
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                needs_reconfigure = true;
                f
            }
            other => anyhow::bail!("取 surface texture 失败: {other:?}"),
        };

        let view = frame.texture.create_view(&Default::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
        gpu.queue.present(frame);

        metrics.end_frame();
        if metrics.should_report() {
            log::info!("{}", metrics.summary());
        }
    }

    Ok(())
}

// ─────────────────────────────── 测试 ───────────────────────────────
//
// 这些测试**不需要声卡**（也就不受 VNC 影响）：它们只验头和数据。
// 为什么值得单独验：头里的两个长度字段写错**不会有任何运行期症状** ——
// 多数播放器容忍长度为 0 的头、照放不误。**「能打开」根本不能证明「头是对的」**，
// 唯一的办法是逐字节读回来对。
#[cfg(test)]
mod tests {
    use super::*;

    /// 落盘到一个临时文件并读回原始字节。
    fn dump_and_read(name: &str, rate: u32, samples: &[i16]) -> Vec<u8> {
        let path = std::env::temp_dir().join(name);
        let mut d = WavDump::create(&path, rate).expect("建 dump");
        d.write(samples);
        d.finish().expect("收尾");
        let bytes = std::fs::read(&path).expect("读回");
        let _ = std::fs::remove_file(&path);
        bytes
    }

    fn u32_at(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    fn u16_at(b: &[u8], off: usize) -> u16 {
        u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
    }

    /// 头是不是**恰好** 44 字节、字段对不对、两个长度字段有没有回填。
    #[test]
    fn header_is_44_bytes_and_lengths_are_backfilled() {
        // 100 帧 = 200 个 i16 = 400 字节 payload。
        let samples: Vec<i16> = (0..200).map(|i| i as i16).collect();
        let b = dump_and_read("dslsmusic-dump-test-header.wav", 44100, &samples);

        assert_eq!(b.len(), WAV_HEADER_LEN + 400, "文件长 = 44 + payload");
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(&b[12..16], b"fmt ", "块名是 4 字节的 fmt+空格");
        assert_eq!(&b[36..40], b"data", "data 偏移必须是 36（设计文档 §18.8）");

        assert_eq!(u32_at(&b, 16), 16, "fmt 块长度");
        assert_eq!(u16_at(&b, 20), 1, "1 = 整数 PCM");
        assert_eq!(u16_at(&b, 22), WAV_CHANNELS);
        assert_eq!(u32_at(&b, 24), 44100, "采样率 = 源率");
        assert_eq!(u32_at(&b, 28), 44100 * 4, "字节率 = 采样率 × 帧字节数");
        assert_eq!(u16_at(&b, 32), WAV_FRAME_BYTES, "block_align");
        assert_eq!(u16_at(&b, 34), WAV_BITS);

        // 两个长度字段：写文件时是 0，`finish` 必须回填成真值。
        assert_eq!(u32_at(&b, 40), 400, "data size");
        assert_eq!(
            u32_at(&b, 4) as usize,
            b.len() - 8,
            "RIFF size = 全长 − 8（含 'WAVE' 与两个块头）"
        );

        // payload 逐字节等于样本的小端表示 —— 头对不等于内容对。
        let payload: Vec<u8> = (0..200i16).flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(&b[WAV_HEADER_LEN..], &payload[..]);
    }

    /// 秒数按**写入头里的那个采样率**算。写成设备率的话时长会差 8.84%
    /// （44100 vs 48000），而文件照样能打开 —— 这是最容易被放过去的那种错。
    #[test]
    fn seconds_follow_the_header_rate() {
        let path = std::env::temp_dir().join("dslsmusic-dump-test-seconds.wav");
        let mut d = WavDump::create(&path, 44100).expect("建 dump");
        let one_second: Vec<i16> = vec![0; 44100 * 2];
        d.write(&one_second);
        d.finish().expect("收尾");

        assert!((d.seconds() - 1.0).abs() < 1e-9, "got {}", d.seconds());
        assert!(!d.incomplete(), "正常写完不该标成不完整");
        let _ = std::fs::remove_file(&path);
    }

    /// **头不能撒谎。** 模拟「写到一半文件被截断」：头必须按磁盘上**真实**的
    /// 字节数补，而不是按我们以为写了多少 —— 后者会产出「头说 400 字节、
    /// 实际 0 字节」的文件，而它照样能被多数播放器打开。
    #[test]
    fn header_follows_the_file_not_the_counter() {
        let path = std::env::temp_dir().join("dslsmusic-dump-test-truncated.wav");
        let mut d = WavDump::create(&path, 44100).expect("建 dump");
        let samples: Vec<i16> = vec![0; 200];
        d.write(&samples);
        // 外部把 payload 全砍掉，只留 44 字节的头。
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(WAV_HEADER_LEN as u64)
            .unwrap();
        d.finish().expect("收尾");

        let b = std::fs::read(&path).unwrap();
        assert_eq!(u32_at(&b, 40), 0, "data size 必须跟磁盘上的真实长度走");
        assert_eq!(u32_at(&b, 4), WAV_RIFF_OVERHEAD, "RIFF size = 36 + 0");
        assert!(d.incomplete(), "这种情况必须标成不完整");
        let _ = std::fs::remove_file(&path);
    }

    /// 建文件失败要**返回 Err**（`play` 把它变成一条可见的错误日志 + 继续播放），
    /// 不是 panic、更不是静默。
    #[test]
    fn create_failure_is_an_error_not_a_panic() {
        let path = Path::new("/nonexistent-dir-dslsmusic-dump/x.wav");
        assert!(WavDump::create(path, 44100).is_err());
    }
}
