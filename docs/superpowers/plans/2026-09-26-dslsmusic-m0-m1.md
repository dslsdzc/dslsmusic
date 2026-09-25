# dslsmusic M0 + M1 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** M0 打通 SDL3 → wgpu 渲染链路并建立帧时间测量能力；M1 用纯 CLI 验证完整音乐链路（扫码登录 → 搜索 → 取流 → 出声），**不依赖任何 UI runtime**。

**Architecture:** Cargo workspace。`crates/platform` 封装全部 SDL3 调用并把能力探测暴露成可单测的纯逻辑；`crates/provider-api` 定义不含任何 UI 类型的音源 trait；`crates/provider-netease` 是其第一个实现；`crates/audio` 做解码与输出；`app` 是 CLI 组合根。

**Tech Stack:** Rust（edition 2024）· sdl3 0.20 · wgpu 30.0.1 · ncm-api-rs 0.1 · symphonia · cpal · tokio

## Global Constraints

以下为项目级硬约束，**每个任务的要求都隐含包含本节**：

- **Rust edition 2024**，全 workspace 统一。
- **`sdl3` 必须显式启用 `raw-window-handle` feature** —— 它的 `default = []`，不写就没有这个模块。
- **`parley` 必须钉死 `features = ["complex-scripts"]`** —— 默认关闭，不开则 CJK 词典式断行失效（设计文档 §7.2）。M0/M1 用不到，但写进 workspace 依赖表以便后续统一。
- **禁止 `use sdl3::...` 出现在 `crates/platform` 之外** —— 所有 SDL 调用包在 trait 后面（设计文档 §11.3）。
- **`provider-*` 不得依赖任何 `ui-*` crate** —— 边界由 crate 强制，不靠约定（设计文档 §2.3）。
- **运行时必须断言拿到硬件 adapter**，`llvmpipe` / `lavapipe` / `swiftshader` 一律拒绝（设计文档 §16.0 坑 2）。
- **登录只做只读路径**（扫码 / UID），**绝不调用签到、打卡、云盘上传类接口**（设计文档 §4.4）。
- **音频回调线程内禁止分配内存、加锁、IO**（设计文档 §16.2）。
- 平台：Linux（Wayland/niri）为主，**无 X11 退路**。
- **本项目是 GPLv3** —— 因此 GPL-3.0 / AGPL-3.0 的参考实现**可用**（此前被错误否决），但**非商业许可（如 PolyForm NC）与 GPLv3 不兼容，绝不可用**；无 license 的同样不可用（§14.1）。

---

## 文件结构

```
dslsmusic/
├── Cargo.toml                      # workspace root
├── rust-toolchain.toml             # 固定工具链
├── crates/
│   ├── platform/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs              # 公开 trait + 重导出
│   │       ├── capability.rs       # RenderTier 探测（纯逻辑，可单测）★
│   │       ├── window.rs           # SDL3 窗口封装
│   │       └── gpu.rs              # wgpu 实例/适配器/设备/surface
│   ├── provider-api/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── error.rs            # SourceError ★
│   │       ├── capability.rs       # CapabilitySet ★
│   │       └── model.rs            # Track / Artist / Album / Quality
│   ├── provider-netease/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs           # ncm-api-rs 封装 + 会话
│   │       ├── qr_login.rs         # 扫码登录状态机 ★
│   │       └── parse.rs            # ApiResponse → model ★
│   └── audio/
│       └── src/
│           ├── lib.rs
│           ├── decode.rs           # symphonia
│           └── output.rs           # cpal + ring buffer
└── app/
    ├── Cargo.toml
    └── src/
        ├── main.rs                 # CLI 入口
        ├── metrics.rs              # 帧时间直方图 ★
        └── shader.wgsl
```

★ = 含可单测的纯逻辑

---

# M0：链路验证 + 测量基建

## Task 1: Workspace 骨架

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `crates/platform/Cargo.toml`
- Create: `crates/platform/src/lib.rs`
- Create: `app/Cargo.toml`
- Create: `app/src/main.rs`

**Interfaces:**
- Consumes: 无
- Produces: workspace 成员 `platform`、`app`；后续所有 crate 追加到同一个 `[workspace].members`

- [ ] **Step 1: 写 workspace 根配置**

`Cargo.toml`：

```toml
[workspace]
resolver = "3"
members = ["crates/platform", "app"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "GPL-3.0-or-later"

[workspace.dependencies]
# ── 平台层 ──
sdl3 = { version = "0.20", features = ["raw-window-handle"] }
wgpu = "30.0.1"
pollster = "0.4"
raw-window-handle = "0.6"

# ── 基础设施 ──
anyhow = "1"
thiserror = "2"
log = "0.4"
env_logger = "0.11"
```

**注意 `sdl3` 的 feature 必须显式写 `raw-window-handle`** —— 它的 `default = []`。

- [ ] **Step 2: 固定工具链 + 放 GPLv3 许可证**

`rust-toolchain.toml`：

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

再创建仓库根的 `LICENSE`，**内容是 GPLv3 全文**：

```bash
cp /usr/share/common-licenses/GPL-3 LICENSE
```

**若该文件不存在，报告 BLOCKED**——GPLv3 是法律文本，**不要凭记忆写一份**。

- [ ] **Step 3: 建 platform crate 骨架**

`crates/platform/Cargo.toml`：

```toml
[package]
name = "dsls-platform"
version.workspace = true
edition.workspace = true

[dependencies]
sdl3.workspace = true
wgpu.workspace = true
# Task 3 的 gpu.rs 里用 pollster::block_on 驱动 request_adapter / request_device
pollster.workspace = true
raw-window-handle.workspace = true
thiserror.workspace = true
log.workspace = true
```

`crates/platform/src/lib.rs`：

```rust
//! SDL3 与 wgpu 的唯一接触面。
//!
//! 本 crate 之外的任何地方都不允许出现 `use sdl3::...`（设计文档 §11.3）。

pub mod capability;

pub use capability::RenderTier;
```

- [ ] **Step 4: 建 app 骨架**

`app/Cargo.toml`：

```toml
[package]
name = "dslsmusic"
version.workspace = true
edition.workspace = true

[dependencies]
dsls-platform = { path = "../crates/platform" }
# main.rs 直接用 wgpu 类型建管线；SDL 的一切都经 dsls-platform
wgpu.workspace = true
anyhow.workspace = true
log.workspace = true
env_logger.workspace = true
```

`app/src/main.rs`（占位，Task 3 替换）：

```rust
fn main() -> anyhow::Result<()> {
    env_logger::init();
    println!("dslsmusic workspace ok");
    Ok(())
}
```

- [ ] **Step 5: 验证构建**

```bash
cargo build --workspace
```

Expected: 编译通过。首次会下载并编译 SDL3（可能数分钟）。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml rust-toolchain.toml crates/platform app Cargo.lock
git commit -m "chore: workspace 骨架（platform + app）"
```

---

## Task 2: RenderTier 能力探测

**这块是纯逻辑，是本计划里唯一能真正做 TDD 的部分。**

**Files:**
- Create: `crates/platform/src/capability.rs`
- Test: `crates/platform/src/capability.rs`（内联 `#[cfg(test)]`）

**Interfaces:**
- Consumes: 无
- Produces:
  - `pub enum RenderTier { A, B, C }`
  - `pub struct AdapterInfo { pub name: String, pub backend: String, pub is_software: bool }`
  - `pub fn is_software_adapter(name: &str) -> bool`
  - `pub fn detect_tier(info: &AdapterInfo, supports_16f_blend: bool) -> RenderTier`

- [ ] **Step 1: 写失败的测试**

