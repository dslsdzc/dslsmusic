/// **保真度**档位。有序，可沿 [`Quality::lower`] 逐级降。
/// 各平台原始标识在 provider 内部映射（设计文档 §4.5）。
///
/// ⚠️ **这里只有保真度。** 网易云的 `jyeffect` / `jymaster` / `sky` /
/// `dolby` / `vivid` **不属于本枚举**——它们是不同的混音／声道结构，
/// 不可排序、也不可互相降级（设计文档 §4.5）。见 [`AudioEffect`]。
///
/// `#[non_exhaustive]`：resolver 马上要 `match` 它，而写操作/新音源可能
/// 引入新档位。**加变体是破坏性变更，现在标比以后改便宜。**
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
    ///
    /// **没有它，resolver 的降级链写不出来**——`Quality` 只派生 `Ord`，
    /// 无法枚举档位。而 `SourceError::should_retry_lower_quality()` 返回
    /// true 时，调用方必须知道「更低是哪一档」才能重试。
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
    /// 臻品音质。⚠️ 需要 `os=android` + `appver=9.5.61`，**与 `Dolby` 互斥**
    Vivid,
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
    fn lower_walks_the_five_fidelity_tiers_to_none() {
        assert_eq!(Quality::HiRes.lower(), Some(Quality::Lossless));
        assert_eq!(Quality::Lossless.lower(), Some(Quality::High));
        assert_eq!(Quality::High.lower(), Some(Quality::Standard));
        assert_eq!(Quality::Standard.lower(), Some(Quality::Low));
        assert_eq!(Quality::Low.lower(), None);
    }

    /// 钉住**档位总数**。加档位却忘了接进 `lower()` 的话，这条会失败。
    #[test]
    fn ladder_has_exactly_five_rungs() {
        let mut n = 1;
        let mut q = Quality::HiRes;
        while let Some(next) = q.lower() {
            n += 1;
            q = next;
        }
        assert_eq!(n, 5, "保真度梯度应为 5 档；加档位时必须同时更新 lower()");
    }

    #[test]
    fn lower_is_strictly_decreasing() {
        // 单调性：每一档严格低于上一档。用 Ord 验证，而不是硬编码顺序。
        let mut q = Quality::HiRes;
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
