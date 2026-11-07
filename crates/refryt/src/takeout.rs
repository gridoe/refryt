//! Google Takeout's watch history, brought into the play log.
//!
//! An export is the whole account's history with exact times, including plays from other devices,
//! so Stats can start with years of listening rather than from today. Importing the same or a
//! newer export again is safe: what is already there is skipped.
//!
//! An export only has a video's id, title and channel. Details come from a play of the same song
//! logged here where there is one; otherwise a row has no album, and its time is an estimate.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::play_log::{self, LoggedArtist, Play, Source};

/// A play here and in the export this close together are the same play, reported to YouTube.
const SAME_PLAY_SECS: i64 = 90;
/// How long a song is taken to be when nothing says.
pub(crate) const ESTIMATED_SECS: u64 = 210;

/// Takeout translates "Watched X". Longest first, since some start others; an unknown language
/// keeps the prefix in the title, which only affects how it reads.
const WATCHED_PREFIXES: [&str; 4] = [
    "Παρακολουθήσατε το βίντεο ",
    "Παρακολουθήσατε το ",
    "Παρακολουθήσατε ",
    "Watched ",
];
/// The product's name in `header`, translated too.
const MUSIC_HEADERS: [&str; 2] = ["YouTube Music", "Μουσική YouTube"];

#[derive(Deserialize)]
struct Item {
    header: Option<String>,
    title: Option<String>,
    #[serde(rename = "titleUrl")]
    title_url: Option<String>,
    #[serde(default)]
    subtitles: Vec<Subtitle>,
    time: Option<String>,
}

#[derive(Deserialize)]
struct Subtitle {
    name: Option<String>,
}

/// One music play in an export.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Watched {
    video_id: String,
    title: String,
    /// `None` when the row names no channel.
    artist: Option<String>,
    at: i64,
}

/// What an import did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// New plays added.
    pub added: u64,
    /// Plays in the export that were already in the log.
    pub skipped: u64,
    /// Plays with guessed times, replaced by the export's exact ones.
    pub replaced: u64,
}

fn video_id(url: &str) -> Option<&str> {
    url.split_once('?')?
        .1
        .split('&')
        .find_map(|pair| pair.strip_prefix("v="))
        .filter(|id| !id.is_empty())
}

fn clean_title(title: &str) -> &str {
    WATCHED_PREFIXES
        .iter()
        .find_map(|prefix| title.strip_prefix(prefix))
        .unwrap_or(title)
}

fn is_vevo(channel: &str) -> bool {
    channel.replace(' ', "").to_uppercase().contains("VEVO")
}

/// The artist behind a channel's name: "Queen - Topic" is Queen, and "TaylorSwiftVEVO" is Taylor
/// Swift.
fn artist_name(channel: &str) -> String {
    if let Some(name) = channel.strip_suffix(" - Topic") {
        return name.to_owned();
    }
    let trimmed = channel.trim_end();
    let bare = trimmed
        .len()
        .checked_sub(4)
        .filter(|at| trimmed.is_char_boundary(*at) && trimmed[*at..].eq_ignore_ascii_case("vevo"))
        .map(|at| trimmed[..at].trim_end());
    let Some(bare) = bare.filter(|bare| !bare.is_empty()) else {
        return channel.to_owned();
    };
    if bare.contains(' ') {
        return bare.to_owned();
    }
    let mut spaced = String::with_capacity(bare.len() + 4);
    let mut previous_lower = false;
    for c in bare.chars() {
        if c.is_uppercase() && previous_lower {
            spaced.push(' ');
        }
        previous_lower = c.is_lowercase();
        spaced.push(c);
    }
    spaced
}

/// The music plays in an export. Music shows itself three ways: the product's name, a
/// music.youtube.com link, or an auto-generated or VEVO channel on plain YouTube. Everything else
/// is left out: other videos, and removed ones, which have no link or only a link for a title.
fn parse(reader: impl Read) -> Result<Vec<Watched>> {
    let items: Vec<Item> =
        serde_json::from_reader(reader).context("this isn't a watch history in JSON format")?;
    Ok(items
        .into_iter()
        .filter_map(|item| {
            let url = item.title_url.as_deref()?;
            let channel = item
                .subtitles
                .first()
                .and_then(|s| s.name.as_deref())
                .unwrap_or("");
            let music = item
                .header
                .as_deref()
                .is_some_and(|h| MUSIC_HEADERS.contains(&h))
                || url.contains("music.youtube.com")
                || channel.ends_with(" - Topic")
                || is_vevo(channel);
            if !music {
                return None;
            }
            let title = clean_title(item.title.as_deref()?);
            if title.starts_with("http") {
                return None;
            }
            let at = chrono::DateTime::parse_from_rfc3339(item.time.as_deref()?).ok()?;
            Some(Watched {
                video_id: video_id(url)?.to_owned(),
                title: title.to_owned(),
                artist: (!channel.is_empty()).then(|| artist_name(channel)),
                at: at.timestamp(),
            })
        })
        .collect())
}