`crates/platform/src/capability.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llvmpipe_is_software() {
        assert!(is_software_adapter("llvmpipe (LLVM 22.1.8, 256 bits)"));
    }

    #[test]
    fn lavapipe_is_software() {
        assert!(is_software_adapter("lavapipe"));
    }

    #[test]
    fn swiftshader_is_software() {
        assert!(is_software_adapter("SwiftShader Device (Subzero)"));
    }

    #[test]
    fn real_gpu_is_not_software() {
        assert!(!is_software_adapter("NVIDIA GeForce GTX 1650"));
        assert!(!is_software_adapter("AMD Radeon Graphics (RADV RENOIR)"));
        assert!(!is_software_adapter("Apple M2"));
    }

    #[test]
    fn software_detection_is_case_insensitive() {
        assert!(is_software_adapter("LLVMpipe"));
    }

    #[test]
    fn tier_a_when_16f_blend_supported() {
        let info = AdapterInfo {
            name: "NVIDIA GeForce GTX 1650".into(),
            backend: "Vulkan".into(),
            is_software: false,
        };
        assert_eq!(detect_tier(&info, true), RenderTier::A);
    }

    #[test]
    fn tier_b_when_no_16f_blend() {
        let info = AdapterInfo {
            name: "Mali-G52".into(),
            backend: "Gl".into(),
            is_software: false,
        };
        assert_eq!(detect_tier(&info, false), RenderTier::B);
    }

    #[test]
    fn tier_c_when_software() {
        let info = AdapterInfo {
            name: "llvmpipe".into(),
            backend: "Vulkan".into(),
            is_software: true,
        };
        assert_eq!(detect_tier(&info, true), RenderTier::C);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test -p dsls-platform
```

Expected: 编译失败，`cannot find function is_software_adapter`。

- [ ] **Step 3: 写最小实现**

在 `capability.rs` 的 `mod tests` **之前**插入：

```rust
/// 渲染能力档。设计文档 §17.3。
///
/// **原则：能力不足时降级质量，而不是拒绝运行。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTier {
    /// 完整：Rgba16Float 线性中间格式、7 通道色散、完整折射
    A,
    /// 降级：Rgba8Unorm + dithering、3 通道色散
    B,
    /// 最低：无中间格式、关闭色散与折射
    C,
}

#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub name: String,
    pub backend: String,
    pub is_software: bool,
}

/// 软件光栅化适配器名单。
///
/// **这条防线很重要**：wgpu 选中软件光栅化时**性能慢两个数量级且不报错**
/// （本机实测 Vulkan 设备列表里就有 llvmpipe）。设计文档 §16.0 坑 2。
const SOFTWARE_MARKERS: &[&str] = &["llvmpipe", "lavapipe", "swiftshader", "softpipe"];

pub fn is_software_adapter(name: &str) -> bool {
    let lower = name.to_lowercase();
    SOFTWARE_MARKERS.iter().any(|m| lower.contains(m))
}

pub fn detect_tier(info: &AdapterInfo, supports_16f_blend: bool) -> RenderTier {
    if info.is_software {
        return RenderTier::C;
    }
    if supports_16f_blend {
        RenderTier::A
    } else {
        RenderTier::B
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cargo test -p dsls-platform
```

Expected: 8 passed。

- [ ] **Step 5: 提交**

```bash
git add crates/platform/src/capability.rs
git commit -m "feat(platform): RenderTier 能力探测与软件适配器识别"
```

---

## Task 3: SDL3 窗口 + wgpu surface + 三角形

**本任务的代码直接派生自 `sdl3` crate 自带的官方示例 `examples/raw-window-handle-with-wgpu/main.rs`（已核实）**。wgpu 30 的 API 与旧版差异很大，**不要凭记忆写**。

**Files:**
- Create: `crates/platform/src/window.rs`
- Create: `crates/platform/src/gpu.rs`
- Modify: `crates/platform/src/lib.rs`
- Create: `app/src/shader.wgsl`
- Modify: `app/src/main.rs`

**Interfaces:**
- Consumes: `RenderTier`、`is_software_adapter`（Task 2）
- Produces:
  - `pub struct PlatformWindow`（持 SDL3 window + event pump）
  - `pub enum PlatformEvent { Resized(u32, u32), CloseRequested, None }`
  - `pub struct GpuContext<'a> { pub device: wgpu::Device, pub queue: wgpu::Queue, pub surface: wgpu::Surface<'a>, pub config: wgpu::SurfaceConfiguration, pub tier: RenderTier }`
  - `pub fn init_gpu<'a>(window: &'a PlatformWindow) -> anyhow::Result<GpuContext<'a>>`

> **生命周期约束**：`surface` 借自窗口，所以**在 `main` 里必须先声明 `window` 再声明 `gpu`**（Rust 的 drop 顺序是后声明先析构，这样 `gpu` 先于 `window` 释放）。顺序写反编译不过——这是好事，编译器会挡住。

- [ ] **Step 1: 写 SDL3 窗口封装**

`crates/platform/src/window.rs`：

```rust
use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::Keycode;

/// 对上层暴露的事件。SDL 的类型不允许逸出本 crate（设计文档 §11.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformEvent {
    Resized(u32, u32),
    CloseRequested,
    /// 本帧没有需要处理的事件
    None,
}

pub struct PlatformWindow {
    window: sdl3::video::Window,
    event_pump: sdl3::EventPump,
}

impl PlatformWindow {
    pub fn new(title: &str, width: u32, height: u32) -> anyhow::Result<Self> {
        let sdl = sdl3::init()?;
        let video = sdl.video()?;
        let window = video
            .window(title, width, height)
            .position_centered()
            .resizable()
            .build()
            .map_err(|e| anyhow::anyhow!("SDL 窗口创建失败: {e}"))?;
        let event_pump = sdl.event_pump()?;
        Ok(Self { window, event_pump })
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.window.size();
        (w, h)
    }

    /// 供 `gpu.rs` 创建 surface 使用。**不要在别处调用。**
    pub(crate) fn raw_window(&self) -> &sdl3::video::Window {
        &self.window
    }

    /// 取走本帧的全部事件（只保留最后一个 resize / 第一个 quit）。
    pub fn poll_events(&mut self) -> PlatformEvent {
        let mut result = PlatformEvent::None;
        let self_id = self.window.id();
        for event in self.event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => return PlatformEvent::CloseRequested,
                Event::KeyDown { keycode: Some(Keycode::Escape), .. } => {
                    return PlatformEvent::CloseRequested;
                }
                Event::Window {
                    window_id,
                    win_event: WindowEvent::PixelSizeChanged(w, h)
                        | WindowEvent::Resized(w, h),
                    ..
                } if window_id == self_id => {
                    result = PlatformEvent::Resized(w as u32, h as u32);
                }
                _ => {}
            }
        }
        result
    }
}
```

> **注意**：`WindowEvent::PixelSizeChanged` 在分数缩放下才是真实物理像素。Wayland 上两者可能不同，**优先用 `PixelSizeChanged`**（match 里已放在前面）。

- [ ] **Step 2: 写 wgpu 初始化**

`crates/platform/src/gpu.rs`：

```rust
use crate::capability::{detect_tier, is_software_adapter, AdapterInfo, RenderTier};
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
        fn window_handle(
            &self,
        ) -> Result<wgpu::rwh::WindowHandle<'_>, wgpu::rwh::HandleError> {
            self.0.window_handle()
        }
    }
    impl HasDisplayHandle for SyncWindow<'_> {
        fn display_handle(
            &self,
        ) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
            self.0.display_handle()
        }
    }
}

pub fn init_gpu<'a>(window: &'a PlatformWindow) -> anyhow::Result<GpuContext<'a>> {
    let instance = wgpu::Instance::new(
        wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
    );

    // 官方示例的写法：surface 的生命周期绑定在窗口借用上，无需任何 unsafe 转换。
    // 调用方必须保证 window 活得不比 GpuContext 短（在 main 里先声明 window）。
    let surface = instance.create_surface(rwh_bridge::SyncWindow(window.raw_window()))?;

    let adapter = pollster::block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        },
    ))?;

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

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("dslsmusic device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        },
    ))?;

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

    Ok(GpuContext { device, queue, surface, config, tier })
}
```

- [ ] **Step 3: 更新 lib.rs**

```rust
//! SDL3 与 wgpu 的唯一接触面。
//!
//! 本 crate 之外的任何地方都不允许出现 `use sdl3::...`（设计文档 §11.3）。

pub mod capability;
pub mod gpu;
pub mod window;

pub use capability::{AdapterInfo, RenderTier, detect_tier, is_software_adapter};
pub use gpu::{GpuContext, init_gpu};
pub use window::{PlatformEvent, PlatformWindow};
```

- [ ] **Step 4: 写 shader**

`app/src/shader.wgsl`：

```wgsl
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>( 0.0,  0.6),
        vec2<f32>(-0.6, -0.4),
        vec2<f32>( 0.6, -0.4),
    );
    return vec4<f32>(pos[idx], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.35, 0.62, 0.98, 1.0);
}
```

