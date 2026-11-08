//! Plays made away from this app, found by watching the account's history.
//!
//! YouTube's history lists each song once, newest first, moving a replayed song to the top. So
//! comparing the list with the last one seen says what was played since, wherever it was played.
//! Refryt's own plays are in that list too, and each one it reports waits here to cancel its
//! entry: a song skipped after two seconds still reaches the list, and must not come back as a
//! play from elsewhere.
//!
//! Two things it can't see: the song already on top being played again, and how much of a song
//! was listened to, so a skip elsewhere counts as a play.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Days, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

use crate::model::{Entry, Shelf, Track};
use crate::play_log::Play;

/// How long a reported play can wait to cancel a song, past the check that first looked for it.
/// Long enough for YouTube's list to catch up. After that it is dropped: replaying the song on
/// top doesn't change the list, and it would otherwise cancel a real play later.
const REPORT_LASTS_SECS: i64 = 600;

/// The state file is read and rewritten by both the check and each reported play.
static FILE: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Which account and channel `list` belongs to.
    channel: String,
    /// The video ids last seen, newest first.
    list: Vec<String>,
    /// When `list` was seen, in seconds since the Unix epoch.
    checked: i64,
    /// Songs Refryt has reported and when, not yet matched to the list.
    reported: HashMap<String, i64>,
    /// The list changed too much to follow, so plays may be missing.
    gap: bool,
}

/// What a check found.
#[derive(Debug, Default)]
pub struct Found {
    /// Plays to add to the log, oldest first.
    pub plays: Vec<Play>,
    pub gap: bool,
}

fn path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "refryt")
        .context("could not find the config directory")?;
    Ok(dirs.config_dir().join("history_check.json"))
}

fn load() -> Result<State> {
    Ok(match std::fs::read(path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => State::default(),
    })
}

