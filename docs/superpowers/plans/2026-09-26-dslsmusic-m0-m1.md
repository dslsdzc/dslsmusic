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
- **任何依赖 `log` 输出的验证命令必须带 `RUST_LOG=info`。** `env_logger` 在无此环境变量时默认 Error 级，`log::info!` / `log::warn!` 会被静默丢弃——**命令会「成功」但打不出任何东西，Expected 因此不可达**。（Task 3 实测踩到，同一根因在计划里有 3 处。）
- **本项目是 GPLv3** —— 因此 GPL-3.0 / AGPL-3.0 的参考实现**可用**（此前被错误否决），但**非商业许可（如 PolyForm NC）与 GPLv3 不兼容，绝不可用**；无 license 的同样不可用（§14.1）。
- **⚠️ 本计划里的代码块是节选，不保证 `cargo fmt` 干净。** 这不是「计划写错了」，
  而是**预期行为**：代码块为可读性省掉了括号/换行/import 排序，而 rustfmt 有
  `fn_call_width = 60` 之类的硬性宽度（仓库内**没有** `rustfmt.toml`，走默认配置）。
  **实现时一律以 `rustfmt` 为准**，把规范化处列进报告的偏离节即可（含回退法）。
  *（这条已经发生过 4 次，每次实现者都要重新发现一遍——写在这里省掉那一轮。）*
- **⚠️ 做「编译期穷尽性检查」的 `match` 时，`_ =>` 和 `other =>` 都会让检查失效**
  （`other` 是**绑定模式**，匹配一切）。要**逐个列出变体**。
  这个坑已经踩过一次：改了写法、看着更合理，但性质丢了。**换写法后要重新验证性质，不是验证语法。**

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
│   │       └── model.rs            # Quality / Track（Artist、Album 属 v2 详情页，M1 不做）
│   ├── provider-netease/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs           # ncm-api-rs 封装 + 会话
│   │       ├── qr_login.rs         # 扫码登录状态机 ★
│   │       └── parse.rs            # ApiResponse → model ★
│   └── audio/
│       └── src/
│           ├── lib.rs              # AudioEngine
│           ├── decode.rs           # symphonia
│           ├── output.rs           # 无锁 AudioRing（ringbuf）
│           └── pcm.rs              # v2 DSP 挂载点（v1 为空图）
└── app/
    ├── Cargo.toml
    ├── tests/
    │   └── metrics.rs              # percentile 的集成测试
    └── src/
        ├── main.rs                 # CLI 入口
        ├── lib.rs                   # 暴露 metrics，供 tests/ 使用
        ├── metrics.rs              # 帧时间直方图 ★
        └── shader.wgsl
```

★ = 含可单测的纯逻辑

> ⚠️ **这段索引是手工维护的，会漂移。** 它已经漂移过三次
> （`pcm.rs`、`app/src/lib.rs`、`app/tests/` 都是后加的任务产出，
> 加完没回头更新这里），以及一次反向漂移（曾声称 `model.rs` 有
> `Artist` / `Album` 而实际没有）。
>
> **权威是各任务 body 里的 `Files:` 列表，不是这段。**
> 改动下层文件清单时，回来同步这里；或者直接把这段删掉——
> 它提供的信息（目录概览）不值得持续维护成本。

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
# ⚠️ 用 glob 而不是逐个列 —— Task 5/6/8 会各自新建 crate，
# 逐个列的话每加一个都要记得回来改这里，漏一次 cargo 就报
# "package not found"。glob 让新 crate 自动成为成员。
members = ["crates/*", "app"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "GPL-3.0-or-later"
# MSRV。edition 2024 本身要求 ≥1.85，所以这个值是自洽的下限。
# 与 rust-toolchain.toml 的 channel pin 是两件事：
#   - channel pin  → 保证「编出相同的代码」（可复现），代价是每个贡献者多下一份工具链
#   - rust-version → 保证「旧工具链得到清晰错误」而非难解失败
# 依赖的 MSRV 可能更高，那种情况 cargo 会指名报错，不会静默失败。
rust-version = "1.85"

[workspace.dependencies]
# ── 平台层 ──
sdl3 = { version = "0.20", features = ["raw-window-handle"] }
wgpu = "30.0.1"
pollster = "1"
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
# 固定到具体版本，而不是 "stable" —— 后者的实际含义是「用你本机装的
# 那个 stable」，既不提供可复现性（新 stable 会静默编出不同的代码），
# 又会让旧 stable 用户撞上难以理解的失败。本机实测为 1.96.1。
channel = "1.96"
components = ["rustfmt", "clippy"]
```

再创建仓库根的 `LICENSE`，**内容是 GPLv3 全文**：

系统的 GPLv3 文本位置因发行版而异，**按序找第一个存在的**：

```bash
for p in /usr/share/common-licenses/GPL-3 \
         /usr/share/licenses/spdx/GPL-3.0-or-later.txt \
         /usr/share/licenses/GPL-3.0-or-later/LICENSE \
         /usr/share/licenses/gpl-3.0-or-later.txt; do
  [ -f "$p" ] && cp "$p" LICENSE && echo "取自 $p" && break
done
```

**若一个都不存在，报告 BLOCKED**——GPLv3 是法律文本，**不要凭记忆写一份**。

**选到文件后必须验证完整性**，不能只信文件名：
- 字节数应在 34–36 KB 量级
- 应含 `END OF TERMS AND CONDITIONS` 与 `How to Apply These Terms`
- 应含 0–17 共 18 个编号小节
- 若该文件由系统包提供，用包管理器校验（Arch: `pacman -Qkk licenses`；Debian: `dpkg -V`）

> **计划早期版本把这里写成了单一 Debian 路径 `/usr/share/common-licenses/GPL-3`
> 并要求「不存在就 BLOCKED」——那在本机（Arch）根本不存在，指令字面上无法执行。
> 已改为按序查找 + 完整性验证，这才是可移植的写法。**

- [ ] **Step 3: 建 platform crate 骨架**

`crates/platform/Cargo.toml`：

```toml
[package]
name = "dsls-platform"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
sdl3.workspace = true
wgpu.workspace = true
# Task 3 的 gpu.rs 里用 pollster::block_on 驱动 request_adapter / request_device
pollster.workspace = true
# 公开接口返回 anyhow::Result。
# **这是显式选择，不是漂移**：M0–M1 里 platform 的错误全是启动期致命错误
# （SDL init / 窗口创建 / adapter / device / create_surface），调用方除了
# 报错退出没有别的动作，类型化错误枚举不会被产出。
# 若将来要改类型化（device lost、显示器热插拔、适配器/音频设备切换都会
# 变成可恢复的），需同时改 `init_gpu` 与 `PlatformWindow::new` 的签名。
anyhow.workspace = true
log.workspace = true

# ── 已删除的两个依赖，勿再加回 ──
# thiserror：同上的理由，M0–M1 不产出类型化错误（推迟，非否决）
# raw-window-handle：代码一律走 `wgpu::rwh`（wgpu 的无条件重导出），
#   直接依赖它**既买不到版本对齐**（cargo 按各自 semver 解析），
#   **反而制造陷阱**：后来者若写 `use raw_window_handle::HasWindowHandle`，
#   实现的会是「直接依赖解析到的那个版本」的 trait，不再保证与 wgpu 的
#   同版本 —— 那正是我们要避免的「两份 trait」错误。
```

`crates/platform/src/lib.rs`：

```rust
//! SDL3 的唯一接触面。
//!
//! 本 crate 之外的任何地方都不允许出现 `use sdl3::...`（设计文档 §11.3）。
//! **这条由依赖图强制**：只有本 crate 依赖 sdl3，别处写了会直接编译失败。
//!
//! 注意：**wgpu 不是本 crate 独有**——`app` 的 main.rs 会直接用 wgpu 类型
//! 建管线。让渲染收敛到 `ui-render` 是后续目标，不是当前的不变量。

pub mod capability;
```

**注意：这里不要写 `pub use capability::RenderTier;`** —— `RenderTier` 是 Task 2
才创建的类型，现在写这行是硬编译错误。Task 3 会重写本文件并补上完整的重导出。

- [ ] **Step 4: 建 app 骨架**

`app/Cargo.toml`：

