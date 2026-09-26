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
use std::io::Write;
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
        Some(Cmd::Play { keywords, quality }) => play(&keywords, &quality).await,
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

/// `play`：搜索 → 取流 → 下载 → 解码 → 出声。
async fn play(keywords: &str, quality: &str) -> anyhow::Result<()> {
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
    println!("已下载 {} KB，开始播放…（Ctrl-C 停止）", bytes.len() / 1024);

    let mut engine = AudioEngine::new()?;
    let cursor = std::io::Cursor::new(bytes.to_vec());

    // 闭包借用 engine 与 pcm（`push_samples` 要 `&mut`），闭包结束借用即释放，
    // 所以后面的 `ring_available()` / `xrun_count()` 可以直接用。
    const CHUNK: usize = 4096;
    // v1 是空图（零开销）；v2 的 EQ / 响度 / 变速都挂在这里
    let mut pcm = dsls_audio::PcmGraph::new();
    let src_rate =
        dsls_audio::decode::decode_to_i16(Box::new(cursor), Some("mp3"), &mut |samples| {
            for chunk in samples.chunks_mut(CHUNK) {
                pcm.process(chunk); // ← 解码与 ring 之间：DSP 的位置
                engine.push_samples(chunk);
            }
            // 背压：`push_samples` 在环满时**丢数据**（只记一次 overflow，
            // 见 crates/audio/src/output.rs 的既定语义），而 `AudioEngine` 没有把
            // overflow 暴露出来 ⇒ 这里不压住就是**静默丢样本**。
            // 阈值 3 秒，环容量 4 秒（`AudioRing::new(44100 * 2 * 4)`）。
            while engine.ring_available() > 44100 * 2 * 3 {
                std::thread::sleep(Duration::from_millis(20));
            }
        })?;

    // ⚠️ **这个数必须与上面「音频输出」那行的 Hz 相同，否则一定是变速变调。**
    // `decode_to_i16` 按**源**采样率产出样本（重采样是它文档里明说的 TODO），
    // 而 cpal 那条流按**设备**采样率（本机 48000）消费 —— 中间没有任何转换。
    // 44.1kHz 的 mp3 在 48kHz 设备上会是 1.088 倍速、+1.46 半音，
    // **而 xrun 恒为 0、不报任何错**。打出来是为了让这件事可观测。
    log::info!("解码源采样率 = {src_rate}Hz（必须与上面「音频输出」的 Hz 一致）");

    // ⚠️ **xrun 必须在「解码结束」这一刻采一次，不能只看收尾的总数。**
    // 环被**自然读空**（歌放完了）时回调必然记一次欠载 —— 那是流的终点，
    // 不是故障。真正有意义的口径是「还有数据可推的时候，音频线程有没有被
    // 抽干」，也就是解码结束这一刻的计数：此时背压刚把环维持在 ~3 秒，
    // 这一刻的读数是干净的。
    let xrun_during_playback = engine.xrun_count();

    // 等缓冲播完（否则 main 一返回，AudioEngine 析构 = 立刻断声）。
    while engine.ring_available() > 0 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let xrun_total = engine.xrun_count();
    let xrun_tail = xrun_total.saturating_sub(xrun_during_playback);

    println!("\n播放结束。");
    println!(
        "xrun：播放期间 = {xrun_during_playback}（这条必须为 0）；\
         总计 = {xrun_total}（其中收尾抽干 {xrun_tail} 次 —— 歌放完、环读空，\
         回调必然记一次，不是故障。口径见 crates/audio/src/lib.rs 与设计文档 §16.2）"
    );
    // ⚠️ 本行以上的数字**只覆盖「我们自己的 ring 被抽干」**。
    // ALSA/设备侧的 xrun 不在这里，它只会在 cpal 的错误回调里以
    // 「音频流错误: A buffer underrun or overrun occurred.」的形式出现 ——
    // 设备卡顿而我方缓冲有货时，上面的 0 照样是 0（设计文档 §16.2 的口径）。
    Ok(())
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