- [ ] **Step 5: 写 app 主循环**

`app/src/main.rs`：

```rust
mod metrics;

use dsls_platform::{PlatformEvent, PlatformWindow, init_gpu};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut window = PlatformWindow::new("dslsmusic", 1280, 720)?;
    let mut gpu = init_gpu(&window)?;
    log::info!("RenderTier = {:?}", gpu.tier);

    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("triangle"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
    });

    let bgl = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[],
    });
    let layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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

        metrics.begin_frame();

        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded => continue,
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost => {
                // surface 失效 → 重新配置后跳过本帧。Android 前后台切换走这条路径。
                gpu.surface.configure(&gpu.device, &gpu.config);
                continue;
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
```

- [ ] **Step 6: 先放一个空的 metrics 让本任务能编译**

`app/src/metrics.rs`（Task 4 会替换成真实实现）：

```rust
pub struct FrameMetrics;

impl FrameMetrics {
    pub fn new() -> Self { Self }
    pub fn begin_frame(&mut self) {}
    pub fn end_frame(&mut self) {}
    pub fn should_report(&self) -> bool { false }
    pub fn summary(&self) -> String { String::new() }
}
```

- [ ] **Step 7: 构建并运行**

```bash
cargo run -p dslsmusic 2>&1 | head -20
```

Expected:
- 日志出现 `适配器: NVIDIA GeForce GTX 1650 (Vulkan) | Rgba16Float 可混合: true | RenderTier: A`
- 弹出一个 1280×720 窗口，深色背景上一个蓝色三角形
- 按 Esc 或关窗口退出

**如果日志里是 `llvmpipe`** —— 说明软件适配器防线被绕过了，**这是必须解决的阻塞问题，不要继续**。

- [ ] **Step 8: 手动验证 resize**

拖拽窗口边缘。Expected: 三角形跟随窗口尺寸正确缩放，无拉伸、无崩溃。

- [ ] **Step 9: 提交**

```bash
git add crates/platform app
git commit -m "feat(m0): SDL3 窗口 + wgpu surface + 三角形渲染

基于 sdl3 官方示例 examples/raw-window-handle-with-wgpu 移植。
含软件适配器拒绝、RenderTier 探测、surface lost/outdated 处理。"
```

---

## Task 4: 帧时间测量基建

设计文档 §16.5 要求 M0 就建立测量能力。**没有它，M3 的性能原型会变成猜谜。**

**Files:**
- Modify: `app/src/metrics.rs`
- Create: `app/tests/metrics.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `pub struct FrameMetrics`
  - `FrameMetrics::new()`, `begin_frame()`, `end_frame()`, `should_report()`, `summary()`
  - `pub fn percentile(sorted: &[f32], p: f32) -> f32`（纯函数，可单测）

- [ ] **Step 1: 写失败的测试**

`app/tests/metrics.rs`：

```rust
use dslsmusic::metrics::percentile;

#[test]
fn percentile_of_single_value() {
    assert_eq!(percentile(&[5.0], 0.5), 5.0);
}

#[test]
fn p50_of_known_series() {
    let v: Vec<f32> = (1..=100).map(|x| x as f32).collect();
    let p50 = percentile(&v, 0.50);
    assert!((p50 - 50.0).abs() < 1.0, "got {p50}");
}

#[test]
fn p99_catches_the_tail() {
    // 99 帧 8ms + 1 帧 100ms —— 平均值看起来还行，p99 暴露问题
    let mut v: Vec<f32> = vec![8.0; 99];
    v.push(100.0);
    let avg: f32 = v.iter().sum::<f32>() / v.len() as f32;
    assert!(avg < 10.0, "平均值骗人: {avg}");
    let p99 = percentile(&v, 0.99);
    assert!(p99 > 50.0, "p99 应暴露长尾: {p99}");
}

#[test]
fn percentile_clamps_out_of_range() {
    let v = vec![1.0, 2.0, 3.0];
    assert_eq!(percentile(&v, 0.0), 1.0);
    assert_eq!(percentile(&v, 1.0), 3.0);
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test -p dslsmusic --test metrics
```

Expected: 编译失败，`unresolved import dslsmusic::metrics`。

- [ ] **Step 3: 让 app 变成 lib + bin**

`app/Cargo.toml` 追加：

```toml
[lib]
name = "dslsmusic"
path = "src/lib.rs"

[[bin]]
name = "dslsmusic"
path = "src/main.rs"
```

`app/src/lib.rs`：

```rust
pub mod metrics;
```

`app/src/main.rs` 顶部改为 `use dslsmusic::metrics;`，并删掉 `mod metrics;`。

- [ ] **Step 4: 写实现**

`app/src/metrics.rs`：

```rust
use std::time::{Duration, Instant};

/// 环形缓冲容量：约 10 秒 @60fps。
const CAPACITY: usize = 600;
/// 每这么多帧汇报一次。
const REPORT_EVERY: u64 = 300;

pub struct FrameMetrics {
    last: Option<Instant>,
    /// 毫秒，环形缓冲
    samples: Vec<f32>,
    write_at: usize,
    frame_count: u64,
}

impl Default for FrameMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameMetrics {
    pub fn new() -> Self {
        Self {
            last: None,
            samples: Vec::with_capacity(CAPACITY),
            write_at: 0,
            frame_count: 0,
        }
    }

    pub fn begin_frame(&mut self) {
        self.last = Some(Instant::now());
    }

    pub fn end_frame(&mut self) {
        let Some(start) = self.last else { return };
        let ms = start.elapsed().as_secs_f32() * 1000.0;
        if self.samples.len() < CAPACITY {
            self.samples.push(ms);
        } else {
            self.samples[self.write_at] = ms;
            self.write_at = (self.write_at + 1) % CAPACITY;
        }
        self.frame_count += 1;
    }

    pub fn should_report(&self) -> bool {
        self.frame_count > 0 && self.frame_count % REPORT_EVERY == 0
    }

    pub fn summary(&self) -> String {
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let avg: f32 = sorted.iter().sum::<f32>() / sorted.len().max(1) as f32;
        format!(
            "帧时间 ms  avg {:>6.2} | p50 {:>6.2} | p95 {:>6.2} | p99 {:>6.2} | max {:>6.2}  (n={})",
            avg,
            percentile(&sorted, 0.50),
            percentile(&sorted, 0.95),
            percentile(&sorted, 0.99),
            sorted.last().copied().unwrap_or(0.0),
            sorted.len(),
        )
    }
}

/// 输入必须已升序排序。
pub fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let p = p.clamp(0.0, 1.0);
    let idx = ((sorted.len() - 1) as f32 * p).round() as usize;
    sorted[idx]
}

/// 供后续 GPU timestamp 查询使用；GLES 后端不支持时回退到 CPU 计时（设计文档 §16.5）。
pub fn frame_budget_ms(target_fps: u32) -> f32 {
    1000.0 / target_fps as f32
}

#[allow(dead_code)]
fn _assert_duration_used(_: Duration) {}
```

- [ ] **Step 5: 运行测试确认通过**

```bash
cargo test -p dslsmusic --test metrics
```

Expected: 4 passed。

- [ ] **Step 6: 运行并观察真实数据**

```bash
cargo run -p dslsmusic 2>&1 | grep "帧时间" | head -5
```

Expected: 每 300 帧输出一行，`p99` 应接近 `avg`（空载场景不该有长尾）。**若 p99 明显高于 avg，说明有卡顿源，这是 M3 要重点查的对象。**

- [ ] **Step 7: 提交**

```bash
git add app
git commit -m "feat(m0): 帧时间测量基建（avg/p50/p95/p99/max）"
```

---

# M1：音乐链路（纯 CLI，无 UI runtime）

## Task 5: provider-api 错误模型与能力集

**设计文档 §4.5**：不能把 `Unsupported` / `NotFound` / `Unavailable` 压成同一个空结果。

**Files:**
- Create: `crates/provider-api/Cargo.toml`
- Create: `crates/provider-api/src/lib.rs`
- Create: `crates/provider-api/src/error.rs`
- Create: `crates/provider-api/src/capability.rs`
- Create: `crates/provider-api/src/model.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `pub enum SourceError { Unsupported, NotFound, AuthRequired, Unavailable, RateLimited, Network(String), Protocol(String) }`
  - `pub type SourceResult<T> = Result<Option<T>, SourceError>`
  - `pub struct CapabilitySet`（bitflags 或 HashSet）
  - `pub enum Quality { Low, Standard, High, Super }`
  - `pub struct Track { pub id: String, pub title: String, pub artist: String, pub album: String, pub duration_ms: u64 }`

