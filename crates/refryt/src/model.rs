//! Domain types.
//!
//! These are ours, deliberately. `ytmapi-rs` types stay behind `api.rs` so that swapping the
//! auth mode, or the API crate itself, touches one file rather than every page.

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    /// Every artist on the track, each with its own browse id where the source linked it.
    ///
    /// Not one joined string: YouTube writes `"James Blake, Ludwig Göransson & Travis Scott"` as
    /// three separately-linked runs, and folding them together made only the first one clickable.
    pub artists: Vec<ArtistLink>,
    pub album: Option<String>,
    /// The album's browse id, when the source linked it. Only then is the album name clickable —
    /// a name without an id would be a dead link.
    pub album_id: Option<String>,
    /// Seconds. `None` when YouTube reported a duration we could not parse.
    pub duration: Option<u64>,
    /// Play count as YouTube shows it (`"12M plays"`). Not every source carries one.
    pub plays: Option<String>,
    pub explicit: bool,
    /// Your rating. Parsed where the row carries it (signed-in menus); `Indifferent` elsewhere,
    /// including every typed fallback — those responses never include it.
    pub like: LikeStatus,
    /// Whether the song is in your library, from the row's bookmark toggle. Only signed-in
    /// rows carry one.
    pub in_library: bool,
    /// The save/remove feedback tokens behind that toggle, as `Arc<str>` — cheap to clone in
    /// a long queue, like the thumbnail.
    pub library_add: Option<Arc<str>>,
    pub library_remove: Option<Arc<str>>,
    /// The playlist item id, from `playlistItemData.playlistSetVideoId`. Only playlist rows
    /// carry one, and only it makes "Remove from playlist" offerable.
    pub set_video_id: Option<Arc<str>>,
    /// Cover art URL, largest available.
    pub thumbnail: Option<Arc<str>>,
    /// A music video rather than an album-audio track: its still is 16:9, so cards draw it
    /// wide. Read off the response's own video kind — no `aspectRatio` field exists on any
    /// captured card, and the thumbnail URLs arrive square even for videos, so the kind is
    /// the only signal that survives.
    pub wide: bool,
    /// A chart rank (`"1"`), from the row's `customIndexColumn`. Only chart playlists carry
    /// one; everywhere else this is `None` and the row shows its position instead.
    pub rank: Option<String>,
    /// Which way the rank moved since the last chart. Unknown where the row named none.
    pub trend: ChartTrend,
    /// Set only for a track scanned off disk rather than fetched from YouTube — its playback
    /// source, and the thing that makes `video_id` a plain path string rather than a real
    /// YouTube id for this track. `None` for everything else in the app.
    pub local_path: Option<Arc<std::path::Path>>,
}

/// Your rating of a song, as the row's like button reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LikeStatus {
    #[default]
    Indifferent,
    Like,
    Dislike,
}

impl LikeStatus {
    /// The toggled state when its own thumb is pressed: pressing Like on a liked song
    /// un-likes it, as on the site.
    pub fn toggled(self, pressed: Self) -> Self {
        if self == pressed {
            Self::Indifferent
        } else {
            pressed
        }
    }
}

/// One artist credited on a track, and the page it leads to when the source linked it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistLink {
    pub name: String,
    /// `None` where the source gave a name but no id — a dead link is worse than plain text.
    pub id: Option<String>,
}

impl ArtistLink {
    pub fn plain(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: None,
        }
    }
}

/// Something to play, as YouTube handed it over: a video to start on and the playlist it belongs
/// to. Neither id is derivable — an artist's shuffle and radio playlists are not built from the
/// channel id — so they are only ever read from a response, never constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchTarget {
    pub video_id: Option<String>,
    pub playlist_id: String,
    pub params: Option<String>,
}

/// Where a play reports itself, from the `player` response's `playbackTracking`.
///
/// Fetching bytes from a stream URL is invisible to YouTube's accounting — what counts a play
/// on your account is beaconing these URLs: the playback URL once when the track starts, the
/// watchtime URL every few seconds with the position. Both arrive with the session baked in,
/// so they are only ever read from a response, never constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackTracking {
    /// Fired once per play. A GET here is what writes the history item.
    pub playback_url: String,
    /// The base to append `rt`/`st`/`et`/`cmt`/`state` to while the track runs.
    pub watchtime_base: String,
}

/// What an artist's own menu offers, where the response carried it.
///
/// Every field is optional because the same artist appears in several shapes: a library row
/// offers shuffle, mix and share; a home card adds subscribe and don't-recommend; a signed-out
/// page has no subscribe state at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtistActions {
    pub shuffle: Option<WatchTarget>,
    pub mix: Option<WatchTarget>,
    /// `None` when the source said nothing about it — which is not the same as "not subscribed".
    pub subscribed: Option<bool>,
    /// The `params` token the button carried for each direction. YouTube's subscribe and
    /// unsubscribe calls take the button's own token alongside the channel id, and the two
    /// directions carry different ones; missing is not fatal, it just falls back to the bare
    /// channel id the call used to send.
    pub subscribe_params: Option<Arc<str>>,
    pub unsubscribe_params: Option<Arc<str>>,
    /// The token that stops YouTube recommending this artist. One-way, so it is only offered
    /// where the response actually provided it.
    pub dont_recommend: Option<Arc<str>>,
}

/// A search hit that links to an artist page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
    pub subscribers: Option<String>,
    pub thumbnail: Option<Arc<str>>,
    pub actions: ArtistActions,
}

/// A search hit that links to an album page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumRef {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: Option<String>,
    /// `"Album"`, `"Single"` or `"EP"` when the source labels it. Used to split a full
    /// discography; search hits leave it empty.
    pub kind: Option<String>,
    pub thumbnail: Option<Arc<str>>,
}

/// Where a playlist lives: a real YouTube playlist round-tripping through its API, or a
/// client-only one backed by `local_playlists.rs`. A track can only ever be offered playlists
/// of its own source as a save target — YouTube's API cannot accept a local file, and a local
/// playlist has nowhere to send a YouTube video id either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaylistSource {
    #[default]
    YouTube,
    Local,
}

/// A playlist reference: library entries and mood playlists. Links to a playlist page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistRef {
    pub id: String,
    pub title: String,
    pub author: String,
    /// The owner's channel id, when the card linked it — so the rail and the page header can
    /// open their profile rather than showing a dead name.
    pub author_id: Option<String>,
    pub track_count: Option<String>,
    pub thumbnail: Option<Arc<str>>,
    /// Whether this is a playlist you can add songs to.
    ///
    /// Read from the card's own menu — an editable playlist offers `EDIT`, an auto one (Liked
    /// Music) and the "New playlist" tile do not. The alternative was comparing the card's
    /// author against the account name, which does not survive the layout: a library card reads
    /// `"IV • 13 tracks"` while a home card reads `"Playlist • YouTube Music"`, so the author is
    /// in a different field in each. Icons do not move and do not localise.
    pub editable: bool,
    pub source: PlaylistSource,
}

impl PlaylistRef {
    /// The line under a playlist's name: how much is in it when it is yours — or Liked Music,
    /// which has no maker — and who made it, with its size, when it is someone else's. What a
    /// saved list needs from a rail row is its creator, not just another count.
    pub fn detail(&self) -> String {
        if self.editable || is_liked_auto(&self.id) {
            self.track_count.clone().unwrap_or_default()
        } else {
            match (self.author.trim(), self.track_count.as_deref()) {
                ("", Some(count)) => count.to_owned(),
                (author, Some(count)) => format!("{author} • {count}"),
                (author, None) => author.to_owned(),
            }
        }
    }
}

