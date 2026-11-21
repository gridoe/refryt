//! lrc.red: Apple Music's TTML, filed by recording (ISRC).
//!
//! A search (`search.json?q=`) names the recordings it has, each with its ISRC, title, artist
//! and length. The best match's document is then `s/<ISRC>.ttml`, the same TTML BetterLyrics
//! serves, but for any song lrc.red indexed rather than only those a cache holds. Matching
//! follows BitChord's: the same title once brackets are set aside, the same version words
//! ("live", "remix", …), a shared artist, and a length within 3 seconds.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::{Lyrics, Track};

const BASE: &str = "https://lrc.red/";
const LENGTH_TOLERANCE_SECS: f64 = 3.0;

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    hits: Vec<Hit>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Hit {
    isrc: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    duration: Option<f64>,
}

pub async fn lyrics(http: &reqwest::Client, track: &Track) -> Result<Option<Lyrics>> {
    let artist = track.artist_line();
    let query = format!("{} {}", track.title, artist);
    let response: Response = http
        .get(format!("{BASE}search.json"))
        .query(&[("q", query.trim())])
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .context("lrc.red search")?
        .error_for_status()
        .context("lrc.red search")?
        .json()
        .await
        .context("lrc.red search")?;
    let Some(isrc) = best(&response.hits, &track.title, &artist, track.duration)
        .and_then(|hit| hit.isrc.clone())
    else {
        return Ok(None);
    };
    let document = http
        .get(format!("{BASE}s/{isrc}.ttml"))
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .context("lrc.red document")?;
    if document.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let ttml = document
        .error_for_status()
        .context("lrc.red document")?
        .text()
        .await
        .context("lrc.red document")?;
    Ok(super::better_lyrics::parse_ttml(&ttml))
}

/// The hit that is this recording, nearest in length when several are.
fn best<'a>(hits: &'a [Hit], title: &str, artist: &str, seconds: Option<u64>) -> Option<&'a Hit> {
    let core = super::title_core(title);
    let version = super::version_words(title);
    let artists = super::artist_names(artist);
    let distance = |hit: &Hit| match (seconds, hit.duration) {
        (Some(s), Some(d)) if s > 0 => (d - s as f64).abs(),
        _ => 0.0,
    };
    hits.iter()
        .filter(|hit| {
            let Some(name) = hit.title.as_deref() else {
                return false;
            };
            hit.isrc.as_deref().is_some_and(valid_isrc)
                && super::title_core(name) == core
                && super::version_words(name) == version
                && (artists.is_empty()
                    || super::artist_names(hit.artist.as_deref().unwrap_or_default())
                        .iter()
                        .any(|a| artists.contains(a)))
                && distance(hit) <= LENGTH_TOLERANCE_SECS
        })
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
}

/// Two letters, three letters or digits, seven digits: the shape of an ISRC, so nothing else
/// ends up in the document's path.
fn valid_isrc(isrc: &str) -> bool {
    let b = isrc.as_bytes();
    b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..5]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && b[5..].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(isrc: &str, title: &str, artist: &str, duration: f64) -> Hit {
        Hit {
            isrc: Some(isrc.to_owned()),
            title: Some(title.to_owned()),
            artist: Some(artist.to_owned()),
            duration: Some(duration),
        }
    }

    #[test]
    fn the_same_recording_is_picked_and_other_versions_are_not() {
        let hits = vec![
            hit(
                "USUG11904206",
                "Blinding Lights (Live)",
                "The Weeknd",
                200.0,
            ),
            hit("USUG11904207", "Blinding Lights", "Someone Else", 200.0),
            hit("USUG11904208", "Blinding Lights", "The Weeknd", 210.0),
            hit(
                "USUG11904209",
                "Blinding Lights",
                "The Weeknd & Rosalía",
                201.0,
            ),
        ];
        let picked = best(&hits, "Blinding Lights", "The Weeknd", Some(200)).unwrap();
        assert_eq!(picked.isrc.as_deref(), Some("USUG11904209"));
        assert!(best(&hits, "Blinding Lights", "Nobody", Some(200)).is_none());
    }

    #[test]
    fn only_isrc_shaped_codes_are_used() {
        assert!(valid_isrc("USUG11904206"));
        assert!(valid_isrc("GBCFB1700229"));
        assert!(!valid_isrc("../etc/passwd"));
        assert!(!valid_isrc("usug11904206"));
    }
}
