use dsls_provider_api::SourceError;

/// 网易云扫码登录返回码。
/// 801 = 等待扫码，802 = 待确认，803 = 成功（带 cookie），800 = 二维码过期。
///
/// ⚠️ **这四个码必须从响应体的 `code` 字段读，绝不能读 `ApiResponse.status`。**
/// 已核实 crate 源码 `src/request.rs:17-19`：
/// ```ignore
/// static SPECIAL_STATUS_CODES: LazyLock<HashSet<i64>> =
///     LazyLock::new(|| [201, 302, 400, 502, 800, 801, 802, 803].into());
/// ```
/// 配合 `:473-484` 的折叠逻辑，**800/801/802/803 全被改写成 `200`**。
/// 所以在 `resp.status` 上这四个状态完全不可区分——读到 200 不等于登录成功。
/// （同样的原因：业务错误码 400 也被折叠成 `Ok(status=200)`。**`Ok` ≠ 成功**，
/// 任何接口都应先看 `body["code"]`。）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrState {
    WaitingScan,
    WaitingConfirm,
    Confirmed { cookie: String },
    Expired,
}

impl QrState {
    /// `raw_cookies` 是 `ApiResponse.cookie`，即响应头 `Set-Cookie` 的**原始条目**
    /// （每条含 `Path=` / `Expires=` / `HttpOnly` 等属性，crate 只删了 `Domain=`）。
    /// **返回 `Result` 而不是裸 `QrState`**，两个理由（复审 Important 1 + Minor 2）：
    ///
    /// 1. **803 但拿不到会话 cookie，必须无法被忽略。** 原来写
    ///    `session_cookie(...).unwrap_or_default()`，把「没有会话」折成一个空串
    ///    cookie；调用方只 `match` 变体就写盘，症状与「登录成功但仍是游客」
    ///    一模一样，而且更隐蔽。返回 `Err` 之后这条路径**不可能**被静默吞掉，
    ///    也才让 `session_cookie` 里那句 `is_empty → None` 真正生效。
    /// 2. **未知状态码不能再折成 `Expired`。** 原来 `_ => Expired` 会把
    ///    400 / 999 / 200 都说成「二维码已过期」，误导用户；且
    ///    `is_upstream_changed()` 在这条路径上**永不触发**，上游变了也没人知道。
    pub fn from_code(code: i64, raw_cookies: &[String]) -> Result<Self, SourceError> {
        match code {
            801 => Ok(QrState::WaitingScan),
            802 => Ok(QrState::WaitingConfirm),
            800 => Ok(QrState::Expired),
            803 => match session_cookie(raw_cookies) {
                Some(cookie) => Ok(QrState::Confirmed { cookie }),
                // 803 承诺带 cookie，没带到 = 上游变了或有别的问题。
                // **绝不能**当成登录成功。
                None => Err(SourceError::Protocol(
                    "803 但 Set-Cookie 里没有可用的 MUSIC_U".into(),
                )),
            },
            other => Err(SourceError::Protocol(format!(
                "login_qr_check 返回未知状态码 {other}"
            ))),
        }
    }
}

/// 从 `key=value; key=value` 串里取出某个 cookie 的值。
///
/// `=` 两侧都 trim —— crate 自己的 `cookie_to_json` 也是两侧都 trim
/// （`src/util/cookie.rs:5-17`），我们与它对齐，否则 `MUSIC_U = tok`
/// 这种带空格的形态会被我们判成「没有 MUSIC_U」而它认。**两套解析不一致
/// 是最难查的一类 bug。**
fn extract_cookie(raw: &str, key: &str) -> Option<String> {
    raw.split(';')
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().to_string())
}

/// 取出 MUSIC_U 的值。**这是给测试与诊断用的，不是要持久化的东西**——
/// 要持久化请用 `session_cookie`。
pub fn extract_music_u(raw: &str) -> Option<String> {
    extract_cookie(raw, "MUSIC_U")
}

