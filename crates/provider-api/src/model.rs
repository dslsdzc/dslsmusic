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
