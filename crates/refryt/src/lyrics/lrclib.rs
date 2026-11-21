//! lrclib.net: community lyrics, synced where someone timed them. No auth, one GET.

use crate::model::{LyricLine, Lyrics, Track};

/// LRCLIB's lyrics for a track: synced when it has them, else its plain text. `None` on any
/// miss.
pub async fn lyrics(http: &reqwest::Client, track: &Track) -> Option<Lyrics> {
    let artist = track.artist_line();
    let duration = track.duration.map(|d| d.to_string());
    let mut query: Vec<(&str, &str)> = vec![
        ("track_name", track.title.as_str()),
        ("artist_name", artist.as_str()),
    ];
    if let Some(album) = track.album.as_deref() {
        query.push(("album_name", album));
    }
    if let Some(duration) = duration.as_deref() {
        query.push(("duration", duration));
    }
    let response = http
        .get("https://lrclib.net/api/get")
        .query(&query)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().await.ok()?;
    if let Some(lrc) = body.get("syncedLyrics").and_then(serde_json::Value::as_str) {
        let lines = parse_lrc(lrc);
        if !lines.is_empty() {
            return Some(Lyrics {
                lines,
                synced: true,
                source: "LRCLIB".to_owned(),
            });
        }
    }
    let plain = body
        .get("plainLyrics")
        .and_then(serde_json::Value::as_str)?;
    Some(Lyrics {
        lines: plain_lines(plain),
        synced: false,
        source: "LRCLIB".to_owned(),
    })
}

/// Plain text as lines, each at zero.
pub fn plain_lines(text: &str) -> Vec<LyricLine> {
    text.lines()
        .map(|l| LyricLine {
            at_ms: 0,
            text: l.trim_end().to_owned(),
            romanised: None,
        })
        .collect()
}

/// Parse an LRC body (`[mm:ss.xx] line`) into sorted, timestamped lines. Malformed stamps are
/// skipped rather than failing the whole set.
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        let Some(close) = line.find(']') else {
            continue;
        };
        let Some(at_ms) = lrc_time_ms(&line[1..close]) else {
            continue;
        };
        out.push(LyricLine {
            at_ms,
            text: without_word_stamps(&line[close + 1..]),
            romanised: None,
        });
    }
    out.sort_by_key(|l| l.at_ms);
    out
}

/// A line's text with enhanced LRC's word stamps (`<00:01.40>`) taken out, since lines light
/// up whole.
fn without_word_stamps(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        out.push_str(&rest[..lt]);
        if lrc_time_ms(&rest[lt + 1..lt + gt]).is_none() {
            out.push_str(&rest[lt..=lt + gt]);
        }
        rest = &rest[lt + gt + 1..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `mm:ss.xx` (or `.x`, `.xxx`) to milliseconds. The fraction's digits count as written, so
/// `.05` is 50 ms, not 500.
fn lrc_time_ms(stamp: &str) -> Option<u64> {
    let (mm, rest) = stamp.split_once(':')?;
    let mm: u64 = mm.trim().parse().ok()?;
    let (ss, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let ss: u64 = ss.trim().parse().ok()?;
    let digits: String = frac.trim().chars().take(3).collect();
    let ms = if digits.is_empty() {
        0
    } else {
        let value: u64 = digits.parse().ok()?;
        value * 10u64.pow(3 - digits.len() as u32)
    };
    Some(mm * 60_000 + ss * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LRC stamps: the fraction counts as many digits as it has, and a malformed stamp is
    /// skipped rather than dropping the whole lyric set.
    #[test]
    fn lrc_times_parse_in_every_fraction_width() {
        assert_eq!(lrc_time_ms("00:12.34"), Some(12_340));
        assert_eq!(lrc_time_ms("01:02.345"), Some(62_345));
        assert_eq!(lrc_time_ms("02:00"), Some(120_000));
        assert_eq!(lrc_time_ms("00:01.5"), Some(1_500));
        // A leading zero used to read as 500 ms.
        assert_eq!(lrc_time_ms("00:01.05"), Some(1_050));
        assert_eq!(lrc_time_ms("00:01.005"), Some(1_005));
        assert_eq!(lrc_time_ms("not a stamp"), None);
    }

    #[test]
    fn lrc_lines_are_sorted_and_timed() {
        let lines = parse_lrc("[00:20.00] second\nnoise\n[00:10.50] first\n");
        assert_eq!(lines.len(), 2, "the header-less line is skipped");
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[0].at_ms, 10_500);
        assert_eq!(lines[1].text, "second");
    }
}
