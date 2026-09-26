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

/// **关键契约**（设计文档 §4.5，2026-09-26 收口）：
///
/// - **「查到了，但确实没有结果」** = `Ok(Some(空集合))` —— resolver **不该**换音源，
///   这就是答案；
/// - **「本 provider 不处理这个请求」**（= 没这个能力）= `Ok(None)`，与
///   `Err(Unsupported)` **语义等价**（`Option` 带来的两种拼写）—— resolver
///   **应该**换一个音源试。
///
/// ⚠️ 本注释早先写的是「`Ok(None)` 表示请求合法，但确实没有」——**那个定义与
/// `Ok(Some(空集合))` 撞车**，两者会变成同一个意思，于是 resolver 再也分不出
/// 「没搜到」（不该换音源）与「没这个能力」（应该换音源）。以设计文档 §4.5 为准。
pub type SourceResult<T> = Result<Option<T>, SourceError>;

// （此处原有一个 `const _: () = { fn _t(_: SourceResult<Track>) {} };` 块，
//   声称「编译期确认 Track 被用在类型别名里」——**它什么也不检查**：
//   `SourceResult<T>` 是泛型别名，约束不了任何东西。已删除。）

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_distinguishable_from_empty() {
        // ⚠️ 收口后的语义（设计文档 §4.5，Task 7 复审发现的定义冲突）：
        //   「查到了但没结果」= Ok(Some(空集合))   ← resolver **不该**换音源
        //   「本 provider 不处理」= Ok(None) 或 Err(Unsupported) ← **应该**换音源
        //
        // 下面这条断言钉的是「resolver 能看出这两类不同」。
        // 注意 Ok(None) 与 Err(Unsupported) **语义等价**（Option 带来的两种拼写），
        // 不是需要区分的两种情况 —— 早先的注释把它们当成两种，那是错的。
        let empty: SourceResult<Vec<Track>> = Ok(Some(Vec::new()));
        let not_handled: SourceResult<Vec<Track>> = Ok(None);
        let unsupported: SourceResult<Vec<Track>> = Err(SourceError::Unsupported);

        assert_ne!(empty, not_handled, "「没结果」与「不处理」必须可区分");
        assert_ne!(empty, unsupported, "「没结果」与「不处理」必须可区分");
        // 这两种拼写语义相同，值当然不同 —— 断言它们不同是在钉「两种拼写都存在」，
        // 不是在说它们是要区分的两种情况
        assert_ne!(not_handled, unsupported);
        // ⚠️ 计划里这一行原文是 `assert_eq!(empty, Ok(None));` —— **照抄会失败**
        // （实测 `left: Ok(Some([])) / right: Ok(None)`）：`empty` 在收口后已经
        // 改成 `Ok(Some(空集合))`，但这一行的主语还是旧定义里的 `empty`。
        // 它要钉的是「不处理的拼写就是 `Ok(None)`」，所以主语应当是 `not_handled`。
        assert_eq!(not_handled, Ok(None));
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

    #[test]
    fn only_unsupported_justifies_trying_another_provider() {
        // 只有 Unsupported 说明「换一个音源有意义」——其余理由换音源都无济于事
        assert!(SourceError::Unsupported.should_try_next_provider());

        for e in [
            SourceError::NotFound,
            SourceError::AuthRequired,
            SourceError::Unavailable,
            SourceError::RateLimited,
            SourceError::Network("x".into()),
            SourceError::Protocol("x".into()),
        ] {
            assert!(
                !e.should_try_next_provider(),
                "{e:?} 不应被当作「该换音源」"
            );
        }
    }

    #[test]
    fn only_unavailable_and_rate_limited_justify_lowering_quality() {
        assert!(SourceError::Unavailable.should_retry_lower_quality());
        assert!(SourceError::RateLimited.should_retry_lower_quality());

        for e in [
            SourceError::Unsupported,
            SourceError::NotFound,
            SourceError::AuthRequired,
            SourceError::Network("x".into()),
            SourceError::Protocol("x".into()),
        ] {
            assert!(
                !e.should_retry_lower_quality(),
                "{e:?} 不应被当作「降音质能解决」"
            );
        }
    }

    #[test]
    fn only_protocol_counts_as_upstream_change() {
        assert!(SourceError::Protocol("yrc 字段消失".into()).is_upstream_changed());

        for e in [
            SourceError::Unsupported,
            SourceError::NotFound,
            SourceError::AuthRequired,
            SourceError::Unavailable,
            SourceError::RateLimited,
            SourceError::Network("x".into()),
        ] {
            assert!(!e.is_upstream_changed(), "{e:?} 不是协议变化");
        }
    }

    #[test]
    fn display_strings_are_non_empty_and_distinct() {
        // 这些串会出现在日志与用户可见的错误提示里，不能是空的、也不该互相撞车
        let all = [
            SourceError::Unsupported,
            SourceError::NotFound,
            SourceError::AuthRequired,
            SourceError::Unavailable,
            SourceError::RateLimited,
            SourceError::Network("网络细节".into()),
            SourceError::Protocol("协议细节".into()),
        ];
        let mut seen = std::collections::HashSet::new();
        for e in &all {
            let s = e.to_string();
            assert!(!s.trim().is_empty(), "{e:?} 的 Display 是空的");
            seen.insert(s.clone());
        }
        // 七个变体应有七个不同的串（Network/Protocol 带 payload，天然不同）
        assert_eq!(
            seen.len(),
            all.len(),
            "有变体的 Display 串互相撞车：{seen:?}"
        );
    }
}