- [ ] **Step 1: 写失败的测试**

`crates/provider-api/src/error.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_distinguishable_from_empty() {
        let a: SourceResult<Track> = Ok(None);
        let b: SourceResult<Track> = Err(SourceError::Unsupported);
        assert!(a.is_ok() && a.unwrap().is_none());
        assert!(matches!(b, Err(SourceError::Unsupported)));
    }

    #[test]
    fn resolver_can_decide_whether_to_try_another_provider() {
        // 只有 Unsupported 说明「换一个音源有意义」
        assert!(SourceError::Unsupported.should_try_next_provider());
        // NotFound 说明换音源也没用（歌就是不存在）
        assert!(!SourceError::NotFound.should_try_next_provider());
        // Unavailable 说明有版权但拿不到，可能降音质能解决
        assert!(!SourceError::Unavailable.should_try_next_provider());
        assert!(SourceError::Unavailable.should_retry_lower_quality());
    }

    #[test]
    fn protocol_error_signals_upstream_change() {
        let e = SourceError::Protocol("yrc 字段消失".into());
        assert!(e.is_upstream_changed());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test -p dsls-provider-api
```

Expected: 编译失败（crate 尚不存在）。

- [ ] **Step 3: 建 crate 并写实现**

`crates/provider-api/Cargo.toml`：

```toml
[package]
name = "dsls-provider-api"
version.workspace = true
edition.workspace = true

[dependencies]
thiserror.workspace = true
```

`crates/provider-api/src/model.rs`：

```rust
/// 音质档位。各平台原始标识在 provider 内部映射（设计文档 §4.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    Low,
    Standard,
    High,
    Super,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 毫秒
    pub duration_ms: u64,
}
```

`crates/provider-api/src/error.rs`：

```rust
use crate::model::Track;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// provider 根本没这个能力
    #[error("该音源不支持此操作")]
    Unsupported,
    /// 有能力，但这个对象不存在
    #[error("资源不存在")]
    NotFound,
    /// 需要登录态
    #[error("需要登录")]
    AuthRequired,
    /// 有能力，但当前拿不到（版权 / 地区）
    #[error("当前不可用")]
    Unavailable,
    #[error("触发限流")]
    RateLimited,
    #[error("网络错误: {0}")]
    Network(String),
    /// 上游接口变了 —— 对网易云尤其重要（设计文档 §13.1）
    #[error("上游协议变化: {0}")]
    Protocol(String),
}

impl SourceError {
    /// 换一个音源试有没有意义？
    pub fn should_try_next_provider(&self) -> bool {
        matches!(self, SourceError::Unsupported)
    }

    /// 降音质重试有没有意义？
    pub fn should_retry_lower_quality(&self) -> bool {
        matches!(self, SourceError::Unavailable | SourceError::RateLimited)
    }

    /// 是不是上游接口变了（需要人工介入）
    pub fn is_upstream_changed(&self) -> bool {
        matches!(self, SourceError::Protocol(_))
    }
}

/// **关键契约**：`Ok(None)` 表示「请求合法，但确实没有」，
/// 与 `Err(Unsupported)`「provider 没这个能力」是两回事。
pub type SourceResult<T> = Result<Option<T>, SourceError>;

const _: () = {
    // 编译期确认 Track 被用在类型别名里
    #[allow(dead_code)]
    fn _t(_: SourceResult<Track>) {}
};
```

`crates/provider-api/src/capability.rs`：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Search,
    TrackDetail,
    StreamUrl,
    Cover,
    Lyric,
    Playlist,
    Album,
    Artist,
}

#[derive(Debug, Clone, Default)]
pub struct CapabilitySet(std::collections::HashSet<Capability>);

impl CapabilitySet {
    pub fn new() -> Self { Self::default() }

    pub fn with(mut self, c: Capability) -> Self {
        self.0.insert(c);
        self
    }

    pub fn supports(&self, c: Capability) -> bool {
        self.0.contains(&c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_supports_nothing() {
        let s = CapabilitySet::new();
        assert!(!s.supports(Capability::Search));
    }

    #[test]
    fn builder_accumulates() {
        let s = CapabilitySet::new()
            .with(Capability::Search)
            .with(Capability::Lyric);
        assert!(s.supports(Capability::Search));
        assert!(s.supports(Capability::Lyric));
        assert!(!s.supports(Capability::StreamUrl));
    }
}
```

`crates/provider-api/src/lib.rs`：

```rust
//! 音源契约。**本 crate 不得依赖任何 UI crate**（设计文档 §2.3）。

pub mod capability;
pub mod error;
pub mod model;

pub use capability::{Capability, CapabilitySet};
pub use error::{SourceError, SourceResult};
pub use model::{Quality, Track};

use async_trait::async_trait;

#[async_trait]
pub trait MusicSource: Send + Sync {
    fn name(&self) -> &str;

    fn capabilities(&self) -> &CapabilitySet;

    async fn search(
        &self,
        keywords: &str,
        limit: u32,
    ) -> SourceResult<Vec<Track>>;

    async fn stream_url(
        &self,
        track_id: &str,
        quality: Quality,
    ) -> SourceResult<String>;
}
```

`crates/provider-api/Cargo.toml` 追加：

```toml
async-trait = "0.1"
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cargo test -p dsls-provider-api
```

Expected: 5 passed（3 个 error + 2 个 capability）。

- [ ] **Step 5: 提交**

```bash
git add crates/provider-api
git commit -m "feat(provider-api): MusicSource trait + typed SourceError + CapabilitySet"
```

---

## Task 6: 网易云 provider —— 扫码登录

**Files:**
- Create: `crates/provider-netease/Cargo.toml`
- Create: `crates/provider-netease/src/lib.rs`
- Create: `crates/provider-netease/src/qr_login.rs`
- Create: `crates/provider-netease/src/client.rs`

**Interfaces:**
- Consumes: `dsls-provider-api`
- Produces:
  - `pub enum QrState { WaitingScan, WaitingConfirm, Confirmed { cookie: String }, Expired }`
  - `pub async fn begin_qr_login(client: &ApiClient) -> anyhow::Result<(String, Vec<u8>)>`（返回 key + PNG 字节）
  - `pub async fn poll_qr_login(client: &ApiClient, key: &str) -> anyhow::Result<QrState>`
  - `pub fn extract_music_u(cookie: &str) -> Option<String>` ★ 纯函数，可单测

- [ ] **Step 1: 写失败的测试**

`crates/provider-netease/src/qr_login.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_music_u_from_cookie_string() {
        let raw = "MUSIC_U=abc123def; __csrf=xyz; NMTID=1";
        assert_eq!(extract_music_u(raw), Some("abc123def".to_string()));
    }

    #[test]
    fn returns_none_when_music_u_absent() {
        assert_eq!(extract_music_u("__csrf=xyz"), None);
    }

    #[test]
    fn handles_music_u_at_end_without_semicolon() {
        assert_eq!(extract_music_u("__csrf=x; MUSIC_U=tail"), Some("tail".to_string()));
    }

    #[test]
    fn maps_code_801_to_waiting_scan() {
        assert_eq!(QrState::from_code(801, ""), QrState::WaitingScan);
    }

    #[test]
    fn maps_code_802_to_waiting_confirm() {
        assert_eq!(QrState::from_code(802, ""), QrState::WaitingConfirm);
    }

    #[test]
    fn maps_code_800_to_expired() {
        assert_eq!(QrState::from_code(800, ""), QrState::Expired);
    }

    #[test]
    fn maps_code_803_to_confirmed_with_cookie() {
        let s = QrState::from_code(803, "MUSIC_U=tok; __csrf=y");
        assert_eq!(s, QrState::Confirmed { cookie: "tok".to_string() });
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test -p dsls-provider-netease
```

Expected: 编译失败（crate 尚不存在）。

- [ ] **Step 3: 建 crate 并写实现**

`crates/provider-netease/Cargo.toml`：

```toml
[package]
name = "dsls-provider-netease"
version.workspace = true
edition.workspace = true

[dependencies]
dsls-provider-api = { path = "../provider-api" }
ncm-api-rs = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
async-trait = "0.1"
anyhow.workspace = true
log.workspace = true
serde_json = "1"
```

`crates/provider-netease/src/qr_login.rs`：

```rust
/// 网易云扫码登录返回码。
/// 801 = 等待扫码，802 = 待确认，803 = 成功（带 cookie），800 = 二维码过期。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrState {
    WaitingScan,
    WaitingConfirm,
    Confirmed { cookie: String },
    Expired,
}

impl QrState {
    pub fn from_code(code: i64, raw_cookie: &str) -> Self {
        match code {
            801 => QrState::WaitingScan,
            802 => QrState::WaitingConfirm,
            803 => QrState::Confirmed {
                cookie: extract_music_u(raw_cookie).unwrap_or_default(),
            },
            _ => QrState::Expired,
        }
    }
}

/// 从 `key=value; key=value` 形式的 cookie 串里取出 MUSIC_U。
/// 登录态只需要这一个 key（设计文档 §4.2）。
pub fn extract_music_u(raw: &str) -> Option<String> {
    raw.split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(k, _)| *k == "MUSIC_U")
        .map(|(_, v)| v.to_string())
}
```

`crates/provider-netease/src/client.rs`：

```rust
use ncm_api_rs::{ApiClient, Query, create_client};
use dsls_provider_api::SourceError;

pub struct NeteaseClient {
    inner: ApiClient,
    cookie: Option<String>,
}

impl NeteaseClient {
    pub fn new(cookie: Option<String>) -> Self {
        let inner = create_client(cookie.clone());
        Self { inner, cookie }
    }

