//! One artist's or one song's listening, for their stats page: what the play log says about
//! them all-time, and where they stand against everything else in it.
//!
//! One pass over the log, on a background thread, like the Stats page's report. Ranking needs
//! every artist's (or song's) play count, so the same pass counts those too and keeps nothing
//! else of them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate, TimeZone};

use crate::listening_stats::{Bar, StatsScope, artist_key, plays_label, span_bars};
use crate::model::{Entry, Track};
use crate::play_log::{self, Play, SongDetails};

/// How far back "recent" goes for the second rank: the 6 months the Stats page means.
const RECENT_SECS: i64 = 183 * 86_400;
/// How many of an artist's songs their page lists.
pub const ARTIST_TOP_SONGS: usize = 10;

/// What a stats page is about. An artist goes by their lowercased name, as the Stats page
/// counts them: an import knows an artist only by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityKey {
    Artist(String),
    Song(String),
}

impl EntityKey {
    pub fn artist(name: &str) -> Self {
        Self::Artist(name.to_lowercase())
    }

    fn matches(&self, play: &Play) -> bool {
        match self {
            Self::Artist(key) => play.artists.iter().any(|a| artist_key(a) == *key),
            Self::Song(id) => play.video_id == *id,
        }
    }
}

/// Place among everything played: 1 for the most played. Ties share a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rank {
    pub place: u64,
    pub of: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntityReport {
    pub key: EntityKey,
    /// The artist's name or the song's title, as the latest play gave it.
    pub name: String,
    /// The artist's YouTube Music id, once any play gave it.
    pub artist_id: Option<String>,
    /// The song, playable: from its latest play.
    pub track: Option<Track>,
    /// The song's cover, or the cover of the artist's most played song.
    pub art: Option<Arc<str>>,
    pub plays: u64,
    pub seconds: u64,
    /// `seconds` is partly an estimate, from an import or the account's history.
    pub seconds_estimated: bool,
    pub first: Option<i64>,
    pub last: Option<i64>,
    pub rank: Option<Rank>,
    /// Over the last 6 months; none when not played in them.
    pub rank_recent: Option<Rank>,
    /// Different songs played: the artist's, or 1 for a song.
    pub songs: u64,
    /// The day with the most plays, and how many.
    pub best_day: Option<(NaiveDate, u32)>,
    /// The artist's most played songs, up to [`ARTIST_TOP_SONGS`], with their plays.
    pub top_songs: Vec<Entry>,
    /// The artist's first song played, and when.
    pub first_song: Option<(Track, i64)>,
    /// Plays by month from the first one, or by year for a long history.
    pub bars: Vec<Bar>,
}

struct Tally {
    plays: u64,
    last: i64,
    play: Play,
}

/// Counts for one kind of thing (artists or songs), to rank against.
#[derive(Default)]
struct Board {
    all: HashMap<String, u64>,
    recent: HashMap<String, u64>,
}

impl Board {
    fn rank(map: &HashMap<String, u64>, key: &str) -> Option<Rank> {
        let mine = *map.get(key).filter(|n| **n > 0)?;
        Some(Rank {
            place: 1 + map.values().filter(|n| **n > mine).count() as u64,
            of: map.len() as u64,
        })
    }
}

struct Builder<Tz: TimeZone> {
    key: EntityKey,
    now: DateTime<Tz>,
    details: HashMap<String, SongDetails>,
    scope: StatsScope,
    board: Board,
    name: (i64, String),
    artist_id: Option<String>,
    plays: u64,
    seconds: u64,
    estimated: bool,
    first: Option<(i64, Play)>,
    last: Option<i64>,
    songs: HashMap<String, Tally>,
    days: HashMap<NaiveDate, u32>,
}

impl<Tz: TimeZone> Builder<Tz> {
    fn new(key: EntityKey, now: DateTime<Tz>, details: HashMap<String, SongDetails>) -> Self {
        Self {
            key,
            now,
            details,
            scope: StatsScope::Everywhere,
            board: Board::default(),
            name: (i64::MIN, String::new()),
            artist_id: None,
            plays: 0,
            seconds: 0,
            estimated: false,
            first: None,
            last: None,
            songs: HashMap::new(),
            days: HashMap::new(),
        }
    }

