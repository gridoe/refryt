//! Application state and the per-frame update.
//!
//! Rendering lives in `crate::ui`, keyed off a [`Route`] with a browser-style history stack
//! (Back / Forward in the titlebar). This file holds the state and the wiring: what the route
//! currently is, what has been fetched for it, and how playback advances.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Color32;

use crate::backend::{Backend, Event};
use crate::chrome;
use crate::images::Images;
use crate::model::{Entry, LikeStatus, SearchResults, SearchScope, Track};
use crate::player::{NowPlaying, Player};
use crate::queue::{PlayQueue, Rewind, Slot};
use crate::stats::Stats;
use crate::theme::{self, Palette};
use crate::ui;
use crate::widgets;

/// Autocomplete is cheap and wants to feel immediate; search itself is never triggered by typing,
/// only by an explicit submit — see `App::submit_search`.
const SUGGEST_DEBOUNCE: Duration = Duration::from_millis(120);

/// While audio is playing the progress bar and the clock need to move, and a frame is also what
/// notices a track end (the player is polled, not evented). The quick tick is for the moments
/// those matter at a finer grain than a second; the rest of the time a frame a second is enough.
const PLAYING_FAST_REPAINT: Duration = Duration::from_millis(250);

/// How long before a track's end the quick tick takes over, so the next song starts about as
/// promptly as it did when every tick was quick.
const TRACK_END_WINDOW: f64 = 3.0;

/// What decides when the next frame is due while a song plays.
#[derive(Clone, Copy, Debug)]
struct PlayingTick {
    /// Seconds into the track.
    position: f64,
    /// The track's length in seconds; zero or less when not known.
    duration: f64,
    /// Whether this attempt has been seen playing yet. Until it has, a quick tick lets the stall
    /// guard tell a track that played briefly from one that never started.
    seen_playing: bool,
    /// Seconds at which the next synced lyric line starts, when lyrics are on screen.
    next_lyric: Option<f64>,
    /// The window is minimised or hidden behind others: nothing it paints is seen.
    hidden: bool,
}

/// When to ask for the next frame while a song plays. A frame a second, on the second so the
/// clock and the bar move together, or earlier for a lyric line that starts sooner; a quick tick
/// at the start of an attempt and near the track's end, where promptness is behaviour and not
/// looks. Hidden, it relaxes to two seconds, still waking for the track's end.
fn playing_repaint_after(t: PlayingTick) -> Duration {
    let remaining = t.duration - t.position;
    if !t.seen_playing || t.duration <= 0.0 || remaining <= TRACK_END_WINDOW {
        return PLAYING_FAST_REPAINT;
    }
    let slow = if t.hidden { 2.0 } else { 1.0 };
    // To the next whole second, a hair past it so the frame lands after the clock has ticked.
    let to_second = (1.0 - t.position.fract()) + 0.02;
    let mut wait = if t.hidden { slow } else { to_second };
    if !t.hidden
        && let Some(at) = t.next_lyric
    {
        wait = wait.min((at - t.position).max(0.0) + 0.02);
    }
    // Never later than the quick-tick window opens, so the end is not slept through.
    wait = wait.min((remaining - TRACK_END_WINDOW).max(0.0) + 0.02);
    Duration::from_secs_f64(wait.clamp(0.05, slow))
}

/// How far into a track the transport's Previous restarts it rather than stepping back to the
/// previous one: within this window of the start a press goes back a track, so consecutive
/// presses walk backwards, while past it the press returns to the top of the current track.
/// The convention the streaming apps use, so a press never loses the track being listened to.
const PREVIOUS_RESTART_AFTER: Duration = Duration::from_secs(2);
/// Every local track's folder-path segments paired with its position in `App::local_tracks` —
/// `ui::library::local_entries`'s return shape.
pub(crate) type LocalEntries = Vec<(Vec<String>, usize)>;

/// A [`crate::ui::library::LocalView`] and what it was derived from: the scan generation, the
/// configured folders and the folder open in the Local tab.
pub(crate) struct LocalViewCache {
    pub(crate) generation: u64,
    pub(crate) folders: Vec<String>,
    pub(crate) path: Vec<String>,
    pub(crate) view: std::sync::Arc<crate::ui::library::LocalView>,
}

/// How long the startup splash may hold the shell back, counted from process start. Generous
/// against a slow session check and a slow first feed, but a hard stop for the one case neither
/// has a timeout for: a request that simply never answers would otherwise leave the splash — or
/// an unresponsive window — up indefinitely.
const SPLASH_MAX: Duration = Duration::from_secs(10);

/// How many times an attempt that never produced audio is reloaded before it is called a failure.
const MAX_STALL_RETRIES: u32 = 3;
/// The wait before each reload, indexed by the retry it precedes. An audio device can take a
/// moment to come up after login, and reloading every frame would burn the cap before it does.
const STALL_BACKOFF: [Duration; MAX_STALL_RETRIES as usize] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];
/// How far past its own start position an attempt must get before it counts as playing. mpv
/// reports the `start` seek target as the position, so a resumed track that never advanced would
/// otherwise look exactly like one that played.
const PLAYED_EPSILON: f64 = 0.25;

/// What the transport does at the end of the queue.
/// Whether a Previous press restarts the current track instead of stepping back to the previous
/// one: with the track underway it restarts, and only within [`PREVIOUS_RESTART_AFTER`] of its
/// start does it step back. Pure so the rule is testable without a player.
fn previous_restarts(position: f64) -> bool {
    position >= PREVIOUS_RESTART_AFTER.as_secs_f64()
}

/// The remembered volume and mute after the volume is set to `set`, from `last`: an audible
/// volume is remembered and unmutes, and silence mutes but keeps `last`, so an unmute or the next
/// launch comes back to it rather than to nothing. Pure for the test.
fn volume_after(last: f64, set: f64) -> (f64, bool) {
    if set > 0.5 {
        (set, false)
    } else {
        (last, true)
    }
}

/// How far one press of the volume keys moves the volume, out of 100: the macOS menu's
/// Volume Up and Down, and Ctrl+↑/↓ elsewhere.
pub(crate) const VOLUME_STEP: f64 = 10.0;

/// The volume a volume key asks for: a step from what is heard, which is silence while muted.
/// Unclamped; [`App::set_user_volume`] clamps. Pure for the test.
fn nudged_volume(muted: bool, last: f64, step: f64) -> f64 {
    (if muted { 0.0 } else { last }) + step
}

/// Which mix row is the seed, when the mix should be adopted under the song already playing
/// rather than started from the top: the seed must be the loaded track, playback must have
/// actually started, and the mix must carry the seed itself. Pure so the rule is testable.
fn mix_seed_index(
    playback_started: bool,
    now_id: Option<&str>,
    seed: &str,
    tracks: &[Track],
) -> Option<usize> {
    if !playback_started || now_id != Some(seed) {
        return None;
    }
    tracks.iter().position(|t| t.video_id == seed)
}

/// What an idle transition means for the attempt that just ended.
#[derive(Debug, PartialEq)]
enum IdleEnd {
    /// The track played and finished (or was stopped): behave as before.
    Ended,
    /// Nothing was ever heard: reload the track once the delay has passed.
    Retry(Duration),
    /// The retries are used up: surface the failure and leave the queue alone.
    GiveUp,
}

/// Tells "the track ended" apart from "the attempt never played" across mpv's idle transition.
///
/// `playback_started` is set when mpv accepts the load, so a device that never opened still ends
/// up idle, and the queue-advance check reads that as a finished track. The retry count belongs
/// to a *track*, not an attempt: `Event::Playing` fires again on every reload, so resetting it
/// there would make the cap unreachable.
#[derive(Default)]
struct StallGuard {
    /// Set once a frame saw this attempt at a position past its start.
    played_any: bool,
    /// Retries used for the current track.
    retries: u32,
    /// When the next reload is due; `None` when none is pending.
    retry_at: Option<Instant>,
    /// The track the retries belong to.
    track: Option<String>,
    /// The attempt's own start position, for judging progress and resuming a reload.
    start_at: Option<f64>,
}

impl StallGuard {
    /// A load is being issued for `video_id`, resuming at `start_at`. A different track starts
    /// fresh; the same track keeps its retries, since this is one of our own reloads.
    fn on_load(&mut self, video_id: &str, start_at: Option<f64>) {
        self.played_any = false;
        self.retry_at = None;
        if self.track.as_deref() != Some(video_id) {
            self.track = Some(video_id.to_owned());
            self.retries = 0;
        }
        self.start_at = start_at;
    }

    /// A frame's position while the attempt is loaded.
    fn observe(&mut self, position: f64) {
        if position > self.start_at.unwrap_or(0.0) + PLAYED_EPSILON {
            self.played_any = true;
        }
    }

    /// mpv went idle: did the track end, or did the attempt never play?
    fn on_idle(&mut self, now: Instant) -> IdleEnd {
        if self.played_any {
            self.reset();
            return IdleEnd::Ended;
        }
        if self.retries < MAX_STALL_RETRIES {
            let delay = STALL_BACKOFF[self.retries as usize];
            self.retries += 1;
            self.retry_at = Some(now + delay);
            return IdleEnd::Retry(delay);
        }
        self.reset();
        IdleEnd::GiveUp
    }

    /// Take a pending reload once its backoff has passed. False before then, or when none waits.
    fn take_due(&mut self, now: Instant) -> bool {
        match self.retry_at {
            Some(at) if now >= at => {
                self.retry_at = None;
                true
            }
            _ => false,
        }
    }

    /// When the pending reload is due, so the app can ask for a frame at that time. An idle app
    /// paints nothing, and the reload would otherwise never be reached.
    fn pending_at(&self) -> Option<Instant> {
        self.retry_at
    }

    /// The attempt's start position, so a reload resumes where the first try did.
    fn start_position(&self) -> Option<f64> {
        self.start_at
    }

    fn reset(&mut self) {
        self.played_any = false;
        self.retries = 0;
        self.retry_at = None;
    }
}

/// What the OS media widget last showed, as `publish_media` compares it: the track by id and
/// title, borrowed for the comparison and cloned only when it changes.
struct MediaKey {
    id: Option<String>,
    title: Option<String>,
    scalars: MediaScalars,
}

#[derive(Clone, Copy, PartialEq)]
struct MediaScalars {
    playing: bool,
    position: u64,
    duration: u64,
    volume: i32,
    repeat: RepeatMode,
    shuffle: bool,
    can_previous: bool,
    can_next: bool,
}

/// When `--screenshot` takes its picture.
#[derive(Clone, Copy, Debug)]
pub enum ShotWhen {
    /// This long after launch (`--screenshot-after`, default 8 s).
    After(Duration),
    /// While a song's loading ring is showing (`--screenshot-on ring`).
    Ring,
    /// While a page's skeletons are showing (`--screenshot-on skeleton`).
    Skeleton,
}

/// A kind of work in flight, for deciding what shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    Search,
    Artist,
    Album,
    Playlist,
    ArtistExpand,
    Home,
    MoodPlaylists,
    Explore,
    History,
    Releases,
    Charts,
    Moods,
    Library,
    /// A Home shelf's full list, behind its "Show all".
    HomeShelf,
    /// The radio topping the queue up. The app starts this itself rather than the listener.
    Radio,
    /// The sidebar's playlists refreshing in the background.
    SidebarPlaylists,
    /// The current song being resolved: the loading ring's job, not a page's.
    Song,
}

impl Load {
    /// Whether this is a page the listener is waiting on — what skeletons stand in for.
    /// Background work shows nothing, and a song shows its ring instead.
    fn shows_skeleton(self) -> bool {
        !matches!(self, Load::Radio | Load::SidebarPlaylists | Load::Song)
    }
}

/// How old the home feed may get before a visit to Home reloads it.
/// The stages of leaving a window restored fullscreen: looking at what it started as, waiting for
/// it to be out of fullscreen, done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullscreenCheck {
    Looking,
    Leaving,
    Done,
}

/// What to do about it this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullscreenAction {
    Nothing,
    /// Ask the window to leave fullscreen.
    Leave,
    /// Left, but not out yet: look again shortly.
    Wait,
    /// Out of fullscreen: put the window at its default size.
    Resize,
}

/// One step of getting a window out of a fullscreen it was restored into. Pure, for the test.
/// The size goes only once the window says it is out: asked in the same breath, it is applied to
/// a window still fullscreen and the compositor puts the old size back.
fn fullscreen_step(
    check: FullscreenCheck,
    player_has_it: bool,
    fullscreen: bool,
    since_start: Duration,
) -> (FullscreenCheck, FullscreenAction) {
    use FullscreenAction::{Leave, Nothing, Resize, Wait};
    use FullscreenCheck::{Done, Leaving, Looking};
    match check {
        Done => (Done, Nothing),
        Looking if player_has_it => (Done, Nothing),
        Looking if fullscreen => (Leaving, Leave),
        Looking if since_start > Duration::from_secs(3) => (Done, Nothing),
        Looking => (Looking, Nothing),
        Leaving if fullscreen => (Leaving, Wait),
        Leaving => (Done, Resize),
    }
}

/// The window's size when nothing else says: what `main` opens it at.
const DEFAULT_WINDOW_SIZE: (f32, f32) = (1100.0, 720.0);

/// How often the window is asked to grow to the shell's minimum before giving up: a compositor may
/// ignore the first asks, and one that never complies must not be asked for ever.
const WINDOW_FIT_TRIES: u8 = 12;

/// macOS: where the traffic lights' close button starts, in points from the window's left. Inset
/// from the wide sidebar card's edge, and kept there when the sidebar folds, as Music and Finder
/// keep theirs: a rail is too narrow to hold all three with room around them.
const TRAFFIC_LIGHTS_LEFT: f32 = theme::GUTTER + 12.0;

const HOME_STALE: Duration = Duration::from_secs(30 * 60);

/// Whether the channel page for `loading` is a person's: marked as one when it was opened, or
/// the signed-in account's own channel.
fn is_profile(
    loading: Option<&str>,
    marked: &std::collections::HashSet<String>,
    own_channel: Option<&str>,
) -> bool {
    loading.is_some_and(|id| marked.contains(id) || own_channel == Some(id))
}

/// Whether a feed fetched at `loaded_at` is due a refresh by `now`. A feed with no time is
/// not stale: it has not loaded yet, and loading it is `load_home`'s job.
fn home_is_stale(loaded_at: Option<Instant>, now: Instant) -> bool {
    loaded_at.is_some_and(|at| now.saturating_duration_since(at) >= HOME_STALE)
}

/// How long a playing track's position may go unsaved. A crash loses at most this much of where
/// you were; pausing, seeking, skipping and quitting all save at once.
const POSITION_SAVE_EVERY: Duration = Duration::from_secs(30);

/// How often the playback state is compared with what `playback.json` holds.
const PLAYBACK_CHECK_EVERY: Duration = Duration::from_secs(1);

/// What `playback.json` was last written with, for [`playback_save_due`].
struct SavedPlayback {
    fingerprint: u64,
    position: f64,
    paused: bool,
    at: Instant,
}

/// Whether the saved playback state is out of date: never written, the queue or transport
/// changed, playback paused or resumed, the position jumped (a seek), or a playing track has
/// moved on for [`POSITION_SAVE_EVERY`]. A paused, unchanged session is never written again.
fn playback_save_due(
    saved: Option<&SavedPlayback>,
    fingerprint: u64,
    position: f64,
    paused: bool,
) -> bool {
    let Some(saved) = saved else {
        return true;
    };
    if saved.fingerprint != fingerprint || saved.paused != paused {
        return true;
    }
    let elapsed = saved.at.elapsed();
    let expected = saved.position + if paused { 0.0 } else { elapsed.as_secs_f64() };
    (position - expected).abs() > 3.0 || (!paused && elapsed >= POSITION_SAVE_EVERY)
}

/// A seed for the shuffle LCG: the clock mixed with the process id, so two starts differ.
fn rng_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b9);
    nanos ^ (std::process::id() as u64).wrapping_mul(0x2545f4914f6cdd1d)
}

/// Stable-sort playlists into the saved id order. Ids the saved list does not name keep their
/// relative order, after the named ones — which is where a newly created playlist belongs.
fn order_by_saved(items: &mut [crate::model::PlaylistRef], order: &[String]) {
    if order.is_empty() {
        return;
    }
    let rank: std::collections::HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    items.sort_by_key(|p| rank.get(p.id.as_str()).copied().unwrap_or(usize::MAX));
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) enum RepeatMode {
    /// Play the queue once and stop.
    #[default]
    Off,
    /// Wrap to the start when the queue ends.
    All,
    /// Replay the current track when it ends.
    One,
}

/// What confirming the new-playlist modal actually creates.
#[derive(Default)]
pub(crate) enum NewPlaylistKind {
    #[default]
    YouTube,
    /// Seeded with a specific set of local files — the Library's Local tab's "Save as
    /// playlist," for whichever folder was open when it was pressed.
    Local {
        track_paths: Vec<std::path::PathBuf>,
    },
}

/// The playlist rename modal's live fields.
pub(crate) struct PlaylistEditor {
    pub id: String,
    pub title: String,
    pub description: String,
    /// The title before editing, so a refused save can be reverted.
    pub original_title: String,
}

/// Where the app is looking right now. `Search` carries no payload: the editable query lives in
/// `App::query`, so the box in the titlebar and the route never disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Search,
    /// What YouTube puts where Search used to be in the rail: new releases, moods, videos.
    Explore,
    /// Explore's first button: new albums & singles.
    NewReleases,
    /// Explore's second button: charts. The params are what the Explore feed sent with its
    /// Charts button, and a reply for params since changed is stale.
    Charts(Option<String>),
    /// Explore's third button: every mood category.
    Moods,
    /// What you played, in dated shelves. Signed in only.
    History,
    Library,
    Settings,
    Artist(String),
    /// The songs by one artist that are in the account's library — the "You Liked" page, from
    /// the artist page's own tile. Reads the already-loaded artist, so `ensure_route_loaded`
    /// only has to make sure that artist is the one being shown.
    ArtistLiked(String),
    Album(String),
    Playlist(String),
    /// A mood's playlists. The payload is the opaque params string from `MoodRef`.
    Mood(String),
    /// A Home shelf's "Show all": every item in it, by the shelf's title. The title rather than
    /// its position, so loading more of the feed or refreshing it does not repoint the page.
    HomeShelf(String),
}

impl Route {
    /// Parse a `--page` argument.
    ///
    /// The payload-free pages by name, and the rest as `kind:id` — `artist:UC…`, `album:MPRE…`,
    /// `playlist:VL…`. Those cannot be reached any other way from outside, and they are the pages
    /// whose headers are hardest to check without clicking.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "home" => Some(Route::Home),
            "search" => Some(Route::Search),
            "explore" => Some(Route::Explore),
            "releases" => Some(Route::NewReleases),
            "charts" => Some(Route::Charts(None)),
            "moods" => Some(Route::Moods),
            "history" => Some(Route::History),
            "library" => Some(Route::Library),
            "settings" => Some(Route::Settings),
            other => match other.split_once(':') {
                Some(("artist", id)) => Some(Route::Artist(id.to_owned())),
                Some(("artistliked", id)) => Some(Route::ArtistLiked(id.to_owned())),
                Some(("album", id)) => Some(Route::Album(id.to_owned())),
                Some(("playlist", id)) => Some(Route::Playlist(id.to_owned())),
                Some(("homeshelf", title)) => Some(Route::HomeShelf(title.to_owned())),
                _ => None,
            },
        }
    }
}

/// Which section the Library page shows.
///
/// All four come from `App::library`, which one fetch already fills, so switching costs no
/// request. It used to drive the sidebar rail, which is how the rail ended up duplicating the
/// page it points at — and saying "Library" twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RailFilter {
    #[default]
    Playlists,
    Songs,
    Albums,
    Artists,
    /// Folders scanned from `settings.local_folders`, browsed as a tree rather than paged from
    /// the server like the other four.
    Local,
    /// Songs saved to `~/Music/refryt` — local, but still real YouTube tracks, so unlike `Local`
    /// they play through the resolver's download shortcut and scrobble.
    Downloads,
}

impl RailFilter {
    pub const ALL: [Self; 6] = [
        Self::Playlists,
        Self::Songs,
        Self::Albums,
        Self::Artists,
        Self::Local,
        Self::Downloads,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Playlists => "Playlists",
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Local => "Local",
            Self::Downloads => "Downloads",
        }
    }
}

/// Song ordering for the library page — exactly the orders YouTube can perform, applied by the
/// server across the whole library.
///
/// Sorting by artist or album used to be offered and could only ever reorder the loaded page: 25
/// rows of a 689-row library, presented as a sort of the library. YouTube has no param for those,
/// so they are gone rather than wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibrarySort {
    /// Recently saved, which is also YouTube's default order.
    #[default]
    Latest,
    NameAsc,
    NameDesc,
}

impl LibrarySort {
    pub(crate) fn label(self) -> &'static str {
        match self {
            LibrarySort::Latest => "Latest",
            LibrarySort::NameAsc => "A–Z",
            LibrarySort::NameDesc => "Z–A",
        }
    }

    fn param(self) -> crate::api::LibrarySortParam {
        match self {
            LibrarySort::Latest => crate::api::LibrarySortParam::RecentlySaved,
            LibrarySort::NameAsc => crate::api::LibrarySortParam::NameAsc,
            LibrarySort::NameDesc => crate::api::LibrarySortParam::NameDesc,
        }
    }
}

/// Track ordering for a playlist page — unlike [`LibrarySort`], never a server param: YouTube's
/// playlist endpoints (`Api::playlist`/`playlist_more`) take no sort at all, so every one of
/// these is a client-side re-sort of the tracks already loaded. Title and duration only — an
/// "artist" order would need to pick one artist off a possibly-multi-artist credit, and "date
/// added" isn't offered because `Track` carries no such timestamp. `Reverse` is the honest
/// stand-in for that: YouTube serves an ordinary playlist oldest-added-first, so flipping the
/// served order is a free "newest first" for those — but it is labelled as what it actually
/// does, since an algorithmic playlist (a radio mix, say) has no "date added" for it to mean.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlaylistSort {
    /// The order YouTube served the playlist in.
    #[default]
    Default,
    /// The served order, flipped.
    Reverse,
    TitleAsc,
    TitleDesc,
    DurationAsc,
    DurationDesc,
}

impl PlaylistSort {
    pub(crate) const ALL: [PlaylistSort; 6] = [
        PlaylistSort::Default,
        PlaylistSort::Reverse,
        PlaylistSort::TitleAsc,
        PlaylistSort::TitleDesc,
        PlaylistSort::DurationAsc,
        PlaylistSort::DurationDesc,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            PlaylistSort::Default => "Default",
            PlaylistSort::Reverse => "Reverse order",
            PlaylistSort::TitleAsc => "Title A–Z",
            PlaylistSort::TitleDesc => "Title Z–A",
            PlaylistSort::DurationAsc => "Shortest first",
            PlaylistSort::DurationDesc => "Longest first",
        }
    }

    /// Re-order `tracks` in place. Only ever called once every page is loaded — sorting a
    /// partial playlist would present a truncated list as the whole of it, exactly the mistake
    /// `LibrarySort`'s own doc comment describes YouTube's library sort avoiding by being a
    /// server param instead; a playlist has no such param to hand this to; see `sort_playlist`.
    /// The liked page sorts its index-derived rows with this too, where "the whole list" is
    /// whatever the index holds.
    ///
    /// `Default` and `Reverse` are never actually reached here — [`apply_playlist_sort`]
    /// intercepts both to restore the served order from its own snapshot instead, since this
    /// method only knows how to build a *new* order and has nowhere to restore an old one from.
    /// The arms stay as harmless no-ops rather than `unreachable!()`, so a future direct caller
    /// gets "did nothing" instead of a panic.
    pub(crate) fn apply(self, tracks: &mut [crate::model::Track]) {
        match self {
            PlaylistSort::Default | PlaylistSort::Reverse => {}
            PlaylistSort::TitleAsc => tracks.sort_by_key(|t| t.title.to_lowercase()),
            PlaylistSort::TitleDesc => {
                tracks.sort_by_key(|t| std::cmp::Reverse(t.title.to_lowercase()))
            }
            PlaylistSort::DurationAsc => tracks.sort_by_key(|t| t.duration.unwrap_or(0)),
            PlaylistSort::DurationDesc => {
                tracks.sort_by_key(|t| std::cmp::Reverse(t.duration.unwrap_or(0)))
            }
        }
    }
}

/// Browser-style navigation history.
///
/// Split out from `App` because it is the one piece of navigation that can be tested without an
/// egui context — and it is where the Back/Forward bug lived.
#[derive(Debug)]
pub(crate) struct History {
    entries: Vec<Route>,
    cursor: usize,
}

impl History {
    fn new(initial: Route) -> Self {
        Self {
            entries: vec![initial],
            cursor: 0,
        }
    }

    fn current(&self) -> &Route {
        &self.entries[self.cursor]
    }

    /// Push a route, dropping anything ahead of the cursor first. Returns false when the route
    /// is already on screen, so callers can skip the reload.
    fn push(&mut self, route: Route) -> bool {
        if *self.current() == route {
            return false;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(route);
        self.cursor = self.entries.len() - 1;
        true
    }

    fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    fn back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        self.cursor -= 1;
        true
    }

    fn forward(&mut self) -> bool {
        if !self.can_go_forward() {
            return false;
        }
        self.cursor += 1;
        true
    }
}

/// What the command line asked for. Grouped so `App::new` takes one argument rather than eight.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Hand the window back to the OS instead of drawing our own chrome.
    pub decorated: bool,
    /// Print the time to first painted frame and exit.
    pub startup_timing: bool,
    /// Start on Search with this query.
    pub search: Option<String>,
    /// Play the first result once that search lands.
    pub autoplay: bool,
    /// Show the live counter overlay.
    pub stats: bool,
    /// Print one line of counters after this long, then exit.
    pub stats_after: Option<Duration>,
    /// Save a PNG of the window to this path when `ShotWhen` says, then exit. The window
    /// captures itself, so nothing else on the desktop can end up in it.
    pub screenshot: Option<(std::path::PathBuf, ShotWhen)>,
    /// Start on a named page, overriding the default of Home.
    pub page: Option<Route>,
    /// Scroll the current page steadily downward. A test aid: paging is driven by the viewport
    /// reaching the end of a list, and there is no way to reproduce that from the outside on
    /// Wayland.
    pub autoscroll: bool,
    /// Where `autoscroll` stops, in points. `None` scrolls on forever.
    pub autoscroll_max: Option<f32>,
    /// Open the queue panel at startup. A test aid, like `autoscroll`: the panel is opened by a
    /// click in the player bar, and Wayland offers no way to synthesise one from outside.
    pub queue: bool,
    /// Open the lyrics panel at startup, the same kind of test aid as `queue`.
    pub lyrics: bool,
    /// Open the fullscreen now-playing view at startup (without changing the window), the same
    /// kind of test aid. With `lyrics`, it opens with the lyrics showing.
    pub now_playing: bool,
    /// Override the saved theme for this run, by name as typed (`catppuccin-frappe`), without
    /// touching `settings.json`. Also forces that theme's own mode. A test aid, for
    /// screenshotting the whole matrix without clicking through Settings.
    pub scheme: Option<String>,
    /// Override the saved opacity for this run, same rule as `scheme`.
    pub opacity: Option<crate::settings::Opacity>,
    /// Never bind the KDE blur protocol this run. Not a test aid but a safety hatch: a Wayland
    /// protocol error there is fatal to the whole connection, and `blur` persists — a session
    /// that dies on it needs a way back into Settings that does not depend on Settings.
    pub no_blur: bool,
    /// Turn Autoplay off for this run without touching `settings.json`. A test aid, like
    /// `scheme`: the switch lives in Settings, and a comparison run needs the off state without
    /// clicking through to it.
    pub no_autoplay: bool,
    /// Exercise the tray path without a mouse: after this long, hide the window, show it three
    /// seconds later, then quit three seconds after that — each step injected through the same
    /// queue-and-wake path a real tray event takes. A test aid, like `autoscroll`: there is no
    /// way to click a tray icon from outside the process, and the point is to time each step.
    pub tray_test: Option<Duration>,
    /// Login started this run (`autostart::ARG`): once the window is up, ask the compositor to
    /// bring it forward. Wayland maps an autostarted window behind whatever already has focus
    /// and cannot be asked to raise itself later, so the request has to be made up front; on
    /// X11 and Windows it is the same `Focus` a launcher would have given the window anyway.
    pub autostart: bool,
}

/// How far a river has flowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum River {
    /// A hand-picked queue's latent river: no page yet, seeded from the queue's last track.
    Pending,
    /// Flowing: the `nextRadioContinuationData` token that extends it.
    Open(String),
    /// Ran dry or failed. Never fetched again: a dead continuation is not retried, and a page
    /// that adds nothing would leave the low-water mark tripped forever.
    Dead,
}

/// Autoplay river state. Every queue opens one: Start Mix and an artist's Shuffle/Mix open it
/// flowing, a hand-picked queue opens it pending (M21). The Autoplay setting decides whether it
/// is ever asked for a page; what it delivered stays either way.
#[derive(Clone, Debug)]
pub(crate) struct Radio {
    pub(crate) state: River,
    /// The video id a pending river seeds from — the queue's last track — and the reseed
    /// fallback once flowing.
    pub(crate) seed: String,
    /// The video ids the listener chose: the queue they started plus anything queued by hand.
    /// Tracks not in this list are the algorithm's and carry a "Radio" marker in Up Next.
    /// Ids, not indices — inserting into the queue shifts indices, and ids do not. A set, because
    /// only membership is ever asked, once per queued track on every frame the queue is open.
    pub(crate) chosen: std::collections::HashSet<String>,
    /// The radio's filters, from the page that opened it: "All", "Familiar", "Discover", a
    /// decade, a genre. Empty until that page lands, and for a radio that came without them.
    pub(crate) filters: Vec<crate::innertube::QueueFilter>,
    /// A filter asked for and not landed yet: its playlist id, which its reply is matched on,
    /// and the river's state before, put back if the request fails.
    pub(crate) filtering: Option<(String, River)>,
}

impl Radio {
    /// The latent river for a hand-picked queue. `None` only for an empty queue.
    fn pending(tracks: &[Track]) -> Option<Self> {
        Some(Self {
            state: River::Pending,
            seed: nearest_song_id(tracks, tracks.len().checked_sub(1)?)?,
            chosen: tracks.iter().map(|t| t.video_id.clone()).collect(),
            filters: Vec::new(),
            filtering: None,
        })
    }

    /// For `playback.json`. A filter in flight is saved as the river it replaced. Only the
    /// chosen songs still in `queue` are written: the set otherwise keeps every id ever queued.
    fn to_persisted(&self, queue: &[Track]) -> crate::playback::PersistedRadio {
        let state = self
            .filtering
            .as_ref()
            .map_or(&self.state, |(_, before)| before);
        crate::playback::PersistedRadio {
            seed: self.seed.clone(),
            chosen: queue
                .iter()
                .filter(|t| self.chosen.contains(&t.video_id))
                .map(|t| t.video_id.clone())
                .collect(),
            pending: *state == River::Pending,
            token: match state {
                River::Open(t) => Some(t.clone()),
                _ => None,
            },
            filters: self
                .filters
                .iter()
                .map(|f| crate::playback::PersistedFilter {
                    label: f.label.clone(),
                    playlist_id: f.target.playlist_id.clone(),
                    params: f.target.params.clone(),
                    selected: f.selected,
                })
                .collect(),
        }
    }

    fn from_persisted(r: crate::playback::PersistedRadio) -> Self {
        Self {
            // A river that had run dry starts afresh from the queue's end, as a restored queue
            // always did: a new session is worth asking again.
            state: match (r.pending, r.token) {
                (false, Some(t)) => River::Open(t),
                _ => River::Pending,
            },
            seed: r.seed,
            chosen: r.chosen.into_iter().collect(),
            filters: r
                .filters
                .into_iter()
                .map(|f| crate::innertube::QueueFilter {
                    label: f.label,
                    target: crate::model::WatchTarget {
                        video_id: None,
                        playlist_id: f.playlist_id,
                        params: f.params,
                    },
                    selected: f.selected,
                })
                .collect(),
            filtering: None,
        }
    }
}

/// The nearest song (not video) at or before `from`, falling back to whatever sits at `from`
/// if the scanned run is video all the way back — seeding or reseeding the radio from a video
/// makes YouTube's own algorithm answer mostly video, which then gets just as likely to be
/// shuffled into next: a loop with nothing to break it. Anchoring on the nearest song instead
/// keeps the algorithm answering songs, so a stray video pick doesn't compound.
///
/// A local track is excluded outright, from both the primary scan and the fallback — its
/// `video_id` is a filesystem path, not something YouTube's radio endpoint can seed from, so
/// unlike a video (a real, just suboptimal, seed) there is no "last resort" that makes sense.
/// `None` when every candidate in range is local, which is what leaves an all-local queue with
/// no radio at all rather than one seeded from nonsense.
fn nearest_song_id(tracks: &[Track], from: usize) -> Option<String> {
    let candidates: Vec<&Track> = tracks[..=from]
        .iter()
        .filter(|t| t.local_path.is_none())
        .collect();
    candidates
        .iter()
        .rev()
        .find(|t| !t.wide)
        .or_else(|| candidates.last())
        .map(|t| t.video_id.clone())
}

/// A local playlist's tracks, resolved against the current scan — a `track_paths` entry with
/// no matching current track (its file moved or was removed since) is simply dropped rather
/// than shown as a broken row. Free rather than an `App` method so the sidebar's rail-menu
/// preview (`ui::sidebar::rail_list`) can build one from its own partial destructure of `App`
/// without needing the whole thing.
pub(crate) fn build_local_playlist(
    lp: &crate::local_playlists::LocalPlaylist,
    local_tracks: &[Track],
) -> crate::model::Playlist {
    let tracks: Vec<Track> = lp
        .track_paths
        .iter()
        .filter_map(|path| {
            let mut t = local_tracks
                .iter()
                .find(|t| t.local_path.as_deref() == Some(path.as_path()))?
                .clone();
            // Not carried from the scan (a plain local track has no playlist to be a member
            // of) — set fresh per playlist, exactly as YouTube's own `set_video_id` is only
            // ever a playlist row's property, not a track's. This is what makes "Remove from
            // playlist" offerable for a local playlist's rows.
            t.set_video_id = Some(Arc::from(path.to_string_lossy().as_ref()));
            Some(t)
        })
        .collect();
    crate::model::Playlist {
        id: lp.id.clone(),
        title: lp.title.clone(),
        description: None,
        author: String::new(),
        author_id: None,
        year: None,
        duration_text: None,
        track_count: Some(format!("{} tracks", tracks.len())),
        thumbnail: None,
        editable: true,
        in_library: false,
        library_add: None,
        library_remove: None,
        saveable: false,
        tracks: crate::model::Paged::new(tracks, None),
        source: crate::model::PlaylistSource::Local,
        extras: None,
    }
}

/// A local playlist as the `PlaylistRef` the sidebar and Library page already know how to
/// render — `editable: true` since it's always yours to add to, `track_count` computed rather
/// than fetched since there's no server round trip that would carry one.
fn local_playlist_ref(lp: &crate::local_playlists::LocalPlaylist) -> crate::model::PlaylistRef {
    crate::model::PlaylistRef {
        id: lp.id.clone(),
        title: lp.title.clone(),
        author: String::new(),
        author_id: None,
        track_count: Some(format!("{} tracks", lp.track_paths.len())),
        thumbnail: None,
        editable: true,
        source: crate::model::PlaylistSource::Local,
    }
}

/// What the river wants from the network.
enum RiverCall {
    /// The first page of a hand-picked queue's tail.
    Seed { seed: String },
    /// The next page of an open river.
    Follow { token: String, reseed: String },
}

/// Decide whether the river wants a page, borrowing its state rather than cloning it — the
/// chosen list is as long as the queue. A pending river seeds as soon as its queue starts
/// (YouTube Music shows the autoplay tail from the beginning), while an open river is followed
/// at the low-water mark: once fewer than [`RADIO_LOW_WATER`] tracks are `remaining` to play,
/// counted in play order, since under shuffle the playing track's place in the list says nothing
/// about how much is left.
fn river_call(
    radio: &Radio,
    current: Option<usize>,
    remaining: usize,
    queue: &[Track],
    autoplay: bool,
) -> Option<RiverCall> {
    let current = current?;
    if !autoplay {
        return None;
    }
    match &radio.state {
        River::Pending => Some(RiverCall::Seed {
            seed: nearest_song_id(queue, queue.len().checked_sub(1)?)?,
        }),
        River::Open(token) if remaining < RADIO_LOW_WATER => Some(RiverCall::Follow {
            token: token.clone(),
            reseed: nearest_song_id(queue, current)?,
        }),
        River::Open(_) | River::Dead => None,
    }
}

/// Where the river stands once a page lands: flowing while pages keep adding and handing back a
/// token, dead the moment either stops.
fn landed(added: usize, next: Option<String>) -> River {
    match next {
        Some(token) if added > 0 => River::Open(token),
        _ => River::Dead,
    }
}

/// How long a confirmation bubble stays up.
const TOAST_DURATION: Duration = Duration::from_secs(3);
/// How long an error toast stays: see [`App::toast_error`].
const ERROR_TOAST_DURATION: Duration = Duration::from_secs(5);

/// A bubble above the player bar: "Song will play next", "Saved to library", or the reason
/// something did not happen. Fire-and-forget — it clears itself, and only a live one asks for
/// another frame.
struct Toast {
    text: String,
    until: Instant,
    /// Painted in the danger colour. A refusal that looks exactly like a confirmation is worse
    /// than no message at all, which is what every failure used to get.
    error: bool,
}

/// How close to the end of the queue autoplay starts fetching more.
const RADIO_LOW_WATER: usize = 10;

/// Apply a playlist sort in place, keeping "Default" (and "Reverse") honest.
///
/// [`PlaylistSort::apply`] on its own only knows how to build a *new* order — asking it for
/// `Default` did nothing, which left the tracks exactly as some earlier sort had left them
/// rather than back the way YouTube actually served them, until the playlist was closed and
/// reopened; `Reverse` has the same problem, needing that same served order as something to
/// flip. `original_order` is the fix: the video-id order captured the first time this playlist
/// is sorted to anything at all, consulted here for both instead of either being a no-op.
fn apply_playlist_sort(
    sort: PlaylistSort,
    tracks: &mut [Track],
    original_order: &mut Option<Vec<String>>,
) {
    // Captured on whichever sort is chosen first, `Default`/`Reverse` included: either one
    // needs the served order as a baseline just as much as Title/Duration need it as something
    // to get back to later.
    let order =
        original_order.get_or_insert_with(|| tracks.iter().map(|t| t.video_id.clone()).collect());
    match sort {
        PlaylistSort::Default | PlaylistSort::Reverse => {
            let position: std::collections::HashMap<&str, usize> = order
                .iter()
                .enumerate()
                .map(|(i, id)| (id.as_str(), i))
                .collect();
            let rank = |t: &Track| position.get(t.video_id.as_str()).copied().unwrap_or(0);
            if sort == PlaylistSort::Reverse {
                tracks.sort_by_key(|t| std::cmp::Reverse(rank(t)));
            } else {
                tracks.sort_by_key(rank);
            }
        }
        _ => sort.apply(tracks),
    }
}

/// The two panels whose width can be dragged.
#[derive(Clone, Copy)]
enum PanelEdge {
    Sidebar,
    Queue,
}

impl PanelEdge {
    /// The panel's egui id, which its stored width and its drag handle hang off.
    fn name(self) -> &'static str {
        match self {
            PanelEdge::Sidebar => "sidebar",
            PanelEdge::Queue => "right-panel",
        }
    }

    /// Where this panel's drag state is kept (`panel_dragging`, `panel_save`).
    fn slot(self) -> usize {
        match self {
            PanelEdge::Sidebar => 0,
            PanelEdge::Queue => 1,
        }
    }
}

/// Write a panel's stored width: where egui's own record of it is changed, and the next pass lays
/// the panel out at it.
fn set_panel_width(ctx: &egui::Context, id: egui::Id, width: f32) {
    use egui::containers::panel::PanelState;
    let Some(state) = PanelState::load(ctx, id) else {
        return;
    };
    ctx.data_mut(|d| {
        d.insert_persisted(
            id,
            PanelState {
                outer_rect: egui::Rect::from_min_size(
                    state.outer_rect.min,
                    egui::vec2(width, state.outer_rect.height()),
                ),
            },
        );
    });
    ctx.request_repaint();
}

/// The width to put a panel back to, if any: what the listener chose, when the panel is more than a
/// few points narrower than it. Never while the panel is `settling`, the drag that set its width
/// has not finished or Settings does not hold the result yet: the old width is still the saved one
/// then, and putting it back undid every shrink.
fn restored_width(current: f32, want: f32, settling: bool) -> Option<f32> {
    (!settling && current + 4.0 < want).then_some(want)
}

/// Switch off the line egui draws along a resizable panel's edge under the pointer and while
/// dragging: it reads those two strokes from the parent's style, so blanking them there removes
/// it without touching anything drawn inside the panels.
fn quiet_panel_edges(ui: &mut egui::Ui) {
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.hovered.fg_stroke.width = 0.0;
    widgets.active.fg_stroke.width = 0.0;
}

/// How often, while offline, the app asks whether YouTube Music answers again.
const OFFLINE_PROBE: Duration = Duration::from_secs(10);

/// A save refused because the song is already in the playlist — what the dialog asks about.
#[derive(Clone)]
pub(crate) struct DuplicatePrompt {
    pub playlist_id: String,
    pub video_id: String,
    /// YouTube's own sentence for it, in the account's language.
    pub message: String,
}

/// Why a request to YouTube failed, in words a listener can act on, when the error text tells:
/// offline, a session YouTube no longer accepts, or YouTube asking to slow down. `None` when it
/// does not say, and the toast keeps its plain wording; the raw text only ever goes to the log.
fn failure_reason(error: &str) -> Option<&'static str> {
    if crate::api::looks_offline(error) {
        return Some("you're offline");
    }
    let e = error.to_ascii_lowercase();
    let any = |signs: &[&str]| signs.iter().any(|s| e.contains(s));
    if any(&["429", "too many requests", "rate limit", "ratelimit"]) {
        return Some("YouTube is busy, try again in a minute");
    }
    if any(&[
        "401",
        "403",
        "unauthorized",
        "forbidden",
        "sign in",
        "signed out",
        "login required",
    ]) {
        return Some("sign in again");
    }
    None
}

/// A failure toast: `what` failed, and why when [`failure_reason`] can tell.
fn failure_text(what: &str, error: &str) -> String {
    match failure_reason(error) {
        Some(reason) => format!("{what}: {reason}"),
        None => what.to_owned(),
    }
}

/// YouTube's own sentence for refusing a playlist edit, when the error is one ("This playlist
/// can't be edited") rather than a request failure: short, and nothing technical in it.
fn youtube_sentence(error: &str) -> Option<&str> {
    let s = error.trim();
    let e = s.to_ascii_lowercase();
    let technical = [
        "error", "http", "://", "status", "{", "request", "json", "failed",
    ]
    .iter()
    .any(|t| e.contains(t));
    (!s.is_empty() && s.len() <= 120 && !technical).then_some(s)
}

/// A playlist's count line ("52 tracks", "4,782 songs") moved by `delta`, keeping YouTube's own
/// wording. English nouns follow the number ("1 song", "2 songs"), and an empty "No tracks"
/// counts up from nothing; text in another language keeps its noun as it was, and text with no
/// number in it otherwise comes back unchanged.
fn bump_count(text: &str, delta: i64) -> String {
    for noun in ["tracks", "songs"] {
        if text == format!("No {noun}") {
            return if delta > 0 {
                bump_count(&format!("0 {noun}"), delta)
            } else {
                text.to_owned()
            };
        }
    }
    let Some(start) = text.find(|c: char| c.is_ascii_digit()) else {
        return text.to_owned();
    };
    let len = text[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == ','))
        .unwrap_or(text.len() - start);
    let digits = &text[start..start + len];
    let Ok(n) = digits.replace(',', "").parse::<i64>() else {
        return text.to_owned();
    };
    let n = (n + delta).max(0);
    // Thousands separators only where YouTube wrote them.
    let shown = if digits.contains(',') {
        let raw = n.to_string();
        let mut out = String::new();
        for (i, c) in raw.chars().enumerate() {
            if i > 0 && (raw.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(c);
        }
        out
    } else {
        n.to_string()
    };
    let rest = &text[start + len..];
    let rest = match rest {
        " track" | " tracks" => {
            if n == 1 {
                " track"
            } else {
                " tracks"
            }
        }
        " song" | " songs" => {
            if n == 1 {
                " song"
            } else {
                " songs"
            }
        }
        other => other,
    };
    format!("{}{shown}{rest}", &text[..start])
}

/// Move `playlist_id`'s count by `delta` in every copy of it on screen: its open page and its row
/// in the library list, which the sidebar and the Library page both draw. Pure, for the test.
fn bump_counts_in(
    page: Option<&mut crate::model::Playlist>,
    rows: &mut [crate::model::PlaylistRef],
    playlist_id: &str,
    delta: i64,
) {
    let bare = crate::model::bare_playlist_id;
    let id = bare(playlist_id);
    let bump = |count: &mut Option<String>| {
        if let Some(text) = count.as_deref() {
            *count = Some(bump_count(text, delta));
        }
    };
    if let Some(page) = page.filter(|p| bare(&p.id) == id) {
        bump(&mut page.track_count);
    }
    for row in rows.iter_mut().filter(|r| bare(&r.id) == id) {
        bump(&mut row.track_count);
    }
}

/// The local half of a playlist drag-reorder: move `items[from]` to sit immediately before
/// whatever is at `to`. Returns the pair of playlist item ids (`setVideoId`s) the server call
/// needs — the moved track, then the one it now precedes — or `None` when there is nothing to
/// do: `from == to`, either index is out of range, or either track carries no item id (not a
/// real playlist row).
fn reorder_tracks(items: &mut Vec<Track>, from: usize, to: usize) -> Option<(Arc<str>, Arc<str>)> {
    if from == to || from >= items.len() || to >= items.len() {
        return None;
    }
    // The item that will still be at `to` once the drag has landed *before* it — read now,
    // since removing `from` first would shift what index `to` itself pointed at when `from < to`.
    let before_set_video_id = items[to].set_video_id.clone()?;
    let set_video_id = items[from].set_video_id.clone()?;
    let track = items.remove(from);
    let insert_at = if from < to { to - 1 } else { to };
    items.insert(insert_at, track);
    Some((set_video_id, before_set_video_id))
}

/// Whether this is a Wayland session — `$WAYLAND_DISPLAY`, the same environment variable every
/// cross-platform GUI toolkit checks for this, since there is no portable API for it.
pub(crate) fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Ask the compositor to bring the window forward.
///
/// `ViewportCommand::Focus` is a no-op on Wayland (winit's own `focus_window` says so outright),
/// so the only lever there is `RequestUserAttention`: winit turns it into an `xdg_activation`
/// request, which KWin answers by activating the window when it can — no user interaction since
/// the session started, no other active window — and by making the taskbar entry demand
/// attention when it cannot. On every other platform `Focus` works, and an attention request
/// would only flash the taskbar on top of a successful focus.
///
/// On Plasma's Wayland the window is also restored through KWin itself (`kwin`): a minimised
/// window, which is what "hide to tray" leaves there, otherwise never comes back.
pub(crate) fn request_foreground(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    if is_wayland() {
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Critical,
        ));
        #[cfg(target_os = "linux")]
        if crate::kwin::is_plasma() {
            crate::kwin::set_hidden(false);
        }
    }
}

/// The command that actually hides or shows the window, on whichever platform this is.
///
/// `ViewportCommand::Visible` is a *documented no-op on Wayland* — winit's own Wayland backend
/// says so outright ("Not possible on Wayland") — which is why "Hide tray on close" appeared
/// to do nothing there: the veto fired, `Visible(false)` was sent, and Wayland silently
/// ignored it, leaving the window exactly as it was. `Minimized` is the closest real substitute
/// Wayland's xdg-shell actually offers, but only in the hide direction — winit's own
/// `set_minimized(false)` is *also* a documented no-op there ("You can't unminimize the window
/// on Wayland"), and so is `ViewportCommand::Focus`'s `focus_window()`. So on Wayland, showing
/// the window again from the tray is a best-effort request only: the reliable way back is the
/// user's own taskbar/pager entry for the minimized window, the same as any other Wayland app
/// behind this exact protocol gap.
///
/// macOS uses `Visible(false)` like X11 and Windows. It stops the window receiving frames while
/// hidden, which is why the tray handlers raise the window through AppKit themselves (see the
/// `tray` module docs) instead of relying on the next frame.
fn hide_command(hide: bool) -> egui::ViewportCommand {
    hide_command_for(is_wayland(), hide)
}

/// Hide the window to the tray, or show it again: [`hide_command`], and on Plasma's Wayland the
/// rest of the hide through KWin (`kwin`), which takes the window off the taskbar, the pager and
/// Alt+Tab, as on Windows. The way back is [`request_foreground`], which every show is followed
/// by and which restores it there.
fn set_window_hidden(ctx: &egui::Context, hide: bool) {
    ctx.send_viewport_cmd(hide_command(hide));
    #[cfg(target_os = "linux")]
    if hide && is_wayland() && crate::kwin::is_plasma() {
        crate::kwin::set_hidden(true);
    }
}

/// [`hide_command`], parameterised on the platform check instead of reading it — so the two
/// branches are each a plain unit test rather than something only checkable by hand on both
/// kinds of session.
fn hide_command_for(wayland: bool, hide: bool) -> egui::ViewportCommand {
    if wayland {
        egui::ViewportCommand::Minimized(hide)
    } else {
        egui::ViewportCommand::Visible(!hide)
    }
}

pub struct App {
    pub(crate) palette: Palette,
    /// The themes folder as last read. Settings lists these; `settings.custom_theme` picks one.
    pub(crate) custom_themes: theme::custom::Themes,
    /// Raised by the folder watcher when a theme file is added, removed or saved.
    theme_watch: theme::custom::Watcher,
    /// Whether the window had focus on the last frame, to notice it coming back.
    was_focused: bool,
    history: History,
    pub(crate) decorated: bool,
    startup_timing: bool,
    /// The tray icon, while "Hide tray on close" is on — dropping it removes the icon. `None`
    /// the rest of the time, so a listener who never changes the setting never gets a tray icon.
    tray: Option<crate::tray::Tray>,
    /// macOS: the Playback, View, Window and Help menus in the menu bar. `None` elsewhere, and
    /// if AppKit would not take them.
    #[cfg(target_os = "macos")]
    menu_bar: Option<crate::menu_bar::MenuBar>,
    /// We hid the window ourselves (tray-icon close, or the tray menu's own Show/Hide) rather
    /// than the OS reporting it occluded or minimized — the two are different questions, and
    /// this is the one that decides whether the repaint heartbeat below needs to keep running.
    hidden: bool,
    /// The tray's own Quit was chosen: the next close request must actually go through rather
    /// than being caught by the hide-on-close veto, which would otherwise just hide it again.
    quitting: bool,
    /// Armed by `--autostart` for the first frame: ask the compositor to bring the window
    /// forward (see `request_foreground`). Disarmed after that one attempt.
    autostart_attention: bool,

    pub(crate) backend: Backend,
    pub(crate) player: Option<Player>,
    /// mpv's state, read at the top of the frame for the player bar and lyrics, and again after
    /// the panels (whose clicks can change it) for the queue advance and the OS publishers. Each
    /// read is four blocking property calls into mpv, so nothing else asks it per frame.
    pub(crate) player_state: crate::player::PlayerState,
    /// Set when libmpv could not be initialised, so the UI can explain instead of silently
    /// doing nothing when you press play.
    pub(crate) player_error: Option<String>,

    pub(crate) query: String,
    /// When the query last changed, for debouncing.
    pub(crate) query_changed_at: Option<Instant>,
    /// Autocomplete for the search field: the items, the query they belong to (a staleness guard
    /// like `searched`), and where the keyboard cursor sits among them.
    pub(crate) suggestions: Vec<String>,
    pub(crate) suggestions_for: Option<String>,
    /// Where the arrow keys have moved the suggestion highlight, if anywhere. `None` — the
    /// state the list opens in — means nothing is highlighted and Enter searches exactly what
    /// was typed, so the first Down reaches the *first* suggestion rather than skipping it.
    pub(crate) suggestion_cursor: Option<usize>,
    /// What was searched before, offered by the search box while it is focused and empty.
    pub(crate) search_history: crate::search_history::SearchHistory,
    /// Escape closed the recent-searches list. Without this it would reopen on the next frame,
    /// the box still being focused and empty; typing or refocusing clears it.
    pub(crate) recent_dismissed: bool,
    /// A search waiting for `poll_search` to fire it. Search is submit-driven — Enter, a chosen
    /// suggestion, or the `--search` CLI — never a keystroke.
    pending_search: bool,
    pub(crate) searched: String,
    pub(crate) searching: bool,
    pub(crate) results: SearchResults,
    /// Bumped every time `results` is replaced, so `ui::search`'s `All`-scope filtered index list
    /// can cache itself against this instead of re-filtering `results.items` every frame.
    pub(crate) results_generation: u64,
    pub(crate) status: Option<String>,
    /// A request failed for want of a connection, and none has got through since. Drives the
    /// offline banner and the quiet probe that notices the connection coming back.
    pub(crate) offline: bool,
    /// The signed-in account holds YouTube Premium, per its memberships page. Downloads need it,
    /// as they do on YouTube. False until the check answers, and for signed-out use.
    pub(crate) premium: bool,
    /// When the next probe may go out while offline.
    offline_probe_at: Option<Instant>,
    /// A probe is in flight; its answer arrives as `Event::Connection`.
    offline_probing: bool,
    /// The page drew the full offline screen last frame, so the banner stays out of its way.
    pub(crate) offline_page_shown: bool,
    /// Whether each resizable panel's edge was being dragged last frame, so the width is saved
    /// once, when the drag ends — not on every frame of it, and not when the window's own size
    /// clamps the panel.
    panel_dragging: [bool; 2],
    /// Frames left in which to save a just-dragged width: the release frame and the one after,
    /// in case egui records the final size a frame late. An unchanged save writes nothing.
    panel_save: [u8; 2],
    /// The panels' widths have been set from Settings this session. See `seed_panel_widths`.
    panels_seeded: bool,
    /// The window minimum width last asked of the compositor, so a change is sent once, not every
    /// frame. See `fit_window_to_shell`.
    min_window_w: f32,
    /// How many times the window has been asked to grow to `min_window_w` since that last changed;
    /// at [`WINDOW_FIT_TRIES`] it is done, wide enough or not.
    window_fit_tries: u8,
    /// Where the check for a window restored fullscreen has got to.
    fullscreen_check: FullscreenCheck,
    /// Which kind of result the search page is filtered to. `All` is a view over `results`
    /// (never a second request); every other scope drives a dedicated per-category fetch, since
    /// the mixed ranked response often surfaces far fewer of that kind than actually exist — see
    /// [`SearchScope`]'s own doc comment.
    pub(crate) search_scope: SearchScope,
    /// The dedicated per-category fetch's results, valid only while `scoped_for` names the
    /// `(query, scope)` pair currently on screen — mirrors the `artist_for` staleness-guard
    /// pattern used elsewhere in this file.
    pub(crate) scoped_results: Vec<Entry>,
    pub(crate) scoped_loading: bool,
    pub(crate) scoped_error: Option<String>,
    /// `status` holds a failed search's error chain rather than a message for the page.
    pub(crate) search_failed: bool,
    scoped_for: Option<(String, SearchScope)>,
    /// Cache for `ui::search`'s `All`-scope filtered index list: the `results_generation` it was
    /// computed for, and the indices themselves. Recomputed only when the generation moves on.
    pub(crate) search_all_indices: Option<(u64, Vec<usize>)>,

    /// The artist page being shown, plus whether a fetch is outstanding / failed.
    ///
    /// `artist_for` is the id the other three fields describe. Without it "nothing loaded" and
    /// "a *different* artist is loaded" look identical, which is what let Back show the wrong
    /// page and let a stale reply latch `artist_loading` forever.
    pub(crate) artist: Option<crate::model::Artist>,
    pub(crate) artist_loading: bool,
    pub(crate) artist_error: Option<String>,
    artist_for: Option<String>,
    /// The album page being shown.
    pub(crate) album: Option<crate::model::Album>,
    pub(crate) album_loading: bool,
    pub(crate) album_error: Option<String>,
    album_for: Option<String>,
    /// Bumped on `forget_account_data` and every real sign-in/sign-out transition. Threaded
    /// through `fetch_library`/`fetch_library_playlists`/`fetch_home`/`fetch_accounts`'s events
    /// so a reply already in flight when the account changed underneath it — the window
    /// `forget_account_data` itself cannot close, since it cannot cancel a request already
    /// sent — is dropped instead of quietly repopulating freshly-cleared state with the
    /// previous account's data.
    pub(crate) account_epoch: u64,
    /// The id of the album/playlist card most recently asked to play — `Event::CardTracksReady`
    /// checks this before acting, the same "is this reply for what I most recently asked for"
    /// guard `album_for`/`playlist_for` already give the page loaders, since two cards' play
    /// buttons clicked close together race each other's fetch exactly the same way.
    pending_card_play: Option<String>,
    /// A sidebar or card menu item picked before that page's songs had loaded: the page's id
    /// and what to do. Done when the fetch lands (`cache_album` / `cache_rail_playlist`); a newer
    /// pick replaces an older one, as `pending_card_play` does.
    pending_page_action: Option<(String, crate::ui::PageThen)>,

    /// The app's own icon, as a texture for the Settings page's branding and the startup splash.
    /// Loaded once, lazily (whichever of the two needs it first fills it in rather than doing it
    /// at startup), and kept for the rest of the session.
    pub(crate) logo: Option<egui::TextureHandle>,

    /// Which browsers have a cookie profile on disk, for the sign-in page's "Import from a
    /// browser" list. Cached the same way as `logo` above (lazily, on first Settings visit) since
    /// this cannot change without a restart or a new browser profile being created mid-session.
    pub(crate) detected_browsers: Option<Vec<crate::browser_cookies::Browser>>,
    /// A browser import is under way: the Import buttons read "Importing…" until it lands.
    pub(crate) browser_importing: bool,

    /// What the Settings page shows about authentication.
    pub(crate) auth: crate::auth::AuthState,
    /// Whether the startup splash may still cover frames. Armed at construction only when a
    /// stored session is being checked, released for good the first frame it is not wanted — auth
    /// resolved and, when the landing route is Home, its first feed arrived or failed — so it can
    /// never reappear mid-session. See [`Self::splash_visible`].
    startup_splash: bool,
    /// Why the last sign-in attempt failed. Kept apart from `auth`, so a refused attempt does not
    /// make a working session look lost.
    pub(crate) sign_in_error: Option<String>,
    /// The signed-in user's library, in sections.
    pub(crate) library: crate::model::Library,
    pub(crate) library_loading: bool,
    pub(crate) library_error: Option<String>,
    pub(crate) library_loaded: bool,
    pub(crate) library_sort: LibrarySort,
    /// Every song the account has liked, built by a background page-through of Liked Music and
    /// cached to disk. There is no liked-by-artist endpoint — see `crate::liked`'s own doc — so
    /// this is what the artist page's "You Liked" section and the player bar's backfill read.
    pub(crate) liked: crate::liked::Index,
    /// A liked scan is in flight.
    pub(crate) liked_scanning: bool,
    /// The in-flight scan is a full one: its pages accumulate in `liked_buffer` and are swapped
    /// in when it completes, so the displayed index never empties mid-scan.
    liked_full_scan: bool,
    liked_buffer: Vec<crate::model::Track>,
    /// The index changed since it was last written out.
    liked_dirty: bool,
    last_liked_save: Instant,
    /// Every track found under `settings.local_folders` by the last full rescan — there is no
    /// incremental update, a changed folder list just rescans everything again.
    pub(crate) local_tracks: Vec<crate::model::Track>,
    /// Bumped whenever `local_tracks` is replaced, so `ui::library::local_view` can cache what
    /// the Local tab shows instead of rebuilding it every frame the tab is open.
    pub(crate) local_tracks_generation: u64,
    /// Cache for `ui::library::local_view`: what the Local tab shows for the folder open in it.
    pub(crate) local_view_cache: Option<LocalViewCache>,
    pub(crate) local_scanning: bool,
    pub(crate) local_scan_error: Option<String>,
    /// Which folder the Library's Local tab is currently browsing, one path segment per level,
    /// relative to whichever configured root it sits under. Empty is the top level.
    pub(crate) local_browse_path: Vec<String>,
    /// Local playlists: loaded once at startup and mutated directly, since there is no server
    /// round trip to hide behind an `Event` — see `local_playlists.rs`'s own doc comment.
    /// Mirrored into `library.playlists.items` by `resync_local_playlists` so the sidebar and
    /// the Library page see one merged list without their own code knowing two sources exist.
    pub(crate) local_playlists: Vec<crate::local_playlists::LocalPlaylist>,
    /// The playlist page being shown.
    pub(crate) playlist: Option<crate::model::Playlist>,
    pub(crate) playlist_loading: bool,
    /// The Suggestions under the open playlist, once fetched. Only your own playlists get them.
    pub(crate) playlist_suggestions: Option<crate::innertube::Suggestions>,
    /// "Related playlists" under someone else's playlist, from its extras continuation.
    pub(crate) playlist_related: Vec<crate::model::Shelf>,
    /// A Suggestions fetch — first or Refresh — is in flight.
    pub(crate) playlist_suggestions_loading: bool,
    pub(crate) playlist_error: Option<String>,
    playlist_for: Option<String>,
    /// How the open playlist's track list is currently shown — client-side and session-only,
    /// reset whenever a different playlist opens (see [`PlaylistSort`]'s own doc comment for
    /// why this can't be a server order the way [`LibrarySort`] is).
    pub(crate) playlist_sort: PlaylistSort,
    /// A sort was chosen before the playlist had fully loaded: the remaining pages are being
    /// fetched, and the sort applies once `tracks.next` goes `None`.
    pub(crate) playlist_sort_pending: bool,
    /// The track order YouTube actually served, by video id — captured the first time this
    /// playlist is sorted to anything other than [`PlaylistSort::Default`], since choosing
    /// Default again has to put it back and sorting in place is otherwise destructive. `None`
    /// until then (no snapshot needed if nobody has left Default yet), and reset whenever a
    /// different playlist opens.
    playlist_original_order: Option<Vec<String>>,
    /// Full-list expansion of one artist shelf: which artist and which shelf, while it is in
    /// flight. Per shelf rather than global, because "Singles & EPs", "Videos" and "Playlists"
    /// each have their own expansion.
    pub(crate) artist_expand: Option<(String, String)>,
    pub(crate) artist_expand_loading: bool,
    pub(crate) artist_expand_error: Option<String>,
    /// How the "Liked Songs by <artist>" page orders its rows. Client-side like the playlist
    /// sort, but it needs no snapshot to return to Default: the liked index always holds the
    /// served order, and each frame's rows are rebuilt from it. Sticky across artists — it is
    /// about the list style, not the one artist.
    pub(crate) artist_liked_sort: PlaylistSort,
    /// The home feed, as YouTube serves it. Replaces the moods-and-history page we used to
    /// assemble ourselves, which used none of YouTube's ranking.
    ///
    /// Behind an `Arc` like every other feed here: the page renderers need `&mut App` while
    /// they draw, so they take a snapshot for the frame, and a refcount bump is what that
    /// costs instead of a deep clone of every shelf on every frame. A write during a frame
    /// goes through `Arc::make_mut`, which copies once and leaves the frame's snapshot alone.
    pub(crate) home: Option<Arc<Vec<crate::model::Shelf>>>,
    pub(crate) home_loading: bool,
    /// Where the feed continues past what is loaded; the next page is fetched as the listener
    /// scrolls toward the end. `None` once YouTube has sent it all.
    home_next: Option<String>,
    pub(crate) home_more_loading: bool,
    /// When the feed on screen was fetched: a visit to Home after [`HOME_STALE`] reloads it.
    home_loaded_at: Option<Instant>,
    /// A stale feed is being replaced: the old shelves stay up until the new ones land.
    home_refreshing: bool,
    /// The full list behind the "Show all" page open now, by shelf title, when that shelf had a
    /// "more" target to fetch it from. Without one the page shows the shelf's own items.
    pub(crate) home_shelf_full: Option<(String, Arc<crate::model::Shelf>)>,
    home_shelf_loading: Option<String>,
    pub(crate) home_error: Option<String>,
    /// The mood bar above the feed (Workout, Relax, …). Parsed from the same browse.
    pub(crate) home_chips: Vec<crate::model::HomeChip>,
    /// The selected chip's params. `None` is the default feed.
    pub(crate) home_filter: Option<String>,
    pub(crate) mood_playlists: Option<crate::model::MoodPlaylists>,
    pub(crate) mood_playlists_loading: bool,
    pub(crate) mood_playlists_error: Option<String>,
    pub(crate) mood_playlists_params: Option<String>,

    pub(crate) now: NowPlaying,
    /// The playable list the transport operates on, the track playing, its restored position,
    /// the shuffle order and the way back: see [`crate::queue`]. Search results, an artist's top
    /// songs and an album's tracks each become the queue when one of them is started.
    pub(crate) q: PlayQueue,
    /// Set once we have asked mpv to play, so we can tell "not started yet" from "track ended".
    pub(crate) playback_started: bool,
    /// Distinguishes a finished track from an attempt that never produced audio; see [`StallGuard`].
    stall: StallGuard,
    /// The generation of the most recent `Backend::play`/`play_video` call — every
    /// `Event::Resolving`/`Playing`/`PlaybackFailed` carries the generation it was started
    /// under, and one whose generation doesn't match this is a reply to a play that has since
    /// been superseded (Next/Previous, another queue row, the video toggle, all firing again
    /// before the first one's yt-dlp resolve finished) and must not overwrite `now`.
    play_generation: u64,
    /// Whether the most recent start went through `Backend::play_video` rather than `play`.
    /// `PlaybackFailed` uses it to fall back to the song's audio instead of leaving the video
    /// view over a frozen frame when the video path is the one that broke.
    play_is_video: bool,
    /// Autoplay river state, opened by Start Mix. `None` for a hand-picked queue.
    pub(crate) radio: Option<Radio>,
    /// What the transport does at the end of the queue.
    pub(crate) repeat: RepeatMode,
    /// A river extension is in flight. Guards the low-water trigger, which runs every frame.
    radio_loading: bool,
    /// The Up Next panel is open.
    pub(crate) upnext_open: bool,
    /// The lyrics panel is open. Mutually exclusive with the queue: they share the right panel.
    pub(crate) lyrics_open: bool,
    /// The fullscreen now-playing view: whether it is open, and what it remembers.
    pub(crate) fs: ui::fullscreen::State,
    /// The loaded lyrics for [`Self::lyrics_for`], if any.
    pub(crate) lyrics: Option<crate::model::Lyrics>,
    pub(crate) lyrics_loading: bool,
    pub(crate) lyrics_error: Option<String>,
    /// Which video the current lyrics (or in-flight request) belong to, so a reply for a track
    /// already left behind is dropped.
    lyrics_for: Option<String>,
    /// The playlist rename editor, when open.
    pub(crate) playlist_edit: Option<PlaylistEditor>,
    /// A playlist awaiting delete confirmation: its id and title.
    pub(crate) playlist_delete: Option<(String, String)>,
    /// A save YouTube refused because the song is already in that playlist, awaiting the
    /// "Add anyway" / "Cancel" answer.
    pub(crate) playlist_duplicate: Option<DuplicatePrompt>,
    /// A playlist whose editor should open as soon as its page loads (Edit from the rail).
    edit_on_load: Option<String>,
    /// The title a rename is reverting to if YouTube refuses it: (id, old title).
    edit_revert: Option<(String, String)>,
    /// Playlists fetched for the sidebar's context menu, so it can offer the page's track
    /// actions (shuffle, mix, queue, save) without opening the page. Bounded.
    pub(crate) rail_cache: std::collections::HashMap<String, crate::model::Playlist>,
    /// The same idea for albums: kept so an album card's context menu can seed its mix, queue
    /// and save items once the right-click prefetch lands. Bounded like the rail cache.
    pub(crate) album_cache: std::collections::HashMap<String, crate::model::Album>,
    /// Which library section the Library page shows.
    pub(crate) rail_filter: RailFilter,
    /// The name being typed into the rail's new-playlist field, while it is open.
    pub(crate) new_playlist: Option<String>,
    /// What that same field commits to once confirmed — a real YouTube playlist by default, or
    /// (when opened from the Library's Local tab's "Save as playlist") a local one seeded with
    /// a specific folder's tracks. One shared modal (`ui::sidebar::new_playlist_popup`) rather
    /// than a second one, since both are just "type a name, confirm."
    pub(crate) new_playlist_kind: NewPlaylistKind,
    pub(crate) explore: Option<Arc<Vec<crate::model::Shelf>>>,
    pub(crate) explore_loading: bool,
    pub(crate) explore_error: Option<String>,
    pub(crate) explore_links: Vec<crate::model::ExploreLink>,
    pub(crate) releases: Option<Arc<Vec<crate::model::Shelf>>>,
    pub(crate) releases_loading: bool,
    pub(crate) releases_error: Option<String>,
    pub(crate) charts: Option<Arc<crate::model::ChartsFeed>>,
    pub(crate) charts_loading: bool,
    pub(crate) charts_error: Option<String>,
    pub(crate) charts_params: Option<String>,
    pub(crate) moods: Option<Arc<Vec<crate::model::MoodSection>>>,
    pub(crate) moods_loading: bool,
    pub(crate) moods_error: Option<String>,
    pub(crate) history_shelves: Option<Arc<Vec<crate::model::Shelf>>>,
    pub(crate) history_loading: bool,
    pub(crate) history_error: Option<String>,
    /// The History visit in progress. Bumped on every entry to the page and whenever the data is
    /// invalidated, and carried by every fetch so a reply for a superseded visit can be dropped.
    history_epoch: u64,
    /// The visit the page last settled on: equal to `history_epoch` once the current visit's
    /// fetch has landed — or failed with a list already on screen — which is what stops the
    /// per-frame loader refetching.
    history_loaded_epoch: u64,
    /// The volume to restore on unmute. Updated whenever you set a nonzero volume.
    pub(crate) last_volume: f64,
    /// What `playback.json` was last written with, reduced to what decides whether it is stale.
    /// `None` until the first write.
    playback_saved: Option<SavedPlayback>,
    /// When `playback_saved` was last compared against the live state; the check runs once a
    /// second, not every frame.
    last_playback_check: Instant,
    /// Muted by the button versus turned down to zero. `last_volume` remembers what to come back
    /// to.
    pub(crate) muted: bool,
    /// When the page's current load began, in egui time; see `publish_loading`.
    page_loading_since: Option<f64>,
    /// When the current song started resolving; see `publish_loading`.
    resolving_since: Option<Instant>,
    /// A confirmation bubble above the player bar, if one is live.
    toast: Option<Toast>,
    /// Play the first result as soon as a search lands (`--autoplay`).
    autoplay: bool,
    pub(crate) images: Images,
    /// User preferences. Everything in here changes something the app actually does.
    pub(crate) settings: crate::settings::Settings,
    /// The channels this account can browse as. Empty until Settings asks.
    pub(crate) accounts: Vec<crate::model::Account>,
    /// Channels opened from a link that names a person rather than an artist (a playlist's
    /// owner). The page cannot tell, before it loads, which kind of channel it is opening; this
    /// is how its skeleton takes the profile's shape instead of the artist's.
    profile_ids: std::collections::HashSet<String>,
    /// Live counters behind `--stats`; free when off.
    pub(crate) stats: Stats,
    /// Print one line of counters and exit, once this much time has passed. Used by
    /// `scripts/perf.sh` to measure a *loaded* app rather than an empty one.
    stats_after: Option<Duration>,
    /// `--screenshot`: where to save the window, when, and whether it has been asked for yet.
    screenshot: Option<(std::path::PathBuf, ShotWhen, bool)>,

    /// Debug autoscroll offset, advanced each frame when `--autoscroll` is set.
    pub(crate) autoscroll: Option<f32>,
    autoscroll_max: f32,
    /// Set for one frame after opening a page, so it starts at the top. Back and Forward do not
    /// set it: returning to a page should return you to where you were on it.
    pub(crate) scroll_to_top: bool,
    /// Set for one frame after jumping to a track from the Queue panel, so the panel scrolls
    /// back to its own top. Not set by every way `current` can change — skip and autoplay
    /// should not move the list while the listener is watching it scroll.
    pub(crate) queue_scroll_reset: bool,
    /// Set for one frame when the Lyrics panel opens or its song changes, so it starts at the
    /// top rather than wherever the previous song's lyrics happened to be left scrolled to.
    pub(crate) lyrics_scroll_reset: bool,
    /// The active lyric line last scrolled to, so the panel only scrolls again once that
    /// actually changes — not on every repaint a paused, manually-scrolled view would otherwise
    /// get undone by.
    pub(crate) lyrics_active: Option<usize>,
    /// A page's description, while its Info popup is open. Set by the header ••• menu's Info
    /// item; cleared by the popup's Close button.
    pub(crate) page_info: Option<String>,
    /// The keyboard-shortcut cheatsheet is open. Opened by `?`/F1 and the account menu, closed by
    /// the modal itself.
    pub(crate) help_open: bool,
    /// The open tab of the Settings page. Kept for the session, not saved.
    pub(crate) settings_tab: ui::settings::SettingsTab,
    /// The "Order of homepage" window is open.
    pub(crate) home_order_open: bool,
    /// The "Paste cookies" window is open, and what has been pasted into it so far. Cleared the
    /// moment the window closes or hands it over.
    pub(crate) paste_cookies_open: bool,
    pub(crate) pasted_cookie: String,
    /// A crash the previous run left behind, offered once on this launch and then cleared.
    pub(crate) crash_report: Option<crate::crash::Crash>,
    /// What the update check found, and how far an install has got (see `crate::update`).
    pub(crate) update: ui::update::State,
    /// What is downloaded to `~/Music/refryt`, indexed by video id. Pushed into the backend so a
    /// downloaded track plays from disk.
    pub(crate) downloads: crate::downloads::Index,
    /// The downloaded ids, shared with the row menus through egui's temp data. Rebuilt only when
    /// `downloads` changes; each frame hands egui another reference to the same set.
    downloaded_ids: std::sync::Arc<std::collections::HashSet<String>>,
    /// The Library's Downloads tab, built from `downloads` at the same moments as the ids.
    pub(crate) downloaded_tracks: std::sync::Arc<Vec<Track>>,
    /// In-flight download progress, by video id (0–100).
    pub(crate) download_progress: std::collections::HashMap<String, f32>,

    /// The content card's inner rect this frame: what the page is drawn into, before the
    /// card's own margin. Pages that paint to the card's edges read it rather than guessing
    /// from a clip rect. See where it is set in [`Self::ui`].
    pub(crate) content_rect: egui::Rect,
    /// The KDE blur-behind connection. Connected only on a KDE Wayland session that offers the
    /// protocol; `blur.available()` is false everywhere else — an X11 session, GNOME, a
    /// non-Linux build — and the Settings row for it disables itself off that rather than
    /// offering a switch that does nothing.
    pub(crate) blur: crate::blur::Blur,
    /// Wayland: activates the window with a tray click's token, the way a hidden window comes
    /// back where KWin's scripting cannot be reached (the Flatpak) or on other compositors.
    #[cfg(target_os = "linux")]
    activation: crate::activation::Activation,
    /// macOS: the system's window buttons, kept in the shell's top-left corner. A no-op
    /// elsewhere.
    traffic_lights: crate::chrome::TrafficLights,
    /// Whether the sidebar was drawn folded to the icon rail last frame, for placing the traffic
    /// lights over it.
    pub(crate) sidebar_rail: bool,
    /// Whether the traffic lights are on screen this frame (macOS, windowed or zoomed), so the
    /// sidebar keeps their corner clear. False in fullscreen, where the system hides them and the
    /// corner goes back to the nav.
    pub(crate) traffic_lights_shown: bool,
    /// OS media controls and media keys, when the platform offers them. `None` on a session with
    /// no media service (e.g. no D-Bus session bus); the app runs the same without it.
    pub(crate) media: Option<crate::media::Media>,
    /// The Discord Rich Presence worker, while the setting is on and an application id is set.
    /// Dropping it tells the worker to clear the presence and stop.
    discord: Option<crate::discord::Discord>,
    /// What was last handed to Discord — the track's id, whether it was playing, and its title —
    /// so the presence is only resent when something material changes rather than every frame.
    presence_key: Option<(Option<String>, bool, String)>,
    /// What the OS media widget was last handed, reduced to what it shows. `publish_media` runs
    /// every frame; building and sending the full snapshot (every string cloned, then cloned
    /// twice more inside playwire) only happens when this changes.
    media_key: Option<MediaKey>,
    /// Where the track was when the presence was last sent, and when: together they say whether
    /// the progress bar Discord is drawing has been broken by a seek, which needs a resend.
    presence_position: f64,
    presence_sent_at: Instant,
    /// Kept for the video view, which creates its mpv render context lazily.
    ctx: egui::Context,
    /// Whether the in-app music video is showing in the content card.
    pub(crate) video_open: bool,
    /// The software render surface, created the first time a video is opened.
    pub(crate) video: Option<crate::video::Video>,
}

impl Drop for App {
    /// mpv's render context must be freed *before* the mpv core is destroyed — `render.h` calls
    /// the other order undefined behavior — and `video` is declared after `player`, so without
    /// this the player's context would go first. Clearing the field here runs before any field
    /// is dropped.
    fn drop(&mut self) {
        // Brackets the teardown in the log: the exit line in `main` comes after every field has
        // dropped, so a quit that still drags can be pinned on whichever drop stops logging.
        tracing::info!("app: dropping");
        self.video = None;
        tracing::info!("app: video surface freed");
    }
}

/// The window's native handle for the OS media service — SMTC attaches to a window on Windows.
/// `None` everywhere else, where `playwire` needs no handle.
#[cfg(target_os = "windows")]
fn window_hwnd(cc: &eframe::CreationContext<'_>) -> Option<u64> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(w) => Some(w.hwnd.get() as u64),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn window_hwnd(_: &eframe::CreationContext<'_>) -> Option<u64> {
    None
}

/// Whether the History page needs a fetch for the visit it is on: signed in, nothing already in
/// flight, no latched error, and the shelves on screen (if any) loaded under a different epoch.
/// Split out from `load_history` so the "once per visit" rule is testable without an `App`.
fn history_needs_load(
    signed_in: bool,
    loading: bool,
    failed: bool,
    loaded_epoch: u64,
    epoch: u64,
) -> bool {
    signed_in && !loading && !failed && loaded_epoch != epoch
}

/// Whether a start belongs to the video view: the view is open and the track has a video to
/// show. Split out so the rule every track change rides is pinned by a test — it used to be
/// only the toggle's own reload, which left a frozen frame under the new song's audio.
fn plays_as_video(video_open: bool, wide: bool) -> bool {
    video_open && wide
}

/// Whether two lists are the same queue, track for track. `start_from` needs the *whole* list,
/// not just the loaded track: a mix seeded from the song playing now starts with it, and matching
/// on the track alone turned Start Mix into a pause button that never touched the queue.
fn same_queue(a: &[Track], b: &[Track]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.video_id == y.video_id)
}

/// How long "Later" on the update window keeps the launch check quiet about that release.
const UPDATE_LATER_SECS: i64 = 24 * 60 * 60;

/// Unix seconds, for the Discord presence's timestamps. Zero on a clock that predates 1970, which
/// only ever costs a progress bar.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl App {
    /// "Later" on the update window: no window for this release at launch for a day.
    pub fn update_later(&mut self, ctx: &egui::Context, version: &str) {
        let entry = Some((version.to_owned(), unix_now()));
        self.update_settings(ctx, |s| s.update_later = entry);
    }

    pub fn new(cc: &eframe::CreationContext<'_>, options: Options) -> anyhow::Result<Self> {
        let Options {
            decorated,
            startup_timing,
            search,
            autoplay,
            stats,
            stats_after,
            screenshot,
            page,
            autoscroll,
            autoscroll_max,
            queue,
            lyrics,
            now_playing,
            scheme,
            opacity,
            no_blur,
            no_autoplay,
            tray_test,
            autostart,
        } = options;
        theme::install(&cc.egui_ctx);
        // No Ctrl/⌘ with +, - or 0 zoom: the layout is built at fixed point sizes — cards, the
        // sidebar, row widths — and egui's keyboard zoom pushed them out of their cards. The
        // system's display scaling still applies, through `pixels_per_point`.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // Let the single-instance listener thread wake this UI (`instance`'s own doc comment).
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        crate::instance::attach(&cc.egui_ctx);
        let mut settings = crate::settings::load();
        // The themes folder is written on the very first run, then read — before anything that
        // names a theme, so an old settings file and `--scheme` both resolve against it.
        theme::custom::seed();
        widgets::init_reduced_motion();
        let custom_themes = theme::custom::load();
        for (file, error) in &custom_themes.errors {
            tracing::warn!(%file, %error, "custom theme did not load");
        }
        settings.migrate_legacy(&custom_themes);
        // Session-only, and applied before the first palette build so the very first frame
        // already shows what was asked for. Never saved: `update_settings` only ever writes
        // `self.settings`, and this happens before that struct is stored on `App`, so a later
        // settings edit persists the override right back — which is fine for a scripted run
        // and would be wrong for anything else, which is why these stay CLI-only.
        if let Some(query) = scheme {
            match custom_themes.find_loose(&query) {
                Some(t) if t.dark => {
                    settings.dark_theme = t.name.clone();
                    settings.theme = crate::settings::Theme::Dark;
                }
                Some(t) => {
                    settings.light_theme = t.name.clone();
                    settings.theme = crate::settings::Theme::Light;
                }
                None => tracing::warn!(%query, "--scheme: no theme by that name"),
            }
        }
        if let Some(opacity) = opacity {
            settings.opacity = opacity;
        }
        if no_blur {
            settings.blur = false;
        }
        if no_autoplay {
            settings.autoplay = false;
        }
        let theme_watch = theme::custom::Watcher::new();
        let palette = settings.palette(&cc.egui_ctx, &custom_themes);
        theme::apply(&cc.egui_ctx, &palette);
        let blur = crate::blur::Blur::new(cc);
        #[cfg(target_os = "linux")]
        let activation = crate::activation::Activation::new(cc);
        let traffic_lights = crate::chrome::TrafficLights::new(cc);
        // Until the sidebar is first drawn (the splash comes first), from its saved width.
        let sidebar_rail = settings.sidebar_width.is_some_and(theme::sidebar_is_rail);

        let backend = Backend::new(cc.egui_ctx.clone())?;
        backend.set_audio_quality(settings.quality.format());
        // One handle per process; a failure just means running without OS media controls.
        let media = crate::media::Media::new(cc.egui_ctx.clone(), window_hwnd(cc));
        // libmpv is initialised on the first play, not here — see `Player`.
        let player = Some(Player::new());
        // A stored session is only a claim until YouTube confirms it — cookies expire between
        // runs, and asserting otherwise is what showed an empty library under a "Signed in" label.
        let checking = backend.is_signed_in_sync();
        let auth = if checking {
            backend.verify_session();
            crate::auth::AuthState::Checking
        } else {
            crate::auth::AuthState::SignedOut
        };

        // The rail must not wait on the session round trip: with a stored cookie the sidebar
        // fetch fires immediately, and a dead cookie fails it into the empty rail — the state
        // that would have shown anyway.
        let mut app = Self {
            palette,
            custom_themes,
            theme_watch,
            was_focused: true,
            // Open on Home, as YouTube Music does — it is a real feed now rather than a page we
            // assembled. `--search` starts on Search, and `--page` overrides both.
            history: History::new(match (page, search.is_some()) {
                (Some(route), _) => route,
                (None, true) => Route::Search,
                (None, false) => Route::Home,
            }),
            decorated,
            startup_timing,
            // A tray icon from a previous run's setting shows up again this run without
            // needing the toggle re-flipped.
            tray: settings
                .hide_tray_on_close
                .then(|| crate::tray::spawn(&cc.egui_ctx)),
            #[cfg(target_os = "macos")]
            menu_bar: crate::menu_bar::MenuBar::install(&cc.egui_ctx),
            hidden: false,
            quitting: false,
            autostart_attention: autostart,
            backend,
            player,
            player_state: Default::default(),
            player_error: None,
            query: search.clone().unwrap_or_default(),
            query_changed_at: None,
            suggestions: Vec::new(),
            suggestions_for: None,
            suggestion_cursor: None,
            search_history: crate::search_history::load(),
            recent_dismissed: false,
            pending_search: search.is_some(),
            searched: String::new(),
            searching: false,
            results: SearchResults::default(),
            results_generation: 0,
            status: None,
            offline: false,
            premium: false,
            offline_probe_at: None,
            offline_probing: false,
            offline_page_shown: false,
            panel_dragging: [false; 2],
            panel_save: [0; 2],
            panels_seeded: false,
            min_window_w: theme::WINDOW_MIN_W,
            window_fit_tries: 0,
            fullscreen_check: FullscreenCheck::Looking,
            search_scope: SearchScope::default(),
            scoped_results: Vec::new(),
            scoped_loading: false,
            scoped_error: None,
            search_failed: false,
            scoped_for: None,
            search_all_indices: None,
            artist: None,
            artist_loading: false,
            artist_error: None,
            artist_for: None,
            album: None,
            album_loading: false,
            album_error: None,
            album_for: None,
            account_epoch: 0,
            pending_card_play: None,
            pending_page_action: None,
            logo: None,
            detected_browsers: None,
            browser_importing: false,
            auth,
            startup_splash: checking,
            sign_in_error: None,
            library: crate::model::Library::default(),
            library_loading: false,
            library_error: None,
            library_loaded: false,
            library_sort: LibrarySort::Latest,
            liked: crate::liked::Index::default(),
            liked_scanning: false,
            liked_full_scan: false,
            liked_buffer: Vec::new(),
            liked_dirty: false,
            last_liked_save: Instant::now(),
            playlist: None,
            playlist_loading: false,
            playlist_suggestions: None,
            playlist_suggestions_loading: false,
            playlist_related: Vec::new(),
            playlist_error: None,
            playlist_for: None,
            playlist_sort: PlaylistSort::default(),
            playlist_sort_pending: false,
            playlist_original_order: None,
            artist_expand: None,
            artist_expand_loading: false,
            artist_expand_error: None,
            artist_liked_sort: PlaylistSort::default(),
            home: None,
            home_loading: false,
            home_next: None,
            home_more_loading: false,
            home_loaded_at: None,
            home_refreshing: false,
            home_shelf_full: None,
            home_shelf_loading: None,
            home_error: None,
            home_chips: Vec::new(),
            home_filter: None,
            mood_playlists: None,
            mood_playlists_loading: false,
            mood_playlists_error: None,
            mood_playlists_params: None,
            now: NowPlaying::default(),
            q: PlayQueue::new(rng_seed()),
            playback_started: false,
            stall: StallGuard::default(),
            play_generation: 0,
            play_is_video: false,
            radio: None,
            repeat: RepeatMode::Off,
            radio_loading: false,
            upnext_open: queue,
            lyrics_open: lyrics,
            fs: if now_playing {
                // A test aid: `--now-playing --queue` opens the fullscreen view with its queue
                // drawer out, which nothing outside the app can click open.
                let mut fs = ui::fullscreen::State::opened(lyrics || settings.fullscreen_lyrics);
                fs.queue = queue;
                fs
            } else {
                ui::fullscreen::State::default()
            },
            lyrics: None,
            lyrics_loading: false,
            lyrics_error: None,
            lyrics_for: None,
            playlist_edit: None,
            playlist_delete: None,
            playlist_duplicate: None,
            edit_on_load: None,
            edit_revert: None,
            rail_cache: std::collections::HashMap::new(),
            album_cache: std::collections::HashMap::new(),
            rail_filter: RailFilter::default(),
            new_playlist: None,
            new_playlist_kind: NewPlaylistKind::default(),
            explore: None,
            explore_loading: false,
            explore_error: None,
            explore_links: Vec::new(),
            releases: None,
            releases_loading: false,
            releases_error: None,
            charts: None,
            charts_loading: false,
            charts_error: None,
            charts_params: None,
            moods: None,
            moods_loading: false,
            moods_error: None,
            history_shelves: None,
            history_loading: false,
            history_error: None,
            history_epoch: 1,
            history_loaded_epoch: 0,
            last_volume: 70.0,
            playback_saved: None,
            page_loading_since: None,
            resolving_since: None,
            last_playback_check: Instant::now(),
            muted: false,
            toast: None,
            autoplay,
            images: Images::new(),
            settings,
            accounts: Vec::new(),
            profile_ids: std::collections::HashSet::new(),
            autoscroll: autoscroll.then_some(0.0),
            autoscroll_max: autoscroll_max.unwrap_or(f32::INFINITY),
            scroll_to_top: true,
            queue_scroll_reset: false,
            lyrics_scroll_reset: false,
            lyrics_active: None,
            page_info: None,
            help_open: false,
            settings_tab: ui::settings::SettingsTab::default(),
            home_order_open: false,
            paste_cookies_open: false,
            pasted_cookie: String::new(),
            crash_report: crate::crash::last(),
            update: ui::update::State::default(),
            downloads: crate::downloads::Index::default(),
            downloaded_ids: Default::default(),
            downloaded_tracks: Default::default(),
            download_progress: std::collections::HashMap::new(),
            stats: Stats::new(stats),
            stats_after,
            screenshot: screenshot.map(|(path, when)| (path, when, false)),
            content_rect: egui::Rect::NOTHING,
            blur,
            #[cfg(target_os = "linux")]
            activation,
            traffic_lights,
            sidebar_rail,
            traffic_lights_shown: false,
            media,
            discord: None,
            presence_key: None,
            media_key: None,
            presence_position: 0.0,
            presence_sent_at: Instant::now(),
            ctx: cc.egui_ctx.clone(),
            video_open: false,
            video: None,
            local_tracks: Vec::new(),
            local_tracks_generation: 0,
            local_view_cache: None,
            local_scanning: false,
            local_scan_error: None,
            local_browse_path: Vec::new(),
            local_playlists: crate::local_playlists::load(),
        };
        app.resync_local_playlists();
        if !app.settings.local_folders.is_empty() {
            app.local_scanning = true;
            app.backend
                .scan_local_folders(app.settings.local_folders.clone());
        }
        app.load_sidebar_playlists();
        app.downloads = crate::downloads::load();
        app.push_downloads_to_backend();
        app.restore_playback();
        // Start the presence here too, not only from a Settings change: a launch with the setting
        // already on should show up without the user having to toggle it off and back on.
        app.sync_discord();
        // Heal the autostart entry an earlier version left behind — a stale path, or one without
        // the `--autostart` flag this version's startup request needs — and drop one the setting
        // no longer wants. The Settings toggle writes it directly; this is the once-a-launch
        // check, and it deliberately leaves a working entry that points at another install.
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        if let Err(e) = crate::autostart::sync(app.settings.start_on_boot) {
            tracing::warn!(error = %format!("{e:#}"), "could not update the autostart entry");
        }
        // Not on a scripted run: a timing, a screenshot or a stats line should measure the app,
        // not an update window or the request behind it.
        if app.settings.check_for_updates
            && !startup_timing
            && app.screenshot.is_none()
            && app.stats_after.is_none()
        {
            app.check_for_update(false);
        }
        if let Some(after) = tray_test {
            app.spawn_tray_test(after);
        }
        Ok(app)
    }

    /// The `--tray-test` thread: hide, show, quit — each injected through [`crate::tray::inject`],
    /// the same queue-and-wake path a real tray click takes. The log lines carry the timestamps
    /// the steps actually landed on, so a slow quit can be attributed to the event being acted
    /// on late (line long after injection) or to teardown (line, then a long silence).
    fn spawn_tray_test(&self, after: Duration) {
        use crate::tray::{TrayAction, inject};
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            tracing::info!("tray test: hiding");
            inject(&ctx, TrayAction::ToggleVisible);
            std::thread::sleep(Duration::from_secs(3));
            tracing::info!("tray test: showing");
            inject(&ctx, TrayAction::ToggleVisible);
            std::thread::sleep(Duration::from_secs(3));
            tracing::info!("tray test: quitting");
            inject(&ctx, TrayAction::Quit);
        });
    }

    /// Keyboard shortcuts. Space is play/pause, unless a text field has focus — otherwise it
    /// would be impossible to type a space into the search box.
    /// Spotify's own desktop defaults, kept identical rather than invented, so the muscle
    /// memory carries over. `command` is egui's own per-platform stand-in for the modifier each
    /// OS actually uses (Ctrl on Windows/Linux, ⌘ on macOS) — the same reasoning Spotify's own
    /// shortcuts follow across platforms.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        if typing {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.toggle_playback();
        }
        if ctx.input(|i| i.modifiers.is_none() && i.key_pressed(egui::Key::ArrowUp)) {
            self.skip(-1);
        }
        if ctx.input(|i| i.modifiers.is_none() && i.key_pressed(egui::Key::ArrowDown)) {
            self.skip(1);
        }
        // Ctrl+↑/↓, Spotify's volume keys. macOS has them as ⌘↑/↓ in its menu bar
        // (`poll_menu_bar`), which already acts on the press.
        #[cfg(not(target_os = "macos"))]
        {
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::ArrowUp)) {
                self.nudge_volume(VOLUME_STEP);
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::ArrowDown)) {
                self.nudge_volume(-VOLUME_STEP);
            }
        }
        if ctx.input(|i| i.modifiers.is_none() && i.key_pressed(egui::Key::M)) {
            self.toggle_mute();
        }
        // ⌘M and ⌘W are the Window menu's on macOS (`menu_bar`), as in every Mac app.
        // The fullscreen view: F opens and closes it, Escape closes the queue drawer and then
        // the view.
        if ctx.input(|i| i.modifiers.is_none() && i.key_pressed(egui::Key::F)) {
            if self.fs.open {
                self.fs_exit(ctx);
            } else {
                self.fs_enter(ctx);
            }
        }
        if self.fs.open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.fs.queue {
                self.fs.queue = false;
            } else {
                self.fs_exit(ctx);
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.toggle_shuffle();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::R)) {
            self.cycle_repeat();
        }
        // Spotify binds both; neither ever hurts to also have here.
        if ctx.input(|i| {
            i.modifiers.command && (i.key_pressed(egui::Key::K) || i.key_pressed(egui::Key::L))
        }) {
            ctx.memory_mut(|m| m.request_focus(crate::ui::topbar::search_id()));
        }
        // The cheatsheet. `?` arrives as `Questionmark` on layouts that produce it and as
        // Shift+/ elsewhere, so both are accepted; F1 for keyboards without a `?`.
        if ctx.input(|i| {
            i.key_pressed(egui::Key::F1)
                || i.key_pressed(egui::Key::Questionmark)
                || (i.modifiers.shift && i.key_pressed(egui::Key::Slash))
        }) {
            self.help_open = true;
        }
    }

    /// Drop everything that belonged to the previous account.
    ///
    /// A brand channel has its own library, playlists and home feed; keeping the old ones on
    /// screen after a switch would be showing one account's data under another's name.
    pub(crate) fn forget_account_data(&mut self) {
        self.account_epoch += 1;
        // Asked again for whichever channel comes next; until it answers, no downloads.
        self.premium = false;
        if self.signed_in() {
            self.backend.check_premium(self.account_epoch);
        }
        self.library = crate::model::Library::default();
        self.library_loaded = false;
        self.library_error = None;
        self.liked = crate::liked::Index::default();
        self.liked_scanning = false;
        self.liked_buffer.clear();
        self.liked_dirty = false;
        crate::liked::remove();
        self.clear_home();
        self.home_error = None;
        self.history_shelves = None;
        self.history_error = None;
        self.history_invalidate();
        self.cancel_account_fetches();
        self.playlist = None;
        self.playlist_for = None;
        self.accounts.clear();
        // Local playlists are not tied to any YouTube account — they should survive an account
        // switch exactly like they survive sign-out.
        self.resync_local_playlists();
    }

    /// Abandon the in-flight, account-scoped fetches an epoch bump orphans.
    ///
    /// A reply for an old epoch is dropped rather than allowed to repopulate cleared state —
    /// but the reply is also what clears the `*_loading` flag, so the flag would otherwise stay
    /// set and the page would never ask again: a skeleton that latches until the app restarts.
    /// That is exactly what a sign-in or sign-out racing the home fetch produced. History is not
    /// listed because `history_invalidate` already does this for it.
    fn cancel_account_fetches(&mut self) {
        self.home_loading = false;
        self.library_loading = false;
    }

    /// Apply and persist a settings change.
    ///
    /// Applied immediately rather than on restart: the palette is rebuilt, the resolver retuned,
    /// and only then is the file written — a failed write should not leave the UI disagreeing
    /// with what the app is doing.
    /// Rebuild the palette from the settings and the loaded themes, and hand it to egui.
    fn repaint_palette(&mut self, ctx: &egui::Context) {
        self.palette = self.settings.palette(ctx, &self.custom_themes);
        theme::apply(ctx, &self.palette);
    }

    /// Re-read the themes folder when the watcher saw it change, repainting if the theme on
    /// screen is among what changed. Cheap to call every frame: one atomic load at rest.
    fn reload_custom_themes(&mut self, ctx: &egui::Context) {
        // The way back from an editor: the window is focused again, so look at the folder.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        if focused && !self.was_focused {
            self.theme_watch.check();
        }
        self.was_focused = focused;
        if !self.theme_watch.take_changed() {
            return;
        }
        let themes = theme::custom::load();
        if themes == self.custom_themes {
            return;
        }
        for (file, error) in &themes.errors {
            tracing::warn!(%file, %error, "custom theme did not load");
        }
        self.custom_themes = themes;
        let palette = self.settings.palette(ctx, &self.custom_themes);
        if palette != self.palette {
            self.palette = palette;
            theme::apply(ctx, &self.palette);
        }
    }

    pub(crate) fn update_settings(
        &mut self,
        ctx: &egui::Context,
        change: impl FnOnce(&mut crate::settings::Settings),
    ) {
        let before = self.settings.clone();
        change(&mut self.settings);
        if self.settings == before {
            return;
        }
        // Any of what makes a palette: the mode, the theme chosen for either mode, or how much
        // of the desktop shows through it — and the corners, which egui's style carries too.
        if self.settings.theme != before.theme
            || self.settings.dark_theme != before.dark_theme
            || self.settings.light_theme != before.light_theme
            || self.settings.opacity != before.opacity
            || self.settings.card_outline != before.card_outline
            || self.settings.corners != before.corners
        {
            self.repaint_palette(ctx);
        }
        if self.settings.quality != before.quality {
            self.backend
                .set_audio_quality(self.settings.quality.format());
        }
        // A new order re-arranges the feed already loaded. Settings is not Home, so nothing is
        // being read while it moves, and late pages fold into their proper places too.
        if self.settings.home_order != before.home_order
            && self.home_filter.is_none()
            && let Some(home) = &mut self.home
        {
            let order = crate::model::normalise_order(&self.settings.home_order);
            let shelves: &mut Vec<crate::model::Shelf> = Arc::make_mut(home);
            crate::model::order_home(shelves, &order);
        }
        if self.settings.account_page_id != before.account_page_id {
            self.backend
                .switch_account(self.settings.account_page_id.clone());
        }
        if self.settings.hide_tray_on_close != before.hide_tray_on_close {
            // Built once, ever, the first time this turns on — not rebuilt on every toggle,
            // which used to leak a thread and a duplicate tray icon per toggle (see `tray`'s
            // own module doc comment). Every toggle after the first is just `set_visible`.
            match &self.tray {
                Some(tray) => tray.set_visible(self.settings.hide_tray_on_close),
                None if self.settings.hide_tray_on_close => {
                    self.tray = Some(crate::tray::spawn(ctx));
                }
                None => {}
            }
        }
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        if self.settings.start_on_boot != before.start_on_boot {
            let result = if self.settings.start_on_boot {
                crate::autostart::enable()
            } else {
                crate::autostart::disable()
            };
            if let Err(e) = result {
                tracing::warn!(error = %format!("{e:#}"), "could not update the autostart entry");
            }
        }
        if self.settings.local_folders != before.local_folders {
            self.local_scanning = true;
            self.local_scan_error = None;
            self.backend
                .scan_local_folders(self.settings.local_folders.clone());
        }
        // The toggle takes effect at once; the application id itself is fixed in `discord.rs`,
        // so there is nothing else about the integration that can change.
        if self.settings.discord_rich_presence != before.discord_rich_presence {
            self.sync_discord();
        }
        if let Err(e) = crate::settings::save(&self.settings) {
            tracing::warn!(error = %format!("{e:#}"), "could not save settings");
        }
    }

    /// Ask GitHub for the latest release. `manual` is the About tab's "Check now": it asks at
    /// once, and answers even when there is nothing new.
    pub(crate) fn check_for_update(&mut self, manual: bool) {
        if self.update.checking || self.update.progress.is_some() {
            return;
        }
        self.update.checking = true;
        let delay = Duration::from_secs(if manual { 0 } else { 5 });
        self.backend.check_for_update(delay, manual);
    }

    /// Download and install the release the check found. The window closes once it is ready.
    pub(crate) fn install_update(&mut self) {
        let Some(release) = self.update.available.clone() else {
            return;
        };
        if self.update.progress.is_some() || !release.installable() {
            return;
        }
        self.update.progress = Some(0.0);
        self.update.error = None;
        self.backend.install_update(release);
    }

    /// The titlebar's own close button was clicked: hide to the tray if that's configured,
    /// otherwise actually close.
    ///
    /// Handled here rather than inline in `chrome::window_buttons` because a self-issued
    /// `ViewportCommand::Close` on the root viewport does not surface as a cancelable
    /// `close_requested()` the way an OS-originated close does (Alt+F4, or the OS's own window
    /// controls in `--decorated` mode) — egui's own doc for `ViewportCommand::Close` says as
    /// much: for the root viewport it "usually results in the application shutting down"
    /// outright. That OS-originated path is instead caught centrally in `poll_tray`, every
    /// frame, since it can arrive with no button click at all.
    pub(crate) fn request_close(&mut self, ctx: &egui::Context) {
        if self.settings.hide_tray_on_close {
            self.hidden = true;
            set_window_hidden(ctx, true);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// macOS: keep the system's window buttons in the window's top-left corner, centred on the
    /// top bar — inside the sidebar card when it is wide, above the rail when it is folded (see
    /// [`Self::lights_over_rail`]). Hidden while the window is fullscreen or the fullscreen player
    /// is up, which have no corner for them. A no-op elsewhere and with `--decorated`.
    fn place_traffic_lights(&mut self, fullscreen: bool) {
        let fullscreen = fullscreen || self.traffic_lights.window_fullscreen();
        self.traffic_lights_shown = chrome::NATIVE_WINDOW_BUTTONS && !self.decorated && !fullscreen;
        if self.decorated {
            return;
        }
        self.traffic_lights.place(
            TRAFFIC_LIGHTS_LEFT,
            theme::TITLEBAR_HEIGHT / 2.0,
            !fullscreen,
        );
    }

    /// macOS, sidebar folded to the rail: the traffic lights keep their place from the wide
    /// sidebar, which is wider than the rail, so they sit on the window above it instead of in
    /// the card, and the rail starts below them, level with the page card.
    pub(crate) fn lights_over_rail(&self) -> bool {
        self.traffic_lights_shown && self.sidebar_rail
    }

    /// The right edge of the traffic lights in points from the window's left, while they are on
    /// screen.
    pub(crate) fn traffic_lights_right(&self) -> f32 {
        TRAFFIC_LIGHTS_LEFT + self.traffic_lights.width()
    }

    /// Bring the window back from hidden or minimized, best-effort. Shared by the tray's Show,
    /// the OS media widget's Raise, and a second launch asking the running instance to come
    /// forward — all three want the same thing, and on Wayland all three hit the same protocol
    /// gap (`request_foreground`).
    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        set_window_hidden(ctx, false);
        request_foreground(ctx);
    }

    /// macOS: act on what was chosen from the menu bar, and keep its items in line with the
    /// app. In `logic`, so a choice is acted on while the window is hidden to the tray too.
    #[cfg(target_os = "macos")]
    fn poll_menu_bar(&mut self, ctx: &egui::Context) {
        use crate::menu_bar::Command;
        while let Some(command) = crate::menu_bar::poll() {
            if command.shows_window() && self.hidden {
                self.show_window(ctx);
            }
            match command {
                Command::PlayPause => self.toggle_playback(),
                Command::Next => self.skip(1),
                Command::Previous => self.skip(-1),
                Command::VolumeUp => self.nudge_volume(VOLUME_STEP),
                Command::VolumeDown => self.nudge_volume(-VOLUME_STEP),
                Command::Mute => self.toggle_mute(),
                Command::Shuffle => self.toggle_shuffle(),
                Command::Repeat => self.cycle_repeat(),
                Command::Home => self.navigate(Route::Home),
                Command::Explore => self.navigate(Route::Explore),
                Command::Library => self.navigate(Route::Library),
                Command::History => {
                    if self.signed_in() {
                        self.navigate(Route::History);
                    }
                }
                Command::Search => {
                    ctx.memory_mut(|m| m.request_focus(crate::ui::topbar::search_id()));
                }
                Command::Queue => self.upnext_open = !self.upnext_open,
                Command::Lyrics => self.toggle_lyrics(),
                Command::FullscreenPlayer => {
                    if self.fs.open {
                        self.fs_exit(ctx);
                    } else {
                        self.fs_enter(ctx);
                    }
                }
                Command::Settings => self.navigate(Route::Settings),
                Command::Shortcuts => self.help_open = true,
                Command::Website => crate::open_url(crate::menu_bar::WEBSITE),
            }
        }
        let state = crate::menu_bar::State {
            has_track: self.now.track.is_some(),
            playing: self.playback_started && !self.now.paused,
            muted: self.muted,
            shuffle: self.q.is_shuffled(),
            repeat: match self.repeat {
                RepeatMode::Off => "Off",
                RepeatMode::All => "All",
                RepeatMode::One => "One",
            },
            queue_open: self.upnext_open,
            lyrics_open: self.lyrics_open,
            fullscreen_player: self.fs.open,
            signed_in: self.signed_in(),
            typing: ctx.memory(|m| m.focused().is_some()),
        };
        if let Some(menu_bar) = self.menu_bar.as_mut() {
            menu_bar.update(state);
        }
    }

    /// Drain the actions the tray's event handlers queued, and veto a genuine OS close request
    /// into a hide when "Hide tray on close" is on. Called once a frame regardless of whether a
    /// tray icon currently exists — `tray::poll` is a cheap non-blocking pop off a queue that is
    /// simply empty when there is nothing to show a tray icon at all. The handlers themselves
    /// wake this frame up (`request_repaint`), so an idle app acts on a click at once instead of
    /// at the next unrelated repaint.
    fn poll_tray(&mut self, ctx: &egui::Context) {
        while let Some(action) = crate::tray::poll() {
            // The click's activation token, if the tray host sent one; used only to show.
            #[cfg(target_os = "linux")]
            let token = crate::tray::take_activation_token();
            match action {
                crate::tray::TrayAction::ToggleVisible => {
                    self.hidden = !self.hidden;
                    set_window_hidden(ctx, self.hidden);
                    if !self.hidden {
                        request_foreground(ctx);
                        #[cfg(target_os = "linux")]
                        if let Some(token) = &token {
                            self.activation.activate(token);
                        }
                    }
                }
                crate::tray::TrayAction::Show => {
                    // A double-click shows and focuses, never hides — the Show/Hide menu item
                    // remains the way to hide on purpose.
                    self.show_window(ctx);
                }
                crate::tray::TrayAction::Quit => {
                    // Bypasses the veto below on the same or a following frame — Quit means
                    // quit, not "hide again".
                    tracing::info!("tray: quit chosen");
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if !self.quitting
            && self.settings.hide_tray_on_close
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            set_window_hidden(ctx, true);
            self.hidden = true;
        }
        if self.hidden {
            // Nothing else wakes a hidden window — no cursor motion reaches it, nothing is
            // animating — so without this, a tray click asking to show it again would sit
            // unnoticed until some unrelated event happened to schedule a repaint.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Drain and act on whatever the OS media service sent — a media key, the shell widget, or
    /// another MPRIS client. Events arrive on a backend thread and are queued by `media.rs`.
    fn poll_media(&mut self, ctx: &egui::Context) {
        let events = match self.media.as_ref() {
            Some(media) => media.drain(),
            None => return,
        };
        for event in events {
            match event {
                playwire::Event::Play => {
                    if !self.playback_started || self.now.paused {
                        self.toggle_playback();
                    }
                }
                playwire::Event::Pause => {
                    if self.playback_started && !self.now.paused {
                        self.toggle_playback();
                    }
                }
                playwire::Event::PlayPause => self.toggle_playback(),
                playwire::Event::Stop => {
                    if self.playback_started && !self.now.paused {
                        self.toggle_playback();
                    }
                    if let Some(player) = self.player.as_ref() {
                        player.seek(0.0);
                    }
                }
                playwire::Event::Next => self.skip(1),
                playwire::Event::Previous => self.skip(-1),
                playwire::Event::SeekTo(position) => {
                    if let Some(player) = self.player.as_ref() {
                        player.seek(position.as_secs_f64());
                    }
                }
                playwire::Event::SeekBy(offset) => {
                    if let Some(player) = self.player.as_ref() {
                        let to = (player.state().position + offset).max(0.0);
                        player.seek(to);
                    }
                }
                playwire::Event::SetVolume(v) => self.set_user_volume(v * 100.0),
                playwire::Event::SetShuffle(on) => self.set_shuffle(on),
                playwire::Event::SetRepeat(repeat) => {
                    self.repeat = match repeat {
                        playwire::Repeat::Off => RepeatMode::Off,
                        playwire::Repeat::One => RepeatMode::One,
                        playwire::Repeat::All => RepeatMode::All,
                    };
                }
                playwire::Event::Raise => self.show_window(ctx),
                playwire::Event::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                // `OpenUri`, and anything a future backend adds: nothing to do.
                _ => {}
            }
        }
    }

    /// Publish the transport to the OS widget. Once a frame is fine — `playwire` diffs, so an
    /// unchanged snapshot costs almost nothing.
    fn read_player_state(&self) -> crate::player::PlayerState {
        self.player.as_ref().map(|p| p.state()).unwrap_or_default()
    }

    fn publish_media(&mut self) {
        let state = self.player_state;
        // A restored track is not loaded yet, so mpv's position is 0; show the saved one so the
        // OS scrubber matches the bar.
        let position = if self.playback_started {
            state.position
        } else {
            self.q.resume_here().unwrap_or(state.position)
        };
        let Some(media) = self.media.as_mut() else {
            return;
        };
        let playing = self.playback_started && !state.paused;
        let track = self.now.track.as_ref();
        let scalars = MediaScalars {
            playing,
            // Whole seconds: the OS extrapolates between updates, so a sub-second tick adds
            // nothing, but a seek or a stall still lands within the second.
            position: position as u64,
            duration: state.duration as u64,
            volume: if self.muted {
                0
            } else {
                self.last_volume.round() as i32
            },
            repeat: self.repeat,
            shuffle: self.q.is_shuffled(),
            // Previous restarts the track, or brings back a queue this one replaced, so the
            // first track of a queue can go back too.
            can_previous: self.q.current().is_some(),
            can_next: !self.q.is_empty(),
        };
        if self.media_key.as_ref().is_some_and(|k| {
            k.scalars == scalars
                && k.id.as_deref() == track.map(|t| t.video_id.as_str())
                && k.title.as_deref() == track.map(|t| t.title.as_str())
        }) {
            return;
        }
        let snapshot = crate::media::playback_state(
            self.now.track.as_ref(),
            playing,
            position,
            state.duration,
            if self.muted {
                0.0
            } else {
                (self.last_volume / 100.0).clamp(0.0, 1.0)
            },
            scalars.repeat,
            scalars.shuffle,
            scalars.can_previous,
            scalars.can_next,
        );
        media.set_state(&snapshot);
        self.media_key = Some(MediaKey {
            id: track.map(|t| t.video_id.clone()),
            title: track.map(|t| t.title.clone()),
            scalars,
        });
    }

    /// Hand Discord the current track, when there is one. Called right after `publish_media`, so
    /// both see the same transport.
    ///
    /// Only on a material change: Discord rate-limits activity updates, and the end timestamp is
    /// absolute, so it keeps the bar moving between sends on its own. A seek is the exception —
    /// it changes neither track nor state but does break that bar, so the position is compared
    /// against where the track should be by now rather than against its last value.
    fn publish_presence(&mut self) {
        let Some(discord) = self.discord.as_ref() else {
            return;
        };
        let state = self.player_state;
        let playing = self.playback_started && !state.paused;
        // Mirrors `publish_media`: a restored, not-yet-loaded track shows its saved position.
        let position = if self.playback_started {
            state.position
        } else {
            self.q.resume_here().unwrap_or(state.position)
        };
        let Some(track) = self.now.track.as_ref() else {
            // Nothing to show: withdraw, but only if something was ever announced.
            if self.presence_key.take().is_some() {
                discord.clear();
            }
            return;
        };
        let id = track.local_path.is_none().then(|| track.video_id.clone());
        let was_playing = self.presence_key.as_ref().is_some_and(|(_, p, _)| *p);
        let expected = self.presence_position
            + if was_playing {
                self.presence_sent_at.elapsed().as_secs_f64()
            } else {
                0.0
            };
        let changed = match &self.presence_key {
            Some((last_id, last_playing, last_title)) => {
                last_id != &id
                    || *last_playing != playing
                    || last_title != &track.title
                    || (position - expected).abs() > 1.5
            }
            None => true,
        };
        if !changed {
            return;
        }
        let duration = if state.duration > 0.0 {
            state.duration
        } else {
            track.duration.map(|d| d as f64).unwrap_or(0.0)
        };
        discord.set(crate::discord::Presence {
            youtube_id: id.clone(),
            title: track.title.clone(),
            artist: track
                .artists
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
            album: track.album.clone(),
            thumbnail: track.thumbnail.as_deref().map(str::to_owned),
            wide: track.wide,
            playing,
            end_unix: playing.then(|| unix_now() + (duration - position).max(0.0) as i64),
        });
        self.presence_key = Some((id, playing, track.title.clone()));
        self.presence_position = position;
        self.presence_sent_at = Instant::now();
    }

    /// Start or stop the Discord worker to match the setting. Called on a Settings change and
    /// once at launch. The application id is fixed, so the toggle is the whole decision.
    pub(crate) fn sync_discord(&mut self) {
        self.discord = if self.settings.discord_rich_presence {
            Some(crate::discord::Discord::spawn())
        } else {
            None
        };
        // A new worker has not been told what is playing.
        self.presence_key = None;
    }

    /// Bring back the last queue, track and transport from `playback.json`. The track is shown
    /// paused; pressing play loads it and seeks to `resume`. Nothing is fetched or scrobbled
    /// here — a restore is not a play.
    fn restore_playback(&mut self) {
        let Some(store) = crate::playback::load() else {
            return;
        };
        // The volume first: it is kept with no queue as well, so a volume set before anything
        // was ever queued (a fresh install) survives a relaunch.
        if store.volume > 0.5 {
            self.last_volume = store.volume;
        }
        self.muted = store.muted;
        if store.queue.is_empty() {
            return;
        }
        let tracks: Vec<Track> = store.queue.into_iter().map(|t| t.into_track()).collect();
        // The radio as it was: its songs still marked as its own, its filters, and the page to
        // continue from. A file from before it was saved takes the whole queue as chosen and
        // gets a river like any hand-picked one, asked for once the queue first plays (see the
        // autoplay step), not here.
        self.radio = match store.radio {
            Some(r) => Some(Radio::from_persisted(r)),
            None => Radio::pending(&tracks),
        };
        // Anything past a couple of seconds is worth resuming; the very start is not. The
        // shuffle order is not saved: a shuffled session draws a fresh one.
        self.q.restore_session(
            tracks,
            store.current,
            (store.position > 1.0).then_some(store.position),
            store.shuffle,
            self.radio.as_ref().map(|r| &r.chosen),
        );
        self.now.track = self.q.current_track().cloned();
        self.repeat = store.repeat.map(Into::into).unwrap_or(RepeatMode::Off);
        // The video view only comes back for a track that has a video to show; the flag can be
        // stale against a queue that has since moved to an audio-only track.
        self.video_open = store.video_open && self.now.track.as_ref().is_some_and(|t| t.wide);
    }

    /// The queue and transport, as the on-disk store shape. Written with an empty queue too, for
    /// the volume; nothing empties a queue once it has tracks, so this never overwrites one.
    fn playback_store(&self) -> Option<crate::playback::Store> {
        let position = self.saved_position();
        Some(crate::playback::Store {
            version: 1,
            queue: self
                .q
                .tracks()
                .iter()
                .map(crate::playback::PersistedTrack::from_track)
                .collect(),
            current: self.q.current(),
            position,
            repeat: Some(self.repeat.into()),
            shuffle: self.q.is_shuffled(),
            volume: self.last_volume,
            muted: self.muted,
            video_open: self.video_open,
            radio: self.radio.as_ref().map(|r| r.to_persisted(self.q.tracks())),
        })
    }

    /// Write the queue and transport off the render thread. Throttled by the caller; best-effort
    /// (a failure is only a debug line — losing the queue is not worth a toast). Not what runs on
    /// exit: `eframe::App::save` needs the write to actually finish before the process does, so it
    /// calls `crate::playback::save` directly instead of handing this off to the backend.
    fn save_playback(&mut self) {
        if let Some(store) = self.playback_store() {
            self.playback_saved = Some(self.saved_playback(store.position));
            self.backend.save_playback(store);
        }
    }

    /// The position `playback.json` records: the player's once the track is playing, the
    /// restored one until then.
    fn saved_position(&self) -> f64 {
        if self.playback_started {
            self.player_state.position
        } else {
            self.q.resume().map(|(_, p)| p).unwrap_or(0.0)
        }
    }

    fn saved_playback(&self, position: f64) -> SavedPlayback {
        SavedPlayback {
            fingerprint: self.playback_fingerprint(),
            position,
            paused: self.now.paused || !self.playback_started,
            at: Instant::now(),
        }
    }

    /// Everything in `playback.json` except the position, hashed: the queue (by id), where in it
    /// we are, and the transport settings. Cheap enough to take once a second.
    fn playback_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for track in self.q.tracks() {
            track.video_id.hash(&mut h);
        }
        self.q.current().hash(&mut h);
        self.repeat.hash(&mut h);
        self.q.is_shuffled().hash(&mut h);
        self.last_volume.to_bits().hash(&mut h);
        self.muted.hash(&mut h);
        self.video_open.hash(&mut h);
        h.finish()
    }

    /// Whether `playback.json` no longer says what is true: see [`playback_save_due`].
    fn playback_stale(&self) -> bool {
        playback_save_due(
            self.playback_saved.as_ref(),
            self.playback_fingerprint(),
            self.saved_position(),
            self.now.paused || !self.playback_started,
        )
    }

    /// The transport, once a pass: see [`eframe::App::logic`] for why it is not in `ui`.
    fn transport_step(&mut self, ctx: &egui::Context) {
        // Advance the queue when mpv runs out of file. A click in the last frame's panels that
        // loaded, paused or seeked is read here, a frame later.
        let state = self.read_player_state();
        self.player_state = state;
        // Kept in sync here rather than at every `toggle_pause`/`set_paused` call site — mpv is
        // the single source of truth for this, and every row/card's play-vs-pause glyph reads
        // it off `now` rather than reaching for the player directly.
        self.now.paused = state.paused;
        // Restore the saved position once its track is loaded, and keep `playback.json` fresh.
        self.poll_playback(ctx, &state);
        if self.playback_started {
            self.stall.observe(state.position);
        }
        if self.playback_started && state.idle && !self.now.resolving {
            self.playback_started = false;
            match self.stall.on_idle(Instant::now()) {
                // Repeat One replays the track that just ended; anything else advances normally.
                IdleEnd::Ended => {
                    if self.repeat == RepeatMode::One {
                        if let Some(current) = self.q.current() {
                            self.play_index(current);
                        }
                    } else {
                        self.skip(1);
                    }
                }
                IdleEnd::Retry(delay) => {
                    tracing::warn!(
                        position = state.position,
                        duration = state.duration,
                        retry = self.stall.retries,
                        ?delay,
                        "playback went idle without ever playing; reloading the track"
                    );
                }
                IdleEnd::GiveUp => {
                    tracing::error!(
                        track = %self.now.track.as_ref().map_or("?", |t| t.title.as_str()),
                        "playback never started after the retries; leaving the queue where it is"
                    );
                    self.player_error =
                        Some("Playback did not start. Check the audio output device.".to_owned());
                    self.toast_error("Playback did not start");
                }
            }
        }
        // A stalled attempt is reloaded here once its backoff has passed. `play_index_restore`,
        // not `play_index`: a retry is not a play, and must not write the history.
        if self.stall.take_due(Instant::now())
            && let Some(index) = self.q.current()
        {
            if let Some(start) = self.stall.start_position() {
                self.q.set_resume(Some((index, start)));
            }
            self.play_index_restore(index);
        }

        // Autoplay: one `next` call returns ~50 tracks. A hand-picked queue seeds its tail as
        // soon as it starts, so the tail is there from the beginning; an open river is
        // extended at the low-water mark. Nothing is fetched while the setting is off, nor under
        // Repeat All, which loops the chosen tracks instead (YouTube Music does the same).
        if !self.radio_loading {
            // Not before anything has played this session: a restored queue waits for play.
            let wants = self.settings.autoplay
                && self.repeat != RepeatMode::All
                && self.play_generation > 0;
            let remaining = self
                .q
                .remaining(self.repeat, self.radio.as_ref().map(|r| &r.chosen));
            let call = self.radio.as_ref().and_then(|radio| {
                river_call(radio, self.q.current(), remaining, self.q.tracks(), wants)
            });
            if let Some(call) = call {
                self.radio_loading = true;
                match call {
                    RiverCall::Seed { seed } => {
                        // The guard compares the pending river's seed: record what was asked
                        // for, which is the queue's tail as it stands now, not when it started.
                        if let Some(radio) = &mut self.radio {
                            radio.seed = seed.clone();
                        }
                        self.backend.seed_mix(seed);
                    }
                    RiverCall::Follow { token, reseed } => {
                        self.backend.extend_mix(token, reseed);
                    }
                }
            }
        }

        // Hand the OS widget the current transport. After the advance/autoplay above, so a track
        // that just changed is what the OS shows.
        self.publish_media();
        // Same snapshot, said to Discord.
        self.publish_presence();

        // The next wake-up, which is what keeps this running while the window is hidden: eframe
        // calls `logic` then only when a repaint has been asked for. Loading asks for no frames of
        // its own here: the skeletons and the loading ring each ask for theirs while they are on
        // screen, and background work shows nothing.
        if self.playback_started && !state.paused {
            let hidden = self.hidden
                || ctx.input(|i| {
                    let v = i.viewport();
                    v.occluded.unwrap_or(false) || v.minimized.unwrap_or(false)
                });
            ctx.request_repaint_after(playing_repaint_after(PlayingTick {
                position: state.position,
                duration: state.duration,
                seen_playing: self.stall.played_any,
                next_lyric: self.next_lyric_at(state.position),
                hidden,
            }));
        }
        // A pending stall reload needs a frame at its due time; nothing else will ask for one.
        if let Some(at) = self.stall.pending_at() {
            ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
    }

    /// Act on a restored position once its track is actually playing, and save the transport
    /// every few seconds so a crash or a kill loses little.
    fn poll_playback(&mut self, ctx: &egui::Context, state: &crate::player::PlayerState) {
        if let Some((index, position)) = self.q.resume() {
            if self.q.current() != Some(index) {
                // The queue moved on; the saved position belongs to a track no longer loaded.
                self.q.set_resume(None);
            } else if self.playback_started && state.duration > 0.0 {
                if let Some(player) = self.player.as_ref() {
                    player.seek(position);
                }
                self.q.set_resume(None);
            }
        }
        let since_check = self.last_playback_check.elapsed();
        if since_check >= PLAYBACK_CHECK_EVERY {
            self.last_playback_check = Instant::now();
            if self.playback_stale() {
                self.save_playback();
            }
        } else if self.playback_stale() {
            // Changed inside the throttle: an idle app paints no later frame to save it on, and
            // a change from the desktop (shuffle, volume) would wait for the next one, or be lost
            // to a kill before it. Ask for the pass that saves it.
            ctx.request_repaint_after(PLAYBACK_CHECK_EVERY - since_check);
        }
        if self.liked_dirty && self.last_liked_save.elapsed() >= Duration::from_secs(5) {
            self.save_liked_if_dirty();
        }
    }

    /// Tell the backend which video ids are on disk, so `play` uses the file.
    /// Every change to `downloads` ends here, so this is also where the menus' copy of the ids
    /// is refreshed.
    fn push_downloads_to_backend(&mut self) {
        self.downloaded_ids = std::sync::Arc::new(self.downloads.entries.keys().cloned().collect());
        self.downloaded_tracks = std::sync::Arc::new(self.downloads.tracks());
        let map = self
            .downloads
            .entries
            .iter()
            .map(|(id, r)| (id.clone(), std::path::PathBuf::from(&r.path)))
            .collect();
        self.backend.set_downloads(map);
    }

    /// Whether downloads are on offer: the account holds Premium.
    pub(crate) fn can_download(&self) -> bool {
        self.premium
    }

    fn start_download(&mut self, track: Track) {
        if !self.can_download() {
            self.toast_error("Downloads need YouTube Premium");
            return;
        }
        if self.downloads.entries.contains_key(&track.video_id) {
            self.toast("Already downloaded");
            return;
        }
        if self.download_progress.contains_key(&track.video_id) {
            return;
        }
        self.download_progress.insert(track.video_id.clone(), 0.0);
        self.backend
            .download_track(track, self.settings.quality.format());
        self.toast("Downloading…");
    }

    fn start_download_many(&mut self, tracks: Vec<Track>) {
        if !self.can_download() {
            self.toast_error("Downloads need YouTube Premium");
            return;
        }
        let pending: Vec<Track> = tracks
            .into_iter()
            .filter(|t| {
                !self.downloads.entries.contains_key(&t.video_id)
                    && !self.download_progress.contains_key(&t.video_id)
            })
            .collect();
        if pending.is_empty() {
            self.toast("Already downloaded");
            return;
        }
        for track in &pending {
            self.download_progress.insert(track.video_id.clone(), 0.0);
        }
        let count = pending.len();
        self.backend
            .download_many(pending, self.settings.quality.format());
        self.toast(&format!("Downloading {count} tracks…"));
    }

    fn remove_download(&mut self, video_id: &str) {
        if let Some(record) = self.downloads.entries.remove(video_id) {
            let _ = std::fs::remove_file(&record.path);
            if let Err(e) = crate::downloads::save(&self.downloads) {
                tracing::warn!(error = %format!("{e:#}"), "could not save the download index");
            }
            self.push_downloads_to_backend();
            self.toast("Removed download");
        }
    }

    /// Show or hide the in-app music video. Opening creates the software render surface (once)
    /// and reloads the current track as video; closing hands playback back to the audio resolver
    /// — both at the current position.
    pub(crate) fn toggle_video(&mut self) {
        if self.video_open {
            self.video_open = false;
            self.reload_current();
            return;
        }
        if !self.now.track.as_ref().is_some_and(|t| t.wide) {
            return;
        }
        if !self.ensure_video_surface() {
            self.toast_error("Could not open the video view");
            return;
        }
        self.video_open = true;
        self.reload_current();
    }

    /// Make sure the software render surface exists, creating it once libmpv is up.
    ///
    /// The surface must be created *before* a video is loaded: `vo=libmpv` with no render
    /// context makes mpv fall back to opening a window of its own. False when it cannot be
    /// created, in which case the caller must play audio.
    fn ensure_video_surface(&mut self) -> bool {
        if self.video.is_none()
            && let Some(player) = self.player.as_mut()
            && let Some(handle) = player.handle()
        {
            self.video = crate::video::Video::new(&handle, self.ctx.clone());
        }
        self.video.is_some()
    }

    /// Hand `track` to mpv in the mode the video view is in.
    ///
    /// The video view only ever plays a track that has a video: anything else closes it and
    /// plays the song's audio, which is the one path `play_index_restore` and the toggle share.
    /// Without this, every start but the toggle's own reload went to `Backend::play`, which sets
    /// `vid=no` — the new song's audio played over the previous video's last frame.
    fn start_track(
        &mut self,
        track: Track,
        handle: Arc<libmpv2::Mpv>,
        volume: f64,
        start_at: Option<f64>,
    ) -> u64 {
        tracing::debug!(
            video_open = self.video_open,
            wide = track.wide,
            id = %track.video_id,
            ?start_at,
            "starting track"
        );
        self.stall.on_load(&track.video_id, start_at);
        if plays_as_video(self.video_open, track.wide) && self.ensure_video_surface() {
            self.play_is_video = true;
            return self.backend.play_video(track, handle, volume, start_at);
        }
        // An audio-only track cannot stay in the video view.
        self.video_open = false;
        self.play_is_video = false;
        self.backend.play(
            track,
            handle,
            volume,
            self.settings.normalise_volume,
            start_at,
        )
    }

    /// Reload the current track in the mode the video toggle is now in, keeping the position.
    fn reload_current(&mut self) {
        let Some(index) = self.q.current() else {
            return;
        };
        let Some(track) = self.q.get(index).cloned() else {
            return;
        };
        let position = self
            .player
            .as_ref()
            .map(|p| p.state().position)
            .unwrap_or(0.0);
        let volume = if self.muted { 0.0 } else { self.last_volume };
        let Some(player) = self.player.as_mut() else {
            return;
        };
        let Some(handle) = player.handle() else {
            return;
        };
        // The old file is still loaded for an instant; without clearing this, `poll_playback`
        // would seek the *outgoing* file to the saved position and consume `resume` before the
        // new one starts — which is why handing playback back restarted from zero. Same guard
        // `play_index_restore` uses: wait for the new file to actually report playing.
        self.playback_started = false;
        // The position rides the load itself (`start`), applied once every track — including a
        // video's external audio — is open; a seek after the load moved only the video.
        let start_at = (position > 1.0).then_some(position);
        self.q.set_resume(start_at.map(|p| (index, p)));
        self.play_generation = self.start_track(track, handle, volume, start_at);
    }

    /// Pause or resume, or start the queue if nothing has played yet.
    pub(crate) fn toggle_playback(&mut self) {
        match (&self.player, self.playback_started) {
            (Some(pl), true) => pl.toggle_pause(),
            (Some(_), false) if !self.q.is_empty() => {
                self.play_index(self.q.current().unwrap_or_else(|| self.q.first()))
            }
            _ => {}
        }
    }

    fn is_maximized(ctx: &egui::Context) -> bool {
        ctx.input(|i| i.viewport().maximized.unwrap_or(false))
    }

    // ------------------------------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------------------------------

    /// The page currently on screen.
    pub(crate) fn current_route(&self) -> Route {
        self.history.current().clone()
    }

    pub(crate) fn navigate(&mut self, route: Route) {
        if self.history.push(route) {
            self.scroll_to_top = true;
            self.note_route_entered();
            self.ensure_route_loaded();
        }
    }

    /// Mark the route now on screen as entered, for the pages that must refresh per visit.
    ///
    /// History is the only one today: it is dated ("Today", "Yesterday"), so a list fetched an
    /// hour ago would show the wrong day labels. `ensure_route_loaded`'s per-frame safety net cannot do this by itself —
    /// its `load_*` guards are "already have it" tests — so entry is the one place that knows a
    /// new visit has begun. The shelves on screen are deliberately kept: a revisit refreshes
    /// underneath the old list rather than showing the loading skeleton again.
    fn note_route_entered(&mut self) {
        // Settings is where the themes folder is offered, so what is in it is looked at fresh.
        if matches!(self.current_route(), Route::Settings) {
            self.theme_watch.check();
        }
        if matches!(self.current_route(), Route::Home) {
            self.refresh_home_if_stale();
            // Home is a fresh start: the last search's text would read as still in play.
            self.query.clear();
            self.clear_suggestions();
        }
        // Back or forward onto the results puts their words back in the box Home emptied.
        if matches!(self.current_route(), Route::Search) && self.query.is_empty() {
            self.query = self.searched.clone();
        }
        if matches!(self.current_route(), Route::History) {
            self.history_error = None;
            self.history_invalidate();
        }
    }

    /// Supersede whatever the History page has loaded, in flight or on screen: the next
    /// `load_history` fetches, and a reply still on its way for the old epoch is dropped.
    fn history_invalidate(&mut self) {
        self.history_epoch += 1;
        self.history_loading = false;
    }

    pub(crate) fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    pub(crate) fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    /// Back and Forward must reload, not just move the cursor. Going back from artist B to
    /// artist A used to leave B's content on screen because these only touched the cursor.
    /// Both clear a pending scroll-to-top: it belonged to the page being left, and Back must
    /// return to where you were, not to the top.
    pub(crate) fn back(&mut self) {
        if self.history.back() {
            self.scroll_to_top = false;
            self.note_route_entered();
            self.ensure_route_loaded();
        }
    }

    pub(crate) fn forward(&mut self) {
        if self.history.forward() {
            self.scroll_to_top = false;
            self.note_route_entered();
            self.ensure_route_loaded();
        }
    }

    /// Make the loaded data match the route on screen.
    ///
    /// Every `load_*` below is idempotent — it compares the wanted id against the `_for` field
    /// that says what the current state describes, and returns when they already agree. That is
    /// what makes this safe to call from `navigate`, from Back and Forward, *and* once per frame
    /// as a safety net for a route whose fetch was dropped while we were elsewhere.
    /// Whether the startup splash should cover this frame — see [`Self::startup_splash`]. The
    /// moment it is not wanted it is released for good, so nothing later in the session can bring
    /// it back. The `--startup-timing`/`--stats-after` measurement runs keep the old path: they
    /// exist to time the shell, and `verify_session` has no request timeout to bound them by.
    fn splash_visible(&mut self) -> bool {
        if !self.startup_splash
            || self.startup_timing
            || self.stats_after.is_some()
            || self.screenshot.is_some()
        {
            return false;
        }
        let waiting = match self.auth {
            crate::auth::AuthState::Checking => true,
            // The shell paints before the landing feed is in — the rail, the chips, a grid of
            // skeletons — and that half-built frame is the same flash the splash exists to cover.
            // Hold until Home has something real to show, or has failed and has an error to show.
            crate::auth::AuthState::SignedIn { .. } => {
                matches!(self.current_route(), Route::Home)
                    && self.home.is_none()
                    && self.home_error.is_none()
            }
            // Signed out, or the check failed: there is no feed coming, and the shell — sign-in
            // prompt included — is the real state, not a placeholder for one.
            _ => false,
        };
        let visible = waiting && crate::elapsed_since_start() < SPLASH_MAX;
        if !visible {
            self.startup_splash = false;
        }
        visible
    }

    pub(crate) fn ensure_route_loaded(&mut self) {
        match self.current_route() {
            Route::Artist(id) => self.load_artist(id),
            // The liked page draws the artist's own `from_library`, so it only needs that
            // artist loaded — the same idempotent fetch, so Back/Forward and a cold entry work.
            Route::ArtistLiked(id) => self.load_artist(id),
            Route::Album(id) => self.load_album(id),
            Route::Playlist(id) => self.load_playlist(id),
            Route::Mood(params) => self.load_mood_playlists(params),
            Route::Library => self.load_library(),
            Route::Home => self.load_home(),
            // Its shelf comes from the home feed, so the feed must be there first.
            Route::HomeShelf(title) => {
                self.load_home();
                self.load_home_shelf(&title);
            }
            Route::Explore => self.load_explore(),
            Route::NewReleases => self.load_new_releases(),
            Route::Charts(params) => self.load_charts(params),
            Route::Moods => self.load_moods(),
            Route::History => self.load_history(),
            Route::Search | Route::Settings => {}
        }
    }

    /// Fetch an artist page unless the current state already describes this one — loaded,
    /// in flight, or failed. A latched error is cleared by [`Self::retry_route`].
    fn load_artist(&mut self, id: String) {
        if self.artist_for.as_deref() == Some(id.as_str()) {
            return;
        }
        self.artist = None;
        self.artist_error = None;
        self.artist_loading = true;
        self.artist_for = Some(id.clone());
        self.artist_expand = None;
        self.artist_expand_loading = false;
        self.artist_expand_error = None;
        self.backend.fetch_artist(id);
    }

    fn load_album(&mut self, id: String) {
        if self.album_for.as_deref() == Some(id.as_str()) {
            return;
        }
        self.album = None;
        self.album_error = None;
        self.album_loading = true;
        self.album_for = Some(id.clone());
        self.backend.fetch_album(id);
    }

    /// An album card's own play button: start playing it without navigating to its page —
    /// see `Event::CardTracksReady`'s own doc comment for why this is a separate fetch from
    /// `load_album`'s.
    ///
    /// The album already loaded (playing or paused) is paused or resumed on the spot, as its
    /// button shows, rather than fetched again and decided once the reply lands.
    pub(crate) fn play_album(&mut self, id: String) {
        if crate::ui::album_active(&self.now, &id) {
            self.toggle_playback();
            return;
        }
        self.pending_card_play = Some(id.clone());
        self.backend.play_album(id);
    }

    /// [`Self::play_album`], for a playlist card. The check matters most for a mix: fetched
    /// again it comes back as a new list without the song playing, and started over from another.
    pub(crate) fn play_playlist(&mut self, id: String) {
        if crate::ui::playlist_active(&self.now, &id) {
            self.toggle_playback();
            return;
        }
        self.pending_card_play = Some(id.clone());
        if crate::model::is_radio_mix_id(&id) {
            // One of YouTube's mixes plays as the site plays it: as its radio, which goes on
            // past the mix's first page and brings Up next's filters, rather than as a fixed
            // list whose radio (and filters) only arrive once it nearly runs out.
            let target = crate::model::WatchTarget {
                video_id: None,
                playlist_id: crate::model::bare_playlist_id(&id).to_owned(),
                params: None,
            };
            self.backend.play_target(id, target);
        } else {
            self.backend.play_playlist(id);
        }
    }

    /// Pick one of the radio's filters (Up next's "All ▾"): the radio's songs still to come are
    /// swapped for that filter's once its page lands. The playing song and the listener's own
    /// stay. While it is in flight the river is held, so a page of the old radio arriving
    /// meanwhile is dropped rather than appended.
    pub(crate) fn filter_radio(&mut self, index: usize) {
        let playing = self
            .now
            .track
            .as_ref()
            .filter(|t| t.local_path.is_none())
            .map(|t| t.video_id.clone());
        let Some(radio) = &mut self.radio else {
            return;
        };
        let Some(filter) = radio.filters.get(index) else {
            return;
        };
        if filter.selected || radio.filtering.is_some() {
            return;
        }
        let mut target = filter.target.clone();
        // The site sends the song playing with it, so the filtered radio continues from there.
        target.video_id = playing;
        let before = std::mem::replace(&mut radio.state, River::Dead);
        radio.filtering = Some((target.playlist_id.clone(), before));
        self.backend.filter_radio(target);
    }

    fn load_playlist(&mut self, id: String) {
        if self.playlist_for.as_deref() == Some(id.as_str()) {
            return;
        }
        self.playlist_for = Some(id.clone());
        // A new playlist starts back at its own order, not whatever the last one was sorted
        // to, and cancels any load-the-rest-then-sort chain that was still in flight.
        self.playlist_sort = PlaylistSort::default();
        self.playlist_sort_pending = false;
        self.playlist_original_order = None;
        self.playlist_suggestions = None;
        self.playlist_suggestions_loading = false;
        self.playlist_related.clear();
        // No fetch, no loading state: a local playlist is already fully in memory, built fresh
        // each time so a rescan's updated tags show up without needing its own cache-busting.
        if let Some(lp) = self.local_playlists.iter().find(|p| p.id == id).cloned() {
            self.playlist = Some(self.local_playlist_as_playlist(&lp));
            self.apply_default_playlist_sort();
            self.playlist_loading = false;
            self.playlist_error = None;
            // Never fires `Event::PlaylistLoaded` — the event that would otherwise consume
            // this — so `begin_edit_playlist`'s "arrive, then open the editor" has to happen
            // right here instead.
            if self.edit_on_load.as_deref() == Some(id.as_str()) {
                self.edit_on_load = None;
                self.open_playlist_editor();
            }
            return;
        }
        self.playlist = None;
        self.playlist_error = None;
        self.playlist_loading = true;
        self.backend.fetch_playlist(id);
    }

    /// A local playlist's tracks, resolved against the last scan — a `track_paths` entry with
    /// no matching current track (its file moved or was removed since) is simply dropped
    /// rather than shown as a broken row.
    fn local_playlist_as_playlist(
        &self,
        lp: &crate::local_playlists::LocalPlaylist,
    ) -> crate::model::Playlist {
        build_local_playlist(lp, &self.local_tracks)
    }

    fn load_mood_playlists(&mut self, params: String) {
        if self.mood_playlists_params.as_deref() == Some(params.as_str()) {
            return;
        }
        self.mood_playlists = None;
        self.mood_playlists_error = None;
        self.mood_playlists_loading = true;
        self.mood_playlists_params = Some(params.clone());
        self.backend.fetch_mood_playlists(params);
    }

    /// Page Liked Music into the liked index. A full scan when the cache is missing or old,
    /// otherwise just the first page — the newest likes — so a like made elsewhere shows up at
    /// once. Both run off the UI thread; the pages arrive as events.
    fn start_liked_scan(&mut self) {
        if !matches!(self.auth, crate::auth::AuthState::SignedIn { .. }) {
            return;
        }
        self.liked_scanning = true;
        self.liked_full_scan = self.liked.needs_full_scan();
        self.liked_buffer.clear();
        self.backend
            .fetch_liked(self.account_epoch, self.liked_full_scan);
    }

    /// Write the liked index out when it has changed. Serialising a few thousand tracks is a
    /// handful of milliseconds, and this only runs after a scan completes or a rating changed —
    /// not per frame.
    fn save_liked_if_dirty(&mut self) {
        if !self.liked_dirty || self.liked.is_empty() {
            return;
        }
        self.liked_dirty = false;
        self.last_liked_save = Instant::now();
        if let Err(e) = crate::liked::save(&self.liked) {
            tracing::warn!(error = %format!("{e:#}"), "could not save the liked index");
        }
    }

    /// The signed-in library, once per session. Signed out, the page shows a prompt instead.
    fn load_library(&mut self) {
        if !matches!(self.auth, crate::auth::AuthState::SignedIn { .. })
            || self.library_loaded
            || self.library_loading
            || self.library_error.is_some()
        {
            return;
        }
        self.library_loading = true;
        self.backend
            .fetch_library(self.library_sort.param(), self.account_epoch);
    }

    /// The home feed loads once per filter and stays cached while it is selected.
    ///
    /// Waits out `Checking` rather than firing on whatever cookie is stored before it is
    /// actually confirmed: a stored cookie `verify_session` still needs to refresh (Google
    /// rotates it under us) makes this exact fetch come back in the thin, signed-out shape —
    /// and unlike `home()`'s own signed-out padding, which only ever covers a *genuinely*
    /// signed-out response, there is nothing here to retry it once the real session settles.
    /// `ensure_route_loaded` runs every frame, so this simply tries again once `Checking`
    /// resolves either way.
    /// How far a Home load follows the feed: the strict plan when the listener turned on Strict
    /// Home Order and this is the default feed (a mood's shelves are never ordered), else the
    /// ordinary few pages.
    fn home_fetch(&self) -> crate::model::HomeFetch {
        if self.settings.home_strict_order && self.home_filter.is_none() {
            crate::model::HomeFetch::strict(&crate::model::normalise_order(
                &self.settings.home_order,
            ))
        } else {
            crate::model::HomeFetch::Quick
        }
    }

    fn load_home(&mut self) {
        if matches!(self.auth, crate::auth::AuthState::Checking) {
            return;
        }
        if self.home.is_none() && self.home_error.is_none() && !self.home_loading {
            self.home_loading = true;
            self.backend.fetch_home(
                self.home_filter.clone(),
                self.account_epoch,
                self.home_fetch(),
            );
        }
    }

    /// Empty the home feed and everything that hangs off it — for a sign-out, an account
    /// switch, a new filter.
    fn clear_home(&mut self) {
        self.home = None;
        self.home_error = None;
        self.home_next = None;
        self.home_more_loading = false;
        self.home_loaded_at = None;
        self.home_refreshing = false;
        self.home_shelf_full = None;
        self.home_shelf_loading = None;
    }

    /// Fetch the next page of the home feed, once, if there is one.
    pub(crate) fn load_home_more(&mut self) {
        if self.home_more_loading || self.home_loading {
            return;
        }
        let Some(token) = self.home_next.clone() else {
            return;
        };
        self.home_more_loading = true;
        tracing::debug!("home: loading the next page of the feed");
        self.backend
            .fetch_home_more(token, self.home_filter.clone(), self.account_epoch);
    }

    /// Whether the full list behind this shelf's "Show all" is on its way.
    pub(crate) fn home_shelf_loading(&self, title: &str) -> bool {
        self.home_shelf_loading.as_deref() == Some(title)
    }

    /// Whether the feed still has pages to load.
    pub(crate) fn home_has_more(&self) -> bool {
        self.home_next.is_some()
    }

    /// Opening Home to a feed older than [`HOME_STALE`] reloads it underneath the shelves on
    /// screen, which stay until the new ones land. Only on entering the page: a feed is never
    /// swapped out while it is being looked at.
    fn refresh_home_if_stale(&mut self) {
        if self.home.is_none() || self.home_loading || self.home_refreshing {
            return;
        }
        if !home_is_stale(self.home_loaded_at, Instant::now()) {
            return;
        }
        tracing::info!("home feed is stale; refreshing it");
        self.home_refreshing = true;
        self.home_loading = true;
        self.backend.fetch_home(
            self.home_filter.clone(),
            self.account_epoch,
            self.home_fetch(),
        );
    }

    /// The full list behind a Home shelf's "Show all", when the shelf names where to get it.
    fn load_home_shelf(&mut self, title: &str) {
        if self
            .home_shelf_full
            .as_ref()
            .is_some_and(|(t, _)| t == title)
            || self.home_shelf_loading.as_deref() == Some(title)
        {
            return;
        }
        let Some(shelf) = self
            .home
            .as_ref()
            .and_then(|h| h.iter().find(|s| s.title == title))
        else {
            return;
        };
        let (Some(browse_id), Some(params)) =
            (shelf.more_browse_id.clone(), shelf.more_params.clone())
        else {
            return;
        };
        self.home_shelf_full = None;
        self.home_shelf_loading = Some(title.to_owned());
        self.backend
            .fetch_home_shelf(browse_id, params, title.to_owned());
    }

    /// Filter the home feed through a mood-bar chip. `None` clears back to the default feed,
    /// as the site does when the selected chip is clicked again. The scroll reset is sticky:
    /// the filtered feed arrives frames later, and it must open at the top, not at the old
    /// feed's offset.
    pub(crate) fn set_home_filter(&mut self, params: Option<String>) {
        if self.home_filter == params {
            return;
        }
        self.home_filter = params;
        self.clear_home();
        self.home_error = None;
        self.home_loading = true;
        self.scroll_to_top = true;
        self.backend.fetch_home(
            self.home_filter.clone(),
            self.account_epoch,
            self.home_fetch(),
        );
    }

    fn artist_is_for(&self, id: &str) -> bool {
        self.artist_for.as_deref() == Some(id)
    }

    /// The id the artist page on screen was actually browsed as — what a route into that artist
    /// must reuse.
    ///
    /// Not `Artist::id`: that is the id the response's own subscribe button targets, which can
    /// be a different channel entirely (the live signed-in Tommy Richman page subscribed as
    /// `UC7eum…` while every browse id linking to it — and the page's own share entity — said
    /// `UCl7…`; opening the former has no "From your library" shelf, so the liked page came up
    /// empty). `artist_for` is by construction the id the load used.
    pub(crate) fn artist_id(&self) -> Option<&str> {
        self.artist_for.as_deref()
    }

    /// Remember that `id` is somebody's profile, before navigating to it.
    pub(crate) fn mark_profile(&mut self, id: &str) {
        self.profile_ids.insert(id.to_owned());
    }

    /// Whether the channel page now loading is a profile (a person's) and not an artist's: your
    /// own, or one opened from an owner link. Decides which skeleton stands in for it.
    pub(crate) fn loading_a_profile(&self) -> bool {
        is_profile(self.artist_id(), &self.profile_ids, self.own_channel_id())
    }

    /// The signed-in account's own channel, once the account list is in.
    pub(crate) fn own_channel_id(&self) -> Option<&str> {
        self.accounts
            .iter()
            .find(|account| account.selected)
            .and_then(|account| account.channel_id.as_deref())
    }

    fn album_is_for(&self, id: &str) -> bool {
        self.album_for.as_deref() == Some(id)
    }

    fn playlist_is_for(&self, id: &str) -> bool {
        self.playlist_for.as_deref() == Some(id)
    }

    /// Clear whatever latched the current route's failure and fetch it again. This is what the
    /// "Try again" buttons call: navigating to the route you are already on is a no-op, so a
    /// retry has to go through the state rather than through the history. A retry refetches
    /// fresh content, so it resets the scroll like a fresh open — otherwise the reloaded page
    /// lands at the failed attempt's offset.
    pub(crate) fn retry_route(&mut self) {
        match self.current_route() {
            Route::Artist(_) | Route::ArtistLiked(_) => {
                self.artist_for = None;
                self.artist_error = None;
            }
            Route::Album(_) => {
                self.album_for = None;
                self.album_error = None;
            }
            Route::Playlist(_) => {
                self.playlist_for = None;
                self.playlist_error = None;
            }
            Route::Mood(_) => {
                self.mood_playlists_params = None;
                self.mood_playlists_error = None;
            }
            Route::Library => {
                self.library_error = None;
                self.library_loaded = false;
                self.library = crate::model::Library::default();
                self.resync_local_playlists();
            }
            Route::Home => {
                self.home_error = None;
                self.clear_home();
            }
            Route::Explore => {
                self.explore_error = None;
                self.explore = None;
                self.explore_links.clear();
            }
            Route::NewReleases => {
                self.releases_error = None;
                self.releases = None;
            }
            Route::Charts(_) => {
                self.charts_error = None;
                self.charts = None;
            }
            Route::Moods => {
                self.moods_error = None;
                self.moods = None;
            }
            Route::History => {
                self.history_error = None;
                self.history_shelves = None;
                self.history_invalidate();
            }
            Route::HomeShelf(_) => {
                self.home_shelf_full = None;
                self.home_shelf_loading = None;
            }
            Route::Search | Route::Settings => return,
        }
        self.scroll_to_top = true;
        self.ensure_route_loaded();
    }

    /// Fetch the full list behind one artist shelf's "Show all".
    pub(crate) fn expand_artist_shelf(&mut self, title: &str) {
        if self.artist_expand_loading {
            return;
        }
        let Some(artist) = &self.artist else { return };
        let Some(shelf) = artist.shelves.iter().find(|s| s.title == title) else {
            return;
        };
        let Some(params) = shelf.more_params.clone() else {
            return;
        };
        // The shelf's own browse id, when we have one — a "more" button routinely points
        // somewhere other than the artist's own channel id (see `Shelf::more_browse_id`).
        // Falling back to the artist's id here only matters for the typed-API fallback shelves,
        // which never carry one; every raw-parsed shelf has its real id.
        let browse_id = shelf
            .more_browse_id
            .clone()
            .unwrap_or_else(|| artist.id.clone());
        self.artist_expand = Some((browse_id.clone(), title.to_owned()));
        self.artist_expand_loading = true;
        self.artist_expand_error = None;
        self.backend
            .fetch_artist_albums(browse_id, params, title.to_owned());
    }

    /// Change the library's order.
    ///
    /// Refetches from page one, because the order is the server's and applies to the whole
    /// library — not to whatever happens to be loaded.
    pub(crate) fn sort_library(&mut self, sort: LibrarySort) {
        if self.library_sort == sort {
            return;
        }
        self.library_sort = sort;
        self.library = crate::model::Library::default();
        self.library_loaded = false;
        self.library_error = None;
        self.resync_local_playlists();
        if matches!(self.auth, crate::auth::AuthState::SignedIn { .. }) {
            self.library_loading = true;
            self.backend.fetch_library(sort.param(), self.account_epoch);
        }
    }

    /// Change how the open playlist's track list is shown.
    ///
    /// The playlist endpoints take no sort of their own — unlike [`Self::sort_library`], there
    /// is no server order to hand this to, so this always sorts the tracks already loaded. If
    /// the playlist has not fully loaded yet (`tracks.next.is_some()`), sorting only what's
    /// loaded would present a truncated list as the whole playlist — the mistake `LibrarySort`
    /// was already changed to avoid — so the remaining pages load first, with the ordinary
    /// loading-row affordance shown meanwhile, and the sort applies once the last page arrives
    /// (see the `Event::PlaylistMore` handler).
    pub(crate) fn sort_playlist(&mut self, sort: PlaylistSort) {
        self.playlist_sort = sort;
        let Some(playlist) = self.playlist.as_mut() else {
            self.playlist_sort_pending = false;
            return;
        };
        // Defense-in-depth: the UI already hides Sort for one of these (`Playlist::is_expanding`),
        // but an unbounded radio river has no "the rest" to load, so this must never chase one
        // even if reached some other way.
        if playlist.is_expanding() {
            self.playlist_sort_pending = false;
            return;
        }
        if playlist.tracks.next.is_none() {
            self.playlist_sort_pending = false;
            apply_playlist_sort(
                sort,
                &mut playlist.tracks.items,
                &mut self.playlist_original_order,
            );
            return;
        }
        self.playlist_sort_pending = true;
        self.load_more_playlist();
    }

    /// A song landed in one of your playlists: bring the open copy of it up to date, so the
    /// count and the list say what the playlist now holds without a reload. Added from its own
    /// Suggestions, the song moves out of them and into the list. A playlist that is not open but
    /// was the last one opened is forgotten, so going back to it fetches it fresh rather than
    /// showing the copy from before the save.
    /// Move a playlist's count by `delta` wherever it shows, at the moment an add or a removal is
    /// asked for rather than when YouTube confirms it: a refusal moves it back.
    fn bump_playlist_counts(&mut self, playlist_id: &str, delta: i64) {
        bump_counts_in(
            self.playlist.as_mut(),
            &mut self.library.playlists.items,
            playlist_id,
            delta,
        );
    }

    fn note_saved_to_playlist(&mut self, playlist_id: &str, video_id: &str) {
        let bare = crate::model::bare_playlist_id;
        let open = self
            .playlist
            .as_ref()
            .is_some_and(|p| bare(&p.id) == bare(playlist_id));
        if !open {
            if self
                .playlist_for
                .as_deref()
                .is_some_and(|id| bare(id) == bare(playlist_id))
            {
                self.playlist_for = None;
            }
            return;
        }
        let from_suggestions = self.playlist_suggestions.as_mut().and_then(|s| {
            let i = s.tracks.iter().position(|t| t.video_id == video_id)?;
            Some(s.tracks.remove(i))
        });
        let track = from_suggestions.or_else(|| {
            let mut found = None;
            self.for_each_track(|t| {
                if found.is_none() && t.video_id == video_id {
                    found = Some(t.clone());
                }
            });
            found
        });
        let sort = self.playlist_sort;
        let Some(pl) = self.playlist.as_mut() else {
            return;
        };
        // The count went up when the add was asked for (`bump_playlist_counts`).
        // Only onto a list that is all here: appended to a partial one, it would sit in the
        // middle once the next page arrived after it.
        if pl.tracks.next.is_none()
            && let Some(mut track) = track
        {
            // Not a row of this playlist yet as far as its item id goes; the next load has it.
            track.set_video_id = None;
            track.rank = None;
            if let Some(order) = self.playlist_original_order.as_mut() {
                order.push(track.video_id.clone());
            }
            pl.tracks.items.push(track);
            if sort != PlaylistSort::Default || self.playlist_original_order.is_some() {
                apply_playlist_sort(
                    sort,
                    &mut pl.tracks.items,
                    &mut self.playlist_original_order,
                );
            }
        }
    }

    /// The duplicate dialog's "Add anyway": the same save, with YouTube's check skipped.
    pub(crate) fn add_duplicate_anyway(&mut self) {
        if let Some(p) = self.playlist_duplicate.take() {
            self.backend
                .add_to_playlist(p.playlist_id.clone(), p.video_id, true);
            self.bump_playlist_counts(&p.playlist_id, 1);
        }
    }

    /// A request found no connection. Everything else keeps failing the same way until one
    /// comes back, so from here the app says so once, in the banner, and listens for it.
    fn note_offline(&mut self) {
        if self.offline {
            return;
        }
        tracing::info!("offline: a request could not reach YouTube Music");
        self.offline = true;
        self.offline_probe_at = Some(Instant::now() + OFFLINE_PROBE);
    }

    /// While offline, ask every [`OFFLINE_PROBE`] whether YouTube Music answers again.
    fn probe_while_offline(&mut self, ctx: &egui::Context) {
        if !self.offline {
            return;
        }
        let now = Instant::now();
        if !self.offline_probing && self.offline_probe_at.is_none_or(|at| now >= at) {
            self.offline_probing = true;
            self.offline_probe_at = Some(now + OFFLINE_PROBE);
            self.backend.probe_connection();
        }
        // An idle window paints nothing, so the clock that sends the next probe needs a frame.
        ctx.request_repaint_after(OFFLINE_PROBE);
    }

    /// The connection is back: confirm the saved session that could not be checked, fill the
    /// rail, and load again whatever the page on screen failed to load — no Try again needed.
    fn back_online(&mut self) {
        if !self.offline {
            return;
        }
        tracing::info!("online again: reloading what failed");
        self.offline = false;
        self.offline_probe_at = None;
        if matches!(self.auth, crate::auth::AuthState::Failed(_))
            && self.backend.is_signed_in_sync()
        {
            // Back to `Checking`, as at launch, so the confirmation that follows is treated as
            // the startup one and does not throw away what is already loaded.
            self.auth = crate::auth::AuthState::Checking;
            self.backend.verify_session();
        }
        self.load_sidebar_playlists();
        if self.signed_in() {
            self.backend.check_premium(self.account_epoch);
        }
        match self.current_route() {
            Route::Search => {
                if self.status.is_some() && !self.searched.is_empty() {
                    self.search_again(self.searched.clone());
                }
            }
            _ => self.retry_route(),
        }
    }

    /// Start each session at the widths Settings holds, not the ones egui restored. egui persists
    /// a panel's size with its memory, and brought it back a frame margin short: the sidebar lost
    /// 8 pt on every launch until it hit its floor. Within a session its own record is
    /// consistent, so this runs once, before the first panel is shown.
    fn seed_panel_widths(&mut self, ctx: &egui::Context) {
        if self.panels_seeded {
            return;
        }
        self.panels_seeded = true;
        let sidebar = self.settings.sidebar_width.unwrap_or(theme::SIDEBAR_WIDTH);
        let sidebar_id = egui::Id::new("sidebar");
        let queue_id = egui::Id::new("right-panel");
        ctx.data_mut(|d| {
            d.insert_persisted(
                sidebar_id,
                egui::containers::panel::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(sidebar, 1.0),
                    ),
                },
            );
            // The queue's starting width depends on the window (see its own panel), so its
            // record is dropped and `default_size` decides.
            d.remove::<egui::containers::panel::PanelState>(queue_id);
        });
    }

    /// Keep the window from being shrunk past what the shell can hold: its least width is
    /// [`theme::WINDOW_MIN_W`], the rail beside the page card, and a window already narrower than
    /// that grows to it rather than leaving the page squeezed to a sliver. Not more while the
    /// queue/lyrics panel is open: that panel is hidden in a narrow window instead
    /// ([`theme::right_panel_fits`]), since a floor above what the OS snaps a window to (half a
    /// portrait screen is 540) left the window spilling off the screen. A maximized or fullscreen
    /// window is left to the compositor.
    fn fit_window_to_shell(&mut self, ctx: &egui::Context) {
        let min_w = theme::WINDOW_MIN_W;
        if (self.min_window_w - min_w).abs() >= 0.5 {
            self.min_window_w = min_w;
            self.window_fit_tries = 0;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                min_w,
                theme::WINDOW_MIN_H,
            )));
        }
        if self.window_fit_tries >= WINDOW_FIT_TRIES {
            return;
        }
        let (size, maximized, fullscreen) = ctx.input(|i| {
            let v = i.viewport();
            (
                i.raw.screen_rect.map(|r| r.size()),
                v.maximized.unwrap_or(false),
                v.fullscreen.unwrap_or(false),
            )
        });
        // macOS's simple fullscreen is not a winit fullscreen; see `window_fullscreen`.
        let fullscreen = fullscreen || crate::window_fullscreen::is(ctx);
        let Some(size) = size else { return };
        if size.x >= min_w - 0.5 || maximized || fullscreen {
            self.window_fit_tries = WINDOW_FIT_TRIES;
            return;
        }
        // The compositor may not take a size before the window is first configured, so ask a few
        // times, a moment apart, until the window is as wide as it needs to be.
        self.window_fit_tries += 1;
        tracing::debug!(
            width = size.x,
            min_w,
            "growing the window to fit the panels"
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(min_w, size.y)));
        ctx.request_repaint_after(Duration::from_millis(120));
    }

    /// Take a window that came up fullscreen out of it, when the app did not put it there.
    ///
    /// eframe remembers the window's last state and restores it at the next launch, so an app
    /// closed while the fullscreen player had the window starts fullscreen next time: a window
    /// whose own buttons do nothing (a fullscreen window cannot be minimised or resized) and that
    /// the shell, thinking it is merely windowed, paints with rounded corners. Only the fullscreen
    /// player puts the window there, so a window found fullscreen with the player closed is leaving
    /// it, back to the default size: the size it remembers is the screen's. Looked at for the
    /// first few seconds only, which is how long the compositor takes to say what the window is.
    fn leave_restored_fullscreen(&mut self, ctx: &egui::Context) {
        if self.fullscreen_check == FullscreenCheck::Done {
            return;
        }
        let fullscreen = crate::window_fullscreen::is(ctx);
        let player_has_it = self.fs.open || self.fs.manages_window;
        let (next, action) = fullscreen_step(
            self.fullscreen_check,
            player_has_it,
            fullscreen,
            crate::elapsed_since_start(),
        );
        self.fullscreen_check = next;
        match action {
            FullscreenAction::Nothing => {}
            FullscreenAction::Leave => {
                tracing::info!("the window was restored fullscreen; leaving it");
                crate::window_fullscreen::set(ctx, false);
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            FullscreenAction::Wait => ctx.request_repaint_after(Duration::from_millis(100)),
            FullscreenAction::Resize => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    DEFAULT_WINDOW_SIZE.0,
                    DEFAULT_WINDOW_SIZE.1,
                )));
            }
        }
    }

    /// Bring a panel back to the width the listener chose once the window has room for it again.
    /// egui clamps a panel into its range every frame and keeps the clamped width, so after a
    /// narrow spell a panel would stay narrow; this puts it back, and never fights a drag.
    fn restore_panel_width(
        &self,
        ctx: &egui::Context,
        edge: PanelEdge,
        preferred: f32,
        range: std::ops::RangeInclusive<f32>,
    ) {
        use egui::containers::panel::PanelState;
        let id = egui::Id::new(edge.name());
        if ctx
            .read_response(id.with("__resize"))
            .is_some_and(|r| r.dragged())
        {
            return;
        }
        // A drag that has just ended is still being saved (`remember_panel_width`): until it is,
        // the preference is the width from before it.
        let settling = self.panel_dragging[edge.slot()] || self.panel_save[edge.slot()] > 0;
        let want = preferred.clamp(*range.start(), *range.end());
        let Some(state) = PanelState::load(ctx, id) else {
            return;
        };
        if let Some(want) = restored_width(state.size().x, want, settling) {
            set_panel_width(ctx, id, want);
        }
    }

    /// Save a panel's width to Settings when a drag of its edge ends. egui keeps the width in
    /// its own memory too, but that is a cache it may drop; Settings is the record.
    fn remember_panel_width(&mut self, ctx: &egui::Context, edge: PanelEdge) {
        let slot = edge.slot();
        let id = egui::Id::new(edge.name());
        // The drag handle egui registers for the edge: `<panel id>.with("__resize")`.
        let dragging = ctx
            .read_response(id.with("__resize"))
            .is_some_and(|r| r.dragged());
        if self.panel_dragging[slot] && !dragging {
            self.panel_save[slot] = 2;
        }
        self.panel_dragging[slot] = dragging;
        if self.panel_save[slot] == 0 {
            return;
        }
        self.panel_save[slot] -= 1;
        let Some(width) = egui::containers::panel::PanelState::load(ctx, id).map(|s| s.size().x)
        else {
            return;
        };
        let mut width = (width * 2.0).round() / 2.0;
        // The sidebar is the icon rail or at least its old minimum, never a width between where
        // the names are cut to a few letters: let go in that gap and it settles at the nearer.
        if matches!(edge, PanelEdge::Sidebar) {
            let snapped = theme::snap_sidebar_width(width);
            if (snapped - width).abs() >= 0.5 {
                set_panel_width(ctx, id, snapped);
            }
            width = snapped;
        }
        self.update_settings(ctx, |s| match edge {
            PanelEdge::Sidebar => s.sidebar_width = Some(width),
            PanelEdge::Queue => s.panel_width = Some(width),
        });
    }

    /// Swap the open playlist's Suggestions for another batch.
    pub(crate) fn refresh_playlist_suggestions(&mut self) {
        if self.playlist_suggestions_loading {
            return;
        }
        let (Some(id), Some(token)) = (
            self.playlist.as_ref().map(|p| p.id.clone()),
            self.playlist_suggestions
                .as_ref()
                .and_then(|s| s.refresh.clone()),
        ) else {
            return;
        };
        self.playlist_suggestions_loading = true;
        self.backend.fetch_playlist_suggestions(id, token);
    }

    /// Open a freshly loaded playlist in the order Settings asks for. Through [`Self::sort_playlist`],
    /// so a long playlist loads the rest first, exactly as picking it from the page's own menu.
    /// Mixes stay as served: `sort_playlist` leaves an expanding one alone.
    fn apply_default_playlist_sort(&mut self) {
        let sort = self.settings.playlist_default_sort;
        if sort != PlaylistSort::Default {
            self.sort_playlist(sort);
        }
    }

    /// Ask for the next page of the open playlist's tracks.
    pub(crate) fn load_more_playlist(&mut self) {
        let Some(playlist) = self.playlist.as_mut() else {
            return;
        };
        if !playlist.tracks.can_load_more() {
            return;
        }
        let Some(token) = playlist.tracks.next.clone() else {
            return;
        };
        playlist.tracks.loading = true;
        let id = playlist.id.clone();
        self.backend.fetch_playlist_more(id, token);
    }

    /// Ask for the next page of one library section, if there is one and none is in flight.
    pub(crate) fn load_more_library(&mut self, section: crate::api::LibrarySection) {
        let (can, token) = match section {
            crate::api::LibrarySection::Songs => (
                self.library.songs.can_load_more(),
                self.library.songs.next.clone(),
            ),
            crate::api::LibrarySection::Artists => (
                self.library.artists.can_load_more(),
                self.library.artists.next.clone(),
            ),
            crate::api::LibrarySection::Albums => (
                self.library.albums.can_load_more(),
                self.library.albums.next.clone(),
            ),
            crate::api::LibrarySection::Playlists => (
                self.library.playlists.can_load_more(),
                self.library.playlists.next.clone(),
            ),
        };
        let (true, Some(token)) = (can, token) else {
            return;
        };
        match section {
            crate::api::LibrarySection::Songs => self.library.songs.loading = true,
            crate::api::LibrarySection::Artists => self.library.artists.loading = true,
            crate::api::LibrarySection::Albums => self.library.albums.loading = true,
            crate::api::LibrarySection::Playlists => self.library.playlists.loading = true,
        }
        self.backend.fetch_library_more(section, token);
    }

    /// A load the page on screen is waiting for — what its skeletons stand in for. Not the song
    /// being resolved (the loading ring says that), and not background work the listener never
    /// asked for: the sidebar's refresh, the radio topping up the queue.
    fn visible_loading(&self) -> bool {
        self.loads().any(Load::shows_skeleton)
    }

    /// Everything in flight right now.
    fn loads(&self) -> impl Iterator<Item = Load> {
        [
            (self.searching, Load::Search),
            (self.artist_loading, Load::Artist),
            (self.album_loading, Load::Album),
            (self.playlist_loading, Load::Playlist),
            (self.artist_expand_loading, Load::ArtistExpand),
            (self.home_loading, Load::Home),
            (self.mood_playlists_loading, Load::MoodPlaylists),
            (self.explore_loading, Load::Explore),
            (self.history_loading, Load::History),
            (self.releases_loading, Load::Releases),
            (self.charts_loading, Load::Charts),
            (self.moods_loading, Load::Moods),
            (self.library_loading, Load::Library),
            (self.home_shelf_loading.is_some(), Load::HomeShelf),
            (self.radio_loading, Load::Radio),
            (self.library.playlists.loading, Load::SidebarPlaylists),
            (self.now.resolving, Load::Song),
        ]
        .into_iter()
        .filter_map(|(on, load)| on.then_some(load))
    }

    /// `--screenshot`: ask egui for the window's pixels once the deadline passes, save the reply
    /// as a PNG, and quit.
    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let ready = match self.screenshot.as_ref().map(|(_, when, _)| *when) {
            None => return,
            Some(ShotWhen::After(after)) => crate::elapsed_since_start() >= after,
            // A beat into each, so the ring has turned and the shimmer is mid-pass.
            Some(ShotWhen::Ring) => {
                widgets::track_loading(ctx)
                    && self
                        .resolving_since
                        .is_some_and(|s| s.elapsed() >= Duration::from_millis(450))
            }
            Some(ShotWhen::Skeleton) => self.page_loading_since.is_some_and(|s| now - s >= 0.75),
        };
        let Some((path, _, asked)) = &mut self.screenshot else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(16));
        if !*asked && crate::elapsed_since_start() >= Duration::from_secs(60) {
            eprintln!("screenshot: the moment never came; nothing saved");
            self.screenshot = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !*asked && ready {
            *asked = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(std::sync::Arc::clone(image)),
                _ => None,
            })
        });
        let Some(image) = shot else {
            return;
        };
        let [w, h] = image.size;
        let saved = image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec())
            .map(|rgba| rgba.save(&*path));
        match saved {
            Some(Ok(())) => println!("screenshot={}", path.display()),
            _ => eprintln!("could not save the screenshot to {}", path.display()),
        }
        self.screenshot = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Publish this frame's loading signals: when the page's load began, for the skeletons'
    /// show delay, and whether the current song has been loading long enough to show its ring.
    fn publish_loading(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.page_loading_since = if self.visible_loading() {
            Some(self.page_loading_since.unwrap_or(now))
        } else {
            None
        };
        widgets::set_skeleton_since(ctx, self.page_loading_since);

        self.resolving_since = if self.now.resolving {
            Some(self.resolving_since.unwrap_or_else(Instant::now))
        } else {
            None
        };
        let shown = self.resolving_since.is_some_and(|since| {
            match widgets::track_ring_due(since.elapsed()) {
                Ok(()) => true,
                Err(left) => {
                    // Wake up exactly when the ring is due, not a frame later.
                    ctx.request_repaint_after(left);
                    false
                }
            }
        });
        widgets::set_track_loading(ctx, shown);
    }

    /// Fill the sidebar's playlist list as soon as there is a session.
    ///
    /// They come from the library, and the library only loaded when the Library page was opened
    /// — so the rail stayed empty until you clicked Library once, every run. Only the playlists
    /// section is fetched: it is the only one the rail shows, and pulling all four at startup
    /// cost 20 MB of the idle memory budget for three lists nothing was displaying.
    ///
    /// A stored cookie counts: the fetch fires while the session is still `Checking`, and a
    /// refusal simply leaves the rail empty — the state that would have shown anyway.
    fn load_sidebar_playlists(&mut self) {
        if !matches!(
            self.auth,
            crate::auth::AuthState::SignedIn { .. } | crate::auth::AuthState::Checking
        ) || self.library_loaded
            || self
                .library
                .playlists
                .items
                .iter()
                .any(|p| p.source == crate::model::PlaylistSource::YouTube)
            || self.library.playlists.loading
        {
            return;
        }
        self.library.playlists.loading = true;
        self.backend.fetch_library_playlists(self.account_epoch);
    }

    /// Mirrors `local_playlists` into `library.playlists.items` as `PlaylistRef`s, so the
    /// sidebar and the Library page render one merged list without knowing two sources exist.
    /// Called after anything that fully replaces `library.playlists` (a real fetch landing, a
    /// sign-out clearing it) and after any local-playlist mutation — the YouTube-sourced entries
    /// those places just set are left alone; only the local ones are dropped and rebuilt.
    pub(crate) fn resync_local_playlists(&mut self) {
        self.library
            .playlists
            .items
            .retain(|p| p.source != crate::model::PlaylistSource::Local);
        self.library
            .playlists
            .items
            .extend(self.local_playlists.iter().map(local_playlist_ref));
    }

    // ------------------------------------------------------------------------------------------
    // Search
    // ------------------------------------------------------------------------------------------

    /// Run the typed query. Search is submit-driven, not live: typing only refreshes the
    /// autocomplete list, so the results change on Enter, on a chosen suggestion, or from the
    /// `--search` CLI — never mid-keystroke. This is what keeps a network round trip from firing
    /// for every character and what lets the listener finish typing before the page moves.
    pub(crate) fn submit_search(&mut self) {
        // Enter on a blank box is not a search. Going to the Search page anyway showed the last
        // results, as if that search had been asked for again.
        if self.query.trim().is_empty() {
            return;
        }
        if !matches!(self.current_route(), Route::Search) {
            self.navigate(Route::Search);
        }
        // Mark autocomplete as already fetched for this exact text so the field does not
        // immediately re-open its suggestion list for the query that was just submitted.
        self.suggestions_for = Some(self.query.trim().to_owned());
        self.pending_search = true;
    }

    /// Write the recent searches out. A failure only costs the list on the next launch, so it is
    /// logged and never stops a search.
    pub(crate) fn save_search_history(&self) {
        if let Err(e) = crate::search_history::save(&self.search_history) {
            tracing::warn!(error = %format!("{e:#}"), "could not save the search history");
        }
    }

    /// Drop the autocomplete list and its cursor state.
    pub(crate) fn clear_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestions_for = None;
        self.suggestion_cursor = None;
    }

    /// Fire a submitted search. Returns how long until the next check is due, so the caller can
    /// schedule exactly one repaint rather than spinning.
    fn poll_search(&mut self) {
        if !std::mem::take(&mut self.pending_search) {
            return;
        }
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        // Before the repeat check below: searching the same thing again still moves it up.
        self.search_history.record(&query);
        self.save_search_history();
        if query == self.searched {
            return;
        }

        self.searched = query.clone();
        self.searching = true;
        self.status = None;
        // Drop the previous query's rows so the page shows its skeleton instead of results that
        // no longer answer what was asked.
        self.results = SearchResults::default();
        self.results_generation += 1;
        self.backend.search(query);
        if self.search_scope != SearchScope::All {
            self.fetch_scoped_search(self.search_scope);
        }
    }

    /// Fetch autocomplete once per settled query, and only while the field has focus — a hidden
    /// field needs none. The guard is `suggestions_for`, so re-typing the same text does not
    /// refetch. Returns a repaint delay while the debounce is still running. This is the *only*
    /// thing typing triggers; the search itself waits for a submit.
    fn poll_suggestions(&mut self, ctx: &egui::Context) -> Option<Duration> {
        if self.query.trim().is_empty() {
            self.suggestions.clear();
            self.suggestions_for = None;
            self.suggestion_cursor = None;
            return None;
        }
        if !ctx.memory(|m| m.has_focus(crate::ui::topbar::search_id())) {
            return None;
        }
        if self.suggestions_for.as_deref() == Some(self.query.as_str()) {
            return None;
        }
        let waited = self.query_changed_at?.elapsed();
        if waited < SUGGEST_DEBOUNCE {
            return Some(SUGGEST_DEBOUNCE - waited);
        }
        self.suggestions_for = Some(self.query.clone());
        self.backend.fetch_suggestions(self.query.clone());
        None
    }

    /// Kicks off the dedicated per-category fetch for `scope` against the current `searched`
    /// query, unless a fetch for that exact `(query, scope)` pair is already outstanding or
    /// done. Called when a scope chip is picked, and again from `poll_search` whenever a fresh
    /// search lands while a non-`All` scope is already selected.
    pub(crate) fn fetch_scoped_search(&mut self, scope: SearchScope) {
        if scope == SearchScope::All || self.searched.is_empty() {
            return;
        }
        if self
            .scoped_for
            .as_ref()
            .is_some_and(|(q, s)| q == &self.searched && *s == scope)
        {
            return;
        }
        self.scoped_for = Some((self.searched.clone(), scope));
        self.scoped_loading = true;
        self.scoped_error = None;
        self.scoped_results.clear();
        self.backend.search_scoped(self.searched.clone(), scope);
    }

    /// Open the sign-in window: what every Sign in button outside Settings does. The prompts on
    /// Home, Library and History used to only go to Settings, where a second Sign in opened it.
    /// Elsewhere there is no window, and Settings' browser import is the way in.
    pub(crate) fn sign_in(&mut self) {
        // Deliberately not `auth = Checking`: that state means "verifying a stored session at
        // startup", which is what tells `AuthChanged` whether the loaded home feed was already
        // personalised or is the anonymous one this sign-in just invalidated. Same reason as the
        // Settings buttons.
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        self.backend.sign_in_from_window();
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        self.navigate(Route::Settings);
    }

    /// Import from `arg`, a browser found by `browser_cookies::detected`.
    pub(crate) fn import_from(&mut self, arg: String) {
        self.browser_importing = true;
        self.backend.sign_in_from_browser(arg);
    }

    /// Run `query` as a fresh search, even when it is the one on screen: Try again after a
    /// failure, or YouTube's "Did you mean" spelling.
    pub(crate) fn search_again(&mut self, query: String) {
        self.searched.clear();
        self.query = query;
        self.submit_search();
    }

    /// Try again after a failed per-category fetch: the failed pair still counts as done, so it
    /// is forgotten first.
    pub(crate) fn retry_scoped_search(&mut self) {
        self.scoped_for = None;
        self.fetch_scoped_search(self.search_scope);
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        // Mouse side buttons walk the history, like a browser — on press, not on click.
        // A click needs a clean press/release cycle and egui tracks one shared press
        // origin, so any concurrent drag or pointer movement voids the release and the
        // button "does nothing". `back`/`forward` are no-ops with nowhere to go.
        if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1)) {
            self.back();
        }
        if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra2)) {
            self.forward();
        }
        for event in self.backend.drain() {
            // A test aid for debug builds: pages never land, so each one stays on its skeleton
            // while the session and the sidebar load as usual — the only way to see a signed-in
            // page's placeholder for long enough to screenshot it.
            #[cfg(debug_assertions)]
            if std::env::var_os("REFRYT_HOLD_LOADING").is_some() && event.is_page_load() {
                continue;
            }
            if event.error().is_some_and(crate::api::looks_offline) {
                self.note_offline();
            }
            match event {
                Event::PremiumChecked { premium, epoch } if epoch == self.account_epoch => {
                    if let Some(premium) = premium {
                        tracing::info!(premium, "YouTube Premium checked");
                        // A test aid: see the app as a free account does. It can only turn
                        // downloads off, never on.
                        self.premium = premium && std::env::var_os("REFRYT_ASSUME_FREE").is_none();
                    }
                }
                Event::PremiumChecked { .. } => {}
                Event::Connection { online } => {
                    self.offline_probing = false;
                    if online {
                        self.back_online();
                    }
                }
                // A slow search that finished after the user typed something else must not
                // replace fresher results.
                Event::SearchDone { query, results } if query == self.searched => {
                    self.searching = false;
                    self.search_failed = false;
                    self.status = results
                        .is_empty()
                        .then(|| format!("No results for “{query}”."));
                    self.results = results;
                    self.results_generation += 1;
                    let playable = self.results.playable();
                    if self.autoplay && !playable.is_empty() {
                        self.autoplay = false;
                        self.start_from(playable, 0);
                    }
                }
                Event::SearchFailed { query, error } if query == self.searched => {
                    tracing::warn!(error = %error, "search failed");
                    self.searching = false;
                    self.search_failed = true;
                    self.status = Some(error);
                }
                Event::SearchDone { .. } | Event::SearchFailed { .. } => {}

                // Same staleness guard as above, but against `scoped_for` rather than
                // `searched` — a scope switch (or a fresh search) can outrun an earlier
                // scoped fetch, and that reply must not overwrite a newer one.
                Event::ScopedSearchDone {
                    query,
                    scope,
                    results,
                } if self
                    .scoped_for
                    .as_ref()
                    .is_some_and(|(q, s)| q == &query && *s == scope) =>
                {
                    self.scoped_loading = false;
                    self.scoped_results = results;
                }
                Event::ScopedSearchFailed {
                    query,
                    scope,
                    error,
                } if self
                    .scoped_for
                    .as_ref()
                    .is_some_and(|(q, s)| q == &query && *s == scope) =>
                {
                    tracing::warn!(scope = scope.label(), error = %error, "scoped search failed");
                    self.scoped_loading = false;
                    self.scoped_error = Some(error);
                }
                Event::ScopedSearchDone { .. } | Event::ScopedSearchFailed { .. } => {}

                // A reply for a query no longer in the box is dropped, the same staleness guard
                // `scoped_for` uses: typing on must not flash the previous suggestions back up.
                Event::SuggestionsDone { query, items } => {
                    if self.query == query {
                        self.suggestions = items;
                        self.suggestions_for = Some(query);
                        self.suggestion_cursor = None;
                    }
                }
                Event::SuggestionsFailed { query, error } => {
                    tracing::debug!(%query, %error, "suggestions failed");
                }

                Event::DownloadProgress { video_id, percent } => {
                    self.download_progress.insert(video_id, percent);
                }
                Event::DownloadDone {
                    video_id,
                    path,
                    track,
                } => {
                    self.download_progress.remove(&video_id);
                    self.downloads.entries.insert(
                        video_id,
                        crate::downloads::Record {
                            path: path.to_string_lossy().into_owned(),
                            track: crate::playback::PersistedTrack::from_track(&track),
                        },
                    );
                    if let Err(e) = crate::downloads::save(&self.downloads) {
                        tracing::warn!(error = %format!("{e:#}"), "could not save the download index");
                    }
                    self.push_downloads_to_backend();
                    self.toast(&format!("Downloaded “{}”", track.title));
                }
                Event::DownloadFailed { video_id, error } => {
                    self.download_progress.remove(&video_id);
                    tracing::warn!(%video_id, %error, "download failed");
                    self.toast_error(&failure_text("Could not download that track", &error));
                }

                Event::LocalScanDone { tracks } => {
                    self.local_scanning = false;
                    self.local_scan_error = None;
                    self.local_tracks = tracks;
                    self.local_tracks_generation += 1;
                }
                Event::LocalScanFailed { error } => {
                    self.local_scanning = false;
                    self.local_scan_error = Some(error);
                }

                Event::UpdateChecked { release, manual } => {
                    self.update.checking = false;
                    self.update.error = None;
                    match release {
                        Some(release) => {
                            let skipped = self.settings.skipped_update.as_deref()
                                == Some(release.version.as_str())
                                || self.settings.update_later.as_ref().is_some_and(
                                    |(version, at)| {
                                        *version == release.version
                                            && (0..UPDATE_LATER_SECS).contains(&(unix_now() - at))
                                    },
                                );
                            tracing::info!(version = %release.version, "update available");
                            self.update.open |= manual || !skipped;
                            self.update.available = Some(*release);
                        }
                        None => {
                            self.update.available = None;
                            self.update.up_to_date = true;
                            if manual {
                                self.toast("refryt is up to date");
                            }
                        }
                    }
                }
                Event::UpdateCheckFailed { error, manual } => {
                    self.update.checking = false;
                    tracing::warn!(%error, "update check failed");
                    // The launch check stays quiet: being offline is not worth a toast.
                    if manual {
                        self.toast_error(&failure_text("Could not check for updates", &error));
                    }
                }
                Event::UpdateProgress { percent } => {
                    if self.update.progress.is_some() {
                        self.update.progress = Some(percent);
                    }
                }
                Event::UpdateReady => {
                    tracing::info!("update staged, closing");
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Event::UpdateFailed { error } => {
                    tracing::warn!(%error, "update failed");
                    self.update.progress = None;
                    self.update.error = Some(error);
                    self.update.open = true;
                }

                // A reply is kept only when it answers the fetch still outstanding. Matching on
                // `*_for` rather than on the visible route matters: a reply that arrives while
                // you are on another page is still the answer to the fetch we are waiting for,
                // and dropping it silently is what used to leave the page loading forever.
                Event::ArtistLoaded { id, artist } if self.artist_is_for(&id) => {
                    self.artist_loading = false;
                    // The page itself carries only the first 5 top songs; the rest (up to the
                    // 15 Top Songs shows) live in the shelf's own auto-generated playlist,
                    // fetched here so the page opens already showing as many as it can.
                    let more = artist.top_songs_playlist.clone();
                    self.artist = Some(artist);
                    if let Some(playlist_id) = more {
                        self.backend.fetch_playlist(playlist_id);
                    }
                }
                Event::ArtistFailed { id, error } if self.artist_is_for(&id) => {
                    self.artist_loading = false;
                    self.artist_error = Some(error);
                }
                Event::ArtistLoaded { .. } | Event::ArtistFailed { .. } => {}

                Event::AlbumLoaded { id, album } if self.album_is_for(&id) => {
                    self.album_loading = false;
                    self.cache_album(&id, &album);
                    self.album = Some(album);
                }
                Event::AlbumFailed { id, error } if self.album_is_for(&id) => {
                    self.album_loading = false;
                    self.album_error = Some(error);
                    self.page_action_failed(&id, "album");
                }
                // A release card's right-click prefetch, not a page view.
                Event::AlbumLoaded { id, album } => self.cache_album(&id, &album),
                Event::AlbumFailed { id, .. } => self.page_action_failed(&id, "album"),

                // An album/playlist card's play button, not a page view — see
                // `Event::CardTracksReady`'s own doc comment for why this isn't folded into
                // `AlbumLoaded`/`PlaylistLoaded`. Same "already loaded → pause/resume" rule
                // the album/playlist header's own Play button follows. Guarded by
                // `pending_card_play` for the same reason page loads guard on `album_is_for`:
                // two cards' play buttons clicked close together race each other's fetch, and
                // only the reply for the one clicked most recently should act.
                Event::CardTracksReady { id, tracks }
                    if !tracks.is_empty() && self.pending_card_play.as_deref() == Some(&id) =>
                {
                    self.pending_card_play = None;
                    // See `NowPlaying::playlist_playing`'s own doc comment: a playlist card has
                    // no per-track field naming it the way an album card reads `album_id`, so
                    // this is what lets its own play button show Pause too.
                    self.now.playlist_playing =
                        Some((id, tracks.iter().map(|t| t.video_id.clone()).collect()));
                    if self.queue_matches(&tracks) {
                        self.toggle_playback();
                    } else {
                        self.start_from(tracks, 0);
                    }
                }
                Event::CardTracksReady { .. } => {}

                // The expansion replaces the shelf it came from, in place, and drops its
                // "Show all" button along with it. `expanded` is what `artist.rs::shelves()`
                // reads to switch that shelf from its carousel preview to the full boxed grid.
                //
                // The reply is narrowed to the preview's own kinds first: a shelf's "more"
                // button can answer with a whole artist page (the captured "From your library"
                // expansion did), and letting that land wholesale is what mixed albums and
                // playlists into a songs shelf. `shelf_expansion` never empties a shelf.
                Event::ArtistAlbumsLoaded { artist_id, items } => {
                    self.artist_expand_loading = false;
                    if let Some((id, title)) = self.artist_expand.clone()
                        && id == artist_id
                        && let Some(artist) = self.artist.as_mut()
                        && let Some(shelf) = Arc::make_mut(&mut artist.shelves)
                            .iter_mut()
                            .find(|s| s.title == title)
                    {
                        shelf.items = crate::model::shelf_expansion(items, &shelf.items);
                        shelf.more_params = None;
                        shelf.expanded = true;
                    }
                }
                Event::ArtistAlbumsFailed { artist_id, error } => {
                    self.artist_expand_loading = false;
                    if self
                        .artist_expand
                        .as_ref()
                        .is_some_and(|(id, _)| *id == artist_id)
                    {
                        self.artist_expand_error = Some(error);
                    }
                }

                Event::PlaylistLoaded { id, playlist } if self.playlist_is_for(&id) => {
                    self.playlist_loading = false;
                    // Keep it for the rail's context menu too.
                    self.cache_rail_playlist(&id, &playlist);
                    // Suggestions only under your own lists, as on the site: a public playlist's
                    // extras are other playlists, not songs to add, and show as those.
                    if let Some(token) = playlist.extras.clone() {
                        if playlist.editable {
                            self.playlist_suggestions_loading = true;
                            self.backend.fetch_playlist_suggestions(id.clone(), token);
                        } else {
                            self.backend.fetch_playlist_related(id.clone(), token);
                        }
                    }
                    self.playlist = Some(playlist);
                    self.apply_default_playlist_sort();
                    // Edit from the rail: the page has arrived, so open its editor now.
                    if self.edit_on_load.as_deref() == Some(id.as_str()) {
                        self.edit_on_load = None;
                        self.open_playlist_editor();
                    }
                }
                // A further page of the open playlist, appended in order.
                Event::PlaylistMore { id, page } if self.playlist_is_for(&id) => {
                    if let Some(playlist) = self.playlist.as_mut() {
                        let next = page.next;
                        playlist.tracks.extend(
                            page.items
                                .into_iter()
                                .filter_map(|e| e.track().cloned())
                                .collect(),
                            next,
                        );
                    }
                    // A sort is waiting on the rest of the playlist: either it just arrived, or
                    // there is still more to fetch before it can apply.
                    if self.playlist_sort_pending {
                        let fully_loaded = self
                            .playlist
                            .as_ref()
                            .is_some_and(|p| p.tracks.next.is_none());
                        if fully_loaded {
                            self.playlist_sort_pending = false;
                            let sort = self.playlist_sort;
                            if let Some(playlist) = self.playlist.as_mut() {
                                apply_playlist_sort(
                                    sort,
                                    &mut playlist.tracks.items,
                                    &mut self.playlist_original_order,
                                );
                            }
                        } else {
                            self.load_more_playlist();
                        }
                    }
                }
                Event::PlaylistMoreFailed { id, error } if self.playlist_is_for(&id) => {
                    tracing::warn!(%id, %error, "could not load another playlist page");
                    if let Some(playlist) = self.playlist.as_mut() {
                        playlist.tracks.loading = false;
                    }
                    // The chain that was fetching the rest of the playlist to sort it broke —
                    // leave what's loaded in its current order rather than retrying forever.
                    self.playlist_sort_pending = false;
                }
                Event::PlaylistMore { .. } | Event::PlaylistMoreFailed { .. } => {}
                Event::PlaylistSuggestions { id, suggestions } if self.playlist_is_for(&id) => {
                    self.playlist_suggestions_loading = false;
                    // A failed Refresh keeps the batch on screen rather than emptying the shelf.
                    if suggestions.is_some() {
                        self.playlist_suggestions = suggestions;
                    }
                }
                Event::PlaylistSuggestions { .. } => {}
                Event::PlaylistRelated { id, shelves } if self.playlist_is_for(&id) => {
                    self.playlist_related = shelves;
                }
                Event::PlaylistRelated { .. } => {}

                Event::PlaylistFailed { id, error } if self.playlist_is_for(&id) => {
                    self.playlist_loading = false;
                    self.playlist_error = Some(error);
                    self.page_action_failed(&id, "playlist");
                }
                // The open artist's own top-songs prefetch, not the playlist page: replace the
                // page's 5 embedded songs with up to 15 of the real list, so Top Songs shows the
                // fuller list instead of sitting at just the 5.
                Event::PlaylistLoaded { id, playlist }
                    if self
                        .artist
                        .as_ref()
                        .is_some_and(|a| a.top_songs_playlist.as_deref() == Some(id.as_str())) =>
                {
                    if let Some(artist) = self.artist.as_mut() {
                        artist.top_songs = playlist.tracks.items.into_iter().take(15).collect();
                    }
                }
                // A prefetch for the rail's context menu: keep it, do not touch the page.
                Event::PlaylistLoaded { id, playlist } => self.cache_rail_playlist(&id, &playlist),
                Event::PlaylistFailed { id, .. } => self.page_action_failed(&id, "playlist"),

                // See `App::account_epoch`'s own doc comment: a reply for an account already
                // switched away from is dropped rather than repopulating cleared state.
                Event::LibraryLoaded { library, epoch } if epoch == self.account_epoch => {
                    self.library_loading = false;
                    self.library = library;
                    self.library_loaded = true;
                    self.resync_local_playlists();
                    self.apply_playlist_order();
                    // The restored track is the one copy that never went through
                    // `Resolving`/`Playing`, so `reconcile_state` has not run on it — a liked
                    // song restored at launch showed an unrated thumb in the bar until the
                    // next play. Now that Liked Songs is here, fill it in; only blanks are
                    // filled, so a rating the account disagrees with is never overwritten.
                    if let Some(mut track) = self.now.track.take() {
                        self.reconcile_state(&mut track);
                        self.now.track = Some(track);
                    }
                }
                Event::LibraryLoaded { .. } => {}
                // The liked scan: pages accumulate, then a completed full scan swaps in (so an
                // unlike made elsewhere is dropped, not merged back) and the index is written
                // out. A quick refresh merges as pages arrive. Epoch-guarded like the library's.
                Event::LikedPage { tracks, epoch } if epoch == self.account_epoch => {
                    if self.liked_full_scan {
                        self.liked_buffer.extend(tracks);
                    } else if !tracks.is_empty() {
                        self.liked.merge(tracks);
                        self.liked_dirty = true;
                    }
                }
                Event::LikedPage { .. } => {}
                Event::LikedScanDone { complete, epoch } if epoch == self.account_epoch => {
                    self.liked_scanning = false;
                    if self.liked_full_scan && complete {
                        self.liked.replace(std::mem::take(&mut self.liked_buffer));
                        self.liked_dirty = true;
                        // The restored track was reconciled against an empty index at launch;
                        // now that it is complete, the bar can be told.
                        if let Some(mut track) = self.now.track.take() {
                            self.reconcile_state(&mut track);
                            self.now.track = Some(track);
                        }
                    }
                    self.liked_buffer.clear();
                    self.save_liked_if_dirty();
                }
                Event::LikedScanDone { .. } => {}
                // A further page of one section, appended in order.
                Event::LibraryMore { section, page } => {
                    let next = page.next;
                    match section {
                        crate::api::LibrarySection::Songs => self.library.songs.extend(
                            page.items
                                .into_iter()
                                .filter_map(|e| e.track().cloned())
                                .collect(),
                            next,
                        ),
                        crate::api::LibrarySection::Artists => self.library.artists.extend(
                            page.items
                                .into_iter()
                                .filter_map(|e| match e {
                                    crate::model::Entry::Artist(a) => Some(a),
                                    _ => None,
                                })
                                .collect(),
                            next,
                        ),
                        crate::api::LibrarySection::Albums => self.library.albums.extend(
                            page.items
                                .into_iter()
                                .filter_map(|e| match e {
                                    crate::model::Entry::Album(a) => Some(a),
                                    _ => None,
                                })
                                .collect(),
                            next,
                        ),
                        crate::api::LibrarySection::Playlists => {
                            self.library.playlists.extend(
                                page.items
                                    .into_iter()
                                    .filter_map(|e| match e {
                                        crate::model::Entry::Playlist(p) => Some(p),
                                        _ => None,
                                    })
                                    .collect(),
                                next,
                            );
                            self.apply_playlist_order();
                        }
                    }
                }
                // The sidebar's own prefetch. It *replaces* rather than appends, and yields
                // entirely to a real library load: the two race, and appending after one had
                // already landed listed every playlist twice.
                Event::SidebarPlaylists { page, epoch } if epoch == self.account_epoch => {
                    if self.library_loaded {
                        self.library.playlists.loading = false;
                    } else {
                        self.library.playlists = crate::model::Paged::new(
                            page.items
                                .into_iter()
                                .filter_map(|e| match e {
                                    crate::model::Entry::Playlist(p) => Some(p),
                                    _ => None,
                                })
                                .collect(),
                            page.next,
                        );
                        self.resync_local_playlists();
                        self.apply_playlist_order();
                    }
                }
                Event::SidebarPlaylists { .. } => {}
                Event::SidebarPlaylistsFailed { error, epoch } if epoch == self.account_epoch => {
                    tracing::warn!(%error, "could not load the sidebar's playlists");
                    self.library.playlists.loading = false;
                }
                Event::SidebarPlaylistsFailed { .. } => {}
                Event::LibraryMoreFailed { section, error } => {
                    tracing::warn!(?section, %error, "could not load another library page");
                    match section {
                        crate::api::LibrarySection::Songs => self.library.songs.loading = false,
                        crate::api::LibrarySection::Artists => self.library.artists.loading = false,
                        crate::api::LibrarySection::Albums => self.library.albums.loading = false,
                        crate::api::LibrarySection::Playlists => {
                            self.library.playlists.loading = false
                        }
                    }
                }
                Event::LibraryFailed { error, epoch } if epoch == self.account_epoch => {
                    self.library_loading = false;
                    self.library_error = Some(error);
                }
                Event::LibraryFailed { .. } => {}

                Event::SubscriptionChanged {
                    channel_id,
                    subscribed,
                } => {
                    tracing::debug!(%channel_id, subscribed, "subscription updated");
                    self.toast(if subscribed {
                        "Subscribed"
                    } else {
                        "Unsubscribed"
                    });
                }
                Event::SubscriptionFailed {
                    channel_id,
                    previous,
                    error,
                } => {
                    tracing::warn!(%channel_id, %error, "subscription refused; reverting");
                    self.set_subscribed_state(&channel_id, previous);
                    self.toast_error(&failure_text("Could not update that subscription", &error));
                }
                Event::ExploreLoaded { shelves, links } => {
                    self.explore_loading = false;
                    self.explore = Some(Arc::new(shelves));
                    self.explore_links = links;
                }
                Event::ExploreFailed { error } => {
                    self.explore_loading = false;
                    self.explore_error = Some(error);
                }
                Event::ReleasesLoaded { shelves } => {
                    self.releases_loading = false;
                    self.releases = Some(Arc::new(shelves));
                }
                Event::ReleasesFailed { error } => {
                    self.releases_loading = false;
                    self.releases_error = Some(error);
                }
                Event::ChartsLoaded { params, feed } => {
                    if self.charts_params != params {
                        return;
                    }
                    self.charts_loading = false;
                    self.charts = Some(Arc::new(feed));
                }
                Event::ChartsFailed { params, error } => {
                    if self.charts_params != params {
                        return;
                    }
                    self.charts_loading = false;
                    self.charts_error = Some(error);
                }
                Event::MoodsLoaded { sections } => {
                    self.moods_loading = false;
                    self.moods = Some(Arc::new(sections));
                }
                Event::MoodsFailed { error } => {
                    self.moods_loading = false;
                    self.moods_error = Some(error);
                }
                Event::HistoryLoaded { shelves, epoch } if epoch == self.history_epoch => {
                    self.history_loading = false;
                    self.history_shelves = Some(Arc::new(shelves));
                    self.history_loaded_epoch = epoch;
                }
                Event::HistoryFailed { error, epoch } if epoch == self.history_epoch => {
                    self.history_loading = false;
                    // A refresh failing with a list already on screen must not throw that list
                    // away: the visit was an attempt to update it, and a transient failure — a
                    // stale session, a rate limit — should not blank a page that was working.
                    // The visit is still marked settled (so the per-frame loader does not retry
                    // in a loop), the failure rides a toast, and the next visit retries. With
                    // nothing on screen there is nothing to keep, so the page shows its error
                    // and "Try again".
                    if self.history_shelves.is_some() {
                        tracing::warn!(%error, "history refresh failed; keeping the last list");
                        self.history_loaded_epoch = epoch;
                        self.toast_error(&failure_text("Could not refresh your history", &error));
                    } else {
                        self.history_error = Some(error);
                    }
                }
                // A reply for a History visit since superseded: dropped, so a slow fetch cannot
                // overwrite a newer one's shelves.
                Event::HistoryLoaded { .. } | Event::HistoryFailed { .. } => {}
                Event::PlaylistCreated { title } => {
                    self.toast(&format!("Created “{title}”"));
                    // The rail lists what the library holds, so it has to be asked again before
                    // the new playlist can appear in it.
                    self.library.playlists = crate::model::Paged::default();
                    self.library_loaded = false;
                    self.resync_local_playlists();
                    self.load_sidebar_playlists();
                }
                Event::PlaylistCreateFailed { error } => {
                    tracing::warn!(%error, "could not create the playlist");
                    self.toast_error(&failure_text("Could not create that playlist", &error));
                }
                Event::HomeLoaded {
                    filter,
                    shelves,
                    chips,
                    next,
                    epoch,
                } => {
                    // A reply for a filter since changed, or an account since switched away
                    // from, is stale — drop it rather than showing one filter's shelves under
                    // another's chips, or one account's feed under another's.
                    if self.home_filter == filter && epoch == self.account_epoch {
                        self.home_loading = false;
                        self.home_refreshing = false;
                        let mut shelves = shelves;
                        tracing::debug!(
                            shelves = ?shelves.iter().map(|s| (s.strapline.as_deref(), s.title.as_str())).collect::<Vec<_>>(),
                            "home: first page"
                        );
                        self.order_home_shelves(&mut shelves);
                        self.home = Some(Arc::new(shelves));
                        self.home_chips = chips;
                        self.home_next = next;
                        self.home_more_loading = false;
                        self.home_loaded_at = Some(Instant::now());
                    }
                }
                Event::HomeFailed {
                    filter,
                    error,
                    epoch,
                } => {
                    if self.home_filter == filter && epoch == self.account_epoch {
                        self.home_loading = false;
                        if self.home_refreshing {
                            // A refresh that failed keeps the feed already on screen.
                            self.home_refreshing = false;
                            tracing::warn!(%error, "home refresh failed; keeping the old feed");
                        } else {
                            self.home_error = Some(error);
                        }
                    }
                }
                Event::HomeMore {
                    filter,
                    shelves,
                    next,
                    epoch,
                } => {
                    if self.home_filter == filter && epoch == self.account_epoch {
                        self.home_more_loading = false;
                        tracing::debug!(
                            shelves = shelves.len(),
                            more = next.is_some(),
                            "home: next page landed"
                        );
                        self.home_next = next;
                        // Arranged among themselves and appended: what is already loaded may be
                        // on screen, and must not move.
                        let mut shelves = shelves;
                        tracing::debug!(
                            shelves = ?shelves.iter().map(|s| (s.strapline.as_deref(), s.title.as_str())).collect::<Vec<_>>(),
                            "home: later page"
                        );
                        self.order_home_shelves(&mut shelves);
                        if let Some(home) = &mut self.home {
                            Arc::make_mut(home).extend(shelves);
                        }
                    }
                }
                Event::HomeMoreFailed {
                    filter,
                    error,
                    epoch,
                } => {
                    if self.home_filter == filter && epoch == self.account_epoch {
                        self.home_more_loading = false;
                        // The end of what can be loaded, rather than asking again every frame.
                        self.home_next = None;
                        tracing::warn!(%error, "could not load more of the home feed");
                    }
                }
                Event::HomeShelfLoaded { title, items } => {
                    if self.home_shelf_loading.as_deref() == Some(title.as_str()) {
                        self.home_shelf_loading = None;
                        let base = self
                            .home
                            .as_ref()
                            .and_then(|h| h.iter().find(|s| s.title == title).cloned());
                        if let Some(mut shelf) = base
                            && !items.is_empty()
                        {
                            shelf.items = items;
                            shelf.expanded = true;
                            self.home_shelf_full = Some((title, Arc::new(shelf)));
                        }
                    }
                }
                Event::HomeShelfFailed { title, error } => {
                    if self.home_shelf_loading.as_deref() == Some(title.as_str()) {
                        self.home_shelf_loading = None;
                        tracing::warn!(%title, %error, "could not load a home shelf in full");
                    }
                }

                Event::MoodPlaylistsLoaded { params, mood }
                    if self.mood_playlists_params.as_deref() == Some(params.as_str()) =>
                {
                    self.mood_playlists_loading = false;
                    self.mood_playlists = Some(mood);
                }
                Event::MoodPlaylistsFailed { params, error }
                    if self.mood_playlists_params.as_deref() == Some(params.as_str()) =>
                {
                    self.mood_playlists_loading = false;
                    self.mood_playlists_error = Some(error);
                }
                Event::MoodPlaylistsLoaded { .. } | Event::MoodPlaylistsFailed { .. } => {
                    self.mood_playlists_loading = false;
                }

                Event::AccountsLoaded { accounts, epoch } if epoch == self.account_epoch => {
                    self.accounts = accounts;
                }
                Event::AccountsLoaded { .. } => {}

                Event::SignInFailed { error } => {
                    self.browser_importing = false;
                    self.sign_in_error = Some(error);
                    // The reason is shown on the Account tab, so that is where Settings opens.
                    self.settings_tab = ui::settings::SettingsTab::Account;
                }

                // Told rather than shown a login window: the user did not ask to sign in, and
                // Home, Library and History each carry a Sign in button for when they want to.
                Event::SessionExpired => {
                    const EXPIRED: &str =
                        "Your sign-in expired. Sign in again to see your library.";
                    self.toast_error(EXPIRED);
                    self.sign_in_error = Some(EXPIRED.to_owned());
                    self.settings_tab = ui::settings::SettingsTab::Account;
                }

                Event::AuthChanged(state) => {
                    // `Checking` → `SignedIn` is the ordinary *startup* confirmation of a stored
                    // session, and the flag is read before the assignment below because that
                    // assignment is itself the fix this line needs. Only startup ever sets
                    // `Checking` — an interactive sign-in leaves the state alone until this
                    // event — so the flag really does mean "the startup check".
                    let was_checking = matches!(self.auth, crate::auth::AuthState::Checking);
                    // Assigned before the loaders run, not after: `load_library` gates on the
                    // *current* auth state, and at startup this event arrives while `self.auth`
                    // is still `Checking` — so its call below silently bailed and Liked Songs
                    // was only ever fetched once the Library page happened to be visited
                    // (which is why a liked song restored at launch, or played from a card,
                    // could show unrated in the bar all session).
                    self.auth = state;
                    self.browser_importing = false;
                    match &self.auth {
                        // A fresh session invalidates any previously fetched library.
                        crate::auth::AuthState::SignedIn { .. } => {
                            self.library_loaded = false;
                            self.sign_in_error = None;
                            // Only a *real* transition into signed-in — from
                            // `SignedOut`/`Failed`, finishing sign-in from Settings mid-session
                            // — invalidates what's already loaded, since that feed genuinely
                            // was the anonymous one. `Api::client_ref` already uses the stored
                            // cookie optimistically for every request from the very first
                            // frame, before this event ever fires (see its own doc comment), so
                            // a home feed fetched while still `Checking` already came back
                            // personalised; wiping and refetching it here regardless used to
                            // show an unnecessary second load on every launch: the feed
                            // flashing back to its skeleton and reappearing with, as far as the
                            // user can see, the same content.
                            if !was_checking {
                                self.account_epoch += 1;
                                self.clear_home();
                                self.home_error = None;
                                self.history_shelves = None;
                                self.history_error = None;
                                self.history_invalidate();
                                self.cancel_account_fetches();
                            }
                            // The topbar's photo chip reads these; asking once here beats
                            // every-frame refires from the pages that display them.
                            self.backend.fetch_accounts(self.account_epoch);
                            // Downloads follow the account's own Premium, as on YouTube.
                            self.backend.check_premium(self.account_epoch);
                            // Liked Songs is also how a restored or card-played track's like
                            // status gets backfilled (`reconcile_state`).
                            self.load_library();
                            // The liked index: the cache first (counts are there before the
                            // scan is), then a page-through of Liked Music in the background.
                            self.liked = crate::liked::load();
                            self.start_liked_scan();
                        }
                        crate::auth::AuthState::SignedOut => {
                            self.account_epoch += 1;
                            self.premium = false;
                            // Session data, all of it: the channels belong to the account that
                            // just left, and the signed-out menu on Windows would otherwise
                            // still list them. Clearing the chosen channel through
                            // `update_settings` also rebuilds the HTTP client without it, so a
                            // brand channel from the old account cannot leak into the next one.
                            self.accounts.clear();
                            self.update_settings(ctx, |s| s.account_page_id = None);
                            self.library = crate::model::Library::default();
                            self.library_loaded = false;
                            // The liked index is account data: cleared from memory and removed
                            // from disk, never left for whoever signs in next.
                            self.liked = crate::liked::Index::default();
                            self.liked_scanning = false;
                            self.liked_buffer.clear();
                            self.liked_dirty = false;
                            crate::liked::remove();
                            // Local playlists have nothing to do with a YouTube session — they
                            // stay visible signed out too.
                            self.resync_local_playlists();
                            // The feed is personalised; a session change invalidates it.
                            self.clear_home();
                            self.home_error = None;
                            self.history_shelves = None;
                            self.history_error = None;
                            self.history_invalidate();
                            self.cancel_account_fetches();
                        }
                        _ => {}
                    }
                    self.load_sidebar_playlists();
                }

                // A reply whose generation doesn't match the latest `play`/`play_video` call is
                // for a play already superseded by a later one (see `play_generation`'s own doc
                // comment) — dropped rather than let it silently overwrite `now` out of order.
                Event::Resolving(mut track, generation) if generation == self.play_generation => {
                    self.reconcile_state(&mut track);
                    self.now = NowPlaying {
                        track: Some(track),
                        resolving: true,
                        error: None,
                        paused: false,
                        playlist_playing: self.now.playlist_playing.clone(),
                    };
                }
                Event::Resolving(..) => {}
                Event::Playing(mut track, generation) if generation == self.play_generation => {
                    self.reconcile_state(&mut track);
                    let video_id = track.video_id.clone();
                    let wide = track.wide;
                    self.now = NowPlaying {
                        track: Some(track),
                        resolving: false,
                        error: None,
                        paused: false,
                        playlist_playing: self.now.playlist_playing.clone(),
                    };
                    self.playback_started = true;
                    // A track with no video cannot keep the video view up.
                    if self.video_open && !wide {
                        self.video_open = false;
                    }
                    self.prefetch_next();
                    // Lyrics follow the track while the panel is open; a song change starts a
                    // fresh request, and the guard drops it if the track changes again.
                    if self.lyrics_wanted()
                        && self.lyrics_for.as_deref() != Some(video_id.as_str())
                        && let Some(track) = self.now.track.clone()
                    {
                        self.fetch_lyrics(track);
                    }
                }
                Event::Playing(..) => {}
                // A mix card's own play (`play_playlist`), seeded with the mix's id: only the
                // card clicked last may start, as for `CardTracksReady`.
                Event::MixLoaded { seed, .. }
                    if crate::model::is_radio_mix_id(&seed)
                        && self.pending_card_play.as_deref() != Some(&seed) => {}
                Event::MixLoaded { seed, mut queue } => {
                    if self.pending_card_play.as_deref() == Some(&seed) {
                        self.pending_card_play = None;
                        self.now.playlist_playing = Some((
                            seed.clone(),
                            queue.tracks.iter().map(|t| t.video_id.clone()).collect(),
                        ));
                    }
                    // The player overlay is the now-playing song's own rating and the only like
                    // state a mix carries; write it onto the first row so the bar's thumbs show
                    // it. (Rows have no button of their own.)
                    if let Some(like) = queue.like
                        && let Some(t) = queue.tracks.first_mut()
                    {
                        t.like = like;
                    }
                    // Starting a mix on the song already playing must not restart it: the queue
                    // is swapped under the running audio instead. Another song, or nothing
                    // loaded, starts the mix from its seed as before.
                    let seed_index = {
                        let now_id = self.now.track.as_ref().map(|t| t.video_id.as_str());
                        mix_seed_index(self.playback_started, now_id, &seed, &queue.tracks)
                    };
                    // A mix opens a flowing river, where a hand-picked start opens a latent one.
                    // No token means the mix itself was one page long.
                    let radio = Some(Radio {
                        state: queue.next.map_or(River::Dead, River::Open),
                        chosen: std::iter::once(seed.clone()).collect(),
                        seed: seed.clone(),
                        filters: queue.filters,
                        filtering: None,
                    });
                    if let Some(seed_index) = seed_index {
                        tracing::info!(%seed, "start mix: keeping the playing track");
                        self.replace_queue(queue.tracks, seed_index, radio);
                        self.q.set_current(seed_index);
                        // The mix's own row carries fresher state than the one now playing (its
                        // rating came with the mix); the audio is the same file either way.
                        if let Some(t) = self.q.get(seed_index).cloned() {
                            self.now.track = Some(t);
                        }
                        self.prefetch_next();
                    } else {
                        self.start_queue(queue.tracks, 0, radio);
                    }
                }
                Event::MixFailed { seed, error } => {
                    tracing::warn!(%seed, %error, "start mix failed");
                    self.toast_error(&failure_text("Could not start a mix", &error));
                }
                Event::MixSeeded { seed, queue } => {
                    self.radio_loading = false;
                    // Only the pending river this page was fetched for may take it: a queue
                    // started while the request was in flight is pending with its own seed, and
                    // an open river (Start Mix) is not pending at all. Switching Autoplay off
                    // mid-flight drops the page the same way.
                    let fresh = self.settings.autoplay
                        && self
                            .radio
                            .as_ref()
                            .is_some_and(|r| r.state == River::Pending && r.seed == seed);
                    if fresh {
                        let added = self
                            .q
                            .append_radio(&queue.tracks, self.radio.as_ref().map(|r| &r.chosen));
                        tracing::debug!(added, "seeded the autoplay tail");
                        if let Some(radio) = &mut self.radio {
                            radio.state = landed(added, queue.next);
                            radio.filters = queue.filters;
                        }
                        self.q.trim_played_prefix();
                    }
                }
                Event::RadioFiltered { playlist_id, queue } => {
                    let fresh = self.radio.as_ref().is_some_and(|r| {
                        r.filtering
                            .as_ref()
                            .is_some_and(|(id, _)| *id == playlist_id)
                    });
                    if fresh {
                        let chosen = self.radio.as_ref().map(|r| &r.chosen);
                        let dropped = self.q.drop_upcoming_radio(self.repeat, chosen);
                        let added = self.q.append_radio(&queue.tracks, chosen);
                        tracing::debug!(dropped, added, "filtered the radio");
                        if let Some(radio) = &mut self.radio {
                            radio.filtering = None;
                            radio.state = landed(added, queue.next);
                            // The reply marks its own chip selected; a reply without any keeps
                            // the old list, with the pick marked by hand.
                            if queue.filters.is_empty() {
                                for f in &mut radio.filters {
                                    f.selected = f.target.playlist_id == playlist_id;
                                }
                            } else {
                                radio.filters = queue.filters;
                            }
                        }
                        self.prefetch_next();
                    }
                }
                Event::RadioFilterFailed { playlist_id, error } => {
                    if let Some(radio) = &mut self.radio
                        && let Some((id, before)) = radio.filtering.take()
                    {
                        if id == playlist_id {
                            radio.state = before;
                            tracing::warn!(%error, "radio filter failed");
                            self.toast_error(&failure_text("Could not change the radio", &error));
                        } else {
                            radio.filtering = Some((id, before));
                        }
                    }
                }
                Event::MixSeedFailed { seed, error } => {
                    self.radio_loading = false;
                    // Same rule as a dead continuation: never retried, so a request storm is
                    // impossible. The queue simply ends where the listener's tracks do.
                    if self
                        .radio
                        .as_ref()
                        .is_some_and(|r| r.state == River::Pending && r.seed == seed)
                    {
                        tracing::warn!(%error, "autoplay seed failed");
                        if let Some(radio) = &mut self.radio {
                            radio.state = River::Dead;
                        }
                    }
                }
                Event::LyricsLoaded { video_id, lyrics }
                    if self.lyrics_for.as_deref() == Some(video_id.as_str()) =>
                {
                    self.lyrics_loading = false;
                    self.lyrics_error = None;
                    self.lyrics = Some(lyrics);
                }
                Event::LyricsFailed { video_id, error }
                    if self.lyrics_for.as_deref() == Some(video_id.as_str()) =>
                {
                    tracing::debug!(%video_id, %error, "no lyrics");
                    self.lyrics_loading = false;
                    self.lyrics = None;
                    self.lyrics_error = Some(error);
                }
                Event::LyricsLoaded { .. } | Event::LyricsFailed { .. } => {}
                Event::MixMore { token, queue } => {
                    self.radio_loading = false;
                    // A newer river may have opened while this was in flight — appending
                    // another river's tracks would corrupt the queue, so stale replies drop.
                    let fresh = self.settings.autoplay
                        && self
                            .radio
                            .as_ref()
                            .is_some_and(|r| matches!(&r.state, River::Open(t) if t == &token));
                    if fresh {
                        let added = self
                            .q
                            .append_radio(&queue.tracks, self.radio.as_ref().map(|r| &r.chosen));
                        tracing::debug!(added, "extended the radio river");
                        if let Some(radio) = &mut self.radio {
                            // A page that adds nothing ends the river. Keeping its token would
                            // leave the low-water mark still tripped, so the next frame asks
                            // again — and a river that keeps handing back tracks we already
                            // hold would be requested forever, one round trip at a time.
                            radio.state = landed(added, queue.next);
                        }
                        self.q.trim_played_prefix();
                    }
                }
                Event::MixMoreFailed { token, error } => {
                    self.radio_loading = false;
                    // Both the follow and the reseed failed — the network or the session is
                    // at fault, and retrying every frame would be a request storm. End the
                    // river quietly; what it delivered keeps playing and stays marked.
                    if self
                        .radio
                        .as_ref()
                        .is_some_and(|r| matches!(&r.state, River::Open(t) if t == &token))
                    {
                        tracing::warn!(%error, "radio river ended");
                        if let Some(radio) = &mut self.radio {
                            radio.state = River::Dead;
                        }
                    }
                }
                Event::RatingFailed {
                    video_id,
                    attempted,
                    previous,
                    error,
                } => {
                    // Revert only when the failed attempt is still what is on screen — a newer
                    // rating has its own refusal coming if it needs one.
                    if self.like_of(&video_id) == attempted {
                        tracing::warn!(%video_id, %error, "rating refused; reverting");
                        self.set_like(&video_id, previous);
                        self.toast_error(&failure_text("YouTube refused that rating", &error));
                    }
                }
                Event::LibraryToggled {
                    video_id,
                    in_library,
                } => {
                    // Confirmation only: the state was painted optimistically when asked for,
                    // and per-video toggles resolve in the order they were tapped.
                    tracing::debug!(%video_id, in_library, "library toggle confirmed");
                    self.toast(if in_library {
                        "Saved to library"
                    } else {
                        "Removed from library"
                    });
                }
                Event::LibraryToggleFailed {
                    video_id,
                    previous,
                    error,
                } => {
                    tracing::warn!(%video_id, %error, "library toggle refused; reverting");
                    self.set_library_state(&video_id, previous);
                    self.toast_error(&failure_text("Could not update your library", &error));
                }
                Event::PageLibraryToggled { in_library, .. } => {
                    tracing::debug!(in_library, "page library toggle confirmed");
                    // Already painted optimistically; this is the confirmation. The rail was
                    // spliced by the page that asked (see `splice_rail_playlist`); refetching
                    // here would race server propagation and can put a stale list back over
                    // the splice, so the next library load stays authoritative instead.
                    self.toast(if in_library {
                        "Saved to library"
                    } else {
                        "Removed from library"
                    });
                }
                Event::PlaylistEdited { id } => {
                    tracing::debug!(%id, "playlist updated");
                    self.edit_revert = None;
                    self.toast("Playlist updated");
                }
                Event::PlaylistEditFailed { id, error } => {
                    tracing::warn!(%id, %error, "playlist update refused");
                    if let Some((revert_id, old)) = self.edit_revert.take()
                        && revert_id == id
                    {
                        self.set_playlist_title(&id, old);
                    }
                    self.toast_error(&failure_text("Could not update the playlist", &error));
                }
                Event::PlaylistDeleted { id } => {
                    tracing::debug!(%id, "playlist deleted");
                    self.drop_playlist(&id);
                    let was_open = self.current_route() == Route::Playlist(id.clone());
                    if was_open {
                        self.navigate(Route::Library);
                    }
                    self.toast("Playlist deleted");
                }
                Event::PlaylistDeleteFailed { id, error } => {
                    tracing::warn!(%id, %error, "playlist delete refused");
                    self.toast_error(&failure_text("Could not delete the playlist", &error));
                }
                Event::PlaylistLibraryChanged { id, saved } => {
                    tracing::debug!(%id, saved, "playlist library membership changed");
                    if !saved {
                        self.drop_playlist(&id);
                        self.toast("Removed from library");
                    }
                }
                Event::PlaylistLibraryFailed { id, error } => {
                    tracing::warn!(%id, %error, "playlist library write refused");
                    self.toast_error(&failure_text("Could not update your library", &error));
                }
                Event::PageLibraryFailed {
                    album,
                    previous,
                    error,
                } => {
                    tracing::warn!(album, %error, "page library toggle refused; reverting");
                    self.set_page_library(album, previous);
                    self.toast_error(&failure_text("Could not update your library", &error));
                }
                Event::PlaylistBulkSaved {
                    playlist_id,
                    added,
                    duplicates,
                    total,
                } => {
                    tracing::debug!(%playlist_id, added, duplicates, total, "bulk save landed");
                    // The count went up by `total` when the save was asked for; whatever was
                    // not added comes back off.
                    self.bump_playlist_counts(&playlist_id, added as i64 - total as i64);
                    if duplicates > 0 && added + duplicates == total {
                        self.toast(&match (added, duplicates) {
                            (0, 1) => "Already in that playlist".to_owned(),
                            (0, _) => "All of these are already in that playlist".to_owned(),
                            (a, d) => format!("{a} saved to playlist, {d} already in it"),
                        });
                    } else if added == 0 {
                        self.toast_error("Could not save to that playlist");
                    } else if added < total {
                        self.toast(&format!("{added} of {total} songs saved to playlist"));
                    } else if added == 1 {
                        self.toast("Saved to playlist");
                    } else {
                        self.toast(&format!("{added} songs saved to playlist"));
                    }
                }
                Event::PlaylistSaved {
                    playlist_id,
                    video_id,
                } => {
                    tracing::debug!(%playlist_id, %video_id, "saved to playlist");
                    self.toast("Saved to playlist");
                    self.note_saved_to_playlist(&playlist_id, &video_id);
                }
                Event::PlaylistDuplicate {
                    playlist_id,
                    video_id,
                    message,
                } => {
                    tracing::debug!(%playlist_id, %video_id, "already in the playlist; asking");
                    // Not added (yet): "Add anyway" asks again, and counts it again.
                    self.bump_playlist_counts(&playlist_id, -1);
                    self.playlist_duplicate = Some(DuplicatePrompt {
                        playlist_id,
                        video_id,
                        message,
                    });
                }
                Event::PlaylistSaveFailed {
                    playlist_id,
                    video_id,
                    error,
                } => {
                    tracing::warn!(%playlist_id, %video_id, %error, "save to playlist refused");
                    self.bump_playlist_counts(&playlist_id, -1);
                    // YouTube says why it refused an edit, in the account's language; a request
                    // that failed outright gets the reason the error text gives, if any.
                    let text = match youtube_sentence(&error) {
                        Some(said) if failure_reason(&error).is_none() => {
                            format!("Could not save to that playlist: {said}")
                        }
                        _ => failure_text("Could not save to that playlist", &error),
                    };
                    self.toast_error(&text);
                }
                Event::PlaylistRemoved {
                    playlist_id,
                    video_id,
                } => {
                    tracing::debug!(%playlist_id, %video_id, "removed from playlist");
                    // The count went down when the removal was asked for.
                    self.toast("Removed from playlist");
                }
                Event::PlaylistRemoveFailed {
                    playlist_id,
                    video_id,
                    error,
                } => {
                    // The row is already gone and cannot be reconstructed — reload the page.
                    // `playlist_for` is cleared first: `load_playlist` treats a matching id
                    // as already loading and would no-op.
                    tracing::warn!(%playlist_id, %video_id, %error, "remove refused; reloading");
                    self.toast_error(&failure_text("Could not remove that song", &error));
                    // The open page reloads below with YouTube's own count; the library list
                    // has no reload of its own, so the removal is taken back there.
                    self.bump_playlist_counts(&playlist_id, 1);
                    if self.playlist.as_ref().is_some_and(|p| p.id == playlist_id) {
                        self.playlist = None;
                        self.playlist_for = None;
                        self.load_playlist(playlist_id);
                    }
                }
                Event::PlaylistMoved { playlist_id } => {
                    tracing::debug!(%playlist_id, "playlist reorder confirmed");
                }
                Event::PlaylistMoveFailed { playlist_id, error } => {
                    // The row already moved locally when the drag was released, and cannot be
                    // put back precisely — reload, the same recovery `PlaylistRemoveFailed`
                    // uses. `playlist_for` is cleared first: `load_playlist` treats a matching
                    // id as already loading and would no-op.
                    tracing::warn!(%playlist_id, %error, "reorder refused; reloading");
                    self.toast_error(&failure_text("Could not reorder that playlist", &error));
                    if self.playlist.as_ref().is_some_and(|p| p.id == playlist_id) {
                        self.playlist = None;
                        self.playlist_for = None;
                        self.load_playlist(playlist_id);
                    }
                }
                Event::ImageReady(loaded) => self.images.insert(ctx, *loaded),
                Event::ImageFailed(url, px, wide) => self.images.mark_failed(url, px, wide),

                Event::PlaybackFailed {
                    track,
                    error,
                    generation,
                } if generation == self.play_generation => {
                    // The player bar shows this too, but only while it is looked at: a play that
                    // fails while the queue runs on deserves a line in the log either way.
                    tracing::warn!(
                        video_id = %track.video_id,
                        title = %track.title,
                        %error,
                        "playback failed"
                    );
                    let was_video = self.play_is_video;
                    self.play_is_video = false;
                    self.now = NowPlaying {
                        track: Some(track),
                        resolving: false,
                        error: Some(error),
                        paused: false,
                        playlist_playing: self.now.playlist_playing.clone(),
                    };
                    self.playback_started = false;
                    // The video path is the fragile one (yt-dlp, a direct stream); when it is
                    // what failed, play the song's audio rather than leaving the view over a
                    // frozen frame. `reload_current` picks the audio path now that the flag is
                    // clear, and its own generation supersedes this failed attempt.
                    if was_video {
                        self.video_open = false;
                        self.toast("Video failed — playing the song instead");
                        self.reload_current();
                    } else if let Some(player) = &self.player {
                        // The song before this one is still loaded and would play on under the
                        // error, with play/pause now aimed at the failed track instead of it.
                        // Not on the video path: `reload_current` reads the position from what
                        // is still playing, to carry on from there.
                        player.stop();
                    }
                }
                Event::PlaybackFailed { .. } => {}
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Playback
    // ------------------------------------------------------------------------------------------

    /// Make `tracks` the queue and start playing `index`.
    pub(crate) fn start_from(&mut self, tracks: Vec<Track>, index: usize) {
        if index >= tracks.len() {
            return;
        }
        // The clicked row is already the loaded track *and* this is the queue it is in:
        // pause/resume it, the same convention an album/playlist header's own Play button
        // follows, rather than restarting it from the top. The whole-list comparison matters:
        // a mix seeded from the song playing now opens with that song, and matching the track
        // alone made Start Mix just toggle playback, leaving the queue untouched.
        if self.q.current() == Some(index) && same_queue(self.q.tracks(), &tracks) {
            self.toggle_playback();
            return;
        }
        // Every hand-picked queue carries a latent river; `Event::MixLoaded` opens a flowing one
        // instead. The Autoplay setting decides whether either is asked for a page, so switching
        // it on mid-queue works.
        let radio = Radio::pending(&tracks);
        self.start_queue(tracks, index, radio);
    }

    /// Make `tracks` the queue, with `radio` as its river, and play `index`.
    fn start_queue(&mut self, tracks: Vec<Track>, index: usize, radio: Option<Radio>) {
        self.replace_queue(tracks, index, radio);
        self.play_index_restore(index);
    }

    /// Swap the queue under whatever is (or is not) playing, without loading anything. The old
    /// queue and its river are remembered for `Previous`, and the panel is asked to show the new
    /// queue's start. `start` is the track about to play, which a shuffle draws first.
    fn replace_queue(&mut self, tracks: Vec<Track>, start: usize, radio: Option<Radio>) {
        let old = std::mem::replace(&mut self.radio, radio);
        self.q
            .replace(tracks, old, start, self.radio.as_ref().map(|r| &r.chosen));
        // A brand new queue, so the panel should show its start — matching what the queue
        // panel's own row click already does for the in-queue case. Set here rather than in
        // `play_index_restore` itself, which is also what natural end-of-track auto-advance and
        // Previous/Next share: neither of those should reset the scroll position on every
        // track change.
        self.queue_scroll_reset = true;
    }

    /// A single-song click — a search result or a Home shelf — where the list around the song
    /// is not something to play through. A YouTube track gets its own radio: the queue is that
    /// song, and the generated next-up is appended as it arrives. A local file keeps the list it
    /// was clicked in, since there is no radio to seed from a filesystem path. Album, playlist,
    /// artist and library rows use [`Self::start_from`] instead: the list plays, then its radio.
    ///
    /// The song already loaded is paused or resumed instead, as its cover's button shows. The
    /// queue can't tell: it has grown the song's radio, so it never equals the one-song list
    /// [`Self::start_from`] compares, and every press restarted the song.
    pub(crate) fn play_row(&mut self, tracks: Vec<Track>, index: usize) {
        let loaded = |t: &Track| {
            self.now
                .track
                .as_ref()
                .is_some_and(|nt| nt.video_id == t.video_id)
        };
        if tracks.get(index).is_some_and(loaded) {
            self.toggle_playback();
            return;
        }
        match tracks.get(index) {
            Some(t) if t.local_path.is_none() => {
                let track = t.clone();
                self.start_from(vec![track], 0);
            }
            Some(_) => self.start_from(tracks, index),
            None => {}
        }
    }

    /// Start the queue item at `index`, remembering the track we are leaving for `Previous`.
    pub(crate) fn play_index(&mut self, index: usize) {
        // Already the loaded track: pause/resume it rather than restarting from the top — the
        // same convention `start_from` follows for the same reason. `playback_started` matters:
        // a restored queue arrives with `current` set but nothing loaded, and that case must
        // fall through and actually load the track rather than toggle (which would recurse).
        if self.q.current() == Some(index) && self.playback_started {
            self.toggle_playback();
            return;
        }
        self.q.remember_current();
        self.play_index_restore(index);
    }

    /// Up next's ✕: the listener's songs still to come go; the playing one and the radio after
    /// them stay.
    pub(crate) fn clear_up_next(&mut self) {
        let chosen = self.radio.as_ref().map(|r| &r.chosen);
        if self.q.clear_up_next(self.repeat, chosen) > 0 {
            self.toast("Cleared your queue");
        }
    }

    /// A click on a song in Up next. With "Keep the songs you jump over" on, the song is brought
    /// up next first (`PlayQueue::bring_up_next`), so the songs between it and the playing one
    /// still play, after it; off, it is a plain jump to that place in the queue.
    pub(crate) fn play_from_queue(&mut self, index: usize) {
        if self.settings.queue_click_keeps_order && self.q.current() != Some(index) {
            // Clicked to play, so it is the listener's choice now: a radio song moved in among
            // their own must not end Repeat All's loop early (see `PlayQueue::tail_start`).
            if let (Some(radio), Some(track)) = (&mut self.radio, self.q.get(index)) {
                radio.chosen.insert(track.video_id.clone());
            }
            if let Some(to) = self.q.bring_up_next(index) {
                self.play_index(to);
                return;
            }
        }
        self.play_index(index);
    }

    /// `play_index` without touching the history — for `Previous` itself and a fresh queue.
    fn play_index_restore(&mut self, index: usize) {
        let Some(track) = self.q.get(index).cloned() else {
            return;
        };
        let Some(player) = self.player.as_mut() else {
            return;
        };
        let handle = player.handle();
        // The user's volume, not `player.volume()`: the Audio normaliser folds each track's gain
        // into the live value, and the next track must not inherit this one's.
        let volume = if self.muted { 0.0 } else { self.last_volume };
        let init_error = player.init_error().map(str::to_owned);
        let Some(handle) = handle else {
            // libmpv could not be brought up — surface it now, the same message an eager
            // failure used to show at startup.
            self.player_error = init_error;
            return;
        };
        // A retry that worked clears the note an earlier failure put up.
        self.player_error = None;
        self.q.set_current(index);
        self.playback_started = false;
        // A restored queue — or a scrub before play — carries a position for this very track;
        // it rides the load as a per-file `start`, and is consumed here so a later frame does
        // not seek the newly opened file a second time.
        let start_at = self.q.take_resume_for(index);
        self.play_generation = self.start_track(track, handle, volume, start_at);
    }

    /// Ask for the next track's stream ahead of time. Under shuffle too: the order is drawn when
    /// shuffle goes on, so the next track is known.
    fn prefetch_next(&self) {
        let chosen = self.radio.as_ref().map(|r| &r.chosen);
        if let Some(next) = self
            .q
            .peek_next(self.repeat, chosen)
            .and_then(|i| self.q.get(i))
        {
            // The video view will hand the next track to `play_video`, which needs the video
            // resolver's URLs — an audio resolve would be thrown away. An audio-only track still
            // gets the audio resolve, since that is what it will play.
            if self.video_open && next.wide {
                self.backend.prefetch_video(next.clone());
            } else {
                self.backend.prefetch(next.clone());
            }
        }
    }

    pub(crate) fn skip(&mut self, delta: isize) {
        if delta < 0 {
            self.previous();
            return;
        }
        let next = self
            .q
            .next(self.repeat, self.radio.as_ref().map(|r| &r.chosen));
        match next {
            // Next landing on the track already playing — a one-track queue under Repeat All, or
            // a shuffle with nothing else left — replays it. `play_index` would take that for a
            // click on the loaded track and pause it.
            Some(next) if self.q.current() == Some(next) => self.play_index_restore(next),
            Some(next) => self.play_index(next),
            // At the end with Repeat One, "next" replays the current track.
            None if self.repeat == RepeatMode::One => {
                if let Some(current) = self.q.current() {
                    self.play_index_restore(current);
                }
            }
            None => {}
        }
    }

    /// Advance one repeat mode: Off → All → One → Off.
    pub(crate) fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        };
    }

    /// Set the volume from the slider or the desktop's media controls. Silence is a mute: the
    /// last audible volume stays, to come back to on unmute and on the next launch.
    pub(crate) fn set_user_volume(&mut self, volume: f64) {
        let volume = volume.clamp(0.0, 100.0);
        (self.last_volume, self.muted) = volume_after(self.last_volume, volume);
        if let Some(player) = self.player.as_mut() {
            player.set_volume(if self.muted { 0.0 } else { self.last_volume });
        }
    }

    /// One step of the volume keys: up or down from what is heard, so a step up while muted
    /// starts from silence rather than jumping past the remembered volume.
    pub(crate) fn nudge_volume(&mut self, step: f64) {
        self.set_user_volume(nudged_volume(self.muted, self.last_volume, step));
    }

    /// Mute at the current volume, or restore whatever it was — the same "remember, then put
    /// back" the volume slider uses ([`Self::set_user_volume`] never overwrites `last_volume`
    /// with silence).
    pub(crate) fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let volume = if self.muted { 0.0 } else { self.last_volume };
        if let Some(player) = self.player.as_mut() {
            player.set_volume(volume);
        }
    }

    /// Turn shuffle on or off: see [`Self::set_shuffle`].
    pub(crate) fn toggle_shuffle(&mut self) {
        self.set_shuffle(!self.q.is_shuffled());
    }

    /// Turn shuffle on or off. On, the rest of the queue is drawn into an order that then holds,
    /// so Up next shows it and the next track is preloaded; off, the list order goes on from the
    /// track playing.
    pub(crate) fn set_shuffle(&mut self, on: bool) {
        self.q
            .set_shuffle(on, self.radio.as_ref().map(|r| &r.chosen));
        self.prefetch_next();
    }

    /// Go back: first within the track, then within this queue, then to the queue that preceded
    /// it. A press while the track is underway restarts it (see [`previous_restarts`]); only a
    /// press near its start steps back a track. Going back a track rewinds the queue state
    /// itself — an autoplay tail added since is dropped, and a replaced queue is restored —
    /// rather than only moving the cursor.
    pub(crate) fn previous(&mut self) {
        if self.restart_current() {
            return;
        }
        // Under shuffle, the track before this one in the order it is playing in.
        if let Some(index) = self.q.previous_in_order() {
            self.play_index_restore(index);
            return;
        }
        match self.q.rewind() {
            Some(Rewind::WithinQueue { index, queue_len }) => {
                self.q.truncate(queue_len);
                self.play_index_restore(index);
            }
            Some(Rewind::PreviousQueue {
                queue,
                index,
                radio,
            }) => {
                self.radio = radio;
                self.q
                    .restore_queue(queue, index, self.radio.as_ref().map(|r| &r.chosen));
                self.play_index_restore(index);
            }
            // Nothing remembered: a plain positional step, like any transport — except under
            // shuffle, where list order is not the order things played in, so there is nothing
            // honest to step back to.
            None => {
                if !self.q.is_shuffled()
                    && let Some(current) = self.q.current()
                    && current > 0
                {
                    self.play_index_restore(current - 1);
                }
            }
        }
    }

    /// Restart the current track when Previous is pressed with it underway, returning whether it
    /// did — the caller then leaves the queue alone. A track that has not loaded yet (a restored
    /// session) has no mpv position, so its saved resume position stands in, exactly as
    /// `publish_media` substitutes it.
    fn restart_current(&mut self) -> bool {
        let Some(current) = self.q.current() else {
            return false;
        };
        let position = if self.playback_started {
            self.player
                .as_ref()
                .map(|p| p.state().position)
                .unwrap_or(0.0)
        } else {
            self.q.resume_here().unwrap_or(0.0)
        };
        if !previous_restarts(position) {
            return false;
        }
        if self.playback_started && !self.play_is_video {
            if let Some(player) = self.player.as_ref() {
                player.seek(0.0);
            }
        } else {
            // A video carries its audio as a separately attached stream, and a post-load seek
            // moves only the video (see `player::set_start`), so restarting one means loading it
            // again from the top. A restored, not-yet-loaded track is the same: it has to load,
            // and starting over makes its saved position wrong.
            self.q.set_resume(None);
            self.play_index_restore(current);
        }
        true
    }

    /// Move playback to `seconds` into the current song: the seek bar and a synced lyric line
    /// both land here. Before playback has started there is nothing to seek, so it moves the
    /// restored position instead.
    pub(crate) fn seek_to(&mut self, seconds: f64) {
        if self.playback_started {
            if let Some(pl) = &self.player {
                pl.seek(seconds);
            }
        } else if let Some(index) = self.q.current() {
            self.q.set_resume(Some((index, seconds)));
        }
    }

    /// The second at which the next synced lyric line starts, when lyrics are on screen: what a
    /// slow frame rate has to wake for, or the highlight would trail the singer by up to a second.
    fn next_lyric_at(&self, position: f64) -> Option<f64> {
        let lyrics = self.lyrics.as_ref().filter(|l| l.synced)?;
        if !self.lyrics_wanted() {
            return None;
        }
        lyrics
            .lines
            .iter()
            .map(|l| l.at_ms as f64 / 1000.0)
            .find(|at| *at > position)
    }

    /// Whether anything on screen shows lyrics: the side panel, or the fullscreen view's column.
    fn lyrics_wanted(&self) -> bool {
        self.lyrics_open || (self.fs.open && self.fs.lyrics)
    }

    /// Whether the current song is known to have no lyrics: the request for it has finished and
    /// found none. See [`ui::fullscreen::lyrics_missing`].
    pub(crate) fn lyrics_missing(&self) -> bool {
        let current = self.now.track.as_ref().map(|t| t.video_id.as_str());
        ui::fullscreen::lyrics_missing(
            current.is_some() && self.lyrics_for.as_deref() == current,
            self.lyrics_loading,
            self.lyrics.as_ref().map(|l| l.lines.len()),
        )
    }

    /// Arrange shelves by the listener's Home order — for the default feed only: with a mood chip
    /// selected the shelves are that mood's own and keep the order they came in.
    fn order_home_shelves(&self, shelves: &mut [crate::model::Shelf]) {
        if self.home_filter.is_none() {
            crate::model::order_home(
                shelves,
                &crate::model::normalise_order(&self.settings.home_order),
            );
        }
    }

    /// Open the fullscreen now-playing view and take the window fullscreen with it. Does
    /// nothing with no song loaded: there is nothing to show.
    pub(crate) fn fs_enter(&mut self, ctx: &egui::Context) {
        if self.now.track.is_none() || self.fs.open {
            return;
        }
        let was_fullscreen = crate::window_fullscreen::is(ctx);
        tracing::debug!(was_fullscreen, "fullscreen view: entering");
        self.fs = ui::fullscreen::State::entering(self.settings.fullscreen_lyrics, was_fullscreen);
        if !self.fs.window_was_fullscreen {
            crate::window_fullscreen::set(ctx, true);
        }
    }

    /// Close the fullscreen view, and leave window fullscreen unless something else had it on
    /// first (the video view's own).
    pub(crate) fn fs_exit(&mut self, ctx: &egui::Context) {
        if !self.fs.open {
            return;
        }
        if self.fs.manages_window && !self.fs.window_was_fullscreen {
            crate::window_fullscreen::set(ctx, false);
        }
        self.fs.open = false;
        self.fs.queue = false;
    }

    /// Open or close the lyrics panel. Independent of the queue, which shares the same panel.
    pub(crate) fn toggle_lyrics(&mut self) {
        self.lyrics_open = !self.lyrics_open;
        if self.lyrics_open
            && let Some(track) = self.now.track.clone()
        {
            self.fetch_lyrics(track);
        }
    }

    /// Keep an open lyrics panel on the loaded track. Song changes and the panel's own toggle
    /// already ask; this covers the ways a track arrives without either — a session restored at
    /// launch, or `--lyrics` opening the panel before it — which left the panel saying "Play a
    /// song" over a song sitting right there in the bar. Once per track: `lyrics_for` is set
    /// by the request, so a failed fetch is not retried every frame.
    fn sync_lyrics(&mut self) {
        if self.lyrics_wanted()
            && !self.lyrics_loading
            && let Some(track) = self.now.track.as_ref()
            && self.lyrics_for.as_deref() != Some(track.video_id.as_str())
        {
            let track = track.clone();
            self.fetch_lyrics(track);
        }
    }

    /// Start a lyrics request for `track`, replacing whatever is shown. The `lyrics_for` guard
    /// drops the reply if the track changes before it lands.
    fn fetch_lyrics(&mut self, track: Track) {
        self.lyrics_for = Some(track.video_id.clone());
        self.lyrics = None;
        self.lyrics_error = None;
        self.lyrics_loading = true;
        self.lyrics_scroll_reset = true;
        self.backend
            .fetch_lyrics(track, self.settings.lrclib_lyrics);
    }

    /// Put `track` straight after the playing one, in the list and in a shuffle's order.
    pub(crate) fn play_next(&mut self, track: Track) {
        self.take_from_radio(&track.video_id);
        match self.q.current() {
            Some(i) => {
                if let Some(radio) = &mut self.radio {
                    radio.chosen.insert(track.video_id.clone());
                }
                self.q.insert(
                    i + 1,
                    track,
                    Slot::Next(0),
                    self.radio.as_ref().map(|r| &r.chosen),
                );
                self.prefetch_next();
            }
            // Nothing playing yet: next is meaningless, so this becomes a plain append.
            None => self.add_to_queue(track),
        }
    }

    /// Append `track` to the end of the queue, without disturbing what is playing. With a
    /// river's tail already queued, "the end" is before its songs still to come: the listener's
    /// additions come after the songs they chose and before the algorithm's, in a shuffle's
    /// order as well.
    pub(crate) fn add_to_queue(&mut self, track: Track) {
        self.take_from_radio(&track.video_id);
        if let Some(radio) = &mut self.radio {
            radio.chosen.insert(track.video_id.clone());
        }
        let chosen = self.radio.as_ref().map(|r| &r.chosen);
        let at = self.q.own_end(chosen);
        self.q.insert(at, track, Slot::BeforeRadio, chosen);
    }

    /// Take `video_id` out of the radio's songs still to come, before the listener queues it
    /// themselves: it moves from Autoplay to Next up rather than playing twice.
    fn take_from_radio(&mut self, video_id: &str) {
        if let Some(radio) = &self.radio {
            self.q
                .take_upcoming_radio(video_id, self.repeat, Some(&radio.chosen));
        }
    }

    /// Whether there is a confirmed session. Every write — rating, the library, playlists —
    /// needs one, and the controls for them are hidden without it.
    pub(crate) fn signed_in(&self) -> bool {
        matches!(self.auth, crate::auth::AuthState::SignedIn { .. })
    }

    /// Whether the track currently loaded — playing, paused, or stopped — is one of `tracks`.
    /// An album/playlist header's own Play button reads this to decide what clicking it means:
    /// resume or pause what is already loaded (see [`Self::is_playing_any`] and
    /// [`Self::toggle_playback`]), rather than always restarting the whole list from the top.
    ///
    /// Membership in the list, not "did the queue start from this exact page": tracking the
    /// latter needs a separate flag that autoplay's own radio continuation, a manual reorder or
    /// "play next" would all have to remember to keep honest, and any place that missed one
    /// would show a stale Play/Pause. Whether the current track is still one of this page's own
    /// is self-correcting every frame instead, at the cost of not noticing once playback has
    /// moved on to radio tracks past the end of a shorter list.
    pub(crate) fn queue_matches(&self, tracks: &[Track]) -> bool {
        self.now
            .track
            .as_ref()
            .is_some_and(|t| tracks.iter().any(|x| x.video_id == t.video_id))
    }

    /// [`Self::queue_matches`], and actually playing right now rather than paused or stopped —
    /// what an album/playlist header's own Play button shows Pause for.
    pub(crate) fn is_playing_any(&self, tracks: &[Track]) -> bool {
        self.playback_started
            && self.player.as_ref().is_some_and(|p| !p.state().paused)
            && self.queue_matches(tracks)
    }

    /// One handler for every song menu in the app, so the pages forward the choice rather
    /// than copying the logic.
    pub(crate) fn row_action(&mut self, action: crate::ui::RowAction) {
        use crate::ui::RowAction;
        match action {
            RowAction::PlayTarget { seed, target } => self.play_target(seed, target),
            RowAction::Subscribe {
                channel_id,
                subscribe,
                params,
            } => self.toggle_subscribed(&channel_id, subscribe, params),
            RowAction::DontRecommend { token, name } => {
                if !self.signed_in() {
                    self.toast_error("Sign in to change your recommendations");
                    return;
                }
                self.backend.send_feedback(token);
                self.toast(&format!("Won't recommend {name}"));
            }
            RowAction::StartMix(id) => {
                // Show what it did: a mix replaces the queue with fifty tracks you have not
                // seen, and leaving the panel shut makes that invisible.
                self.upnext_open = true;
                self.backend.start_mix(id);
            }
            RowAction::StartMixPlaylist(id) => {
                self.upnext_open = true;
                self.backend.start_playlist_mix(id);
            }
            RowAction::Download(track) => self.start_download(track),
            RowAction::DownloadMany(tracks) => self.start_download_many(tracks),
            RowAction::RemoveDownload(video_id) => self.remove_download(&video_id),
            RowAction::PlayNext(track) => {
                self.play_next(track);
                self.toast("Song will play next");
            }
            RowAction::AddToQueue(track) => {
                self.add_to_queue(track);
                self.toast("Song added to queue");
            }
            RowAction::GoToAlbum(id) => self.navigate(Route::Album(id)),
            RowAction::GoToArtist(id) => self.navigate(Route::Artist(id)),
            RowAction::CopyLink(id) => {
                self.backend.copy_text(crate::model::track_url(&id));
                self.toast("Link copied");
            }
            RowAction::Rate { video_id, status } => {
                if !self.signed_in() {
                    // No optimistic update and no request: without a session the write is
                    // impossible, and painting it first would flicker.
                    self.toast_error("Sign in to like songs");
                    return;
                }
                // Optimistic: a thumb that waits for a round trip feels unresponsive. The refusal
                // carries the previous state back.
                let previous = self.like_of(&video_id);
                self.set_like(&video_id, status);
                self.backend.rate(video_id, status, previous);
            }
            RowAction::ToggleLibrary(track) => {
                if !self.signed_in() {
                    self.toast_error("Sign in to save songs");
                    return;
                }
                let saving = !track.in_library;
                let token = if saving {
                    track.library_add.as_deref()
                } else {
                    track.library_remove.as_deref()
                };
                let Some(token) = token else {
                    tracing::warn!(video = %track.video_id, "no bookmark token; not saved");
                    self.toast_error("This song cannot be saved");
                    return;
                };
                self.set_library_state(&track.video_id, saving);
                self.backend
                    .toggle_library(token.to_owned(), track.video_id, saving);
            }
            RowAction::SaveToPlaylist {
                playlist_id,
                video_id,
            } => {
                // A local playlist's own target list (`widgets::row_menu`) only ever offers
                // other local playlists, so this id names one — `video_id` is that track's
                // canonical path, per `Track.local_path`'s own doc comment.
                if self.is_local_playlist(&playlist_id) {
                    self.add_to_local_playlist(&playlist_id, std::path::PathBuf::from(video_id));
                    return;
                }
                if !self.signed_in() {
                    self.toast_error("Sign in to edit playlists");
                    return;
                }
                self.bump_playlist_counts(&playlist_id, 1);
                self.backend.add_to_playlist(playlist_id, video_id, false);
            }
            RowAction::RemoveFromPlaylist {
                playlist_id,
                video_id,
                set_video_id,
            } => {
                if self.is_local_playlist(&playlist_id) {
                    // `video_id` is the track's own canonical path, per `Track.local_path`'s
                    // own doc comment — the same string `local_playlist_as_playlist` gave this
                    // row as its `set_video_id` so this action could even be offered.
                    let path = std::path::PathBuf::from(&video_id);
                    if let Some(lp) = self
                        .local_playlists
                        .iter_mut()
                        .find(|p| p.id == playlist_id)
                    {
                        lp.track_paths.retain(|p| p != &path);
                    }
                    if let Err(e) = crate::local_playlists::save(&self.local_playlists) {
                        tracing::warn!(error = %format!("{e:#}"), "could not save local playlists");
                    }
                    self.remove_playlist_row(&playlist_id, &video_id);
                    self.resync_local_playlists();
                    return;
                }
                if !self.signed_in() {
                    self.toast_error("Sign in to edit playlists");
                    return;
                }
                // Optimistic: the row leaves at once; a refusal cannot put it back, so that
                // case refetches the page instead of guessing.
                self.remove_playlist_row(&playlist_id, &video_id);
                self.bump_playlist_counts(&playlist_id, -1);
                self.backend
                    .remove_from_playlist(playlist_id, video_id, set_video_id);
            }
            RowAction::PageShuffle(tracks) => {
                self.shuffle_play(tracks);
            }
            RowAction::PageWhenLoaded {
                page_id,
                kind,
                then,
            } => {
                if self.page_tracks(&page_id).is_some() {
                    // The fetch landed while the menu was open.
                    self.page_then(&page_id, then);
                    return;
                }
                // The right-click asked for the songs already; asking again only matters when
                // that fetch was lost, and a second answer finds nothing pending.
                match kind {
                    crate::ui::PageKind::Album => self.backend.fetch_album(page_id.clone()),
                    crate::ui::PageKind::Playlist => self.backend.fetch_playlist(page_id.clone()),
                }
                self.pending_page_action = Some((page_id, then));
            }
            RowAction::PagePlayNext(tracks) => {
                let n = tracks.len();
                match self.q.current() {
                    Some(i) => {
                        for (k, t) in tracks.into_iter().enumerate() {
                            if let Some(radio) = &mut self.radio {
                                radio.chosen.insert(t.video_id.clone());
                            }
                            self.q.insert(
                                i + 1 + k,
                                t,
                                Slot::Next(k),
                                self.radio.as_ref().map(|r| &r.chosen),
                            );
                        }
                        self.prefetch_next();
                    }
                    // Nothing playing yet: next is meaningless, so this becomes an append.
                    None => self.queue_many(tracks),
                }
                let text = Self::songs_will(n, "will play next");
                self.toast(&text);
            }
            RowAction::PageQueue(tracks) => {
                let n = tracks.len();
                self.queue_many(tracks);
                let text = Self::songs_will(n, "added to queue");
                self.toast(&text);
            }
            RowAction::PageSaveToPlaylist {
                playlist_id,
                title,
                video_ids,
            } => {
                if self.is_local_playlist(&playlist_id) {
                    let n = video_ids.len();
                    for video_id in video_ids {
                        self.add_to_local_playlist(
                            &playlist_id,
                            std::path::PathBuf::from(video_id),
                        );
                    }
                    self.toast(&format!("Saved {n} songs to {title}"));
                    return;
                }
                if !self.signed_in() {
                    self.toast_error("Sign in to edit playlists");
                    return;
                }
                let n = video_ids.len();
                self.bump_playlist_counts(&playlist_id, n as i64);
                self.backend.add_many_to_playlist(playlist_id, video_ids);
                // Says the work started; the bulk event's verdict overwrites it.
                self.toast(&format!("Saving {n} songs to {title}…"));
            }
            RowAction::PageSaveAsNewPlaylist { title, video_ids } => {
                // Same two-line handoff `save_as_playlist_row`'s button already does — this
                // just reaches it from a folder card's right-click menu instead.
                self.new_playlist = Some(title);
                self.new_playlist_kind = NewPlaylistKind::Local {
                    track_paths: video_ids
                        .into_iter()
                        .map(std::path::PathBuf::from)
                        .collect(),
                };
            }
            RowAction::PageLibrary {
                album,
                write,
                in_library,
            } => {
                if !self.signed_in() {
                    self.toast_error("Sign in to save to your library");
                    return;
                }
                self.set_page_library(album, in_library);
                self.backend.toggle_page_library(album, write, in_library);
            }
            RowAction::PageCopyLink(url) => {
                self.backend.copy_text(url);
                self.toast("Link copied");
            }
            RowAction::PageInfo => {
                // The text lives on the page; the page sets `page_info` when forwarding.
            }
            RowAction::EditPlaylist(id) => self.begin_edit_playlist(id),
            RowAction::DeletePlaylist(id) => self.request_delete_playlist(id),
            RowAction::UnsavePlaylist(id) => self.backend.unsave_playlist(id),
            RowAction::RemoveFromQueue(i) => {
                // The "Now playing" row never carries this menu; the queue refuses to remove
                // the playing track anyway, and moves everything that indexes it.
                self.q.remove(i);
            }
        }
    }

    /// Play `tracks` in random order. Fisher–Yates over a time-seeded LCG — no rand dependency
    /// for one shuffle. A hand-picked queue closes the river, same as `start_from`.
    pub(crate) fn shuffle_play(&mut self, mut tracks: Vec<Track>) {
        let mut x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e3779b9)
            .wrapping_add(std::process::id() as u64);
        for i in (1..tracks.len()).rev() {
            x = x
                .wrapping_mul(0x5851f42d4c957f2d)
                .wrapping_add(0x14057b7ef767814f);
            tracks.swap(i, (x >> 33) as usize % (i + 1));
        }
        self.start_from(tracks, 0);
    }

    /// Append `tracks` to the queue in order, each landing before the river's tail.
    fn queue_many(&mut self, tracks: Vec<Track>) {
        for t in tracks {
            self.add_to_queue(t);
        }
    }

    /// Paint a page header's library state at once; a refusal carries the previous state back.
    fn set_page_library(&mut self, album: bool, in_library: bool) {
        if album {
            if let Some(a) = self.album.as_mut() {
                a.in_library = in_library;
            }
        } else if let Some(p) = self.playlist.as_mut() {
            p.in_library = in_library;
        }
    }

    /// Show a playlist save or removal in the rail at once. A saved list is "recently saved",
    /// so it lands after Liked Music — the top of the user's own lists, where YouTube's order
    /// would put it — and a removed one leaves. The next library load re-sorts authoritatively.
    pub(crate) fn splice_rail_playlist(
        &mut self,
        playlist: crate::model::PlaylistRef,
        saved: bool,
    ) {
        self.library.playlists.items.retain(|p| p.id != playlist.id);
        if saved {
            let at = self
                .library
                .playlists
                .items
                .iter()
                .position(|p| crate::model::is_liked_auto(&p.id))
                .map_or(0, |i| i + 1);
            self.library.playlists.items.insert(at, playlist);
        }
        self.save_playlist_order();
    }

    /// Apply the saved rail order: named playlists keep the order the user dragged them into;
    /// anything new stays after them in YouTube's own order (a stable sort by rank). Ids the
    /// saved list does not know about rank last, so a new playlist appears at the end.
    fn apply_playlist_order(&mut self) {
        order_by_saved(
            &mut self.library.playlists.items,
            &self.settings.playlist_order,
        );
    }

    /// Remember the rail's current order, by id, so it survives a library refresh and a restart.
    fn save_playlist_order(&mut self) {
        self.settings.playlist_order = self
            .library
            .playlists
            .items
            .iter()
            .map(|p| p.id.clone())
            .collect();
        if let Err(e) = crate::settings::save(&self.settings) {
            tracing::warn!(error = %format!("{e:#}"), "could not save the playlist order");
        }
    }

    /// Move a rail playlist from one index to another (drag and drop), and remember it.
    pub(crate) fn move_playlist(&mut self, from: usize, to: usize) {
        let items = &mut self.library.playlists.items;
        if from >= items.len() || to >= items.len() || from == to {
            return;
        }
        let p = items.remove(from);
        items.insert(to, p);
        self.save_playlist_order();
    }

    /// Follow or unfollow, painted at once and reverted if YouTube refuses. `params` is the
    /// page button's own token for the direction — see `ArtistActions`'s field docs.
    pub(crate) fn toggle_subscribed(
        &mut self,
        channel_id: &str,
        subscribe: bool,
        params: Option<Arc<str>>,
    ) {
        if !self.signed_in() {
            self.toast_error("Sign in to subscribe");
            return;
        }
        self.set_subscribed_state(channel_id, subscribe);
        self.backend
            .set_subscribed(channel_id.to_owned(), subscribe, params);
    }

    /// Start whatever YouTube handed over as something to play — an artist's shuffle or mix.
    pub(crate) fn play_target(&mut self, seed: String, target: crate::model::WatchTarget) {
        self.upnext_open = true;
        self.backend.play_target(seed, target);
    }

    /// Paint a subscription state wherever this artist appears, so the button answers at once.
    fn set_subscribed_state(&mut self, channel_id: &str, subscribed: bool) {
        if let Some(a) = self.artist.as_mut()
            && a.id == channel_id
        {
            a.actions.subscribed = Some(subscribed);
        }
        let mark = |items: &mut Vec<crate::model::Entry>| {
            for e in items {
                if let crate::model::Entry::Artist(a) = e
                    && a.id == channel_id
                {
                    a.actions.subscribed = Some(subscribed);
                }
            }
        };
        mark(&mut self.results.items);
        self.for_each_feed_shelf_mut(|shelf| mark(&mut shelf.items));
    }

    /// Fetch the Explore feed once. Works signed out, so no auth gate.
    fn load_explore(&mut self) {
        if self.explore.is_some() || self.explore_loading || self.explore_error.is_some() {
            return;
        }
        self.explore_loading = true;
        self.backend.fetch_explore();
    }

    /// Fetch New releases once. Works signed out, so no auth gate.
    fn load_new_releases(&mut self) {
        if self.releases.is_some() || self.releases_loading || self.releases_error.is_some() {
            return;
        }
        self.releases_loading = true;
        self.backend.fetch_new_releases();
    }

    /// Fetch Charts once per params. Works signed out, so no auth gate.
    fn load_charts(&mut self, params: Option<String>) {
        if self.charts_params == params
            && (self.charts.is_some() || self.charts_loading || self.charts_error.is_some())
        {
            return;
        }
        self.charts = None;
        self.charts_error = None;
        self.charts_loading = true;
        self.charts_params = params.clone();
        self.backend.fetch_charts(params);
    }

    /// Fetch every mood category once. Works signed out, so no auth gate.
    fn load_moods(&mut self) {
        if self.moods.is_some() || self.moods_loading || self.moods_error.is_some() {
            return;
        }
        self.moods_loading = true;
        self.backend.fetch_moods();
    }

    /// Fetch what you played, once per visit.
    ///
    /// Unlike every sibling `load_*`, "already loaded" is *not* `history_shelves.is_some()`:
    /// History is time-based, so each entry to the page refetches. `note_route_entered` bumps
    /// `history_epoch` on the way in and this fetches for the new epoch, keeping the shelves
    /// already on screen up until the reply replaces them. The latched error still stops the
    /// per-frame safety net from hammering a failing fetch; a visit (or `retry_route`) clears it.
    ///
    /// Signed out the page shows a prompt instead — YouTube answers history with "sign in" and
    /// parsing that would only produce an error.
    fn load_history(&mut self) {
        if !history_needs_load(
            self.signed_in(),
            self.history_loading,
            self.history_error.is_some(),
            self.history_loaded_epoch,
            self.history_epoch,
        ) {
            return;
        }
        self.history_loading = true;
        self.backend.fetch_history(self.history_epoch);
    }

    /// Make a new playlist on your account and show it in the rail.
    pub(crate) fn create_playlist(&mut self, title: String) {
        if !self.signed_in() {
            self.toast_error("Sign in to make playlists");
            return;
        }
        self.backend.create_playlist(title);
    }

    /// A local playlist commits immediately — there is no server to wait on, so this doesn't
    /// need sign-in and doesn't go through `Backend`/`Event` the way `create_playlist` does.
    pub(crate) fn create_local_playlist(
        &mut self,
        title: String,
        track_paths: Vec<std::path::PathBuf>,
    ) {
        self.local_playlists
            .push(crate::local_playlists::LocalPlaylist {
                id: crate::local_playlists::new_id(),
                title: title.clone(),
                track_paths,
            });
        if let Err(e) = crate::local_playlists::save(&self.local_playlists) {
            tracing::warn!(error = %format!("{e:#}"), "could not save local playlists");
        }
        self.resync_local_playlists();
        self.toast(&format!("Created “{title}”"));
    }

    pub(crate) fn is_local_playlist(&self, id: &str) -> bool {
        self.local_playlists.iter().any(|p| p.id == id)
    }

    /// Appends one file to a local playlist and saves immediately — the same "no server, so no
    /// need for an `Event` round trip" reasoning as `create_local_playlist`. Silently does
    /// nothing if `id` doesn't name a local playlist (defensive; `row_menu`'s own filter should
    /// already prevent this from ever being called that way).
    fn add_to_local_playlist(&mut self, id: &str, path: std::path::PathBuf) {
        let Some(playlist) = self.local_playlists.iter_mut().find(|p| p.id == id) else {
            return;
        };
        if playlist.track_paths.contains(&path) {
            self.toast_error("Already in that playlist");
            return;
        }
        playlist.track_paths.push(path);
        if let Err(e) = crate::local_playlists::save(&self.local_playlists) {
            tracing::warn!(error = %format!("{e:#}"), "could not save local playlists");
        }
        self.resync_local_playlists();
        self.toast("Saved to playlist");
    }

    /// Open the rename editor for the currently loaded playlist.
    pub(crate) fn open_playlist_editor(&mut self) {
        let Some(pl) = self.playlist.as_ref() else {
            return;
        };
        self.playlist_edit = Some(PlaylistEditor {
            id: pl.id.clone(),
            title: pl.title.clone(),
            description: pl.description.clone().unwrap_or_default(),
            original_title: pl.title.clone(),
        });
    }

    /// Edit a playlist from the rail: edit it here if it is the open page, else navigate and let
    /// the page open the editor once it arrives.
    pub(crate) fn begin_edit_playlist(&mut self, id: String) {
        if self.playlist.as_ref().is_some_and(|pl| pl.id == id) {
            self.open_playlist_editor();
        } else {
            self.edit_on_load = Some(id.clone());
            self.navigate(Route::Playlist(id));
        }
    }

    /// Save the editor's title and description. Optimistic; a refusal reverts the title.
    pub(crate) fn save_playlist_editor(&mut self) {
        let Some(editor) = self.playlist_edit.take() else {
            return;
        };
        let title = editor.title.trim().to_owned();
        if title.is_empty() {
            self.toast_error("A playlist needs a name");
            self.playlist_edit = Some(editor);
            return;
        }
        self.set_playlist_title(&editor.id, title.clone());
        if let Some(pl) = self.playlist.as_mut()
            && pl.id == editor.id
        {
            pl.description = Some(editor.description.clone());
        }
        if self.is_local_playlist(&editor.id) {
            if let Some(lp) = self.local_playlists.iter_mut().find(|p| p.id == editor.id) {
                lp.title = title;
            }
            if let Err(e) = crate::local_playlists::save(&self.local_playlists) {
                tracing::warn!(error = %format!("{e:#}"), "could not save local playlists");
            }
            self.resync_local_playlists();
            self.toast("Playlist updated");
            return;
        }
        self.edit_revert = Some((editor.id.clone(), editor.original_title));
        self.backend
            .update_playlist(editor.id, title, editor.description);
    }

    /// Ask to delete a playlist: raises the confirm modal.
    pub(crate) fn request_delete_playlist(&mut self, id: String) {
        let title = self.playlist_title(&id);
        self.playlist_delete = Some((id, title));
    }

    /// Delete the playlist the modal is confirming.
    pub(crate) fn confirm_delete_playlist(&mut self) {
        let Some((id, _)) = self.playlist_delete.take() else {
            return;
        };
        if self.is_local_playlist(&id) {
            self.local_playlists.retain(|p| p.id != id);
            if let Err(e) = crate::local_playlists::save(&self.local_playlists) {
                tracing::warn!(error = %format!("{e:#}"), "could not save local playlists");
            }
            self.drop_playlist(&id);
            self.resync_local_playlists();
            if self.current_route() == Route::Playlist(id.clone()) {
                self.navigate(Route::Library);
            }
            self.toast("Playlist deleted");
            return;
        }
        self.backend.delete_playlist(id);
    }

    /// A playlist's title from whichever list holds it, for the delete prompt.
    fn playlist_title(&self, id: &str) -> String {
        if let Some(pl) = &self.playlist
            && pl.id == id
        {
            return pl.title.clone();
        }
        self.library
            .playlists
            .items
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.title.clone())
            .unwrap_or_else(|| "this playlist".to_owned())
    }

    /// Paint a playlist's new title on the open page and in the rail.
    fn set_playlist_title(&mut self, id: &str, title: String) {
        if let Some(pl) = self.playlist.as_mut()
            && pl.id == id
        {
            pl.title = title.clone();
        }
        if let Some(p) = self.library.playlists.items.iter_mut().find(|p| p.id == id) {
            p.title = title;
        }
    }

    /// Drop a playlist from the rail and the saved order (deleted or unsaved).
    fn drop_playlist(&mut self, id: &str) {
        self.library.playlists.items.retain(|p| p.id != id);
        self.settings.playlist_order.retain(|pid| pid != id);
        if let Err(e) = crate::settings::save(&self.settings) {
            tracing::warn!(error = %format!("{e:#}"), "could not save the playlist order");
        }
    }

    /// Keep a fetched playlist for the rail's context menu, bounded so it cannot grow forever.
    fn cache_rail_playlist(&mut self, id: &str, playlist: &crate::model::Playlist) {
        const RAIL_CACHE: usize = 8;
        if self.rail_cache.len() >= RAIL_CACHE && !self.rail_cache.contains_key(id) {
            // Drop an arbitrary other entry; the cache is a convenience, not a source of truth.
            if let Some(key) = self.rail_cache.keys().next().cloned() {
                self.rail_cache.remove(&key);
            }
        }
        self.rail_cache.insert(id.to_owned(), playlist.clone());
        self.run_pending_page_action(id);
    }

    /// Keep a fetched album for a release card's context menu, bounded like the rail cache.
    fn cache_album(&mut self, id: &str, album: &crate::model::Album) {
        const ALBUM_CACHE: usize = 8;
        if self.album_cache.len() >= ALBUM_CACHE && !self.album_cache.contains_key(id) {
            // Drop an arbitrary other entry; the cache is a convenience, not a source of truth.
            if let Some(key) = self.album_cache.keys().next().cloned() {
                self.album_cache.remove(&key);
            }
        }
        self.album_cache.insert(id.to_owned(), album.clone());
        self.run_pending_page_action(id);
    }

    /// A menu pick that was waiting on this page's songs, now that they are cached.
    fn run_pending_page_action(&mut self, id: &str) {
        if self.pending_page_action.as_ref().map(|(p, _)| p.as_str()) != Some(id) {
            return;
        }
        if let Some((id, then)) = self.pending_page_action.take() {
            self.page_then(&id, then);
        }
    }

    /// The pick's page could not be loaded: say so, rather than let the click vanish.
    fn page_action_failed(&mut self, id: &str, kind: &str) {
        if self.pending_page_action.as_ref().map(|(p, _)| p.as_str()) == Some(id) {
            self.pending_page_action = None;
            self.toast_error(&format!("Couldn't load that {kind}"));
        }
    }

    /// The songs and mix seed a menu pick acts on, if the page is loaded or cached.
    fn page_tracks(&self, id: &str) -> Option<(Vec<Track>, Option<String>)> {
        if let Some(album) = self.album_cache.get(id) {
            return Some((album.tracks.clone(), album.like_id.clone()));
        }
        let playlist = self
            .rail_cache
            .get(id)
            .or_else(|| self.playlist.as_ref().filter(|pl| pl.id == id))?;
        Some((playlist.tracks.items.clone(), Some(playlist.id.clone())))
    }

    /// Do a page menu pick with the page's songs — at once when they are here, else once they
    /// land.
    fn page_then(&mut self, id: &str, then: crate::ui::PageThen) {
        use crate::ui::{PageThen, RowAction};
        let Some((tracks, radio)) = self.page_tracks(id) else {
            return;
        };
        if tracks.is_empty() && radio.is_none() {
            self.toast_error("Nothing to play here");
            return;
        }
        let action = match then {
            PageThen::StartMix => match (radio, tracks.first()) {
                (Some(radio), _) => RowAction::StartMixPlaylist(radio),
                (None, Some(first)) => RowAction::StartMix(first.video_id.clone()),
                (None, None) => return,
            },
            _ if tracks.is_empty() => {
                self.toast_error("Nothing to play here");
                return;
            }
            PageThen::Shuffle => RowAction::PageShuffle(tracks),
            PageThen::PlayNext => RowAction::PagePlayNext(tracks),
            PageThen::Queue => RowAction::PageQueue(tracks),
            PageThen::DownloadAll => RowAction::DownloadMany(tracks),
            PageThen::SaveToPlaylist { playlist_id, title } => RowAction::PageSaveToPlaylist {
                playlist_id,
                title,
                video_ids: tracks.into_iter().map(|t| t.video_id).collect(),
            },
        };
        self.row_action(action);
    }

    /// Show a confirmation bubble above the player bar for a few seconds.
    fn toast(&mut self, text: &str) {
        self.toast = Some(Toast {
            text: text.to_owned(),
            until: Instant::now() + TOAST_DURATION,
            error: false,
        });
    }

    /// Say that something did not happen, and why.
    ///
    /// Every write can be refused — a stale token, a playlist that is not yours, no session —
    /// and each of those used to be a log line the user never saw, while every success spoke up.
    fn toast_error(&mut self, text: &str) {
        self.toast = Some(Toast {
            text: text.to_owned(),
            // Longer than a confirmation: an error is the one toast worth reading to the end,
            // and it often says why now.
            until: Instant::now() + ERROR_TOAST_DURATION,
            error: true,
        });
    }

    /// A bulk queue toast with a singular that matches the single-song wording: "Song will
    /// play next", "2 songs will play next".
    fn songs_will(n: usize, what: &str) -> String {
        if n == 1 {
            format!("Song {what}")
        } else {
            format!("{n} songs {what}")
        }
    }

    /// Every shelf the app is holding, across each feed that is made of them.
    ///
    /// Home, Explore, New releases, Charts and History are all lists of the same shelves of the
    /// same songs, so the fan-outs below have to reach all of them — a page whose copy is
    /// skipped shows a rating that snaps back the moment you look at it. One place to add the
    /// next feed to, rather than five.
    fn feed_shelves(&self) -> impl Iterator<Item = &crate::model::Shelf> {
        self.home
            .iter()
            .flat_map(|v| v.iter())
            .chain(self.explore.iter().flat_map(|v| v.iter()))
            .chain(self.releases.iter().flat_map(|v| v.iter()))
            .chain(self.history_shelves.iter().flat_map(|v| v.iter()))
            .chain(self.charts.iter().flat_map(|c| c.shelves.iter()))
            .chain(self.artist.iter().flat_map(|a| a.shelves.iter()))
    }

    /// The mutable twin of [`Self::feed_shelves`], and it must list the same feeds.
    ///
    /// `Arc::make_mut` is what makes a write safe while a page is drawing from a snapshot it
    /// took this frame: the copy happens here, the frame keeps painting what it had, and the
    /// next frame reads the updated one.
    fn for_each_feed_shelf_mut(&mut self, mut f: impl FnMut(&mut crate::model::Shelf)) {
        for feed in [
            &mut self.home,
            &mut self.explore,
            &mut self.releases,
            &mut self.history_shelves,
        ] {
            if let Some(shelves) = feed.as_mut() {
                for shelf in Arc::make_mut(shelves) {
                    f(shelf);
                }
            }
        }
        if let Some(charts) = self.charts.as_mut() {
            for shelf in &mut Arc::make_mut(charts).shelves {
                f(shelf);
            }
        }
        if let Some(artist) = self.artist.as_mut() {
            for shelf in Arc::make_mut(&mut artist.shelves) {
                f(shelf);
            }
        }
    }

    /// Visit every `Track` the app holds — what is playing, the queue, and each loaded page.
    /// The read-only twin of the fan-out in [`Self::set_like`], and it must stay in step with
    /// it: a list one of them knows about and the other does not is a copy that goes stale.
    fn for_each_track(&self, mut f: impl FnMut(&Track)) {
        if let Some(t) = self.now.track.as_ref() {
            f(t);
        }
        for t in self.q.tracks() {
            f(t);
        }
        for t in self
            .results
            .items
            .iter()
            .filter_map(crate::model::Entry::track)
        {
            f(t);
        }
        if let Some(artist) = self.artist.as_ref() {
            for t in &artist.top_songs {
                f(t);
            }
        }
        if let Some(album) = self.album.as_ref() {
            for t in &album.tracks {
                f(t);
            }
        }
        if let Some(playlist) = self.playlist.as_ref() {
            for t in &playlist.tracks.items {
                f(t);
            }
        }
        for t in &self.library.songs.items {
            f(t);
        }
        for t in self
            .feed_shelves()
            .flat_map(|s| s.items.iter())
            .filter_map(crate::model::Entry::track)
        {
            f(t);
        }
        // The chart cells are tracks too, and rating one from there is an ordinary rating.
        for t in self
            .charts
            .iter()
            .flat_map(|c| c.ranked.iter())
            .flat_map(|s| s.rows.iter())
            .filter_map(|r| r.entry.track())
        {
            f(t);
        }
    }

    /// Fill a track's rating and library state in from any copy that knows better.
    ///
    /// A card carries no menu, so a song started from a shelf arrives unrated and with no
    /// bookmark tokens — and the player echoes that very copy back as [`Event::Playing`],
    /// which is what the bar reads. Any list that parsed the same song from a *row* does know,
    /// so ask them before believing an empty state — and failing every loaded copy, the liked
    /// index knows every song the account has liked, which is the one list that outlives the
    /// page it was read on.
    fn reconcile_state(&self, track: &mut Track) {
        let mut like = None;
        let mut library = None;
        self.for_each_track(|t| {
            if t.video_id != track.video_id {
                return;
            }
            if like.is_none() && t.like != LikeStatus::Indifferent {
                like = Some(t.like);
            }
            if library.is_none() && (t.library_add.is_some() || t.library_remove.is_some()) {
                library = Some((
                    t.in_library,
                    t.library_add.clone(),
                    t.library_remove.clone(),
                ));
            }
        });
        if like.is_none() {
            like = self.liked.like_of(&track.video_id);
        }
        if track.like == LikeStatus::Indifferent
            && let Some(like) = like
        {
            track.like = like;
        }
        if track.library_add.is_none()
            && track.library_remove.is_none()
            && let Some((in_library, add, remove)) = library
        {
            track.in_library = in_library;
            track.library_add = add;
            track.library_remove = remove;
        }
    }

    /// A track's rating, from the first copy that holds it. Everything is kept in sync by
    /// [`Self::set_like`], so any copy answers for all of them.
    fn like_of(&self, video_id: &str) -> LikeStatus {
        self.q
            .tracks()
            .iter()
            .find(|t| t.video_id == video_id)
            .map(|t| t.like)
            .or_else(|| {
                self.now
                    .track
                    .as_ref()
                    .filter(|t| t.video_id == video_id)
                    .map(|t| t.like)
            })
            .unwrap_or(LikeStatus::Indifferent)
    }

    /// Set a track's library membership everywhere it appears. Same lists as
    /// [`Self::set_like`]: the toggle arrives optimistically and reverts on failure.
    fn set_library_state(&mut self, video_id: &str, in_library: bool) {
        let set = |t: &mut Track| {
            if t.video_id == video_id {
                t.in_library = in_library;
            }
        };
        if let Some(t) = self.now.track.as_mut() {
            set(t);
        }
        for t in self.q.tracks_mut() {
            set(t);
        }
        for t in self
            .results
            .items
            .iter_mut()
            .filter_map(crate::model::Entry::track_mut)
        {
            set(t);
        }
        if let Some(artist) = self.artist.as_mut() {
            for t in &mut artist.top_songs {
                set(t);
            }
        }
        if let Some(album) = self.album.as_mut() {
            for t in &mut album.tracks {
                set(t);
            }
        }
        if let Some(playlist) = self.playlist.as_mut() {
            for t in &mut playlist.tracks.items {
                set(t);
            }
        }
        for t in &mut self.library.songs.items {
            set(t);
        }
        self.for_each_feed_shelf_mut(|shelf| {
            for t in shelf
                .items
                .iter_mut()
                .filter_map(crate::model::Entry::track_mut)
            {
                set(t);
            }
        });
        if let Some(charts) = self.charts.as_mut() {
            for row in Arc::make_mut(charts)
                .ranked
                .iter_mut()
                .flat_map(|s| s.rows.iter_mut())
            {
                if let Some(t) = row.entry.track_mut() {
                    set(t);
                }
            }
        }
    }

    /// Drop a track from the open playlist page, when it is the playlist it was removed from.
    /// A refused removal cannot reconstruct the row — position, neighbours, continuation —
    /// so that case reloads the page instead of guessing.
    fn remove_playlist_row(&mut self, playlist_id: &str, video_id: &str) {
        if let Some(playlist) = self.playlist.as_mut()
            && playlist.id == playlist_id
        {
            playlist.tracks.items.retain(|t| t.video_id != video_id);
        }
    }

    /// A row was dragged onto another one in the open playlist's own track list: move it to sit
    /// immediately before whatever was at `to`, both locally and on the server.
    ///
    /// Optimistic, like every other playlist edit here: the row moves at once, rather than
    /// waiting on the round trip, and a refusal reloads the whole page instead of trying to
    /// compute its way back (`Event::PlaylistMoveFailed`). `from`/`to` index the list as it was
    /// the moment the drag was released — this only ever runs the same frame that produced
    /// them, so nothing else has had a chance to change it first.
    pub(crate) fn reorder_playlist_track(&mut self, from: usize, to: usize) {
        let Some(playlist) = self.playlist.as_mut() else {
            return;
        };
        let is_local = playlist.source == crate::model::PlaylistSource::Local;
        let Some((set_video_id, before_set_video_id)) =
            reorder_tracks(&mut playlist.tracks.items, from, to)
        else {
            return;
        };
        if is_local {
            // No `set_video_id`/`before_set_video_id` to send anywhere — a local playlist's
            // order is just this `Vec`'s order, so the reorder `reorder_tracks` already did to
            // `playlist.tracks.items` is rebuilt straight back into `track_paths`.
            let playlist_id = playlist.id.clone();
            let track_paths: Vec<std::path::PathBuf> = playlist
                .tracks
                .items
                .iter()
                .filter_map(|t| t.local_path.as_deref().map(std::path::Path::to_path_buf))
                .collect();
            if let Some(lp) = self
                .local_playlists
                .iter_mut()
                .find(|p| p.id == playlist_id)
            {
                lp.track_paths = track_paths;
            }
            if let Err(e) = crate::local_playlists::save(&self.local_playlists) {
                tracing::warn!(error = %format!("{e:#}"), "could not save local playlists");
            }
            self.resync_local_playlists();
            return;
        }
        self.backend.move_playlist_item(
            playlist.id.clone(),
            set_video_id.to_string(),
            before_set_video_id.to_string(),
        );
    }

    /// Set a track's rating everywhere it appears: the queue, the now-playing copy, and every
    /// loaded list. Ratings arrive optimistically and revert on failure, so every copy must
    /// move together or the row and the player bar disagree.
    ///
    /// The liked index moves with them: an artist page must count a song the moment it is liked,
    /// not at the next scan.
    fn set_like(&mut self, video_id: &str, like: LikeStatus) {
        let mut copy: Option<Track> = None;
        let mut set = |t: &mut Track| {
            if t.video_id == video_id {
                t.like = like;
                if copy.is_none() {
                    copy = Some(t.clone());
                }
            }
        };
        if let Some(t) = self.now.track.as_mut() {
            set(t);
        }
        for t in self.q.tracks_mut() {
            set(t);
        }
        for t in self
            .results
            .items
            .iter_mut()
            .filter_map(crate::model::Entry::track_mut)
        {
            set(t);
        }
        if let Some(artist) = self.artist.as_mut() {
            for t in &mut artist.top_songs {
                set(t);
            }
        }
        if let Some(album) = self.album.as_mut() {
            for t in &mut album.tracks {
                set(t);
            }
        }
        if let Some(playlist) = self.playlist.as_mut() {
            for t in &mut playlist.tracks.items {
                set(t);
            }
        }
        for t in &mut self.library.songs.items {
            set(t);
        }
        self.for_each_feed_shelf_mut(|shelf| {
            for t in shelf
                .items
                .iter_mut()
                .filter_map(crate::model::Entry::track_mut)
            {
                set(t);
            }
        });
        if let Some(charts) = self.charts.as_mut() {
            for row in Arc::make_mut(charts)
                .ranked
                .iter_mut()
                .flat_map(|s| s.rows.iter_mut())
            {
                if let Some(t) = row.entry.track_mut() {
                    set(t);
                }
            }
        }
        match like {
            LikeStatus::Like => {
                if let Some(track) = copy {
                    self.liked.set_liked(track);
                    self.liked_dirty = true;
                }
            }
            // Dislike is not liked: the index is the liked set and nothing else.
            LikeStatus::Indifferent | LikeStatus::Dislike => {
                self.liked.set_unliked(video_id);
                self.liked_dirty = true;
            }
        }
    }
}

/// A test aid for debug builds: `REFRYT_CLICK="x,y,button@secs;…"` clicks at those window
/// points once each time comes, `button` being `primary`, `secondary` or `hover` (the pointer
/// moves there and stays, no click) — how a screenshot
/// run opens a right-click menu, which nothing outside the app can do on Wayland. A `button`
/// of `scroll:<dy>` scrolls the wheel by that many points there instead (negative is down),
/// and `drag:<x2>:<y2>` presses at the point, moves to the second one and releases there,
/// spread over a few frames the way a real drag is.
#[cfg(debug_assertions)]
fn scripted_input(raw_input: &mut egui::RawInput) {
    use std::sync::Mutex;
    /// What one scripted input does.
    #[derive(Clone, Copy)]
    enum Act {
        Click,
        Scroll(f32),
        Press,
        Move,
        Release,
    }
    /// An input still to come: where, which button, what, and when (seconds since start).
    type Step = (f32, f32, egui::PointerButton, Act, f64);
    static STEPS: Mutex<Option<Vec<Step>>> = Mutex::new(None);
    let Ok(mut steps) = STEPS.lock() else {
        return;
    };
    let steps = steps.get_or_insert_with(|| {
        let mut out: Vec<Step> = Vec::new();
        for spec in std::env::var("REFRYT_CLICK").unwrap_or_default().split(';') {
            let Some((point, secs)) = spec.split_once('@') else {
                continue;
            };
            let mut parts = point.split(',');
            let (Some(x), Some(y), Some(at)) = (
                parts.next().and_then(|v| v.trim().parse::<f32>().ok()),
                parts.next().and_then(|v| v.trim().parse::<f32>().ok()),
                secs.trim().parse::<f64>().ok(),
            ) else {
                continue;
            };
            let kind = parts.next().map(str::trim);
            let primary = egui::PointerButton::Primary;
            if let Some(to) = kind.and_then(|k| k.strip_prefix("drag:")) {
                if let Some((x2, y2)) = to
                    .split_once(':')
                    .and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))
                {
                    out.push((x, y, primary, Act::Press, at));
                    out.push((x2, y2, primary, Act::Move, at + 0.25));
                    out.push((x2, y2, primary, Act::Release, at + 0.5));
                }
                continue;
            }
            // Only move the pointer there and leave it: how a screenshot gets a tooltip.
            if kind == Some("hover") {
                out.push((x, y, primary, Act::Move, at));
                continue;
            }
            let button = match kind {
                Some("secondary") => egui::PointerButton::Secondary,
                _ => primary,
            };
            let act = kind
                .and_then(|k| k.strip_prefix("scroll:"))
                .and_then(|dy| dy.parse().ok())
                .map_or(Act::Click, Act::Scroll);
            out.push((x, y, button, act, at));
        }
        out
    });
    let now = crate::elapsed_since_start().as_secs_f64();
    let Some(i) = steps.iter().position(|c| c.4 <= now) else {
        return;
    };
    let (x, y, button, act, _) = steps.remove(i);
    let pos = egui::pos2(x, y);
    raw_input.events.push(egui::Event::PointerMoved(pos));
    let press = |pressed| egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    match act {
        Act::Scroll(dy) => raw_input.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        }),
        Act::Click => {
            raw_input.events.push(press(true));
            raw_input.events.push(press(false));
        }
        Act::Press => raw_input.events.push(press(true)),
        Act::Move => {}
        Act::Release => raw_input.events.push(press(false)),
    }
}

impl eframe::App for App {
    /// eframe calls this periodically and on exit, so the queue survives a normal close as well
    /// as the five-second periodic save.
    ///
    /// Written synchronously and directly (not through `Backend::save_playback`, which the
    /// periodic path uses to keep this same write off the render thread): this call must have
    /// actually reached disk before eframe finishes tearing the process down, and a background
    /// task handed to the backend's runtime has no such guarantee.
    /// On exit, and on eframe's own 30-second autosave: written here, synchronously, so the exit
    /// write finishes before the process does — but only when the file is stale, so the autosave
    /// of an unchanged session writes nothing.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if !self.playback_stale() {
            return;
        }
        let Some(store) = self.playback_store() else {
            return;
        };
        match crate::playback::save(&store) {
            Ok(()) => self.playback_saved = Some(self.saved_playback(store.position)),
            Err(e) => {
                tracing::debug!(error = %format!("{e:#}"), "could not save the playback state");
            }
        }
    }

    /// Input before egui sees it. Two jobs: the button release a window move or resize swallowed
    /// ([`crate::chrome::take_pointer_handoff`]), and, in debug builds, scripted input
    /// ([`scripted_input`]).
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // The title bar and the window edges hand their press to the compositor, which moves or
        // resizes the window and keeps the release for itself: egui never hears the button come
        // back up. It then takes the press for a drag still in progress, and while a drag lasts
        // no other widget lights under the pointer and no scroll area takes the wheel — until a
        // click somewhere sends a release of its own. So the release is supplied here, the
        // frame after the hand-off, ahead of anything else that frame brings.
        if crate::chrome::take_pointer_handoff() {
            let pos = ctx
                .input(|i| i.pointer.latest_pos())
                .unwrap_or(egui::Pos2::ZERO);
            raw_input.events.insert(
                0,
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            );
        }
        #[cfg(debug_assertions)]
        scripted_input(raw_input);
    }

    /// Transparent so the rounded corners show the desktop through them.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::TRANSPARENT.to_normalized_gamma_f32()
    }

    /// Everything the app does that is not drawing: backend events, the tray and media keys,
    /// and the transport (the end of a song, a stalled load, autoplay). eframe calls this before
    /// every [`Self::ui`], and on its own while the window is hidden, where it runs no `ui` at
    /// all. Windows and macOS count a minimized window as hidden: with this work in `ui`, a
    /// minimized window played its song out and stopped there, and answered no media key.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events(ctx);
        self.probe_while_offline(ctx);
        self.poll_tray(ctx);
        #[cfg(target_os = "macos")]
        self.poll_menu_bar(ctx);
        self.poll_media(ctx);
        // A second plain launch asked the running instance to show itself (see `instance`).
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        if crate::instance::poll() {
            tracing::info!("second launch: showing the window");
            self.show_window(ctx);
        }
        self.transport_step(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.stats.begin_frame();
        self.handle_keys(&ctx);
        if ctx.input(|i| i.key_pressed(egui::Key::F2)) {
            self.stats.enabled = !self.stats.enabled;
        }
        self.reload_custom_themes(&ctx);
        // Login started this run: it is on screen now, so this is the moment to ask. Wayland
        // maps an autostarted window behind whatever already has focus and refuses to raise a
        // window that did not ask; asking on the first frame is also the best chance of getting
        // a real activation, before the first user input invalidates the token. Skipped when the
        // window already has focus — a request then would only flash the taskbar.
        if self.autostart_attention {
            self.autostart_attention = false;
            if !ctx.input(|i| i.viewport().focused.unwrap_or(false)) {
                tracing::info!("autostart: asking the compositor to bring the window forward");
                request_foreground(&ctx);
            }
        }
        // Publish the playback click model for the row and card widgets this frame, so no page
        // has to thread the setting through.
        crate::widgets::set_double_click_play(&ctx, self.settings.double_click_to_play);
        self.publish_loading(&ctx);
        crate::widgets::set_downloaded(&ctx, std::sync::Arc::clone(&self.downloaded_ids));
        crate::widgets::set_premium(&ctx, self.can_download());

        self.leave_restored_fullscreen(&ctx);
        // A fullscreen window is as square as a maximised one: nothing of the shell's own edge
        // shows, and there is nothing to resize.
        let fullscreen = crate::window_fullscreen::is(&ctx);
        let maximized = Self::is_maximized(&ctx) || fullscreen || self.fs.open;
        // Windows and macOS never paint their own corners: the system rounds the window and
        // clips the shadow (and the blur) to that shape, so a second, app-drawn radius on top
        // would only disagree with it. On Windows that is DWM's corner preference, asked for in
        // `blur.rs` — squared off again by DWM itself when maximized. On macOS the window is a
        // native one with a hidden title bar (see `main`), rounded windowed and zoomed alike.
        let rounded = !self.decorated
            && !maximized
            && !cfg!(target_os = "windows")
            && !cfg!(target_os = "macos");
        let p = self.palette;
        self.place_traffic_lights(fullscreen || self.fs.open);

        chrome::paint_background(ui, &p, rounded);

        // Ask KWin to blur what the transparency shows through. A no-op call every frame once
        // the size and state settle — `Blur::set` compares against what it last pushed and only
        // touches the compositor when something actually changed. `content_rect` — the same
        // rect `chrome::paint_background` just filled — is already in the window's own
        // logical-pixel space, which is what a Wayland surface's local coordinates are.
        let size = ctx.content_rect().size();
        // Windows tints its acrylic light unless the window says it is dark; a no-op elsewhere.
        self.blur.set_dark(p.dark);
        self.blur.set(
            self.settings.blur && p.translucent(),
            (
                size.x.round().max(0.0) as u32,
                size.y.round().max(0.0) as u32,
            ),
            if rounded {
                theme::WINDOW_RADIUS as u32
            } else {
                0
            },
        );

        // Safety net: a route whose fetch was dropped while we were elsewhere must load rather
        // than render an empty page. Idempotent, so calling it every frame costs a comparison.
        //
        // Before the splash check, deliberately: the landing page's fetch must be in flight
        // underneath the splash for the half of it that waits on the first feed to have anything
        // to wait for.
        self.ensure_route_loaded();

        // A stored session being checked is not the signed-out shell: rendering one — a chip
        // wired to Settings, "No playlists yet.", a "Sign in" prompt — is wrong for the moment it
        // is on screen, and with a saved cookie that moment happens on every launch. Neither is
        // the half-built signed-in shell of the frame after it settles, while the landing feed is
        // still in flight. The splash covers both; a fresh profile never enters `Checking`, so the
        // fast path is untouched. The background and blur are painted above, so the window looks
        // the same underneath it.
        if self.splash_visible() {
            ui::splash::screen(self, ui, maximized);
            // `begin_frame` above must be closed even on a frame that draws none of the shell, or
            // this frame's start time is silently discarded by the next one's.
            self.stats.end_frame();
            return;
        }

        // The fullscreen view is closed from outside it too: the window leaving fullscreen
        // (F11, the compositor) or the song going away.
        if self.fs.open {
            if self.fs.manages_window {
                if Instant::now() < self.fs.grace_until {
                    // Still waiting for the window to take fullscreen.
                    ctx.request_repaint_after(Duration::from_millis(200));
                } else if !crate::window_fullscreen::is(&ctx) {
                    tracing::debug!("fullscreen view: the window left fullscreen");
                    self.fs.open = false;
                    self.fs.queue = false;
                }
            }
            if self.now.track.is_none() {
                self.fs_exit(&ctx);
            }
        }

        if self.fs.open {
            ui::fullscreen::view(self, ui);
        } else {
            let route = self.current_route();

            // Claim order is the shell. The player bar goes first so it spans the whole window;
            // the sidebar second so it runs from the top edge down to the player; the top bar
            // third so it covers everything to the right of the rail and its window buttons stay
            // in the window's corner; the queue after that; the content takes what is left.
            let card = |fill: Color32, margin: egui::Margin| {
                egui::Frame::NONE
                    .fill(fill)
                    .corner_radius(egui::CornerRadius::same(theme::radii().panel))
                    // A hairline gives a card an edge to tell it from the gutter beside it, which
                    // a see-through one otherwise lacks. A setting, on by default. Only a
                    // see-through card's frame draws it: a solid one's is drawn after its page,
                    // once (`widgets::CardEdge`).
                    .stroke(match p.card_edge() {
                        Some(c) if p.translucent() => egui::Stroke::new(1.0, c),
                        _ => egui::Stroke::NONE,
                    })
                    .inner_margin(margin)
            };
            let g = theme::GUTTER as i8;

            egui::Panel::bottom("player")
                .exact_size(theme::PLAYER_BAR_HEIGHT)
                .resizable(false)
                // No separator line: the gap is the edge, and egui draws one by default.
                .show_separator_line(false)
                // No card: the player bar sits directly on the gutter, as the reference does —
                // and transparent rather than window-coloured, because the gutter is already
                // painted underneath. Filling it again would stack two alphas and leave the bar
                // more opaque than the gutter it is supposed to sit on.
                .frame(
                    egui::Frame::NONE
                        .fill(Color32::TRANSPARENT)
                        .inner_margin(egui::Margin::symmetric(g + 6, 0)),
                )
                .show(ui, |ui| ui::player_bar::player_bar(self, ui));

            // Resizable by its inner edge, within limits: narrower cuts playlist names to nothing,
            // wider takes the page's room for no gain. egui keeps the dragged width in its own
            // persisted memory, so it survives a restart.
            // egui marks a resizable edge with a full-height line in the hover/active text colour
            // while the pointer is on it — a bright seam through the gutter. The resize cursor is
            // the cue here; the line is switched off for the panels' edges and the panels' own
            // contents get the ordinary style back.
            self.seed_panel_widths(&ctx);
            // How narrow the window is allowed to be, and how much of it the two panels may take:
            // they give way before the page card drops under its floor (`theme::panel_limits`).
            // Too narrow for the queue/lyrics panel, it is left open but not drawn, so it comes
            // back by itself when the window widens again.
            self.fit_window_to_shell(&ctx);
            let screen_w = ctx.input(|i| i.raw.screen_rect.map_or(0.0, |r| r.width()));
            let right_open =
                (self.upnext_open || self.lyrics_open) && theme::right_panel_fits(screen_w);
            let sidebar_pref = self.settings.sidebar_width.unwrap_or(theme::SIDEBAR_WIDTH);
            let right_cap = (screen_w * 0.40).clamp(theme::UPNEXT_MIN, theme::UPNEXT_MAX);
            let (sidebar_max, right_max) = theme::panel_limits(
                screen_w,
                sidebar_pref.clamp(theme::SIDEBAR_RAIL, theme::SIDEBAR_MAX),
                right_open,
                right_cap,
            );
            self.restore_panel_width(
                &ctx,
                PanelEdge::Sidebar,
                sidebar_pref,
                theme::SIDEBAR_RAIL..=sidebar_max,
            );
            let widgets = ui.visuals().widgets.clone();
            quiet_panel_edges(ui);
            // The corner above the rail, where the traffic lights sit when the sidebar is folded,
            // belongs to no panel; it drags the window like the rest of the title bar.
            if self.lights_over_rail() {
                let corner = egui::Rect::from_min_max(
                    egui::Pos2::ZERO,
                    egui::pos2(theme::SIDEBAR_RAIL, theme::TITLEBAR_HEIGHT),
                );
                chrome::drag_surface(ui, corner, self.decorated, "rail-corner");
            }
            egui::Panel::left("sidebar")
                .default_size(sidebar_pref.min(sidebar_max))
                .size_range(theme::SIDEBAR_RAIL..=sidebar_max)
                .resizable(true)
                .show_separator_line(false)
                .frame(
                    card(p.card_fill(), theme::CARD_INSET).outer_margin(egui::Margin {
                        left: g,
                        right: 0,
                        top: if self.lights_over_rail() {
                            theme::TITLEBAR_HEIGHT as i8
                        } else {
                            g
                        },
                        bottom: 0,
                    }),
                )
                .show(ui, |ui| {
                    ui.visuals_mut().widgets = widgets.clone();
                    ui::sidebar::sidebar(self, ui)
                });
            // After the panel is laid out, not inside it: egui stores a panel's width when the
            // panel has been shown, so from inside, the release of a drag was seen a pass early
            // and the width read was the one from before it. The correct width then only reached
            // Settings on a later pass, and an app that went idle or was closed in between kept
            // the old one.
            self.remember_panel_width(&ctx, PanelEdge::Sidebar);
            ui.visuals_mut().widgets = widgets.clone();

            egui::Panel::top("topbar")
                .exact_size(theme::TITLEBAR_HEIGHT)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(Color32::TRANSPARENT)
                        .inner_margin(egui::Margin::symmetric(g + 6, 0)),
                )
                .show(ui, |ui| ui::topbar::top_bar(self, ui));

            if right_open {
                // The queue is auxiliary, so it yields before the page does: a flat 300 is 40%
                // of a 750pt window, and the card behind it became too narrow to read — every name in the
                // artist and playlist headers ellipsising while a third of the window sat in an
                // empty queue. Down to a floor it keeps enough rail to be a queue.
                //
                // Dragged by its inner edge, it keeps to a range: never under what the lyrics need
                // to set a line without breaking every other word, and never more than 40% of the
                // window, so a wide panel dragged on a big screen still yields on a small one.
                let screen_w = ui
                    .ctx()
                    .input(|i| i.raw.screen_rect.map_or(0.0, |r| r.width()));
                let queue_w = self
                    .settings
                    .panel_width
                    .unwrap_or(theme::UPNEXT_WIDTH.min(screen_w * 0.30))
                    .max(theme::UPNEXT_MIN);
                let queue_max = right_max;
                self.restore_panel_width(
                    &ctx,
                    PanelEdge::Queue,
                    queue_w,
                    theme::UPNEXT_MIN..=queue_max,
                );
                let show_queue = self.upnext_open;
                let show_lyrics = self.lyrics_open;
                // The card insets: a small side padding, and none top or bottom — content runs to
                // the card's own top and bottom edges.
                let margins = theme::CARD_INSET;
                quiet_panel_edges(ui);
                if show_queue && show_lyrics {
                    // Two *cards* sharing the column, not one card split by a hairline: the right
                    // panel itself is transparent, and each half is rounded with the shell's gutter
                    // between them, exactly like the sidebar and the page card.
                    egui::Panel::right("right-panel")
                        .default_size(queue_w)
                        .size_range(theme::UPNEXT_MIN..=queue_max)
                        .resizable(true)
                        .show_separator_line(false)
                        .frame(egui::Frame::NONE.outer_margin(egui::Margin {
                            left: 0,
                            right: g,
                            top: 0,
                            bottom: 0,
                        }))
                        .show(ui, |ui| {
                            ui.visuals_mut().widgets = widgets.clone();
                            let half = ((ui.available_height() - f32::from(g)) / 2.0).max(80.0);
                            egui::Panel::top("queue-card")
                                .exact_size(half + f32::from(g))
                                .resizable(false)
                                .show_separator_line(false)
                                .frame(card(p.card_fill(), margins).outer_margin(egui::Margin {
                                    left: 0,
                                    right: 0,
                                    top: 0,
                                    bottom: g,
                                }))
                                .show(ui, |ui| ui::upnext::panel(self, ui));
                            egui::Panel::top("lyrics-card")
                                .exact_size(half)
                                .resizable(false)
                                .show_separator_line(false)
                                .frame(card(p.card_fill(), margins))
                                .show(ui, |ui| ui::lyrics::panel(self, ui));
                        });
                } else if show_queue || show_lyrics {
                    egui::Panel::right("right-panel")
                        .default_size(queue_w)
                        .size_range(theme::UPNEXT_MIN..=queue_max)
                        .resizable(true)
                        .show_separator_line(false)
                        .frame(card(p.card_fill(), margins).outer_margin(egui::Margin {
                            left: 0,
                            right: g,
                            top: 0,
                            bottom: 0,
                        }))
                        .show(ui, |ui| {
                            ui.visuals_mut().widgets = widgets.clone();
                            if show_queue {
                                ui::upnext::panel(self, ui);
                            } else {
                                ui::lyrics::panel(self, ui);
                            }
                        });
                }
                ui.visuals_mut().widgets = widgets;
                self.remember_panel_width(&ctx, PanelEdge::Queue);
            }

            // The video view takes the whole card: no inner margin, so its frame meets the card's
            // own rounded edge instead of sitting inset behind a black border.
            let content = if self.video_open {
                egui::Frame::NONE
                    .fill(Color32::BLACK)
                    .corner_radius(egui::CornerRadius::same(theme::radii().panel))
            } else {
                card(p.card_fill(), theme::PAGE_INSET)
            };
            egui::CentralPanel::default()
                .frame(content.outer_margin(egui::Margin {
                    left: g,
                    right: g,
                    top: 0,
                    bottom: 0,
                }))
                .show(ui, |ui| {
                    // Captured first: `available_rect_before_wrap` only means "the top" before
                    // the page allocates. It is the card's *content* rect — the panel less its
                    // inner margin — which is the only honest source for where the card's own
                    // edges are. A scroll area's clip rect is not it: egui builds that from the
                    // viewport and the parent's clip, and it lands a few points out on every side,
                    // which is how the header wash ended up short at the top and over the edge on
                    // the right.
                    let top = ui.available_rect_before_wrap();
                    self.content_rect = top;
                    // The video view takes the content card; the page it was opened from is still
                    // behind it, one toggle away.
                    let card = widgets::card_rect(top, theme::PAGE_INSET);
                    let _edge = widgets::CardEdge::new(ui, card, &p);
                    if self.video_open {
                        ui::video::panel(self, ui);
                        return;
                    }
                    widgets::clip_card_content(ui, card, theme::PAGE_INSET_X, &p);
                    // Last frame's answer: the page draws after the banner, and a page that is itself
                    // the offline screen says it in full, so the strip would only repeat it.
                    let offline_page = std::mem::take(&mut self.offline_page_shown);
                    // The page lays out inside the card and no taller: a loader is not in a scroll
                    // area, and a frame as tall as its content grew past the card, taking its
                    // rounded bottom corners out of the clip.
                    widgets::contain(ui, top, |ui| {
                        if self.offline && !offline_page {
                            ui::offline_banner(self, ui);
                        }
                        match route {
                            Route::Search => ui::search::page(self, ui),
                            Route::Explore => ui::explore::page(self, ui),
                            Route::NewReleases => ui::releases::page(self, ui),
                            Route::Charts(_) => ui::charts::page(self, ui),
                            Route::Moods => ui::moods::page(self, ui),
                            Route::HomeShelf(title) => ui::home::shelf_page(self, ui, &title),
                            Route::History => ui::history::page(self, ui),
                            Route::Home => ui::home::page(self, ui),
                            Route::Library => ui::library::page(self, ui),
                            Route::Settings => ui::settings::page(self, ui),
                            Route::Artist(_) => ui::artist::page(self, ui),
                            Route::ArtistLiked(_) => ui::artist_liked::page(self, ui),
                            Route::Album(_) => ui::album::page(self, ui),
                            Route::Playlist(_) => ui::playlist::page(self, ui),
                            Route::Mood(_) => ui::mood::page(self, ui),
                        }
                    });
                    // The card's rounded corners, painted after the page: content runs to the card's
                    // own top and bottom edges, and the part of it outside the rounded silhouette is
                    // covered with the gutter. The video view returns above — its frame is already
                    // rounded — so this never touches it.
                    widgets::paint_corner_notches(ui, card, theme::PAGE_INSET_X, &p);
                });
        }

        // The playlist management modals, over everything and on every route.
        ui::playlist::modals(self, ui);
        ui::help::modal(self, ui);
        ui::home_order::modal(self, ui);
        ui::paste_cookies::modal(self, ui);
        ui::crash::modal(self, ui);
        ui::update::modal(self, ui);

        if !self.decorated && !maximized {
            chrome::resize_handles(ui);
        }

        self.sync_lyrics();

        // After the tray, the title bar's buttons and the rest have had their say about this
        // frame, and before its swap: a window that is on its way out must not wait for the
        // screen (`pacing`).
        if is_wayland() {
            let (focused, pointer_inside) = ctx.input(|i| {
                (
                    i.viewport().focused.unwrap_or(true),
                    i.pointer.has_pointer(),
                )
            });
            crate::pacing::apply(focused, pointer_inside, self.hidden);
        }

        // Repaint discipline. Exactly one of these, and only when something is actually moving:
        // a pending autocomplete debounce, or a progress bar that needs to advance. Search itself
        // is submit-driven and fires here once, on demand.
        let suggest_due = self.poll_suggestions(&ctx);
        self.poll_search();
        // The playing song's own wake-ups are asked for in `transport_step`, hidden or not.
        if let Some(due) = suggest_due {
            ctx.request_repaint_after(due);
        }

        // `scroll_to_top` is NOT cleared here: it is consumed by `page_scroll` itself, so a
        // reset requested while the page still shows its loader survives until the content
        // draws. Clearing it here reopened every fresh fetch at the previous offset.

        // Toasts, centred above the player bar. Only a live one asks for another frame, so an
        // idle app with no toast paints nothing extra.
        let remaining = self
            .toast
            .as_ref()
            .map(|t| t.until.saturating_duration_since(Instant::now()));
        match remaining {
            Some(left) if !left.is_zero() => {
                let (text, error) = self
                    .toast
                    .as_ref()
                    .map(|t| (t.text.clone(), t.error))
                    .unwrap_or_default();
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(
                        egui::Align2::CENTER_BOTTOM,
                        egui::vec2(0.0, -(theme::PLAYER_BAR_HEIGHT + 16.0)),
                    )
                    .order(egui::Order::Foreground)
                    .show(&ctx, |ui| {
                        egui::Frame::NONE
                            .fill(self.palette.surface)
                            .stroke(egui::Stroke::new(1.0, self.palette.outline))
                            .corner_radius(egui::CornerRadius::same(theme::radii().popup))
                            .inner_margin(egui::Margin::symmetric(14, 8))
                            .show(ui, |ui| {
                                // One line, always: a toast is a short confirmation, and the
                                // anchored area's width is not something to wrap a two-word
                                // phrase against. `Extend` sizes the bubble to the text.
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(text).font(theme::medium(13.0)).color(
                                            if error {
                                                self.palette.danger
                                            } else {
                                                self.palette.text
                                            },
                                        ),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            });
                    });
                ctx.request_repaint_after(left);
            }
            _ => {
                self.toast = None;
            }
        }

        // Take only the link commands, leaving clipboard ones for eframe to handle.
        let mut urls = Vec::new();
        ctx.output_mut(|o| {
            o.commands.retain(|c| match c {
                egui::OutputCommand::OpenUrl(open) => {
                    urls.push(open.url.clone());
                    false
                }
                _ => true,
            });
        });
        for url in urls {
            crate::open_url(&url);
        }

        // Advance the debug scroll, and keep painting so it can actually move.
        if let Some(offset) = &mut self.autoscroll {
            *offset = (*offset + 40.0).min(self.autoscroll_max);
            ctx.request_repaint();
        }

        self.stats.end_frame();
        self.stats.overlay(ui, &p, &self.images);

        // A loaded-state measurement: give the app time to fetch and draw real content, then
        // report the counters and quit. This is what `scripts/perf.sh` reads.
        if let Some(after) = self.stats_after
            && crate::elapsed_since_start() >= after
        {
            self.stats_after = None;
            self.stats.snapshot(&ctx, &self.images).print();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.stats_after.is_some() {
            // Keep painting until the deadline, or the app would idle and never get there.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.take_screenshot(&ctx);

        if self.startup_timing {
            // egui runs one more pass after the close command, so latch it.
            self.startup_timing = false;
            println!(
                "first_frame_ms={:.1}",
                crate::elapsed_since_start().as_secs_f64() * 1000.0
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    /// A window restored fullscreen is taken out of it and then resized — in that order, and the
    /// resize only once the window reports it is out. The fullscreen player's own fullscreen is
    /// never touched, nor is a window that was never fullscreen, and a fullscreen that arrives
    /// well after start (the listener's own) is left alone.
    #[test]
    fn a_restored_fullscreen_is_left_but_the_players_is_not() {
        use FullscreenAction::{Leave, Nothing, Resize, Wait};
        use FullscreenCheck::{Done, Leaving, Looking};
        let t = |ms| Duration::from_millis(ms);
        // Restored fullscreen: leave, wait while it still is, resize when it is not, then done.
        assert_eq!(
            fullscreen_step(Looking, false, true, t(300)),
            (Leaving, Leave)
        );
        assert_eq!(
            fullscreen_step(Leaving, false, true, t(400)),
            (Leaving, Wait)
        );
        assert_eq!(
            fullscreen_step(Leaving, false, false, t(700)),
            (Done, Resize)
        );
        assert_eq!(fullscreen_step(Done, false, false, t(900)), (Done, Nothing));
        // The player has the window: never touched, however fullscreen it is.
        assert_eq!(
            fullscreen_step(Looking, true, true, t(300)),
            (Done, Nothing)
        );
        // Never fullscreen: watched for three seconds, then done.
        assert_eq!(
            fullscreen_step(Looking, false, false, t(2900)),
            (Looking, Nothing)
        );
        assert_eq!(
            fullscreen_step(Looking, false, false, t(3100)),
            (Done, Nothing)
        );
        // Fullscreen only after the first seconds is the listener's doing, and stays.
        assert_eq!(fullscreen_step(Done, false, true, t(9000)), (Done, Nothing));
    }

    fn tick(position: f64) -> PlayingTick {
        PlayingTick {
            position,
            duration: 200.0,
            seen_playing: true,
            next_lyric: None,
            hidden: false,
        }
    }

    fn near(a: Duration, secs: f64) -> bool {
        (a.as_secs_f64() - secs).abs() < 0.001
    }

    /// While a song plays the next frame lands just past the next whole second, so the clock and
    /// the bar change together, and never later than a second away.
    #[test]
    fn a_playing_frame_is_due_on_the_next_second() {
        assert!(near(playing_repaint_after(tick(10.3)), 0.72));
        assert!(near(playing_repaint_after(tick(10.0)), 1.02f64.min(1.0)));
        // Just before a boundary the wait never drops below 50 ms.
        assert!(near(playing_repaint_after(tick(10.99)), 0.05));
    }

    /// The quick tick covers what a slow one would break: an attempt not yet seen playing (the
    /// stall guard tells "played briefly" from "never started" by what a frame saw), the last
    /// seconds of a track (the next song starts as promptly as before), and a length not known.
    #[test]
    fn the_quick_tick_covers_the_start_the_end_and_the_unknown() {
        let mut t = tick(1.2);
        t.seen_playing = false;
        assert_eq!(playing_repaint_after(t), PLAYING_FAST_REPAINT);
        assert_eq!(playing_repaint_after(tick(197.5)), PLAYING_FAST_REPAINT);
        assert_eq!(playing_repaint_after(tick(200.0)), PLAYING_FAST_REPAINT);
        let mut unknown = tick(50.0);
        unknown.duration = 0.0;
        assert_eq!(playing_repaint_after(unknown), PLAYING_FAST_REPAINT);
        // Hidden does not excuse the end: a track must still advance.
        let mut hidden = tick(198.0);
        hidden.hidden = true;
        assert_eq!(playing_repaint_after(hidden), PLAYING_FAST_REPAINT);
    }

    /// A lyric line that starts before the next second pulls the frame forward to it, one later
    /// than that changes nothing, and a hidden window ignores lyrics and waits two seconds.
    #[test]
    fn a_lyric_line_pulls_the_frame_forward_and_hidden_relaxes_it() {
        let mut t = tick(10.3);
        t.next_lyric = Some(10.5);
        assert!(near(playing_repaint_after(t), 0.22));
        t.next_lyric = Some(14.0);
        assert!(near(playing_repaint_after(t), 0.72));
        t.hidden = true;
        t.next_lyric = Some(10.5);
        assert!(near(playing_repaint_after(t), 2.0));
    }

    /// Played out frame by frame, a 200-second song takes about 200 frames — not the 800 a quick
    /// tick took — and no gap in the last three seconds is longer than the quick tick.
    #[test]
    fn a_whole_song_takes_a_frame_a_second_and_wakes_for_its_end() {
        let (mut position, mut frames, mut worst_gap_at_end) = (0.0f64, 0u32, 0.0f64);
        let mut seen = false;
        while position < 200.0 {
            let wait = playing_repaint_after(PlayingTick {
                position,
                duration: 200.0,
                seen_playing: seen,
                next_lyric: None,
                hidden: false,
            })
            .as_secs_f64();
            if position >= 197.0 {
                worst_gap_at_end = worst_gap_at_end.max(wait);
            }
            position += wait;
            frames += 1;
            seen = seen || position > 0.5;
        }
        assert!(frames < 260, "{frames} frames for 200 s");
        assert!(frames > 190, "{frames} frames: the bar would stall");
        assert!(
            worst_gap_at_end <= 0.2501,
            "gap {worst_gap_at_end} near the end"
        );
    }

    use super::*;

    #[test]
    fn a_channel_page_is_a_profile_when_marked_or_your_own() {
        let marked: std::collections::HashSet<String> = ["UCowner".to_owned()].into();
        let none = std::collections::HashSet::new();
        assert!(is_profile(Some("UCowner"), &marked, None));
        assert!(is_profile(Some("UCme"), &none, Some("UCme")));
        assert!(!is_profile(Some("UCartist"), &marked, Some("UCme")));
        assert!(!is_profile(None, &marked, Some("UCme")));
    }

    fn artist(id: &str) -> Route {
        Route::Artist(id.to_owned())
    }

    /// A feed is due a refresh once it is `HOME_STALE` old, and never before it has loaded.
    #[test]
    fn a_count_line_moves_and_keeps_its_wording() {
        assert_eq!(bump_count("52 tracks", 1), "53 tracks");
        assert_eq!(bump_count("53 tracks", -1), "52 tracks");
        assert_eq!(bump_count("4,782 songs", 1), "4,783 songs");
        assert_eq!(bump_count("1,000 songs", -1), "999 songs");
        assert_eq!(bump_count("0 songs", -1), "0 songs");
        assert_eq!(bump_count("No tracks", -1), "No tracks");
        assert_eq!(bump_count("Playlist", 1), "Playlist");
    }

    /// English nouns follow the number, and an empty playlist counts up from "No …".
    #[test]
    fn a_count_line_keeps_singular_and_plural_right() {
        assert_eq!(bump_count("1 song", 1), "2 songs");
        assert_eq!(bump_count("2 songs", -1), "1 song");
        assert_eq!(bump_count("1 track", -1), "0 tracks");
        assert_eq!(bump_count("No tracks", 1), "1 track");
        assert_eq!(bump_count("No songs", 3), "3 songs");
        assert_eq!(
            bump_count("12 canciones", 1),
            "13 canciones",
            "another language keeps its noun"
        );
    }

    /// An add moves the count on the open page and on the playlist's row in the library list,
    /// which the sidebar shows, matching the bare id either way; other playlists are left alone.
    #[test]
    fn a_count_moves_on_the_page_and_in_the_sidebar() {
        let mut page = crate::model::Playlist {
            id: "VLPLabc".to_owned(),
            title: String::new(),
            description: None,
            author: String::new(),
            author_id: None,
            year: None,
            duration_text: None,
            track_count: Some("4 songs".to_owned()),
            thumbnail: None,
            editable: true,
            in_library: false,
            library_add: None,
            library_remove: None,
            saveable: false,
            tracks: crate::model::Paged::new(Vec::new(), None),
            source: crate::model::PlaylistSource::YouTube,
            extras: None,
        };
        let mut rows = vec![pref("PLabc"), pref("PLother")];
        rows[0].track_count = Some("4 tracks".to_owned());
        rows[1].track_count = Some("9 tracks".to_owned());
        bump_counts_in(Some(&mut page), &mut rows, "PLabc", 1);
        assert_eq!(page.track_count.as_deref(), Some("5 songs"));
        assert_eq!(rows[0].track_count.as_deref(), Some("5 tracks"));
        assert_eq!(rows[1].track_count.as_deref(), Some("9 tracks"));
        // A refused add takes it back.
        bump_counts_in(Some(&mut page), &mut rows, "VLPLabc", -1);
        assert_eq!(page.track_count.as_deref(), Some("4 songs"));
        assert_eq!(rows[0].track_count.as_deref(), Some("4 tracks"));
        // With another page open, the sidebar row still moves.
        page.id = "PLother".to_owned();
        bump_counts_in(Some(&mut page), &mut rows, "PLabc", 2);
        assert_eq!(rows[0].track_count.as_deref(), Some("6 tracks"));
        assert_eq!(page.track_count.as_deref(), Some("4 songs"));
    }

    #[test]
    fn home_goes_stale_after_half_an_hour() {
        let now = Instant::now() + HOME_STALE * 2;
        assert!(!home_is_stale(None, now));
        assert!(!home_is_stale(Some(now - Duration::from_secs(60)), now));
        assert!(!home_is_stale(
            Some(now - (HOME_STALE - Duration::from_secs(1))),
            now
        ));
        assert!(home_is_stale(Some(now - HOME_STALE), now));
        assert!(home_is_stale(Some(now - HOME_STALE * 2), now));
    }

    /// Background work and the song's own load never show page skeletons; every page load does.
    #[test]
    fn only_page_loads_show_skeletons() {
        assert!(!Load::Radio.shows_skeleton());
        assert!(!Load::SidebarPlaylists.shows_skeleton());
        assert!(!Load::Song.shows_skeleton());
        for page in [
            Load::Search,
            Load::Home,
            Load::Library,
            Load::Album,
            Load::Playlist,
            Load::Artist,
        ] {
            assert!(page.shows_skeleton(), "{page:?}");
        }
    }

    fn saved(position: f64, paused: bool, ago: Duration) -> SavedPlayback {
        SavedPlayback {
            fingerprint: 7,
            position,
            paused,
            at: Instant::now() - ago,
        }
    }

    /// Nothing changed: a paused session is never rewritten, and a playing one only every
    /// `POSITION_SAVE_EVERY` — not every few seconds as before.
    #[test]
    fn an_unchanged_session_is_not_rewritten() {
        let paused = saved(42.0, true, Duration::from_secs(3600));
        assert!(!playback_save_due(Some(&paused), 7, 42.0, true));

        let playing = saved(42.0, false, Duration::from_secs(10));
        assert!(!playback_save_due(Some(&playing), 7, 52.0, false));
        let later = saved(42.0, false, POSITION_SAVE_EVERY);
        assert!(playback_save_due(
            Some(&later),
            7,
            42.0 + POSITION_SAVE_EVERY.as_secs_f64(),
            false
        ));
    }

    /// What the file records changing is saved straight away: the first write, the queue or
    /// transport, pausing or resuming, and a seek.
    #[test]
    fn a_real_change_is_saved_at_once() {
        let playing = saved(42.0, false, Duration::from_secs(2));
        assert!(playback_save_due(None, 7, 0.0, true), "never written");
        assert!(
            playback_save_due(Some(&playing), 8, 44.0, false),
            "queue or transport"
        );
        assert!(playback_save_due(Some(&playing), 7, 44.0, true), "paused");
        assert!(
            playback_save_due(Some(&playing), 7, 120.0, false),
            "seeked forward"
        );
        assert!(
            playback_save_due(Some(&playing), 7, 5.0, false),
            "seeked back"
        );
    }

    /// A track that got past its start is one that played, so its end is an end.
    #[test]
    fn a_track_that_played_ends_normally() {
        let mut guard = StallGuard::default();
        guard.on_load("a", None);
        guard.observe(0.5);
        assert_eq!(guard.on_idle(Instant::now()), IdleEnd::Ended);
        assert_eq!(guard.retries, 0);
        assert!(guard.pending_at().is_none());
    }

    /// The bug this exists for: idle with nothing ever played retries instead of skipping.
    #[test]
    fn an_attempt_that_never_played_is_retried_to_the_cap_then_gives_up() {
        let mut guard = StallGuard::default();
        guard.on_load("a", None);
        let t0 = Instant::now();
        guard.observe(0.0);
        assert_eq!(
            guard.on_idle(t0),
            IdleEnd::Retry(Duration::from_millis(500))
        );
        assert_eq!(guard.on_idle(t0), IdleEnd::Retry(Duration::from_secs(1)));
        assert_eq!(guard.on_idle(t0), IdleEnd::Retry(Duration::from_secs(2)));
        assert_eq!(guard.on_idle(t0), IdleEnd::GiveUp);
        assert_eq!(guard.retries, 0);
    }

    /// mpv reports the `start` seek target as the position; a resume that never advanced is not
    /// playback, or a restored queue on a failed device would be skipped like a finished track.
    #[test]
    fn a_resumed_track_sitting_at_its_start_position_stalls() {
        let mut guard = StallGuard::default();
        guard.on_load("a", Some(210.0));
        guard.observe(210.0);
        assert_eq!(
            guard.on_idle(Instant::now()),
            IdleEnd::Retry(Duration::from_millis(500))
        );
    }

    #[test]
    fn a_resumed_track_that_advanced_ends_normally() {
        let mut guard = StallGuard::default();
        guard.on_load("a", Some(210.0));
        guard.observe(211.0);
        assert_eq!(guard.on_idle(Instant::now()), IdleEnd::Ended);
    }

    /// A retry's own reload must not reset the count, or the cap never arrives; a real track
    /// change must, or the next track inherits the last one's stalls.
    #[test]
    fn the_retry_count_belongs_to_the_track() {
        let mut guard = StallGuard::default();
        guard.on_load("a", None);
        guard.on_idle(Instant::now());
        guard.on_load("a", None);
        assert_eq!(
            guard.on_idle(Instant::now()),
            IdleEnd::Retry(Duration::from_secs(1))
        );
        guard.on_load("b", None);
        assert_eq!(
            guard.on_idle(Instant::now()),
            IdleEnd::Retry(Duration::from_millis(500))
        );
    }

    #[test]
    fn a_retry_is_taken_only_once_its_backoff_has_passed() {
        let mut guard = StallGuard::default();
        guard.on_load("a", None);
        let t0 = Instant::now();
        guard.on_idle(t0);
        assert!(!guard.take_due(t0));
        assert!(guard.take_due(t0 + Duration::from_millis(500)));
        assert!(!guard.take_due(t0 + Duration::from_millis(500)));
        assert!(guard.pending_at().is_none());
    }

    /// Start Mix on the song already playing is adopted, not restarted; anything else starts
    /// from the seed as before.
    #[test]
    fn a_mix_on_the_playing_song_is_adopted_not_restarted() {
        let tracks = vec![track("a"), track("b")];
        assert_eq!(mix_seed_index(true, Some("a"), "a", &tracks), Some(0));
        assert_eq!(mix_seed_index(true, Some("b"), "b", &tracks), Some(1));
        // Another song's mix starts from its seed.
        assert_eq!(mix_seed_index(true, Some("a"), "b", &tracks), None);
        // Nothing loaded yet: the normal start.
        assert_eq!(mix_seed_index(false, Some("a"), "a", &tracks), None);
        // A mix that does not carry the seed cannot be adopted under it.
        assert_eq!(mix_seed_index(true, Some("a"), "c", &tracks), None);
    }

    #[test]
    fn back_and_forward_walk_the_pages_in_order() {
        let mut h = History::new(Route::Search);
        assert!(h.push(artist("a")));
        assert!(h.push(Route::Album("x".into())));

        assert!(h.back());
        assert_eq!(*h.current(), artist("a"));
        assert!(h.back());
        assert_eq!(*h.current(), Route::Search);
        assert!(!h.can_go_back());
        assert!(!h.back());

        assert!(h.forward());
        assert_eq!(*h.current(), artist("a"));
    }

    /// The shape of the Back bug: two pages of the same kind must stay distinguishable, so the
    /// caller is told to reload rather than keep whatever is on screen.
    #[test]
    fn back_between_two_artists_lands_on_the_first_one() {
        let mut h = History::new(artist("a"));
        h.push(artist("b"));
        assert!(h.back());
        assert_eq!(*h.current(), artist("a"));
    }

    #[test]
    fn navigating_drops_the_forward_history() {
        let mut h = History::new(Route::Search);
        h.push(artist("a"));
        h.push(artist("b"));
        h.back();
        assert!(h.can_go_forward());

        h.push(Route::Settings);
        assert!(!h.can_go_forward(), "artist b should have been dropped");
        assert!(h.back());
        assert_eq!(*h.current(), artist("a"));
    }

    #[test]
    fn re_entering_the_same_page_is_not_a_new_entry() {
        let mut h = History::new(Route::Search);
        assert!(h.push(artist("a")));
        assert!(!h.push(artist("a")), "same route should be refused");
        assert!(h.back());
        assert_eq!(*h.current(), Route::Search);
    }

    fn track(id: &str) -> Track {
        Track {
            video_id: id.to_owned(),
            title: id.to_owned(),
            artists: vec![crate::model::ArtistLink::plain("Someone")],
            album: None,
            album_id: None,
            duration: None,
            plays: None,
            explicit: false,
            like: crate::model::LikeStatus::Indifferent,
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            thumbnail: None,
            rank: None,
            trend: crate::model::ChartTrend::Unknown,
            wide: false,
            local_path: None,
        }
    }

    fn queue_ids(ids: &[&str]) -> Vec<Track> {
        ids.iter().map(|s| track(s)).collect()
    }

    /// The bug this fixes: sorting and then choosing Default again left the tracks exactly as
    /// the earlier sort had ordered them, since `PlaylistSort::apply`'s own `Default => {}` arm
    /// did nothing — only closing and reopening the playlist (which re-fetches YouTube's own
    /// order from scratch) actually put it back.
    #[test]
    fn choosing_default_again_restores_the_order_youtube_served() {
        let mut tracks = queue_ids(&["a", "b", "c"]);
        let mut original = None;
        apply_playlist_sort(PlaylistSort::TitleDesc, &mut tracks, &mut original);
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
        assert!(
            original.is_some(),
            "the first non-default sort must capture what order to restore"
        );

        apply_playlist_sort(PlaylistSort::Default, &mut tracks, &mut original);
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"],
            "Default must restore the served order, not leave the tracks as the last sort left them"
        );
    }

    /// Choosing Default when nothing has ever been sorted away from it is a no-op, not a crash
    /// on a snapshot that was never taken.
    #[test]
    fn default_before_any_sort_leaves_the_order_alone() {
        let mut tracks = queue_ids(&["a", "b", "c"]);
        let mut original = None;
        apply_playlist_sort(PlaylistSort::Default, &mut tracks, &mut original);
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    /// `Reverse` flips the order YouTube actually served, not whatever the tracks currently
    /// happen to be in. Served order here is `b, a, c` (deliberately not alphabetical) so
    /// reversing *that* (`c, a, b`) reads differently from reversing the Title-A–Z view
    /// (`c, b, a`) it's applied on top of — a bug that reversed the current order instead would
    /// produce the latter.
    #[test]
    fn reverse_flips_the_served_order_not_the_current_one() {
        let mut tracks = queue_ids(&["b", "a", "c"]);
        let mut original = None;
        apply_playlist_sort(PlaylistSort::TitleAsc, &mut tracks, &mut original);
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );

        apply_playlist_sort(PlaylistSort::Reverse, &mut tracks, &mut original);
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["c", "a", "b"]
        );
    }

    /// `All` matches everything search can return; every other scope matches only its own
    /// `Entry` variant. Nothing here ever changes ranking or order — see `SearchScope`'s own
    /// doc comment for why a scope is purely a filter over the one ranked response.
    #[test]
    fn search_scope_matches_only_its_own_entry_kind() {
        use crate::model::{AlbumRef, ArtistRef, PlaylistRef};

        let song = Entry::Song(track("a"));
        let video = Entry::Video(track("a"));
        let artist = Entry::Artist(ArtistRef {
            id: "a".to_owned(),
            name: "A".to_owned(),
            subscribers: None,
            thumbnail: None,
            actions: crate::model::ArtistActions::default(),
        });
        let album = Entry::Album(AlbumRef {
            id: "a".to_owned(),
            title: "A".to_owned(),
            artist: "A".to_owned(),
            year: None,
            kind: None,
            thumbnail: None,
        });
        let playlist = Entry::Playlist(PlaylistRef {
            id: "a".to_owned(),
            title: "A".to_owned(),
            author: String::new(),
            author_id: None,
            track_count: None,
            thumbnail: None,
            editable: false,
            source: crate::model::PlaylistSource::YouTube,
        });
        let all = [&song, &video, &artist, &album, &playlist];

        assert!(all.iter().all(|e| SearchScope::All.matches(e)));
        assert_eq!(
            all.iter().filter(|e| SearchScope::Songs.matches(e)).count(),
            1
        );
        assert!(SearchScope::Songs.matches(&song));
        assert!(SearchScope::Videos.matches(&video));
        assert!(SearchScope::Artists.matches(&artist));
        assert!(SearchScope::Albums.matches(&album));
        assert!(SearchScope::Playlists.matches(&playlist));
        assert!(!SearchScope::Songs.matches(&video));
        assert!(!SearchScope::Playlists.matches(&album));
    }

    /// A track with a playlist item id, unlike plain `track()` — what `reorder_tracks` needs to
    /// have anything to send the server.
    fn playlist_track(id: &str) -> Track {
        Track {
            set_video_id: Some(id.into()),
            ..track(id)
        }
    }

    #[test]
    fn reorder_moves_the_dragged_track_before_the_target() {
        let mut items = vec![
            playlist_track("a"),
            playlist_track("b"),
            playlist_track("c"),
            playlist_track("d"),
        ];
        // Drag "a" to land before "c".
        let moved = reorder_tracks(&mut items, 0, 2);
        assert_eq!(
            items
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["b", "a", "c", "d"]
        );
        let (set_video_id, before_set_video_id) = moved.expect("a real move");
        assert_eq!(&*set_video_id, "a");
        assert_eq!(&*before_set_video_id, "c");
    }

    #[test]
    fn reorder_upward_also_lands_before_the_target() {
        let mut items = vec![
            playlist_track("a"),
            playlist_track("b"),
            playlist_track("c"),
            playlist_track("d"),
        ];
        // Drag "d" to land before "b".
        let moved = reorder_tracks(&mut items, 3, 1);
        assert_eq!(
            items
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "d", "b", "c"]
        );
        let (set_video_id, before_set_video_id) = moved.expect("a real move");
        assert_eq!(&*set_video_id, "d");
        assert_eq!(&*before_set_video_id, "b");
    }

    #[test]
    fn reorder_onto_itself_or_out_of_range_does_nothing() {
        let mut items = vec![playlist_track("a"), playlist_track("b")];
        assert!(reorder_tracks(&mut items, 0, 0).is_none());
        assert!(reorder_tracks(&mut items, 0, 5).is_none());
        assert!(reorder_tracks(&mut items, 5, 0).is_none());
        assert_eq!(
            items
                .iter()
                .map(|t| t.video_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"],
            "a no-op drag must not touch the list"
        );
    }

    #[test]
    fn reorder_refuses_a_track_with_no_playlist_item_id() {
        // `track()` alone (not `playlist_track()`) leaves `set_video_id: None` — a row that
        // is not really a playlist item, which the server call has nothing to identify it by.
        let mut items = vec![track("a"), playlist_track("b")];
        assert!(reorder_tracks(&mut items, 0, 1).is_none());
    }

    /// A panel dragged narrower stays where it was let go. The width is only saved a frame or two
    /// after the release, so on the first frames the preference is still the old, wider one, and
    /// restoring it put the sidebar straight back.
    #[test]
    fn a_panel_just_dragged_narrower_is_not_restored() {
        assert_eq!(
            restored_width(200.0, 280.0, true),
            None,
            "settling after the release"
        );
        assert_eq!(
            restored_width(200.0, 200.0, false),
            None,
            "saved: the preference is the new width"
        );
    }

    /// What the restore is for: a window that was too narrow squeezed the panel, and now has room.
    #[test]
    fn a_panel_the_window_squeezed_comes_back() {
        assert_eq!(restored_width(240.0, 288.0, false), Some(288.0));
        assert_eq!(restored_width(283.9, 288.0, false), Some(288.0));
    }

    /// Within a few points, and wider than wanted, is left alone.
    #[test]
    fn a_panel_close_to_or_over_its_width_is_left_alone() {
        assert_eq!(
            restored_width(284.0, 288.0, false),
            None,
            "exactly the margin"
        );
        assert_eq!(restored_width(288.0, 288.0, false), None);
        assert_eq!(restored_width(340.0, 280.0, false), None, "dragged wider");
    }

    /// The edge names and slots the drag state is kept under are distinct and stable.
    #[test]
    fn each_panel_edge_has_its_own_name_and_slot() {
        assert_eq!(PanelEdge::Sidebar.name(), "sidebar");
        assert_eq!(PanelEdge::Queue.name(), "right-panel");
        assert_ne!(PanelEdge::Sidebar.slot(), PanelEdge::Queue.slot());
        assert!(PanelEdge::Sidebar.slot() < 2 && PanelEdge::Queue.slot() < 2);
    }

    /// The bug this fixes: `ViewportCommand::Visible` is a documented no-op on Wayland (winit's
    /// own Wayland backend says so — "Not possible on Wayland"), so "Hide tray on close" appeared
    /// to do nothing there — the veto fired, `Visible(false)` went out, and nothing
    /// happened. `Minimized` is the real substitute Wayland's xdg-shell offers, but only in the
    /// hide direction (`set_minimized(false)` is *also* a documented no-op there).
    #[test]
    fn hiding_on_wayland_minimizes_instead_of_the_broken_visible_command() {
        assert_eq!(
            hide_command_for(true, true),
            egui::ViewportCommand::Minimized(true)
        );
        assert_eq!(
            hide_command_for(true, false),
            egui::ViewportCommand::Minimized(false),
            "still the honest command to ask for — that winit ignores it on Wayland is a \
             platform fact this function isn't responsible for hiding"
        );
        assert_eq!(
            hide_command_for(false, true),
            egui::ViewportCommand::Visible(false)
        );
        assert_eq!(
            hide_command_for(false, false),
            egui::ViewportCommand::Visible(true)
        );
    }

    fn pref(id: &str) -> crate::model::PlaylistRef {
        crate::model::PlaylistRef {
            id: id.to_owned(),
            title: id.to_owned(),
            author: String::new(),
            author_id: None,
            track_count: None,
            thumbnail: None,
            editable: false,
            source: crate::model::PlaylistSource::YouTube,
        }
    }

    /// The saved rail order sorts known ids and leaves unknown ones in place at the end; an
    /// empty order is a no-op, which is what a fresh install has.
    #[test]
    fn saved_order_sorts_known_ids_and_appends_new_ones() {
        let mut items = vec![pref("a"), pref("b"), pref("c")];
        order_by_saved(&mut items, &["c".into(), "a".into()]);
        assert_eq!(
            items.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["c", "a", "b"]
        );
        order_by_saved(&mut items, &[]);
        assert_eq!(items[0].id, "c", "an empty order changes nothing");
    }

    /// Previous restarts the track once it is underway, and only steps back within the first
    /// few seconds — so pressing it repeatedly walks backwards from the start.
    #[test]
    fn previous_restarts_the_track_once_it_is_underway() {
        assert!(!previous_restarts(0.0));
        assert!(!previous_restarts(1.0), "the first second still steps back");
        assert!(!previous_restarts(1.999));
        assert!(previous_restarts(2.0));
        assert!(previous_restarts(10.0));
    }

    /// Muting, from the speaker icon, a drag to the end or the desktop's controls, keeps the
    /// volume to come back to; only an audible volume replaces it.
    #[test]
    fn silence_mutes_and_keeps_the_volume_to_come_back_to() {
        assert_eq!(volume_after(40.0, 0.0), (40.0, true));
        assert_eq!(
            volume_after(40.0, 0.3),
            (40.0, true),
            "too quiet to hear is a mute"
        );
        assert_eq!(volume_after(40.0, 65.0), (65.0, false));
        let (last, muted) = volume_after(40.0, 0.0);
        assert!(muted);
        assert_eq!(volume_after(last, last), (40.0, false), "the icon's unmute");
    }

    /// A volume key steps from what is heard: up from silence while muted, and its result goes
    /// through the same remember-or-mute rule as the slider.
    #[test]
    fn volume_keys_step_from_what_is_heard() {
        assert_eq!(nudged_volume(false, 40.0, VOLUME_STEP), 50.0);
        assert_eq!(nudged_volume(false, 40.0, -VOLUME_STEP), 30.0);
        assert_eq!(nudged_volume(true, 40.0, VOLUME_STEP), 10.0);
        let (last, muted) = volume_after(10.0, nudged_volume(false, 10.0, -VOLUME_STEP));
        assert_eq!(
            (last, muted),
            (10.0, true),
            "stepping down to silence mutes"
        );
    }

    fn radio(state: River, seed: &str, chosen: &[&str]) -> Radio {
        Radio {
            state,
            seed: seed.to_owned(),
            chosen: chosen.iter().map(|s| (*s).to_owned()).collect(),
            filters: Vec::new(),
            filtering: None,
        }
    }

    /// A restart keeps the radio: which songs were the listener's, the page to continue from,
    /// and its filters with the one picked. A filter still in flight saves the river it replaced.
    #[test]
    fn the_radio_survives_a_restart() {
        let mut r = radio(River::Open("tok".into()), "a", &["a", "gone"]);
        r.filters = vec![crate::innertube::QueueFilter {
            label: "Familiar".into(),
            target: crate::model::WatchTarget {
                video_id: None,
                playlist_id: "RDATx".into(),
                params: Some("p".into()),
            },
            selected: true,
        }];
        let queue = vec![track("a"), track("r1")];
        let back = Radio::from_persisted(r.to_persisted(&queue));
        assert_eq!(back.state, River::Open("tok".into()));
        assert_eq!(back.seed, "a");
        assert_eq!(back.chosen, ["a".to_owned()].into_iter().collect());
        assert_eq!(back.filters, r.filters);

        r.filtering = Some(("RDATy".into(), River::Open("old".into())));
        r.state = River::Dead;
        let back = Radio::from_persisted(r.to_persisted(&queue));
        assert_eq!(back.state, River::Open("old".into()));
        assert!(back.filtering.is_none());

        // Run dry: a new session asks again from the queue's end.
        r.filtering = None;
        let back = Radio::from_persisted(r.to_persisted(&queue));
        assert_eq!(back.state, River::Pending);
    }

    /// A hand-picked queue seeds its tail as soon as playback starts, from its last track —
    /// position in the queue is irrelevant; YouTube Music shows the tail from the beginning.
    #[test]
    fn a_pending_river_seeds_from_the_queue_tail_immediately() {
        let queue = vec![track("a"), track("b"), track("c")];
        let r = radio(River::Pending, "c", &["a", "b", "c"]);
        for current in [0, 1, 2] {
            match river_call(&r, Some(current), queue.len() - current - 1, &queue, true) {
                Some(RiverCall::Seed { seed }) => assert_eq!(seed, "c"),
                _ => panic!("pending river should seed at position {current}"),
            }
        }
        assert!(river_call(&r, None, queue.len(), &queue, true).is_none());
    }

    /// An open river follows its token only once the queue is within the low-water mark; a dead
    /// river and a queue with nothing playing never ask for anything.
    #[test]
    fn an_open_river_follows_only_at_the_low_water_mark() {
        let queue: Vec<Track> = (0..20).map(|i| track(&i.to_string())).collect();
        let open = radio(River::Open("tok".into()), "v", &[]);
        assert!(river_call(&open, Some(5), 14, &queue, true).is_none());
        match river_call(&open, Some(10), 9, &queue, true) {
            Some(RiverCall::Follow { token, reseed }) => {
                assert_eq!(token, "tok");
                assert_eq!(reseed, "10");
            }
            _ => panic!("expected a follow at the low-water mark"),
        }
        let dead = radio(River::Dead, "v", &[]);
        assert!(river_call(&dead, Some(19), 0, &queue, true).is_none());
    }

    /// One gate over every river: the setting stops a pending seed and an open follow alike.
    #[test]
    fn the_autoplay_setting_gates_every_river() {
        let queue = vec![track("a")];
        let pending = radio(River::Pending, "a", &["a"]);
        assert!(river_call(&pending, Some(0), 0, &queue, false).is_none());
        let open = radio(River::Open("tok".into()), "a", &[]);
        assert!(river_call(&open, Some(0), 0, &queue, false).is_none());
    }

    /// A page only keeps the river flowing while it adds something and hands back a token.
    #[test]
    fn a_river_dies_when_a_page_adds_nothing_or_has_no_token() {
        assert_eq!(landed(2, Some("t".into())), River::Open("t".into()));
        assert_eq!(landed(0, Some("t".into())), River::Dead);
        assert_eq!(landed(2, None), River::Dead);
    }

    /// The eager tail changes where "add to queue" lands: after the songs the listener chose,
    /// before the algorithm's.
    #[test]
    fn the_tail_starts_at_the_first_track_the_listener_did_not_choose() {
        let queue = vec![track("a"), track("b"), track("t1"), track("t2")];
        let mut q = PlayQueue::new(1);
        q.restore_session(queue.clone(), Some(0), None, false, None);
        let mut r = radio(River::Open("tok".into()), "b", &["a", "b"]);
        assert_eq!(q.tail_start(Some(&r.chosen)), Some(2));
        r.chosen.insert("t1".into());
        r.chosen.insert("t2".into());
        assert_eq!(q.tail_start(Some(&r.chosen)), None);

        // And the pending constructor records the queue as chosen, so nothing marks it Radio.
        let pending = Radio::pending(&queue).expect("non-empty queue");
        assert_eq!(pending.state, River::Pending);
        assert_eq!(pending.seed, "t2");
        assert_eq!(q.tail_start(Some(&pending.chosen)), None);
    }

    /// History refreshes once per visit, not once per app run: the entry bumps the epoch, the
    /// loader fetches while the epochs differ, and the landing reply closes the gap. In between,
    /// the per-frame safety net must not refetch — and a latched error or a load in flight must
    /// not either, or a failing fetch would be hammered every frame.
    #[test]
    fn history_fetches_once_per_visit() {
        // A fresh visit: nothing loaded for this epoch yet.
        assert!(history_needs_load(true, false, false, 0, 1));
        // The reply landed: nothing to do until the next visit bumps the epoch.
        assert!(!history_needs_load(true, false, false, 1, 1));
        // Already in flight.
        assert!(!history_needs_load(true, true, false, 0, 1));
        // Failed: wait for a visit (or a retry) to clear the latch.
        assert!(!history_needs_load(true, false, true, 0, 1));
        // Signed out: the page is a prompt, not a feed.
        assert!(!history_needs_load(false, false, false, 0, 1));
        // The next visit.
        assert!(history_needs_load(true, false, false, 1, 2));
    }

    /// The video view is only for tracks that have a video: a song without one hands the view
    /// back and plays as audio, and a closed view never opens itself for a wide track.
    #[test]
    fn only_a_wide_track_plays_as_video() {
        assert!(plays_as_video(true, true));
        assert!(!plays_as_video(true, false));
        assert!(!plays_as_video(false, true));
        assert!(!plays_as_video(false, false));
    }

    /// Start Mix on the song playing now: the mix's queue opens with that song, so a guard that
    /// looked only at the loaded track treated it as "already playing this" and toggled playback
    /// instead of replacing the queue. The queue has to match track for track.
    #[test]
    fn a_list_only_toggles_when_it_is_the_queue_on_screen() {
        let playing = vec![track("x")];
        assert!(same_queue(&playing, &[track("x")]));
        // The mix opens with the playing song but is its own queue: replace, do not toggle.
        assert!(!same_queue(&playing, &[track("x"), track("y")]));
        // Same tracks, different order or length: a different queue.
        assert!(!same_queue(
            &[track("x"), track("y")],
            &[track("y"), track("x")]
        ));
        assert!(!same_queue(&[track("x")], &[track("x"), track("y")]));
        assert!(!same_queue(&[], &playing));
    }

    /// A failure toast says why when the error tells, and nothing technical ever reaches it.
    #[test]
    fn a_failure_toast_says_why_when_it_can() {
        assert_eq!(
            failure_text(
                "Could not remove that song",
                "error sending request for url"
            ),
            "Could not remove that song: you're offline"
        );
        assert_eq!(
            failure_text(
                "Could not update your library",
                "HTTP status client error (401 Unauthorized)"
            ),
            "Could not update your library: sign in again"
        );
        assert_eq!(
            failure_text(
                "Could not start a mix",
                "HTTP status client error (429 Too Many Requests)"
            ),
            "Could not start a mix: YouTube is busy, try again in a minute"
        );
        assert_eq!(
            failure_text(
                "Could not start a mix",
                "YouTube returned an empty watch queue"
            ),
            "Could not start a mix",
            "an unknown cause keeps the plain wording"
        );
    }

    /// Only a plain sentence counts as YouTube's own reason; request failures do not.
    #[test]
    fn youtube_s_own_sentence_is_told_from_a_request_failure() {
        assert_eq!(
            youtube_sentence("This playlist can't be edited."),
            Some("This playlist can't be edited.")
        );
        assert_eq!(youtube_sentence(""), None);
        assert_eq!(youtube_sentence("HTTP status server error (500)"), None);
        assert_eq!(youtube_sentence("error decoding response body"), None);
        assert_eq!(youtube_sentence(&"x".repeat(200)), None);
    }
}
