use crate::capability::{AdapterInfo, RenderTier, detect_tier, is_software_adapter};
use crate::window::PlatformWindow;

/// 生命周期参数 `'a` 借自窗口。**不要用 `transmute` 拉成 `'static`** ——
/// 官方示例用的就是这个生命周期参数写法，二者语义等价但这个是安全的。
pub struct GpuContext<'a> {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: wgpu::Surface<'a>,
    pub config: wgpu::SurfaceConfiguration,
    pub tier: RenderTier,
}

/// SDL 窗口不是 Send + Sync，但 raw-window-handle 要求是。
///
/// **这是官方示例的做法**（`sdl3-0.20.0/examples/raw-window-handle-with-wgpu/main.rs`），
/// 把 unsafe impl 关在一个小模块里，不让它泄漏出去。
mod rwh_bridge {
    use sdl3::video::Window;
    use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

    pub struct SyncWindow<'a>(pub &'a Window);

    unsafe impl Send for SyncWindow<'_> {}
    unsafe impl Sync for SyncWindow<'_> {}

    impl HasWindowHandle for SyncWindow<'_> {
        fn window_handle(&self) -> Result<wgpu::rwh::WindowHandle<'_>, wgpu::rwh::HandleError> {
            self.0.window_handle()
        }
    }
    impl HasDisplayHandle for SyncWindow<'_> {
        fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
            self.0.display_handle()
        }
    }
}

pub fn init_gpu<'a>(window: &'a PlatformWindow) -> anyhow::Result<GpuContext<'a>> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

    // 官方示例的写法：surface 的生命周期绑定在窗口借用上，无需任何 unsafe 转换。
    // 调用方必须保证 window 活得不比 GpuContext 短（在 main 里先声明 window）。
    let surface = instance.create_surface(rwh_bridge::SyncWindow(window.raw_window()))?;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: Some(&surface),
        apply_limit_buckets: false,
    }))?;

    let info = adapter.get_info();
    let adapter_info = AdapterInfo {
        name: info.name.clone(),
        backend: format!("{:?}", info.backend),
        is_software: is_software_adapter(&info.name)
            || matches!(info.device_type, wgpu::DeviceType::Cpu),
    };

    // ── 硬约束（设计文档 §16.0 坑 2）─────────────────────────
    if adapter_info.is_software {
        anyhow::bail!(
            "拒绝使用软件渲染适配器：{}（后端 {}）。\
             这会让性能下降两个数量级且不会有其他报错。",
            adapter_info.name,
            adapter_info.backend
        );
    }

    // 设计文档 §17.4：不只看格式可用性，还要看能否混合
    let f16 = wgpu::TextureFormat::Rgba16Float;
    let features = adapter.get_texture_format_features(f16);
    let supports_16f_blend = features
        .flags
        .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE);

    let tier = detect_tier(&adapter_info, supports_16f_blend);
    log::info!(
        "适配器: {} ({}) | Rgba16Float 可混合: {} | RenderTier: {:?}",
        adapter_info.name,
        adapter_info.backend,
        supports_16f_blend,
        tier
    );

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("dslsmusic device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        trace: wgpu::Trace::Off,
    }))?;

    let caps = surface.get_capabilities(&adapter);
    let format = *caps
        .formats
        .iter()
        .find(|f| f.is_srgb())
        .unwrap_or(&caps.formats[0]);
    let (width, height) = window.size();

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        desired_maximum_frame_latency: 2,
        view_formats: vec![],
    };
    surface.configure(&device, &config);

    Ok(GpuContext {
        device,
        queue,
        surface,
        config,
        tier,
    })
}
