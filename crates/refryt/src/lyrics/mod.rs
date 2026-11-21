//! Where lyrics come from: several sources, asked together, the best answer shown.
//!
//! Every source the listener has ticked in Settings is asked at the same time, and the answers
//! are taken in the listener's order: the first source with *synced* lyrics wins, and plain
//! text is shown only when no source had timing. A source that is ticked off is never
//! contacted. The lyrics panel lets the listener switch to another source's answer for the
//! song that is playing.
//!
//! The approach follows BitChord's (github.com/kushagrasinghx/BitChord, GPL-3.0 like refryt):
//! an ordered, tickable list of sources, all asked at once and read in order, with a manual
//! pick per song.

pub mod better_lyrics;
pub mod genius;
pub mod kugou;
pub mod lrc_red;
pub mod lrclib;
pub mod unison;
pub mod youtube;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::Lyrics;

/// How long one source gets to answer before it counts as having nothing. Long enough for a
/// slow host, short enough that a source that is down doesn't hold the panel's skeleton for long:
/// a higher source still pending is waited on before a lower one's answer is shown.
pub const SOURCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// What the lyrics hosts are told the client is: the app, never the listener.
pub const USER_AGENT: &str = concat!(
    "refryt/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/gridoe/refryt)"
);

/// A place lyrics can come from. The order here is the default order they are asked in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Apple Music's timing through the BetterLyrics extension's API. Only songs its cache
    /// already holds: anything else it now answers with "API key required".
    BetterLyrics,
    /// lrc.red: the same Apple Music TTML, filed by recording, for any song it indexed.
    LrcRed,
    /// YouTube Music's own timed lyrics (from Musixmatch or LyricFind), as its Android app
    /// gets them, matched on the exact video.
    YouTubeSynced,
    /// lrclib.net's community lyrics, matched on the title, artist, album and length.
    Lrclib,
    /// KuGou's line-timed lyrics, strong outside the English catalogue.
    KuGou,
    /// Unison: lyrics contributed by listeners, from the BetterLyrics project.
    Unison,
    /// YouTube Music's lyrics as the website shows them: plain text, no timing.
    YouTube,
    /// Genius: plain text from the biggest catalogue there is.
    Genius,
}

impl Source {
    /// Every source, in the default order.
    pub const ALL: [Source; 8] = [
        Source::BetterLyrics,
        Source::LrcRed,
        Source::YouTubeSynced,
        Source::Lrclib,
        Source::KuGou,
        Source::Unison,
        Source::YouTube,
        Source::Genius,
    ];

    /// The name kept in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Source::BetterLyrics => "better_lyrics",
            Source::YouTubeSynced => "youtube_synced",
            Source::Lrclib => "lrclib",
            Source::KuGou => "kugou",
            Source::Unison => "unison",
            Source::YouTube => "youtube",
            Source::Genius => "genius",
            Source::LrcRed => "lrc_red",
        }
    }

    pub fn from_key(key: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::BetterLyrics => "BetterLyrics",
            Source::YouTubeSynced => "YouTube Music, synced",
            Source::Lrclib => "LRCLIB",
            Source::KuGou => "KuGou",
            Source::Unison => "Unison",
            Source::YouTube => "YouTube Music, plain",
            Source::Genius => "Genius",
            Source::LrcRed => "lrc.red",
        }
    }

    /// The name in the lyrics panel's picker, where there's little room.
    pub fn short_label(self) -> &'static str {
        match self {
            Source::BetterLyrics => "BetterLyrics",
            Source::YouTubeSynced => "YouTube synced",
            Source::Lrclib => "LRCLIB",
            Source::KuGou => "KuGou",
            Source::Unison => "Unison",
            Source::YouTube => "YouTube plain",
            Source::Genius => "Genius",
            Source::LrcRed => "lrc.red",
        }
    }

    /// One line on what it is and what it is sent, for Settings.
    pub fn detail(self) -> &'static str {
        match self {
            Source::BetterLyrics => {
                "Apple Music's timing, for songs it already has. Gets the title, artist, album \
                 and length."
            }
            Source::YouTubeSynced => {
                "YouTube's own timed lyrics, matched to the exact song. Gets the video id."
            }
            Source::Lrclib => {
                "Community lyrics from lrclib.net. Gets the title, artist, album and length."
            }
            Source::YouTube => "YouTube's lyrics without timing. Gets the video id.",
            Source::LrcRed => {
                "Apple Music's timing for any song it has indexed, not only cached ones. Gets \
                 the title, artist and length."
            }
            Source::KuGou => {
                "Timed lyrics from a large Chinese music service, good outside English. Gets \
                 the title, artist, album and length."
            }
            Source::Unison => {
                "Lyrics added by listeners, for songs nobody licenses. Gets the title, artist, \
                 album and length."
            }
            Source::Genius => {
                "Plain text, no timing, from the biggest catalogue. Gets the title and artist."
            }
        }
    }
}

