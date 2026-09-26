use dsls_provider_api::SourceError;
use ncm_api_rs::{ApiClient, Query, create_client};
use serde_json::Value;

pub struct NeteaseClient {
    inner: ApiClient,
    cookie: Option<String>,
}

impl NeteaseClient {
    pub fn new(cookie: Option<String>) -> Self {
        let inner = create_client(cookie.clone());
        Self { inner, cookie }
    }

    pub fn with_cookie(cookie: String) -> Self {
        Self::new(Some(cookie))
    }

    pub fn inner(&self) -> &ApiClient {
        &self.inner
    }
    pub fn cookie(&self) -> Option<&str> {
        self.cookie.as_deref()
    }

    /// 网易云二维码登录第一步：拿 key。
    pub async fn qr_key(&self) -> Result<String, SourceError> {
        let resp = self
            .inner
            .login_qr_key(&Query::new())
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;
        // ⚠️ `login_qr_key` 把网易的原始响应原样透传（`login_qr_key.rs:15-20`），
        // 不做任何整形，所以形状**由网易决定、不在 crate 里**——crate 源码里
        // 根本没有 `unikey` 这个字段名（只在 `:16` 作为 URL 路径出现过）。
        //
        // **2026-09-26 实测定案（对 music.163.com 真机打过一次）：unikey 在顶层。**
        // 真实响应：`{"code":200,"unikey":"341ffc6d-2051-4f74-b219-a94a8032df15"}`
        // 顶层没有 `data` 包一层，所以下面的 `pointer("/data/unikey")` 在当前
        // 上游版本上**不会命中**。
        //
        // 那条 fallback 仍然保留：网易是该 provider 里唯一不受我们控制的、
        // 无版本号的上游（设计文档 §13.1 整节都在讲这个），多留一条分支
        // 换的是「上游改形状时还能活着」而不是「更正确」。
        //
        // ⚠️ **fallback 的顺序很重要**（复审 Minor 8）：必须先 `and_then(as_str)`
        // 再 `or_else`。旧写法 `.get(..).or_else(|| pointer(..))` 里 `or_else`
        // 挂在 `Option<&Value>` 上——上游若发 `{"unikey": null, "data": {...}}`，
        // `get` 得到的是 `Some(Null)` 而不是 `None`，**`or_else` 根本不会触发**，
        // 最后仍是 `None`。那恰好是 fallback 本该救的场景，等于它白留了。
        pick_unikey(&resp.body)
            .map(|s| s.to_string())
            .ok_or_else(|| SourceError::Protocol("login_qr_key 未返回 unikey".into()))
    }

    /// 第二步：拿到扫码 URL，**在本地渲染**成 PNG。
    ///
    /// ⚠️ **crate 不提供二维码图片，这一步必须我们自己做。**
    /// 已核实 `src/api/login_qr_create.rs` 全文 **29 行**（892 字节、29 个换行、
    /// 以换行结尾）：它**不发任何网络请求**，
    /// 只在本地拼出 `https://music.163.com/login?codekey=<key>`，且
    /// `qrimg` **恒为空字符串**、`qrimg` 参数**根本不被读取**。
    /// crate 的依赖里也没有任何二维码/图片库。
    ///
    /// （Node 参考实现是在客户端用 npm `qrcode` 库现渲染的，见
    /// `api-enhanced/module/login_qr_create.js`。Rust 版把这个能力丢了。）
    ///
    /// 附带一句：该 crate 自带的 `docs/API.md:112,116` 仍写着「返回二维码
    /// base64」——**那是从 Node 文档逐字继承的，与本 crate 的实现矛盾**。
    pub async fn qr_image(&self, key: &str) -> Result<Vec<u8>, SourceError> {
        let resp = self
            .inner
            .login_qr_create(&Query::new().param("key", key))
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;

        let qrurl = resp
            .body
            .pointer("/data/qrurl")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SourceError::Protocol("login_qr_create 未返回 qrurl".into()))?;

        render_qr_png(qrurl)
    }

    /// 第三步：轮询扫码状态。
    pub async fn qr_check(&self, key: &str) -> Result<crate::qr_login::QrState, SourceError> {
        let resp = self
            .inner
            .login_qr_check(&Query::new().param("key", key))
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;

        // ⚠️ 读 body 的 code，**不要读 resp.status** —— 800/801/802/803 在
        // request.rs:17-19 被折叠成 200，在 status 上无法区分（理由见 QrState 的文档）。
        // `json_code` 容忍字符串码，与 crate 自己的折叠逻辑对齐（复审 Minor 3）。
        let code = resp
            .body
            .pointer("/code")
            .and_then(json_code)
            .ok_or_else(|| SourceError::Protocol("login_qr_check 未返回 code".into()))?;

        // ⚠️ cookie **不在 body 里** —— 它在 `ApiResponse.cookie`，由响应头
        // `Set-Cookie` 填充（request.rs:448-458，crate 只删了 `Domain=`）。
        // 已核实 `ApiResponse` 是 { status: i64, body: Value, cookie: Vec<String> }。
        // Node 版会把 cookie 塞进 body（login_qr_check.js），**Rust 版没有这层整形**，
        // 所以「读 body.cookie」的写法在这里不适用。
        //
        // 传原始条目进去，由 `session_cookie` 负责拼装与清洗。
        // `from_code` 自己返回 `Result`（803 拿不到会话 / 未知状态码都是 `Err`），
        // 所以这里**不要**再包一层 `Ok(..)`。
        crate::qr_login::QrState::from_code(code, &resp.cookie)
    }
}