    pub fn with_cookie(cookie: String) -> Self {
        Self::new(Some(cookie))
    }

    pub fn inner(&self) -> &ApiClient { &self.inner }
    pub fn cookie(&self) -> Option<&str> { self.cookie.as_deref() }

    /// 网易云二维码登录第一步：拿 key。
    pub async fn qr_key(&self) -> Result<String, SourceError> {
        let resp = self
            .inner
            .login_qr_key(&Query::new())
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;
        resp.body
            .pointer("/data/unikey")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| SourceError::Protocol("login_qr_key 未返回 unikey".into()))
    }

    /// 第二步：用 key 生成二维码图片（PNG 字节）。
    pub async fn qr_image(&self, key: &str) -> Result<Vec<u8>, SourceError> {
        let resp = self
            .inner
            .login_qr_create(&Query::new().param("key", key).param("qrimg", "true"))
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;

        let data_url = resp
            .body
            .pointer("/data/qrimg")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SourceError::Protocol("login_qr_create 未返回 qrimg".into()))?;

        // 返回形如 "data:image/png;base64,...."
        let b64 = data_url
            .split_once("base64,")
            .map(|(_, b)| b)
            .ok_or_else(|| SourceError::Protocol("qrimg 不是 base64 data URL".into()))?;
        decode_base64(b64)
    }

    /// 第三步：轮询扫码状态。
    pub async fn qr_check(&self, key: &str) -> Result<crate::qr_login::QrState, SourceError> {
        let resp = self
            .inner
            .login_qr_check(&Query::new().param("key", key))
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;

        let code = resp
            .body
            .pointer("/code")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| SourceError::Protocol("login_qr_check 未返回 code".into()))?;

        // ⚠️ cookie 不在 body 里 —— 它在 ApiResponse.cookie（由 Set-Cookie 头填充）。
        // 已核实：ncm-api-rs 的 ApiResponse 是
        //   { status: i64, body: Value, cookie: Vec<String> }
        let cookie = resp.cookie.join("; ");

        Ok(crate::qr_login::QrState::from_code(code, &cookie))
    }
}

fn decode_base64(s: &str) -> Result<Vec<u8>, SourceError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| SourceError::Protocol(format!("base64 解码失败: {e}")))
}
```

`crates/provider-netease/Cargo.toml` 追加：

```toml
base64 = "0.22"
```

`crates/provider-netease/src/lib.rs`：

```rust
//! 网易云音源实现。**本 crate 不得依赖任何 UI crate。**

pub mod client;
pub mod qr_login;

pub use client::NeteaseClient;
pub use qr_login::{QrState, extract_music_u};
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cargo test -p dsls-provider-netease
```

Expected: 7 passed。

- [ ] **Step 5: 提交**

```bash
git add crates/provider-netease
git commit -m "feat(provider-netease): 扫码登录状态机 + cookie 提取"
```

---

## Task 7: 网易云 provider —— 搜索与取流

**Files:**
- Modify: `crates/provider-netease/src/client.rs`
- Create: `crates/provider-netease/src/parse.rs`
- Modify: `crates/provider-netease/src/lib.rs`

**Interfaces:**
- Consumes: Task 6 的 `NeteaseClient`
- Produces:
  - `pub fn parse_search_tracks(body: &serde_json::Value) -> Result<Vec<Track>, SourceError>` ★
  - `pub fn parse_stream_url(body: &serde_json::Value, quality: Quality) -> Result<String, SourceError>` ★
  - `NeteaseClient::search(&self, kw, limit)`、`NeteaseClient::stream_url(&self, id, quality)`

- [ ] **Step 1: 写失败的测试**

`crates/provider-netease/src/parse.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use dsls_provider_api::Quality;
    use serde_json::json;

    #[test]
    fn parses_cloudsearch_result() {
        let body = json!({
            "result": {
                "songs": [{
                    "id": 123456,
                    "name": "测试歌曲",
                    "dt": 245000,
                    "ar": [{"name": "歌手A"}, {"name": "歌手B"}],
                    "al": {"name": "专辑X"}
                }]
            }
        });
        let tracks = parse_search_tracks(&body).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "123456");
        assert_eq!(tracks[0].title, "测试歌曲");
        assert_eq!(tracks[0].artist, "歌手A / 歌手B");
        assert_eq!(tracks[0].album, "专辑X");
        assert_eq!(tracks[0].duration_ms, 245000);
    }

    #[test]
    fn empty_result_is_ok_not_error() {
        // Ok(None) / 空列表 与 Err(Unsupported) 必须区分（设计文档 §4.5）
        let body = json!({"result": {"songs": []}});
        assert_eq!(parse_search_tracks(&body).unwrap().len(), 0);
    }

    #[test]
    fn missing_result_field_is_protocol_error() {
        let body = json!({"code": 200});
        let err = parse_search_tracks(&body).unwrap_err();
        assert!(err.is_upstream_changed(), "应为 Protocol 错误: {err:?}");
    }

    #[test]
    fn parses_stream_url() {
        let body = json!({
            "data": [{"url": "https://m8.music.126.net/x.mp3", "br": 320000, "code": 200}]
        });
        let url = parse_stream_url(&body, Quality::High).unwrap();
        assert_eq!(url, "https://m8.music.126.net/x.mp3");
    }

    #[test]
    fn null_url_means_unavailable_not_protocol_error() {
        // 版权下架：有能力但拿不到 → Unavailable，不是 Protocol
        let body = json!({"data": [{"url": null, "code": 200}]});
        let err = parse_stream_url(&body, Quality::High).unwrap_err();
        assert!(matches!(err, dsls_provider_api::SourceError::Unavailable));
        assert!(err.should_retry_lower_quality());
    }

    #[test]
    fn empty_data_array_is_unavailable() {
        let body = json!({"data": []});
        let err = parse_stream_url(&body, Quality::High).unwrap_err();
        assert!(matches!(err, dsls_provider_api::SourceError::Unavailable));
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test -p dsls-provider-netease parse
```

Expected: 编译失败，`cannot find function parse_search_tracks`。

- [ ] **Step 3: 写实现**

`crates/provider-netease/src/parse.rs` 的 `mod tests` **之前**插入：

```rust
use dsls_provider_api::{Quality, SourceError, Track};
use serde_json::Value;

pub fn parse_search_tracks(body: &Value) -> Result<Vec<Track>, SourceError> {
    let songs = body
        .pointer("/result/songs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            SourceError::Protocol("cloudsearch 响应缺少 result.songs".into())
        })?;

    Ok(songs
        .iter()
        .filter_map(|s| {
            let id = s.get("id")?.as_i64()?;
            let title = s.get("name")?.as_str()?.to_string();
            let artist = s
                .get("ar")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            let album = s
                .pointer("/al/name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let duration_ms = s.get("dt").and_then(|v| v.as_u64()).unwrap_or(0);
            Some(Track { id: id.to_string(), title, artist, album, duration_ms })
        })
        .collect())
}

/// 音质档位 → 网易云 br 参数。
fn quality_to_br(q: Quality) -> u32 {
    match q {
        Quality::Low => 128_000,
        Quality::Standard => 192_000,
        Quality::High => 320_000,
        Quality::Super => 999_000, // 无损
    }
}

pub fn parse_stream_url(body: &Value, _q: Quality) -> Result<String, SourceError> {
    let data = body
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| SourceError::Protocol("song/url 响应缺少 data".into()))?;

    let first = data.first().ok_or(SourceError::Unavailable)?;
    match first.get("url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => Ok(u.to_string()),
        // url 为 null / 空：有能力但当前拿不到（版权、地区、需要 VIP）
        _ => Err(SourceError::Unavailable),
    }
}