```toml
[package]
name = "dslsmusic"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

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
    /// ⚠️ 必须是 `RefCell`：`GpuContext<'a>` 的 surface 借自本结构体
    /// （经 `raw_window()`），而且是**全程共享借用**，所以 `poll_events`
    /// 拿不到 `&mut self`。用内部可变性只让 event_pump 可变，公开签名
    /// 保持 `&self` 不变。
    ///
    /// ## ⚠️ 下游契约（后续所有任务都要遵守）
    ///
    /// 由此派生出一条**长期规则**：`PlatformWindow` 上任何需要改状态的
    /// 公开方法**都不能再用 `&mut self`** —— sdl3 里 `set_title` /
    /// `set_fullscreen` / `set_min_size` / 光标 / 剪贴板**全部**是
    /// `&mut self`。它们必须走内部可变性（再包一个 `RefCell`/`Cell`），
    /// 或者保证在有 `GpuContext` 存活时不可达。
    ///
    /// **这不是假想风险**：本项目迟早要做「窗口标题跟随当前曲目」，
    /// 那正好会撞上这一条。
    ///
    /// 另：任何调用点都必须把 `window` 声明在 `gpu` **之前**（借用检查器
    /// 强制，绕不过去）。`main.rs` 的 `let window = ...` 去掉 `mut`
    /// 已经是这条契约的第一个表现。
    event_pump: std::cell::RefCell<sdl3::EventPump>,
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
        Ok(Self {
            window,
            event_pump: std::cell::RefCell::new(event_pump),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.window.size();
        (w, h)
    }

    /// 供 `gpu.rs` 创建 surface 使用。**不要在别处调用。**
    pub(crate) fn raw_window(&self) -> &sdl3::video::Window {
        &self.window
    }

    /// 取走本帧的全部事件。
    ///
    /// **resize 不是「最后一个获胜」**：物理像素的 `PixelSizeChanged` 优先于
    /// 逻辑点的 `Resized`（见下面的 `got_pixel_size` 标志位）；遇到
    /// `Quit` / `Esc` 立即返回。
    ///
    /// **收 `&self` 而非 `&mut self`** —— `GpuContext` 全程共享借用本结构体，
    /// 收 `&mut` 会与它冲突（E0502）。可变性由 `RefCell` 提供。
    pub fn poll_events(&self) -> PlatformEvent {
        let mut result = PlatformEvent::None;
        // ⚠️ 真需要这个标志位：同一个 pump 批次里可能**两个事件都来**，
        //    而 or-pattern 只能决定单次匹配内的绑定顺序，**不构成两个
        //    事件之间的优先级**。没有它的话，若 PixelSizeChanged 之后
        //    又来了 Resized，物理值会被逻辑值覆盖。
        let mut got_pixel_size = false;
        let self_id = self.window.id();
        for event in self.event_pump.borrow_mut().poll_iter() {
            match event {
                Event::Quit { .. } => return PlatformEvent::CloseRequested,
                Event::KeyDown { keycode: Some(Keycode::Escape), .. } => {
                    return PlatformEvent::CloseRequested;
                }
                Event::Window {
                    window_id,
                    win_event: WindowEvent::PixelSizeChanged(w, h),
                    ..
                } if window_id == self_id => {
                    result = PlatformEvent::Resized(w as u32, h as u32);
                    got_pixel_size = true;
                }
                Event::Window {
                    window_id,
                    win_event: WindowEvent::Resized(w, h),
                    ..
                } if window_id == self_id && !got_pixel_size => {
                    // 物理像素优先：已拿到 PixelSizeChanged 就不被逻辑值覆盖。
                    // 条件写进 match guard（而不是块内 if）——否则触发 clippy
                    // 的 collapsible_match，而完成标准要求
                    // `cargo clippy --workspace -- -D warnings` 无告警。
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
    // ⚠️ 必须用 size_in_pixels()（SDL_GetWindowSizeInPixels，物理像素），
    // **不是 size()**（SDL_GetWindowSize，逻辑点）。SurfaceConfiguration
    // 的 width/height 是物理像素，而 resize 路径用的是物理的
    // PixelSizeChanged —— 用逻辑尺寸会让首次 configure 与之后每一次都
    // 不一致（scale != 1.0 时），swapchain 被合成器放大变模糊，
    // 并产生 VK_SUBOPTIMAL_KHR（见 main.rs 对 Suboptimal 的处理）。
    let (width, height) = window.size_in_pixels();

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
//! SDL3 的唯一接触面。
//!
//! 本 crate 之外的任何地方都不允许出现 `use sdl3::...`（设计文档 §11.3）。
//! **这条由依赖图强制**：只有本 crate 依赖 sdl3，别处写了会直接编译失败。
//!
//! 注意：**wgpu 不是本 crate 独有**——`app` 的 main.rs 会直接用 wgpu 类型
//! 建管线。让渲染收敛到 `ui-render` 是后续目标，不是当前的不变量。

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

    // window 不需要 mut：poll_events 收 &self（见 window.rs 的说明）
    let window = PlatformWindow::new("dslsmusic", 1280, 720)?;
    // gpu 需要 mut：下面会调用 surface.configure
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
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded => continue,
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost => {
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
            // Validation 是真正无法继续的状态，bail 合理
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
# ⚠️ 必须带 RUST_LOG=info —— env_logger 无此环境变量时默认 Error 级，
#    log::info! 会被全部丢弃，下面那行 Expected 根本打不出来。
RUST_LOG=info cargo run -p dslsmusic 2>&1 | head -20
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

> ⚠️ **实现前必读**：`app/src/main.rs` 里有**两条 `continue` 路径**
> （`Timeout|Occluded`、`Outdated|Lost`），它们都在 `begin_frame()` 之后、
> `end_frame()` 之前跳出循环。
>
> （注：早先此处写「三条」并列入 `Suboptimal`——**那是错的**。`Suboptimal`
> 修复后返回纹理并**落到 `end_frame`**，那些帧是被采样的。）
> **真实的 `FrameMetrics` 会看到未配对的 `begin_frame`。**
> 你的实现必须容忍这一点——例如把 `begin_frame` 只当作「记下起点」，
> 而让 `end_frame` 成为唯一的样本产出点；**或**把配对责任移到调用方。
> 选一种并在文档注释里写清楚，不要让「未配对」变成隐式的样本丢失。
>
> （Task 3 的占位实现是空函数，所以这个问题当时不显形。）

- [ ] **Step 4: 写实现**

`app/src/metrics.rs`：

```rust
use std::time::Instant;

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
///
/// 用**线性插值**（Hazen 估计 / Hyndman & Fan type 5）：`pos = p·n − 0.5`。
///
/// ⚠️ **不要用纯取整下标**。本计划早先用的是 `((n-1) as f32 * p).round()`，
/// 它通不过自己的测试：对「99×8ms + 1×100ms」，`(99 * 0.99).round() = 98`
/// → 返回 `sorted[98] = 8.0ms`，**恰好漏掉它该抓的那一帧**——而 §16.5 里
/// p99 存在的唯一理由就是抓这种长尾。
/// （n=100 时 p50 要求下标 49、p99 要求下标 99，任何纯取整公式都无法同时
/// 满足这两条。）
pub fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let n = sorted.len();
    let p = p.clamp(0.0, 1.0);
    let pos = p * n as f32 - 0.5;
    if pos <= 0.0 {
        return sorted[0];
    }
    let hi = pos.floor() as usize;
    if hi + 1 >= n {
        return sorted[n - 1];
    }
    let frac = pos - hi as f32;
    sorted[hi] * (1.0 - frac) + sorted[hi + 1] * frac
}

/// 供后续 GPU timestamp 查询使用；GLES 后端不支持时回退到 CPU 计时（设计文档 §16.5）。
///
/// 注意 `target_fps = 0` 会返回 `inf`。v1 无调用方，等它有了调用方再补测试与守卫。
pub fn frame_budget_ms(target_fps: u32) -> f32 {
    1000.0 / target_fps as f32
}
```

- [ ] **Step 5: 运行测试确认通过**

```bash
cargo test -p dslsmusic --test metrics
```

Expected: 4 passed。

> ⚠️ **两条测量语义（审查实测确认，M3 之前必须知道）**：
>
> 1. **`n=600` 时 p99 是第 6 差的样本**——实测 `[16.7;599] + [1002.0]` 的 p99
>    仍是 16.7，**要 ≥7 个慢样本才能推动它**。孤立的一次卡顿 p99 看不见，
>    **只有 `max` 能抓到**。§16.5 说「要看 p99 而非平均值」仍然成立，
>    但**不能只靠 p99**。
> 2. **测量窗口排除了 `poll_events` 与推迟的 `configure`**（两者都在
>    `begin_frame` 之前，见 `main.rs`）。所以**事件循环与重配的停顿对五个
>    统计量全部不可见**——而那正是 **M3 玻璃原型要反复触发的区域**。
>    若要让那类停顿进入统计，需把 `begin_frame()` 上移到 `poll_events` 之前。
>    **这是 M3 前要重新决定的事，不是现在改。**

- [ ] **Step 6: 运行并观察真实数据**

```bash
# 必须带 RUST_LOG=info，否则 log::info! 被 env_logger 丢弃，grep 什么都找不到
RUST_LOG=info cargo run -p dslsmusic 2>&1 | grep "帧时间" | head -5
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
    ⚠️ 语义已于 2026-09-26 收口：**「查到了但没结果」= `Ok(Some(空集合))`；
    `Ok(None)` = 「本 provider 不处理」，与 `Err(Unsupported)` 等价**（设计文档 §4.5）
  - `pub struct CapabilitySet`（bitflags 或 HashSet）
  - `pub enum Quality { Low, Standard, High, Super }` —— ⚠️ **Task 5b 会把它扩到 5 档**
    并改名（`Super` → `Lossless`，新增 `HiRes`）；Task 7 起请以 Task 5b 的为准
  - `pub struct Track { pub id: String, pub platform: String, pub title: String, pub artist: String, pub album: String, pub duration_ms: u64 }`

- [ ] **Step 1: 写失败的测试**

`crates/provider-api/src/error.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_distinguishable_from_empty() {
        // ⚠️ 收口后的语义（设计文档 §4.5，Task 7 复审发现的定义冲突）：
        //   「查到了但没结果」= Ok(Some(空集合))   ← resolver **不该**换音源
        //   「本 provider 不处理」= Ok(None) 或 Err(Unsupported) ← **应该**换音源
        //
        // ⚠️ **本测试只有编译期内容，一条运行时断言都没有 —— 这不是漏写。**
        //
        // 这里曾经有 5 条 `assert_*!` / `matches!`，它们比的**全是本函数两三行前
        // 自己绑定的字面量**：`Ok(Some([])) != Ok(None)` 由 `Option` 的 `PartialEq`
        // 保证、`Ok(_) != Err(_)` 由 `Result` 的 `PartialEq` 保证 —— **都是 std 的 derive**。
        // ⇒ **没有任何实现改动能让它们变红**（反证：这个测试做不了变异测试，
        // 因为没有可改的实现）。**留一半装饰性断言是最坏的选择** ——
        // 看起来在测，其实什么也没测。已全部删除。
        //
        // 本测试真正的内容是**下面三个绑定的类型标注**：别名若被改成
        // 另一种形状，这三行**直接编译失败**。值不参与任何检查，所以刻意用 `_` 命名。
        //
        // ⚠️ 早先这里写着「这条断言钉的是 resolver 能看出这两类不同」——**那句是假的**。
        // 让 resolver 分得出来的是 `SourceResult` 的**形状**，而形状由这三个绑定钉住。
        // 「该不该换音源」这类判断要在运行时测，就该测**真函数**
        // （`should_try_next_provider` 那种），而不是把字面量互相比一遍。
        let _empty: SourceResult<Vec<Track>> = Ok(Some(Vec::new()));
        let _not_handled: SourceResult<Vec<Track>> = Ok(None);
        let _unsupported: SourceResult<Vec<Track>> = Err(SourceError::Unsupported);
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
license.workspace = true
rust-version.workspace = true

[dependencies]
thiserror.workspace = true
```

`crates/provider-api/src/model.rs`：

```rust
/// 音质档位。各平台原始标识在 provider 内部映射（设计文档 §4.5）。
///
/// `#[non_exhaustive]`：resolver 马上要 `match` 它，而写操作/新音源可能
/// 引入新档位。**加变体是破坏性变更，现在标比以后改便宜。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
// ⚠️ **这是 Task 5 完成时的状态，不是最终形态。**
//    Task 5b 会把它扩到 5 档并改名：`Super` → `Lossless`，新增 `HiRes`。
//    **Task 7 起请以 Task 5b 的代码为准。**
pub enum Quality {
    Low,
    Standard,
    High,
    Super,
}

impl Quality {
    /// 降一档。已是最低档时返回 `None`。
    ///
    /// **没有它，resolver 的降级链写不出来**——`Quality` 只派生 `Ord`，
    /// 无法枚举档位。而 `SourceError::should_retry_lower_quality()` 返回
    /// true 时，调用方必须知道「更低是哪一档」才能重试。
    pub fn lower(self) -> Option<Quality> {
        match self {
            Quality::Super => Some(Quality::High),
            Quality::High => Some(Quality::Standard),
            Quality::Standard => Some(Quality::Low),
            Quality::Low => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    /// **这首歌来自哪个音源**（provider 的 `name()`）。
    ///
    /// 必须有：不同音源的 id 命名空间是私有的（网易云是数字串、本地是
    /// 路径/哈希、云盘是另一套）。没有它，resolver 拿到一个 `Track`
    /// **无法路由回正确的 provider 去调 `stream_url`**，只能外挂一张
    /// (provider, track) 边表；跨音源合并去重也没有键。
    ///
    /// 设计文档 §4.5 的「统一 item」本来就含 `platform`——这里是补齐。
    pub platform: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 毫秒
    pub duration_ms: u64,
}
```

`crates/provider-api/src/error.rs`：

```rust
// Track 只在测试里用到（`SourceResult<T>` 是泛型别名，不构成使用）。
// 早先为了压住这个未使用警告，写过一个「什么也不检查」的 const _ 块——已删。
// 正确做法就是标 cfg(test)。
#[cfg(test)]
use crate::model::Track;

// PartialEq/Eq 是给调用方与测试用的：source-resolver 要能对比错误，
// 测试也要能直接断言「这两个结果不同」，否则只能在已知字面量上调 unwrap()，
// 而那会触发 clippy::unnecessary_literal_unwrap。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
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

/// **关键契约**（语义已于 2026-09-26 收口，见设计文档 §4.5）：
///
/// | 值 | 含义 | resolver 该做什么 |
/// |---|---|---|
/// | `Ok(Some(v))` | 拿到了。**`v` 可以是空集合**＝查到了但没结果 | 用它；**不要**换音源 |
/// | `Ok(None)` | **本 provider 不处理这个请求** | **换一个音源试** |
/// | `Err(Unsupported)` | **同上**，另一种拼写 | 同上 |
/// | `Err(Unavailable)` / `Err(RateLimited)` | 有能力，但当前拿不到 | 降档重试 |
///
/// ⚠️ **`Ok(None)` 与 `Err(Unsupported)` 语义等价** —— 这是 `Option` 带来的冗余，
/// **不是需要区分的两种情况**。早先的注释把「`Ok(None)`＝确实没有」与
/// 「`Err(Unsupported)`＝没这个能力」当成两回事，**那是错的**：
/// 它会让 `Ok(None)` 与 `Ok(Some(空集合))` 撞车，而这两者导向**相反的动作**。
pub type SourceResult<T> = Result<Option<T>, SourceError>;

// （此处原有一个 `const _: () = { fn _t(_: SourceResult<Track>) {} };` 块，
//   声称「编译期确认 Track 被用在类型别名里」——**它什么也不检查**：
//   `SourceResult<T>` 是泛型别名，约束不了任何东西。已删除。）
```

`crates/provider-api/src/capability.rs`：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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

    // 签名写成 rustfmt 稳定的单行 —— 多行写法会被 cargo fmt 折叠，
    // 而全 workspace 其余文件都是 fmt-clean，留着它会是唯一例外。
    async fn search(&self, keywords: &str, limit: u32) -> SourceResult<Vec<Track>>;

    async fn stream_url(&self, track_id: &str, quality: Quality) -> SourceResult<String>;
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

## Task 5b: 音质模型扩到真实的保真度梯度（+ 预留音效轴）

> **为什么有这个任务**：Task 5 建的是 4 档 `Low/Standard/High/Super`——那是我在
> **没核实网易云真实档位**的情况下定的。2026-09-26 的核实推翻了它：真实是
> **5 档保真度**，外加 **5 个不可排序的 AI/空间音频档**。
> 完整论证见设计文档 §4.5「音质是两根轴」。
>
> **Task 7 依赖本任务的产出，所以它必须排在 Task 7 之前。**

**Files:**
- Modify: `crates/provider-api/src/model.rs`
- Modify: `crates/provider-api/src/lib.rs`

**Interfaces:**
- Consumes: Task 5 的 `Quality`
- Produces:
  - `pub enum Quality { Low, Standard, High, Lossless, HiRes }`
    （**`Super` 改名为 `Lossless`，新增 `HiRes`**）
  - `Quality::lower()`：`HiRes → Lossless → High → Standard → Low → None`
    ⚠️ 实现者实测发现初稿的 `ladder_has_exactly_five_rungs` **起点写反了**
    （从 `Low` 起步数数，循环体一次不进，恒为 1）——已按下面的版本改成
    穷尽 `match` + 从 `HiRes` 起步
  - `pub enum AudioEffect { Jyeffect, Jymaster, Sky, Dolby, Vivid }`
    —— **与 `Quality` 之间没有 `Ord` 关系**

- [ ] **Step 1: 改测试（先让它失败）**

`crates/provider-api/src/model.rs` 的 `mod tests` 里，把 `lower` 的两条测试改成
新档位，并**新增一条钉住档位总数的**：

```rust
    #[test]
    fn lower_walks_the_five_fidelity_tiers_to_none() {
        assert_eq!(Quality::HiRes.lower(), Some(Quality::Lossless));
        assert_eq!(Quality::Lossless.lower(), Some(Quality::High));
        assert_eq!(Quality::High.lower(), Some(Quality::Standard));
        assert_eq!(Quality::Standard.lower(), Some(Quality::Low));
        assert_eq!(Quality::Low.lower(), None);
    }

    /// 钉住**保真度梯度就是这 5 档、且是一条直线**。
    ///
    /// ⚠️ **不要从 `Low` 起步往下数。** `lower()` 是**向下降级**的，
    /// 从最低档起步循环体一次都不进，`n` 恒为 1 ——
    /// **那条测试无论实现怎么写都不可能通过。**（本计划的初稿正是这么写的，
    /// 实现者实测 `left: 1, right: 5` 才发现。）
    ///
    /// ⚠️ **也不要只从 `HiRes` 起步数个数。** 那样「在 `HiRes` **之上**
    /// 再加一档」会**静默通过**，与这条测试自称的用途不符。
    ///
    /// 所以用两件事合起来钉：
    /// **(a)** 一个**穷尽 `match`** —— 加变体而没列进这里，`match` 会因为
    /// 不穷尽而**编译失败**（`Quality` 虽标了 `#[non_exhaustive]`，
    /// 但那只约束**下游** crate，crate 内部的 `match` 仍是穷尽的）；
    /// **(b)** 从**最高档**起步逐级降，必须正好 4 步落到 `Low`。
    #[test]
    fn ladder_is_exactly_the_five_fidelity_tiers() {
        let mut q = Quality::HiRes;
        let mut steps = 0usize;
        while let Some(next) = q.lower() {
            steps += 1;
            // 防死循环（不是对梯子形状的断言）：`lower()` 是总函数，
            // 理论上可以配出环，跑测试时挂死比断言失败更难查。
            assert!(steps < 16, "降级链出现环");
            q = next;
        }
        assert_eq!(steps, 4, "HiRes→Low 应正好 4 步（5 档）");

        // ⭐ **终点断言故意写成穷尽 `match`，不加辅助函数、不加 `#[allow]`。**
        //    给 `Quality` 加第 6 个变体而没列进这里 → 这个 `match` 不穷尽
        //    → **E0004 编译失败**。这是唯一能堵住「在 HiRes 之上再加一档
        //    会静默通过」的办法，且不需要 strum 之类的枚举反射依赖。
        //
        //    ⚠️⚠️ **必须逐个列出变体。以下两种写法都会让检查静默失效：**
        //
        //        _ => {}                    // 通配臂
        //        other => panic!(...)       // ← 这个更阴：`other` 是**绑定模式**，
        //                                   //   它匹配一切，所以 match 永远穷尽，
        //                                   //   加第 6 个变体**不会**编译失败。
        //
        //    后者看起来像「其余情况」，读起来也对，但它不是穷尽性检查。
        //    （实现者实测：加第 6 档后这种写法照样 PASS。）
        match q {
            Quality::Low => {}
            Quality::Standard | Quality::High | Quality::Lossless | Quality::HiRes => {
                panic!("降级链的终点必须是 Low，实际是 {q:?}")
            }
        }
    }

    /// 单调性：每一档严格低于上一档。
    ///
    /// **它不是上面那条的复述。** 上面只查「几步到 Low」；若把 `lower()` 的
    /// **声明顺序**写反（例如把 `Quality::Low => None` 挪到最前），
    /// 只有这一条会失败。（复审用变异测试确认过：6 组变异里，
    /// 「声明顺序反转」那一组**只有它 FAIL**。）
    #[test]
    fn lower_is_strictly_decreasing() {
        let mut q = Quality::HiRes;
        let mut steps = 0usize;
        while let Some(next) = q.lower() {
            assert!(next < q, "lower() 必须严格下降：{q:?} -> {next:?}");
            q = next;
            steps += 1;
            // 防死循环。**这不是对梯子形状的断言** —— `lower()` 是总函数，
            // 理论上能配出环，而测试挂死比断言失败难查得多。
            assert!(steps < 16, "降级链出现环");
        }
        assert_eq!(q, Quality::Low, "链的终点必须是最低档");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p dsls-provider-api
```

Expected: 编译失败（`Quality::HiRes` 不存在）。

- [ ] **Step 3: 改实现**

`crates/provider-api/src/model.rs`：

```rust
/// **保真度**档位。有序，可沿 [`Quality::lower`] 逐级降。
///
/// ⚠️ **这里只有保真度。** 网易云的 `jyeffect` / `jymaster` / `sky` /
/// `dolby` / `vivid` **不属于本枚举**——它们是不同的混音／声道结构，
/// 不可排序、也不可互相降级（设计文档 §4.5）。见 [`AudioEffect`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Quality {
    /// 网易云 `standard`，128k
    Low,
    /// 网易云 `higher`，192k
    Standard,
    /// 网易云 `exhigh`，320k
    High,
    /// 网易云 `lossless`，无损
    Lossless,
    /// 网易云 `hires`，高解析度
    HiRes,
}

impl Quality {
    /// 降一档。已是最后一档则返回 `None`——降级链到此为止。
    pub fn lower(self) -> Option<Quality> {
        match self {
            Quality::HiRes => Some(Quality::Lossless),
            Quality::Lossless => Some(Quality::High),
            Quality::High => Some(Quality::Standard),
            Quality::Standard => Some(Quality::Low),
            Quality::Low => None,
        }
    }
}

/// **AI / 空间音频**档位。**与 [`Quality`] 之间没有 `Ord` 关系。**
///
/// 它们不是「同一首歌的更高保真度」，而是**不同的混音或声道结构**：
/// `jymaster` 是神经网络升频、`sky` 是 AI 分离声部后模拟 5.1、
/// `dolby` / `vivid` 是空间音频格式。因此 **`sky → jymaster` 不是降级**，
/// 把它们塞进 `Quality` 的 `Ord` 梯度会产出荒唐的降级路径。
///
/// **v1 不实现。** 这个类型存在的意义是把「这里有一条轴」写进类型里，
/// 防止以后误把音效档当成保真度档塞进 `Quality`（用户 2026-09-26 裁决）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioEffect {
    /// 高清臻音。⚠️ 官方中文名**待定**——来源互相矛盾，别照抄任何一处
    Jyeffect,
    /// 超清母带。**神经网络升频，不是原生更高的保真度**
    Jymaster,
    /// 沉浸环绕声。AI 分离声部后**模拟** 5.1
    Sky,
    /// 杜比全景声。⚠️ 需要 `os=pc`，**与 `Vivid` 互斥**
    Dolby,
    /// 臻音全景声。⚠️ 需要 `os=android` + `appver=9.5.61`，**与 `Dolby` 互斥**
    ///
    /// ⚠️ **中文名的证据弱，落地前需外部核实**：`vivid` 被 api-enhanced 自己的
    /// 枚举、ncm-api-rs 文档、SPlayer 的 `song.ts` 联合类型**全部排除在外**，
    /// 只有 api-enhanced 文档与 SPlayer 的 UI 配置表提到它。
    Vivid,
}
```

`crates/provider-api/src/lib.rs` 的 re-export 补上 `AudioEffect`：

```rust
pub use model::{AudioEffect, Quality, Track};
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p dsls-provider-api
```

Expected: 全过（原 11 条 + 2 条新 = **13 条**：新增 `ladder_is_exactly_the_five_fidelity_tiers`
与 `confirmed_without_session_cookie_is_a_protocol_error` 之外的那条阶梯测试；
`lower_walks_the_five_fidelity_tiers_to_none` 是**改名**自原有测试，不增计数）。
⚠️ 以实际运行结果为准 —— 实现者报的 12 条与本算式不一致，复审时确认过差异来源。

- [ ] **Step 5: 提交**

```bash
git add crates/provider-api
git commit -m "feat(provider-api): Quality 扩到 5 档保真度梯度，新增 AudioEffect 预留音效轴"
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
  - `pub const PLATFORM: &str = "netease"` —— 平台标识的唯一来源
  - `pub enum QrState { WaitingScan, WaitingConfirm, Confirmed { cookie: String }, Expired }`
  - `impl QrState { pub fn from_code(code: i64, raw_cookies: &[String]) -> Result<Self, SourceError> }`
    ⚠️ **是 `Result` 不是裸 `QrState`** —— 803 但拿不到会话 cookie 时必须是 `Err`，
    否则调用方只 `match` 变体就会写成空串 cookie（详见 Step 3 的说明）
    ⚠️ 是 `&[String]` 不是 `&str` —— `login_qr_check` 给的是 `ApiResponse.cookie`
    那个 `Vec<String>`。在调用点先 `join` 会把「清洗 Set-Cookie 属性」的责任
    从 `session_cookie` 漏到调用方。
  - `NeteaseClient::new(cookie: Option<String>) -> Self`、`with_cookie(String) -> Self`
  - `NeteaseClient::qr_key(&self) -> Result<String, SourceError>`
  - `NeteaseClient::qr_image(&self, key: &str) -> Result<Vec<u8>, SourceError>`（PNG 字节）
  - `NeteaseClient::qr_check(&self, key: &str) -> Result<QrState, SourceError>`
  - `pub fn extract_music_u(cookie: &str) -> Option<String>` ★ 纯函数，可单测

> **注意**：这里刻意**不用** `anyhow::Result`。**音源层（`provider-*`）对外一律是
> typed `SourceError`**（设计文档 §4.5），因为音源抽象是第三方实现要绑上去的边界，
> 错误类型必须是可穷尽匹配的。
>
> ⚠️ **不要说成「`anyhow` 只允许出现在 `app` 的边界」——那句话是假的。**
> `crates/platform` 一直在用 `anyhow`，而且**是有意的**：平台层面向 app 内部，
> 不是别人要实现的抽象。**一句为假的约束会让后来的代理去「修」对的东西。**
>
> Task 9 按本块的签名调用，Task 7 在此之上追加方法。

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

    // ⚠️ `from_code` 返回 `Result`，所以下面每条都要 `Ok(..)` 包一层 ——
    //    不包就是 `Result<QrState, _>` 与 `QrState` 比，E0308。

    #[test]
    fn maps_code_801_to_waiting_scan() {
        assert_eq!(QrState::from_code(801, &[]), Ok(QrState::WaitingScan));
    }

    #[test]
    fn maps_code_802_to_waiting_confirm() {
        assert_eq!(QrState::from_code(802, &[]), Ok(QrState::WaitingConfirm));
    }

    #[test]
    fn maps_code_800_to_expired() {
        assert_eq!(QrState::from_code(800, &[]), Ok(QrState::Expired));
    }

    #[test]
    fn maps_code_803_to_confirmed_with_cookie() {
        assert_eq!(
            QrState::from_code(803, &["MUSIC_U=tok; __csrf=y".to_string()]),
            Ok(QrState::Confirmed { cookie: "MUSIC_U=tok; __csrf=y".to_string() })
        );
    }

    #[test]
    fn confirmed_without_session_cookie_is_a_protocol_error() {
        // ★ 803 承诺带 cookie。没带到**绝不能**当成登录成功 ——
        // 否则调用方会写一个空串会话，症状与「登录成功但仍是游客」一模一样。
        let err = QrState::from_code(803, &[]).unwrap_err();
        assert!(matches!(err, dsls_provider_api::SourceError::Protocol(_)));
        assert!(err.is_upstream_changed());
    }

    #[test]
    fn unknown_code_is_a_protocol_error() {
        // 400 / 999 都不是「二维码过期」。折成 Expired 会误导用户，
        // 而且 is_upstream_changed() 在这条路径上永不触发。
        for code in [400, 999, 200] {
            let err = QrState::from_code(code, &[]).unwrap_err();
            assert!(
                matches!(err, dsls_provider_api::SourceError::Protocol(_)),
                "状态码 {code} 应映射为 Protocol，实际 {err:?}"
            );
        }
    }

    #[test]
    fn session_cookie_keeps_key_value_form() {
        // ★ 回归测试。曾经的实现是「只留 MUSIC_U 的值」→ "tok"，
        // 那个字符串喂给 create_client 后会被 cookie_to_json 整个丢掉，
        // 表现为「登录成功但仍是游客」。**必须保留 k=v 形式。**
        let s = session_cookie(&["MUSIC_U=tok; __csrf=y".to_string()]).unwrap();
        assert_eq!(s, "MUSIC_U=tok; __csrf=y");
        assert!(s.contains('='), "不能退化成裸 token");
    }

    #[test]
    fn session_cookie_drops_set_cookie_attributes() {
        // 真实的 Set-Cookie 长这样（crate 只删了 Domain=）。
        // Path/Expires/HttpOnly 混进会话串后会变成垃圾 cookie 再注入请求头。
        let raw = vec![
            "MUSIC_U=tok; Path=/; Expires=Wed, 01 Jan 2027 00:00:00 GMT; HttpOnly".to_string(),
            "__csrf=xyz; Path=/".to_string(),
        ];
        let s = session_cookie(&raw).unwrap();
        assert_eq!(s, "MUSIC_U=tok; __csrf=xyz");
        assert!(!s.contains("Path"), "属性不该进会话串: {s}");
        assert!(!s.contains("HttpOnly"), "属性不该进会话串: {s}");
    }

    #[test]
    fn session_cookie_is_none_without_music_u() {
        assert_eq!(session_cookie(&["__csrf=y".to_string()]), None);
        assert_eq!(session_cookie(&[]), None);
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
license.workspace = true
rust-version.workspace = true

[dependencies]
dsls-provider-api = { path = "../provider-api" }
# 已核实：全 crate 只有 0.1.0 一个发布版本（2026-04-07，作者 imsyy/SPlayer）。
# 保持 "0.1"（= ^0.1 = >=0.1.0,<0.2.0）而不是 =0.1.0：
#   (a) semver 下 0.2 本来就不会被自动选中，破坏性升级不会悄悄进来；
#   (b) =0.1.0 会把后续 0.1.x 的修 bug 版本也一起挡在门外；
#   (c) 真正的防线是提交进仓库的 Cargo.lock。
# 单版本 crate 的长期风险是真的 —— 缓解手段是把它整个包在我们自己的
# NeteaseClient 后面（本任务就是这么做的），换实现时改动能收敛在这里。
ncm-api-rs = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
async-trait = "0.1"
log.workspace = true
serde_json = "1"
# 版本由 cargo add 解析后钉死（Task 6 实测：qrcode 0.14.1 声明 image = "0.25"，
# 与下面的 0.25.9 同一大版本 → 全树只有一个 image，Luma<u8> 不会冲突）
qrcode = { version = "0.14.1", features = ["image"] }
# ⚠️ **必须关掉默认 feature。** `image` 的 default-formats 含 `avif`，
# 走默认会链进 `ravif`（纯 Rust AV1 编码器）。实测依赖树 **506 个 crate**，
# 而我们只用 `ImageFormat::Png` 这一个编码器。
image = { version = "0.25.9", default-features = false, features = ["png"] }
```

> **⚠️ 这里刻意没有 `anyhow`。** 本 crate 对外一律 typed `SourceError`
> （见上面 Interfaces 块末尾的说明）——把 `anyhow` 放进音源层，
> 与「**音源层对外一律 typed `SourceError`**」这条约束**方向相反**。
> M1 里 `dsls-provider-netease` 一次都用不到它。
>
> ⚠️ **不要把这句说成「`anyhow` 只允许出现在 `app` 的边界」——那是假的。**
> `crates/platform` 一直在用它且是有意的（平台层面向 app 内部，不是别人要实现的
> 抽象）。**一句为假的约束会让后来的代理去「修」对的东西。**
>
> `serde_json` 目前 0 引用，但 Task 7 的 `parse.rs` 要用（`Value` + `json!`），**保留**。
> `tokio` / `async-trait` / `log` 同理，都是 Task 7/8 的伏笔。

> **⚠️ 上面两个 `*` 是刻意的**：`qrcode` 的 `image` feature 与 `image` 主版本
> 必须配套，写死版本号极易撞车。**用 `cargo add` 让 cargo 自己解**：
>
> ```bash
> cargo add qrcode --features image -p dsls-provider-netease
> ```
>
> 然后 `cargo add image -p dsls-provider-netease`。
>
> **注意 `Luma` 的类型一致性**：`qrcode` 的 `image` feature 会依赖它自己
> 那一版的 `image`。若 `cargo add` 给 `image` 选了一个不同的大版本，
> `code.render::<Luma<u8>>()` 会因为 `Luma` 是两个不同的类型而编译失败。
> 届时**把 `image` 钉到 `cargo tree -p dsls-provider-netease -i image` 显示的
> 那个版本**即可。
>
> **把 cargo 实际写入的版本号回报到本任务的报告里**，下一版计划据此钉死。
> 不要把 `*` 提交上去——那是过渡态。

`crates/provider-netease/src/qr_login.rs`：

```rust
use dsls_provider_api::SourceError;

/// 网易云扫码登录返回码。
/// 801 = 等待扫码，802 = 待确认，803 = 成功（带 cookie），800 = 二维码过期。
///
/// ⚠️ **这四个码必须从响应体的 `code` 字段读，绝不能读 `ApiResponse.status`。**
/// 已核实 crate 源码 `src/request.rs:17-19`：
/// ```ignore
/// static SPECIAL_STATUS_CODES: LazyLock<HashSet<i64>> =
///     LazyLock::new(|| [201, 302, 400, 502, 800, 801, 802, 803].into());
/// ```
/// 配合 `:473-484` 的折叠逻辑，**800/801/802/803 全被改写成 `200`**。
/// 所以在 `resp.status` 上这四个状态完全不可区分——读到 200 不等于登录成功。
/// （同样的原因：业务错误码 400 也被折叠成 `Ok(status=200)`。**`Ok` ≠ 成功**，
/// 任何接口都应先看 `body["code"]`。）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrState {
    WaitingScan,
    WaitingConfirm,
    Confirmed { cookie: String },
    Expired,
}

impl QrState {
    /// `raw_cookies` 是 `ApiResponse.cookie`，即响应头 `Set-Cookie` 的**原始条目**
    /// （每条含 `Path=` / `Expires=` / `HttpOnly` 等属性，crate 只删了 `Domain=`）。
    /// **返回 `Result` 而不是裸 `QrState`**，两个理由（复审 Important 1 + Minor 2）：
    ///
    /// 1. **803 但拿不到会话 cookie，必须无法被忽略。** 原来写
    ///    `session_cookie(...).unwrap_or_default()`，把「没有会话」折成一个空串
    ///    cookie；调用方只 `match` 变体就写盘，症状与「登录成功但仍是游客」
    ///    一模一样，而且更隐蔽。返回 `Err` 之后这条路径**不可能**被静默吞掉，
    ///    也才让 `session_cookie` 里那句 `is_empty → None` 真正生效。
    /// 2. **未知状态码不能再折成 `Expired`。** 原来 `_ => Expired` 会把
    ///    400 / 999 / 200 都说成「二维码已过期」，误导用户；且
    ///    `is_upstream_changed()` 在这条路径上**永不触发**，上游变了也没人知道。
    pub fn from_code(code: i64, raw_cookies: &[String]) -> Result<Self, SourceError> {
        match code {
            801 => Ok(QrState::WaitingScan),
            802 => Ok(QrState::WaitingConfirm),
            800 => Ok(QrState::Expired),
            803 => match session_cookie(raw_cookies) {
                Some(cookie) => Ok(QrState::Confirmed { cookie }),
                // 803 承诺带 cookie，没带到 = 上游变了或有别的问题。
                // **绝不能**当成登录成功。
                None => Err(SourceError::Protocol(
                    "803 但 Set-Cookie 里没有可用的 MUSIC_U".into(),
                )),
            },
            other => Err(SourceError::Protocol(format!(
                "login_qr_check 返回未知状态码 {other}"
            ))),
        }
    }
}

/// 从 `key=value; key=value` 串里取出某个 cookie 的值。
///
/// `=` 两侧都 trim —— crate 自己的 `cookie_to_json` 也是两侧都 trim
/// （`src/util/cookie.rs:5-17`），我们与它对齐，否则 `MUSIC_U = tok`
/// 这种带空格的形态会被我们判成「没有 MUSIC_U」而它认。**两套解析不一致
/// 是最难查的一类 bug。**
fn extract_cookie(raw: &str, key: &str) -> Option<String> {
    raw.split(';')
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().to_string())
}

/// 取出 MUSIC_U 的值。**这是给测试与诊断用的，不是要持久化的东西**——
/// 要持久化请用 `session_cookie`。
pub fn extract_music_u(raw: &str) -> Option<String> {
    extract_cookie(raw, "MUSIC_U")
}

/// 从 `Set-Cookie` 原始条目里拼出**要持久化的会话 cookie 串**。
///
/// ⚠️ **这里只留 `MUSIC_U` 的值是错的。** `create_client(Some(s))` 要的是
/// `k=v; k=v` 形式的 cookie 串；塞进去一个裸的 `"abc123"`，crate 的
/// `cookie_to_json`（`src/util/cookie.rs:5-17`，按 `;` 切、按 `=` 分）
/// 会把它整个丢掉——**结果是每个请求一个 cookie 都没带上**，
/// 表现为「登录成功了但还是游客」。
///
/// 同时把 `__csrf` 一起留下（部分接口需要），并**丢掉 `Path=` /
/// `Expires=` / `HttpOnly` 这些属性**——它们不是 cookie 键值对，
/// 混进去会被当成垃圾 cookie 重新注入 Cookie 头。
pub fn session_cookie(raw_cookies: &[String]) -> Option<String> {
    let joined = raw_cookies.join("; ");
    let music_u = extract_cookie(&joined, "MUSIC_U")?;
    if music_u.is_empty() {
        return None;
    }

    let mut out = format!("MUSIC_U={music_u}");
    if let Some(csrf) = extract_cookie(&joined, "__csrf") {
        if !csrf.is_empty() {
            out.push_str(&format!("; __csrf={csrf}"));
        }
    }
    Some(out)
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
        // ⚠️ unikey 的嵌套层级**未从 crate 源码核实**：`login_qr_key` 把网易的
        // 原始响应原样透传（`login_qr_key.rs:15-20`），不做任何整形，所以形状
        // 由网易决定、不在 crate 里。两个路径都试，谁先命中用谁。
        // 落地时打一次 `RUST_LOG=debug` 看一眼真实 body，即可定案并删掉另一条。
        resp.body
            .get("unikey")
            .or_else(|| resp.body.pointer("/data/unikey"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| SourceError::Protocol("login_qr_key 未返回 unikey".into()))
    }

    /// 第二步：拿到扫码 URL，**在本地渲染**成 PNG。
    ///
    /// ⚠️ **crate 不提供二维码图片，这一步必须我们自己做。**
    /// 已核实 `src/api/login_qr_create.rs` 全文 28 行：它**不发任何网络请求**，
    /// 只在本地拼出 `https://music.163.com/login?codekey=<key>`，且
    /// `qrimg` **恒为空字符串**、`qrimg` 参数**根本不被读取**。
    /// crate 的依赖里也没有任何二维码/图片库。
    ///
    /// （Node 参考实现是在客户端用 npm `qrcode` 库现渲染的，见
    /// `api-enhanced/module/login_qr_create.js`。Rust 版把这个能力丢了。）
    ///
    /// 附带一句：该 crate 自带的 `docs/API.md:112,116` 仍写着「返回二维码
    /// base64」——**那是从 Node 文档逐字继承的，与本 crate 的实现矛盾**。
    pub async fn qr_image(&self, key: &str) -> Result<Vec<u8>, SourceError> {
        let resp = self
            .inner
            .login_qr_create(&Query::new().param("key", key))
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;

        let qrurl = resp
            .body
            .pointer("/data/qrurl")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SourceError::Protocol("login_qr_create 未返回 qrurl".into()))?;

        render_qr_png(qrurl)
    }

    /// 第三步：轮询扫码状态。
    pub async fn qr_check(&self, key: &str) -> Result<crate::qr_login::QrState, SourceError> {
        let resp = self
            .inner
            .login_qr_check(&Query::new().param("key", key))
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;

        // ⚠️ 读 body 的 code，**不要读 resp.status** —— 800/801/802/803 在
        // request.rs:17-19 被折叠成 200，在 status 上无法区分（理由见 QrState 的文档）。
        let code = resp
            .body
            .pointer("/code")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| SourceError::Protocol("login_qr_check 未返回 code".into()))?;

        // ⚠️ cookie **不在 body 里** —— 它在 `ApiResponse.cookie`，由响应头
        // `Set-Cookie` 填充（request.rs:448-458，crate 只删了 `Domain=`）。
        // 已核实 `ApiResponse` 是 { status: i64, body: Value, cookie: Vec<String> }。
        // Node 版会把 cookie 塞进 body（login_qr_check.js），**Rust 版没有这层整形**，
        // 所以「读 body.cookie」的写法在这里不适用。
        //
        // 传原始条目进去，由 `session_cookie` 负责拼装与清洗。
        // **不要再包一层 `Ok(..)`** —— `from_code` 自己返回 `Result`，
        // 包了就是 `Result<Result<..>>`，E0308。
        crate::qr_login::QrState::from_code(code, &resp.cookie)
    }
}

/// 把二维码内容渲染成 PNG 字节。**纯函数，可单测。**
fn render_qr_png(content: &str) -> Result<Vec<u8>, SourceError> {
    use image::Luma;
    use qrcode::QrCode;

    let code = QrCode::new(content.as_bytes())
        .map_err(|e| SourceError::Protocol(format!("二维码编码失败: {e}")))?;
    // 放大到 256px 见方，否则终端/手机上都太小扫不出来
    let img = code.render::<Luma<u8>>().min_dimensions(256, 256).build();

    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| SourceError::Protocol(format!("PNG 编码失败: {e}")))?;
    Ok(png)
}
```

`crates/provider-netease/src/client.rs` 末尾追加（**Task 7 会往同一个
`mod tests` 里再加一条，所以这个模块要先建出来**）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_qr_png_produces_a_real_png() {
        // crate 不给图，我们自己渲染 —— 这条测试守的是「渲染真的产出了 PNG」，
        // 而不是「调用了某个 API」。
        let png = render_qr_png("https://music.163.com/login?codekey=abc").unwrap();
        assert!(
            png.starts_with(&[0x89, b'P', b'N', b'G']),
            "缺少 PNG 魔数，前 8 字节: {:?}",
            &png[..png.len().min(8)]
        );
        assert!(png.len() > 100, "PNG 太小，可能是空图: {} 字节", png.len());
    }
}
```

`crates/provider-netease/src/lib.rs`：

```rust
//! 网易云音源实现。**本 crate 不得依赖任何 UI crate。**

pub mod client;
pub mod qr_login;

/// 本 provider 的平台标识。
///
/// `Track::platform` 与 `MusicSource::name()` **必须共用这一个值**——
/// 两者若各写一份字面量，会在跨音源路由时静默分叉（设计文档 §4.5）。
pub const PLATFORM: &str = "netease";

pub use client::NeteaseClient;
pub use qr_login::{QrState, extract_music_u, session_cookie};
```

> **⚠️ `Confirmed` 的会话 cookie 契约**：若上游返回 `803` 但 `Set-Cookie` 里
> 没有 `MUSIC_U`，`from_code` **返回 `Err(Protocol)`，不返回 `Confirmed`**。
>
> 这样「登录成功但拿不到会话」就不可能被忽略——早先的写法是
> `unwrap_or_default()` 折成空串 cookie，调用方只 `match` 变体就会写盘，
> 症状与「登录成功但仍是游客」一模一样，且更隐蔽。
> **Task 9 里因此不需要再判空**（那条守卫已成不可达的死代码，已删）。

- [ ] **Step 4: 运行测试确认通过**

```bash
cargo test -p dsls-provider-netease
```

Expected: 11 passed。

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
- Consumes: Task 6 的 `NeteaseClient`；Task 5 的 `MusicSource` / `SourceResult` / `CapabilitySet` / `Capability`
- Produces:
  - `pub fn parse_search_tracks(body: &serde_json::Value) -> Result<Vec<Track>, SourceError>` ★
  - `pub fn parse_stream_url(body: &serde_json::Value, quality: Quality) -> Result<String, SourceError>` ★
  - **`impl MusicSource for NeteaseClient`** —— 本 crate 对外的**唯一**取数入口
  - `NeteaseClient` 增加 `capabilities: CapabilitySet` 字段

> **⚠️ 本任务刻意不提供固有方法 `search` / `stream_url`。**
>
> 理由：同名方法会**遮蔽** trait 方法（Rust 的固有方法优先级更高），
> 结果是 CLI 静默走固有版本、**trait impl 永不执行**——这比不实现更糟，
> 因为它看起来被验证过了。
>
> **整个 M1 里 `MusicSource` 只有一份定义。** 裁决记录见
> `.superpowers/sdd/progress.md`。

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
        // platform 是路由键，不是装饰：resolver 靠它找回正确的 provider
        assert_eq!(tracks[0].platform, PLATFORM);
    }

    #[test]
    fn empty_result_is_ok_not_error() {
        // **空结果 = `Ok(Some(空集合))`，不是 `Ok(None)`**（设计文档 §4.5 收口后）：
        //   「查到了但没结果」→ resolver **不该**换音源
        //   「本 provider 不处理」→ **应该**换音源
        // 本条钉的是前者：空列表必须走 `Ok(Some(..))` 这条边。
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
            "data": [{
                "url": "https://m8.music.126.net/x.mp3",
                "level": "exhigh",
                "br": 320000,
                "code": 200
            }]
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

    #[test]
    fn rejects_silent_downgrade() {
        // ★ 这是本组最重要的一条。要无损、服务端给了极高 ——
        // **实测这是常态而非异常**：匿名/非会员请求 lossless / hires /
        // jymaster 一律返回 level:"exhigh"，且不报错、不提示。
        // 必须拒绝，否则就是静默的谎言：「无损」标签配 320k 的声音。
        let body = json!({
            "data": [{
                "url": "https://m8.music.126.net/x.mp3",
                "level": "exhigh",
                "br": 320000,
                "code": 200
            }]
        });
        let err = parse_stream_url(&body, Quality::Lossless).unwrap_err();
        assert!(matches!(err, dsls_provider_api::SourceError::Unavailable));
        // 这条断言是关键：它保证降档链真的可执行（Task 5 审查的重点）
        assert!(err.should_retry_lower_quality());
    }

    #[test]
    fn hires_is_not_confused_with_lossless() {
        // br 分不出这两档（实测 999000 同时覆盖二者），
        // 所以才改用 level 字符串 —— 这条测试钉的就是这个区别。
        let lossless = json!({
            "data": [{"url": "https://m8.music.126.net/l.flac", "level": "lossless",
                      "br": 999000, "code": 200}]
        });
        assert!(
            matches!(
                parse_stream_url(&lossless, Quality::HiRes),
                Err(dsls_provider_api::SourceError::Unavailable)
            ),
            "br 同为 999000，但 level 是 lossless —— 要 hires 时必须拒绝"
        );
    }

    #[test]
    fn accepts_when_level_absent() {
        // level 缺失时不拒绝：拒绝一个能播的 URL 让用户直接听不到歌，
        // 代价远大于音质低于预期。这是有意的不对称取舍。
        let body = json!({"data": [{"url": "https://m8.music.126.net/x.mp3", "code": 200}]});
        assert_eq!(
            parse_stream_url(&body, Quality::HiRes).unwrap(),
            "https://m8.music.126.net/x.mp3"
        );
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
use crate::PLATFORM;
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
            Some(Track {
                id: id.to_string(),
                platform: PLATFORM.to_string(),
                title,
                artist,
                album,
                duration_ms,
            })
        })
        .collect())
}

/// 从 `song/url/v1` 响应里取出可播放 URL，**并核对实际档位**。
///
/// 核对侧复用 `client.rs` 的 `level_for`（`pub(crate)`）—— **同一份映射，
/// 不各写一份**。
///
/// `want` 是**精确档位要求**，不是「取所能及的最高档」——这是 Task 5 的
/// `MusicSource::stream_url` 文档定下的契约，也是
/// `should_retry_lower_quality()` 唯一有意义的来源。
pub fn parse_stream_url(body: &Value, want: Quality) -> Result<String, SourceError> {
    let data = body
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| SourceError::Protocol("song/url 响应缺少 data".into()))?;

    let first = data.first().ok_or(SourceError::Unavailable)?;

    let url = match first.get("url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u,
        // url 为 null / 空：有能力但当前拿不到（版权、地区、需要 VIP）
        _ => return Err(SourceError::Unavailable),
    };

    // ⚠️ 档位核对。**直接返回低档 URL 是静默的谎言**——调用方会以为
    // 拿到了无损，实际听到的是 320k。返回 Unavailable 才能让 resolver
    // 走 `Quality::lower()` 的显式降级链，把「降档」这件事告诉用户。
    //
    // **判据是 `level` 字符串，不是 `br`。** 实测 `br = 999000` **同时覆盖
    // `lossless` 和 `hires`**，用它分不出这两档；而 `level` 回显服务端
    // 实际授予的档位，可靠且精确。
    //
    // 这条检查会**频繁**命中：实测匿名（或非会员）请求 lossless/hires/jymaster
    // 一律返回 `level: "exhigh"` —— **静默降级是常态**，不是异常。
    //
    // 不对称取舍（有意为之）：`level` **缺失时不拒绝**。拒绝一个本可播放的
    // URL 会让用户直接听不到歌；接受一个无法核实的 URL 最坏只是音质
    // 低于预期。前者的代价大得多。
    if let Some(got) = first.get("level").and_then(|v| v.as_str()) {
        if got != level_for(want) {
            return Err(SourceError::Unavailable);
        }
    }

    Ok(url.to_string())
}
```

`crates/provider-netease/src/lib.rs` 追加一行（**这一步就是本任务 `Files:` 里承诺的 lib.rs 修改**——
不声明 `pub mod parse;`，上面的测试文件根本不会被编译）：

```rust
pub mod parse;
```

- [ ] **Step 4: 在 client.rs 追加方法**

**先在 client.rs 顶部补 import**（**把 Task 6 那行 `use dsls_provider_api::SourceError;`
整行替换掉**，不要新增第二行——同一 crate 两条 use 虽合法但没必要）：

```rust
use dsls_provider_api::{
    Capability, CapabilitySet, MusicSource, Quality, SourceError, SourceResult, Track,
};
```

**给 `NeteaseClient` 加 `capabilities` 字段**（`MusicSource::capabilities()`
返回 `&CapabilitySet`，所以它必须存在结构体里）：

```rust
pub struct NeteaseClient {
    inner: ApiClient,
    cookie: Option<String>,
    capabilities: CapabilitySet,
}
```

`new()` 里构造它。**只申报 M1 真正实现了的能力** —— `CapabilitySet` 是
resolver 用来决定路由的依据（设计文档 §4.5），申报没实现的能力等于撒谎：

```rust
// 只列已实现的。歌词 / 封面 / 歌单 / 专辑 的 provider 方法随各自的
// M2+ 任务落地时再加进来 —— 现在申报它们，resolver 会把请求路由过来，
// 然后撞上 Unimplemented。
capabilities: CapabilitySet::new()
    .with(Capability::Search)
    .with(Capability::StreamUrl),
