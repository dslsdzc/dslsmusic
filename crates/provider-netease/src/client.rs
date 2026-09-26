use dsls_provider_api::SourceError;
use ncm_api_rs::{ApiClient, Query, create_client};

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
        // 无版本号的上游（设计文档 §13.1 整节都在讲这个），多留一条 `or_else`
        // 换的是「上游改形状时还能活着」而不是「更正确」。
        // 要删就删这一行，删掉后本函数的行为在今天的上游上与保留时**完全一致**。
        resp.body
            .get("unikey")
            .or_else(|| resp.body.pointer("/data/unikey"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| SourceError::Protocol("login_qr_key 未返回 unikey".into()))
    }

    /// 第二步：拿到扫码 URL，**在本地渲染**成 PNG。
    ///
    /// ⚠️ **crate 不提供二维码图片，这一步必须我们自己做。**
    /// 已核实 `src/api/login_qr_create.rs` 全文 28 行：它**不发任何网络请求**，
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
        let code = resp
            .body
            .pointer("/code")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| SourceError::Protocol("login_qr_check 未返回 code".into()))?;

        // ⚠️ cookie **不在 body 里** —— 它在 `ApiResponse.cookie`，由响应头
        // `Set-Cookie` 填充（request.rs:448-458，crate 只删了 `Domain=`）。
        // 已核实 `ApiResponse` 是 { status: i64, body: Value, cookie: Vec<String> }。
        // Node 版会把 cookie 塞进 body（login_qr_check.js），**Rust 版没有这层整形**，
        // 所以「读 body.cookie」的写法在这里不适用。
        //
        // 传原始条目进去，由 `session_cookie` 负责拼装与清洗。
        Ok(crate::qr_login::QrState::from_code(code, &resp.cookie))
    }
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
}
