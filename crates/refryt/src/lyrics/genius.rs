//! Genius: plain text, no timing, but the largest catalogue there is. Last in the default
//! order, and only shown when no source had timed lyrics.
//!
//! Its site search (`api/search/multi`) finds the song page; the lyrics are the page's
//! `data-lyrics-container` blocks, with `<br>` as line breaks. The page is read without an HTML
//! library: only those blocks are taken, and inside them only text and line breaks are kept.
//! Section headers ("[Chorus]") are dropped, since no other source has them.

use anyhow::{Context, Result};

use crate::model::{Lyrics, Track};

pub async fn lyrics(http: &reqwest::Client, track: &Track) -> Result<Option<Lyrics>> {
    let title = super::title_core_display(&track.title);
    let artist = track.artist_line();
    let found: serde_json::Value = http
        .get("https://genius.com/api/search/multi")
        .query(&[("q", format!("{artist} {title}").trim())])
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .context("Genius search")?
        .error_for_status()
        .context("Genius search")?
        .json()
        .await
        .context("Genius search")?;
    let Some(url) = best_url(&found, &title, &artist) else {
        return Ok(None);
    };
    let page = http
        .get(url)
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .timeout(super::SOURCE_TIMEOUT)
        .send()
        .await
        .context("Genius page")?
        .error_for_status()
        .context("Genius page")?
        .text()
        .await
        .context("Genius page")?;
    let lines = lyric_lines(&page);
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(Lyrics {
        lines: lines
            .into_iter()
            .map(|text| crate::model::LyricLine {
                at_ms: 0,
                text,
                romanised: None,
            })
            .collect(),
        synced: false,
        source: "Genius".to_owned(),
    }))
}

/// The song hit whose title and artist both match; a translation or romanisation page loses
/// to the original. Only genius.com song pages are followed.
fn best_url(found: &serde_json::Value, title: &str, artist: &str) -> Option<String> {
    let want_title = super::normalised(title);
    let want_artists = super::artist_names(artist);
    let sections = found.pointer("/response/sections")?.as_array()?;
    let hits = sections
        .iter()
        .find(|s| s.get("type").and_then(|t| t.as_str()) == Some("song"))?
        .get("hits")?
        .as_array()?;
    hits.iter()
        .filter_map(|h| h.get("result"))
        .filter_map(|r| {
            let url = r.get("url")?.as_str()?;
            if !url.starts_with("https://genius.com/") {
                return None;
            }
            let name = super::normalised(&super::title_core_display(r.get("title")?.as_str()?));
            let by = r
                .get("artist_names")
                .and_then(|a| a.as_str())
                .unwrap_or_default();
            let artist_ok = want_artists.is_empty()
                || super::artist_names(by)
                    .iter()
                    .any(|a| want_artists.contains(a));
            if name != want_title || !artist_ok {
                return None;
            }
            let path = url.to_ascii_lowercase();
            let side_page = ["translation", "romanized", "annotated", "tracklist"]
                .iter()
                .any(|w| path.contains(w));
            Some((side_page, url.to_owned()))
        })
        .min_by_key(|(side_page, _)| *side_page)
        .map(|(_, url)| url)
}

/// The text of every `data-lyrics-container="true"` block, one entry per line, with headers,
/// blank runs and Genius's own furniture taken out.
fn lyric_lines(html: &str) -> Vec<String> {
    let mut text = String::new();
    let mut rest = html;
    while let Some(at) = rest.find("data-lyrics-container=\"true\"") {
        let Some(open_end) = rest[at..].find('>') else {
            break;
        };
        let body = &rest[at + open_end + 1..];
        let (inner, after) = until_close(body);
        text.push_str(&block_text(inner));
        text.push('\n');
        rest = after;
    }
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let header = line.starts_with('[') && line.ends_with(']');
        if header {
            continue;
        }
        if line.is_empty() {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        lines.push(line.to_owned());
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Split at the `</div>` that closes the block `body` starts inside.
fn until_close(body: &str) -> (&str, &str) {
    until_close_of(body, "div")
}

/// Split at the close tag of the `name` element `body` starts inside: only `<name` and
/// `</name` are counted, so other elements left unclosed don't throw it off.
fn until_close_of<'a>(body: &'a str, name: &str) -> (&'a str, &'a str) {
    let open = format!("<{name}");
    let close = format!("</{name}");
    let mut depth = 1usize;
    let mut i = 0;
    while let Some(lt) = body[i..].find('<') {
        let tag = &body[i + lt..];
        if tag.starts_with(&close) {
            depth -= 1;
            if depth == 0 {
                return (&body[..i + lt], &body[i + lt..]);
            }
        } else if tag.starts_with(&open)
            && tag[open.len()..].starts_with(|c: char| c == '>' || c == '/' || c.is_whitespace())
            && !tag.split('>').next().unwrap_or_default().ends_with('/')
        {
            depth += 1;
        }
        i += lt + 1;
    }
    (body, "")
}

/// A block's text: `<br>` as a line break, any element marked
/// `data-exclude-from-selection="true"` (Genius's "N Contributors" header and the like)
/// skipped whole, other tags removed, entities decoded.
fn block_text(block: &str) -> String {
    let mut out = String::new();
    let mut rest = block;
    while let Some(lt) = rest.find('<') {
        out.push_str(&decode(&rest[..lt]));
        let Some(gt) = rest[lt..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if name == "br" {
            out.push('\n');
        } else if !name.is_empty()
            && !tag.ends_with('/')
            && tag.contains("data-exclude-from-selection=\"true\"")
        {
            let (_, after) = until_close_of(rest, &name);
            rest = after.split_once('>').map_or("", |(_, r)| r);
        }
    }
    out.push_str(&decode(rest));
    out
}

/// The entities Genius pages use: the named five, `&nbsp;`, and numeric ones.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';').filter(|s| *s <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out.replace(['\u{200B}', '\u{FEFF}'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyrics_come_out_of_the_containers_without_headers_or_furniture() {
        let html = r#"<html><div class="x">Not lyrics</div>
            <div data-lyrics-container="true" class="Lyrics"><div data-exclude-from-selection="true"><span>12 Contributors</span><div>Blinding Lights Lyrics</div></div>[Intro]<br/>Yeah<br><br>[Verse 1]<br><a href="/1"><span>I&#x27;ve been tryna call</span></a><br>I&#039;ve been on my own &amp; long enough</div>
            <div>between</div>
            <div data-lyrics-container="true">Maybe you can show me how to love, maybe<br></div></html>"#;
        assert_eq!(
            lyric_lines(html),
            [
                "Yeah",
                "",
                "I've been tryna call",
                "I've been on my own & long enough",
                "Maybe you can show me how to love, maybe",
            ]
        );
    }

    #[test]
    fn the_original_page_wins_over_a_translation() {
        let found = serde_json::json!({"response": {"sections": [
            {"type": "top_hit", "hits": []},
            {"type": "song", "hits": [
                {"result": {"title": "Glue (Traducción al Español)", "artist_names": "Genius Traducciones al Español", "url": "https://genius.com/x-translation"}},
                {"result": {"title": "Glue", "artist_names": "Bicep", "url": "https://genius.com/Bicep-glue-lyrics"}},
                {"result": {"title": "Glue", "artist_names": "Bicep", "url": "https://evil.example/Bicep-glue-lyrics"}}
            ]}
        ]}});
        assert_eq!(
            best_url(&found, "Glue", "Bicep").as_deref(),
            Some("https://genius.com/Bicep-glue-lyrics")
        );
        assert_eq!(best_url(&found, "Glue", "Someone Else"), None);
    }
}