pub use quality_to_br as br_for_quality;
```

- [ ] **Step 4: 在 client.rs 追加方法**

```rust
impl NeteaseClient {
    pub async fn search(
        &self,
        keywords: &str,
        limit: u32,
    ) -> Result<Vec<dsls_provider_api::Track>, SourceError> {
        let resp = self
            .inner
            .cloudsearch(
                &Query::new()
                    .param("keywords", keywords)
                    .param("limit", &limit.to_string())
                    .param("type", "1"),
            )
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;
        crate::parse::parse_search_tracks(&resp.body)
    }

    pub async fn stream_url(
        &self,
        track_id: &str,
        quality: dsls_provider_api::Quality,
    ) -> Result<String, SourceError> {
        // 已核实方法名存在：ncm-api-rs src/api/song_url_v1.rs
        //   pub async fn song_url_v1(&self, query: &Query) -> Result<ApiResponse>
        let resp = self
            .inner
            .song_url_v1(&Query::new().param("id", track_id).param("level", &level_for(quality)))
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;
        crate::parse::parse_stream_url(&resp.body, quality)
    }
}

fn level_for(q: dsls_provider_api::Quality) -> String {
    match q {
        dsls_provider_api::Quality::Low => "standard",
        dsls_provider_api::Quality::Standard => "higher",
        dsls_provider_api::Quality::High => "exhigh",
        dsls_provider_api::Quality::Super => "lossless",
    }
    .to_string()
}
```

> **已核实**：`song_url` / `song_url_v1` / `song_url_v1_302` / `song_url_ncmget` / `song_url_match` 五个方法在 `ncm-api-rs` 0.1.0 中都存在。用 `song_url_v1`（对应 Node 版 `/song/url/v1`，支持 `level` 参数）。

- [ ] **Step 5: 运行测试确认通过**

```bash
cargo test -p dsls-provider-netease
```

Expected: 13 passed。

- [ ] **Step 6: 提交**

```bash
git add crates/provider-netease
git commit -m "feat(provider-netease): 搜索与取流，含 response 解析与错误分类"
```

---

## Task 8: 音频管线 —— 解码与输出

**设计文档 §16.2：音频回调线程内禁止分配、加锁、IO。**

**Files:**
- Create: `crates/audio/Cargo.toml`
- Create: `crates/audio/src/lib.rs`
- Create: `crates/audio/src/decode.rs`
- Create: `crates/audio/src/output.rs`
- Create: `crates/audio/src/pcm.rs` — **v2 DSP 的挂载点**（设计文档 §6.1）

> **关于 v1 留的两个钩子（设计文档 §6.1）——代价不对称，如实处理：**
>
> | 钩子 | v1 怎么处理 |
> |---|---|
> | **PCM graph 挂载点** | **实做。** 它便宜（一个 passthrough 调用点），而且**事后补要改解码循环** |
> | **当前曲 + 下一曲两槽队列** | **只记为扩展点，不实现。** 真正贵的地方不在队列本身，而在**音频回调**——v2 做交叉淡入时回调要从两个 ring 同时取数并混合。那个改动 v1 没法「预留」，硬留就是写没人用的代码 |
>
> **v2 接入 `dsls-audio` 时必须知道：** `AudioEngine` 的 `producer` 字段要变成当前/预备两槽，且 `build_output_stream` 的闭包要从两路取数并按交叉淡入曲线混合。这不是加字段能解决的，是回调逻辑的改写。

**Interfaces:**
- Consumes: 无
- Produces:
  - `pub struct AudioEngine`（`!Sync`）、`AudioEngine::new() -> Result<Self>`、`push_samples(&mut self, &[i16])`、`ring_available(&self) -> usize`、`xrun_count(&self) -> u64`
  - `pub fn xrun_count(&self) -> u64`（硬指标，必须为 0）

- [ ] **Step 1: 写失败的测试**

`crates/audio/src/output.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_order() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3]);
        let mut out = [0i16; 3];
        assert_eq!(cons.pop_samples(&mut out), 3);
        assert_eq!(out, [1, 2, 3]);
    }

    #[test]
    fn counts_underrun_when_empty() {
        let ring = AudioRing::new(8);
        let (_prod, mut cons) = ring.split();
        let mut out = [0i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 0);
        assert_eq!(cons.underruns(), 1);
        // 回调必须填满整块缓冲 —— 没数据时填静音，绝不能留垃圾值（那是爆音）
        assert_eq!(out, [0i16; 4]);
    }

    #[test]
    fn counts_overflow_and_keeps_oldest() {
        // ringbuf 的语义：环满之后**新数据被丢弃**（不是挤掉最旧的）
        let ring = AudioRing::new(4);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(prod.overflows(), 1, "6 个样本推进容量 4 的环，应记一次溢出");
        let mut out = [0i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 4);
        assert_eq!(out, [1, 2, 3, 4], "ringbuf 保留先到的数据");
    }

    #[test]
    fn partial_underrun_still_fills_silence() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[7, 8]);
        let mut out = [99i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 2);
        assert_eq!(out, [7, 8, 0, 0], "尾部必须补静音，不能保留 out 原值");
        assert_eq!(cons.underruns(), 1, "取不满也算一次 underrun");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cargo test -p dsls-audio
```

Expected: 编译失败（crate 尚不存在）。

- [ ] **Step 3: 建 crate 并写 AudioRing（无锁）**

`crates/audio/Cargo.toml`：

```toml
[package]
name = "dsls-audio"
version.workspace = true
edition.workspace = true