impl Track {
    /// The credit line as one string, for the places that still want one — a window title, a
    /// log, the resolver.
    pub fn artist_line(&self) -> String {
        let mut out = String::new();
        for (i, a) in self.artists.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&a.name);
        }
        out
    }

    /// The first artist that carries an id — what "go to artist" means when there are several.
    pub fn primary_artist_id(&self) -> Option<&str> {
        self.artists.iter().find_map(|a| a.id.as_deref())
    }
}

/// One thing YouTube Music can show you, whatever kind it is.
///
/// Search results, an artist's shelves and the home feed are all lists of these, so one renderer
/// serves all three rather than three near-identical grid loops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Song(Track),
    /// A music video. Playable through the same resolver; its cover is a 16:9 still rather than
    /// square album art.
    Video(Track),
    Artist(ArtistRef),
    Album(AlbumRef),
    Playlist(PlaylistRef),
    /// A "Moods & genres" chip. Has no artwork — YouTube draws these as coloured buttons, and so
    /// do we.
    Mood(MoodRef),
}

impl Entry {
    /// The track to play, for the kinds that are playable.
    pub fn track(&self) -> Option<&Track> {
        match self {
            Entry::Song(t) | Entry::Video(t) => Some(t),
            _ => None,
        }
    }

    /// The track to mutate, for the kinds that are playable. Ratings arrive optimistically and
    /// must reach every copy at once, which is what this is for.
    pub fn track_mut(&mut self) -> Option<&mut Track> {
        match self {
            Entry::Song(t) | Entry::Video(t) => Some(t),
            _ => None,
        }
    }

    /// The word shown in the row's type badge.
    pub fn kind(&self) -> &'static str {
        match self {
            Entry::Song(_) => "Song",
            Entry::Video(_) => "Video",
            Entry::Artist(_) => "Artist",
            Entry::Album(_) => "Album",
            Entry::Playlist(_) => "Playlist",
            Entry::Mood(_) => "Mood",
        }
    }

    pub fn thumbnail(&self) -> Option<&Arc<str>> {
        match self {
            Entry::Song(t) | Entry::Video(t) => t.thumbnail.as_ref(),
            Entry::Artist(a) => a.thumbnail.as_ref(),
            Entry::Album(a) => a.thumbnail.as_ref(),
            Entry::Playlist(p) => p.thumbnail.as_ref(),
            Entry::Mood(_) => None,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Entry::Song(t) | Entry::Video(t) => &t.title,
            Entry::Artist(a) => &a.name,
            Entry::Album(a) => &a.title,
            Entry::Playlist(p) => &p.title,
            Entry::Mood(m) => &m.title,
        }
    }

    /// The line under the title, already assembled for display.
    pub fn subtitle(&self) -> String {
        match self {
            Entry::Song(t) | Entry::Video(t) => t.artist_line(),
            Entry::Artist(a) => a.subscribers.clone().unwrap_or_default(),
            Entry::Album(a) => match &a.year {
                Some(y) if !a.artist.is_empty() => format!("{} • {y}", a.artist),
                Some(y) => y.clone(),
                None => a.artist.clone(),
            },
            Entry::Playlist(p) => p.detail(),
            Entry::Mood(_) => String::new(),
        }
    }

    /// Artists are people and are drawn as circles; everything else is a square cover.
    pub fn is_artist(&self) -> bool {
        matches!(self, Entry::Artist(_))
    }
}

/// Which kind of result the search page is showing.
///
/// `All` is a client-side view over the one ranked, cross-type search response — `search.rs`'s
/// own doc comment explains why that response is never split up for its own sake
/// (`travis_means_travis_scott` is the regression test guarding it). Every other scope is the
/// opposite: a dedicated, category-specific request (`Api::search_scoped`), because the ranked
/// response was found to carry far too few of a given kind to be worth merely filtering —
/// searching an artist by name can carry a dozen real matches server-side while the ranked
/// response mentions one or two in passing. Living here rather than in `app.rs` (where
/// `LibrarySort`/`PlaylistSort`, its nearest siblings, do) is what lets `api.rs` depend on it
/// without depending on the app layer above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchScope {
    #[default]
    All,
    Songs,
    Videos,
    Artists,
    Albums,
    Playlists,
}

impl SearchScope {
    pub const ALL: [SearchScope; 6] = [
        SearchScope::All,
        SearchScope::Songs,
        SearchScope::Videos,
        SearchScope::Artists,
        SearchScope::Albums,
        SearchScope::Playlists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SearchScope::All => "All",
            SearchScope::Songs => "Songs",
            SearchScope::Videos => "Videos",
            SearchScope::Artists => "Artists",
            SearchScope::Albums => "Albums",
            SearchScope::Playlists => "Playlists",
        }
    }

    /// Whether `entry` belongs in this scope — only meaningful for `All`'s own client-side
    /// filter over the ranked response; the other scopes get their entries from a request that
    /// only ever returns their own kind in the first place.
    pub fn matches(self, entry: &Entry) -> bool {
        match self {
            SearchScope::All => true,
            SearchScope::Songs => matches!(entry, Entry::Song(_)),
            SearchScope::Videos => matches!(entry, Entry::Video(_)),
            SearchScope::Artists => matches!(entry, Entry::Artist(_)),
            SearchScope::Albums => matches!(entry, Entry::Album(_)),
            SearchScope::Playlists => matches!(entry, Entry::Playlist(_)),
        }
    }
}

/// A titled row of entries: an artist page's "Singles & EPs", a home feed's "New releases".
///
/// Keeping YouTube's own shelf titles is what makes an artist page look the way it should. The
/// parser used to append every carousel into one flat list, which is why albums and singles arrived
/// indistinguishable and five other shelves were lost entirely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shelf {
    pub title: String,
    /// The small line YouTube puts above a shelf's title — "Similar to" over an artist's name,
    /// the account's own name over "Listen again". Without it "Bad Bunny" reads as that artist's
    /// own shelf rather than artists like them.
    pub strapline: Option<String>,
    pub items: Vec<Entry>,
    /// Opaque params for fetching the full list, when YouTube offers a "more" button.
    pub more_params: Option<String>,
    /// The browse id that `more_params` was actually paired with — a shelf's own "more" button
    /// frequently points somewhere other than the artist's own channel id (a discography page,
    /// or even a playlist id for "Videos"). `None` for shelves the typed API fallback built,
    /// which exposes no such id; the caller falls back to the artist's id there, as before.
    pub more_browse_id: Option<String>,
    /// Whether "Show all" has already replaced `items` with the full list. Persists per shelf,
    /// independent of which shelf was expanded most recently.
    pub expanded: bool,
}

/// A kind of section on Home that the listener can place: the named ones YouTube serves, the
/// shelves with a headline over their title, and "everything else" for the rest of the feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HomeSection {
    ListenAgain,
    QuickPicks,
    Discover,
    MixedForYou,
    FromYourLibrary,
    Trending,
    NewReleases,
    AlbumsForYou,
    ForgottenFavorites,
    FeaturedPlaylists,
    MusicVideos,
    CoversAndRemixes,
    LongListens,
    LivePerformances,
    FromTheCommunity,
    SimilarTo,
    Themed,
    Everything,
}

