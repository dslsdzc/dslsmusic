use crate::PLATFORM;
use crate::client::level_for;
use dsls_provider_api::{Quality, SourceError, Track};
use serde_json::Value;

pub fn parse_search_tracks(body: &Value) -> Result<Vec<Track>, SourceError> {
    let songs = body
        .pointer("/result/songs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| SourceError::Protocol("cloudsearch 响应缺少 result.songs".into()))?;

    Ok(songs
        .iter()
        .filter_map(|s| {
            let id = s.get("id")?.as_i64()?;
            let title = s.get("name")?.as_str()?.to_string();
            let artist = s
                .get("ar")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            let album = s
                .pointer("/al/name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let duration_ms = s.get("dt").and_then(|v| v.as_u64()).unwrap_or(0);
            Some(Track {
                id: id.to_string(),
                platform: PLATFORM.to_string(),
                title,
                artist,
                album,
                duration_ms,
            })
        })
        .collect())
}

/// 从 `song/url/v1` 响应里取出可播放 URL，**并核对实际档位**。
///
/// `want` 是**精确档位要求**，不是「取所能及的最高档」——这是 Task 5 的
/// `MusicSource::stream_url` 文档定下的契约，也是
/// `should_retry_lower_quality()` 唯一有意义的来源。
pub fn parse_stream_url(body: &Value, want: Quality) -> Result<String, SourceError> {
    let data = body
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| SourceError::Protocol("song/url 响应缺少 data".into()))?;

    let first = data.first().ok_or(SourceError::Unavailable)?;

    let url = match first.get("url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u,
        // url 为 null / 空：有能力但当前拿不到（版权、地区、需要 VIP）
        _ => return Err(SourceError::Unavailable),
    };

    // ⚠️ 档位核对。**直接返回低档 URL 是静默的谎言**——调用方会以为
    // 拿到了无损，实际听到的是 320k。返回 Unavailable 才能让 resolver
    // 走 `Quality::lower()` 的显式降级链，把「降档」这件事告诉用户。
    //
    // **判据是 `level` 字符串，不是 `br`。** 实测 `br = 999000` **同时覆盖
    // `lossless` 和 `hires`**，用它分不出这两档；而 `level` 回显服务端
    // 实际授予的档位，可靠且精确。
    //
    // 这条检查会**频繁**命中：实测匿名（或非会员）请求 lossless/hires/jymaster
    // 一律返回 `level: "exhigh"` —— **静默降级是常态**，不是异常。
    //
    // 不对称取舍（有意为之）：`level` **缺失时不拒绝**。拒绝一个本可播放的
    // URL 会让用户直接听不到歌；接受一个无法核实的 URL 最坏只是音质
    // 低于预期。前者的代价大得多。
    if let Some(got) = first.get("level").and_then(|v| v.as_str()) {
        if got != level_for(want)? {
            return Err(SourceError::Unavailable);
        }
    }

    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsls_provider_api::Quality;
    use serde_json::json;

    #[test]
    fn parses_cloudsearch_result() {
        let body = json!({
            "result": {
                "songs": [{
                    "id": 123456,
                    "name": "测试歌曲",
                    "dt": 245000,
                    "ar": [{"name": "歌手A"}, {"name": "歌手B"}],
                    "al": {"name": "专辑X"}
                }]
            }
        });
        let tracks = parse_search_tracks(&body).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "123456");
        assert_eq!(tracks[0].title, "测试歌曲");
        assert_eq!(tracks[0].artist, "歌手A / 歌手B");
        assert_eq!(tracks[0].album, "专辑X");
        assert_eq!(tracks[0].duration_ms, 245000);
        // platform 是路由键，不是装饰：resolver 靠它找回正确的 provider
        assert_eq!(tracks[0].platform, PLATFORM);
    }

    #[test]
    fn empty_result_is_ok_not_error() {
        // Ok(None) / 空列表 与 Err(Unsupported) 必须区分（设计文档 §4.5）
        let body = json!({"result": {"songs": []}});
        assert_eq!(parse_search_tracks(&body).unwrap().len(), 0);
    }

    #[test]
    fn missing_result_field_is_protocol_error() {
        let body = json!({"code": 200});
        let err = parse_search_tracks(&body).unwrap_err();
        assert!(err.is_upstream_changed(), "应为 Protocol 错误: {err:?}");
    }

    #[test]
    fn parses_stream_url() {
        let body = json!({
            "data": [{
                "url": "https://m8.music.126.net/x.mp3",
                "level": "exhigh",
                "br": 320000,
                "code": 200
            }]
        });
        let url = parse_stream_url(&body, Quality::High).unwrap();
        assert_eq!(url, "https://m8.music.126.net/x.mp3");
    }

    #[test]
    fn null_url_means_unavailable_not_protocol_error() {
        // 版权下架：有能力但拿不到 → Unavailable，不是 Protocol
        let body = json!({"data": [{"url": null, "code": 200}]});
        let err = parse_stream_url(&body, Quality::High).unwrap_err();
        assert!(matches!(err, dsls_provider_api::SourceError::Unavailable));
        assert!(err.should_retry_lower_quality());
    }

    #[test]
    fn empty_data_array_is_unavailable() {
        let body = json!({"data": []});
        let err = parse_stream_url(&body, Quality::High).unwrap_err();
        assert!(matches!(err, dsls_provider_api::SourceError::Unavailable));
    }

    #[test]
    fn rejects_silent_downgrade() {
        // ★ 这是本组最重要的一条。要无损、服务端给了极高 ——
        // **实测这是常态而非异常**：匿名/非会员请求 lossless / hires /
        // jymaster 一律返回 level:"exhigh"，且不报错、不提示。
        // 必须拒绝，否则就是静默的谎言：「无损」标签配 320k 的声音。
        let body = json!({
            "data": [{
                "url": "https://m8.music.126.net/x.mp3",
                "level": "exhigh",
                "br": 320000,
                "code": 200
            }]
        });
        let err = parse_stream_url(&body, Quality::Lossless).unwrap_err();
        assert!(matches!(err, dsls_provider_api::SourceError::Unavailable));
        // 这条断言是关键：它保证降档链真的可执行（Task 5 审查的重点）
        assert!(err.should_retry_lower_quality());
    }

    #[test]
    fn hires_is_not_confused_with_lossless() {
        // br 分不出这两档（实测 999000 同时覆盖二者），
        // 所以才改用 level 字符串 —— 这条测试钉的就是这个区别。
        let lossless = json!({
            "data": [{"url": "https://m8.music.126.net/l.flac", "level": "lossless",
                      "br": 999000, "code": 200}]
        });
        assert!(
            matches!(
                parse_stream_url(&lossless, Quality::HiRes),
                Err(dsls_provider_api::SourceError::Unavailable)
            ),
            "br 同为 999000，但 level 是 lossless —— 要 hires 时必须拒绝"
        );
    }

    #[test]
    fn accepts_when_level_absent() {
        // level 缺失时不拒绝：拒绝一个能播的 URL 让用户直接听不到歌，
        // 代价远大于音质低于预期。这是有意的不对称取舍。
        let body = json!({"data": [{"url": "https://m8.music.126.net/x.mp3", "code": 200}]});
        assert_eq!(
            parse_stream_url(&body, Quality::HiRes).unwrap(),
            "https://m8.music.126.net/x.mp3"
        );
    }
}
