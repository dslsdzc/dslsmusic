//! 音源契约。**本 crate 不得依赖任何 UI crate**（设计文档 §2.3）。

pub mod capability;
pub mod error;
pub mod model;

pub use capability::{Capability, CapabilitySet};
pub use error::{SourceError, SourceResult};
pub use model::{AudioEffect, Quality, Track};

use async_trait::async_trait;

#[async_trait]
pub trait MusicSource: Send + Sync {
    /// 该音源的标识，用于 `Track::platform`。
    fn name(&self) -> &str;

    /// 声明的能力集。**这是提示，不是事实**——运行时权威是 `Err(SourceError::Unsupported)`。
    fn capabilities(&self) -> &CapabilitySet;

    /// 实现**不得阻塞**（本地音源的目录扫描马上会撞上这条）。
    async fn search(&self, keywords: &str, limit: u32) -> SourceResult<Vec<Track>>;

    /// `quality` 是**精确档位要求**，不是「取所能及的最高档」。
    ///
    /// provider 必须**核对响应里的实际档位**；低于 `quality` 时返回
    /// `Err(SourceError::Unavailable)`，而不是降档返回。
    /// 降档由 resolver 用 `Quality::lower()` 显式走降级链——**这样
    /// `should_retry_lower_quality()` 才是可执行的**。
    ///
    /// 返回 `Ok(Some(url))` 表示**可完整播放的 URL**。
    /// **试听片段 / 残缺响应必须映射成 `Err(Unavailable)`**，不能返回
    /// ——否则播放器会播 30 秒然后停住，而调用方以为成功了。
    async fn stream_url(&self, track_id: &str, quality: Quality) -> SourceResult<String>;
}