impl HomeSection {
    /// The order a fresh install uses.
    pub const DEFAULT: [HomeSection; 18] = [
        HomeSection::ListenAgain,
        HomeSection::QuickPicks,
        HomeSection::Discover,
        HomeSection::MixedForYou,
        HomeSection::FromYourLibrary,
        HomeSection::Trending,
        HomeSection::NewReleases,
        HomeSection::AlbumsForYou,
        HomeSection::ForgottenFavorites,
        HomeSection::FeaturedPlaylists,
        HomeSection::MusicVideos,
        HomeSection::CoversAndRemixes,
        HomeSection::LongListens,
        HomeSection::LivePerformances,
        HomeSection::FromTheCommunity,
        HomeSection::SimilarTo,
        HomeSection::Themed,
        HomeSection::Everything,
    ];

    /// What is saved in `settings.json`: stable text, so renaming a variant cannot break a file.
    pub fn key(self) -> &'static str {
        match self {
            HomeSection::ListenAgain => "listen-again",
            HomeSection::QuickPicks => "quick-picks",
            HomeSection::Discover => "discover",
            HomeSection::MixedForYou => "mixed-for-you",
            HomeSection::FromYourLibrary => "from-your-library",
            HomeSection::Trending => "trending",
            HomeSection::NewReleases => "new-releases",
            HomeSection::AlbumsForYou => "albums-for-you",
            HomeSection::ForgottenFavorites => "forgotten-favorites",
            HomeSection::FeaturedPlaylists => "featured-playlists",
            HomeSection::MusicVideos => "music-videos",
            HomeSection::CoversAndRemixes => "covers-and-remixes",
            HomeSection::LongListens => "long-listens",
            HomeSection::LivePerformances => "live-performances",
            HomeSection::FromTheCommunity => "from-the-community",
            HomeSection::SimilarTo => "similar-to",
            HomeSection::Themed => "themed",
            HomeSection::Everything => "everything-else",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::DEFAULT.into_iter().find(|s| s.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            HomeSection::ListenAgain => "Listen again",
            HomeSection::QuickPicks => "Quick picks",
            HomeSection::Discover => "Daily discover",
            HomeSection::MixedForYou => "Mixed for you",
            HomeSection::FromYourLibrary => "From your library",
            HomeSection::Trending => "Trending songs",
            HomeSection::NewReleases => "New releases",
            HomeSection::AlbumsForYou => "Albums for you",
            HomeSection::ForgottenFavorites => "Forgotten favorites",
            HomeSection::FeaturedPlaylists => "Featured playlists",
            HomeSection::MusicVideos => "Music videos",
            HomeSection::CoversAndRemixes => "Covers and remixes",
            HomeSection::LongListens => "Long listens",
            HomeSection::LivePerformances => "Live performances",
            HomeSection::FromTheCommunity => "From the community",
            HomeSection::SimilarTo => "Similar to an artist",
            HomeSection::Themed => "Themed picks",
            HomeSection::Everything => "Everything else",
        }
    }

    /// A line under the name where the name alone would not say what lands there.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            HomeSection::Discover => Some("YouTube calls it \"Your daily discover\""),
            HomeSection::Trending => Some("\"Trending songs for you\""),
            HomeSection::ForgottenFavorites => Some("Also \"Fresh finds, old favorites\""),
            HomeSection::SimilarTo => Some("Shelves headed \"Similar to\" an artist"),
            HomeSection::Themed => {
                Some("Shelves with a headline above them, like \"Throwback to…\"")
            }
            HomeSection::Everything => Some("Genre shelves, and anything new or unrecognised"),
            _ => None,
        }
    }
}

/// The saved keys as a complete order: unknown keys and repeats dropped, and any section the list
/// lacks added in the default order just before "everything else" (or at the end if that is gone),
/// so a file from an older build still gives every section a place and the named ones still come
/// before the unrecognised.
pub fn normalise_order(saved: &[String]) -> Vec<HomeSection> {
    let mut out: Vec<HomeSection> = Vec::with_capacity(HomeSection::DEFAULT.len());
    for key in saved {
        if let Some(section) = HomeSection::from_key(key)
            && !out.contains(&section)
        {
            out.push(section);
        }
    }
    for section in HomeSection::DEFAULT {
        if !out.contains(&section) {
            let at = out
                .iter()
                .position(|s| *s == HomeSection::Everything)
                .unwrap_or(out.len());
            out.insert(at, section);
        }
    }
    out
}

/// The default order as saved keys.
pub fn default_order_keys() -> Vec<String> {
    HomeSection::DEFAULT
        .iter()
        .map(|s| s.key().to_owned())
        .collect()
}

/// How many of the listener's top-ranked sections Strict Home order waits to see.
pub const STRICT_HOME_SECTIONS: usize = 7;
/// The longest Strict Home order spends fetching, counted from when the request starts. Past it
/// the shelves in hand are sorted and shown, and the rest of the feed loads on scroll.
pub const STRICT_HOME_BUDGET: std::time::Duration = std::time::Duration::from_secs(6);
/// The most extra pages Strict Home order follows, whatever the clock says.
pub const STRICT_HOME_PAGES: usize = 12;
/// The extra pages the ordinary load follows. The feed does not end; this is the point at which
/// more costs a request and buys nothing anyone will scroll to.
pub const QUICK_HOME_PAGES: usize = 3;

/// How far one Home load follows the feed's continuations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeFetch {
    /// A few pages, as it always was.
    Quick,
    /// Pages until every section in `wanted` has appeared, the feed ends, or the `budget` or the
    /// `max_pages` cap is spent — whichever comes first.
    Strict {
        wanted: Vec<HomeSection>,
        budget: std::time::Duration,
        max_pages: usize,
    },
}

impl HomeFetch {
    /// The strict plan for a listener's order: wait for their top [`STRICT_HOME_SECTIONS`].
    pub fn strict(order: &[HomeSection]) -> Self {
        Self::Strict {
            wanted: wanted_sections(order, STRICT_HOME_SECTIONS),
            budget: STRICT_HOME_BUDGET,
            max_pages: STRICT_HOME_PAGES,
        }
    }
}

/// Why a Home load stopped following the feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeStop {
    /// YouTube had no more pages.
    FeedEnded,
    /// The ordinary load's few pages were done.
    Quick,
    /// Every wanted section has appeared.
    Enough,
    /// The time budget was spent.
    Budget,
    /// The page cap was reached.
    PageCap,
}

/// The first `n` sections of `order` a strict load waits for, leaving out "everything else": it is
/// the catch-all, so something always lands there and waiting for it would mean nothing.
pub fn wanted_sections(order: &[HomeSection], n: usize) -> Vec<HomeSection> {
    order
        .iter()
        .copied()
        .filter(|s| *s != HomeSection::Everything)
        .take(n)
        .collect()
}