fn save(state: &State) -> Result<()> {
    let path = path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::persist::write_json_atomic(&path, &serde_json::to_vec(state)?)?;
    Ok(())
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    FILE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Refryt has told YouTube about a play, so the song will turn up in the list.
pub fn note_reported(video_id: &str, now: i64) -> Result<()> {
    let _file = lock();
    let mut state = load()?;
    state.reported.insert(video_id.to_owned(), now);
    save(&state)
}

/// Compare a fresh history with the last one, and remember it.
pub fn check(channel: &str, shelves: &[Shelf], now: DateTime<Local>) -> Result<Found> {
    let _file = lock();
    let mut state = load()?;
    let plays = compare(&mut state, channel, &feed(shelves), now);
    save(&state)?;
    Ok(Found {
        plays,
        gap: state.gap,
    })
}

/// The note about missing plays has been dealt with.
pub fn clear_gap() -> Result<()> {
    let _file = lock();
    let mut state = load()?;
    if state.gap {
        state.gap = false;
        save(&state)?;
    }
    Ok(())
}

/// Stats are off: start over from a fresh list if they come back on, so what was played meanwhile
/// isn't logged.
pub fn forget() -> Result<()> {
    let _file = lock();
    match std::fs::remove_file(path()?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// A song in the list, with the title of the shelf it's under.
struct Seen<'a> {
    track: &'a Track,
    shelf: &'a str,
}

fn feed(shelves: &[Shelf]) -> Vec<Seen<'_>> {
    let mut known = HashSet::new();
    shelves
        .iter()
        .flat_map(|shelf| shelf.items.iter().map(move |item| (shelf, item)))
        .filter_map(|(shelf, item)| match item {
            Entry::Song(track) | Entry::Video(track) => Some(Seen {
                track,
                shelf: &shelf.title,
            }),
            _ => None,
        })
        .filter(|seen| known.insert(seen.track.video_id.as_str()))
        .collect()
}

/// How many songs at the top of `new` were played since `old` was seen: the fewest such that the
/// rest are still in the order `old` had them. The rest only have to appear in that order, since
/// songs fall off the end of the list and can be removed from history. `None` when only the whole
/// list would do, which says nothing lines up.
fn new_songs<S: AsRef<str>>(old: &[S], new: &[S]) -> Option<usize> {
    (0..new.len()).find(|&top| {
        let moved: HashSet<&str> = new[..top].iter().map(AsRef::as_ref).collect();
        let mut remaining = old
            .iter()
            .map(AsRef::as_ref)
            .filter(|id| !moved.contains(id));
        new[top..]
            .iter()
            .all(|id| remaining.any(|old_id| old_id == id.as_ref()))
    })
}

fn compare(state: &mut State, channel: &str, feed: &[Seen], now: DateTime<Local>) -> Vec<Play> {
    // An empty answer changes nothing, so a paused history can't wipe the list.
    if feed.is_empty() {
        return Vec::new();
    }
    let ids: Vec<&str> = feed.iter().map(|s| s.track.video_id.as_str()).collect();
    let at_now = now.timestamp();
    let remember = |state: &mut State| {
        state.list = ids.iter().map(|id| (*id).to_owned()).collect();
        state.checked = at_now;
    };

    // The first look at this channel only sets where to count from.
    if state.channel != channel || state.list.is_empty() {
        state.channel = channel.to_owned();
        state.reported.clear();
        state.gap = false;
        remember(state);
        return Vec::new();
    }

    let old: Vec<&str> = state.list.iter().map(String::as_str).collect();
    let Some(top) = new_songs(&old, &ids) else {
        state.gap = true;
        state.reported.clear();
        remember(state);
        return Vec::new();
    };

    let mut away = Vec::new();
    for seen in &feed[..top] {
        if state.reported.remove(&seen.track.video_id).is_none() {
            away.push(seen);
        }
    }
    state
        .reported
        .retain(|_, at| *at >= at_now - REPORT_LASTS_SECS);
    let plays = dated(&away, state.checked, now);
    remember(state);
    plays
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum When {
    Today,
    Yesterday,
    Unknown,
}

/// What a shelf title says about the date. Titles follow the account's language, so one not
/// recognised just means a less exact guess.
fn when(shelf: &str) -> When {
    match shelf.trim().to_lowercase().as_str() {
        "today" | "σήμερα" => When::Today,
        "yesterday" | "χθες" => When::Yesterday,
        _ => When::Unknown,
    }
}

fn midnight(date: NaiveDate, tz: &Local) -> Option<i64> {
    date.and_hms_opt(0, 0, 0)
        .and_then(|at| tz.from_local_datetime(&at).earliest())
        .map(|at| at.timestamp())
}

/// Best-guess dates, good enough for date ranges: spread evenly across when each could have been
/// played, between the last check and now and inside the day its shelf says. Oldest first.
fn dated(away: &[&Seen], checked: i64, now: DateTime<Local>) -> Vec<Play> {
    let at_now = now.timestamp();
    let today = now.date_naive();
    let today_start = midnight(today, &Local).unwrap_or(at_now);
    let yesterday_start = today
        .checked_sub_days(Days::new(1))
        .and_then(|day| midnight(day, &Local))
        .unwrap_or(today_start);
    let span = |when: When| {
        let (from, to) = match when {
            When::Today => (checked.max(today_start), at_now),
            When::Yesterday => (checked.max(yesterday_start), today_start - 1),
            When::Unknown => (checked, at_now),
        };
        if from <= to {
            (from, to)
        } else {
            (checked, at_now)
        }
    };
    let mut total: HashMap<When, i64> = HashMap::new();
    for seen in away {
        *total.entry(when(seen.shelf)).or_default() += 1;
    }
    let mut done: HashMap<When, i64> = HashMap::new();
    let mut plays: Vec<Play> = away
        .iter()
        .map(|seen| {
            let when = when(seen.shelf);
            let (from, to) = span(when);
            let place = done.entry(when).or_default();
            *place += 1;
            let at = to - *place * (to - from) / (total[&when] + 1);
            Play::seen_in_history(seen.track, at)
        })
        .collect();
    plays.sort_by_key(|play| play.at);
    plays
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play_log::Source;

    fn song(id: &str) -> Track {
        let mut track = Play::seen_in_history(&empty_track(), 0).track();
        track.video_id = id.to_owned();
        track.title = id.to_owned();
        track
    }

    fn empty_track() -> Track {
        Play {
            video_id: "x".to_owned(),
            title: "x".to_owned(),
            artists: Vec::new(),
            album: None,
            album_id: None,
            duration: Some(100),
            thumbnail: None,
            wide: false,
            local: false,
            at: 0,
            listened: 0,
            source: Source::Poll,
        }
        .track()
    }

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 6, 20, 0, 0).unwrap()
    }

    /// `compare` against a list of ids, all on one shelf.
    fn run(state: &mut State, ids: &[&str], shelf: &str, at: DateTime<Local>) -> Vec<String> {
        let tracks: Vec<Track> = ids.iter().map(|id| song(id)).collect();
        let seen: Vec<Seen> = tracks.iter().map(|track| Seen { track, shelf }).collect();
        compare(state, "me", &seen, at)
            .into_iter()
            .map(|play| play.video_id)
            .collect()
    }

    fn watching(ids: &[&str]) -> State {
        let mut state = State::default();
        run(&mut state, ids, "Today", now());
        state
    }

    #[test]
    fn nothing_new_when_the_list_is_the_same() {
        assert_eq!(new_songs(&["a", "b", "c"], &["a", "b", "c"]), Some(0));
    }

    #[test]
    fn new_songs_are_the_ones_on_top() {
        assert_eq!(new_songs(&["a", "b"], &["x", "y", "a", "b"]), Some(2));
    }

    #[test]
    fn a_replay_moves_a_song_to_the_top() {
        assert_eq!(new_songs(&["a", "b", "c"], &["c", "a", "b"]), Some(1));
    }

    /// Comparing only the top song misses this one, since `a` is on top both times.
    #[test]
    fn a_replay_that_ends_back_on_top_is_seen() {
        assert_eq!(new_songs(&["a", "b", "c"], &["a", "c", "b"]), Some(2));
    }

    #[test]
    fn songs_falling_off_the_end_or_removed_mean_nothing() {
        assert_eq!(new_songs(&["a", "b", "c"], &["x", "a", "b"]), Some(1));
        assert_eq!(new_songs(&["a", "b", "c", "d"], &["a", "c", "d"]), Some(0));
    }

    #[test]
    fn a_list_with_nothing_in_common_is_no_answer() {
        assert_eq!(new_songs(&["a", "b"], &["x", "y"]), None);
    }

    #[test]
    fn the_first_check_only_sets_a_starting_point() {
        let mut state = State::default();
        assert!(run(&mut state, &["a", "b"], "Today", now()).is_empty());
        assert_eq!(state.list, ["a", "b"]);
        assert_eq!(state.channel, "me");
    }

    #[test]
    fn a_song_played_elsewhere_is_logged_as_seen_in_history() {
        let mut state = watching(&["a", "b"]);
        let tracks = [song("x"), song("a"), song("b")];
        let seen: Vec<Seen> = tracks
            .iter()
            .map(|track| Seen {
                track,
                shelf: "Today",
            })
            .collect();
        let plays = compare(&mut state, "me", &seen, now());
        assert_eq!(plays.len(), 1);
        assert_eq!(plays[0].video_id, "x");
        assert_eq!(plays[0].source, Source::Poll);
        assert_eq!(plays[0].listened, 100);
        assert_eq!(state.list, ["x", "a", "b"]);
    }

    #[test]
    fn a_reported_song_cancels_its_entry_even_with_no_play_logged() {
        let mut state = watching(&["a", "b"]);
        state
            .reported
            .insert("skipped".to_owned(), now().timestamp());
        let found = run(&mut state, &["away", "skipped", "a", "b"], "Today", now());
        assert_eq!(found, ["away"]);
        assert!(state.reported.is_empty());
    }

    #[test]
    fn a_report_waits_for_the_list_to_catch_up_then_expires() {
        let mut state = watching(&["a", "b"]);
        state.reported.insert("late".to_owned(), now().timestamp());
        assert!(run(&mut state, &["a", "b"], "Today", now()).is_empty());
        assert!(state.reported.contains_key("late"), "still in time");

        // The song on top played again: the list never changes, so the report goes unmatched.
        let later = now() + chrono::Duration::seconds(REPORT_LASTS_SECS + 1);
        assert!(run(&mut state, &["a", "b"], "Today", later).is_empty());
        assert!(state.reported.is_empty(), "too old to cancel a real play");
    }

    /// Reported just before quitting, the check at the next start still has to cancel it, however
    /// long that is.
    #[test]
    fn a_report_from_the_last_session_still_cancels_at_the_next_start() {
        let mut state = watching(&["a", "b"]);
        state.reported.insert("last".to_owned(), now().timestamp());
        let days_later = now() + chrono::Duration::days(3);
        assert!(run(&mut state, &["last", "a", "b"], "Today", days_later).is_empty());
    }

    #[test]
    fn an_empty_answer_changes_nothing() {
        let mut state = watching(&["a", "b"]);
        assert!(run(&mut state, &[], "Today", now()).is_empty());
        assert_eq!(state.list, ["a", "b"]);
    }

    #[test]
    fn another_channel_starts_over_without_logging() {
        let mut state = watching(&["a", "b"]);
        state.reported.insert("a".to_owned(), now().timestamp());
        let tracks = [song("x")];
        let seen = [Seen {
            track: &tracks[0],
            shelf: "Today",
        }];
        assert!(compare(&mut state, "other", &seen, now()).is_empty());
        assert_eq!(state.channel, "other");
        assert_eq!(state.list, ["x"]);
        assert!(state.reported.is_empty());
        assert!(!state.gap);
    }

    #[test]
    fn a_list_that_cant_be_followed_logs_nothing_and_says_so() {
        let mut state = watching(&["a", "b"]);
        assert!(run(&mut state, &["x", "y"], "Today", now()).is_empty());
        assert!(state.gap);
        assert_eq!(state.list, ["x", "y"]);
    }

    #[test]
    fn repeats_across_shelves_count_once() {
        let shelf = |title: &str, ids: &[&str]| Shelf {
            title: title.to_owned(),
            strapline: None,
            items: ids.iter().map(|id| Entry::Song(song(id))).collect(),
            more_params: None,
            more_browse_id: None,
            expanded: false,
        };
        let shelves = [shelf("Today", &["a"]), shelf("Yesterday", &["a", "b"])];
        let seen = feed(&shelves);
        let ids: Vec<&str> = seen.iter().map(|s| s.track.video_id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn dates_stay_in_the_gap_and_the_day_their_shelf_says() {
        let at = now();
        let checked = at.timestamp() - 3 * 86_400;
        let today_start = midnight(at.date_naive(), &Local).unwrap();
        let (one, two, three) = (song("one"), song("two"), song("three"));
        let seen = [
            Seen {
                track: &one,
                shelf: "Today",
            },
            Seen {
                track: &two,
                shelf: "Yesterday",
            },
            Seen {
                track: &three,
                shelf: "This week",
            },
        ];
        let refs: Vec<&Seen> = seen.iter().collect();
        let plays = dated(&refs, checked, at);
        let date = |id: &str| plays.iter().find(|p| p.video_id == id).unwrap().at;
        assert!(date("one") >= today_start && date("one") <= at.timestamp());
        assert!(date("two") < today_start && date("two") >= today_start - 86_400);
        assert!(date("three") >= checked && date("three") <= at.timestamp());
        assert!(plays.windows(2).all(|w| w[0].at <= w[1].at), "oldest first");
    }

    #[test]
    fn a_shelf_that_disagrees_with_the_gap_falls_back_to_the_gap() {
        // Checked this morning, but the shelf says yesterday.
        let at = now();
        let checked = at.timestamp() - 3_600;
        let track = song("x");
        let seen = Seen {
            track: &track,
            shelf: "Yesterday",
        };
        let plays = dated(&[&seen], checked, at);
        assert!(plays[0].at >= checked && plays[0].at <= at.timestamp());
    }
}
