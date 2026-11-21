//! KuGou: a large Chinese music service's line-timed lyrics, strong outside the English
//! catalogue.
//!
//! Three steps, as BitChord does it: a song search for the recordings near this length, a
//! lyrics search by the nearest one's hash (or, failing that, by name and length), and a
//! download of the first candidate as base64 LRC. KuGou's LRC opens and closes with credit lines
//! ("作词 : …"), which are cut, and a track with no words gets a "pure music" notice instead,
//! which counts as no lyrics.

use anyhow::{Context, Result};
use base64::Engine;
use serde::Deserialize;

use crate::model::{Lyrics, Track};

const LENGTH_TOLERANCE_SECS: i64 = 8;

#[derive(Deserialize)]
struct SongSearch {
    data: Option<SongData>,
}
#[derive(Deserialize)]
struct SongData {
    #[serde(default)]
    info: Vec<SongInfo>,
}
#[derive(Deserialize)]
struct SongInfo {
    hash: String,
    #[serde(default = "unknown")]
    duration: i64,
}
fn unknown() -> i64 {
    -1
}
#[derive(Deserialize)]
struct LyricsSearch {
    #[serde(default)]
    candidates: Vec<Candidate>,
}
#[derive(Deserialize)]
struct Candidate {
    id: String,
    accesskey: String,
}
#[derive(Deserialize)]
struct Download {
    content: String,
}

pub async fn lyrics(http: &reqwest::Client, track: &Track) -> Result<Option<Lyrics>> {
    let keyword = keyword(track);
    let seconds = track.duration.map_or(-1, |d| d as i64);

    let mut candidate = None;
    for hash in song_hashes(http, &keyword, seconds).await? {
        let found = search(http, &[("hash", hash.as_str())]).await?;
        if let Some(first) = found.into_iter().next() {
            candidate = Some(first);
            break;
        }
    }
    if candidate.is_none() {
        let ms = (seconds > 0).then(|| (seconds * 1000).to_string());
        let mut query = vec![("keyword", keyword.as_str())];
        if let Some(ms) = ms.as_deref() {
            query.push(("duration", ms));
        }
        candidate = search(http, &query).await?.into_iter().next();
    }
    let Some(candidate) = candidate else {
        return Ok(None);
    };

    let download: Download = get(
        http,
        "https://lyrics.kugou.com/download",
        &[
            ("fmt", "lrc"),
            ("charset", "utf8"),
            ("client", "pc"),
            ("ver", "1"),
            ("id", candidate.id.as_str()),
            ("accesskey", candidate.accesskey.as_str()),
        ],
    )
    .await?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(download.content.trim())
        .context("KuGou lyrics")?;
    let lrc = String::from_utf8_lossy(&bytes);
    let lines = strip_credits(super::lrclib::parse_lrc(&lrc));
    if lines.is_empty() || is_instrumental_notice(&lines) {
        return Ok(None);
    }
    Ok(Some(Lyrics {
        lines,
        synced: true,
        source: "KuGou".to_owned(),
    }))
}

/// "Title - Artist Album", without bracketed parts, as KuGou's own search wants it.
fn keyword(track: &Track) -> String {
    let mut out = format!(
        "{} - {}",
        strip_brackets(&track.title),
        strip_brackets(&track.artist_line())
    );
    if let Some(album) = track.album.as_deref().filter(|a| !a.is_empty()) {
        out.push(' ');
        out.push_str(album);
    }
    out
}

fn strip_brackets(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '(' | '（' => depth += 1,
            ')' | '）' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.trim().to_owned();
    if out.is_empty() {
        text.trim().to_owned()
    } else {
        out
    }
}

