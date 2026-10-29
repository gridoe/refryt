//! The boundary between the UI thread and everything slow.
//!
//! The rule this file exists to enforce: **the UI thread never awaits and never blocks.** Search
//! and stream resolution take seconds; both run on tokio worker threads and report back through a
//! channel that `App` drains once per frame. Background work calls `request_repaint` when it has
//! something to say, which is what lets the app sit at zero CPU when idle and still feel instant.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use libmpv2::Mpv;
use refryt_resolve::{
    ChainResolver, StreamCache, StreamResolver, VideoId, VideoResolver, VideoStreamCache,
    YtDlpResolver,
};
use tracing::{debug, info, warn};

use crate::api::Api;
use crate::auth::{self, AuthState};
use crate::downloads;
use crate::images;
use crate::model::{
    Album, Artist, Entry, Library, MoodPlaylists, Playlist, SearchResults, SearchScope, Shelf,
    Track,
};
use crate::player;

/// Something finished happening off-thread.
#[derive(Debug)]
pub enum Event {
    SearchDone {
        query: String,
        results: SearchResults,
    },
    SearchFailed {
        query: String,
        error: String,
    },
    ScopedSearchDone {
        query: String,
        scope: crate::model::SearchScope,
        results: Vec<Entry>,
    },
    ScopedSearchFailed {
        query: String,
        scope: crate::model::SearchScope,
        error: String,
    },
    /// Autocomplete for the search field. `query` is the staleness guard: the app drops a reply
    /// whose query is no longer in the box.
    SuggestionsDone {
        query: String,
        items: Vec<String>,
    },
    SuggestionsFailed {
        query: String,
        error: String,
    },
    /// A download in progress: `percent` is 0–100.
    DownloadProgress {
        video_id: String,
        percent: f32,
    },
    DownloadDone {
        video_id: String,
        path: std::path::PathBuf,
        track: Box<Track>,
    },
    DownloadFailed {
        video_id: String,
        error: String,
    },
    LocalScanDone {
        tracks: Vec<Track>,
    },
    LocalScanFailed {
        error: String,
    },
    /// The update check's answer: a newer release, or `None` when this build is the latest.
    /// `manual` is a click on "Check now", which wants an answer either way.
    UpdateChecked {
        release: Option<Box<crate::update::Release>>,
        manual: bool,
    },
    UpdateCheckFailed {
        error: String,
        manual: bool,
    },
    /// The update's download, 0–100.
    UpdateProgress {
        percent: f32,
    },
    /// The update is downloaded, checked and staged: close the window and `main` runs it.
    UpdateReady,
    UpdateFailed {
        error: String,
    },
    ArtistLoaded {
        id: String,
        artist: Artist,
    },
    ArtistFailed {
        id: String,
        error: String,
    },
    ArtistAlbumsLoaded {
        /// Whatever browse id the request actually used — the shelf's own, when it had one,
        /// not necessarily the artist's channel id. See `App::expand_artist_shelf`.
        artist_id: String,
        items: Vec<Entry>,
    },
    ArtistAlbumsFailed {
        artist_id: String,
        error: String,
    },
    /// One page of Liked Music, in the playlist's own (recently-saved) order, from the
    /// background scan that builds `App::liked`.
    LikedPage {
        tracks: Vec<crate::model::Track>,
        epoch: u64,
    },
    /// The liked scan ended. `complete` is true only when the playlist was read to its end —
    /// an error midway stops the scan without marking the partial list authoritative.
    LikedScanDone {
        complete: bool,
        epoch: u64,
    },
    AlbumLoaded {
        id: String,
        album: Album,
    },
    AlbumFailed {
        id: String,
        error: String,
    },
    PlaylistLoaded {
        id: String,
        playlist: Playlist,
    },
    PlaylistFailed {
        id: String,
        error: String,
    },
    PlaylistMore {
        id: String,
        page: crate::innertube::Page,
    },
    PlaylistMoreFailed {
        id: String,
        error: String,
    },
    /// Whether the account holds YouTube Premium. `None` when it could not be asked (offline,
    /// an error): the last answer stands. `epoch` is the account it was asked for.
    PremiumChecked {
        premium: Option<bool>,
        epoch: u64,
    },
    /// The answer to [`Backend::probe_connection`]: whether YouTube Music answered at all.
    Connection {
        online: bool,
    },
    /// The Suggestions under one of your playlists. `None` when there were none, or the fetch
    /// failed — either way the page simply shows no Suggestions.
    PlaylistSuggestions {
        id: String,
        suggestions: Option<crate::innertube::Suggestions>,
    },
    /// An album/playlist card's own play button was pressed — its tracks, fetched just to
    /// start playing them, not to open the page. Deliberately its own event rather than
    /// reusing `AlbumLoaded`/`PlaylistLoaded`: those are guarded by `album_is_for`/
    /// `playlist_is_for` so a fetch for a card you never navigated to is silently discarded,
    /// which is exactly wrong here.
    CardTracksReady {
        id: String,
        tracks: Vec<Track>,
    },
    /// `epoch` is the account epoch this fetch was issued under — see `App::account_epoch`'s own
    /// doc comment for why a reply whose epoch has since moved on must be dropped.
    LibraryLoaded {
        library: Library,
        epoch: u64,
    },
    LibraryMore {
        section: crate::api::LibrarySection,
        page: crate::innertube::Page,
    },
    LibraryMoreFailed {
        section: crate::api::LibrarySection,
        error: String,
    },
    /// The sidebar's playlists, fetched on sign-in ahead of the Library page.
    ///
    /// Deliberately not a `LibraryMore` page: that one *appends*, and this can land after a full
    /// library load has already put the same playlists there — which is exactly what happened,
    /// and the rail listed all 31 of them twice.
    SidebarPlaylists {
        page: crate::innertube::Page,
        epoch: u64,
    },
    SidebarPlaylistsFailed {
        error: String,
        epoch: u64,
    },
    LibraryFailed {
        error: String,
        epoch: u64,
    },
    SubscriptionChanged {
        channel_id: String,
        subscribed: bool,
    },
    SubscriptionFailed {
        channel_id: String,
        previous: bool,
        error: String,
    },
    ExploreLoaded {
        shelves: Vec<Shelf>,
        links: Vec<crate::model::ExploreLink>,
    },
    ExploreFailed {
        error: String,
    },
    ReleasesLoaded {
        shelves: Vec<Shelf>,
    },
    ReleasesFailed {
        error: String,
    },
    ChartsLoaded {
        params: Option<String>,
        feed: crate::model::ChartsFeed,
    },
    ChartsFailed {
        params: Option<String>,
        error: String,
    },
    MoodsLoaded {
        sections: Vec<crate::model::MoodSection>,
    },
    MoodsFailed {
        error: String,
    },
    HistoryLoaded {
        shelves: Vec<Shelf>,
        /// The visit this was fetched for; a reply for a superseded visit is stale.
        epoch: u64,
    },
    HistoryFailed {
        error: String,
        epoch: u64,
    },
    PlaylistCreated {
        title: String,
    },
    PlaylistCreateFailed {
        error: String,
    },
    HomeLoaded {
        /// The filter this was fetched with. A reply for a filter since changed is stale.
        filter: Option<String>,
        shelves: Vec<Shelf>,
        chips: Vec<crate::model::HomeChip>,
        /// Where the feed continues; see `HomeFeed::next`.
        next: Option<String>,
        /// See `LibraryLoaded::epoch`'s own doc comment.
        epoch: u64,
    },
    HomeFailed {
        filter: Option<String>,
        error: String,
        epoch: u64,
    },
    /// The next page of the home feed, loaded on scroll.
    HomeMore {
        filter: Option<String>,
        shelves: Vec<Shelf>,
        next: Option<String>,
        epoch: u64,
    },
    HomeMoreFailed {
        filter: Option<String>,
        error: String,
        epoch: u64,
    },
    /// The full list behind a Home shelf's "Show all", from that shelf's own "more" target.
    HomeShelfLoaded {
        title: String,
        items: Vec<crate::model::Entry>,
    },
    HomeShelfFailed {
        title: String,
        error: String,
    },
    MoodPlaylistsLoaded {
        params: String,
        mood: MoodPlaylists,
    },
    MoodPlaylistsFailed {
        params: String,
        error: String,
    },
    /// A radio queue arrived. `seed` is the video id it was opened from — reseeding a dead
    /// continuation starts here (M14.9).
    MixLoaded {
        seed: String,
        queue: crate::innertube::WatchQueue,
    },
    MixFailed {
        seed: String,
        error: String,
    },
    /// More river. `token` is the one that was followed, so the app can drop a reply for a
    /// river that has since been replaced.
    MixMore {
        token: String,
        queue: crate::innertube::WatchQueue,
    },
    MixMoreFailed {
        token: String,
        error: String,
    },
    /// The first page of a hand-picked queue's autoplay tail. `seed` identifies the pending
    /// river it was fetched for — a queue replaced while the request was in flight has its own.
    MixSeeded {
        seed: String,
        queue: crate::innertube::WatchQueue,
    },
    MixSeedFailed {
        seed: String,
        error: String,
    },
    /// One of the radio's filters, its first page. `playlist_id` is the filter's own, which the
    /// app matches against the one it asked for.
    RadioFiltered {
        playlist_id: String,
        queue: crate::innertube::WatchQueue,
    },
    RadioFilterFailed {
        playlist_id: String,
        error: String,
    },
    /// Lyrics for the now-playing track: synced from LRCLIB when it had them, else plain text
    /// from InnerTube.
    LyricsLoaded {
        video_id: String,
        lyrics: crate::model::Lyrics,
    },
    LyricsFailed {
        video_id: String,
        error: String,
    },
    /// A rating write was refused. The app updates optimistically, so this carries what to
    /// revert: the state is never left claiming something YouTube refused.
    RatingFailed {
        video_id: String,
        /// The rating that was attempted. Only revert when nothing newer has landed since —
        /// two overlapping ratings for one track resolve in arrival order otherwise.
        attempted: crate::model::LikeStatus,
        previous: crate::model::LikeStatus,
        error: String,
    },
    /// A library toggle landed. `in_library` is the new state.
    LibraryToggled {
        video_id: String,
        in_library: bool,
    },
    LibraryToggleFailed {
        video_id: String,
        previous: bool,
        error: String,
    },
    /// A page's (album or playlist header) library toggle landed. `album` picks the page to
    /// repaint; `in_library` is the new state.
    PageLibraryToggled {
        album: bool,
        in_library: bool,
    },
    PageLibraryFailed {
        album: bool,
        previous: bool,
        error: String,
    },
    /// A playlist's title/description rename landed, or was refused (the app optimistically
    /// repaints, so the failure carries the id to revert).
    PlaylistEdited {
        id: String,
    },
    PlaylistEditFailed {
        id: String,
        error: String,
    },
    /// A playlist delete landed, or was refused. The rail is repainted optimistically.
    PlaylistDeleted {
        id: String,
    },
    PlaylistDeleteFailed {
        id: String,
        error: String,
    },
    /// A saved playlist was removed from the library (unliked), or the write was refused.
    PlaylistLibraryChanged {
        id: String,
        saved: bool,
    },
    PlaylistLibraryFailed {
        id: String,
        error: String,
    },
    /// A bulk save landed. One verdict for the whole page — per-song toasts would bury the bar.
    PlaylistBulkSaved {
        playlist_id: String,
        added: usize,
        /// Songs left out because the playlist already had them.
        duplicates: usize,
        total: usize,
    },
    /// The song is already in that playlist, so it was not added. `message` is YouTube's own
    /// sentence for it; the app asks whether to add it anyway.
    PlaylistDuplicate {
        playlist_id: String,
        video_id: String,
        message: String,
    },
    /// A playlist write landed. Saving needs no UI change — the song lives elsewhere now —
    /// while a removal drops the row from the open page.
    PlaylistSaved {
        playlist_id: String,
        video_id: String,
    },
    PlaylistSaveFailed {
        playlist_id: String,
        video_id: String,
        error: String,
    },
    PlaylistRemoved {
        playlist_id: String,
        video_id: String,
    },
    PlaylistRemoveFailed {
        playlist_id: String,
        video_id: String,
        error: String,
    },
    /// A drag-to-reorder within the open playlist's own track list was confirmed by the
    /// server. The row already moved locally when the drag was released — this is only the
    /// server catching up.
    PlaylistMoved {
        playlist_id: String,
    },
    PlaylistMoveFailed {
        playlist_id: String,
        error: String,
    },
    AuthChanged(AuthState),
    AccountsLoaded {
        accounts: Vec<crate::model::Account>,
        epoch: u64,
    },
    /// A sign-in *attempt* failed. Deliberately distinct from `AuthChanged`: a refused attempt
    /// must not disturb a session that is already working.
    SignInFailed {
        error: String,
    },
    /// The stored session was checked at launch, YouTube no longer knows it, and it could not be
    /// renewed in the background. Said out loud because the only other sign was the library
    /// quietly emptying.
    SessionExpired,
    /// `generation` is the value `Backend::play`/`play_video` returned when this attempt was
    /// started — see `App::play_generation`'s own doc comment for why every one of these three
    /// carries it.
    Resolving(Track, u64),
    Playing(Track, u64),
    PlaybackFailed {
        track: Track,
        error: String,
        generation: u64,
    },
    /// Cover art finished decoding on a worker; the UI thread uploads it as a texture.
    ImageReady(Box<images::Loaded>),
    /// The URL and the size that was asked for — the cache keys on both.
    ImageFailed(Arc<str>, u32, bool),
}

