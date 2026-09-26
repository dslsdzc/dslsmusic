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