/// The hashes of the recordings within [`LENGTH_TOLERANCE_SECS`] of this one, nearest first.
async fn song_hashes(http: &reqwest::Client, keyword: &str, seconds: i64) -> Result<Vec<String>> {
    let found: SongSearch = get(
        http,
        "https://mobileservice.kugou.com/api/v3/search/song",
        &[
            ("version", "9108"),
            ("plat", "0"),
            ("pagesize", "8"),
            ("showtype", "0"),
            ("keyword", keyword),
        ],
    )
    .await?;
    let mut info: Vec<SongInfo> = found
        .data
        .map(|d| d.info)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| seconds <= 0 || (i.duration - seconds).abs() <= LENGTH_TOLERANCE_SECS)
        .collect();
    info.sort_by_key(|i| (i.duration - seconds).abs());
    Ok(info.into_iter().map(|i| i.hash).collect())
}

async fn search(http: &reqwest::Client, by: &[(&str, &str)]) -> Result<Vec<Candidate>> {
    let mut query = vec![("ver", "1"), ("man", "yes"), ("client", "pc")];
    query.extend_from_slice(by);
    let found: LyricsSearch = get(http, "https://lyrics.kugou.com/search", &query).await?;
    Ok(found.candidates)
}

async fn get<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    query: &[(&str, &str)],
) -> Result<T> {
    http.get(url)
        .query(query)
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .context("KuGou")?
        .error_for_status()
        .context("KuGou")?
        .json()
        .await
        .context("KuGou")
}

/// Whether a line is a credit: "作词 : 某人", "Composer: Someone".
fn is_credit(text: &str) -> bool {
    text.split_once([':', '：']).is_some_and(|(k, v)| {
        !k.trim().is_empty() && !v.trim().is_empty() && k.chars().count() <= 12
    })
}

/// Cut the credit lines KuGou opens and closes its lyrics with: the "Title - Artist" line and
/// the run of credits after it, and the run of credits at the end. Only runs at the very ends,
/// so a short song's closing credit can't take its lyrics with it.
fn strip_credits(mut lines: Vec<crate::model::LyricLine>) -> Vec<crate::model::LyricLine> {
    let title_line = lines.first().is_some_and(|l| l.text.contains(" - "))
        && lines.get(1).is_some_and(|l| is_credit(&l.text));
    let mut head = usize::from(title_line);
    while lines.get(head).is_some_and(|l| is_credit(&l.text)) {
        head += 1;
    }
    lines.drain(..head);
    while lines.last().is_some_and(|l| is_credit(&l.text)) {
        lines.pop();
    }
    lines
}

/// "纯音乐，请欣赏" ("pure music, please enjoy") on every line: an instrumental.
fn is_instrumental_notice(lines: &[crate::model::LyricLine]) -> bool {
    let words: Vec<&str> = lines
        .iter()
        .map(|l| l.text.trim())
        .filter(|t| !t.is_empty())
        .collect();
    !words.is_empty() && words.iter().all(|t| t.contains("纯音乐"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::lrclib::parse_lrc;

    #[test]
    fn credits_at_both_ends_are_cut() {
        let lines = parse_lrc(
            "[00:00.00]夜に駆ける - YOASOBI\n[00:01.00]词：Ayase\n[00:02.00]曲：Ayase\n\
             [00:10.00]沈むように\n[00:12.00]溶けてゆくように\n[03:00.00]Producer: Someone\n",
        );
        let kept: Vec<_> = strip_credits(lines).into_iter().map(|l| l.text).collect();
        assert_eq!(kept, ["沈むように", "溶けてゆくように"]);
    }

    #[test]
    fn a_pure_music_notice_is_no_lyrics() {
        let lines = parse_lrc("[00:00.00]纯音乐，请欣赏\n");
        assert!(is_instrumental_notice(&lines));
        assert!(!is_instrumental_notice(&parse_lrc("[00:00.00]la la\n")));
    }

    #[test]
    fn brackets_leave_the_keyword() {
        assert_eq!(strip_brackets("Glue (Edit)"), "Glue");
        assert_eq!(strip_brackets("夜に駆ける（Remix）"), "夜に駆ける");
        assert_eq!(strip_brackets("(Intro)"), "(Intro)");
    }
}