/// A source and whether it is asked, as the settings file keeps it: by name, so the list can
/// gain a source, or be reordered in code, without disturbing an order someone chose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceChoice {
    pub source: String,
    pub on: bool,
}

/// The default list: every source, in [`Source::ALL`]'s order, all on.
pub fn default_choices() -> Vec<SourceChoice> {
    to_choices(&Source::ALL.map(|s| (s, true)))
}

pub fn to_choices(list: &[(Source, bool)]) -> Vec<SourceChoice> {
    list.iter()
        .map(|(s, on)| SourceChoice {
            source: s.key().to_owned(),
            on: *on,
        })
        .collect()
}

/// A saved list brought up to date with this build: names this build doesn't know dropped,
/// repeats dropped, and a source added since the list was saved put in right after the source
/// it follows in [`Source::ALL`] (at the top if none of those is in the list), switched on. An
/// empty list is the default one.
pub fn normalise(saved: &[SourceChoice]) -> Vec<(Source, bool)> {
    let mut out: Vec<(Source, bool)> = Vec::new();
    for choice in saved {
        if let Some(source) = Source::from_key(&choice.source)
            && !out.iter().any(|(s, _)| *s == source)
        {
            out.push((source, choice.on));
        }
    }
    for (index, source) in Source::ALL.into_iter().enumerate() {
        if out.iter().any(|(s, _)| *s == source) {
            continue;
        }
        let after = Source::ALL[..index]
            .iter()
            .rev()
            .find_map(|prev| out.iter().position(|(s, _)| s == prev));
        out.insert(after.map_or(0, |i| i + 1), (source, true));
    }
    out
}

/// The sources to ask, in order: the ticked ones.
pub fn asked(saved: &[SourceChoice]) -> Vec<Source> {
    normalise(saved)
        .into_iter()
        .filter_map(|(s, on)| on.then_some(s))
        .collect()
}

/// What one source has said about the song so far.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Pending,
    Found(Lyrics),
    /// Nothing for this song, or it could not be reached in time.
    Missing,
}

/// Which answer to show, from what the sources have said so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// A source ahead of every synced answer is still out: its answer may beat them.
    Wait,
    Use(Source),
    /// Every source has answered and none had lyrics.
    Nothing,
}

/// The automatic pick. Going down `order`: a source still out means waiting for it; the first
/// with synced lyrics wins; plain text counts only once every source is in and none had timing,
/// and then the first plain answer is used.
pub fn pick(order: &[Source], answers: &HashMap<Source, Answer>) -> Pick {
    let mut plain = None;
    for source in order {
        match answers.get(source) {
            None | Some(Answer::Pending) => return Pick::Wait,
            Some(Answer::Found(lyrics)) if lyrics.synced => return Pick::Use(*source),
            Some(Answer::Found(_)) => {
                plain.get_or_insert(*source);
            }
            Some(Answer::Missing) => {}
        }
    }
    plain.map_or(Pick::Nothing, Pick::Use)
}