```

**trait impl**（注意 `#[async_trait::async_trait]` —— Task 5 的 trait 定义
就是这个宏，impl 侧必须匹配，否则签名对不上）：

```rust
#[async_trait::async_trait]
impl MusicSource for NeteaseClient {
    fn name(&self) -> &str {
        crate::PLATFORM
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// **三态契约**（设计文档 §4.5）：
    /// `Ok(Some(vec![]))` = 查到了但没结果；`Err(Unsupported)` = 本音源不处理搜索。
    /// 两者绝不可混淆 —— 前者不该触发换音源，后者应该。
    async fn search(&self, keywords: &str, limit: u32) -> SourceResult<Vec<Track>> {
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
        crate::parse::parse_search_tracks(&resp.body).map(Some)
    }

    /// `Err(Unavailable)` 是**有意的**：版权 / 地区限制是「有能力但拿不到」，
    /// 经 `should_retry_lower_quality()` 触发降档重试，而不是 Protocol。
    async fn stream_url(&self, track_id: &str, quality: Quality) -> SourceResult<String> {
        // 已核实方法名存在：ncm-api-rs src/api/song_url_v1.rs
        //   pub async fn song_url_v1(&self, query: &Query) -> Result<ApiResponse>
        let resp = self
            .inner
            .song_url_v1(
                &Query::new()
                    .param("id", track_id)
                    .param("level", level_for(quality)?),
            )
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;
        crate::parse::parse_stream_url(&resp.body, quality).map(Some)
    }
}

/// 档位 → 网易云 `level` 参数。
///
/// ⚠️ **`#[non_exhaustive]` 让「少一个变体就是编译错误」在下游做不到。**
///
/// `Quality` 标了 `#[non_exhaustive]`，而本 crate 是**下游** crate ——
/// 编译器**强制要求**这里写一个通配臂（不写就是 E0004）。也就是说
/// **穷尽性检查在这里根本不存在**，通配臂是编译器逼出来的，不是我们偷懒。
/// （Task 5b 那条阶梯测试能成立，只因它跑在**定义 crate 内部**。）
///
/// ⇒ 所以把「加了档位忘了接线」这件事**从编译期挪到运行期，并让它响亮**：
/// 返回 `Result`，通配臂给 `Err(Protocol)`。
///
/// ⚠️ **通配臂故意不返回 `Unavailable`** —— 那个变体的语义是「有版权但当前
/// 拿不到」，会触发正常的降档重试链。若用它，「加了档位忘了接线」就会
/// **伪装成一次正常的版权降档**，维护信号彻底丢失。
/// `Protocol` 会让 `is_upstream_changed()` 为真，是唯一的正确归类。
///
/// `pub(crate)`：`parse.rs` 的核对侧要用**同一份**映射。各写一份会静默分叉，
/// 而那种分叉的表现是「请求要无损、核对说不是无损」，极难查。
pub(crate) fn level_for(q: Quality) -> Result<&'static str, SourceError> {
    match q {
        Quality::Low => Ok("standard"),
        Quality::Standard => Ok("higher"),
        Quality::High => Ok("exhigh"),
        Quality::Lossless => Ok("lossless"),
        Quality::HiRes => Ok("hires"),
        // `Quality` 是 #[non_exhaustive] 且定义在别的 crate，
        // 这个臂**必须**存在。走到这里 = provider-api 加了新档位而本 crate 没跟上。
        _ => Err(SourceError::Protocol(format!(
            "provider-netease 未接线 Quality 变体 {q:?}，请更新 level_for"
        ))),
    }
}
```

> **已核实**：`song_url` / `song_url_v1` / `song_url_v1_302` / `song_url_ncmget` / `song_url_match` 五个方法在 `ncm-api-rs` 0.1.0 中都存在。用 `song_url_v1`（对应 Node 版 `/song/url/v1`，支持 `level` 参数）。

**在 `client.rs` 已有的 `mod tests` 里追加一条测试** —— 它钉住的是
**编译期契约**，不是运行时行为：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 编译期契约：`NeteaseClient` 必须能当 `dyn MusicSource` 用。
    ///
    /// 若有人删掉 `impl MusicSource`、或把 `name()` 改成别的字面量，
    /// **这个测试编译不过 / 断言失败** —— 那正是它存在的意义。
    /// 它守的是那条「`Track::platform` 与 `MusicSource::name()` 必须同源」
    /// 的约束（见 Task 6 的 `PLATFORM` 常量）。
    #[test]
    fn netease_is_a_music_source_with_matching_platform_id() {
        let client = NeteaseClient::new(None);
        let src: &dyn MusicSource = &client;

        assert_eq!(src.name(), crate::PLATFORM);

        // 只申报已实现的 —— 多申报会让 resolver 路由到未实现的方法
        assert!(src.capabilities().supports(Capability::Search));
        assert!(src.capabilities().supports(Capability::StreamUrl));
    }
}

// Task 6 已经在同一个文件里建好了 `mod tests`（里面有 render_qr_png 那条），
// 这里是在**已有的模块里追加**一条，不要再写第二个 `mod tests`。
```

