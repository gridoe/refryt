//! What the play log adds up to, for the Stats page.
//!
//! One pass over the log for each report, on a background thread: the rows go by and only the
//! totals are kept, so a long history costs time here rather than memory. Everything is pure over
//! the plays and the clock passed in, which is what the tests lean on.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, TimeZone};

use crate::model::{AlbumRef, ArtistActions, ArtistRef, Entry};
use crate::play_log::{self, Play};

/// How far the calendar reaches back.
const CALENDAR_DAYS: u64 = 364;
/// How many rows a top list can open up to.
pub const TOP_LIMIT: usize = 50;
/// How many months the plays-over-time chart will show before going by year.
const MAX_MONTH_BARS: i64 = 36;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatsRange {
    Today,
    Week,
    #[default]
    FourWeeks,
    SixMonths,
    Year,
    All,
}

impl StatsRange {
    pub const ALL: [StatsRange; 6] = [
        StatsRange::Today,
        StatsRange::Week,
        StatsRange::FourWeeks,
        StatsRange::SixMonths,
        StatsRange::Year,
        StatsRange::All,
    ];

    /// The year's chip shows the year itself.
    pub fn label(self) -> String {
        match self {
            StatsRange::Today => "Today".to_owned(),
            StatsRange::Week => "This week".to_owned(),
            StatsRange::FourWeeks => "4 weeks".to_owned(),
            StatsRange::SixMonths => "6 months".to_owned(),
            StatsRange::Year => Local::now().year().to_string(),
            StatsRange::All => "All time".to_owned(),
        }
    }

    /// When the range starts, in seconds since the epoch. Calendar ranges start at a local
    /// midnight; All has no start.
    fn since<Tz: TimeZone>(self, now: &DateTime<Tz>) -> Option<i64> {
        let today = now.date_naive();
        let midnight = |date: NaiveDate| {
            date.and_hms_opt(0, 0, 0)
                .and_then(|at| at.and_local_timezone(now.timezone()).earliest())
                .map_or(now.timestamp(), |at| at.timestamp())
        };
        match self {
            StatsRange::Today => Some(midnight(today)),
            StatsRange::Week => Some(midnight(
                today - Days::new(u64::from(today.weekday().num_days_from_monday())),
            )),
            StatsRange::FourWeeks => Some(now.timestamp() - 28 * 86_400),
            StatsRange::SixMonths => Some(now.timestamp() - 183 * 86_400),
            StatsRange::Year => Some(midnight(today.with_ordinal(1).unwrap_or(today))),
            StatsRange::All => None,
        }
    }
}

/// Change against the period before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delta {
    /// `None` when there was nothing to compare with.
    pub percent: Option<u32>,
    pub up: bool,
}

/// `None` when flat, or nothing either way.
fn delta(current: u64, previous: u64) -> Option<Delta> {
    if previous == 0 {
        return (current > 0).then_some(Delta {
            percent: None,
            up: true,
        });
    }
    let change = (current as f64 - previous as f64) / previous as f64 * 100.0;
    (change.round() != 0.0).then_some(Delta {
        percent: Some(change.abs().round() as u32),
        up: change > 0.0,
    })
}

#[derive(Default)]
struct Period {
    plays: u64,
    seconds: u64,
    tracks: HashSet<String>,
    artists: HashSet<String>,
}

impl Period {
    fn add(&mut self, play: &Play) {
        self.plays += 1;
        self.seconds += play.listened;
        if !self.tracks.contains(&play.video_id) {
            self.tracks.insert(play.video_id.clone());
        }
        for artist in &play.artists {
            let key = artist_key(artist);
            if !self.artists.contains(key) {
                self.artists.insert(key.to_owned());
            }
        }
    }
}

fn artist_key(artist: &play_log::LoggedArtist) -> &str {
    artist.id.as_deref().unwrap_or(&artist.name)
}

