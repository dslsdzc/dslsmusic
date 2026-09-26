use crate::model::Track;

#[derive(Debug, thiserror::Error)]
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

const _: () = {
    // 编译期确认 Track 被用在类型别名里
    #[allow(dead_code)]
    fn _t(_: SourceResult<Track>) {}
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_distinguishable_from_empty() {
        // 经一层函数边界产出，而不是就地写字面量 `Ok(None)`：
        // 后者会让 clippy 的 `unnecessary_literal_unwrap` 判定 `a.unwrap()`
        // 多余（它知道 a 就是 Ok(None)）—— 那会让 `--all-targets` 挂掉，
        // 而断言本身要验的正是「解包后确实是 None」。断言语句因此保持原样。
        fn none_result() -> SourceResult<Track> {
            Ok(None)
        }
        let a: SourceResult<Track> = none_result();
        let b: SourceResult<Track> = Err(SourceError::Unsupported);
        assert!(a.is_ok() && a.unwrap().is_none());
        assert!(matches!(b, Err(SourceError::Unsupported)));
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
