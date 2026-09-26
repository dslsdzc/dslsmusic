use dsls_platform::{PlatformEvent, PlatformWindow, init_gpu};
use dslsmusic::metrics;

fn main() -> anyhow::Result<()> {
    env_logger::init();

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
