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

/// **关键契约**：`Ok(None)` 表示「请求合法，但确实没有」，
/// 与 `Err(Unsupported)`「provider 没这个能力」是两回事。
pub type SourceResult<T> = Result<Option<T>, SourceError>;

// （此处原有一个 `const _: () = { fn _t(_: SourceResult<Track>) {} };` 块，
//   声称「编译期确认 Track 被用在类型别名里」——**它什么也不检查**：
//   `SourceResult<T>` 是泛型别名，约束不了任何东西。已删除。）

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_distinguishable_from_empty() {
        let empty: SourceResult<Track> = Ok(None);
        let unsupported: SourceResult<Track> = Err(SourceError::Unsupported);

        // 两者必须不同 —— 这是 source-resolver 决定「要不要换一个音源试」
        // 与「这首歌确实没有」的依据（设计文档 §4.5）
        assert_ne!(empty, unsupported);
        assert_eq!(empty, Ok(None));
        assert!(matches!(unsupported, Err(SourceError::Unsupported)));
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
