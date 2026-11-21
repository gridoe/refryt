//! YouTube Music's own timed lyrics.
//!
//! The website only ever gets plain text (`Api::lyrics`), but YouTube Music's Android app gets
//! the same lyrics with a start time per line, from Musixmatch or LyricFind. Asked as that app,
//! the `browse` of a song's lyrics id answers `timedLyricsData`: a list of
//! `{lyricLine, cueRange: {startTimeMilliseconds, endTimeMilliseconds}}`, and a `sourceMessage`
//! naming the provider. Checked 2026-11-21: 5 of 6 songs came back timed.
//!
//! Always sent without the session's cookie: the lyrics are the same signed out, and these
//! requests then carry nothing about the account.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::model::{LyricLine, Lyrics};

const ENDPOINT: &str = "https://music.youtube.com/youtubei/v1";
const CLIENT_NAME: &str = "ANDROID_MUSIC";
const CLIENT_VERSION: &str = "7.27.52";
const USER_AGENT: &str =
    "com.google.android.apps.youtube.music/7.27.52 (Linux; U; Android 14) gzip";

/// The timed lyrics for `video_id`, or `None` when YouTube has none with timing.
pub async fn synced(http: &reqwest::Client, video_id: &str) -> Result<Option<Lyrics>> {
    let next = post(http, "next", json!({ "videoId": video_id })).await?;
    let Some(browse_id) = lyrics_browse_id(&next) else {
        return Ok(None);
    };
    let page = post(http, "browse", json!({ "browseId": browse_id })).await?;
    Ok(parse_timed(&page))
}

async fn post(http: &reqwest::Client, endpoint: &str, body: Value) -> Result<Value> {
    let mut body = body;
    body["context"] = json!({
        "client": {
            "clientName": CLIENT_NAME,
            "clientVersion": CLIENT_VERSION,
            "androidSdkVersion": 34,
            "osName": "Android",
            "osVersion": "14",
            "hl": "en",
        }
    });
    let response = http
        .post(format!("{ENDPOINT}/{endpoint}?prettyPrint=false"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .json(&body)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("YouTube {endpoint} for lyrics"))?
        .error_for_status()
        .with_context(|| format!("YouTube {endpoint} for lyrics"))?;
    response
        .json()
        .await
        .with_context(|| format!("YouTube {endpoint} for lyrics"))
}

/// The lyrics tab's browse id (`MPLYt…`) in a `next` answer.
fn lyrics_browse_id(next: &Value) -> Option<String> {
    let mut ids = Vec::new();
    find_all(next, "browseId", &mut ids);
    ids.into_iter()
        .filter_map(Value::as_str)
        .find(|id| id.starts_with("MPLY"))
        .map(str::to_owned)
}

/// The lines of a lyrics page's `timedLyricsData`, with the provider its `sourceMessage` names.
fn parse_timed(page: &Value) -> Option<Lyrics> {
    let mut found = Vec::new();
    find_all(page, "timedLyricsData", &mut found);
    let data = found.into_iter().find_map(Value::as_array)?;
    let mut lines: Vec<LyricLine> = data
        .iter()
        .filter_map(|line| {
            let text = line.get("lyricLine")?.as_str()?;
            let at_ms = line
                .pointer("/cueRange/startTimeMilliseconds")
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| v.as_u64())
                })?;
            Some(LyricLine {
                at_ms,
                text: text.trim().to_owned(),
            })
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|l| l.at_ms);
    let mut messages = Vec::new();
    find_all(page, "sourceMessage", &mut messages);
    let source = messages
        .into_iter()
        .find_map(|m| {
            m.as_str().map(str::to_owned).or_else(|| {
                m.get("runs")?.as_array().map(|runs| {
                    runs.iter()
                        .filter_map(|r| r.get("text")?.as_str())
                        .collect::<String>()
                })
            })
        })
        .map(|s| s.trim_start_matches("Source:").trim().to_owned())
        .unwrap_or_default();
    Some(Lyrics {
        lines,
        synced: true,
        source,
    })
}

/// Every value under `key`, anywhere in `value`.
fn find_all<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    out.push(v);
                }
                find_all(v, key, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                find_all(v, key, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lyrics_tab_id_is_found_among_the_others() {
        let next = json!({"tabs": [
            {"endpoint": {"browseId": "MPTRt_abc"}},
            {"endpoint": {"browseId": "MPLYt_xyz"}},
        ]});
        assert_eq!(lyrics_browse_id(&next).as_deref(), Some("MPLYt_xyz"));
        assert_eq!(lyrics_browse_id(&json!({})), None);
    }

    /// The shape the Android client answers: string milliseconds, lines in order, and the
    /// provider named after "Source:".
    #[test]
    fn timed_lyrics_parse_with_their_source() {
        let page = json!({"contents": {"elementRenderer": {"newElement": {"type": {
            "componentType": {"model": {"timedLyricsModel": {"lyricsData": {
                "timedLyricsData": [
                    {"lyricLine": "second", "cueRange": {"startTimeMilliseconds": "6290", "endTimeMilliseconds": "9000"}},
                    {"lyricLine": "first ", "cueRange": {"startTimeMilliseconds": "2620", "endTimeMilliseconds": "6290"}},
                    {"cueRange": {"startTimeMilliseconds": "9000"}},
                ],
                "sourceMessage": "Source: Musixmatch",
            }}}}
        }}}}});
        let lyrics = parse_timed(&page).unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.source, "Musixmatch");
        assert_eq!(lyrics.lines.len(), 2, "a line with no text is skipped");
        assert_eq!(lyrics.lines[0].text, "first");
        assert_eq!(lyrics.lines[0].at_ms, 2620);
        assert_eq!(lyrics.lines[1].at_ms, 6290);
    }

    #[test]
    fn a_page_without_timing_is_none() {
        let page = json!({"contents": {"messageRenderer": {"text": "Lyrics not available"}}});
        assert!(parse_timed(&page).is_none());
    }
}