    fn add(&mut self, mut play: Play) {
        if !self.scope.counts(&play) {
            return;
        }
        if let Some(details) = self.details.get(&play.video_id) {
            play.apply(details);
        }
        let recent = play.at >= self.now.timestamp() - RECENT_SECS;
        // Every artist or song counts towards the ranking; an artist once per play.
        let keys: Vec<String> = match &self.key {
            EntityKey::Artist(_) => {
                let names: HashSet<String> = play.artists.iter().map(artist_key).collect();
                names.into_iter().collect()
            }
            EntityKey::Song(_) => vec![play.video_id.clone()],
        };
        for key in keys {
            if recent {
                *self.board.recent.entry(key.clone()).or_default() += 1;
            }
            *self.board.all.entry(key).or_default() += 1;
        }
        if !self.key.matches(&play) {
            return;
        }
        self.plays += 1;
        self.seconds += play.listened;
        self.estimated |= play.source != play_log::Source::App;
        let tz = self.now.timezone();
        if let Some(date) = tz
            .timestamp_opt(play.at, 0)
            .earliest()
            .map(|at| at.date_naive())
        {
            *self.days.entry(date).or_default() += 1;
        }
        self.last = Some(self.last.map_or(play.at, |last| last.max(play.at)));
        match &self.key {
            EntityKey::Artist(key) => {
                if let Some(artist) = play.artists.iter().find(|a| artist_key(a) == *key) {
                    if play.at >= self.name.0 {
                        self.name = (play.at, artist.name.clone());
                    }
                    if artist.id.is_some() {
                        self.artist_id.clone_from(&artist.id);
                    }
                }
            }
            EntityKey::Song(_) => {
                if play.at >= self.name.0 {
                    self.name = (play.at, play.title.clone());
                }
            }
        }
        if self.first.as_ref().is_none_or(|(at, _)| play.at < *at) {
            self.first = Some((play.at, play.clone()));
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
                self.songs.insert(
                    play.video_id.clone(),
                    Tally {
                        plays: 1,
                        last: play.at,
                        play,
                    },
                );
            }
        }
    }

    fn finish(self) -> EntityReport {
        let board_key = match &self.key {
            EntityKey::Artist(key) | EntityKey::Song(key) => key.clone(),
        };
        let mut songs: Vec<Tally> = self.songs.into_values().collect();
        songs.sort_unstable_by(|a, b| b.plays.cmp(&a.plays).then(b.last.cmp(&a.last)));
        let art = songs
            .iter()
            .find_map(|t| t.play.thumbnail.as_deref())
            .map(Arc::from);
        let track = match self.key {
            EntityKey::Song(_) => songs.first().map(|t| t.play.track()),
            EntityKey::Artist(_) => None,
        };
        let top_songs = match self.key {
            EntityKey::Artist(_) => songs
                .iter()
                .take(ARTIST_TOP_SONGS)
                .map(|tally| {
                    let mut track = tally.play.track();
                    track.plays = Some(plays_label(tally.plays));
                    if track.wide {
                        Entry::Video(track)
                    } else {
                        Entry::Song(track)
                    }
                })
                .collect(),
            EntityKey::Song(_) => Vec::new(),
        };
        let best_day = self
            .days
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(date, plays)| (*date, *plays));
        let today = self.now.date_naive();
        let bars = self
            .days
            .keys()
            .min()
            .map(|first| span_bars(*first, today, &self.days))
            .unwrap_or_default();
        let first_song = match self.key {
            EntityKey::Artist(_) => self.first.as_ref().map(|(at, play)| (play.track(), *at)),
            EntityKey::Song(_) => None,
        };
        EntityReport {
            rank: Board::rank(&self.board.all, &board_key),
            rank_recent: Board::rank(&self.board.recent, &board_key),
            key: self.key,
            name: self.name.1,
            artist_id: self.artist_id,
            track,
            art,
            plays: self.plays,
            seconds: self.seconds,
            seconds_estimated: self.estimated,
            first: self.first.map(|(at, _)| at),
            last: self.last,
            songs: songs.len() as u64,
            best_day,
            top_songs,
            first_song,
            bars,
        }
    }
}