/// A song's cover, from its id, for a song nothing else is known about. This size has no black
/// bars above and below, unlike the larger default.
fn thumbnail(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg")
}

/// A play from the export, with whatever a play logged here knows about the same song, and an
/// artist's id where a play here knows it.
fn from_export(
    watched: Watched,
    known: Option<&Play>,
    artist_ids: &HashMap<String, String>,
) -> Play {
    let duration = known.and_then(|k| k.duration);
    let listened = duration.unwrap_or(ESTIMATED_SECS);
    let at = watched.at;
    if let Some(known) = known {
        return Play {
            at,
            listened,
            source: Source::Takeout,
            ..known.clone()
        };
    }
    let artists = watched
        .artist
        .map(|name| LoggedArtist {
            id: artist_ids.get(&name.to_lowercase()).cloned(),
            name,
        })
        .into_iter()
        .collect();
    Play {
        thumbnail: Some(thumbnail(&watched.video_id)),
        video_id: watched.video_id,
        title: watched.title,
        artists,
        album: None,
        album_id: None,
        duration: None,
        wide: false,
        local: false,
        at,
        listened,
        source: Source::Takeout,
    }
}

/// The log with the export's plays in it, oldest first.
///
/// Guessed plays up to the export's newest are replaced, since it has the real ones. A play in the
/// export that is already in the log is skipped: the same import again, or a song played here and
/// reported to YouTube, whose row here keeps the time actually listened.
fn merge(existing: Vec<Play>, watched: Vec<Watched>) -> (Vec<Play>, Summary) {
    let mut summary = Summary::default();
    let Some(newest) = watched.iter().map(|w| w.at).max() else {
        return (existing, summary);
    };
    let before = existing.len();
    let mut plays: Vec<Play> = existing
        .into_iter()
        .filter(|p| p.source != Source::Poll || p.at > newest)
        .collect();
    summary.replaced = (before - plays.len()) as u64;

    let mut known: HashMap<String, Play> = HashMap::new();
    let mut artist_ids: HashMap<String, String> = HashMap::new();
    let mut here: HashMap<String, Vec<i64>> = HashMap::new();
    let mut seen: HashSet<(String, i64)> = HashSet::new();
    for play in &plays {
        for artist in &play.artists {
            if let Some(id) = &artist.id {
                artist_ids.insert(artist.name.to_lowercase(), id.clone());
            }
        }
        match play.source {
            Source::App => {
                here.entry(play.video_id.clone()).or_default().push(play.at);
                known.insert(play.video_id.clone(), play.clone());
            }
            Source::Takeout => {
                seen.insert((play.video_id.clone(), play.at));
            }
            Source::Poll => {}
        }
    }

    for w in watched {
        if !seen.insert((w.video_id.clone(), w.at)) {
            summary.skipped += 1;
            continue;
        }
        // Each play here accounts for one in the export, so a real repeat still counts.
        if let Some(times) = here.get_mut(&w.video_id)
            && let Some(i) = times
                .iter()
                .position(|t| (t - w.at).abs() <= SAME_PLAY_SECS)
        {
            times.swap_remove(i);
            summary.skipped += 1;
            continue;
        }
        let details = known.get(&w.video_id);
        plays.push(from_export(w, details, &artist_ids));
        summary.added += 1;
    }
    plays.sort_by_key(|p| p.at);
    (plays, summary)
}