/// 从 `Set-Cookie` 原始条目里拼出**要持久化的会话 cookie 串**。
///
/// ⚠️ **这里只留 `MUSIC_U` 的值是错的。** `create_client(Some(s))` 要的是
/// `k=v; k=v` 形式的 cookie 串；塞进去一个裸的 `"abc123"`，crate 的
/// `cookie_to_json`（`src/util/cookie.rs:5-17`，按 `;` 切、按 `=` 分）
/// 会把它整个丢掉——**结果是每个请求一个 cookie 都没带上**，
/// 表现为「登录成功了但还是游客」。
///
/// 同时把 `__csrf` 一起留下（部分接口需要），并**丢掉 `Path=` /
/// `Expires=` / `HttpOnly` 这些属性**——它们不是 cookie 键值对，
/// 混进去会被当成垃圾 cookie 重新注入 Cookie 头。
pub fn session_cookie(raw_cookies: &[String]) -> Option<String> {
    let joined = raw_cookies.join("; ");
    let music_u = extract_cookie(&joined, "MUSIC_U")?;
    if music_u.is_empty() {
        return None;
    }

    let mut out = format!("MUSIC_U={music_u}");
    if let Some(csrf) = extract_cookie(&joined, "__csrf") {
        if !csrf.is_empty() {
            out.push_str(&format!("; __csrf={csrf}"));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_music_u_from_cookie_string() {
        let raw = "MUSIC_U=abc123def; __csrf=xyz; NMTID=1";
        assert_eq!(extract_music_u(raw), Some("abc123def".to_string()));
    }

    #[test]
    fn returns_none_when_music_u_absent() {
        assert_eq!(extract_music_u("__csrf=xyz"), None);
    }

    #[test]
    fn handles_music_u_at_end_without_semicolon() {
        assert_eq!(
            extract_music_u("__csrf=x; MUSIC_U=tail"),
            Some("tail".to_string())
        );
    }

    #[test]
    fn extract_cookie_trims_both_sides_of_the_equals_sign() {
        // crate 自己的 `cookie_to_json`（`src/util/cookie.rs:5-17`）`=` 两侧都 trim。
        // 我们不对齐的话，`MUSIC_U = tok` 会被我们判成「没有 MUSIC_U」而它认。
        assert_eq!(
            extract_music_u("__csrf=x; MUSIC_U = tok"),
            Some("tok".to_string())
        );
        assert_eq!(
            extract_music_u("MUSIC_U=  spaced  "),
            Some("spaced".to_string())
        );
        // 端到端后果：不 trim 的话这里返回 None → `from_code(803, …)` 报协议错，
        // 即「上游明明给了会话，我们却说没有」。
        assert_eq!(
            session_cookie(&["MUSIC_U = tok".to_string()]),
            Some("MUSIC_U=tok".to_string())
        );
    }

    #[test]
    fn maps_code_801_to_waiting_scan() {
        assert_eq!(QrState::from_code(801, &[]), Ok(QrState::WaitingScan));
    }

    #[test]
    fn maps_code_802_to_waiting_confirm() {
        assert_eq!(QrState::from_code(802, &[]), Ok(QrState::WaitingConfirm));
    }

    #[test]
    fn maps_code_800_to_expired() {
        assert_eq!(QrState::from_code(800, &[]), Ok(QrState::Expired));
    }

    #[test]
    fn maps_code_803_to_confirmed_with_cookie() {
        let s = QrState::from_code(803, &["MUSIC_U=tok; __csrf=y".to_string()]);
        assert_eq!(
            s,
            Ok(QrState::Confirmed {
                cookie: "MUSIC_U=tok; __csrf=y".to_string()
            })
        );
    }

    #[test]
    fn confirmed_without_session_cookie_is_a_protocol_error() {
        // ★ 回归测试（复审 Important 1 / Important 3）。曾经的实现是
        // `session_cookie(..).unwrap_or_default()`，于是这里得到
        // `Confirmed { cookie: "" }` —— 调用方只 match 变体就往盘上写，
        // **持久化一个空会话**，症状与「登录成功但仍是游客」一模一样、且更隐蔽
        // （空串不像裸 token 那样一眼可疑）。
        for raw in [vec![], vec!["MUSIC_U=".to_string()]] {
            let r = QrState::from_code(803, &raw);
            assert!(
                matches!(r, Err(SourceError::Protocol(_))),
                "803 但没有可用 MUSIC_U（{raw:?}）必须报协议错，实得 {r:?}"
            );
            // 这条才是重点：修好之前 `is_upstream_changed()` 在这条路径上
            // **永不触发**，上游变了也没有任何信号指向它。
            assert!(r.unwrap_err().is_upstream_changed());
        }
    }

    #[test]
    fn unknown_code_is_a_protocol_error() {
        // 复审 Minor 2：原来 `_ => Expired` 把「上游变了」和「二维码过期」
        // 压成同一个状态 —— 调用方看到「过期了」就刷新二维码继续轮询，
        // 永远轮询不到成功，且没有任何信号指向「上游变了」。
        for code in [400, 999] {
            let r = QrState::from_code(code, &[]);
            assert!(
                matches!(r, Err(SourceError::Protocol(_))),
                "未知状态码 {code} 必须报协议错，实得 {r:?}"
            );
            assert!(r.unwrap_err().is_upstream_changed());
        }
    }

    #[test]
    fn session_cookie_keeps_key_value_form() {
        // ★ 回归测试。曾经的实现是「只留 MUSIC_U 的值」→ "tok"，
        // 那个字符串喂给 create_client 后会被 cookie_to_json 整个丢掉，
        // 表现为「登录成功但仍是游客」。**必须保留 k=v 形式** ——
        // 下面这条 assert_eq! 已经把整个串钉死，所以它同时覆盖了
        // 「没有退化成裸 token」。
        let s = session_cookie(&["MUSIC_U=tok; __csrf=y".to_string()]).unwrap();
        assert_eq!(s, "MUSIC_U=tok; __csrf=y");
    }

    #[test]
    fn session_cookie_drops_set_cookie_attributes() {
        // 真实的 Set-Cookie 长这样（crate 只删了 Domain=）。
        // Path/Expires/HttpOnly 混进会话串后会变成垃圾 cookie 再注入请求头。
        // 下面这条 assert_eq! 同时覆盖了「Path / Expires / HttpOnly 都没进来」，
        // 单独写 `!s.contains(..)` 是被它完全蕴含的死断言。
        let raw = vec![
            "MUSIC_U=tok; Path=/; Expires=Wed, 01 Jan 2027 00:00:00 GMT; HttpOnly".to_string(),
            "__csrf=xyz; Path=/".to_string(),
        ];
        let s = session_cookie(&raw).unwrap();
        assert_eq!(s, "MUSIC_U=tok; __csrf=xyz");
    }

    #[test]
    fn session_cookie_is_none_without_music_u() {
        assert_eq!(session_cookie(&["__csrf=y".to_string()]), None);
        assert_eq!(session_cookie(&[]), None);
    }
}