/// An artist's or a song's report, from the log on disk, counting the plays `scope` counts.
/// Slow enough to belong off the UI thread.
pub fn load(key: EntityKey, scope: StatsScope) -> Result<EntityReport> {
    let mut builder = Builder::new(key, Local::now(), play_log::load_details());
    builder.scope = scope;
    play_log::for_each(|play| builder.add(play))?;
    Ok(builder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play_log::{LoggedArtist, Source};
    use chrono::{FixedOffset, Timelike};

    /// A Wednesday afternoon, two hours ahead of UTC.
    fn now() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 3, 18, 15, 0, 0)
            .unwrap()
    }

    /// `days_back` days before `now()`, at `hour` local time.
    fn at(days_back: i64, hour: u32) -> i64 {
        let day = now() - chrono::Duration::days(days_back);
        day.with_hour(hour).unwrap().timestamp()
    }

    fn play(song: &str, artists: &[&str], at: i64) -> Play {
        Play {
            video_id: song.to_owned(),
            title: format!("{song} title"),
            artists: artists
                .iter()
                .map(|a| LoggedArtist {
                    name: (*a).to_owned(),
                    id: Some(format!("id-{a}")),
                })
                .collect(),
            album: None,
            album_id: None,
            duration: Some(200),
            thumbnail: Some(format!("art-{song}")),
            wide: false,
            local: false,
            at,
            listened: 100,
            source: Source::App,
        }
    }

    fn report(key: EntityKey, plays: Vec<Play>) -> EntityReport {
        let mut builder = Builder::new(key, now(), HashMap::new());
        for p in plays {
            builder.add(p);
        }
        builder.finish()
    }

    fn log() -> Vec<Play> {
        vec![
            play("a1", &["Ado"], at(400, 9)),
            play("a2", &["Ado", "Eve"], at(30, 22)),
            play("a2", &["Ado", "Eve"], at(30, 23)),
            play("a2", &["Ado", "Eve"], at(2, 22)),
            play("e1", &["Eve"], at(1, 8)),
            play("e1", &["Eve"], at(1, 9)),
            play("e1", &["Eve"], at(1, 10)),
            play("e1", &["Eve"], at(1, 11)),
            play("y1", &["YOASOBI"], at(300, 12)),
        ]
    }

    #[test]
    fn an_artist_adds_up_across_their_songs_and_features() {
        let r = report(EntityKey::artist("ADO"), log());
        assert_eq!(r.name, "Ado");
        assert_eq!(r.artist_id.as_deref(), Some("id-Ado"));
        assert_eq!((r.plays, r.seconds, r.songs), (4, 400, 2));
        assert_eq!((r.first, r.last), (Some(at(400, 9)), Some(at(2, 22))));
        assert_eq!(
            r.art.as_deref(),
            Some("art-a2"),
            "the most played song's cover"
        );
        let titles: Vec<&str> = r.top_songs.iter().map(|e| e.title()).collect();
        assert_eq!(titles, ["a2 title", "a1 title"]);
        assert_eq!(
            r.first_song.as_ref().map(|(t, _)| t.video_id.as_str()),
            Some("a1")
        );
        assert!(r.track.is_none());
    }

    #[test]
    fn rank_is_against_every_artist_and_ties_share_a_place() {
        let r = report(EntityKey::artist("eve"), log());
        // Eve 7, Ado 4, YOASOBI 1.
        assert_eq!(r.rank, Some(Rank { place: 1, of: 3 }));
        let ado = report(EntityKey::artist("ado"), log());
        assert_eq!(ado.rank, Some(Rank { place: 2, of: 3 }));
        // In the last 6 months: Eve 7, Ado 3; YOASOBI's play is older.
        assert_eq!(ado.rank_recent, Some(Rank { place: 2, of: 2 }));
        let yoasobi = report(EntityKey::artist("yoasobi"), log());
        assert_eq!(yoasobi.rank_recent, None);

        let tie = vec![play("x", &["One"], at(1, 9)), play("y", &["Two"], at(1, 9))];
        let r = report(EntityKey::artist("two"), tie);
        assert_eq!(r.rank, Some(Rank { place: 1, of: 2 }));
    }

    #[test]
    fn a_song_has_its_own_totals_and_rank() {
        let r = report(EntityKey::Song("e1".into()), log());
        assert_eq!(r.name, "e1 title");
        assert_eq!((r.plays, r.songs), (4, 1));
        assert_eq!(r.rank, Some(Rank { place: 1, of: 4 }));
        assert_eq!(r.track.as_ref().map(|t| t.video_id.as_str()), Some("e1"));
        assert!(r.top_songs.is_empty() && r.first_song.is_none());
        assert_eq!(r.best_day.map(|(_, n)| n), Some(4));
    }

    #[test]
    fn the_best_day_is_a_local_date() {
        let r = report(EntityKey::artist("eve"), log());
        assert_eq!(
            r.best_day,
            Some(((now() - chrono::Duration::days(1)).date_naive(), 4))
        );
    }

    #[test]
    fn a_long_history_charts_by_month_from_the_first_play() {
        let r = report(EntityKey::artist("ado"), log());
        assert_eq!(r.bars.len(), 14, "Feb 2025 to Mar 2026");
        assert_eq!(r.bars.iter().map(|b| b.plays).sum::<u32>(), 4);
    }

    #[test]
    fn nothing_played_leaves_an_empty_report() {
        let r = report(EntityKey::artist("nobody"), log());
        assert_eq!((r.plays, r.rank, r.first), (0, None, None));
        assert!(r.bars.is_empty() && r.name.is_empty());
    }

    #[test]
    fn the_scope_leaves_out_plays_from_elsewhere() {
        let mut elsewhere = play("e1", &["Eve"], at(3, 9));
        elsewhere.source = Source::Takeout;
        let mut builder = Builder::new(EntityKey::artist("eve"), now(), HashMap::new());
        builder.scope = StatsScope::Here;
        builder.add(elsewhere.clone());
        builder.add(play("e1", &["Eve"], at(1, 9)));
        let r = builder.finish();
        assert_eq!(r.plays, 1);
        assert!(!r.seconds_estimated);
        let everywhere = report(EntityKey::artist("eve"), vec![elsewhere]);
        assert!(everywhere.seconds_estimated);
    }
}
