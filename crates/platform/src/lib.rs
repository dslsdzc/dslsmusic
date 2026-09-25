//! SDL3 的唯一接触面。
//!
//! 本 crate 之外的任何地方都不允许出现 `use sdl3::...`（设计文档 §11.3）。
//! **这条由依赖图强制**：只有本 crate 依赖 sdl3，别处写了会直接编译失败。
//!
//! 注意：**wgpu 不是本 crate 独有**——`app` 的 main.rs 会直接用 wgpu 类型
//! 建管线。让渲染收敛到 `ui-render` 是后续目标，不是当前的不变量。

pub mod capability;