- [ ] **Step 5: 运行测试确认通过**

```bash
cargo test -p dsls-provider-netease
```

Expected: 21 passed。

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
license.workspace = true
rust-version.workspace = true

[dependencies]
# 已核实：cpal 最新稳定版是 0.18.2
cpal = "0.18"
# ⚠️ 刻意钉在 0.5 —— 0.6 已发布（0.6.1）但 API 与本计划中的代码未经核对。
# 升级是 dsls-audio 内部的封闭改动，v1 不冒这个险。
# ⚠️ `pcm` 必须显式列出：`wav` 只是**容器**，PCM 解码器是**独立的 feature**。
# 已核实：现在能解码 WAV 纯属默认 feature 没被关掉——一旦有人加
# `default-features = false`，WAV 会**静默失效**（能打开文件、解不出样本）。
symphonia = { version = "0.5", features = ["mp3", "aac", "flac", "isomp4", "ogg", "vorbis", "wav", "pcm"] }
# ⚠️ **刻意不声明 `reqwest` 与 `tokio`。** 它们在本 crate 里**零使用**：
#   - 下载是 `app`（Task 9）的事 —— 它自己声明 reqwest，然后把 `Cursor` 交进来
#   - 本 crate 是**同步**的（解码循环 + cpal 回调）；Task 9 的 `tokio::time::sleep`
#     在 `app` 里，不在 `dsls-audio` 里
# `reqwest` 尤其重（拖着 rustls + hyper + 一整棵 TLS 树）。
# **这正是设计文档 §18.10「依赖裁剪」要的那件事。**
# 无锁 SPSC 环形缓冲。**音频回调线程里绝不允许加锁**（设计文档 §16.2 / 本计划 Global Constraints）
# 已核实："0.4" 解析到 0.4.8。**不要写死 =0.4.9——那个版本已被 yank。**
# 最新是 0.5.2；本计划按 0.4 的 API 写（traits::{Producer,Consumer,Observer,Split}
# 在 0.4 里已核实存在）。升 0.5 是 dsls-audio 内部的封闭改动，v1 不冒这个险。
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

    /// **不补静音、不计数**的原始取值。永不阻塞、永不分配。
    ///
    /// 音频回调用它，而不用 `pop_samples`——因为**静音必须补在目标采样格式
    /// `T` 的域里**：`f32` 的静音是 `0.0`、`i16` 是 `0`、**`u8` 是 `128`**。
    /// 在 i16 域里补 0 再转换是对的，但直接往 `T` 上写 `0` 在 u8 设备上
    /// 是满幅直流噪声。回调侧的补静音见 `fill_from_ring` 的 `T::EQUILIBRIUM`。
    pub fn pop_raw(&mut self, out: &mut [i16]) -> usize {
        self.inner.pop_slice(out)
    }

    /// 记一次欠载。**按「事件」计，不按缺失样本数计**——与 `pop_samples`
    /// 的口径一致，这样「xrun 必须为 0」这条硬指标才有可比性。
    pub fn count_underruns(&self, n: u64) {
        self.underruns.fetch_add(n, Ordering::Relaxed);
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

/// 解码为交错的 i16。

/// ⚠️ **契约：输出恰好 2 声道交错。** 这是**修正后**的说法 ——
/// 早先只写「立体声」而**代码不归一化**，于是 mono 源会被下游
/// 当成 L/R 两样本 ⇒ **2 倍速 + 升调**（6ch 源 3 倍速），
/// **而 xrun 恒 0、不报错**。这类「听起来不对但指标全正常」最难查。
///
/// 归一则：
/// - **1 声道 → 复制到 L/R**（无损、平凡正确）
/// - **2 声道 → 直通**
/// - **> 2 声道 → 取前两个声道**（⚠️ **朴素降混，不是 BS.775** ——
///   那个系数表我们拿不到，**不许编系数**）。发生时要**记一条日志**，
///   让「我听到了什么」这件事可追。（重采样留给后续 DSP 阶段。）
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
    // `warned_remix` 在**循环外**声明 —— 保证「源声道数不是 2」这件事
    // **整轨只告警一次**，而不是每个 packet 刷一条（那会把线索变成噪音）。
    let mut warned_remix = false;
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
        push_interleaved_i16(&decoded, &mut scratch, &mut warned_remix);
        on_samples(&mut scratch);
    }
    Ok(sample_rate)
}

