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

    async fn search(&self, keywords: &str, limit: u32) -> SourceResult<Vec<Track>>;

    async fn stream_url(&self, track_id: &str, quality: Quality) -> SourceResult<String>;
}