/// 从 `login_qr_key` 的响应体里取 unikey。**纯函数，可单测。**
///
/// 两条路径都试，谁先命中用谁。顺序是 `顶层 → /data`，且**每一步都先
/// `as_str()` 再进 `or_else`**（理由见 `qr_key` 里的注释，复审 Minor 8）：
/// 只有在顶层这个键**不存在**、**不是字符串**（含 `null`）或**是空串**时
/// 才轮到 fallback。
///
/// ⚠️ **空串一律当「没有」**（复审 Minor N3）。上游发
/// `{"code":200,"unikey":""}` 时旧实现返回 `Some("")`，`qr_key` 的
/// `ok_or_else` 不会触发，于是一路走到 `qr_image("")`，把内容为
/// `https://music.163.com/login?codekey=` 的二维码渲染给用户 —— **一张扫不出
/// 任何东西的图，而错误要到好几步之后才暴露**。同一个文件里 `qr_image` 对
/// `qrurl` 已经有 `.filter(|s| !s.is_empty())` 守卫，这里补齐这个不对称。
fn pick_unikey(body: &Value) -> Option<&str> {
    // ⚠️ 两条路径**各自**都要过一遍非空，而不是只在收尾过滤一次：空串与 `null`
    // 同类（键存在但不可用），所以顶层发 `""` 时该轮到 fallback。收尾过滤会把
    // `/data` 里那个**能用的** key 一起挡掉，然后报「未返回 unikey」——
    // 而 unikey 其实就在 body 里。
    body.get("unikey")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| body.pointer("/data/unikey").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

/// 从 JSON 值里读一个整数状态码，**容忍字符串数字**。
///
/// 与 crate 自己的折叠逻辑（`src/request.rs:473-484`）逐字对齐：
/// 那边写的是 `as_i64().or_else(|| as_str().and_then(parse))`，
/// 即 crate 作者预料到过 `"code": "801"` 这种形状。我们不对齐的话，
/// 上游一旦改成字符串码，**crate 本来能扛住、我们却报 `Protocol` 错**
/// （复审 Minor 3）。非数字串与 `null` 仍然返回 `None`。
fn json_code(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// 把二维码内容渲染成 PNG 字节。**纯函数，可单测。**
fn render_qr_png(content: &str) -> Result<Vec<u8>, SourceError> {
    use image::Luma;
    use qrcode::QrCode;

    let code = QrCode::new(content.as_bytes())
        .map_err(|e| SourceError::Protocol(format!("二维码编码失败: {e}")))?;
    // 放大到 256px 见方，否则终端/手机上都太小扫不出来
    let img = code.render::<Luma<u8>>().min_dimensions(256, 256).build();

    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| SourceError::Protocol(format!("PNG 编码失败: {e}")))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_qr_png_produces_a_real_png() {
        // crate 不给图，我们自己渲染 —— 这条测试守的是「渲染真的产出了 PNG」，
        // 而不是「调用了某个 API」。
        let png = render_qr_png("https://music.163.com/login?codekey=abc").unwrap();
        assert!(
            png.starts_with(&[0x89, b'P', b'N', b'G']),
            "缺少 PNG 魔数，前 8 字节: {:?}",
            &png[..png.len().min(8)]
        );
        assert!(png.len() > 100, "PNG 太小，可能是空图: {} 字节", png.len());
    }

    #[test]
    fn rendered_qr_is_a_scannable_image() {
        // ★ 上一条只证明「是 PNG」，抓不到两类会让码扫不出来的回归：
        //   (a) **反色**（dark_color/light_color 对调）—— 很多扫码器读不出反色码；
        //   (b) **丢掉静默区**（`quiet_zone(false)`）—— 扫码率断崖式下降。
        // 复审实测的基准（同一个内容）：259×259、像素只有 0 与 255、
        // 静默区 4 模块。下面三条分别钉住尺寸、静默区、极性。
        let png = render_qr_png("https://music.163.com/login?codekey=abc").unwrap();
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("渲染出来的字节必须能被 PNG 解码器读回")
            .to_luma8();

        let (w, h) = img.dimensions();
        assert_eq!(w, h, "二维码必须是正方形，实得 {w}x{h}");
        assert!(
            w >= 250,
            "边长必须 ≥250px（min_dimensions 的产物），实得 {w}"
        );

        // 静默区：左上角 16×16 必须全白。实测静默区是 4 模块 × 7px = 28px，
        // 所以 16×16 必然整个落在静默区里 —— 反色或去掉静默区都会在这里断。
        for y in 0..16 {
            for x in 0..16 {
                let v = img.get_pixel(x, y).0[0];
                assert_eq!(v, 255, "左上角 ({x},{y}) 应落在白色静默区里，实得 {v}");
            }
        }

        // 极性：整张图只允许纯黑 0 与纯白 255。灰阶抖动（抗锯齿 / 缩放插值）
        // 会引入中间值，而中间值会让二值化阈值不稳。
        let mut dark = 0usize;
        let mut light = 0usize;
        for y in 0..h {
            for x in 0..w {
                let v = img.get_pixel(x, y).0[0];
                assert!(v == 0 || v == 255, "({x},{y}) = {v} 既不是黑也不是白");
                if v == 0 {
                    dark += 1;
                } else {
                    light += 1;
                }
            }
        }
        assert!(
            dark > 0 && light > 0,
            "图像必须同时含黑与白（全白/全黑都不可能是有效二维码），实得 dark={dark} light={light}"
        );
    }

    #[test]
    fn pick_unikey_falls_back_past_a_null_top_level_key() {
        // ★ 回归测试（复审 Minor 8）。旧写法 `.get(..).or_else(..)` 里
        // `or_else` 挂在 `Option<&Value>` 上：顶层是 `null` 时 `get` 得到
        // `Some(Null)` 而非 `None`，**fallback 根本不触发** —— 恰好是它
        // 本该救的场景。
        let body = serde_json::json!({"unikey": null, "data": {"unikey": "x"}});
        assert_eq!(pick_unikey(&body), Some("x"));

        // 顶层命中时优先用顶层（2026-09-26 真机实测的形状）
        let body = serde_json::json!({"code": 200, "unikey": "top", "data": {"unikey": "x"}});
        assert_eq!(pick_unikey(&body), Some("top"));

        // 顶层不是字符串也轮到 fallback
        let body = serde_json::json!({"unikey": 123, "data": {"unikey": "x"}});
        assert_eq!(pick_unikey(&body), Some("x"));

        // 两条路径都没有 → None（调用方据此报 Protocol 错）
        assert_eq!(pick_unikey(&serde_json::json!({"code": 200})), None);
        assert_eq!(pick_unikey(&serde_json::json!({"unikey": null})), None);
    }

    #[test]
    fn pick_unikey_treats_an_empty_string_as_absent() {
        // ★ 回归测试（复审 Minor N3）。实测旧行为：上游发
        // `{"code":200,"unikey":""}` 时返回 `Some("")`，`qr_key` 的
        // `ok_or_else` 不触发 → `Ok("")` → `qr_image("")` 渲染出内容为
        // `https://music.163.com/login?codekey=` 的二维码 —— **扫不出任何东西**，
        // 且错误要到好几步之后才暴露。空串和「没有 unikey」一样没用。
        assert_eq!(
            pick_unikey(&serde_json::json!({"code": 200, "unikey": ""})),
            None
        );
        // 空串与 `null` 同类：键存在但不可用 → 轮到 fallback，
        // 而不是把 `/data` 里那个能用的 key 一起挡掉。
        assert_eq!(
            pick_unikey(&serde_json::json!({"unikey": "", "data": {"unikey": "x"}})),
            Some("x")
        );
        // `/data` 一侧的空串同样不算数 —— 否则 `Some("")` 会从这条路径漏出去
        assert_eq!(
            pick_unikey(&serde_json::json!({"data": {"unikey": ""}})),
            None
        );
        assert_eq!(
            pick_unikey(&serde_json::json!({"unikey": null, "data": {"unikey": ""}})),
            None
        );
    }

    #[test]
    fn json_code_tolerates_string_status_codes() {
        // ★ 回归测试（复审 Minor 3）。crate 自己（`request.rs:473-484`）容忍
        // `"code": "801"`；我们不容忍的话，上游一次形状变化就会把本来能扛住的
        // 情况变成 `Protocol` 错。
        assert_eq!(json_code(&serde_json::json!(801)), Some(801));
        assert_eq!(json_code(&serde_json::json!("801")), Some(801));

        // 但真正的垃圾仍要被拒 —— 不能为了容忍字符串把非数字也放进来
        assert_eq!(json_code(&serde_json::json!("等待扫码")), None);
        assert_eq!(json_code(&serde_json::json!(null)), None);
        assert_eq!(json_code(&serde_json::json!({})), None);
    }
}
