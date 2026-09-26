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
    pub fn from_code(code: i64, raw_cookies: &[String]) -> Self {
        match code {
            801 => QrState::WaitingScan,
            802 => QrState::WaitingConfirm,
            803 => QrState::Confirmed {
                cookie: session_cookie(raw_cookies).unwrap_or_default(),
            },
            _ => QrState::Expired,
        }
    }
}

/// 从 `key=value; key=value` 串里取出某个 cookie 的值。
fn extract_cookie(raw: &str, key: &str) -> Option<String> {
    raw.split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
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
    fn maps_code_801_to_waiting_scan() {
        assert_eq!(QrState::from_code(801, &[]), QrState::WaitingScan);
    }

    #[test]
    fn maps_code_802_to_waiting_confirm() {
        assert_eq!(QrState::from_code(802, &[]), QrState::WaitingConfirm);
    }

    #[test]
    fn maps_code_800_to_expired() {
        assert_eq!(QrState::from_code(800, &[]), QrState::Expired);
    }

    #[test]
    fn maps_code_803_to_confirmed_with_cookie() {
        let s = QrState::from_code(803, &["MUSIC_U=tok; __csrf=y".to_string()]);
        assert_eq!(
            s,
            QrState::Confirmed {
                cookie: "MUSIC_U=tok; __csrf=y".to_string()
            }
        );
    }

    #[test]
    fn session_cookie_keeps_key_value_form() {
        // ★ 回归测试。曾经的实现是「只留 MUSIC_U 的值」→ "tok"，
        // 那个字符串喂给 create_client 后会被 cookie_to_json 整个丢掉，
        // 表现为「登录成功但仍是游客」。**必须保留 k=v 形式。**
        let s = session_cookie(&["MUSIC_U=tok; __csrf=y".to_string()]).unwrap();
        assert_eq!(s, "MUSIC_U=tok; __csrf=y");
        assert!(s.contains('='), "不能退化成裸 token");
    }

    #[test]
    fn session_cookie_drops_set_cookie_attributes() {
        // 真实的 Set-Cookie 长这样（crate 只删了 Domain=）。
        // Path/Expires/HttpOnly 混进会话串后会变成垃圾 cookie 再注入请求头。
        let raw = vec![
            "MUSIC_U=tok; Path=/; Expires=Wed, 01 Jan 2027 00:00:00 GMT; HttpOnly".to_string(),
            "__csrf=xyz; Path=/".to_string(),
        ];
        let s = session_cookie(&raw).unwrap();
        assert_eq!(s, "MUSIC_U=tok; __csrf=xyz");
        assert!(!s.contains("Path"), "属性不该进会话串: {s}");
        assert!(!s.contains("HttpOnly"), "属性不该进会话串: {s}");
    }

    #[test]
    fn session_cookie_is_none_without_music_u() {
        assert_eq!(session_cookie(&["__csrf=y".to_string()]), None);
        assert_eq!(session_cookie(&[]), None);
    }
}