/// One bar of plays over time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    pub label: String,
    pub plays: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub range: StatsRange,
    /// Every play in the log, in any range: nothing at all reads differently from a quiet week.
    pub logged: u64,
    pub plays: u64,
    pub seconds: u64,
    pub songs: u64,
    pub artists: u64,
    pub plays_delta: Option<Delta>,
    pub seconds_delta: Option<Delta>,
    pub songs_delta: Option<Delta>,
    pub artists_delta: Option<Delta>,
    pub top_songs: Vec<Entry>,
    pub top_artists: Vec<Entry>,
    pub top_albums: Vec<Entry>,
    pub bars: Vec<Bar>,
    /// The last [`CALENDAR_DAYS`] and their plays, oldest first, ending today.
    pub calendar: Vec<(NaiveDate, u32)>,
    pub current_streak: u32,
    pub longest_streak: u32,
}

/// "1 play", "12 plays".
pub fn plays_label(plays: u64) -> String {
    if plays == 1 {
        "1 play".to_owned()
    } else {
        format!("{plays} plays")
    }
}

struct Tally {
    plays: u64,
    last: i64,
    play: Play,
}

#[derive(Default)]
struct Group {
    plays: u64,
    last: i64,
    name: String,
    id: Option<String>,
    /// Album only: its artist.
    artist: String,
    art: Option<Arc<str>>,
}

/// Adds up the log for one range, a play at a time.
struct Builder<Tz: TimeZone> {
    now: DateTime<Tz>,
    range: StatsRange,
    since: Option<i64>,
    /// The same length of time before `since`.
    before: Option<i64>,
    logged: u64,
    current: Period,
    previous: Period,
    songs: HashMap<String, Tally>,
    artists: HashMap<String, Group>,
    albums: HashMap<String, Group>,
    range_days: HashMap<NaiveDate, u32>,
    all_days: HashMap<NaiveDate, u32>,
}

impl<Tz: TimeZone> Builder<Tz> {
    fn new(range: StatsRange, now: DateTime<Tz>) -> Self {
        let since = range.since(&now);
        let before = since.map(|since| since - (now.timestamp() - since));
        Self {
            now,
            range,
            since,
            before,
            logged: 0,
            current: Period::default(),
            previous: Period::default(),
            songs: HashMap::new(),
            artists: HashMap::new(),
            albums: HashMap::new(),
            range_days: HashMap::new(),
            all_days: HashMap::new(),
        }
    }

    fn add(&mut self, play: Play) {
        self.logged += 1;
        let tz = self.now.timezone();
        let Some(date) = tz
            .timestamp_opt(play.at, 0)
            .earliest()
            .map(|at| at.date_naive())
        else {
            return;
        };
        *self.all_days.entry(date).or_default() += 1;
        if self.since.is_some_and(|since| play.at < since) {
            if self.before.is_some_and(|before| play.at >= before) {
                self.previous.add(&play);
            }
            return;
        }
        *self.range_days.entry(date).or_default() += 1;
        self.current.add(&play);
        for artist in &play.artists {
            let key = artist_key(artist);
            let group = match self.artists.get_mut(key) {
                Some(group) => group,
                None => self.artists.entry(key.to_owned()).or_default(),
            };
            group.plays += 1;
            if play.at >= group.last || group.name.is_empty() {
                group.last = play.at;
                group.name.clone_from(&artist.name);
                group.id.clone_from(&artist.id);
            }
        }
        // An album needs its id to be a way in to anything; without one it is only a name.
        if let (Some(id), Some(title)) = (&play.album_id, &play.album) {
            let group = match self.albums.get_mut(id) {
                Some(group) => group,
                None => self.albums.entry(id.clone()).or_default(),
            };
            group.plays += 1;
            if play.at >= group.last || group.name.is_empty() {
                group.last = play.at;
                group.name.clone_from(title);
                group.id = Some(id.clone());
                group.artist = play
                    .artists
                    .first()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
            }
        }
        match self.songs.get_mut(&play.video_id) {
            Some(tally) => {
                tally.plays += 1;
                if play.at >= tally.last {
                    tally.last = play.at;
                    tally.play = play;
                }
            }
            None => {
                let key = play.video_id.clone();
                self.songs.insert(
                    key,
                    Tally {
                        plays: 1,
                        last: play.at,
                        play,
                    },
                );
            }
        }
    }