impl Event {
    /// Whether this event carries what a big response was parsed into: a feed, a library, a page.
    /// The parse tree behind it is already freed, and the memory it used is worth handing back.
    fn follows_big_parse(&self) -> bool {
        matches!(
            self,
            Event::HomeLoaded { .. }
                | Event::HomeMore { .. }
                | Event::HomeShelfLoaded { .. }
                | Event::SidebarPlaylists { .. }
                | Event::LibraryLoaded { .. }
                | Event::LibraryMore { .. }
                | Event::LikedPage { .. }
                | Event::SearchDone { .. }
                | Event::ScopedSearchDone { .. }
                | Event::ArtistLoaded { .. }
                | Event::AlbumLoaded { .. }
                | Event::PlaylistLoaded { .. }
                | Event::ExploreLoaded { .. }
                | Event::ReleasesLoaded { .. }
                | Event::ChartsLoaded { .. }
                | Event::MoodsLoaded { .. }
                | Event::HistoryLoaded { .. }
        )
    }

    /// Whether this event is a page's content (or a song's lyrics) arriving or failing to, which
    /// is what swaps a skeleton out — for the debug-only `REFRYT_HOLD_LOADING`.
    #[cfg(debug_assertions)]
    pub fn is_page_load(&self) -> bool {
        matches!(
            self,
            Event::SearchDone { .. }
                | Event::SearchFailed { .. }
                | Event::ScopedSearchDone { .. }
                | Event::ScopedSearchFailed { .. }
                | Event::ArtistLoaded { .. }
                | Event::ArtistFailed { .. }
                | Event::AlbumLoaded { .. }
                | Event::AlbumFailed { .. }
                | Event::PlaylistLoaded { .. }
                | Event::PlaylistFailed { .. }
                | Event::PlaylistSuggestions { .. }
                | Event::LibraryLoaded { .. }
                | Event::LibraryFailed { .. }
                | Event::ExploreLoaded { .. }
                | Event::ExploreFailed { .. }
                | Event::ReleasesLoaded { .. }
                | Event::ReleasesFailed { .. }
                | Event::ChartsLoaded { .. }
                | Event::ChartsFailed { .. }
                | Event::MoodsLoaded { .. }
                | Event::MoodsFailed { .. }
                | Event::HistoryLoaded { .. }
                | Event::HistoryFailed { .. }
                | Event::HomeLoaded { .. }
                | Event::HomeFailed { .. }
                | Event::HomeShelfLoaded { .. }
                | Event::HomeShelfFailed { .. }
                | Event::MoodPlaylistsLoaded { .. }
                | Event::MoodPlaylistsFailed { .. }
                | Event::LyricsLoaded { .. }
                | Event::LyricsFailed { .. }
        )
    }

    /// The error a failure carries, for the one question asked of all of them at once: does
    /// this read as having no connection? See [`crate::api::looks_offline`]. A local folder scan
    /// failing is left out — the disk has nothing to say about the network.
    pub fn error(&self) -> Option<&str> {
        match self {
            Event::SearchFailed { error, .. }
            | Event::ScopedSearchFailed { error, .. }
            | Event::SuggestionsFailed { error, .. }
            | Event::DownloadFailed { error, .. }
            | Event::ArtistFailed { error, .. }
            | Event::ArtistAlbumsFailed { error, .. }
            | Event::AlbumFailed { error, .. }
            | Event::PlaylistFailed { error, .. }
            | Event::PlaylistMoreFailed { error, .. }
            | Event::LibraryMoreFailed { error, .. }
            | Event::SidebarPlaylistsFailed { error, .. }
            | Event::LibraryFailed { error, .. }
            | Event::SubscriptionFailed { error, .. }
            | Event::ExploreFailed { error, .. }
            | Event::ReleasesFailed { error, .. }
            | Event::ChartsFailed { error, .. }
            | Event::MoodsFailed { error, .. }
            | Event::HistoryFailed { error, .. }
            | Event::PlaylistCreateFailed { error, .. }
            | Event::HomeFailed { error, .. }
            | Event::HomeMoreFailed { error, .. }
            | Event::HomeShelfFailed { error, .. }
            | Event::MoodPlaylistsFailed { error, .. }
            | Event::MixFailed { error, .. }
            | Event::MixMoreFailed { error, .. }
            | Event::MixSeedFailed { error, .. }
            | Event::RadioFilterFailed { error, .. }
            | Event::LyricsFailed { error, .. }
            | Event::RatingFailed { error, .. }
            | Event::LibraryToggleFailed { error, .. }
            | Event::PageLibraryFailed { error, .. }
            | Event::PlaylistEditFailed { error, .. }
            | Event::PlaylistDeleteFailed { error, .. }
            | Event::PlaylistLibraryFailed { error, .. }
            | Event::PlaylistSaveFailed { error, .. }
            | Event::PlaylistRemoveFailed { error, .. }
            | Event::PlaylistMoveFailed { error, .. }
            | Event::SignInFailed { error, .. }
            | Event::PlaybackFailed { error, .. } => Some(error),
            Event::AuthChanged(AuthState::Failed(error)) => Some(error),
            _ => None,
        }
    }
}

/// How a page header's library flip is written. Album headers carry no feedback token —
/// saving one is a like on its like-playlist id — while a playlist header that carries a
/// bookmark rides `feedback` like a row does.
#[derive(Clone, Debug)]
pub enum PageWrite {
    Feedback(String),
    LikePlaylist(String),
}

async fn fetch_bytes(http: &reqwest::Client, url: &str) -> anyhow::Result<bytes::Bytes> {
    Ok(http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?)
}

/// LRCLIB's synced lyrics for a track, if it has them. No auth, one GET. `None` on any miss —
/// the caller falls back to InnerTube's plain text.
async fn lrclib(http: &reqwest::Client, track: &Track) -> Option<crate::model::Lyrics> {
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
            return Some(crate::model::Lyrics {
                lines,
                synced: true,
                source: "LRCLIB".to_owned(),
            });
        }
    }
    let plain = body
        .get("plainLyrics")
        .and_then(serde_json::Value::as_str)?;
    Some(crate::model::Lyrics {
        lines: plain_lines(plain),
        synced: false,
        source: "LRCLIB".to_owned(),
    })
}

/// A single/album track's `wide: true` id points at a music-video upload, with no separate
/// audio-only id anywhere in the album metadata itself — the typed and raw album parsers both
/// read exactly one id per row (see `map_album`/`track_states`), so the only way to find the
/// audio version, when one exists, is to search for it. Match confidence is deliberately loose
/// (trimmed, case-insensitive whole-string compare) since remasters/features/parenthetical tags
/// can differ between the two uploads; a search failure or no close-enough match just means the
/// caller falls back to the original id, never blocks or delays playback.
async fn audio_alternative(api: &Api, track: &Track) -> Option<String> {
    let artist = track.artist_line();
    let query = format!("{} {}", track.title, artist);
    let results = api
        .search_scoped(&query, SearchScope::Songs)
        .await
        .inspect_err(|e| warn!(error = %format!("{e:#}"), "audio-alternative search failed"))
        .ok()?;
    results.into_iter().find_map(|entry| match entry {
        Entry::Song(t)
            if t.title.trim().eq_ignore_ascii_case(track.title.trim())
                && t.artist_line().trim().eq_ignore_ascii_case(artist.trim()) =>
        {
            Some(t.video_id)
        }
        _ => None,
    })
}

/// Split plain text into lines, trimming the ends.
fn plain_lines(text: &str) -> Vec<crate::model::LyricLine> {
    text.lines()
        .map(|l| crate::model::LyricLine {
            at_ms: 0,
            text: l.trim_end().to_owned(),
        })
        .collect()
}