/// Bring an export into the play log. Slow on a long history, so not on the UI thread; the app
/// holds back its own writes until this returns.
pub fn import(path: &Path) -> Result<Summary> {
    let file = std::fs::File::open(path).context("could not open that file")?;
    let watched = parse(std::io::BufReader::new(file))?;
    if watched.is_empty() {
        bail!("no YouTube Music plays in that file");
    }
    let (plays, summary) = merge(play_log::read_all()?, watched);
    play_log::replace(&plays)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(title: &str, channel: &str, header: &str, host: &str) -> serde_json::Value {
        json!({
            "header": header,
            "title": title,
            "titleUrl": format!("https://{host}/watch?v=aaaaaaaaaaa"),
            "subtitles": [{"name": channel, "url": "https://example.com"}],
            "time": "2024-03-01T18:22:33.011Z",
        })
    }

    fn parsed(items: serde_json::Value) -> Vec<Watched> {
        parse(items.to_string().as_bytes()).unwrap()
    }

    fn one(item: serde_json::Value) -> Option<Watched> {
        parsed(json!([item])).pop()
    }

    #[test]
    fn a_translated_watched_prefix_comes_off_the_title() {
        for prefix in [
            "Watched ",
            "Παρακολουθήσατε το βίντεο ",
            "Παρακολουθήσατε το ",
            "Παρακολουθήσατε ",
        ] {
            let title = format!("{prefix}My Song");
            let w = one(item(
                &title,
                "A - Topic",
                "YouTube Music",
                "music.youtube.com",
            ));
            assert_eq!(w.unwrap().title, "My Song");
        }
        let unknown = one(item("Vu Song", "A", "YouTube Music", "music.youtube.com"));
        assert_eq!(unknown.unwrap().title, "Vu Song");
    }

    #[test]
    fn music_shows_itself_in_any_of_several_ways() {
        let web = "www.youtube.com";
        assert!(one(item("Watched s", "Plain", "YouTube Music", web)).is_some());
        assert!(one(item("Watched s", "Plain", "Μουσική YouTube", web)).is_some());
        assert!(one(item("Watched s", "Plain", "YouTube", "music.youtube.com")).is_some());
        assert!(one(item("Watched s", "A - Topic", "YouTube", web)).is_some());
        assert!(one(item("Watched s", "A VEVO", "YouTube", web)).is_some());
        assert!(one(item("Watched s", "Cooking Channel", "YouTube", web)).is_none());
    }

    #[test]
    fn rows_that_arent_a_song_are_left_out() {
        let url = "https://music.youtube.com/watch?v=aaaaaaaaaaa";
        let cases = json!([
            {"header": "YouTube Music", "title": "Watched a video that has been removed",
             "time": "2024-03-01T18:22:33Z"},
            {"header": "YouTube Music", "title": format!("Watched {url}"), "titleUrl": url,
             "time": "2024-03-01T18:22:33Z"},
            {"header": "YouTube Music", "title": "Watched s", "titleUrl": url},
            {"header": "YouTube Music", "title": "Watched s", "titleUrl": url, "time": "soon"},
            {"header": "YouTube Music", "title": "Watched s",
             "titleUrl": "https://music.youtube.com/watch", "time": "2024-03-01T18:22:33Z"},
        ]);
        assert!(parsed(cases).is_empty());
    }

    #[test]
    fn a_row_gives_its_id_title_artist_and_time() {
        let w = one(item(
            "Watched s",
            "Queen - Topic",
            "YouTube Music",
            "music.youtube.com",
        ));
        assert_eq!(
            w,
            Some(Watched {
                video_id: "aaaaaaaaaaa".to_owned(),
                title: "s".to_owned(),
                artist: Some("Queen".to_owned()),
                at: 1_709_317_353,
            })
        );
    }

    #[test]
    fn a_link_with_other_parameters_still_gives_its_id() {
        assert_eq!(video_id("https://x/watch?list=L&v=abc&t=3"), Some("abc"));
        assert_eq!(video_id("https://x/watch?v="), None);
        assert_eq!(video_id("https://x/watch"), None);
    }

    #[test]
    fn a_channels_name_becomes_the_artists() {
        assert_eq!(artist_name("Queen - Topic"), "Queen");
        assert_eq!(artist_name("TaylorSwiftVEVO"), "Taylor Swift");
        assert_eq!(artist_name("Taylor Swift VEVO"), "Taylor Swift");
        assert_eq!(artist_name("AC/DC"), "AC/DC");
        assert_eq!(artist_name("VEVO"), "VEVO");
    }

    #[test]
    fn an_export_in_another_format_is_refused() {
        assert!(parse("<html>".as_bytes()).is_err());
        assert!(parse("{}".as_bytes()).is_err());
    }

    fn watched(id: &str, at: i64) -> Watched {
        Watched {
            video_id: id.to_owned(),
            title: format!("title {id}"),
            artist: Some("Queen".to_owned()),
            at,
        }
    }

    fn logged(id: &str, at: i64, source: Source) -> Play {
        Play {
            video_id: id.to_owned(),
            title: "logged title".to_owned(),
            artists: vec![LoggedArtist {
                name: "Queen".to_owned(),
                id: Some("UCqueen".to_owned()),
            }],
            album: Some("Album".to_owned()),
            album_id: Some("MPREb".to_owned()),
            duration: Some(180),
            thumbnail: Some("cover".to_owned()),
            wide: false,
            local: false,
            at,
            listened: 44,
            source,
        }
    }

    #[test]
    fn a_new_play_goes_in_with_an_estimated_time_in_order() {
        let (plays, summary) = merge(
            vec![logged("x", 500, Source::App)],
            vec![watched("a", 900), watched("b", 100)],
        );
        assert_eq!(
            summary,
            Summary {
                added: 2,
                skipped: 0,
                replaced: 0
            }
        );
        let order: Vec<&str> = plays.iter().map(|p| p.video_id.as_str()).collect();
        assert_eq!(order, ["b", "x", "a"]);
        let b = &plays[0];
        assert_eq!(b.source, Source::Takeout);
        assert_eq!(b.listened, ESTIMATED_SECS);
        assert_eq!(b.album, None);
        assert_eq!(b.thumbnail.as_deref(), Some(thumbnail("b").as_str()));
    }

    #[test]
    fn importing_the_same_export_twice_adds_nothing() {
        let export = || vec![watched("a", 100), watched("a", 900), watched("b", 100)];
        let (once, first) = merge(Vec::new(), export());
        assert_eq!(first.added, 3);
        let (twice, second) = merge(once.clone(), export());
        assert_eq!(
            second,
            Summary {
                added: 0,
                skipped: 3,
                replaced: 0
            }
        );
        assert_eq!(twice, once);
    }

    #[test]
    fn the_same_row_twice_in_one_export_counts_once() {
        let (plays, summary) = merge(Vec::new(), vec![watched("a", 100), watched("a", 100)]);
        assert_eq!(plays.len(), 1);
        assert_eq!(summary.skipped, 1);
    }

    /// A song played here is reported to YouTube too; it must not count twice.
    #[test]
    fn a_play_here_and_in_the_export_counts_once_and_keeps_its_measured_time() {
        let (plays, summary) = merge(
            vec![logged("a", 1000, Source::App)],
            vec![watched("a", 1000 + SAME_PLAY_SECS)],
        );
        assert_eq!(plays.len(), 1);
        assert_eq!((plays[0].source, plays[0].listened), (Source::App, 44));
        assert_eq!(summary.skipped, 1);
    }

    #[test]
    fn a_real_repeat_near_a_play_here_still_counts() {
        let (plays, _) = merge(
            vec![logged("a", 1000, Source::App)],
            vec![watched("a", 1000), watched("a", 1060)],
        );
        assert_eq!(plays.len(), 2);
    }

    #[test]
    fn the_same_song_at_another_time_takes_the_details_known_here() {
        let (plays, summary) = merge(
            vec![logged("a", 5000, Source::App)],
            vec![watched("a", 1000)],
        );
        assert_eq!(summary.added, 1);
        let imported = &plays[0];
        assert_eq!(imported.at, 1000);
        assert_eq!(imported.source, Source::Takeout);
        assert_eq!(imported.album.as_deref(), Some("Album"));
        assert_eq!(imported.title, "logged title");
        assert_eq!(imported.listened, 180, "its real length");
    }

    #[test]
    fn an_artist_known_here_keeps_their_id_on_another_of_their_songs() {
        let (plays, _) = merge(
            vec![logged("a", 5000, Source::App)],
            vec![watched("b", 1000)],
        );
        assert_eq!(plays[0].artists[0].id.as_deref(), Some("UCqueen"));
    }

    #[test]
    fn guessed_plays_up_to_the_exports_newest_are_replaced() {
        let (plays, summary) = merge(
            vec![
                logged("old", 100, Source::Poll),
                logged("new", 2000, Source::Poll),
                logged("here", 150, Source::App),
            ],
            vec![watched("a", 1000)],
        );
        assert_eq!(summary.replaced, 1);
        let left: Vec<&str> = plays.iter().map(|p| p.video_id.as_str()).collect();
        assert_eq!(left, ["here", "a", "new"]);
    }

    #[test]
    fn an_export_with_no_plays_changes_nothing() {
        let log = vec![logged("old", 100, Source::Poll)];
        let (plays, summary) = merge(log.clone(), Vec::new());
        assert_eq!(plays, log);
        assert_eq!(summary, Summary::default());
    }
}