/// Whether a load that has followed `pages_done` extra pages in `elapsed` should stop, and why;
/// `None` means follow the next one. Pure, so the whole rule is testable without a clock or a
/// network. The caller handles a feed with no next page, which is [`HomeStop::FeedEnded`].
pub fn home_fetch_stop(
    fetch: &HomeFetch,
    pages_done: usize,
    seen: &[HomeSection],
    elapsed: std::time::Duration,
) -> Option<HomeStop> {
    match fetch {
        HomeFetch::Quick => (pages_done >= QUICK_HOME_PAGES).then_some(HomeStop::Quick),
        HomeFetch::Strict {
            wanted,
            budget,
            max_pages,
        } => {
            if wanted.iter().all(|w| seen.contains(w)) {
                Some(HomeStop::Enough)
            } else if elapsed >= *budget {
                Some(HomeStop::Budget)
            } else if pages_done >= *max_pages {
                Some(HomeStop::PageCap)
            } else {
                None
            }
        }
    }
}

/// Arrange shelves by `order`. Stable: shelves in the same section keep the order YouTube sent
/// them in.
pub fn order_home(shelves: &mut [Shelf], order: &[HomeSection]) {
    shelves.sort_by_key(|shelf| {
        let section = shelf.home_section();
        order
            .iter()
            .position(|s| *s == section)
            .unwrap_or(order.len())
    });
}

impl Shelf {
    /// Whether this shelf is a list of playable songs rather than a grid of cards.
    ///
    /// "Top songs" and "Quick picks" are lists; "Albums", "New releases" and "Videos" are
    /// grids — a video is a `Track` too (so it can play), but its 16:9 art wants the same
    /// thumbnail-forward card layout as everything else with a cover, not a text row.
    pub fn is_track_list(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|i| matches!(i, Entry::Song(_)))
    }

    /// Whether this shelf is "Moods & genres", which is chips rather than cards.
    pub fn is_mood_list(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|i| matches!(i, Entry::Mood(_)))
    }

    /// Which of Home's orderable sections this shelf belongs to. Matched on the English title (and
    /// the "Similar to" line over an artist's name); a shelf that fits none, which is every
    /// personal or themed one, is [`HomeSection::Everything`].
    pub fn home_section(&self) -> HomeSection {
        let title = self.title.trim().to_lowercase();
        let strapline = self
            .strapline
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        match title.as_str() {
            "listen again" => HomeSection::ListenAgain,
            "quick picks" => HomeSection::QuickPicks,
            "your daily discover" | "daily discover" | "discover mix" | "discover mixes" => {
                HomeSection::Discover
            }
            "mixed for you" => HomeSection::MixedForYou,
            "from your library" => HomeSection::FromYourLibrary,
            t if t.starts_with("trending") => HomeSection::Trending,
            "new releases" | "new albums & singles" | "new albums and singles" => {
                HomeSection::NewReleases
            }
            "albums for you" => HomeSection::AlbumsForYou,
            "forgotten favorites" | "forgotten favourites" | "fresh finds, old favorites" => {
                HomeSection::ForgottenFavorites
            }
            t if t.starts_with("featured playlists") => HomeSection::FeaturedPlaylists,
            t if t.starts_with("music videos") => HomeSection::MusicVideos,
            "covers and remixes" => HomeSection::CoversAndRemixes,
            "long listens" => HomeSection::LongListens,
            "live performances" => HomeSection::LivePerformances,
            "from the community" => HomeSection::FromTheCommunity,
            _ if strapline.starts_with("similar to") => HomeSection::SimilarTo,
            // A headline over the title with nothing named above: the themed shelves, whose
            // titles change from day to day but which all carry one.
            _ if !strapline.is_empty() => HomeSection::Themed,
            _ => HomeSection::Everything,
        }
    }

    /// The playable tracks in this shelf, for starting a queue from one of them.
    pub fn tracks(&self) -> Vec<Track> {
        self.items
            .iter()
            .filter_map(|i| i.track().cloned())
            .collect()
    }

    /// Where `items[index]` sits among [`Self::tracks`].
    pub fn track_index(&self, index: usize) -> Option<usize> {
        self.items.get(index)?.track()?;
        Some(
            self.items[..index]
                .iter()
                .filter(|i| i.track().is_some())
                .count(),
        )
    }
}

/// Keep an expanded shelf to the kinds its own preview already showed.
///
/// A shelf's "Show all" can answer with a whole artist page — the captured "From your library"
/// expansion did — and flattening every renderer then mixed albums and playlists into a shelf
/// that was songs. The preview names the kinds this shelf is actually made of; everything else
/// belongs to another shelf. Never empties a shelf: when nothing in the expansion matches, the
/// unfiltered list stands rather than the shelf silently vanishing.
pub fn shelf_expansion(items: Vec<Entry>, preview: &[Entry]) -> Vec<Entry> {
    let wanted: std::collections::HashSet<&str> = preview.iter().map(Entry::kind).collect();
    if wanted.is_empty() || !items.iter().any(|e| wanted.contains(e.kind())) {
        return items;
    }
    items
        .into_iter()
        .filter(|e| wanted.contains(e.kind()))
        .collect()
}