/// Parse an LRC body (`[mm:ss.xx] line`) into sorted, timestamped lines. Malformed stamps are
/// skipped rather than failing the whole set.
fn parse_lrc(text: &str) -> Vec<crate::model::LyricLine> {
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
        out.push(crate::model::LyricLine {
            at_ms,
            text: line[close + 1..].trim().to_owned(),
        });
    }
    out.sort_by_key(|l| l.at_ms);
    out
}

/// `mm:ss.xx` (or `.xxx`) to milliseconds. A two-digit fraction is hundredths.
fn lrc_time_ms(stamp: &str) -> Option<u64> {
    let (mm, rest) = stamp.split_once(':')?;
    let mm: u64 = mm.trim().parse().ok()?;
    let (ss, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let ss: u64 = ss.trim().parse().ok()?;
    let frac: u64 = frac
        .chars()
        .take(3)
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    let ms = match frac.to_string().len() {
        1 => frac * 100,
        2 => frac * 10,
        _ => frac,
    };
    Some(mm * 60_000 + ss * 1000 + ms)
}

/// The `yt-dlp` binary to run.
///
/// yt-dlp is not optional (it is the only resolver that actually produces a playable URL; see
/// `YtDlpResolver`'s own doc comment) and every packaged build carries its own copy rather than
/// assuming the target machine already has one on `PATH`: `scripts/build-appimage.sh` downloads
/// a fresh standalone build into `usr/bin/yt-dlp`, and `scripts/build-windows.sh` places
/// `yt-dlp.exe` right next to `refryt.exe`. `$APPDIR` (the standard AppImage runtime variable an
/// AppRun sets to the mounted image's root) is checked first since that copy is not sitting
/// next to this binary; everywhere else, a copy beside the running executable itself is
/// preferred over a bare `PATH` lookup — deterministic rather than depending on how a given OS
/// orders its search, and what lets `browser_cookies::import` (which needs the very same binary)
/// share this instead of guessing again. A plain `cargo run` has no bundled copy next to the dev
/// binary either, so local development falls back to whatever `yt-dlp` a real `PATH` lookup
/// finds.
///
/// `scripts/build-macos.sh` puts a standalone `yt-dlp` beside the executable inside the app
/// bundle, so a packaged macOS build takes the "beside the executable" path. On macOS there is one
/// more fallback, after that: an app opened from Finder or a login item gets launchd's minimal
/// `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), which does not include Homebrew, so the Homebrew
/// prefixes (`/opt/homebrew/bin` on Apple Silicon, `/usr/local/bin` on Intel) are checked
/// explicitly before giving up and trusting `PATH`.
pub(crate) fn yt_dlp_program() -> String {
    if let Ok(appdir) = std::env::var("APPDIR") {
        return format!("{appdir}/usr/bin/yt-dlp");
    }
    let exe_name = if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(exe_name)));
    match bundled {
        Some(path) if path.is_file() => return path.to_string_lossy().into_owned(),
        _ => {}
    }
    // A bundle launched from Finder or a LaunchAgent gets launchd's minimal `PATH`
    // (`/usr/bin:/bin:/usr/sbin:/sbin`), which has none of Homebrew's prefixes.
    #[cfg(target_os = "macos")]
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let path = std::path::Path::new(dir).join(exe_name);
        if path.is_file() {
            return path.to_string_lossy().into_owned();
        }
    }
    "yt-dlp".to_owned()
}

/// The URL mpv should load for `stream`: through the proxy when the stream has to be read in
/// ranges, its own URL otherwise (or if the proxy is unavailable).
fn playable_url(
    proxy: Option<&refryt_resolve::StreamProxy>,
    id: &VideoId,
    stream: &refryt_resolve::AudioStream,
) -> url::Url {
    match proxy {
        Some(proxy) if stream.needs_ranged => proxy
            .url_for(id, stream)
            .inspect_err(
                |e| warn!(error = %format!("{e:#}"), "stream proxy: playing the URL directly"),
            )
            .unwrap_or_else(|_| stream.url.clone()),
        _ => stream.url.clone(),
    }
}

/// Run yt-dlp for one track, emitting progress as it goes, and return the file it produced.
///
/// Anonymous first, like playback; a failed download is tried once more as the signed-in
/// account, for the songs YouTube only hands to one (its bot check, its age gate). Unlike
/// playback the retry keeps the audio-only format, so a video file never lands in the music
/// folder — see `YtDlpResolver::run_gated` for why signed-in is not the default.
async fn run_download(
    track: &Track,
    session: &refryt_resolve::SessionCookie,
    format: &str,
    tx: &Sender<Event>,
    ctx: &egui::Context,
) -> anyhow::Result<std::path::PathBuf> {
    let anonymous = run_download_as(track, None, format, tx, ctx).await;
    if anonymous.is_ok() {
        return anonymous;
    }
    // Held until yt-dlp exits; dropping it deletes the file.
    let Some(cookies) = refryt_resolve::CookieFile::for_session(session) else {
        return anonymous;
    };
    tracing::info!(video_id = %track.video_id, "download failed anonymously; retrying as the account");
    run_download_as(track, Some(&cookies), format, tx, ctx).await
}

async fn run_download_as(
    track: &Track,
    cookies: Option<&refryt_resolve::CookieFile>,
    format: &str,
    tx: &Sender<Event>,
    ctx: &egui::Context,
) -> anyhow::Result<std::path::PathBuf> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let dir = crate::downloads::dir()?;
    std::fs::create_dir_all(&dir)?;
    let args = crate::downloads::yt_dlp_args(track, format, &dir);
    // Shares playback's cap on concurrent yt-dlp runs; held until the process has exited.
    let _permit = refryt_resolve::ytdlp_permit().await;
    let mut cmd = tokio::process::Command::new(yt_dlp_program());
    cmd.args(&args);
    if let Some(file) = cookies {
        cmd.arg("--cookies").arg(file.path());
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // `CREATE_NO_WINDOW`: yt-dlp is a console-subsystem binary, and without this Windows gives
    // it a console of its own — a terminal window sitting on screen for the whole download.
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start yt-dlp: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("yt-dlp gave no output"))?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(percent) = parse_progress(&line) {
            Backend::emit(
                tx,
                ctx,
                Event::DownloadProgress {
                    video_id: track.video_id.clone(),
                    percent,
                },
            );
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|e| anyhow::anyhow!("yt-dlp did not finish: {e}"))?;
    if !status.success() {
        anyhow::bail!("yt-dlp exited with {status}");
    }
    // Not from stdout: the sanitized title can carry characters yt-dlp's own stream drops (see
    // `yt_dlp_args`), so the file is located by the id marker the template guarantees.
    crate::downloads::find_downloaded(&dir, &track.video_id)
        .ok_or_else(|| anyhow::anyhow!("yt-dlp reported success but left no file"))
}

/// A `[download]  42.3% of ...` line as a number, or `None` for any other line.
fn parse_progress(line: &str) -> Option<f32> {
    let before = line.split('%').next()?;
    if before.len() == line.len() {
        return None;
    }
    before.split_whitespace().last()?.parse::<f32>().ok()
}

/// The track's cover art bytes, for embedding. Best-effort.
async fn cover_bytes(http: &reqwest::Client, track: &Track) -> Option<Vec<u8>> {
    let url = track.thumbnail.as_deref()?;
    let response = http.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    Some(response.bytes().await.ok()?.to_vec())
}

pub struct Backend {
    rt: tokio::runtime::Runtime,
    /// One client, reused for every cover, so connections stay pooled.
    http: reqwest::Client,
    api: Arc<Api>,
    resolver: Arc<StreamCache<ChainResolver>>,
    /// Plays the guest resolver's streams to mpv in ranges (see `refryt_resolve::StreamProxy`).
    /// `None` only if no loopback port could be opened; those streams then play by URL, slowly.
    proxy: Option<Arc<refryt_resolve::StreamProxy>>,
    /// The video view's own resolver and cache. `yt-dlp` is the only path that can produce a
    /// video URL, so this is not a chain — and it is a separate cache because the video URL for
    /// an id is a different answer from the audio one, which the queue may be playing.
    video_resolver: Arc<VideoStreamCache<YtDlpResolver>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    ctx: egui::Context,
    /// Bumped on every `play`, so a superseded track's scrobble task stops itself instead of
    /// beaconing a song mpv has already left.
    scrobble_generation: Arc<AtomicU64>,
    /// Video id → downloaded file. `App` owns `downloads.json` and pushes the map here; the
    /// backend only reads it, to play a downloaded track from disk instead of resolving.
    downloads: Arc<std::sync::Mutex<std::collections::HashMap<String, std::path::PathBuf>>>,
}

impl Drop for Backend {
    /// One line for the exit log: dropping the runtime waits for whatever is still in flight,
    /// so a quit that drags needs to show whether this is where it drags. The `App` drop order
    /// runs this before the player's, so a missing `player: dropping` line names this one.
    fn drop(&mut self) {
        tracing::info!("backend: dropping");
    }
}

impl Backend {
    pub fn new(ctx: egui::Context) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            // Two workers is plenty: this app does a handful of concurrent HTTP requests, and
            // every thread costs memory we are trying not to spend.
            .worker_threads(2)
            .enable_all()
            .thread_name("refryt-worker")
            .build()?;

        let (tx, rx) = channel();

        let api = Arc::new(Api::new()?);
        // One pool for the guest `player` requests and the stream ranges after them, kept warm
        // between tracks: a new connection to the stream host costs ~1.3 s before the first byte,
        // a pooled one ~0.7 s for a whole MiB (measured 2026-09-23).
        let stream_http = reqwest::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .build()
            .map_err(|e| anyhow::anyhow!("could not build the stream client: {e}"))?;
        let mut guest = refryt_resolve::GuestResolver::new(stream_http.clone());
        if let Some(dirs) = directories::ProjectDirs::from("", "", "refryt") {
            let _ = std::fs::create_dir_all(dirs.config_dir());
            guest = guest.with_visitor_file(dirs.config_dir().join("visitor"));
        }
        let resolver = Arc::new(StreamCache::new(ChainResolver::default_order(
            guest,
            YtDlpResolver::new()
                .with_program(yt_dlp_program())
                .with_session(api.ytdlp_session()),
        )));
        let refreshing = Arc::clone(&resolver);
        let refresh: refryt_resolve::Refresh = Arc::new(move |id| {
            let resolver = Arc::clone(&refreshing);
            Box::pin(async move { resolver.refresh(&id).await })
        });
        let proxy = refryt_resolve::StreamProxy::start(rt.handle(), stream_http, refresh)
            .inspect_err(|e| warn!(error = %format!("{e:#}"), "stream proxy did not start"))
            .ok();
        let video_resolver = VideoStreamCache::new(
            YtDlpResolver::new()
                .with_program(yt_dlp_program())
                .with_format(refryt_resolve::VIDEO_FORMAT)
                .with_session(api.ytdlp_session()),
        );

