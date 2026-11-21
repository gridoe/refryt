//! Unison: lyrics contributed and voted on by listeners, from the BetterLyrics project, so it has
//! songs nobody licenses.
//!
//! One GET by title, artist, album and length answers `{"success":…, "data":{"lyrics",
//! "format", "syncType"}}`: TTML, LRC (word stamps or not), or plain text. Smaller than the
//! others, but keyless and quick to say no.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::{Lyrics, Track};

const ENDPOINT: &str = "https://unison.boidu.dev/lyrics";

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    success: bool,
    data: Option<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    lyrics: Option<String>,
    format: Option<String>,
    #[serde(rename = "syncType")]
    sync_type: Option<String>,
}

pub async fn lyrics(http: &reqwest::Client, track: &Track) -> Result<Option<Lyrics>> {
    let mut query: Vec<(&str, String)> = vec![
        ("song", track.title.clone()),
        ("artist", track.artist_line()),
    ];
    if let Some(album) = track.album.as_deref().filter(|a| !a.is_empty()) {
        query.push(("album", album.to_owned()));
    }
    if let Some(seconds) = track.duration.filter(|d| *d > 0) {
        query.push(("duration", seconds.to_string()));
    }
    let response = http
        .get(ENDPOINT)
        .query(&query)
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .context("Unison")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body: Response = response
        .error_for_status()
        .context("Unison")?
        .json()
        .await
        .context("Unison")?;
    Ok(body.success.then_some(body.data).flatten().and_then(read))
}

fn read(entry: Entry) -> Option<Lyrics> {
    let text = entry.lyrics.filter(|t| !t.trim().is_empty())?;
    let is = |field: &Option<String>, value: &str| {
        field
            .as_deref()
            .is_some_and(|f| f.eq_ignore_ascii_case(value))
    };
    let mut lyrics = if is(&entry.format, "ttml") {
        super::better_lyrics::parse_ttml(&text)?
    } else {
        let timed = super::lrclib::parse_lrc(&text);
        if !is(&entry.sync_type, "plain") && !timed.is_empty() {
            Lyrics {
                lines: timed,
                synced: true,
                source: String::new(),
            }
        } else {
            Lyrics {
                lines: super::lrclib::plain_lines(&text),
                synced: false,
                source: String::new(),
            }
        }
    };
    lyrics.source = "Unison".to_owned();
    Some(lyrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(lyrics: &str, format: &str, sync: &str) -> Entry {
        Entry {
            lyrics: Some(lyrics.to_owned()),
            format: Some(format.to_owned()),
            sync_type: Some(sync.to_owned()),
        }
    }

    #[test]
    fn lrc_plain_and_word_stamped_lrc_all_read() {
        let timed = read(entry("[00:01.00]One\n[00:02.50]Two", "lrc", "line")).unwrap();
        assert!(timed.synced);
        assert_eq!(timed.lines[1].at_ms, 2_500);

        let words = read(entry(
            "[00:01.00]<00:01.00>One <00:01.40>more",
            "lrc",
            "word",
        ))
        .unwrap();
        assert_eq!(words.lines[0].text, "One more");

        let plain = read(entry("One\nTwo", "lrc", "plain")).unwrap();
        assert!(!plain.synced);
        assert_eq!(plain.lines.len(), 2);
        assert!(read(entry("  ", "lrc", "line")).is_none());
    }
}