/// Ask one source for `track`'s lyrics. `Ok(None)` is a plain miss; an error is a source that
/// could not be reached or answered something unreadable, which counts as a miss too.
pub async fn fetch(
    source: Source,
    http: &reqwest::Client,
    api: &crate::api::Api,
    track: &crate::model::Track,
) -> anyhow::Result<Option<Lyrics>> {
    match source {
        Source::BetterLyrics => better_lyrics::lyrics(http, track).await,
        Source::YouTubeSynced => youtube::synced(http, &track.video_id).await,
        Source::Lrclib => Ok(lrclib::lyrics(http, track).await),
        Source::YouTube => api.lyrics(&track.video_id).await,
        Source::LrcRed => lrc_red::lyrics(http, track).await,
        Source::KuGou => kugou::lyrics(http, track).await,
        Source::Unison => unison::lyrics(http, track).await,
        Source::Genius => genius::lyrics(http, track).await,
    }
}

// Matching a search hit to the song, for the sources that search by name.

/// Lowercase, everything but letters and digits as spaces, runs of spaces as one.
pub(crate) fn normalised(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The title with bracketed parts and anything after " - " set aside, as written: "Glue (Edit)"
/// and "Glue - 2017 Remaster" are both "Glue". The whole title if that leaves nothing.
pub(crate) fn title_core_display(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for c in title.chars() {
        match c {
            '(' | '[' | '（' => depth += 1,
            ')' | ']' | '）' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out
        .split(" - ")
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if out.is_empty() {
        title.trim().to_owned()
    } else {
        out
    }
}

/// [`title_core_display`], normalised for comparing.
pub(crate) fn title_core(title: &str) -> String {
    normalised(&title_core_display(title))
}

/// The words in a title's brackets or after its " - " that make it a different recording:
/// "live", "remix", "acoustic"… Two titles are the same version when these match.
pub(crate) fn version_words(title: &str) -> std::collections::BTreeSet<String> {
    const WORDS: [&str; 21] = [
        "live",
        "remix",
        "remixed",
        "mix",
        "acoustic",
        "unplugged",
        "instrumental",
        "karaoke",
        "cappella",
        "acapella",
        "demo",
        "edit",
        "version",
        "cover",
        "sped",
        "slowed",
        "reverb",
        "nightcore",
        "lofi",
        "orchestral",
        "extended",
    ];
    let core = title_core_display(title);
    let extra = title.replacen(core.as_str(), " ", 1);
    normalised(&extra)
        .split(' ')
        .filter(|w| WORDS.contains(w))
        .map(str::to_owned)
        .collect()
}

/// The artists in an artist line, normalised: "A & B feat. C" is {a, b, c}.
pub(crate) fn artist_names(artist: &str) -> Vec<String> {
    let mut line = format!(" {} ", artist.to_lowercase());
    for joiner in [
        " and ",
        " x ",
        " with ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
    ] {
        line = line.replace(joiner, ",");
    }
    line.split([',', '&', ';', '/', '、'])
        .map(normalised)
        .filter(|a| !a.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LyricLine;

    fn lyrics(synced: bool) -> Lyrics {
        Lyrics {
            lines: vec![LyricLine {
                at_ms: 0,
                text: "la".to_owned(),
            }],
            synced,
            source: String::new(),
        }
    }

    fn answers(list: &[(Source, Answer)]) -> HashMap<Source, Answer> {
        list.iter().cloned().collect()
    }

    use Source::*;

    #[test]
    fn the_first_synced_answer_in_order_wins() {
        let order = [BetterLyrics, YouTubeSynced, Lrclib];
        let a = answers(&[
            (BetterLyrics, Answer::Missing),
            (YouTubeSynced, Answer::Found(lyrics(true))),
            (Lrclib, Answer::Found(lyrics(true))),
        ]);
        assert_eq!(pick(&order, &a), Pick::Use(YouTubeSynced));
    }

    /// A lower source answering first doesn't jump ahead of one still out above it.
    #[test]
    fn a_pending_source_above_is_waited_for() {
        let order = [BetterLyrics, Lrclib];
        let a = answers(&[
            (BetterLyrics, Answer::Pending),
            (Lrclib, Answer::Found(lyrics(true))),
        ]);
        assert_eq!(pick(&order, &a), Pick::Wait);
    }

    #[test]
    fn plain_text_waits_for_every_source_and_loses_to_timing() {
        let order = [YouTube, Lrclib];
        let mut a = answers(&[
            (YouTube, Answer::Found(lyrics(false))),
            (Lrclib, Answer::Pending),
        ]);
        assert_eq!(pick(&order, &a), Pick::Wait);
        a.insert(Lrclib, Answer::Found(lyrics(true)));
        assert_eq!(pick(&order, &a), Pick::Use(Lrclib));
        a.insert(Lrclib, Answer::Missing);
        assert_eq!(pick(&order, &a), Pick::Use(YouTube));
    }

    #[test]
    fn nothing_anywhere_is_nothing() {
        let order = [YouTube, Lrclib];
        let a = answers(&[(YouTube, Answer::Missing), (Lrclib, Answer::Missing)]);
        assert_eq!(pick(&order, &a), Pick::Nothing);
        assert_eq!(pick(&[], &a), Pick::Nothing);
    }

    #[test]
    fn an_empty_list_is_the_default_order_all_on() {
        assert_eq!(normalise(&[]), Source::ALL.map(|s| (s, true)).to_vec(),);
        assert_eq!(asked(&default_choices()), Source::ALL.to_vec());
    }

    /// A saved order is kept; unknown names and repeats go; a source it never named lands
    /// right after its neighbour in the default order, switched on.
    #[test]
    fn a_saved_list_keeps_its_order_and_gains_new_sources_in_place() {
        let saved = vec![
            SourceChoice {
                source: "lrclib".into(),
                on: false,
            },
            SourceChoice {
                source: "something_gone".into(),
                on: true,
            },
            SourceChoice {
                source: "youtube".into(),
                on: true,
            },
            SourceChoice {
                source: "lrclib".into(),
                on: true,
            },
        ];
        // BetterLyrics follows nothing, so it goes on top; each other new one follows its
        // neighbour in the default order.
        assert_eq!(
            normalise(&saved),
            vec![
                (BetterLyrics, true),
                (LrcRed, true),
                (YouTubeSynced, true),
                (Lrclib, false),
                (KuGou, true),
                (Unison, true),
                (YouTube, true),
                (Genius, true),
            ]
        );
        assert_eq!(
            asked(&saved),
            vec![
                BetterLyrics,
                LrcRed,
                YouTubeSynced,
                KuGou,
                Unison,
                YouTube,
                Genius
            ]
        );
    }

    #[test]
    fn titles_and_artists_match_across_spellings() {
        assert_eq!(title_core("Glue (Edit)"), "glue");
        assert_eq!(title_core("Glue - 2017 Remaster"), "glue");
        assert_eq!(title_core("(Intro)"), "intro");
        assert!(version_words("Glue").is_empty());
        assert_eq!(
            version_words("Blinding Lights (Live) - Acoustic")
                .into_iter()
                .collect::<Vec<_>>(),
            ["acoustic", "live"]
        );
        assert_eq!(
            artist_names("The Weeknd & Rosalía feat. Someone"),
            ["the weeknd", "rosalía", "someone"]
        );
        assert_eq!(artist_names("Simon and Garfunkel"), ["simon", "garfunkel"]);
    }

    #[test]
    fn keys_round_trip() {
        for s in Source::ALL {
            assert_eq!(Source::from_key(s.key()), Some(s));
        }
    }
}