        Ok(Self {
            rt,
            http: reqwest::Client::builder()
                .use_rustls_tls()
                .build()
                .map_err(|e| anyhow::anyhow!("could not build the image client: {e}"))?,
            api,
            resolver,
            proxy,
            video_resolver: Arc::new(video_resolver),
            tx,
            rx,
            ctx,
            scrobble_generation: Arc::new(AtomicU64::new(0)),
            downloads: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        })
    }

    /// Replace the backend's view of what is downloaded, after the index loads or changes.
    pub fn set_downloads(&self, paths: std::collections::HashMap<String, std::path::PathBuf>) {
        if let Ok(mut guard) = self.downloads.lock() {
            *guard = paths;
        }
    }

    /// Download one track to `~/Music/refryt`, reporting progress, then write its tags and cover.
    /// Completion carries the `Track` back so the app can add it to the index.
    pub fn download_track(&self, track: Track, format: &'static str) {
        let (tx, ctx, http) = (self.tx.clone(), self.ctx.clone(), self.http.clone());
        let session = self.api.ytdlp_session();
        self.rt.spawn(async move {
            let video_id = track.video_id.clone();
            match run_download(&track, &session, format, &tx, &ctx).await {
                Ok(path) => {
                    let cover = cover_bytes(&http, &track).await;
                    let (tag_path, tag_track) = (path.clone(), track.clone());
                    // Blocking `lofty` file I/O off the runtime's own worker threads (only 2 of
                    // them — see `Self::new`) so a slow tag-embed can't stall unrelated async
                    // work sharing the runtime.
                    let _ = tokio::task::spawn_blocking(move || {
                        downloads::embed_tags(&tag_path, &tag_track, cover.as_deref());
                    })
                    .await;
                    Self::emit(
                        &tx,
                        &ctx,
                        Event::DownloadDone {
                            video_id,
                            path,
                            track: Box::new(track),
                        },
                    );
                }
                Err(e) => Self::emit(
                    &tx,
                    &ctx,
                    Event::DownloadFailed {
                        video_id,
                        error: format!("{e:#}"),
                    },
                ),
            }
        });
    }

    /// Download several tracks one at a time — an album or playlist header's "Download all". One
    /// task, so a fifty-track album does not launch fifty yt-dlp processes at once.
    pub fn download_many(&self, tracks: Vec<Track>, format: &'static str) {
        let (tx, ctx, http) = (self.tx.clone(), self.ctx.clone(), self.http.clone());
        let session = self.api.ytdlp_session();
        self.rt.spawn(async move {
            for track in tracks {
                let video_id = track.video_id.clone();
                match run_download(&track, &session, format, &tx, &ctx).await {
                    Ok(path) => {
                        let cover = cover_bytes(&http, &track).await;
                        let (tag_path, tag_track) = (path.clone(), track.clone());
                        let _ = tokio::task::spawn_blocking(move || {
                            downloads::embed_tags(&tag_path, &tag_track, cover.as_deref());
                        })
                        .await;
                        Self::emit(
                            &tx,
                            &ctx,
                            Event::DownloadDone {
                                video_id,
                                path,
                                track: Box::new(track),
                            },
                        );
                    }
                    Err(e) => Self::emit(
                        &tx,
                        &ctx,
                        Event::DownloadFailed {
                            video_id,
                            error: format!("{e:#}"),
                        },
                    ),
                }
            }
        });
    }

    fn emit(tx: &Sender<Event>, ctx: &egui::Context, event: Event) {
        let big = event.follows_big_parse();
        if tx.send(event).is_ok() {
            // Wake the UI thread; without this the result would sit unseen until the next
            // input event.
            ctx.request_repaint();
        }
        // The response is parsed and its tree dropped by now; give back what it left behind.
        if big {
            crate::memory::trim_after_parse();
        }
    }

    /// Ask GitHub whether a newer release is out, after `delay`: the launch check waits until
    /// the first pages have had the network to themselves.
    pub fn check_for_update(&self, delay: std::time::Duration, manual: bool) {
        let (tx, ctx, http) = (self.tx.clone(), self.ctx.clone(), self.http.clone());
        self.rt.spawn(async move {
            tokio::time::sleep(delay).await;
            let event = match crate::update::check(&http).await {
                Ok(release) => Event::UpdateChecked {
                    release: release.map(Box::new),
                    manual,
                },
                Err(e) => Event::UpdateCheckFailed {
                    error: format!("{e:#}"),
                    manual,
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Download the release, check it, and stage it for `main` to run once the window closes.
    pub fn install_update(&self, release: crate::update::Release) {
        let (tx, ctx, http) = (self.tx.clone(), self.ctx.clone(), self.http.clone());
        self.rt.spawn(async move {
            let progress = |percent| Self::emit(&tx, &ctx, Event::UpdateProgress { percent });
            let staged = match crate::update::download(&http, &release, progress).await {
                Ok(file) => tokio::task::spawn_blocking(move || crate::update::stage(&file))
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}"))),
                Err(e) => Err(e),
            };
            let event = match staged {
                Ok(()) => Event::UpdateReady,
                Err(e) => Event::UpdateFailed {
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Synchronous check used to pick the initial auth UI state. Everything else that needs the
    /// answer is driven by `Event::AuthChanged`.
    pub fn is_signed_in_sync(&self) -> bool {
        self.api.is_signed_in_sync()
    }

    /// Instant local work that still lives here because this is where the UI thread's
    /// `egui::Context` is kept.
    pub fn copy_text(&self, text: String) {
        self.ctx.copy_text(text);
    }

    pub fn search(&self, query: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.search(&query).await {
                Ok(results) => Event::SearchDone { query, results },
                Err(e) => Event::SearchFailed {
                    query,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Autocomplete for the search field. Failure is silent — the field simply shows nothing — so
    /// only a debug line marks it.
    pub fn fetch_suggestions(&self, query: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.suggestions(&query).await {
                Ok(items) => Event::SuggestionsDone { query, items },
                Err(e) => Event::SuggestionsFailed {
                    query,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    pub fn search_scoped(&self, query: String, scope: crate::model::SearchScope) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.search_scoped(&query, scope).await {
                Ok(results) => Event::ScopedSearchDone {
                    query,
                    scope,
                    results,
                },
                Err(e) => Event::ScopedSearchFailed {
                    query,
                    scope,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Rescans every configured local-music folder from scratch. Directory walking and tag
    /// parsing are blocking I/O, so they run on `spawn_blocking` rather than on the runtime's
    /// async worker threads directly.
    pub fn scan_local_folders(&self, roots: Vec<String>) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match tokio::task::spawn_blocking(move || crate::local::scan(&roots)).await
            {
                Ok(tracks) => Event::LocalScanDone { tracks },
                Err(e) => Event::LocalScanFailed {
                    error: format!("local scan panicked: {e}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Write the playback-persistence store off the render thread. `App::poll_playback` calls
    /// this every few seconds while something plays, and the queue it serializes only grows over
    /// a long session — doing the `serde_json` encode and the disk write synchronously on the UI
    /// thread turned a routine periodic save into a growing frame hitch.
    pub fn save_playback(&self, store: crate::playback::Store) {
        self.rt.spawn(async move {
            if let Err(e) = tokio::task::spawn_blocking(move || crate::playback::save(&store))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("playback save panicked: {e}")))
            {
                tracing::debug!(error = %format!("{e:#}"), "could not save the playback state");
            }
        });
    }

    pub fn fetch_artist(&self, id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.get_artist(&id).await {
                Ok(artist) => Event::ArtistLoaded { id, artist },
                Err(e) => Event::ArtistFailed {
                    id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    pub fn fetch_album(&self, id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.get_album(&id).await {
                Ok(album) => Event::AlbumLoaded { id, album },
                Err(e) => Event::AlbumFailed {
                    id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// An album card's own play button: start playing it without opening its page. Whatever
    /// the first page of tracks turns out to be is what plays — the same as the album/playlist
    /// page's own Play button, which never waits for pagination either.
    pub fn play_album(&self, id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            match api.get_album(&id).await {
                Ok(album) => Self::emit(
                    &tx,
                    &ctx,
                    Event::CardTracksReady {
                        id,
                        tracks: album.tracks,
                    },
                ),
                Err(e) => {
                    tracing::warn!(%id, error = %format!("{e:#}"), "could not load album to play it")
                }
            }
        });
    }

    /// [`Self::play_album`], for a playlist card.
    pub fn play_playlist(&self, id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            match api.playlist(&id).await {
                Ok(playlist) => Self::emit(
                    &tx,
                    &ctx,
                    Event::CardTracksReady {
                        id,
                        tracks: playlist.tracks.items,
                    },
                ),
                Err(e) => {
                    tracing::warn!(%id, error = %format!("{e:#}"), "could not load playlist to play it")
                }
            }
        });
    }

    pub fn fetch_library(&self, sort: crate::api::LibrarySortParam, epoch: u64) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.library(sort).await {
                Ok(library) => Event::LibraryLoaded { library, epoch },
                Err(e) => Event::LibraryFailed {
                    error: format!("{e:#}"),
                    epoch,
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Page Liked Music into `Event::LikedPage`s, one page per event, ending with
    /// `Event::LikedScanDone`.
    ///
    /// `full` reads the playlist to its end; `false` refreshes only the first page — the newest
    /// likes — which is what every launch does. A full scan happens only when the cache is
    /// missing or old (see `crate::liked::MAX_AGE`), because it is dozens of continuation
    /// requests on a large library. Continuations are retried like `fetch_playlist_more`'s: a
    /// flaky body read halfway down a 5,000-row playlist must not end the scan.
    pub fn fetch_liked(&self, epoch: u64, full: bool) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let mut page = match api.playlist("VLLM").await {
                Ok(p) => p.tracks,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "liked songs could not be read");
                    Self::emit(
                        &tx,
                        &ctx,
                        Event::LikedScanDone {
                            complete: false,
                            epoch,
                        },
                    );
                    return;
                }
            };
            loop {
                let done = page.next.is_none() || !full;
                let complete = page.next.is_none();
                let tracks: Vec<crate::model::Track> = std::mem::take(&mut page.items);
                Self::emit(&tx, &ctx, Event::LikedPage { tracks, epoch });
                if done {
                    Self::emit(&tx, &ctx, Event::LikedScanDone { complete, epoch });
                    return;
                }
                // A short breather between pages: this is background work, and the playlist is
                // a long chain of requests that would otherwise look like a scrape.
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                let token = page.next.take().expect("checked above");
                const ATTEMPTS: u32 = 3;
                let mut last_err = None;
                for attempt in 0..ATTEMPTS {
                    if attempt > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            300 * 3u64.pow(attempt - 1),
                        ))
                        .await;
                    }
                    match api.playlist_more(token.clone()).await {
                        Ok(more) => {
                            last_err = None;
                            page = crate::model::Paged::new(
                                more.items
                                    .iter()
                                    .filter_map(|e| e.track().cloned())
                                    .collect(),
                                more.next,
                            );
                            break;
                        }
                        Err(e) => last_err = Some(e),
                    }
                }
                if let Some(e) = last_err {
                    tracing::warn!(error = %format!("{e:#}"), "liked songs scan stopped");
                    Self::emit(
                        &tx,
                        &ctx,
                        Event::LikedScanDone {
                            complete: false,
                            epoch,
                        },
                    );
                    return;
                }
            }
        });
    }
    /// The next page of a playlist's tracks.
    ///
    /// Retried a couple of times on failure, unlike every other single-shot fetch in this file:
    /// a long playlist's "load everything, then sort" chain (`App::sort_playlist`) is dozens of
    /// these continuation requests back to back, one token depending on the last, so there is no
    /// way to parallelise it — and at that length, one of them occasionally drops with a
    /// transient body-read error that a bare retry clears right up. Without this, that single
    /// flaky page killed the whole chain and the sort silently never happened.
    pub fn fetch_playlist_more(&self, id: String, token: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            const ATTEMPTS: u32 = 3;
            let mut last_err = None;
            let mut page = None;
            for attempt in 0..ATTEMPTS {
                if attempt > 0 {
                    // 300ms, then 900ms — long enough to clear a transient hiccup, short
                    // enough that even a fully-retried page barely shows in a chain this long.
                    tokio::time::sleep(std::time::Duration::from_millis(
                        300 * 3u64.pow(attempt - 1),
                    ))
                    .await;
                }
                match api.playlist_more(token.clone()).await {
                    Ok(p) => {
                        page = Some(p);
                        break;
                    }
                    Err(e) => {
                        warn!(attempt, %id, error = %format!("{e:#}"), "playlist continuation failed, retrying");
                        last_err = Some(e);
                    }
                }
            }
            let event = match page {
                Some(page) => Event::PlaylistMore { id, page },
                None => Event::PlaylistMoreFailed {
                    id,
                    error: format!("{:#}", last_err.expect("loop always sets this on failure")),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The Suggestions under one of your playlists — the first batch, or a Refresh.
    pub fn fetch_playlist_suggestions(&self, id: String, token: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let suggestions = match api.playlist_suggestions(token).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(%id, error = %format!("{e:#}"), "could not load playlist suggestions");
                    None
                }
            };
            Self::emit(&tx, &ctx, Event::PlaylistSuggestions { id, suggestions });
        });
    }

    /// Ask whether the signed-in account holds YouTube Premium, which downloads need.
    pub fn check_premium(&self, epoch: u64) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let premium = match api.is_premium().await {
                Ok(p) => Some(p),
                Err(e) => {
                    warn!(error = %format!("{e:#}"), "could not check for YouTube Premium");
                    None
                }
            };
            Self::emit(&tx, &ctx, Event::PremiumChecked { premium, epoch });
        });
    }

    /// Is YouTube Music reachable again? Asked every few seconds while the app believes it is
    /// offline — one small request, answered by `Event::Connection`.
    pub fn probe_connection(&self) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let online = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
            {
                Ok(client) => client
                    .head("https://music.youtube.com/")
                    .send()
                    .await
                    .is_ok(),
                Err(_) => false,
            };
            Self::emit(&tx, &ctx, Event::Connection { online });
        });
    }

    /// The first page of your playlists, for the sidebar.
    pub fn fetch_library_playlists(&self, epoch: u64) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.library_playlists().await {
                Ok(page) => Event::SidebarPlaylists { page, epoch },
                Err(e) => Event::SidebarPlaylistsFailed {
                    error: format!("{e:#}"),
                    epoch,
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The next page of one library section.
    pub fn fetch_library_more(&self, section: crate::api::LibrarySection, token: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.library_more(section, token).await {
                Ok(page) => Event::LibraryMore { section, page },
                Err(e) => Event::LibraryMoreFailed {
                    section,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    pub fn fetch_playlist(&self, id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.playlist(&id).await {
                Ok(playlist) => Event::PlaylistLoaded { id, playlist },
                Err(e) => Event::PlaylistFailed {
                    id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Expand one of an artist's shelf previews (Albums, Videos, Playlists, ...) into its full
    /// list. `browse_id` is whatever id that shelf's own "more" button carries — the artist's
    /// channel id only coincidentally, not by rule — paired with that shelf's `params`.
    /// Expand one artist shelf, `title` naming the shelf so the response can be narrowed to its
    /// own section — see [`crate::api::Api::artist_albums`].
    pub fn fetch_artist_albums(&self, browse_id: String, params: String, title: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.artist_albums(&browse_id, &params, &title).await {
                Ok(items) => Event::ArtistAlbumsLoaded {
                    artist_id: browse_id,
                    items,
                },
                Err(e) => Event::ArtistAlbumsFailed {
                    artist_id: browse_id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The home feed, as YouTube would serve it — optionally through a mood-bar filter.
    /// One-way feedback: "don't recommend this artist". There is nothing to report back but a
    /// failure, and the toast has already gone up.
    pub fn send_feedback(&self, token: String) {
        let api = Arc::clone(&self.api);
        self.rt.spawn(async move {
            if let Err(e) = api.feedback(&token).await {
                tracing::warn!(error = %format!("{e:#}"), "feedback refused");
            }
        });
    }

    /// Follow or unfollow an artist.
    pub fn set_subscribed(&self, channel_id: String, subscribe: bool, params: Option<Arc<str>>) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api
                .set_subscribed(&channel_id, subscribe, params.as_deref())
                .await
            {
                Ok(()) => Event::SubscriptionChanged {
                    channel_id,
                    subscribed: subscribe,
                },
                Err(e) => Event::SubscriptionFailed {
                    channel_id,
                    previous: !subscribe,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Play whatever YouTube handed over as a target — an artist's shuffle or mix.
    pub fn play_target(&self, seed: String, target: crate::model::WatchTarget) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.watch(&target).await {
                Ok(queue) => Event::MixLoaded { seed, queue },
                Err(e) => Event::MixFailed {
                    seed,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The Explore landing feed. Works signed out.
    pub fn fetch_explore(&self) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.explore().await {
                Ok(feed) => Event::ExploreLoaded {
                    shelves: feed.shelves,
                    links: feed.links,
                },
                Err(e) => Event::ExploreFailed {
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// New releases, behind Explore's first button. Works signed out.
    pub fn fetch_new_releases(&self) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.new_releases().await {
                Ok(shelves) => Event::ReleasesLoaded { shelves },
                Err(e) => Event::ReleasesFailed {
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Charts, behind Explore's second button. `params` is what the Explore feed sent with
    /// its Charts button, and a reply for params since changed is stale. Works signed out.
    pub fn fetch_charts(&self, params: Option<String>) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.charts(params.clone()).await {
                Ok(feed) => Event::ChartsLoaded { params, feed },
                Err(e) => Event::ChartsFailed {
                    params,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Every mood category, behind Explore's third button. Works signed out.
    pub fn fetch_moods(&self) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.moods().await {
                Ok(sections) => Event::MoodsLoaded { sections },
                Err(e) => Event::MoodsFailed {
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// What you played, in dated shelves. Signed in only — the page gates, not the call.
    /// `epoch` is the visit this serves, echoed back so a superseded refresh is dropped.
    pub fn fetch_history(&self, epoch: u64) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.history().await {
                Ok(shelves) => Event::HistoryLoaded { shelves, epoch },
                Err(e) => Event::HistoryFailed {
                    error: format!("{e:#}"),
                    epoch,
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Create a playlist on the account, then reload the rail so it appears.
    pub fn create_playlist(&self, title: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.create_playlist(&title).await {
                Ok(()) => Event::PlaylistCreated { title },
                Err(e) => Event::PlaylistCreateFailed {
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    pub fn fetch_home(&self, filter: Option<String>, epoch: u64, fetch: crate::model::HomeFetch) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.home(filter.as_deref(), &fetch).await {
                Ok(feed) => Event::HomeLoaded {
                    filter,
                    shelves: feed.shelves,
                    chips: feed.chips,
                    next: feed.next,
                    epoch,
                },
                Err(e) => Event::HomeFailed {
                    filter,
                    error: format!("{e:#}"),
                    epoch,
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The next page of the home feed; see `Api::home_more`.
    pub fn fetch_home_more(&self, token: String, filter: Option<String>, epoch: u64) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.home_more(token).await {
                Ok((shelves, next)) => Event::HomeMore {
                    filter,
                    shelves,
                    next,
                    epoch,
                },
                Err(e) => Event::HomeMoreFailed {
                    filter,
                    error: format!("{e:#}"),
                    epoch,
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The full list behind a Home shelf's "Show all". The request is the one an artist shelf's
    /// "Show all" makes — `Api::artist_albums` takes any shelf's (browse id, params) pair and
    /// picks that shelf's own section out of whatever page answers.
    pub fn fetch_home_shelf(&self, browse_id: String, params: String, title: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.artist_albums(&browse_id, &params, &title).await {
                Ok(items) => Event::HomeShelfLoaded { title, items },
                Err(e) => Event::HomeShelfFailed {
                    title,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    pub fn fetch_mood_playlists(&self, params: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.mood_playlists(&params).await {
                Ok(mood) => Event::MoodPlaylistsLoaded { params, mood },
                Err(e) => Event::MoodPlaylistsFailed {
                    params,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Radio for a song: the `next` endpoint's watch queue.
    pub fn start_mix(&self, video_id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.mix(&video_id).await {
                Ok(queue) => Event::MixLoaded {
                    seed: video_id,
                    queue,
                },
                Err(e) => Event::MixFailed {
                    seed: video_id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Radio for a playlist or album: the same `next` watch queue as [`start_mix`], seeded by
    /// the playlist rather than a song.
    pub fn start_playlist_mix(&self, playlist_id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.mix_playlist(&playlist_id).await {
                Ok(queue) => Event::MixLoaded {
                    seed: playlist_id,
                    queue,
                },
                Err(e) => Event::MixFailed {
                    seed: playlist_id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// One of the radio's filters (Up next's "All ▾"): the same `next` call, with the filter's
    /// own playlist id and params.
    pub fn filter_radio(&self, target: crate::model::WatchTarget) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let playlist_id = target.playlist_id.clone();
            let event = match api.watch(&target).await {
                Ok(queue) => Event::RadioFiltered { playlist_id, queue },
                Err(e) => Event::RadioFilterFailed {
                    playlist_id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The first page of a hand-picked queue's autoplay tail: radio for its last track,
    /// appended rather than replacing what is playing. The same `next` call as `start_mix`,
    /// with a landing the app can tell apart.
    pub fn seed_mix(&self, seed: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.mix(&seed).await {
                Ok(queue) => Event::MixSeeded { seed, queue },
                Err(e) => Event::MixSeedFailed {
                    seed,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The next ~50 tracks of a radio river. `reseed` is the video id playing now: a dead
    /// token is never retried, `next` is re-seeded from it instead, and either way the new
    /// tracks come back through the same event for the app to append.
    pub fn extend_mix(&self, token: String, reseed: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.mix_more(&token).await {
                Ok(queue) => Event::MixMore { token, queue },
                Err(follow_error) => match api.mix(&reseed).await {
                    Ok(queue) => Event::MixMore { token, queue },
                    Err(e) => Event::MixMoreFailed {
                        token,
                        error: format!("follow failed: {follow_error:#}; reseed failed: {e:#}"),
                    },
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Rate a song. The app updates optimistically — a thumb that waits for a round trip feels
    /// broken — so only the refusal comes back, with the state to revert to. `previous` is the
    /// rating YouTube still holds, read by the app before it paints the new one.
    pub fn rate(
        &self,
        video_id: String,
        status: crate::model::LikeStatus,
        previous: crate::model::LikeStatus,
    ) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.rate(&video_id, status).await {
                Ok(()) => return,
                Err(e) => Event::RatingFailed {
                    video_id,
                    attempted: status,
                    previous,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Flip a page's (album or playlist header) library membership. Same optimistic rule as
    /// a song toggle — the reply carries what to revert — but the write depends on what the
    /// header offered: albums ride the like endpoint, playlist headers (where they carry a
    /// bookmark at all) ride `feedback` like rows do.
    pub fn toggle_page_library(&self, album: bool, write: PageWrite, in_library: bool) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let result = match &write {
                PageWrite::Feedback(token) => api.feedback(token).await,
                PageWrite::LikePlaylist(id) => {
                    let status = if in_library {
                        crate::model::LikeStatus::Like
                    } else {
                        crate::model::LikeStatus::Indifferent
                    };
                    api.rate_playlist(id, status).await
                }
            };
            let event = match result {
                Ok(()) => Event::PageLibraryToggled { album, in_library },
                Err(e) => Event::PageLibraryFailed {
                    album,
                    previous: !in_library,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Rename a playlist and/or set its description. Optimistic: the app repaints first and a
    /// failure puts the old text back.
    pub fn update_playlist(&self, id: String, title: String, description: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let edit = crate::api::PlaylistEdit::SetMeta {
                title: (!title.is_empty()).then(|| title.clone()),
                description: Some(description),
            };
            let event = match api.edit_playlist(&id, edit).await {
                Ok(()) => Event::PlaylistEdited { id },
                Err(e) => Event::PlaylistEditFailed {
                    id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Delete one of your own playlists.
    pub fn delete_playlist(&self, id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.delete_playlist(&id).await {
                Ok(()) => Event::PlaylistDeleted { id },
                Err(e) => Event::PlaylistDeleteFailed {
                    id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Remove a saved (someone else's) playlist from the library by unliking it.
    pub fn unsave_playlist(&self, id: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api
                .rate_playlist(&id, crate::model::LikeStatus::Indifferent)
                .await
            {
                Ok(()) => Event::PlaylistLibraryChanged { id, saved: false },
                Err(e) => Event::PlaylistLibraryFailed {
                    id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Save several videos to one playlist with a single summary event. One `add_to_playlist`
    /// per song would toast per song; a page saves dozens, so the page gets one verdict.
    pub fn add_many_to_playlist(&self, playlist_id: String, video_ids: Vec<String>) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let total = video_ids.len();
            let (mut added, mut duplicates) = (0, 0);
            // A song the playlist already has is skipped, not asked about: a page saves dozens,
            // and a dialog per song would be worse than the summary saying how many were there.
            for video_id in &video_ids {
                match api.add_to_playlist(&playlist_id, video_id, false).await {
                    Ok(crate::innertube::EditReply::Done) => added += 1,
                    Ok(crate::innertube::EditReply::Duplicate(_)) => duplicates += 1,
                    Ok(crate::innertube::EditReply::Refused(m)) => {
                        tracing::warn!(%playlist_id, video_id = %video_id, message = %m,
                            "bulk save refused a song");
                    }
                    Err(e) => tracing::warn!(%playlist_id, video_id = %video_id,
                        error = %format!("{e:#}"), "bulk save refused a song"),
                }
            }
            Self::emit(
                &tx,
                &ctx,
                Event::PlaylistBulkSaved {
                    playlist_id,
                    added,
                    duplicates,
                    total,
                },
            );
        });
    }

    /// Flip a song's library membership with its own bookmark token. `in_library` is the state
    /// being asked for, so the reply can revert to `!in_library` on refusal.
    pub fn toggle_library(&self, token: String, video_id: String, in_library: bool) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.feedback(&token).await {
                Ok(()) => Event::LibraryToggled {
                    video_id,
                    in_library,
                },
                Err(e) => Event::LibraryToggleFailed {
                    video_id,
                    previous: !in_library,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Save a video to one of your playlists. A song already there comes back as
    /// `PlaylistDuplicate` rather than being added twice, unless `allow_duplicate`.
    pub fn add_to_playlist(&self, playlist_id: String, video_id: String, allow_duplicate: bool) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let reply = api
                .add_to_playlist(&playlist_id, &video_id, allow_duplicate)
                .await;
            let event = match reply {
                Ok(crate::innertube::EditReply::Done) => Event::PlaylistSaved {
                    playlist_id,
                    video_id,
                },
                Ok(crate::innertube::EditReply::Duplicate(message)) => Event::PlaylistDuplicate {
                    playlist_id,
                    video_id,
                    message,
                },
                Ok(crate::innertube::EditReply::Refused(m)) => Event::PlaylistSaveFailed {
                    playlist_id,
                    video_id,
                    error: format!("refused: {m}"),
                },
                Err(e) => Event::PlaylistSaveFailed {
                    playlist_id,
                    video_id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Remove a video from one of your playlists, by its playlist item id.
    pub fn remove_from_playlist(
        &self,
        playlist_id: String,
        video_id: String,
        set_video_id: String,
    ) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let edit = crate::api::PlaylistEdit::RemoveVideo {
                video_id: video_id.clone(),
                set_video_id,
            };
            let event = match api.edit_playlist(&playlist_id, edit).await {
                Ok(()) => Event::PlaylistRemoved {
                    playlist_id,
                    video_id,
                },
                Err(e) => Event::PlaylistRemoveFailed {
                    playlist_id,
                    video_id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Move a track within one of your playlists to sit immediately before another — a
    /// drag-to-reorder in the track list itself. Both ids are playlist item ids
    /// (`setVideoId`s), not video ids.
    pub fn move_playlist_item(
        &self,
        playlist_id: String,
        set_video_id: String,
        before_set_video_id: String,
    ) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let edit = crate::api::PlaylistEdit::MoveVideo {
                set_video_id,
                before_set_video_id,
            };
            let event = match api.edit_playlist(&playlist_id, edit).await {
                Ok(()) => Event::PlaylistMoved { playlist_id },
                Err(e) => Event::PlaylistMoveFailed {
                    playlist_id,
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    // ------------------------------------------------------------------------------------------
    // Sign-in
    // ------------------------------------------------------------------------------------------

    /// Confirm a stored session at startup, so the UI never asserts a sign-in it has not checked.
    pub fn verify_session(&self) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.verify_session().await {
                Ok(Some(account)) => Event::AuthChanged(AuthState::SignedIn { account }),
                // YouTube answered and does not know us: the session is genuinely dead.
                Ok(None) => {
                    // Not deleted: a rotated credential whose refresh failed looks identical to a
                    // dead one, and the next run may well recover it.
                    api.forget_session().await;
                    Self::emit(&tx, &ctx, Event::AuthChanged(AuthState::SignedOut));
                    Event::SessionExpired
                }
                // We could not ask. Keep the session — being offline is not being signed out.
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "could not check the saved session");
                    Event::AuthChanged(AuthState::Failed(format!(
                        "could not check your saved session: {e:#}"
                    )))
                }
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Sign in by importing cookies from a browser you are already signed into.
    ///
    /// The path that actually works: a cookie copied from the DevTools console cannot include the
    /// HttpOnly session cookies, so it authenticates nothing.
    pub fn sign_in_from_browser(&self, browser: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.sign_in_from_browser(&browser).await {
                Ok(account) => {
                    Self::forget_legacy_token();
                    Event::AuthChanged(AuthState::SignedIn { account })
                }
                Err(e) => Event::SignInFailed {
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Sign in with a `Cookie` header pasted from a browser's developer tools.
    ///
    /// No source is stored with it: there is nowhere to renew a pasted session from, so when it
    /// expires the user is told and pastes again.
    pub fn sign_in_with_cookie(&self, raw: String) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.install_cookie(raw, None).await {
                Ok(account) => {
                    Self::forget_legacy_token();
                    Event::AuthChanged(AuthState::SignedIn { account })
                }
                Err(e) => Event::SignInFailed {
                    error: format!("{e:#}"),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Sign in through the app's own sign-in window (Windows' WebView2, Linux's WebKitGTK).
    ///
    /// The path for a machine whose only browser is a modern Chromium, and on Linux simply the
    /// one that needs no browser at all: the window owns the session it creates.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    pub fn sign_in_from_window(&self) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.sign_in_from_window().await {
                Ok(account) => {
                    Self::forget_legacy_token();
                    Event::AuthChanged(AuthState::SignedIn { account })
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "the sign-in window failed");
                    Event::SignInFailed {
                        error: format!("{e:#}"),
                    }
                }
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Only one credential at a time: signing in with a cookie drops any OAuth token.
    fn forget_legacy_token() {
        let _ = auth::delete_oauth();
    }

    /// Forget the cookie and the signed-in session.
    pub fn sign_out(&self) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            api.sign_out().await;
            Self::emit(&tx, &ctx, Event::AuthChanged(AuthState::SignedOut));
        });
    }

    /// Resolve a track to a stream URL and start playing it.
    /// Browse as a different channel, then reload what depended on the old one.
    pub fn switch_account(&self, page_id: Option<String>) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let event = match api.switch_account(page_id).await {
                Ok(()) => match api.verify_session().await {
                    Ok(Some(account)) => Event::AuthChanged(AuthState::SignedIn { account }),
                    Ok(None) => Event::AuthChanged(AuthState::SignedOut),
                    Err(e) => Event::AuthChanged(AuthState::Failed(format!("{e:#}"))),
                },
                Err(e) => Event::AuthChanged(AuthState::Failed(format!("{e:#}"))),
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// The channels this account can browse as.
    pub fn fetch_accounts(&self, epoch: u64) {
        let (api, tx, ctx) = (Arc::clone(&self.api), self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            match api.accounts().await {
                Ok(accounts) => {
                    Self::emit(&tx, &ctx, Event::AccountsLoaded { accounts, epoch })
                }
                // Not an error the user can act on — the chip falls back to an initial — but a
                // silent swallow is how a chip with no picture looks exactly like a chip whose
                // picture has not arrived yet.
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "could not list the account's channels")
                }
            }
        });
    }

    /// Ask `yt-dlp` for a different format from now on, and drop cached URLs resolved under the
    /// old one so the change takes effect on the next track rather than eventually.
    pub fn set_audio_quality(&self, format: &'static str) {
        self.resolver.inner().set_format(format);
        self.resolver.clear();
    }

    /// Returns the generation this attempt was started under — see `App::play_generation`'s own
    /// doc comment for why the caller needs it.
    pub fn play(
        &self,
        track: Track,
        mpv: Arc<Mpv>,
        volume: f64,
        normalise: bool,
        start_at: Option<f64>,
    ) -> u64 {
        let downloads = Arc::clone(&self.downloads);
        let (resolver, api, tx, ctx) = (
            Arc::clone(&self.resolver),
            Arc::clone(&self.api),
            self.tx.clone(),
            self.ctx.clone(),
        );
        // A new play supersedes any scrobble task still running for the old track, and — via the
        // `generation` stamped on every event below — any `Resolving`/`Playing`/`PlaybackFailed`
        // reply still in flight for a previous, now-superseded `play`/`play_video` call.
        let generation = self.scrobble_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let scrobble = Scrobble {
            api: Arc::clone(&self.api),
            http: self.http.clone(),
            generation: Arc::clone(&self.scrobble_generation),
        };
        Self::emit(
            &self.tx,
            &self.ctx,
            Event::Resolving(track.clone(), generation),
        );
        let proxy = self.proxy.clone();

        self.rt.spawn(async move {
            // A downloaded track plays from disk without the resolver — offline, and without the
            // yt-dlp latency. Unlike a local file it is still a real YouTube play, so it goes
            // through the same scrobble tail below.
            // `is_file`: an index entry whose file was moved or deleted must fall through to the
            // resolver, not hand mpv a path that cannot open.
            let downloaded = downloads
                .lock()
                .ok()
                .and_then(|guard| guard.get(&track.video_id).cloned())
                .filter(|path| path.is_file());
            if let Some(path) = downloaded {
                let event = match player::load(
                    &mpv,
                    &path.to_string_lossy(),
                    volume,
                    None,
                    normalise,
                    start_at,
                ) {
                    Ok(()) => Event::Playing(track, generation),
                    Err(e) => Event::PlaybackFailed {
                        track,
                        error: format!("{e:#}"),
                        generation,
                    },
                };
                if let Event::Playing(ref track, _) = event {
                    scrobble.maybe_start(track.clone(), Arc::clone(&mpv), generation);
                }
                Self::emit(&tx, &ctx, event);
                return;
            }
            // A local file bypasses the resolver entirely — mpv's `loadfile` takes a plain
            // filesystem path exactly as it takes a URL, and there is nothing to scrobble
            // (YouTube's watch-history endpoint has no idea what this path is).
            if let Some(path) = track.local_path.clone() {
                let event = match player::load(
                    &mpv,
                    &path.to_string_lossy(),
                    volume,
                    None,
                    normalise,
                    start_at,
                ) {
                    Ok(()) => Event::Playing(track, generation),
                    Err(e) => Event::PlaybackFailed {
                        track,
                        error: format!("{e:#}"),
                        generation,
                    },
                };
                Self::emit(&tx, &ctx, event);
                return;
            }
            // A single/album track can hand back a music-video id with no audio-only
            // alternative at all — that's just what YouTube's own album metadata carries for
            // it, and there's nothing to substitute. But when an audio match does exist, it's
            // what you actually meant by pressing play on an album track, so look for one and
            // resolve/scrobble it instead. `track` itself (row, title, thumbnail, album link)
            // stays exactly the album track throughout; only the id handed to the resolver
            // changes.
            let resolve_id = if track.wide {
                audio_alternative(&api, &track)
                    .await
                    .unwrap_or_else(|| track.video_id.clone())
            } else {
                track.video_id.clone()
            };
            let id = match VideoId::new(resolve_id) {
                Ok(id) => id,
                Err(e) => {
                    Self::emit(
                        &tx,
                        &ctx,
                        Event::PlaybackFailed {
                            track,
                            error: format!("{e:#}"),
                            generation,
                        },
                    );
                    return;
                }
            };

            let event = match resolver.resolve(&id).await {
                Ok(stream) => {
                    let url = playable_url(proxy.as_deref(), &id, &stream);
                    match player::load(
                        &mpv,
                        url.as_str(),
                        volume,
                        stream.loudness_db,
                        normalise,
                        start_at,
                    ) {
                        Ok(()) => Event::Playing(track, generation),
                        Err(e) => Event::PlaybackFailed {
                            track,
                            error: format!("{e:#}"),
                            generation,
                        },
                    }
                }
                Err(e) => {
                    warn!(video_id = %track.video_id, error = %format!("{e:#}"), "resolve failed");
                    Event::PlaybackFailed {
                        track,
                        error: format!("{e:#}"),
                        generation,
                    }
                }
            };
            if let Event::Playing(ref track, _) = event {
                scrobble.maybe_start(track.clone(), Arc::clone(&mpv), generation);
            }
            Self::emit(&tx, &ctx, event);
        });
        generation
    }

    /// Play a music video. Unlike `play`, mpv fetches the video itself through its yt-dlp hook —
    /// the resolver only returns audio-only streams. `track` is `wide`, so its id is the video's.
    ///
    /// Shares `play`'s generation counter — it races the exact same "what should mpv have loaded
    /// last" question `Event::Playing`'s staleness guard exists for (the video toggle can fire a
    /// `play_video` right as a new track's own `play` is still resolving).
    pub fn play_video(
        &self,
        track: Track,
        mpv: Arc<Mpv>,
        volume: f64,
        start_at: Option<f64>,
    ) -> u64 {
        let generation = self.scrobble_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let resolver = Arc::clone(&self.video_resolver);
        let scrobble = Scrobble {
            api: Arc::clone(&self.api),
            http: self.http.clone(),
            generation: Arc::clone(&self.scrobble_generation),
        };
        // The video path spends seconds inside yt-dlp before a frame exists; `Resolving` is
        // what tells the bar (and the video view's placeholder) to say so instead of looking
        // stuck on the previous track.
        Self::emit(&tx, &ctx, Event::Resolving(track.clone(), generation));
        self.rt.spawn(async move {
            // The cached direct URLs first — prefetched for the next track, or resolved earlier
            // in the session. mpv's yt-dlp hook stays the fallback: it re-extracts on every
            // load, which is exactly the seconds this path exists to avoid.
            let direct = match VideoId::new(track.video_id.clone()) {
                Ok(id) => resolver.resolve_video(&id).await,
                Err(e) => Err(e),
            };
            let load = match &direct {
                Ok(stream) => player::load_video(
                    &mpv,
                    stream.video.as_str(),
                    stream.audio.as_ref().map(|a| a.as_str()),
                    volume,
                    start_at,
                ),
                Err(e) => {
                    warn!(
                        video_id = %track.video_id,
                        error = %format!("{e:#}"),
                        "video resolve failed; falling back to mpv's yt-dlp hook"
                    );
                    player::load_video(
                        &mpv,
                        &crate::model::track_url(&track.video_id),
                        None,
                        volume,
                        start_at,
                    )
                }
            };
            let event = match load {
                Ok(()) => Event::Playing(track, generation),
                Err(e) => Event::PlaybackFailed {
                    track,
                    error: format!("{e:#}"),
                    generation,
                },
            };
            // A music video is a real YouTube play like any other; the tracker reads mpv's own
            // position, which is the same handle the video runs on.
            if let Event::Playing(ref track, _) = event {
                scrobble.maybe_start(track.clone(), Arc::clone(&mpv), generation);
            }
            Self::emit(&tx, &ctx, event);
        });
        generation
    }

    /// Warm the video resolver's cache for a track the queue is about to reach, so Next in the
    /// video view starts on the cached URLs instead of paying yt-dlp at that moment.
    pub fn prefetch_video(&self, track: Track) {
        let resolver = Arc::clone(&self.video_resolver);
        self.rt.spawn(async move {
            if let Ok(id) = VideoId::new(track.video_id) {
                let _ = resolver.resolve_video(&id).await;
            }
        });
    }

    /// Resolve a track without playing it, so the URL is cached before it is needed.
    ///
    /// With yt-dlp at ~2-3s per resolve this is what makes the *next* track start instantly, and
    /// it is a prerequisite for gapless: mpv needs the next URL before the current track ends.
    pub fn prefetch(&self, track: Track) {
        // On disk already: there is nothing to resolve, and with no yt-dlp that futile call is
        // the only thing it would log.
        if self
            .downloads
            .lock()
            .ok()
            .is_some_and(|guard| guard.contains_key(&track.video_id))
        {
            return;
        }
        let resolver = Arc::clone(&self.resolver);
        let proxy = self.proxy.clone();
        self.rt.spawn(async move {
            let Ok(id) = VideoId::new(track.video_id) else {
                return;
            };
            // Resolved, and for a ranged stream its first 256 KiB fetched too, so the switch
            // starts from memory over a connection that is already open.
            if let Ok(stream) = resolver.resolve(&id).await
                && stream.needs_ranged
                && let Some(proxy) = proxy
                && playable_url(Some(&proxy), &id, &stream) != stream.url
            {
                proxy.warm(&id).await;
            }
        });
    }

    // -- scrobble ----------------------------------------------------------------------------
    //
    // Fetching bytes from a stream URL is invisible to YouTube's accounting: what counts a play
    // on your account is beaconing the `player` response's tracking URLs. The playback URL fires
    // once per play (that GET writes the history item); the watchtime base gets the position
    // every ten seconds while the track runs, plus one last segment when it plays out.
    //
    // Everything is fire-and-forget off the UI thread, and every failure only logs: a refused
    // beacon must never disturb audio. Anonymous plays never get here — `playback_tracking`
    // is `None` with no session — and the settings toggle is read per track, so flipping it
    // takes effect on the next song, not mid-song.

    /// Fetch lyrics for `track`: LRCLIB's synced text first, InnerTube's plain text as fallback.
    /// With `lrclib` off (the Settings switch), InnerTube's alone.
    pub fn fetch_lyrics(&self, track: Track, lrclib: bool) {
        let (tx, ctx, http, api) = (
            self.tx.clone(),
            self.ctx.clone(),
            self.http.clone(),
            Arc::clone(&self.api),
        );
        self.rt.spawn(async move {
            let video_id = track.video_id.clone();
            let synced = if lrclib {
                self::lrclib(&http, &track).await
            } else {
                None
            };
            let lyrics = match synced {
                Some(lyrics) => Some(lyrics),
                None => api.lyrics(&track.video_id).await.unwrap_or(None),
            };
            let event = match lyrics {
                Some(lyrics) => Event::LyricsLoaded { video_id, lyrics },
                None => Event::LyricsFailed {
                    video_id,
                    error: "No lyrics found".to_owned(),
                },
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Fetch and decode a cover.
    ///
    /// `key` is the original URL, which is what the UI looks up; `url` is the size-rewritten one
    /// we actually request. Decoding and the dominant-colour pass both happen here, off the UI
    /// thread — only the texture upload has to happen on it.
    pub fn fetch_image(&self, key: Arc<str>, url: String, px: u32, wide: bool) {
        let (tx, ctx, http) = (self.tx.clone(), self.ctx.clone(), self.http.clone());
        self.rt.spawn(async move {
            let event = match fetch_bytes(&http, &url).await {
                Ok(bytes) => match images::decode(&bytes, px) {
                    Ok((image, dominant)) => Event::ImageReady(Box::new(images::Loaded {
                        url: key,
                        px,
                        wide,
                        image,
                        dominant,
                    })),
                    Err(e) => {
                        debug!(%url, error = %e, "cover decode failed");
                        Event::ImageFailed(key, px, wide)
                    }
                },
                Err(e) => {
                    debug!(%url, error = %e, "cover fetch failed");
                    Event::ImageFailed(key, px, wide)
                }
            };
            Self::emit(&tx, &ctx, event);
        });
    }

    /// Take everything that finished since the last frame. Never blocks.
    pub fn drain(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Scrobble: reporting plays to the signed-in account's watch history.
// ---------------------------------------------------------------------------------------------

/// One watchtime ping per ten seconds, matching the web client's early cadence. Songs are
/// short; a sparser tail would leave most of a track unattributed.
const SCROBBLE_TICK_SECS: u64 = 10;
/// Twenty minutes of ticks. Past any song, so a stuck task dies instead of beaconing forever.
const SCROBBLE_MAX_TICKS: u32 = 120;

/// What one track's tracker carries. Cloned per play; the generation it was born with is
/// checked every tick against the live one.
#[derive(Clone)]
struct Scrobble {
    api: Arc<Api>,
    http: reqwest::Client,
    generation: Arc<AtomicU64>,
}

impl Scrobble {
    /// Fire-and-forget per play. Reads the toggle here so flipping it takes effect on the
    /// next track; the anonymous gate lives one layer down, in `playback_tracking`.
    fn maybe_start(&self, track: Track, mpv: Arc<Mpv>, generation: u64) {
        if !crate::settings::load().scrobble {
            return;
        }
        let scrobble = self.clone();
        tokio::spawn(async move { scrobble.run(track, mpv, generation).await });
    }

    async fn run(&self, track: Track, mpv: Arc<Mpv>, generation: u64) {
        let current = || self.generation.load(Ordering::SeqCst);
        let tracking = match self.api.playback_tracking(&track.video_id).await {
            Ok(tracking) => tracking,
            Err(e) => {
                debug!(video_id = %track.video_id, error = %format!("{e:#}"),
                    "player lookup for the scrobble failed");
                None
            }
        };
        let Some(tracking) = tracking else { return };
        match self.api.report_playback(&tracking.playback_url).await {
            Ok(()) => info!(video_id = %track.video_id, title = %track.title, "play reported"),
            Err(e) => debug!(video_id = %track.video_id, error = %format!("{e:#}"),
                "playback beacon refused"),
        }
        // The playback beacon rides the InnerTube client; the watchtime pings are plain GETs,
        // so they carry the cookie themselves. An OAuth session has no cookie to carry, and
        // keeps the playback beacon only.
        let Some(cookie) = self.api.session_cookie().await else {
            debug!("no cookie for the watchtime pings; playback beacon only");
            return;
        };
        let cpn = scrobble_nonce();
        let started = std::time::Instant::now();
        let mut seg_start = 0.0;
        for _ in 0..SCROBBLE_MAX_TICKS {
            tokio::time::sleep(std::time::Duration::from_secs(SCROBBLE_TICK_SECS)).await;
            if current() != generation {
                // Superseded — the new track's tracker owns mpv now. No final ping: mpv may
                // already be reading the next file, and its position must never be reported
                // against this video id.
                return;
            }
            let pos: f64 = mpv.get_property("time-pos").unwrap_or(seg_start);
            let dur: f64 = mpv.get_property("duration").unwrap_or(0.0);
            // A seek back starts a fresh segment; clamping would invent watched seconds.
            let (st, et) = if pos >= seg_start {
                (seg_start, pos)
            } else {
                (pos, pos)
            };
            let paused: bool = mpv.get_property("pause").unwrap_or(false);
            let tick = Tick {
                rt: started.elapsed().as_secs_f64(),
                st,
                et,
                paused,
                volume: mpv_volume(&mpv),
                len: dur,
            };
            send_watchtime(&self.http, &cookie, &tracking.watchtime_base, &cpn, &tick).await;
            seg_start = pos;
            if dur > 0.0 && pos >= dur - 2.0 {
                // Played out — mpv advances next, whose tracker takes over.
                return;
            }
        }
    }
}

/// mpv's volume property, which is what the site reports. Falls back to silence rather than
/// failing the ping: the position is the point, not the level.
fn mpv_volume(mpv: &Mpv) -> f64 {
    mpv.get_property("volume").unwrap_or(0.0)
}

/// One tick's playback progress, for the watchtime beacon: wall seconds since the track
/// started, the watched segment this tick covers (its end is the position), and the player
/// state the server attributes it with.
struct Tick {
    rt: f64,
    st: f64,
    et: f64,
    paused: bool,
    volume: f64,
    /// Duration, 0.0 when mpv does not know it yet.
    len: f64,
}

async fn send_watchtime(http: &reqwest::Client, cookie: &str, base: &str, cpn: &str, tick: &Tick) {
    let url = watchtime_url(base, cpn, tick);
    match http
        .get(&url)
        .header(reqwest::header::COOKIE, cookie.to_owned())
        .send()
        .await
    {
        Ok(response) => debug!(status = %response.status(), "watchtime ping"),
        Err(e) => debug!(error = %format!("{e:#}"), "watchtime ping failed"),
    }
}

/// This tick's progress appended to a `videostatsWatchtimeUrl` base. What YouTube baked in —
/// session, client, video — is left alone; the per-tick fields are always ours. `cpn` is added
/// once: a base that already carries one keeps it.
fn watchtime_url(base: &str, cpn: &str, tick: &Tick) -> String {
    let sep = if base.contains('?') { '&' } else { '?' };
    let state = if tick.paused { "paused" } else { "playing" };
    let mut url = format!(
        "{base}{sep}rt={:.3}&st={:.3}&et={:.3}&cmt={:.3}\
         &state={state}&volume={:.0}&len={:.3}",
        tick.rt, tick.st, tick.et, tick.et, tick.volume, tick.len
    );
    if !base.contains("cpn=") {
        url.push_str("&cpn=");
        url.push_str(cpn);
    }
    url
}

/// A `cpn`: 16 hex chars from time, pid and a counter. The server treats it as an opaque
/// session nonce — uniqueness per play is what matters, not the alphabet.
fn scrobble_nonce() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() ^ u64::from(d.subsec_nanos()))
        .unwrap_or(0x9e3779b9);
    let pid = u64::from(std::process::id());
    format!(
        "{:016x}",
        t.wrapping_mul(0x9e3779b97f4a7c15)
            .wrapping_add(n)
            .wrapping_add(pid.rotate_left(32))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// yt-dlp's progress lines, and only those, read as a percentage.
    #[test]
    fn download_progress_lines_parse() {
        assert_eq!(
            parse_progress("[download]  42.3% of 3.20MiB at 1.00MiB/s"),
            Some(42.3)
        );
        assert_eq!(
            parse_progress("[download] 100% of 3.20MiB in 00:03"),
            Some(100.0)
        );
        assert_eq!(parse_progress("[download] Destination: /x/y.webm"), None);
        assert_eq!(
            parse_progress("/home/user/Music/refryt/Creep [id].webm"),
            None
        );
    }

    /// LRC stamps: two-digit fractions are hundredths, three are milliseconds, and a malformed
    /// stamp is skipped rather than dropping the whole lyric set.
    #[test]
    fn lrc_times_parse_in_both_fraction_widths() {
        assert_eq!(lrc_time_ms("00:12.34"), Some(12_340));
        assert_eq!(lrc_time_ms("01:02.345"), Some(62_345));
        assert_eq!(lrc_time_ms("02:00"), Some(120_000));
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

    /// The tick fields land on the base, the session the base carries is untouched, and a base
    /// without a query still gets one.
    #[test]
    fn watchtime_url_appends_the_tick() {
        let url = watchtime_url(
            "https://www.youtube.com/api/stats/watchtime?ns=yt&docid=abc",
            "deadbeefdeadbeef",
            &Tick {
                rt: 20.0,
                st: 10.0,
                et: 19.5,
                paused: false,
                volume: 70.0,
                len: 213.0,
            },
        );
        assert!(
            url.starts_with(
                "https://www.youtube.com/api/stats/watchtime?ns=yt&docid=abc&rt=20.000"
            )
        );
        for piece in [
            "st=10.000",
            "et=19.500",
            "cmt=19.500",
            "state=playing",
            "volume=70",
            "len=213.000",
            "cpn=deadbeefdeadbeef",
        ] {
            assert!(url.contains(piece), "missing {piece} in {url}");
        }
    }

    /// A base that already carries a nonce keeps it — two `cpn` keys would leave which one
    /// counts up to server whim.
    #[test]
    fn watchtime_url_keeps_an_existing_cpn() {
        let url = watchtime_url(
            "https://example.com/watchtime?cpn=aaaabbbbccccdddd",
            "eeeeffffgggghhhh",
            &Tick {
                rt: 1.0,
                st: 0.0,
                et: 1.0,
                paused: true,
                volume: 0.0,
                len: 10.0,
            },
        );
        assert_eq!(url.matches("cpn=").count(), 1);
        assert!(url.contains("cpn=aaaabbbbccccdddd"));
        assert!(url.contains("state=paused"));
    }

    /// Fresh, 16 hex chars, every call — a reused nonce would merge two plays into one session.
    #[test]
    fn scrobble_nonces_are_unique_hex() {
        let a = scrobble_nonce();
        let b = scrobble_nonce();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