fn push_interleaved_i16(buf: &AudioBufferRef<'_>, out: &mut Vec<i16>, warned_remix: &mut bool) {
    macro_rules! conv {
        ($b:expr) => {{
            let src_channels = $b.spec().channels.count();
            let frames = $b.frames();
            // ⚠️ **0 声道不需要特判分支**（加了也测不到，是死代码）：
            //    带容量的 0 声道 `AudioBuffer` 构造不出来 —— `AudioBuffer::new`
            //    会在 `duration / channels.count()` 上除零 panic。所以 0 声道
            //    只可能是 `AudioBuffer::unused()`：`frames()` 必为 0、
            //    `chan(0)` 返回空切片（已核实·源码 symphonia-core-0.5.5/
            //    src/audio.rs:586-593）⇒ 下面的循环一次都不跑。
            // ⚠️ **1 声道：右声道读同一个平面** ⇒ 复制到 L/R。
            //    **>2 声道：只看前两个平面**（朴素，**不是 BS.775 降混** ——
            //    我们本轮不引入系数；**后果：5.1 源的中置声道整条被丢**，
            //    对白/人声会听不见。已记为 v2 条目，见 §18.x）。
            let left = $b.chan(0);
            let right = $b.chan(if src_channels > 1 { 1 } else { 0 });
            // 只有「复制」（1 声道）与「丢声道」（>2 声道）才告警。
            // 2 声道直通、0 声道没有样本可摊 —— 给它们报「已降混」是假话。
            if src_channels != 2 && src_channels != 0 && !*warned_remix {
                *warned_remix = true;
                if src_channels == 1 {
                    log::warn!("源音频是 1 声道：已复制到 L/R（下游按 2 声道交错消费）");
                } else {
                    log::warn!(
                        "源音频是 {src_channels} 声道：只取前两个声道（朴素做法，不是 BS.775 降混）"
                    );
                }
            }
            // 输出恒为 **2** 声道 ⇒ reserve 按 2 而非 `src_channels`
            out.reserve(frames * 2);
            for f in 0..frames {
                let (l, r): (f32, f32) = (left[f].into_sample(), right[f].into_sample());
                // ⚠️ **乘 32768.0，不是 `i16::MAX`(32767)。**
                // 用 32767 会让整条路径**衰减 1 LSB**（1000→999、32767→32766、
                // −32768→−32767）——实测确认过。乘 32768.0 后 **i16 源逐位精确**，
                // 且**不需要**额外的正端饱和：`(1.0 * 32768.0) as i16 == 32767`
                // （Rust 的 `as` 浮点→整数转换是**饱和**的，symphonia 同款）。
                out.push((l.clamp(-1.0, 1.0) * 32768.0) as i16);
                out.push((r.clamp(-1.0, 1.0) * 32768.0) as i16);
            }
        }};
    }
    // ⚠️ `AudioBufferRef` 一共 **10 个变体** —— 数一遍，别凭印象。
    // 早先这版只有 9 个（漏了 `S8`），是 **E0004 非穷尽 match**。
    match buf {
        AudioBufferRef::U8(b) => conv!(b),
        AudioBufferRef::U16(b) => conv!(b),
        AudioBufferRef::U24(b) => conv!(b),
        AudioBufferRef::U32(b) => conv!(b),
        AudioBufferRef::S8(b) => conv!(b),
        AudioBufferRef::S16(b) => conv!(b),
        AudioBufferRef::S24(b) => conv!(b),
        AudioBufferRef::S32(b) => conv!(b),
        AudioBufferRef::F32(b) => conv!(b),
        AudioBufferRef::F64(b) => conv!(b),
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
        let supported = device.default_output_config()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        // ⚠️ cpal 0.18 已删除 `DeviceTrait::name()`（已核实·编译：
        //    error[E0599]: no method named `name` found for struct `Device`），
        //    换成 `description()?` 再取 `.name()`。
        // ⚠️ `SampleRate` 在 0.18 是**裸 `u32` 别名**，不是 newtype
        //    （已核实·源码 cpal-0.18.2/src/lib.rs:220-233），
        //    所以 `config.sample_rate().0` 编译不过，要写 `config.sample_rate`。
        log::info!(
            "音频输出: {} @ {}Hz {}ch fmt={:?}",
            device.description()?.name(),
            config.sample_rate,
            config.channels,
            sample_format
        );

        // ~4 秒缓冲（立体声 44.1kHz）。**split 之后消费端移进回调，全程无锁。**
        let (producer, consumer) = AudioRing::new(44100 * 2 * 4).split();

        // ⚠️ **不要硬写 `f32`。** `build_output_stream<T>` 是泛型，`T` 由调用方选；
        //    而设备默认格式是运行时才知道的（本机实测 F32/48000，但换台设备可能是
        //    别的）。硬写 f32 等于假设「所有设备的默认格式都是 F32」——
        //    在不是的设备上直接建流失败，**而这个任务的出口标准就是「出声」**。
        let stream = build_stream_for(&device, config, sample_format, consumer)?;
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

/// 按**设备自己报的**采样格式分发。一份回调体覆盖全部受支持格式。
///
/// ⚠️ **不要照抄 cpal 官方文档的示例**（`cpal-0.18.2/src/lib.rs:104-124`）：
/// 它只分支 `F32/I16/U16`、其余 `panic!`——而**本机实测 `u16` 会被拒**
/// （`Sample format u16 is not supported in any byte order`），
/// 照抄会在本机直接 panic。这里一律返回 `Err`，绝不 panic。
fn build_stream_for(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    consumer: AudioRingConsumer,
) -> anyhow::Result<cpal::Stream> {
    use cpal::SampleFormat as F;
    match format {
        F::F32 => build_output::<f32>(device, config, consumer),
        F::I16 => build_output::<i16>(device, config, consumer),
        F::I32 => build_output::<i32>(device, config, consumer),
        // I24 本机设备确实报告支持，**漏了它会掉进 other 分支**
        F::I24 => build_output::<cpal::I24>(device, config, consumer),
        F::U8 => build_output::<u8>(device, config, consumer),
        other => anyhow::bail!(
            "不支持的输出采样格式 {other:?}。请把这个值报上来，不要静默降级到无声。"
        ),
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut consumer: AudioRingConsumer,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::Sample + cpal::FromSample<i16> + Send + 'static,
{
    // 回调内**绝不分配**：scratch 在这里一次性分配好，再 move 进闭包。
    // （旧写法在回调里 `scratch.resize()`——虽然容量预留着不会真重分配，
    //   但那是把「不分配」这条硬约束押在容量估算上，换个大缓冲就破了。）
    let mut scratch: Vec<i16> = vec![0; 8192];
    // 通道数要在 move 进闭包**之前**取出来（`config` 下面会被按值消费掉）
    let channels = config.channels as usize;

    // ⚠️ 0.18 的 `config` **按值传**（旧版是引用），且多了第 4 个参数
    //    `timeout: Option<Duration>`；错误类型没有 `BuildStreamError` 了，
    //    统一是 `cpal::Error`。已核实·源码 cpal-0.18.2/src/traits.rs:318-329。
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            fill_from_ring::<T>(out, channels, &mut consumer, &mut scratch);
        },
        |err| log::error!("音频流错误: {err}"),
        None, // timeout：None = 无限等待后端初始化
    )?;
    Ok(stream)
}

/// **唯一的回调体。** 一份代码覆盖 f32 / i16 / i32 / I24 / u8。
///
/// 用**分块 `pop_slice`** 而不是逐样本 `try_pop`——实测快约 50 倍
/// （0.4 ns/样本 vs 20.0 ns/样本，各 800 万样本）。本函数全程无锁、
/// 无分配、无 IO（设计文档 §16.2）。
fn fill_from_ring<T>(
    out: &mut [T],
    channels: usize,
    consumer: &mut AudioRingConsumer,
    scratch: &mut [i16],
) where
    T: cpal::SizedSample + cpal::Sample + cpal::FromSample<i16>,
{
    let total_frames = out.len() / channels;
    let cap_frames = (scratch.len() / 2).max(1); // scratch 存的是立体声样本
    let mut frame = 0usize;
    let mut starved = false;

    // ⚠️ **尾部槽位必须先写。** 若 `out.len()` 不是 `channels` 的整数倍，
    // 最后那几个槽位**永远不会被下面的循环写到**，会留下 cpal 缓冲里的旧值
    // （听感＝爆音）。先整体铺静音，再让循环覆盖，是唯一稳妥的顺序。
    for slot in out.iter_mut() {
        *slot = T::EQUILIBRIUM;
    }

    while frame < total_frames {
        let want_frames = (total_frames - frame).min(cap_frames);
        let want = want_frames * 2;
        // ⚠️ **只取整帧。** 环里可能装着**奇数**个样本：push 端按任意边界切分
        //    同一个立体声流是合法的（先推 `[L0,R0,L1]`，再推 `[R1,L2,R2]`）。
        //    按 `available()` 把 pop 长度**向下取到偶数**，那**半个样本就留在环里**，
        //    下一次回调自然接在它后面。
        //
        //    ⚠️ **反过来是错的，而且不可恢复**：直接 pop 奇数个再 `got / 2`
        //    会把那半个样本**丢掉** —— 而它后面的样本还在环里
        //    ⇒ 此后**所有 L/R 配对整体错位**，听感是**声道互换**，
        //    且永远回不来。**比丢一个样本难查得多。**
        let avail = consumer.available();
        // `want` 本身是偶数，只有 `avail` 更小时才可能算出半个帧 ⇒ 清掉最低位。
        let n = want.min(avail) & !1;
        let got = consumer.pop_raw(&mut scratch[..n]);
        let got_frames = got / 2;

        for f in 0..want_frames {
            let (l, r) = if f < got_frames {
                (scratch[f * 2], scratch[f * 2 + 1])
            } else {
                (0, 0) // i16 域的静音；转换到 T 后自然就是该格式的静音
            };
            let base = (frame + f) * channels;
            for ch in 0..channels {
                // 上混：0→左、1→右，其余声道静音
                let v = if ch == 0 {
                    l
                } else if ch == 1 {
                    r
                } else {
                    0
                };
                out[base + ch] = T::from_sample(v);
            }
        }

        if got < want_frames * 2 {
            starved = true;
        }
        frame += got_frames;
        if starved {
            break;
        }
    }

    if starved {
        consumer.count_underruns(1);
        // ⚠️ 静音值随 T 而变：f32→0.0、i16→0、**u8→128**。
        //    手写 `*s = 0` 在 u8 设备上会产生满幅直流噪声。必须用 EQUILIBRIUM。
        for slot in &mut out[frame * channels..] {
            *slot = T::EQUILIBRIUM;
        }
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

Expected: 7 passed。

- [ ] **Step 6: 提交**

```bash
git add crates/audio
git commit -m "feat(audio): 无锁 AudioRing + symphonia 解码 + cpal 输出"
```

---

## Task 10: 采样率对齐 —— 按源率建流，设备不支持才重采样

> **为什么有这个任务**：Task 9 的实现者量出来一个缺陷 —— **`dsls-audio` 不重采样**：
> `decode_to_i16` 按**源**采样率产出，而 `AudioEngine` 按**设备**采样率建流。
> ⇒ **44.1kHz 的歌在 48kHz 设备上放 1.0884 倍速、+1.46 半音**，
> **而 xrun 恒 0、无任何告警**。实测：42.893 秒的歌 **39.4 秒放完**。
>
> **用户裁决：「不是最省而是最好」+「连 ③ 一起」。** 所以本任务按音质最优做，
> **不是**按最省事做。

**Files:**
- Modify: `crates/audio/Cargo.toml`（只加 `rubato`）
- Modify: `crates/audio/src/lib.rs`（`AudioEngine` 构造 + 重采样）
- Modify: `crates/audio/src/decode.rs`（加 `probe_sample_rate`）
- Modify: `app/src/main.rs`（先 probe 再建引擎）

**Interfaces:**
- Consumes: Task 8 的 `AudioEngine` / `decode_to_i16`
- Produces:
  - `AudioEngine::new(desired_rate: u32) -> anyhow::Result<Self>`（**签名变更**）
  - `AudioEngine::is_passthrough(&self) -> bool`、`output_rate(&self) -> u32`
  - `decode::probe_sample_rate(stream) -> anyhow::Result<Option<u32>>`

### 设计（三条，缺一不可）

**① 先试以「源采样率」建流 —— 这一档是音质最优，不只是最省。**

> **任何重采样都是失真，哪怕是高质量算法。** 能不做就不做，
> **在音质上就是最好的**。设备支持源率 ⇒ **零处理、逐位精确**。

**② 设备不支持时才重采样，且必须是高质量的。**

用 **`rubato`**（已核实其 `Cargo.toml` 原文 `license = "MIT OR Apache-2.0"`、
v5.0.0、**1175 万下载**、★356、未归档）—— **纯 Rust、无 C 依赖、许可与 GPLv3 兼容**。
⚠️ **不要用线性插值那种** —— 用户要的是「最好」。

**③ 重采样必须放在「解码与 ring 之间」，绝不放进回调。**

- 放解码侧 ⇒ ring 里就是**设备率**的样本 ⇒ **回调仍然只是 memcpy + 转换**（§16.2）
- 放回调里 ⇒ 实时路径变重，正是 §16.2 禁止的方向

**落点就是 §6.1 留的 PCM graph 挂载点。**

⚠️ **一个实质的 API 后果**：`PcmGraph::process(&mut [i16])` 是**原地、等长**的，
而重采样**改变长度**。所以重采样器**不能**直接做成一个 `PcmStage`
—— 需要一个**变长的 stage 概念**（或让 `AudioEngine` 自己持有重采样器，
绕过 `PcmGraph`）。**实现者需要就这一点做判断并说明理由。**

### 设备能力查询

```rust
// 遍历 device.supported_output_configs()，找有没有 sample_rate == desired_rate 且
// 采样格式受支持的配置。找到 ⇒ 按源率建流（passthrough）；
// 找不到 ⇒ 按 default_output_config() 建流 + 启用重采样。
```

**⇒ 必须把结果暴露出来**（`is_passthrough()`），因为**用户有权知道**
「我现在是逐位精确，还是系统在替我重采样」。CLI 要打印它。

### ⚠️ 验收：**掐表验时长**，不是靠耳朵

Task 9 已经把这条写进验收标准了 —— **保持**。

```
播放一首【44.1kHz】的歌，掐表对比时长
  源 42.893s ⇒ 输出应当 ≈42.9s（不是 39.4s）
```

**为什么不用耳朵**：8.8% 的速度差**在没有参照物时很多人听不出来**。
用时长是**客观、可复现、不依赖听感**的判据。

### 不做的（本任务范围外，且要写进文档）

**独立的设备独占访问**（绕开系统混音器）是 **Task 11** 的事。
⚠️ **但必须写明**：**本任务做完 `is_passthrough() == true` 只代表「我们没重采样」，
不代表「整条链路逐位精确」** —— PipeWire/Pulse 仍可能替我们重采样。
**不写清这一点，会给人一种「已经 bit-perfect」的错觉。**

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
# ⚠️ **不是 0.13。** 计划早先写的 `0.13 + ["stream","rustls-tls"]`
# **编译不过** —— 0.13 把 `rustls-tls` 改名成了 `rustls`（已核实 features2 表）。
# 而且 0.13 的 rustls 走 aws-lc-rs，会新拖进一大坨 C 编译。
#
# 用 **0.12**（与 `ncm-api-rs` 同版本、feature 取并集）⇒ **一个 crate 都不新增**
# —— 这正是 §18.10「依赖裁剪」要的。
# ⚠️ `default-features = false` 是必须的：0.12 的 default 含 `default-tls`
#    = native-tls = openssl-sys，本仓库没有它。
# ⚠️ **不开 `stream`**：这里是**全量下载到内存再解码** —— `decode_to_i16` 要的是
#    `MediaSource`（`Read` + **`Seek`**），流式响应**不可 seek**。
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
clap = { version = "4", features = ["derive"] }
```

- [ ] **Step 2: 写 CLI**

`app/src/main.rs` —— ⚠️ **是「在 Task 3 的 main.rs 之上追加」，不是「替换为」。**

**Task 3 那段渲染路径必须逐字留着** —— M0 的验收标准
（`cargo run -p dslsmusic` 弹出窗口）靠它。
**纯 CLI 会让那条标准直接失效**（clap 发现缺子命令会退出 2）。

子命令因此是**可选**的：`cmd: Option<Cmd>`，`None` 时走渲染路径。
（实现者已按此落地并实跑了三条路径 —— 缺省 / 显式 render / login+play。）

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
    // ⚠️ **不要写裸的 `env_logger::init()`** —— 它在 `RUST_LOG` 未设时默认 **Error**，
    // 会把 Task 8 修复轮特地把默认级别提到 `info` 的那次修复**改回去**
    // （那条修复的存在理由：试听片段诊断的 `warn!` 默认必须可见）。
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
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
                // **不需要判空**：`from_code` 已改成返回 `Result`，
                // 拿不到会话会是 `Err(Protocol)`，在上一行的 `?` 就返回了。
                // 这里再判一次就是不可能到达的死代码。
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
        "lossless" => Quality::Lossless,
        "hires" => Quality::HiRes,
        // ⚠️ **未知值硬报错，不静默降级。** 静默 `_ => High` 会让
        //    `--quality lossles`（少个 s）**悄悄播 320k** —— 用户以为要的是无损。
        other => anyhow::bail!(
            "未知音质 {other:?}；可用: low / standard / high / lossless / hires"
        ),
    };

    // 经**抽象**调用，不碰具体类型 —— M1 存在的理由就是验证这条链路。
    // `Ok(None)` 表示「本音源不处理搜索」，与「没搜到」是两回事（§4.5）。
    let src: &dyn dsls_provider_api::MusicSource = &client;

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

    let mut engine = AudioEngine::new()?;
    // ⚠️ **必须查状态码。** `reqwest` 对 4xx/5xx **不报错** ——
    //    403 的 HTML 错误页会被原样喂给解码器，报出来的是「解码失败」，
    //    而真正的原因是「地址失效」。错误信息会指向完全错误的方向。
    let resp = reqwest::get(&url).await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("下载失败：HTTP {status}（播放地址可能已失效）");
    }
    let bytes = resp.bytes().await?;
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
    // ⚠️ **不能只打一个数，更不能写「必须为 0」。**
    // 歌放完时环被**自然读空**，回调必然记一次欠载 —— 收尾那几次**不是缺陷**
    // （实测收尾 1/8/9/10 次，随轮询与回调的竞态浮动）。
    //
    // 所以打**两个口径**：解码结束时采样的「播放期间」（**这个必须为 0**）
    // + 最终总计。差值就是收尾清空造成的，可解释。
    println!(
        "\n播放结束。xrun：播放期间 = {}（必须为 0）、总计 = {}（差值来自收尾清空）",
        xrun_during_playback,
        engine.xrun_count()
    );
    Ok(())
}
```

`app/Cargo.toml` 再加：

```toml
dirs = "7"
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
RUST_LOG=info cargo run -p dslsmusic -- play "周杰伦 晴天"
```

Expected:
1. 打印搜索结果列表
2. 打印解析到的播放地址
3. **扬声器出声** —— ⚠️ **且「出声对」（见下）**
4. 播放结束打印 `xrun：播放期间 = 0、总计 = N`（**只有「播放期间」那个必须为 0**）

> ⚠️ **「出声」不等于「出声对」—— 这条是 Task 9 实现者实测发现的。**
>
> `dsls-audio` **不重采样**：`decode_to_i16` 按**源**采样率产出样本，
> 而 `AudioEngine` 按**设备**采样率建流。源 44.1kHz + 设备 48kHz ⇒
> **1.0884 倍速、+1.46 半音**，**而 xrun 恒 0、无任何告警**。
> 实测：44100Hz 源的 42.893 秒歌，**39.4 秒放完**。
>
> **⇒ 验收必须包含「速度与音高正确」**（最省事的判据：**掐表对时长**，
> 而不是靠耳朵分辨 8.8% —— 那个很多人在没有参照物时听不出来）。
>
> **这条正是「指标全绿但输出是错的」的实例**：xrun 0、测试全绿、能出声。

**若 `xrun`（播放期间口径）不为 0** —— 说明 buffer 太小或解码线程饿死，
**这是必须修的 bug，不是可接受的误差**。

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
- [ ] workspace 与两个成员都声明了 `rust-version`（MSRV 1.85），`cargo metadata` 可读出
- [ ] `cargo deny check licenses` 通过（GPLv3 兼容性审计；**需要网络**，本机 `cargo metadata --offline` 因缺 android_system_properties 缓存而失败）
- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace -- -D warnings` 无告警
- [ ] `RUST_LOG=info cargo run -p dslsmusic` 弹出窗口、显示三角形、日志显示 `RenderTier: A` 且适配器**不是** llvmpipe
- [ ] 日志出现帧时间统计行
- [ ] `cargo run -p dslsmusic -- login` 能完成扫码并落地 session
- [ ] `cargo run -p dslsmusic -- play "<歌名>"` **能出声**，且**时长与源一致**（掐表验速）
- [ ] 播放期间 `xrun = 0`（收尾那几次不计 —— 那是环自然读空）
- [ ] 搜索无结果时**退出码为 0**

## 已知遗留（明确不在 M0/M1 范围内）

| 遗留项 | 处理时机 |
|---|---|
| ~~`ringbuf` 0.4 的实际 API 签名需在 Task 8 实现时对照 `cargo doc`~~ | **已解决** —— 2026-09-26 从 crates.io 发布包核实过（`overflows`/`underruns` **不存在**、`push_slice`/`pop_slice` 才是真名；`0.4.9` 已被 yank）；Task 8 已按核实结果实现并结案 |
| 网易云 eapi 加密正确性未交叉验证 | M2，对照 `ddy314/netease-music` |
| 未做 Android 真机验证 | M3 之后 |
| 未做 `downlevel_webgl2_defaults()` 的 Tier B/C 路径实测 | M3 |
| 未接 amll-ttml-db 歌词 | M6 |
| UI runtime 完全未开始 | M4 |