[dependencies]
# 已核实：cpal 最新稳定版是 0.18.2
cpal = "0.18"
# ⚠️ 刻意钉在 0.5 —— 0.6 已发布（0.6.1）但 API 与本计划中的代码未经核对。
# 升级是 dsls-audio 内部的封闭改动，v1 不冒这个险。
symphonia = { version = "0.5", features = ["mp3", "aac", "flac", "isomp4", "ogg", "vorbis", "wav"] }
# 已核实：reqwest 最新稳定版是 0.13.5
reqwest = { version = "0.13", features = ["stream", "rustls-tls"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync"] }
# 无锁 SPSC 环形缓冲。**音频回调线程里绝不允许加锁**（设计文档 §16.2 / 本计划 Global Constraints）
ringbuf = "0.4"
anyhow.workspace = true
log.workspace = true
```

`crates/audio/src/output.rs` 的 `mod tests` **之前**插入：

```rust
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// 无锁 SPSC 环形缓冲，用 `AudioRing::split()` 拆成生产端与消费端。
///
/// **消费端在音频回调线程里使用：无锁、无分配、无 IO**（设计文档 §16.2）。
/// 原计划用 `Mutex<Vec<i16>>`，那会在实时线程里加锁 —— 已废弃。
pub struct AudioRing {
    capacity: usize,
}

impl AudioRing {
    pub fn new(capacity: usize) -> Self {
        Self { capacity }
    }

    pub fn split(self) -> (AudioRingProducer, AudioRingConsumer) {
        let (prod, cons) = HeapRb::<i16>::new(self.capacity).split();
        let overflows = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(AtomicU64::new(0));
        (
            AudioRingProducer {
                inner: prod,
                overflows: Arc::clone(&overflows),
                underruns: Arc::clone(&underruns),
            },
            AudioRingConsumer { inner: cons, underruns },
        )
    }
}

/// 生产端 —— 解码线程使用。`!Sync` 但 `Send`。
pub struct AudioRingProducer {
    inner: HeapProd<i16>,
    overflows: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
}

impl AudioRingProducer {
    /// 推入样本。**环满时新数据被丢弃并计一次溢出**，绝不阻塞、绝不分配。
    pub fn push_samples(&mut self, data: &[i16]) {
        let pushed = self.inner.push_slice(data);
        if pushed < data.len() {
            self.overflows.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 当前环内可读样本数（供背压判断）。
    pub fn available(&self) -> usize {
        self.inner.occupied_len()
    }

    pub fn overflows(&self) -> u64 {
        self.overflows.load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

/// 消费端 —— **音频回调线程使用**。`!Sync` 但 `Send`。
pub struct AudioRingConsumer {
    inner: HeapCons<i16>,
    underruns: Arc<AtomicU64>,
}

impl AudioRingConsumer {
    /// 取样本填满 `out`。**取不满时尾部补静音并计一次 underrun**。
    /// 返回实际取到的样本数。永不阻塞、永不分配。
    pub fn pop_samples(&mut self, out: &mut [i16]) -> usize {
        let n = self.inner.pop_slice(out);
        if n < out.len() {
            for slot in out.iter_mut().skip(n) {
                *slot = 0;
            }
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
        n
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}
```

> **用到的 `ringbuf` 0.4 API**：`HeapRb::new` / `Split::split` / `Producer::push_slice` /
> `Consumer::pop_slice` / `Observer::occupied_len`。
> **实现时若签名对不上，以 `cargo doc -p ringbuf --open` 为准**，不要猜——但**不要退回加锁方案**。

**同一个 Step 里再建 `crates/audio/src/pcm.rs`** —— v2 DSP 的挂载点：

```rust
//! PCM 处理链。**v2 的 DSP（EQ / 响度 / 变速 / 交叉淡入）都挂在这里。**
//!
//! v1 是空实现：`process` 是恒等变换，无 stage 时纯属循环开销（可被优化掉）。
//! 留这个调用点的理由——它位于**解码与 ring 之间**，事后补要改解码循环。

pub trait PcmStage: Send {
    fn process(&mut self, samples: &mut [i16]);
}

#[derive(Default)]
pub struct PcmGraph {
    stages: Vec<Box<dyn PcmStage>>,
}

impl PcmGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// v2 用，例如 `graph.push(Box::new(EqStage::new(&config)))`
    pub fn push(&mut self, stage: Box<dyn PcmStage>) {
        self.stages.push(stage);
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// 原地处理。stage 按 push 顺序依次作用。
    pub fn process(&mut self, samples: &mut [i16]) {
        for stage in &mut self.stages {
            stage.process(samples);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Gain(f32);
    impl PcmStage for Gain {
        fn process(&mut self, samples: &mut [i16]) {
            for s in samples.iter_mut() {
                *s = ((*s as f32) * self.0)
                    .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            }
        }
    }

    #[test]
    fn empty_graph_is_identity() {
        let mut g = PcmGraph::new();
        let mut s = [1i16, -2, 3, i16::MAX, i16::MIN];
        let before = s;
        g.process(&mut s);
        assert_eq!(s, before);
    }

    #[test]
    fn stages_apply_in_order() {
        let mut g = PcmGraph::new();
        g.push(Box::new(Gain(2.0)));
        g.push(Box::new(Gain(0.5)));
        let mut s = [10i16, -20, 30];
        g.process(&mut s);
        assert_eq!(s, [10, -20, 30], "×2 再 ×0.5 应回到原值");
    }

    #[test]
    fn gain_must_saturate_not_wrap() {
        // 溢出回绕是音频里最刺耳的 bug 之一，必须饱和
        let mut g = PcmGraph::new();
        g.push(Box::new(Gain(100.0)));
        let mut s = [1000i16, -1000];
        g.process(&mut s);
        assert_eq!(s, [i16::MAX, i16::MIN]);
    }
}
```

这三个测试在 `cargo test -p dsls-audio` 里一并跑（Step 5 的 Expected 会变成 7 passed）。

- [ ] **Step 4: 写解码与引擎**

`crates/audio/src/decode.rs`：

```rust
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// 解码为交错的 i16 立体声（重采样留给后续 DSP 阶段）。
///
/// 回调取 `&mut [i16]` 而**不是** `&[i16]` —— 因为 PCM graph 的 DSP
/// 要**原地**处理样本（设计文档 §6.1）。
pub fn decode_to_i16(
    stream: Box<dyn symphonia::core::io::MediaSource>,
    ext_hint: Option<&str>,
    on_samples: &mut dyn FnMut(&mut [i16]),
) -> anyhow::Result<u32> {
    let mss = MediaSourceStream::new(stream, Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext_hint {
        hint.with_extension(e);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("文件中没有可解码的音轨"))?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut scratch: Vec<i16> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet)?;
        scratch.clear();
        push_interleaved_i16(&decoded, &mut scratch);
        on_samples(&mut scratch);
    }
    Ok(sample_rate)
}

fn push_interleaved_i16(buf: &AudioBufferRef<'_>, out: &mut Vec<i16>) {
    macro_rules! conv {
        ($b:expr) => {{
            let channels = $b.spec().channels.count();
            let frames = $b.frames();
            out.reserve(frames * channels);
            for f in 0..frames {
                for c in 0..channels {
                    let v = $b.chan(c)[f];
                    out.push((v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
            }
        }};
    }
    match buf {
        AudioBufferRef::F32(b) => conv!(b),
        AudioBufferRef::S16(b) => conv!(b),
        AudioBufferRef::S32(b) => conv!(b),
        AudioBufferRef::U8(b) => conv!(b),
        AudioBufferRef::F64(b) => conv!(b),
        AudioBufferRef::S24(b) => conv!(b),
        AudioBufferRef::U16(b) => conv!(b),
        AudioBufferRef::U24(b) => conv!(b),
        AudioBufferRef::U32(b) => conv!(b),
    }
}
```

`crates/audio/src/lib.rs`：

```rust
pub mod decode;
pub mod output;
pub mod pcm;

pub use output::{AudioRing, AudioRingConsumer, AudioRingProducer};
pub use pcm::{PcmGraph, PcmStage};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct AudioEngine {
    _stream: cpal::Stream,
    /// 生产端留在这里（解码线程用）。`HeapProd` 是 `!Sync`，所以
    /// `AudioEngine` 也 `!Sync` —— 它在单线程里使用，没有问题。
    producer: AudioRingProducer,
}

impl AudioEngine {
    pub fn new() -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("没有可用的音频输出设备")?;
        let config = device.default_output_config()?;
        log::info!("音频输出: {} @ {}Hz", device.name()?, config.sample_rate().0);

        // ~4 秒缓冲（立体声 44.1kHz）。**split 之后消费端移进回调，全程无锁。**
        let (mut producer, mut consumer) = AudioRing::new(44100 * 2 * 4).split();

        let channels = config.channels() as usize;
        // ⚠️ 回调内**绝不分配**：这里一次性预分配足够容量，
        //    回调里的 resize 就不会触发重新分配（回调缓冲远小于 8192 帧）。
        let mut scratch: Vec<i16> = Vec::with_capacity(8192 * 2);

        let stream = device.build_output_stream(
            &config.into(),
            move |out: &mut [f32], _| {
                let frames = out.len() / channels;
                scratch.resize(frames * 2, 0); // 引擎内部固定立体声
                consumer.pop_samples(&mut scratch);

                for (i, slot) in out.iter_mut().enumerate() {
                    let frame = i / channels;
                    let ch = (i % channels).min(1);
                    // 上混：立体声 → N 声道
                    *slot = scratch[frame * 2 + ch] as f32 / i16::MAX as f32;
                }
            },
            |err| log::error!("音频流错误: {err}"),
            None,
        )?;
        stream.play()?;

        Ok(Self { _stream: stream, producer })
    }

    /// 供解码线程推送样本。
    pub fn push_samples(&mut self, data: &[i16]) {
        self.producer.push_samples(data);
    }

    pub fn ring_available(&self) -> usize {
        self.producer.available()
    }

    /// **硬指标：必须为 0**（设计文档 §16.2）。
    pub fn xrun_count(&self) -> u64 {
        self.producer.underruns()
    }
}
```

> **为什么用 `ringbuf` 而不是自己写或加锁**：`HeapRb` 是无锁 SPSC 环，
> 消费端在回调里**不加锁、不分配、不阻塞**，满足本计划的 Global Constraints。
> 自己拿 `Mutex<Vec<i16>>` 会在实时线程里加锁 —— 那是 xrun 的头号来源。

- [ ] **Step 5: 运行测试确认通过**

```bash
cargo test -p dsls-audio
```

Expected: 3 passed。

- [ ] **Step 6: 提交**

```bash
git add crates/audio
git commit -m "feat(audio): 无锁 AudioRing + symphonia 解码 + cpal 输出"
```

---

## Task 9: CLI 端到端串联

**Files:**
- Modify: `app/Cargo.toml`
- Modify: `app/src/main.rs`

**Interfaces:**
- Consumes: Tasks 1–8 的全部
- Produces: 可运行的 CLI

- [ ] **Step 1: 加依赖**

`app/Cargo.toml` 追加：

```toml
dsls-provider-api = { path = "../crates/provider-api" }
dsls-provider-netease = { path = "../crates/provider-netease" }
dsls-audio = { path = "../crates/audio" }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
# 已核实：reqwest 最新稳定版是 0.13.5
reqwest = { version = "0.13", features = ["stream", "rustls-tls"] }
clap = { version = "4", features = ["derive"] }
```

- [ ] **Step 2: 写 CLI**

`app/src/main.rs` 替换为：

```rust
use clap::{Parser, Subcommand};
use dsls_audio::AudioEngine;
use dsls_provider_api::Quality;
use dsls_provider_netease::{NeteaseClient, QrState};
use std::time::Duration;

#[derive(Parser)]
#[command(about = "dslsmusic M1 —— 音乐链路 CLI 验证")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 扫码登录，把 MUSIC_U 存到 ~/.config/dslsmusic/session
    Login,
    /// 搜索并播放第一首结果
    Play {
        keywords: String,
        #[arg(long, default_value = "high")]
        quality: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    match Cli::parse().cmd {
        Cmd::Login => login().await,
        Cmd::Play { keywords, quality } => play(&keywords, &quality).await,
    }
}

fn session_path() -> std::path::PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| ".".into());
    p.push("dslsmusic");
    p
}

fn load_cookie() -> Option<String> {
    std::fs::read_to_string(session_path().join("session")).ok()
}

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
                if cookie.is_empty() {
                    anyhow::bail!("登录成功但没有拿到 MUSIC_U");
                }
                std::fs::write(dir.join("session"), &cookie)?;
                println!("\n登录成功，会话已保存。");
                return Ok(());
            }
        }
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
}

async fn play(keywords: &str, quality: &str) -> anyhow::Result<()> {
    let cookie = load_cookie();
    if cookie.is_none() {
        log::warn!("未找到会话，将以游客身份播放（可能受限）。先跑 `login` 更稳。");
    }
    let client = NeteaseClient::new(cookie);

    let q = match quality {
        "low" => Quality::Low,
        "standard" => Quality::Standard,
        "super" => Quality::Super,
        _ => Quality::High,
    };

    println!("搜索: {keywords}");
    let tracks = client.search(keywords, 5).await?;
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
    let url = client.stream_url(&track.id, q).await?;
    println!("得到地址（{} 字符）", url.len());

    let mut engine = AudioEngine::new()?;
    let bytes = reqwest::get(&url).await?.bytes().await?;
    println!("已下载 {} KB，开始播放…（Ctrl-C 停止）", bytes.len() / 1024);

    let cursor = std::io::Cursor::new(bytes.to_vec());
    // push_samples 需要 &mut（生产端是 !Sync）。闭包借用 engine，
    // 闭包结束后借用即释放，后面的 ring_available() 可以直接用。
    const CHUNK: usize = 4096;
    // v1 是空图（零开销）；v2 的 EQ / 响度 / 变速都挂在这里
    let mut pcm = dsls_audio::PcmGraph::new();
    dsls_audio::decode::decode_to_i16(Box::new(cursor), Some("mp3"), &mut |samples| {
        for chunk in samples.chunks_mut(CHUNK) {
            pcm.process(chunk);          // ← 解码与 ring 之间：DSP 的位置
            engine.push_samples(chunk);
        }
        // 背压：缓冲够满就退让，避免解码跑得比播放快太多
        while engine.ring_available() > 44100 * 2 * 3 {
            std::thread::sleep(Duration::from_millis(20));
        }
    })?;

    // 等缓冲播完
    while engine.ring_available() > 0 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("\n播放结束。xrun 计数 = {}（必须为 0）", engine.xrun_count());
    Ok(())
}
```

`crates/audio/src/lib.rs` 追加一个方法：

```rust
impl AudioEngine {
    pub fn ring_available(&self) -> usize {
        self.ring.lock().map(|r| r.available()).unwrap_or(0)
    }
}
```

`app/Cargo.toml` 再加：

```toml
dirs = "6"
```

- [ ] **Step 3: 构建**

```bash
cargo build -p dslsmusic
```

Expected: 编译通过。

- [ ] **Step 4: 端到端验证 —— 登录**

```bash
cargo run -p dslsmusic -- login
```

Expected:
- 输出二维码保存路径
- 打开该 PNG，用网易云 App 扫码 → 手机确认
- 终端最终输出「登录成功，会话已保存。」
- `~/.config/dslsmusic/session` 存在且内容是一串 token

**只做扫码，不要做任何写操作**（签到 / 打卡 / 云盘上传）—— 设计文档 §4.4 硬约束。

- [ ] **Step 5: 端到端验证 —— 出声**

```bash
cargo run -p dslsmusic -- play "周杰伦 晴天"
```

Expected:
1. 打印搜索结果列表
2. 打印解析到的播放地址
3. **扬声器出声**
4. 播放结束打印 `xrun 计数 = 0`

**若 `xrun` 不为 0** —— 说明 buffer 太小或解码线程饿死，**这是必须修的 bug，不是可接受的误差**。

- [ ] **Step 6: 失败路径验证（重要，别跳过）**

搜索一个不存在的词：

```bash
cargo run -p dslsmusic -- play "zzzzzzzzzzzzz不存在zzzzzzzzzzz"
```

Expected: 打印「没有结果。」并以 **退出码 0** 结束（`Ok(None)` 语义，不是错误）。

再验证未登录状态下取一个版权受限的歌。Expected: 出现 `Unavailable` 类错误信息，**而不是 panic 或静默失败**。

- [ ] **Step 7: 提交**

```bash
git add app crates/audio
git commit -m "feat(m1): CLI 端到端串联 —— 扫码登录 / 搜索 / 取流 / 出声"
```

---

## 完成标准

M0 与 M1 全部完成当且仅当：

- [ ] 仓库根有 `LICENSE`（GPLv3 全文），且 workspace `license = "GPL-3.0-or-later"`
- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace -- -D warnings` 无告警
- [ ] 运行 `cargo run -p dslsmusic` 弹出窗口、显示三角形、日志显示 `RenderTier: A` 且适配器**不是** llvmpipe
- [ ] 日志出现帧时间统计行
- [ ] `cargo run -p dslsmusic -- login` 能完成扫码并落地 session
- [ ] `cargo run -p dslsmusic -- play "<歌名>"` **能出声**，结束时 `xrun 计数 = 0`
- [ ] 搜索无结果时**退出码为 0**

## 已知遗留（明确不在 M0/M1 范围内）

| 遗留项 | 处理时机 |
|---|---|
| `ringbuf` 0.4 的实际 API 签名需在 Task 8 实现时对照 `cargo doc` | Task 8 |
| 网易云 eapi 加密正确性未交叉验证 | M2，对照 `ddy314/netease-music` |
| 未做 Android 真机验证 | M3 之后 |
| 未做 `downlevel_webgl2_defaults()` 的 Tier B/C 路径实测 | M3 |
| 未接 amll-ttml-db 歌词 | M6 |
| UI runtime 完全未开始 | M4 |