/// A search, in YouTube's own relevance order.
///
/// One ranked list rather than three parallel ones: the unfiltered response ranks every type
/// against every other, and splitting it into Songs / Artists / Albums shelves throws that away —
/// a highly relevant artist would sit below every song.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub items: Vec<Entry>,
    /// YouTube answered "No results for …" itself: an empty list that is the real answer, not a
    /// parse that found nothing.
    pub no_results: bool,
    /// The spelling YouTube offers ("Did you mean: …"), with an empty answer or with a few loose
    /// matches for text it could not place.
    pub did_you_mean: Option<String>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The playable entries, in order, paired with their index in `items`.
    ///
    /// Starting a song from search queues the other playable results and nothing else, so pressing
    /// next does not stop at the first artist row.
    pub fn playable(&self) -> Vec<Track> {
        self.items
            .iter()
            .filter_map(|i| i.track().cloned())
            .collect()
    }

    /// Where `items[index]` sits among [`Self::playable`].
    pub fn playable_index(&self, index: usize) -> Option<usize> {
        self.items.get(index)?.track()?;
        Some(
            self.items[..index]
                .iter()
                .filter(|i| i.track().is_some())
                .count(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artist {
    pub id: String,
    pub name: String,
    /// The short count from `subscriberCountText` ("20.8M") — what fits on the Subscribe
    /// button. It carries no unit on purpose; the long form is [`Self::subscribers_long`].
    pub subscribers: Option<String>,
    /// `"20.8M subscribers"`, complete and localised by YouTube. The header shows this under
    /// the name when the artist has no monthly-audience line (a small artist), so the page
    /// never prints the unitless button count as a sentence.
    pub subscribers_long: Option<String>,
    /// `"213M monthly audience"`, complete from YouTube. This is what the page shows under the
    /// name; the subscriber count belongs on the Subscribe button and the view total on the
    /// About section, which is where they were double-billed before.
    pub monthly_listeners: Option<String>,
    pub views: Option<String>,
    pub actions: ArtistActions,
    pub description: Option<String>,
    pub thumbnail: Option<Arc<str>>,
    /// The wide channel-art banner (`musicImmersiveHeaderRenderer`'s own thumbnail list, ~2.4:1)
    /// that the artist page's full-bleed hero draws. `None` on a page that only carries a square
    /// portrait — a channel's visual header — and on the typed fallback, where the hero slot
    /// falls back to the circle-art header this page has always had.
    pub hero: Option<Arc<str>>,
    pub top_songs: Vec<Track>,
    /// YouTube's own title for that shelf. "Top songs" on every capture so far, but the artist
    /// page draws whatever the response called it rather than inventing a heading of its own.
    /// `None` on the typed fallback, which has no shelf to read one from.
    pub top_songs_title: Option<String>,
    /// The id of the "Top songs" shelf's own auto-generated playlist — its "Show all" target.
    /// YouTube embeds only 5 songs directly in the artist page; the rest live here, one playlist
    /// fetch away, the same way any other playlist opens.
    pub top_songs_playlist: Option<String>,
    /// The page's shelves, with YouTube's own titles and order: Albums, Singles & EPs, Videos,
    /// Live performances, Featured on, Playlists, Fans might also like. Flattening these into one
    /// list is what made albums and singles indistinguishable and lost the rest.
    ///
    /// `Arc`-wrapped like every sibling feed (`home`/`explore`/`releases`/`moods`/
    /// `history_shelves`) so a per-frame snapshot is a refcount bump, not a deep clone.
    pub shelves: Arc<Vec<Shelf>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub title: String,
    /// Every credited artist joined for display. Kept beside [`Self::artists`] because most
    /// callers want the string; the header wants the individual links.
    pub artist: String,
    pub artist_id: Option<String>,
    /// The credits with their own ids, so a multi-artist album header links each of them.
    pub artists: Vec<ArtistLink>,
    pub year: Option<String>,
    /// `"Single"`, `"Album"` or `"EP"`, as YouTube labels it.
    pub kind: String,
    pub description: Option<String>,
    pub thumbnail: Option<Arc<str>>,
    /// Total duration verbatim (`"32 min"`). Stored raw: only `m:ss` strings parse, and a
    /// missing duration must not hide the rest of the header.
    pub duration_text: Option<String>,
    /// The header toggle's state, read off the page. The flip rides the like endpoint, not
    /// a feedback token — album headers carry none — so this travels with the like-playlist
    /// id below rather than add/remove tokens.
    pub in_library: bool,
    /// The play button's playlist id, which is also the like target that saves the album.
    /// Absent where the page gave no play button; the header save control hides itself then.
    pub like_id: Option<String>,
    pub tracks: Vec<Track>,
}

/// A full playlist: header plus playable tracks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub author: String,
    /// The owner's channel id, from the header facepile's tap target. `None` when the page
    /// gave no link (the typed fallback never does).
    pub author_id: Option<String>,
    pub year: Option<String>,
    pub duration_text: Option<String>,
    pub track_count: Option<String>,
    pub thumbnail: Option<Arc<str>>,
    /// Whether this is your own playlist. Read from the page header — owned playlists render
    /// `musicEditablePlaylistDetailHeaderRenderer` — and the only thing that enables
    /// "Remove from playlist". Conservative by design: the typed fallback cannot tell, so it
    /// says false and the button stays hidden rather than risking a write.
    pub editable: bool,
    /// The header's own library toggle, like [`Album::in_library`]. Playlist headers rarely
    /// carry a bookmark button (neither Liked Music nor an owned list did when checked), so
    /// this is usually `None` and the header save control stays hidden.
    pub in_library: bool,
    pub library_add: Option<Arc<str>>,
    pub library_remove: Option<Arc<str>>,
    /// Whether the page actually offered a save control — the header's bookmark toggle.
    ///
    /// Set by the raw parser when it found one; a public playlist has it (saving rides the like
    /// endpoint, not a feedback token) while your own header does not. The typed fallback cannot
    /// tell, so it says false and the control stays hidden rather than writing blind.
    pub saveable: bool,
    /// Paged: a long playlist arrives 100 tracks at a time. "Liked music" is 5,042.
    pub tracks: Paged<Track>,
    pub source: PlaylistSource,
    /// The token for what YouTube shows below the tracks — the Suggestions on your own
    /// playlists. Never more tracks: those page through [`Self::tracks`].
    pub extras: Option<String>,
}

impl Playlist {
    /// YouTube's own auto-generated mixes/radios ("My Mix", "Supermix", and the rest) open
    /// through the same `Route::Playlist`/`Api::playlist` path as a real playlist, but their id
    /// wraps a radio id — `VLRDCLAK5uy_…` rather than a real `VLPL…`/`VLOLAK…` playlist id — and
    /// unlike a real playlist they are an unbounded algorithmic river with no fixed track list to
    /// sort.
    pub fn is_expanding(&self) -> bool {
        is_expanding_playlist_id(&self.id)
    }
}

/// `Playlist::is_expanding`'s own check, free of the struct so it's testable without building one.
/// `bare_playlist_id` already strips the browse-only `VL` prefix this needs.
fn is_expanding_playlist_id(id: &str) -> bool {
    bare_playlist_id(id).starts_with("RD")
}

/// One of YouTube's radio mixes — "My Mix", "Discover Mix", a song's or a playlist's radio —
/// which the site plays through its radio, with Up next's filters. Its curated `RDCLAK`
/// playlists are expanding ids too, but they are fixed lists that come with no filters, so they
/// play as a playlist does.
pub fn is_radio_mix_id(id: &str) -> bool {
    is_expanding_playlist_id(id) && !bare_playlist_id(id).starts_with("RDCLAK")
}

/// Lyrics for a track: either time-synced or plain text.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    /// Whether `at_ms` is meaningful. Plain lyrics carry every line at zero.
    pub synced: bool,
    /// Where they came from, shown above the text.
    pub source: String,
}

/// One lyric line and, when `synced`, the millisecond it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricLine {
    pub at_ms: u64,
    pub text: String,
}

/// A playlist id without its browse `VL` prefix. Browses want `VL{id}`; the edit endpoint
/// wants the bare id, and the library hands out the prefixed form — so writes strip it.
pub fn bare_playlist_id(id: &str) -> &str {
    id.strip_prefix("VL").unwrap_or(id)
}

/// The public YouTube Music URL for a track. One source of truth for the Share menu, the
/// player bar and the page menus.
pub fn track_url(video_id: &str) -> String {
    format!("https://music.youtube.com/watch?v={video_id}")
}

/// The public YouTube Music URL for an album.
pub fn album_url(album_id: &str) -> String {
    format!("https://music.youtube.com/browse/{album_id}")
}

/// The public YouTube Music URL for a playlist. Accepts the prefixed (`VL…`) or bare id.
pub fn playlist_url(playlist_id: &str) -> String {
    format!(
        "https://music.youtube.com/playlist?list={}",
        bare_playlist_id(playlist_id)
    )
}

/// The public YouTube Music URL for an artist's channel.
pub fn artist_url(artist_id: &str) -> String {
    format!("https://music.youtube.com/channel/{artist_id}")
}

/// Whether an id is the Liked-Music auto playlist, which no edit endpoint accepts.
pub fn is_liked_auto(id: &str) -> bool {
    bare_playlist_id(id) == "LM"
}

/// Whether an id is the "Episodes for later" auto playlist — the personal `SE` list, which is
/// podcasts wearing a playlist's id. The app has no episode page and no podcast playback, so it
/// is dropped wherever playlists are parsed rather than listed unopenable, the same way Liked
/// Music is special-cased where an edit endpoint would refuse it.
pub fn is_episodes_for_later(id: &str) -> bool {
    bare_playlist_id(id) == "SE"
}