    fn finish(mut self) -> Report {
        let mut songs: Vec<Tally> = self.songs.drain().map(|(_, tally)| tally).collect();
        songs.sort_unstable_by(|a, b| b.plays.cmp(&a.plays).then(b.last.cmp(&a.last)));
        // An artist or album's picture is its most played song's cover.
        for tally in &songs {
            let art = tally.play.thumbnail.as_deref();
            for artist in &tally.play.artists {
                if let Some(group) = self.artists.get_mut(artist_key(artist))
                    && group.art.is_none()
                {
                    group.art = art.map(Arc::from);
                }
            }
            if let Some(group) = tally
                .play
                .album_id
                .as_ref()
                .and_then(|id| self.albums.get_mut(id))
                && group.art.is_none()
            {
                group.art = art.map(Arc::from);
            }
        }
        let top_songs = songs
            .into_iter()
            .take(TOP_LIMIT)
            .map(|tally| {
                let mut track = tally.play.track();
                track.plays = Some(plays_label(tally.plays));
                if track.wide {
                    Entry::Video(track)
                } else {
                    Entry::Song(track)
                }
            })
            .collect();
        let top_artists = top_groups(self.artists)
            .into_iter()
            .map(|group| {
                Entry::Artist(ArtistRef {
                    id: group.id.clone().unwrap_or_default(),
                    subscribers: Some(plays_label(group.plays)),
                    name: group.name,
                    thumbnail: group.art,
                    actions: ArtistActions::default(),
                })
            })
            .collect();
        let top_albums = top_groups(self.albums)
            .into_iter()
            .map(|group| {
                Entry::Album(AlbumRef {
                    id: group.id.clone().unwrap_or_default(),
                    title: group.name,
                    artist: group.artist,
                    year: Some(plays_label(group.plays)),
                    kind: None,
                    thumbnail: group.art,
                })
            })
            .collect();

        let today = self.now.date_naive();
        let since = self.since.map_or(
            self.all_days.keys().min().copied().unwrap_or(today),
            |since| {
                self.now
                    .timezone()
                    .timestamp_opt(since, 0)
                    .earliest()
                    .map_or(today, |at| at.date_naive())
            },
        );
        let (current_streak, longest_streak) = streaks(self.all_days.keys().copied(), today);
        let (previous, current) = (&self.previous, &self.current);
        let compare = |now: u64, then: u64| self.since.and_then(|_| delta(now, then));
        Report {
            range: self.range,
            logged: self.logged,
            plays: current.plays,
            seconds: current.seconds,
            songs: current.tracks.len() as u64,
            artists: current.artists.len() as u64,
            plays_delta: compare(current.plays, previous.plays),
            seconds_delta: compare(current.seconds, previous.seconds),
            songs_delta: compare(current.tracks.len() as u64, previous.tracks.len() as u64),
            artists_delta: compare(current.artists.len() as u64, previous.artists.len() as u64),
            top_songs,
            top_artists,
            top_albums,
            bars: bars(self.range, since, today, &self.range_days),
            calendar: calendar(today, &self.all_days),
            current_streak,
            longest_streak,
        }
    }
}

fn top_groups(groups: HashMap<String, Group>) -> Vec<Group> {
    let mut groups: Vec<Group> = groups.into_values().collect();
    groups.sort_unstable_by(|a, b| b.plays.cmp(&a.plays).then(b.last.cmp(&a.last)));
    groups.truncate(TOP_LIMIT);
    groups
}

