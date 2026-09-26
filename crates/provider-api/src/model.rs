/// 音质档位。各平台原始标识在 provider 内部映射（设计文档 §4.5）。
///
/// `#[non_exhaustive]`：resolver 马上要 `match` 它，而写操作/新音源可能
/// 引入新档位。**加变体是破坏性变更，现在标比以后改便宜。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_chains_down_to_none() {
        // 降级链必须能一路走到最低档，且最低档返回 None
        assert_eq!(Quality::Super.lower(), Some(Quality::High));
        assert_eq!(Quality::High.lower(), Some(Quality::Standard));
        assert_eq!(Quality::Standard.lower(), Some(Quality::Low));
        assert_eq!(Quality::Low.lower(), None, "最低档不应能再降");
    }

    #[test]
    fn lower_is_strictly_decreasing() {
        // 单调性：每一档严格低于上一档。用 Ord 验证，而不是硬编码顺序。
        let mut q = Quality::Super;
        let mut steps = 0;
        while let Some(next) = q.lower() {
            assert!(next < q, "lower() 必须严格下降：{q:?} -> {next:?}");
            q = next;
            steps += 1;
            assert!(steps <= 8, "降级链不可能这么长，疑似循环");
        }
        assert_eq!(q, Quality::Low, "链的终点必须是最低档");
    }
}