/// A list that arrives a page at a time.
///
/// The cursor lives with the items so every place that shows a list has to acknowledge that there
/// may be more — which is the thing the library page silently got wrong, presenting the first 25
/// of 689 songs as the whole of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paged<T> {
    pub items: Vec<T>,
    /// Token for the next page; `None` once the list is complete.
    pub next: Option<String>,
    /// A page is in flight, so the view must not ask for another.
    pub loading: bool,
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next: None,
            loading: false,
        }
    }
}

/// Read like the `Vec` it wraps, so a paged list is a slice everywhere it is only being shown.
impl<T> std::ops::Deref for Paged<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl<T> Paged<T> {
    /// A first page.
    pub fn new(items: Vec<T>, next: Option<String>) -> Self {
        Self {
            items,
            next,
            loading: false,
        }
    }

    /// Whether asking for more would do anything.
    pub fn can_load_more(&self) -> bool {
        self.next.is_some() && !self.loading
    }

    pub fn extend(&mut self, items: Vec<T>, next: Option<String>) {
        self.items.extend(items);
        self.next = next;
        self.loading = false;
    }
}

/// The signed-in user's library, in four sections, each paged independently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    pub songs: Paged<Track>,
    pub artists: Paged<ArtistRef>,
    pub albums: Paged<AlbumRef>,
    pub playlists: Paged<PlaylistRef>,
}

/// A channel the signed-in Google account can browse as.
///
/// The personal channel has no `page_id`; brand accounts do, and that id is what selects one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub detail: String,
    pub page_id: Option<String>,
    /// The channel's `UC…` browse id, for opening its profile. Derived from the account list's
    /// `offlineCacheKeyToken` (the id without its `UC` prefix) or a `UC…` page id. `None` when
    /// the Google account has no channel.
    pub channel_id: Option<String>,
    /// The `@handle`, when YouTube sent one.
    pub handle: Option<String>,
    pub selected: bool,
    pub thumbnail: Option<Arc<str>>,
}

/// The buttons across the top of Explore — New releases, Charts, Moods & genres — read off
/// the feed itself, so their browse ids and params never go stale in code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExploreLink {
    pub title: String,
    pub browse_id: String,
    pub params: Option<String>,
}

/// The Explore landing: shelves plus the buttons that lead off it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExploreFeed {
    pub shelves: Vec<Shelf>,
    pub links: Vec<ExploreLink>,
}

/// One ranked row on a chart: the entry plus its rank and movement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartRow {
    pub entry: Entry,
    /// The rank as YouTube writes it (`"1"`). Empty where the row named none.
    pub rank: String,
    pub trend: ChartTrend,
}

/// Which way a chart row moved since the last chart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChartTrend {
    Up,
    Down,
    Steady,
    #[default]
    Unknown,
}

/// One ranked chart — Top songs, Trending — plus the card shelves that share its page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartSection {
    pub title: String,
    pub rows: Vec<ChartRow>,
}

/// The Charts page: ranked sections first, then ordinary card shelves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChartsFeed {
    pub ranked: Vec<ChartSection>,
    pub shelves: Vec<Shelf>,
}
/// One "Moods & genres" chip. `params` is opaque; it goes back into `get_mood_playlists`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoodRef {
    pub title: String,
    pub params: String,
    /// The chip's left stripe, from the card's `solid.leftStripeColor` (ARGB on the wire, RGB
    /// here). `None` where the source carries none — the typed mood categories do not.
    pub colour: Option<[u8; 3]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoodSection {
    pub name: String,
    pub moods: Vec<MoodRef>,
}

/// The playlists behind one mood chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoodPlaylists {
    pub title: String,
    pub playlists: Vec<PlaylistRef>,
}

/// One chip of the home feed's mood bar (Workout, Relax, …): the same feed browsed with
/// different params. Not an [`Entry::Mood`] — those open a genre page, this filters the feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeChip {
    pub title: String,
    /// Opaque browse params. `None` selected means the default feed.
    pub params: String,
    /// Whether the feed arrived with this filter applied.
    pub selected: bool,
}

/// The home feed: its shelves and its mood-bar chips, from one browse.
#[derive(Clone, Debug, Default)]
pub struct HomeFeed {
    pub shelves: Vec<Shelf>,
    pub chips: Vec<HomeChip>,
    /// Where the feed continues: the rest is loaded as the listener scrolls toward the end.
    /// `None` once YouTube has sent it all.
    pub next: Option<String>,
}

/// Parse YouTube Music's `"m:ss"` / `"h:mm:ss"` duration strings.
pub fn parse_duration(s: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut any = false;
    for part in s.trim().split(':') {
        let n: u64 = part.trim().parse().ok()?;
        total = total.checked_mul(60)?.checked_add(n)?;
        any = true;
    }
    any.then_some(total)
}