/// The current and longest run of days with a play. The current one is still going if the last
/// play was yesterday: today isn't over.
fn streaks(days: impl Iterator<Item = NaiveDate>, today: NaiveDate) -> (u32, u32) {
    let mut days: Vec<NaiveDate> = days.collect();
    days.sort_unstable();
    let Some(&last) = days.last() else {
        return (0, 0);
    };
    let (mut longest, mut run) = (1, 1);
    for pair in days.windows(2) {
        run = if (pair[1] - pair[0]).num_days() == 1 {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
    }
    let mut current = 0;
    if (today - last).num_days() <= 1 {
        current = 1;
        for pair in days.windows(2).rev() {
            if (pair[1] - pair[0]).num_days() != 1 {
                break;
            }
            current += 1;
        }
    }
    (current, longest)
}

fn calendar(today: NaiveDate, days: &HashMap<NaiveDate, u32>) -> Vec<(NaiveDate, u32)> {
    (0..CALENDAR_DAYS)
        .rev()
        .map(|back| today - Days::new(back))
        .map(|date| (date, days.get(&date).copied().unwrap_or(0)))
        .collect()
}

/// Plays over time, in bars a chart can fit: days for the short ranges, then weeks, months, and
/// years for a history too long for months. A single day has nothing to chart.
fn bars(
    range: StatsRange,
    since: NaiveDate,
    today: NaiveDate,
    days: &HashMap<NaiveDate, u32>,
) -> Vec<Bar> {
    let plays = |from: NaiveDate, to: NaiveDate| -> u32 {
        from.iter_days()
            .take_while(|d| *d <= to)
            .map(|d| days.get(&d).copied().unwrap_or(0))
            .sum()
    };
    match range {
        StatsRange::Today => Vec::new(),
        StatsRange::Week | StatsRange::FourWeeks => since
            .iter_days()
            .take_while(|d| *d <= today)
            .map(|d| Bar {
                label: d.format("%b %-d").to_string(),
                plays: plays(d, d),
            })
            .collect(),
        StatsRange::SixMonths => since
            .iter_weeks()
            .take_while(|d| *d <= today)
            .map(|d| Bar {
                label: format!("Week of {}", d.format("%b %-d")),
                plays: plays(d, (d + Days::new(6)).min(today)),
            })
            .collect(),
        StatsRange::Year | StatsRange::All => {
            let months = i64::from(today.year() - since.year()) * 12 + i64::from(today.month())
                - i64::from(since.month());
            if months < MAX_MONTH_BARS {
                month_bars(since, today, days)
            } else {
                year_bars(since, today, days)
            }
        }
    }
}

fn month_bars(since: NaiveDate, today: NaiveDate, days: &HashMap<NaiveDate, u32>) -> Vec<Bar> {
    let mut bars: Vec<Bar> = Vec::new();
    let (mut year, mut month) = (since.year(), since.month());
    while (year, month) <= (today.year(), today.month()) {
        let plays = days
            .iter()
            .filter(|(d, _)| (d.year(), d.month()) == (year, month))
            .map(|(_, plays)| *plays)
            .sum();
        let label = NaiveDate::from_ymd_opt(year, month, 1)
            .map_or_else(String::new, |d| d.format("%b %Y").to_string());
        bars.push(Bar { label, plays });
        (year, month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
    }
    bars
}

fn year_bars(since: NaiveDate, today: NaiveDate, days: &HashMap<NaiveDate, u32>) -> Vec<Bar> {
    (since.year()..=today.year())
        .map(|year| Bar {
            label: year.to_string(),
            plays: days
                .iter()
                .filter(|(d, _)| d.year() == year)
                .map(|(_, plays)| *plays)
                .sum(),
        })
        .collect()
}

/// "3 min", "5 h 12 min", "212 h".
pub fn listening_time(seconds: u64) -> String {
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, _) if hours >= 100 => format!("{hours} h"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// The report for a range, from the log on disk. Slow enough to belong off the UI thread.
pub fn load(range: StatsRange) -> Result<Report> {
    let mut builder = Builder::new(range, Local::now());
    play_log::for_each(|play| builder.add(play))?;
    Ok(builder.finish())
}

/// How many times an artist has been played, for their page.
pub fn artist_plays(id: &str) -> Result<u64> {
    let mut plays = 0;
    play_log::for_each(|play| {
        if play.artists.iter().any(|a| a.id.as_deref() == Some(id)) {
            plays += 1;
        }
    })?;
    Ok(plays)
}

/// A time some days back, for tests that place plays relative to now.
#[cfg(test)]
fn at_hour<Tz: TimeZone>(now: &DateTime<Tz>, days_back: u64, hour: u32) -> i64 {
    use chrono::Timelike;
    let date = now.date_naive() - Days::new(days_back);
    let at = date
        .and_hms_opt(hour.min(now.hour()), 0, 0)
        .and_then(|at| at.and_local_timezone(now.timezone()).earliest())
        .expect("a real time");
    at.timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play_log::{LoggedArtist, Source};
    use chrono::FixedOffset;

    /// A Wednesday afternoon, two hours ahead of UTC.
    fn now() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 3, 18, 15, 0, 0)
            .unwrap()
    }

    fn play(song: &str, artist: &str, at: i64) -> Play {
        Play {
            video_id: song.to_owned(),
            title: song.to_owned(),
            artists: vec![LoggedArtist {
                name: artist.to_owned(),
                id: Some(format!("id-{artist}")),
            }],
            album: Some(format!("{song} album")),
            album_id: Some(format!("alb-{song}")),
            duration: Some(200),
            thumbnail: Some(format!("art-{song}")),
            wide: false,
            local: false,
            at,
            listened: 100,
            source: Source::App,
        }
    }

    fn report(range: StatsRange, plays: Vec<Play>) -> Report {
        let mut builder = Builder::new(range, now());
        for p in plays {
            builder.add(p);
        }
        builder.finish()
    }

    fn date(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap()
    }

    #[test]
    fn calendar_ranges_start_at_a_local_midnight() {
        let local = |m, d| {
            now()
                .timezone()
                .with_ymd_and_hms(2026, m, d, 0, 0, 0)
                .unwrap()
                .timestamp()
        };
        let n = now();
        assert_eq!(StatsRange::Today.since(&n), Some(local(3, 18)));
        assert_eq!(StatsRange::Week.since(&n), Some(local(3, 16)), "a Monday");
        assert_eq!(StatsRange::Year.since(&n), Some(local(1, 1)));
        assert_eq!(
            StatsRange::FourWeeks.since(&n),
            Some(n.timestamp() - 28 * 86_400)
        );
        assert_eq!(StatsRange::All.since(&n), None);
    }

    #[test]
    fn change_is_against_the_same_length_of_time_before() {
        assert_eq!(delta(0, 0), None);
        assert_eq!(
            delta(5, 0),
            Some(Delta {
                percent: None,
                up: true
            })
        );
        assert_eq!(
            delta(15, 10),
            Some(Delta {
                percent: Some(50),
                up: true
            })
        );
        assert_eq!(
            delta(5, 10),
            Some(Delta {
                percent: Some(50),
                up: false
            })
        );
        assert_eq!(delta(1000, 1001), None, "flat once rounded");

        let n = now();
        let r = report(
            StatsRange::FourWeeks,
            vec![
                play("a", "x", at_hour(&n, 1, 12)),
                play("a", "x", at_hour(&n, 2, 12)),
                play("b", "x", at_hour(&n, 30, 12)),
                play("c", "x", at_hour(&n, 100, 12)),
            ],
        );
        assert_eq!(r.logged, 4);
        assert_eq!(r.plays, 2);
        assert_eq!(
            r.plays_delta,
            Some(Delta {
                percent: Some(100),
                up: true
            })
        );
        let all = report(StatsRange::All, vec![play("a", "x", at_hour(&n, 1, 12))]);
        assert_eq!(all.plays_delta, None, "All time has no before");
    }

    #[test]
    fn top_lists_count_plays_with_the_latest_details_and_most_recent_first_on_a_tie() {
        let n = now();
        let mut renamed = play("a", "x", at_hour(&n, 1, 12));
        renamed.title = "new title".to_owned();
        let r = report(
            StatsRange::All,
            vec![
                play("b", "y", at_hour(&n, 9, 12)),
                play("a", "x", at_hour(&n, 8, 12)),
                renamed,
                play("c", "z", at_hour(&n, 3, 12)),
            ],
        );
        let titles: Vec<&str> = r.top_songs.iter().map(Entry::title).collect();
        assert_eq!(titles, ["new title", "c", "b"]);
        let Entry::Song(top) = &r.top_songs[0] else {
            panic!("a song");
        };
        assert_eq!(top.plays.as_deref(), Some("2 plays"));
        let names: Vec<&str> = r.top_artists.iter().map(Entry::title).collect();
        assert_eq!(names, ["x", "z", "y"]);
        assert_eq!(r.top_albums.len(), 3);
        assert_eq!((r.songs, r.artists, r.seconds), (3, 3, 400));
    }

    /// Everyone on a song gets the play, and the picture is their most played song's.
    #[test]
    fn a_shared_song_counts_for_each_artist() {
        let n = now();
        let mut duet = play("duet", "x", at_hour(&n, 1, 12));
        duet.artists.push(LoggedArtist {
            name: "y".to_owned(),
            id: None,
        });
        let r = report(
            StatsRange::All,
            vec![duet.clone(), duet, play("solo", "x", at_hour(&n, 1, 10))],
        );
        assert_eq!(r.artists, 2);
        let Entry::Artist(x) = &r.top_artists[0] else {
            panic!("an artist");
        };
        assert_eq!(x.subscribers.as_deref(), Some("3 plays"));
        assert_eq!(x.thumbnail.as_deref(), Some("art-duet"));
        let Entry::Artist(y) = &r.top_artists[1] else {
            panic!("an artist");
        };
        assert_eq!(y.id, "", "no id, so a search rather than a page");
    }

    #[test]
    fn an_album_without_an_id_is_left_out_of_the_top_list() {
        let mut p = play("a", "x", at_hour(&now(), 1, 12));
        p.album_id = None;
        assert!(report(StatsRange::All, vec![p]).top_albums.is_empty());
    }

    #[test]
    fn streaks_run_over_consecutive_days_and_survive_until_a_day_is_missed() {
        let today = date(3, 18);
        let run = |days: &[NaiveDate]| streaks(days.iter().copied(), today);
        assert_eq!(run(&[]), (0, 0));
        assert_eq!(run(&[today]), (1, 1));
        assert_eq!(run(&[date(3, 16), date(3, 17)]), (2, 2), "yesterday counts");
        assert_eq!(run(&[date(3, 15), date(3, 16)]), (0, 2), "broken");
        assert_eq!(
            run(&[
                date(3, 1),
                date(3, 2),
                date(3, 3),
                date(3, 4),
                date(3, 17),
                date(3, 18)
            ]),
            (2, 4)
        );
    }

    #[test]
    fn bars_fit_the_range() {
        let today = date(3, 18);
        let mut days = HashMap::new();
        days.insert(date(3, 16), 2);
        days.insert(date(3, 18), 3);
        assert!(bars(StatsRange::Today, today, today, &days).is_empty());

        let week = bars(StatsRange::Week, date(3, 16), today, &days);
        assert_eq!(week.len(), 3);
        assert_eq!(week[0].plays, 2);
        assert_eq!(week[0].label, "Mar 16");
        assert_eq!(week[2].plays, 3);

        let six = bars(StatsRange::SixMonths, date(1, 1), today, &days);
        assert_eq!(six.last().unwrap().plays, 5, "the week's total");
        assert_eq!(six.iter().map(|b| b.plays).sum::<u32>(), 5);

        let year = bars(StatsRange::Year, date(1, 1), today, &days);
        assert_eq!(year.len(), 3);
        assert_eq!(year[2].plays, 5);
        assert_eq!(year[2].label, "Mar 2026");

        let long = bars(
            StatsRange::All,
            NaiveDate::from_ymd_opt(2020, 6, 1).unwrap(),
            today,
            &days,
        );
        assert_eq!(long.len(), 7, "by year");
        assert_eq!(long[6].label, "2026");
        assert_eq!(long[6].plays, 5);
    }

    #[test]
    fn the_calendar_ends_today() {
        let r = report(
            StatsRange::Week,
            vec![play("a", "x", at_hour(&now(), 0, 12))],
        );
        assert_eq!(r.calendar.len(), CALENDAR_DAYS as usize);
        assert_eq!(*r.calendar.last().unwrap(), (date(3, 18), 1));
    }

    /// A play late in the evening UTC belongs to the next day here.
    #[test]
    fn days_are_local() {
        let late = now()
            .timezone()
            .with_ymd_and_hms(2026, 3, 18, 0, 30, 0)
            .unwrap()
            .timestamp();
        let r = report(StatsRange::Today, vec![play("a", "x", late)]);
        assert_eq!(r.plays, 1);
        assert_eq!(*r.calendar.last().unwrap(), (date(3, 18), 1));
    }

    #[test]
    fn listening_time_reads_at_every_size() {
        assert_eq!(listening_time(59), "0 min");
        assert_eq!(listening_time(180), "3 min");
        assert_eq!(listening_time(3600), "1 h");
        assert_eq!(listening_time(5 * 3600 + 12 * 60), "5 h 12 min");
        assert_eq!(listening_time(212 * 3600 + 60), "212 h");
    }

    #[test]
    fn plays_read_in_the_singular_and_the_plural() {
        assert_eq!(plays_label(1), "1 play");
        assert_eq!(plays_label(12), "12 plays");
    }
}
