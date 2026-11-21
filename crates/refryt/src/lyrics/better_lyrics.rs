//! BetterLyrics: Apple Music's timed lyrics, through the API behind the BetterLyrics browser
//! extension.
//!
//! One GET keyed on the title, artist, length and album, answering `{"ttml": …}` with Apple's
//! TTML: a `<p begin=… end=…>` per line, word `<span>`s inside, background vocals in a
//! `<span ttm:role="x-bg">`. Only songs its cache already holds are answered: since late 2026
//! anything else gets 401 "API key required", which counts as a miss here.
//!
//! Lines only for now: the words' own timing is read past, and each line lights up as a whole.

use anyhow::{Context, Result};

use crate::model::{LyricLine, Lyrics, Track};

const ENDPOINT: &str = "https://lyrics-api.boidu.dev/getLyrics";

pub async fn lyrics(http: &reqwest::Client, track: &Track) -> Result<Option<Lyrics>> {
    let artist = track.artist_line();
    let mut query: Vec<(&str, String)> = vec![("s", track.title.clone()), ("a", artist)];
    if let Some(seconds) = track.duration.filter(|d| *d > 0) {
        query.push(("d", seconds.to_string()));
    }
    if let Some(album) = track.album.as_deref().filter(|a| !a.is_empty()) {
        query.push(("al", album.to_owned()));
    }
    let response = http
        .get(ENDPOINT)
        .query(&query)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .context("BetterLyrics")?;
    // Not in its cache (401), or no lyrics at all (404): a miss, not a failure.
    if matches!(response.status().as_u16(), 401 | 404) {
        return Ok(None);
    }
    let body: serde_json::Value = response
        .error_for_status()
        .context("BetterLyrics")?
        .json()
        .await
        .context("BetterLyrics")?;
    let Some(ttml) = body.get("ttml").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    Ok(parse_ttml(ttml))
}

/// Apple's TTML as lines. `itunes:timing="None"` (or a line with no `begin`) is plain text.
pub fn parse_ttml(ttml: &str) -> Option<Lyrics> {
    let doc = roxmltree::Document::parse(ttml).ok()?;
    let root = doc.root_element();
    let untimed = root
        .attributes()
        .any(|a| a.name() == "timing" && a.value().eq_ignore_ascii_case("none"));
    let body = root.children().find(|n| n.has_tag_name("body"))?;
    let romanised = transliterations(&doc);
    let mut lines = Vec::new();
    let mut synced = !untimed;
    for p in body.descendants().filter(|n| n.has_tag_name("p")) {
        let text = collapse(
            &p.descendants()
                .filter(|n| n.is_text())
                .filter_map(|n| n.text())
                .collect::<String>(),
        );
        let at_ms = p.attribute("begin").and_then(ttml_time_ms);
        if at_ms.is_none() {
            synced = false;
        }
        // Each line names its key (`itunes:key` or `lrc:key`, "L12"), which is how its
        // romanised text is filed. A romanisation that only repeats the line (an English line
        // in a Korean song) is left out.
        let romanised = p
            .attributes()
            .find(|a| a.name() == "key")
            .and_then(|key| romanised.get(key.value()))
            .filter(|r| !r.eq_ignore_ascii_case(&text))
            .cloned();
        lines.push(LyricLine {
            at_ms: at_ms.unwrap_or(0),
            text,
            romanised,
        });
    }
    if lines.is_empty() {
        return None;
    }
    if synced {
        lines.sort_by_key(|l| l.at_ms);
    } else {
        lines.iter_mut().for_each(|l| l.at_ms = 0);
    }
    Some(Lyrics {
        lines,
        synced,
        source: "Apple Music".to_owned(),
    })
}

/// Apple's romanised lines, by line key: `<transliteration xml:lang="ja-Latn">` in the head,
/// with a `<text for="L1">` per line (per-word spans inside, read as one line). The first
/// transliteration in Latin letters, when a song has several.
fn transliterations(doc: &roxmltree::Document) -> std::collections::HashMap<String, String> {
    let latin = |n: &roxmltree::Node| {
        n.attributes()
            .find(|a| a.name() == "lang")
            .is_some_and(|a| a.value().contains("Latn"))
    };
    let mut all = doc
        .descendants()
        .filter(|n| n.has_tag_name("transliteration"));
    let Some(chosen) = all.clone().find(latin).or_else(|| all.next()) else {
        return Default::default();
    };
    chosen
        .children()
        .filter(|n| n.has_tag_name("text"))
        .filter_map(|t| {
            let key = t.attribute("for")?;
            let text = collapse(
                &t.descendants()
                    .filter(|n| n.is_text())
                    .filter_map(|n| n.text())
                    .collect::<String>(),
            );
            (!text.is_empty()).then(|| (key.to_owned(), text))
        })
        .collect()
}

