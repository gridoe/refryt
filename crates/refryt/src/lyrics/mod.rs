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
///
/// With `romanise` on, synced lyrics in Japanese, Korean or Chinese that have no romanisation
/// are only a stand-in: shown at once, but the first source further down that has a
/// romanisation takes over when it answers.
pub fn pick(order: &[Source], answers: &HashMap<Source, Answer>, romanise: bool) -> Pick {
    let mut plain = None;
    let mut stand_in = None;
    for source in order {
        match answers.get(source) {
            None | Some(Answer::Pending) => {
                return stand_in.map_or(Pick::Wait, Pick::Use);
            }
            Some(Answer::Found(lyrics)) if lyrics.synced => {
                let wanting = romanise && needs_romanising(lyrics) && !has_romanised(lyrics);
                match (wanting, stand_in) {
                    (false, _) if stand_in.is_none() || has_romanised(lyrics) => {
                        return Pick::Use(*source);
                    }
                    (true, None) => stand_in = Some(*source),
                    _ => {}
                }
            }
            Some(Answer::Found(_)) => {
                plain.get_or_insert(*source);
            }
            Some(Answer::Missing) => {}
        }
    }
    stand_in.or(plain).map_or(Pick::Nothing, Pick::Use)
}

/// Whether a line is in a script romanisation is for: kana, CJK ideographs, Hangul.
pub fn in_romanisable_script(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c,
            '\u{3040}'..='\u{30ff}'   // hiragana, katakana
            | '\u{31f0}'..='\u{31ff}' // katakana extensions
            | '\u{ff66}'..='\u{ff9f}' // half-width katakana
            | '\u{3400}'..='\u{4dbf}' // CJK extension A
            | '\u{4e00}'..='\u{9fff}' // CJK ideographs
            | '\u{f900}'..='\u{faff}' // CJK compatibility
            | '\u{1100}'..='\u{11ff}' // Hangul jamo
            | '\u{3130}'..='\u{318f}' // Hangul compatibility jamo
            | '\u{ac00}'..='\u{d7af}' // Hangul syllables
        )
    })
}

/// Lyrics with lines romanisation is for.
pub fn needs_romanising(lyrics: &Lyrics) -> bool {
    lyrics.lines.iter().any(|l| in_romanisable_script(&l.text))
}

pub fn has_romanised(lyrics: &Lyrics) -> bool {
    lyrics.lines.iter().any(|l| l.romanised.is_some())
}

fn has_kana(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{ff66}'..='\u{ff9f}'))
}

/// The words of Latin text and how many end the way Japanese syllables do, in a vowel or `n`.
/// Romaji runs well over 70% of them, English nearer 30–50%. Words in brackets are left out:
/// backing vocals like "(Hey! Hey!)" are the same in either.
fn japanese_endings(text: &str) -> (usize, usize) {
    let mut outside = String::new();
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '(' | '[' | '（' => depth += 1,
            ')' | ']' | '）' if depth > 0 => depth -= 1,
            _ if depth == 0 => outside.push(c),
            _ => {}
        }
    }
    let words = outside
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase);
    let (mut all, mut japanese) = (0, 0);
    for w in words {
        all += 1;
        if w.ends_with([
            'a', 'e', 'i', 'o', 'u', 'n', 'ā', 'ē', 'ī', 'ō', 'ū', 'â', 'ô', 'û',
        ]) {
            japanese += 1;
        }
    }
    (all, japanese)
}

