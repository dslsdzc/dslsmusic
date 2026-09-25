//! SDL3 与 wgpu 的唯一接触面。
//!
//! 本 crate 之外的任何地方都不允许出现 `use sdl3::...`（设计文档 §11.3）。

pub mod capability;