/// Runs of whitespace, including the line breaks between spans, as one space.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A TTML clock value to milliseconds: `ss.fff`, `m:ss.fff`, `h:mm:ss.fff`, or with an `s` or
/// `ms` unit.
fn ttml_time_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(ms) = value.strip_suffix("ms") {
        return ms.trim().parse::<f64>().ok().map(|ms| ms.round() as u64);
    }
    let value = value.strip_suffix('s').unwrap_or(value);
    let mut seconds = 0.0;
    for part in value.split(':') {
        let n: f64 = part.trim().parse().ok()?;
        seconds = seconds * 60.0 + n;
    }
    (seconds >= 0.0).then(|| (seconds * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" itunes:timing="Word" xml:lang="en"><head><metadata><ttm:agent type="person" xml:id="v1"/></metadata></head><body dur="3:21.000"><div begin="0:01.000" end="3:21.000"><p begin="2:22.475" end="2:26.452"><span begin="2:22.475" end="2:23.000">Second</span> <span begin="2:23.000" end="2:24.000">line</span></p><p begin="2:20.059" end="2:22.261"><span begin="2:20.059" end="2:20.353">calling</span> <span begin="2:20.353" end="2:20.726">back</span> <span ttm:role="x-bg"><span begin="2:21.720" end="2:22.154">(Back</span> <span begin="2:22.154" end="2:22.295">again)</span></span></p></div></body></tt>"#;

    #[test]
    fn apple_ttml_reads_as_timed_lines() {
        let lyrics = parse_ttml(SAMPLE).unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].at_ms, 140_059, "sorted by start");
        assert_eq!(lyrics.lines[0].text, "calling back (Back again)");
        assert_eq!(lyrics.lines[1].text, "Second line");
    }

    /// Apple's romaji for a Japanese line sits under it; a line whose romanisation only repeats
    /// it gets none.
    #[test]
    fn apple_romanisations_attach_to_their_lines() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:lrc="http://lrc.red" itunes:timing="Word"><head><metadata><iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal"><transliterations><transliteration xml:lang="ja-Latn"><text for="L1"><span begin="0.789" end="1.254" xmlns="http://www.w3.org/ns/ttml">shizu</span><span begin="1.254" end="1.690" xmlns="http://www.w3.org/ns/ttml">mu</span> <span begin="1.690" end="2.430" xmlns="http://www.w3.org/ns/ttml">you ni</span></text><text for="L2">Hello</text></transliteration></transliterations></iTunesMetadata></metadata></head><body><div><p begin="0.789" end="2.430" lrc:key="L1"><span begin="0.789" end="1.254">沈</span><span begin="1.254" end="1.690">む</span><span begin="1.690" end="2.430">ように</span></p><p begin="3.0" end="4.0" itunes:key="L2">Hello</p><p begin="5.0" end="6.0" itunes:key="L3">終わり</p></div></body></tt>"#;
        let lyrics = parse_ttml(ttml).unwrap();
        assert_eq!(lyrics.lines[0].text, "沈むように");
        assert_eq!(lyrics.lines[0].romanised.as_deref(), Some("shizumu you ni"));
        assert_eq!(lyrics.lines[1].romanised, None, "only repeats the line");
        assert_eq!(lyrics.lines[2].romanised, None, "none given");
    }

    #[test]
    fn untimed_ttml_is_plain_text() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="None"><body><div><p>One</p><p>Two</p></div></body></tt>"#;
        let lyrics = parse_ttml(ttml).unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines[1].text, "Two");
        assert!(lyrics.lines.iter().all(|l| l.at_ms == 0));
    }

    #[test]
    fn ttml_clock_values_in_every_form() {
        assert_eq!(ttml_time_ms("12.5"), Some(12_500));
        assert_eq!(ttml_time_ms("2:20.059"), Some(140_059));
        assert_eq!(ttml_time_ms("1:02:03.004"), Some(3_723_004));
        assert_eq!(ttml_time_ms("1.25s"), Some(1_250));
        assert_eq!(ttml_time_ms("1250ms"), Some(1_250));
        assert_eq!(ttml_time_ms("soon"), None);
    }

    #[test]
    fn broken_xml_is_none() {
        assert!(parse_ttml("<tt><body><p>").is_none());
    }
}