/// Romaji a lyrics file carries as lines of its own. Some LRC files hold the song in Japanese
/// and again in romaji under the same timestamps, so each Japanese line has a twin at the same
/// moment. Those twins become the line's romanisation, and a line both halves share (an English
/// line) is kept once.
///
/// Judged over the whole file, not line by line: the twins must read as romaji taken together,
/// so an English translation filed the same way stays as lines, and most Japanese lines must
/// have one, so a stray repeated stamp changes nothing.
pub fn pair_romaji(lyrics: &mut Lyrics) {
    if !lyrics.synced || has_romanised(lyrics) || !lyrics.lines.iter().any(|l| has_kana(&l.text)) {
        return;
    }
    let lines = &lyrics.lines;
    let japanese = lines
        .iter()
        .filter(|l| in_romanisable_script(&l.text))
        .count();
    let mut out = Vec::with_capacity(lines.len());
    let (mut twins, mut words, mut endings) = (0, 0, 0);
    let mut i = 0;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && lines[j].at_ms == lines[i].at_ms {
            j += 1;
        }
        if let [a, b] = &lines[i..j] {
            let twin = match (
                in_romanisable_script(&a.text),
                in_romanisable_script(&b.text),
            ) {
                (true, false) => Some((a, b)),
                (false, true) => Some((b, a)),
                _ => None,
            };
            if let Some((original, roman)) = twin {
                let (all, japanese_like) = japanese_endings(&roman.text);
                words += all;
                endings += japanese_like;
                twins += 1;
                out.push(crate::model::LyricLine {
                    at_ms: original.at_ms,
                    text: original.text.clone(),
                    romanised: Some(roman.text.trim().to_owned()).filter(|r| !r.is_empty()),
                });
                i = j;
                continue;
            }
            if a.text == b.text {
                out.push(a.clone());
                i = j;
                continue;
            }
        }
        out.extend(lines[i..j].iter().cloned());
        i = j;
    }
    if twins * 2 > japanese && words > 0 && endings * 10 >= words * 7 {
        lyrics.lines = out;
    }
}

