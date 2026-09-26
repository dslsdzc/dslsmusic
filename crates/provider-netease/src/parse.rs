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
            let id = s.get("id").and_then(json_id)?;
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

/// 从 JSON 值里读一个整数 id，**容忍字符串数字**。
///
/// 与 `client.rs::json_code` 是同一套容忍规则（那个是状态码、这个是资源 id），
/// 理由也一样：**上游发什么形状由它自己决定**，而 crate 自己的折叠逻辑
/// （`ncm-api-rs/src/request.rs:473-484` 的 `as_i64().or_else(|| as_str()…)`）
/// 说明作者已经预料过「数字被发成字符串」这种形状。
///
/// ⚠️ **这里的失败是静默的**，所以容忍不是可选的美化（复审 M1）：`as_i64()`
/// 对字符串 id 返回 `None`，而调用点在 `filter_map` 里用 `?` 丢掉该条 ——
/// **全部**丢光时我们返回 `Ok(Some(vec![]))`，按三态契约那是「查到了但没结果」
/// ⇒ 用户看到「没搜到」，**不换音源、不报错**。这是最坏的一类失败。
///
/// 非数字串与 `null` 仍然返回 `None`（该条照旧被跳过）—— 容忍字符串数字
/// **不等于**接受垃圾。
fn json_id(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
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

    // ⚠️ **试听片段判定**（Task 7 复审 Important I1）。
    //
    // 契约在 `provider-api/src/lib.rs:31-33` 与设计文档 §4.5：
    // **试听片段 / 残缺响应必须映射成 `Err(Unavailable)`**。网易 `song/url`
    // 响应族里带试听信号的是 `freeTrialInfo`（字段确实存在）；而试听条目的
    // `url` 是**非 null 的**，所以只看 `url` 与 `level` 会把「30 秒试听」
    // 当成功返回 —— 播放器播 30 秒然后停住，而调用方以为整首歌到手了。
    //
    // **判据只看「是不是 null」，绝不下钻它的内部**：`freeTrialInfo` 非 null 的
    // 确切形状**从未被观测过**（复审匿名实测 6 首 —— 1 首免费 + 5 首 VIP ——
    // 全是 `url:null` + `freeTrialInfo:null`；曝出面需要「已登录的非会员会话」，
    // 而那正是用户的常见状态，所以这个洞是真的）。今天对内部形状的每一处假设，
    // 都可能在第一次真实触发时是错的 —— 所以只问「有没有」，不问「长什么样」。
    //
    // ⚠️ **代价的不对称**：若上游对**可完整播放**的曲目也带上非 null 的
    // `freeTrialInfo`，这里会误拒一个能播的 URL（比原缺口更糟的类型）。
    // 没有观测数据就没法从代码上消掉这个风险 —— 能消掉的是**不可诊断性**：
    // 下面这条日志把实际形状原样打出来，这条路径今天不可达，
    // **第一次真实触发时它是唯一线索**（复审明确要求不许静默）。
    if let Some(trial) = first.get("freeTrialInfo").filter(|v| !v.is_null()) {
        log::warn!(
            "song/url 返回试听片段（freeTrialInfo 非 null）→ 按 Unavailable 处理；\
             实测形状: freeTrialInfo={trial}, code={code}, level={level}",
            code = first.get("code").unwrap_or(&Value::Null),
            level = first.get("level").unwrap_or(&Value::Null),
        );
        return Err(SourceError::Unavailable);
    }

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
    fn tolerates_string_ids_in_search_results() {
        // ★ 回归测试（复审 M1）。上游若把 `id` 发成字符串，`as_i64()` 返回 `None`
        // → `filter_map` 里的 `?` **静默丢掉该条**；全部丢光时我们返回
        // `Ok(Some(vec![]))`，按三态契约那是「查到了但没结果」⇒ 用户看到
        // 「没搜到」，不换音源、不报错。同 crate 的 `json_code` 已为同一理由
        // 容忍字符串码，这条测试补齐那个不对称。
        let body = json!({
            "result": {
                "songs": [{
                    "id": "123456",
                    "name": "字符串 id 的歌",
                    "dt": 245000,
                    "ar": [{"name": "歌手A"}],
                    "al": {"name": "专辑X"}
                }]
            }
        });
        let tracks = parse_search_tracks(&body).unwrap();
        assert_eq!(tracks.len(), 1, "字符串 id 不该让整条结果静默消失");
        assert_eq!(tracks[0].id, "123456");
        assert_eq!(tracks[0].title, "字符串 id 的歌");
    }

    #[test]
    fn tolerance_does_not_swallow_garbage_ids() {
        // 反面：容忍「字符串数字」**不等于**接受垃圾。非数字串仍要被跳过
        // （该条无法定位，留着只会造出一个 id 不可用的 Track）。
        let body = json!({
            "result": {
                "songs": [
                    {"id": "abc", "name": "垃圾 id"},
                    {"id": 42, "name": "好的"}
                ]
            }
        });
        let tracks = parse_search_tracks(&body).unwrap();
        assert_eq!(tracks.len(), 1, "垃圾 id 的条目应被跳过，好的那条要留下");
        assert_eq!(tracks[0].id, "42");
    }

    #[test]
    fn empty_result_is_ok_not_error() {
        // 「查到了但没结果」（`Ok(Some(空集合))`）与「本音源不处理」
        // （`Ok(None)` / `Err(Unsupported)`）必须区分 —— 设计文档 §4.5，
        // 2026-09-26 收口：`Ok(None)` 只表示「不处理」，不再表示「确实没有」。
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
    fn non_null_free_trial_info_is_unavailable_not_a_playable_url() {
        // ★ 回归测试（复审 Important I1）。契约（`provider-api/src/lib.rs:31-33`、
        // 设计文档 §4.5）：**试听片段 / 残缺响应必须映射成 `Err(Unavailable)`**
        // —— 否则播放器播 30 秒然后停住，而调用方以为整首歌到手了。
        //
        // ⚠️ 判据**只能是「非 null」**：非 null 的确切形状从未被观测过
        // （复审匿名实测 6 首全是 `freeTrialInfo:null`；曝露面要「已登录的
        // 非会员会话」）。所以这里故意喂三种**形状各异**的非 null 值 ——
        // 包含空对象。若哪天实现改成下钻内部结构（比如「有 start/end 才算试听」），
        // 空对象那一条会立刻变红，正是它的用处。
        for trial in [json!({}), json!({"start": 0, "end": 30}), json!("未知形状")] {
            let body = json!({
                "data": [{
                    "url": "https://m8.music.126.net/trial.mp3",
                    "level": "exhigh",
                    "br": 320000,
                    "code": 200,
                    "freeTrialInfo": trial
                }]
            });
            let err = parse_stream_url(&body, Quality::High).unwrap_err();
            assert!(
                matches!(err, dsls_provider_api::SourceError::Unavailable),
                "freeTrialInfo={trial} 时必须是 Unavailable，实得 {err:?}"
            );
        }
    }

    #[test]
    fn null_free_trial_info_still_plays() {
        // 反面（同样重要）：`freeTrialInfo: null` 是**匿名实测的常态**，
        // 误判成试听会让所有匿名取流都变成 Unavailable —— 那比原缺口更糟。
        let body = json!({
            "data": [{
                "url": "https://m8.music.126.net/x.mp3",
                "level": "exhigh",
                "br": 320000,
                "code": 200,
                "freeTrialInfo": null
            }]
        });
        assert_eq!(
            parse_stream_url(&body, Quality::High).unwrap(),
            "https://m8.music.126.net/x.mp3"
        );
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