/// Fold "styled" letters back to plain ones: 𝐓𝐢𝐦𝐞𝐥𝐞𝐬𝐬 → Timeless.
///
/// Channels write titles in Unicode's Mathematical Alphanumeric Symbols (U+1D400–U+1D7FF) for
/// bold or script looks. No font we load covers that block, so they drew as a row of boxes, and
/// each box measured narrower than it painted, so the title ran out of its card. Compatibility
/// normalisation (NFKC) maps each to the letter or digit it styles. Only that block is touched —
/// NFKC over whole titles would also rewrite full-width letters, superscripts and ™ — and a
/// title without any comes back borrowed, untouched.
pub fn fold_styled_letters(text: &str) -> std::borrow::Cow<'_, str> {
    let styled = |c: char| ('\u{1D400}'..='\u{1D7FF}').contains(&c);
    if !text.chars().any(styled) {
        return std::borrow::Cow::Borrowed(text);
    }
    let nfkc = icu_normalizer::ComposingNormalizerBorrowed::new_nfkc();
    let mut out = String::with_capacity(text.len());
    let mut buf = [0u8; 4];
    for c in text.chars() {
        if styled(c) {
            out.push_str(&nfkc.normalize(c.encode_utf8(&mut buf)));
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(title: &str, strapline: Option<&str>) -> Shelf {
        Shelf {
            title: title.to_owned(),
            strapline: strapline.map(str::to_owned),
            items: Vec::new(),
            more_params: None,
            more_browse_id: None,
            expanded: false,
        }
    }

    fn titles(shelves: &[Shelf]) -> Vec<&str> {
        shelves.iter().map(|s| s.title.as_str()).collect()
    }

    /// The named sections are found by their English titles, whatever the case or spacing; the
    /// "Similar to" line marks the artist shelves; any other headline over a title makes a themed
    /// shelf; a bare unknown title is "everything else".
    #[test]
    fn a_shelf_belongs_to_the_section_its_title_names() {
        use HomeSection::*;
        let cases = [
            ("Listen again", Some("IV"), ListenAgain),
            ("  QUICK picks ", None, QuickPicks),
            ("Your daily discover", None, Discover),
            ("Mixed for you", None, MixedForYou),
            ("From your library", None, FromYourLibrary),
            ("Trending songs for you", None, Trending),
            ("New releases", None, NewReleases),
            ("Albums for you", None, AlbumsForYou),
            ("Forgotten favorites", None, ForgottenFavorites),
            ("Fresh finds, old favorites", None, ForgottenFavorites),
            ("Featured playlists for you", None, FeaturedPlaylists),
            ("Music videos for you", None, MusicVideos),
            ("Covers and remixes", None, CoversAndRemixes),
            ("Long listens", None, LongListens),
            ("Live performances", None, LivePerformances),
            ("From the community", None, FromTheCommunity),
            ("Drake", Some("Similar to"), SimilarTo),
            (
                "Brb, Being Nostalgic!",
                Some("Throwback to the og eras of music"),
                Themed,
            ),
            ("Hip-hop", None, Everything),
            ("Something new", None, Everything),
        ];
        for (title, strapline, want) in cases {
            assert_eq!(shelf(title, strapline).home_section(), want, "{title}");
        }
        // The account's name over "Listen again" does not make it a themed shelf.
        assert_eq!(shelf("Listen again", None).home_section(), ListenAgain);
    }

    /// A strict load waits for the top seven named sections, never for the catch-all, and follows
    /// the listener's own order rather than the default.
    #[test]
    fn a_strict_load_waits_for_the_top_named_sections() {
        let default = normalise_order(&default_order_keys());
        let wanted = wanted_sections(&default, STRICT_HOME_SECTIONS);
        assert_eq!(wanted.len(), STRICT_HOME_SECTIONS);
        assert_eq!(wanted[0], HomeSection::ListenAgain);
        assert!(!wanted.contains(&HomeSection::Everything));

        // "Everything else" ranked first is skipped, not waited for.
        let mut keys = default_order_keys();
        keys.retain(|k| k != HomeSection::Everything.key());
        keys.insert(0, HomeSection::Everything.key().to_owned());
        keys.insert(1, HomeSection::LongListens.key().to_owned());
        let custom = wanted_sections(&normalise_order(&keys), 3);
        assert_eq!(custom[0], HomeSection::LongListens);
        assert_eq!(custom.len(), 3);
        assert!(!custom.contains(&HomeSection::Everything));
    }

    /// The stop rule: strict stops the moment everything wanted is in, then at the clock, then at
    /// the page cap; the ordinary load keeps to its few pages whatever has arrived.
    #[test]
    fn the_strict_stop_rule_follows_found_then_clock_then_pages() {
        use std::time::Duration;
        let wanted = vec![HomeSection::ListenAgain, HomeSection::QuickPicks];
        let strict = HomeFetch::Strict {
            wanted: wanted.clone(),
            budget: Duration::from_secs(6),
            max_pages: 12,
        };
        let secs = Duration::from_secs;
        let one = [HomeSection::ListenAgain];
        // Missing one wanted section, with time and pages to spare: keep going.
        assert_eq!(home_fetch_stop(&strict, 2, &one, secs(1)), None);
        // All wanted in: stop, even with time left, and even before any page.
        assert_eq!(
            home_fetch_stop(&strict, 0, &wanted, secs(0)),
            Some(HomeStop::Enough)
        );
        // Still missing one: the clock stops it, and so does the page cap.
        assert_eq!(
            home_fetch_stop(&strict, 3, &one, secs(6)),
            Some(HomeStop::Budget)
        );
        assert_eq!(
            home_fetch_stop(&strict, 12, &one, secs(2)),
            Some(HomeStop::PageCap)
        );
        // The ordinary load is the old three pages, found or not.
        assert_eq!(home_fetch_stop(&HomeFetch::Quick, 2, &[], secs(30)), None);
        assert_eq!(
            home_fetch_stop(&HomeFetch::Quick, 3, &[], secs(0)),
            Some(HomeStop::Quick)
        );
        // Nothing wanted at all (an empty order) is trivially satisfied.
        let none = HomeFetch::Strict {
            wanted: Vec::new(),
            budget: secs(6),
            max_pages: 12,
        };
        assert_eq!(
            home_fetch_stop(&none, 0, &[], secs(0)),
            Some(HomeStop::Enough)
        );
    }

    /// The default puts the named sections first, in the agreed order, and sections of one kind keep
    /// the order YouTube sent them in.
    #[test]
    fn the_default_order_ranks_the_named_sections_and_is_stable() {
        let mut feed = vec![
            shelf("Hip-hop", None),
            shelf("Quick picks", None),
            shelf("Brb, Being Nostalgic!", Some("Throwback")),
            shelf("Another unknown", None),
            shelf("Listen again", None),
            shelf("Artist B", Some("Similar to")),
            shelf("Artist A", Some("Similar to")),
            shelf("Your daily discover", None),
            shelf("From your library", None),
            shelf("Mixed for you", None),
        ];
        order_home(&mut feed, &normalise_order(&default_order_keys()));
        assert_eq!(
            titles(&feed),
            [
                "Listen again",
                "Quick picks",
                "Your daily discover",
                "Mixed for you",
                "From your library",
                "Artist B",
                "Artist A",
                "Brb, Being Nostalgic!",
                "Hip-hop",
                "Another unknown"
            ]
        );
    }

    /// A custom order is honoured, "everything else" included.
    #[test]
    fn a_custom_order_can_put_everything_else_first() {
        let mut feed = vec![shelf("Listen again", None), shelf("Themed", None)];
        let order = normalise_order(&["everything-else".to_owned(), "listen-again".to_owned()]);
        order_home(&mut feed, &order);
        assert_eq!(titles(&feed), ["Themed", "Listen again"]);
    }

    /// A saved list from another build still yields every section exactly once.
    #[test]
    fn a_saved_order_is_always_made_complete() {
        let order = normalise_order(&[
            "quick-picks".to_owned(),
            "no-such-section".to_owned(),
            "quick-picks".to_owned(),
        ]);
        assert_eq!(order.len(), HomeSection::DEFAULT.len());
        assert_eq!(order[0], HomeSection::QuickPicks);
        for section in HomeSection::DEFAULT {
            assert_eq!(order.iter().filter(|s| **s == section).count(), 1);
        }
        assert_eq!(
            normalise_order(&[]),
            HomeSection::DEFAULT.to_vec(),
            "nothing saved is the default"
        );
        // Sections an older build never knew come in before "everything else", not after it.
        let old = normalise_order(&["listen-again".to_owned(), "everything-else".to_owned()]);
        let everything = old
            .iter()
            .position(|s| *s == HomeSection::Everything)
            .unwrap();
        assert_eq!(
            everything,
            old.len() - 1,
            "the unrecognised still come last"
        );
    }

    /// A late page is arranged among itself and appended, so what was loaded does not move.
    #[test]
    fn a_late_page_never_moves_what_was_already_loaded() {
        let order = normalise_order(&default_order_keys());
        let mut feed = vec![shelf("Themed", None), shelf("Quick picks", None)];
        order_home(&mut feed, &order);
        let loaded = titles(&feed)
            .iter()
            .map(|t| (*t).to_owned())
            .collect::<Vec<_>>();
        let mut page = vec![shelf("Later themed", None), shelf("Listen again", None)];
        order_home(&mut page, &order);
        feed.extend(page);
        assert_eq!(
            titles(&feed),
            [
                loaded[0].as_str(),
                loaded[1].as_str(),
                "Listen again",
                "Later themed"
            ]
        );
    }

    /// Styled letters and digits fold to plain ones; everything else passes through untouched,
    /// and a title with nothing to fold is not copied.
    #[test]
    fn styled_letters_fold_to_plain_ones() {
        assert_eq!(
            fold_styled_letters("𝐓𝐢𝐦𝐞𝐥𝐞𝐬𝐬 - 𝐏𝐥𝐚𝐲𝐛𝐨𝐢 𝐂𝐚𝐫𝐭𝐢"),
            "Timeless - Playboi Carti"
        );
        assert_eq!(
            fold_styled_letters("𝘪𝘵𝘢𝘭𝘪𝘤 𝕕𝕠𝕦𝕓𝕝𝕖 𝟐𝟎𝟐𝟒"),
            "italic double 2024"
        );
        assert_eq!(fold_styled_letters("Mix 𝐅𝐫𝐞𝐬𝐡 ²™"), "Mix Fresh ²™");
        for plain in ["Timeless", "Приятного вечера", "夜に駆ける", "ＦＵＬＬ"]
        {
            assert!(
                matches!(fold_styled_letters(plain), std::borrow::Cow::Borrowed(_)),
                "{plain}"
            );
        }
    }

    #[test]
    fn result_kinds_read_as_words() {
        let mood = Entry::Mood(MoodRef {
            title: "Chill".to_owned(),
            params: String::new(),
            colour: None,
        });
        assert_eq!(mood.kind(), "Mood");
    }

    fn song(id: &str) -> Entry {
        Entry::Song(Track {
            video_id: id.to_owned(),
            title: id.to_owned(),
            artists: vec![ArtistLink::plain("Someone")],
            album: None,
            album_id: None,
            duration: None,
            plays: None,
            explicit: false,
            like: LikeStatus::Indifferent,
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            thumbnail: None,
            rank: None,
            trend: crate::model::ChartTrend::Unknown,
            wide: false,
            local_path: None,
        })
    }

    fn artist(id: &str) -> Entry {
        Entry::Artist(ArtistRef {
            id: id.to_owned(),
            name: id.to_owned(),
            subscribers: None,
            thumbnail: None,
            actions: ArtistActions::default(),
        })
    }

    fn album(id: &str) -> Entry {
        Entry::Album(AlbumRef {
            id: id.to_owned(),
            title: id.to_owned(),
            artist: String::new(),
            year: None,
            kind: None,
            thumbnail: None,
        })
    }

    /// An expansion that came back as a whole artist page must not mix other shelves' content
    /// into the one that was expanded — and must never empty the shelf either.
    #[test]
    fn an_expansion_keeps_only_the_kinds_its_preview_showed() {
        let mixed = vec![song("s1"), album("a1"), artist("x")];
        let kept = shelf_expansion(mixed.clone(), &[song("s1")]);
        assert_eq!(kept, vec![song("s1")]);

        let kept = shelf_expansion(mixed.clone(), &[artist("x")]);
        assert_eq!(kept, vec![artist("x")]);

        // Nothing of the preview's kind in the expansion: the list stands rather than the
        // shelf emptying itself.
        let kept = shelf_expansion(
            mixed.clone(),
            &[Entry::Mood(MoodRef {
                title: "Chill".to_owned(),
                params: "p".to_owned(),
                colour: None,
            })],
        );
        assert_eq!(kept, mixed);

        // No preview at all: nothing to narrow to.
        let kept = shelf_expansion(mixed.clone(), &[]);
        assert_eq!(kept, mixed);
    }

    /// Playing the third row of a mixed list must start the third *playable* item, not the third
    /// row — otherwise clicking a song after two artists plays the wrong track.
    #[test]
    fn playing_a_row_maps_past_the_unplayable_ones() {
        let r = SearchResults {
            items: vec![artist("a"), song("s1"), artist("b"), song("s2")],
            ..Default::default()
        };
        assert_eq!(r.playable().len(), 2);
        assert_eq!(r.playable_index(1), Some(0));
        assert_eq!(r.playable_index(3), Some(1));
        assert_eq!(r.playable_index(0), None, "an artist row is not playable");
        assert_eq!(r.playable_index(9), None, "out of range");
    }

    #[test]
    fn parses_the_duration_formats_youtube_returns() {
        assert_eq!(parse_duration("3:33"), Some(213));
        assert_eq!(parse_duration("0:09"), Some(9));
        assert_eq!(parse_duration("1:02:05"), Some(3725));
        assert_eq!(parse_duration(" 4:20 "), Some(260));
    }

    #[test]
    fn rejects_things_that_are_not_durations() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("live"), None);
        assert_eq!(parse_duration("3:xx"), None);
    }

    /// Pressing a thumb rates; pressing it again un-rates, as on the site.
    #[test]
    fn pressing_a_thumb_toggles() {
        use LikeStatus::{Dislike, Indifferent, Like};
        assert_eq!(Indifferent.toggled(Like), Like);
        assert_eq!(Like.toggled(Like), Indifferent);
        assert_eq!(Like.toggled(Dislike), Dislike);
        assert_eq!(Dislike.toggled(Dislike), Indifferent);
    }

    /// Writes want the bare id; browses and the library hand out the `VL` form.
    #[test]
    fn playlist_ids_shed_one_browse_prefix() {
        assert_eq!(bare_playlist_id("VLLM"), "LM");
        assert_eq!(bare_playlist_id("VLPLabc"), "PLabc");
        assert_eq!(bare_playlist_id("LM"), "LM");
        assert_eq!(bare_playlist_id("PLabc"), "PLabc");
        assert!(is_liked_auto("VLLM"));
        assert!(is_liked_auto("LM"));
        assert!(!is_liked_auto("VLPLabc"));
        assert!(is_episodes_for_later("VLSE"));
        assert!(is_episodes_for_later("SE"));
        assert!(!is_episodes_for_later("VLPLabc"));
        assert!(is_expanding_playlist_id("VLRDCLAK5uy_khy"));
        assert!(is_expanding_playlist_id("RDCLAK5uy_khy"));
        assert!(!is_expanding_playlist_id("VLPLabc"));
        assert!(!is_expanding_playlist_id("VLLM"));
        // Played through the radio: personal mixes and radios, not the curated playlists.
        assert!(is_radio_mix_id("RDTMAK5uy_abc"));
        assert!(is_radio_mix_id("VLRDAMPLPLabc"));
        assert!(!is_radio_mix_id("VLRDCLAK5uy_khy"));
        assert!(!is_radio_mix_id("PLabc"));
    }

    /// The rail's second line: your own lists show how much is in them — as does Liked Music,
    /// which has no maker — while someone else's list leads with who made it, count and all.
    #[test]
    fn a_playlist_detail_leads_with_its_maker_when_it_is_someone_elses() {
        let mut p = PlaylistRef {
            id: "VLPLabc".to_owned(),
            title: "anime bangers".to_owned(),
            author: "IV".to_owned(),
            author_id: None,
            track_count: Some("29 tracks".to_owned()),
            thumbnail: None,
            editable: true,
            source: PlaylistSource::YouTube,
        };
        assert_eq!(p.detail(), "29 tracks");
        p.editable = false;
        p.author = "Someone Else".to_owned();
        assert_eq!(p.detail(), "Someone Else • 29 tracks");
        p.id = "VLLM".to_owned();
        assert_eq!(p.detail(), "29 tracks", "Liked Music keeps its count");
        p.id = "VLPLabc".to_owned();
        p.track_count = None;
        assert_eq!(p.detail(), "Someone Else");
        p.author = String::new();
        p.track_count = Some("3 songs".to_owned());
        assert_eq!(p.detail(), "3 songs", "no name, just the count");
        p.track_count = None;
        assert_eq!(p.detail(), "", "nothing to say is said as nothing");
    }
}