/// Ask one source for `track`'s lyrics. `Ok(None)` is a plain miss; an error is a source that
/// could not be reached or answered something unreadable, which counts as a miss too.
pub async fn fetch(
    source: Source,
    http: &reqwest::Client,
    api: &crate::api::Api,
    track: &crate::model::Track,
) -> anyhow::Result<Option<Lyrics>> {
    let mut found = match source {
        Source::BetterLyrics => better_lyrics::lyrics(http, track).await,
        Source::YouTubeSynced => youtube::synced(http, &track.video_id).await,
        Source::Lrclib => Ok(lrclib::lyrics(http, track).await),
        Source::YouTube => api.lyrics(&track.video_id).await,
        Source::LrcRed => lrc_red::lyrics(http, track).await,
        Source::KuGou => kugou::lyrics(http, track).await,
        Source::Unison => unison::lyrics(http, track).await,
        Source::Genius => genius::lyrics(http, track).await,
    }?;
    if let Some(lyrics) = found.as_mut() {
        pair_romaji(lyrics);
    }
    Ok(found)
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
                romanised: None,
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
        assert_eq!(pick(&order, &a, false), Pick::Use(YouTubeSynced));
    }

    /// A lower source answering first doesn't jump ahead of one still out above it.
    #[test]
    fn a_pending_source_above_is_waited_for() {
        let order = [BetterLyrics, Lrclib];
        let a = answers(&[
            (BetterLyrics, Answer::Pending),
            (Lrclib, Answer::Found(lyrics(true))),
        ]);
        assert_eq!(pick(&order, &a, false), Pick::Wait);
    }

    #[test]
    fn plain_text_waits_for_every_source_and_loses_to_timing() {
        let order = [YouTube, Lrclib];
        let mut a = answers(&[
            (YouTube, Answer::Found(lyrics(false))),
            (Lrclib, Answer::Pending),
        ]);
        assert_eq!(pick(&order, &a, false), Pick::Wait);
        a.insert(Lrclib, Answer::Found(lyrics(true)));
        assert_eq!(pick(&order, &a, false), Pick::Use(Lrclib));
        a.insert(Lrclib, Answer::Missing);
        assert_eq!(pick(&order, &a, false), Pick::Use(YouTube));
    }

    #[test]
    fn nothing_anywhere_is_nothing() {
        let order = [YouTube, Lrclib];
        let a = answers(&[(YouTube, Answer::Missing), (Lrclib, Answer::Missing)]);
        assert_eq!(pick(&order, &a, false), Pick::Nothing);
        assert_eq!(pick(&[], &a, false), Pick::Nothing);
    }

    fn line(at_ms: u64, text: &str, romanised: Option<&str>) -> LyricLine {
        LyricLine {
            at_ms,
            text: text.to_owned(),
            romanised: romanised.map(str::to_owned),
        }
    }

    fn synced(lines: Vec<LyricLine>) -> Lyrics {
        Lyrics {
            lines,
            synced: true,
            source: String::new(),
        }
    }

    /// The song in Japanese, then again in romaji under the same stamps: each line gets its
    /// romaji, and a line both halves share is kept once.
    #[test]
    fn romaji_twins_in_a_file_become_romanisations() {
        let mut l = synced(crate::lyrics::lrclib::parse_lrc(
            "[00:00.87]無敵の笑顔で荒らすメディア\n[00:03.78]知りたいその秘密ミステリアス\n\
             [00:14.76](You're my savior)\n\
             [00:00.87]Muteki no egao de arasu media\n[00:03.78]Shiritai sono himitsu misuteriasu\n\
             [00:14.76](You're my savior)\n",
        ));
        pair_romaji(&mut l);
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[0].text, "無敵の笑顔で荒らすメディア");
        assert_eq!(
            l.lines[0].romanised.as_deref(),
            Some("Muteki no egao de arasu media")
        );
        assert_eq!(l.lines[2].text, "(You're my savior)");
        assert_eq!(l.lines[2].romanised, None);
    }

    /// Romaji with English mixed in ("(Hey! Hey!)") still counts, and an empty twin leaves the
    /// Japanese line on its own.
    #[test]
    fn romaji_with_shouts_in_it_still_pairs() {
        let mut l = synced(crate::lyrics::lrclib::parse_lrc(
            "[00:46.24]嘘か本当か知り得ない\n[00:49.43]そんな言葉にまた一人堕ちる\n\
             [01:18.47]はいはいあの子は特別です\n\
             [00:46.24]Uso ka hontou ka shirienai (Hey! Hey! Hey! Hey!)\n\
             [00:49.43]Sonna kotoba ni mata hitori ochiru (Hey! Hey! Hey! Hey!)\n\
             [01:18.47]\n",
        ));
        pair_romaji(&mut l);
        assert_eq!(l.lines.len(), 3);
        assert!(
            l.lines[0]
                .romanised
                .as_deref()
                .unwrap()
                .starts_with("Uso ka")
        );
        assert_eq!(l.lines[2].text, "はいはいあの子は特別です");
        assert_eq!(l.lines[2].romanised, None);
    }

    /// An English translation under the same stamps isn't romaji, and is left as lines.
    #[test]
    fn a_translation_twin_is_not_taken_for_romaji() {
        let lrc = "[00:01.00]知りたいその秘密\n[00:01.00]I want to know that secret\n\
                   [00:03.00]完璧で嘘つきな君は\n[00:03.00]You, perfect and a liar\n";
        let mut l = synced(crate::lyrics::lrclib::parse_lrc(lrc));
        pair_romaji(&mut l);
        assert_eq!(l.lines.len(), 4);
        assert!(!has_romanised(&l));
    }

    /// Romanisation on: Japanese without it stands in at once, and the first source that has
    /// it takes over when it answers. Lyrics with nothing to romanise win as before.
    #[test]
    fn a_romanised_answer_takes_over_from_a_stand_in() {
        let japanese = synced(vec![line(0, "無敵の笑顔", None)]);
        let romaji = synced(vec![line(0, "無敵の笑顔", Some("Muteki no egao"))]);
        let english = synced(vec![line(0, "Couldn't beat her smile", None)]);
        let order = [YouTubeSynced, Lrclib, LrcRed];
        let mut a = answers(&[
            (YouTubeSynced, Answer::Found(japanese.clone())),
            (Lrclib, Answer::Found(english)),
            (LrcRed, Answer::Pending),
        ]);
        assert_eq!(
            pick(&order, &a, true),
            Pick::Use(YouTubeSynced),
            "shown meanwhile"
        );
        assert_eq!(pick(&order, &a, false), Pick::Use(YouTubeSynced));
        a.insert(LrcRed, Answer::Found(romaji));
        assert_eq!(pick(&order, &a, true), Pick::Use(LrcRed));
        assert_eq!(
            pick(&order, &a, false),
            Pick::Use(YouTubeSynced),
            "off: order as ever"
        );
        a.insert(LrcRed, Answer::Missing);
        assert_eq!(
            pick(&order, &a, true),
            Pick::Use(YouTubeSynced),
            "none had it"
        );
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
