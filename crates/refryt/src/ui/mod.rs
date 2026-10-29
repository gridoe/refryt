//! Page and panel renderers. Each takes the whole `App` and borrows its fields disjointly, so a
//! page can iterate one list while fetching art textures for the rows it is drawing.

pub mod album;
pub mod artist;
pub mod artist_liked;
pub mod charts;
pub mod crash;
pub mod explore;
pub mod fullscreen;
pub mod help;
pub mod history;
pub mod home;
pub mod home_order;
pub mod library;
pub mod lyrics;
pub mod mood;
pub mod moods;
pub mod paste_cookies;
pub mod player_bar;
pub mod playlist;
pub mod releases;
pub mod search;
pub mod settings;
pub mod sidebar;
pub mod splash;
pub mod topbar;
pub mod update;
pub mod upnext;
pub mod video;

use std::sync::Arc;

use crate::app::Route;
use crate::backend::Backend;
use crate::images::Images;
use crate::model::LikeStatus;
use crate::model::{Entry, PlaylistRef, Shelf, Track};
use crate::theme;
use crate::widgets;

/// Art sizes, in **points** — what is actually drawn. The pixel size is derived per-frame from
/// the display scale in [`art_texture`]. These used to be hand-picked pixel constants (96, 320,
/// 256) that had no relationship to the slots they filled: a 320px cover in a 132pt card is
/// ~5.8x the pixels needed on a 1x display, and a full cache of them is 80 MB of texture memory.
pub(crate) const ROW_ART_PT: f32 = theme::ART_SIZE;
/// Page headers (artist / album / playlist art).
pub(crate) const HEADER_ART_PT: f32 = 160.0;
/// How small the header art gets before it refuses to shrink further — a disc, not a stamp.
pub(crate) const HEADER_ART_FLOOR_PT: f32 = 64.0;
/// Album and playlist headers, drawn large like the reference. Request matches display.
pub(crate) const PAGE_ART_SIZE: f32 = 192.0;
/// A detail header's title: bold, and shrinking in 2pt steps from here down to
/// [`DETAIL_TITLE_MIN`] rather than ellipsizing, so a long album or playlist name keeps its one
/// line — the way the reference handles a name that would not fit at full size.
pub(crate) const DETAIL_TITLE_MAX: f32 = 56.0;
pub(crate) const DETAIL_TITLE_MIN: f32 = 26.0;
/// The detail header's Play button: the artist page's own 56, so every page header's primary
/// action is the same size.
pub(crate) const DETAIL_PLAY_PT: f32 = 56.0;

/// How wide a card is in a grid; its cover is inset by 8pt on each side.
pub(crate) const CARD_W: f32 = 132.0;
/// How far a grid card may grow past [`CARD_W`] to fill its row. A little is what closes the
/// dead strip a fixed width leaves at the right edge; much more and one column short of a wide
/// window turns into five oversized covers.
const CARD_STRETCH_MAX: f32 = 1.3;

/// The card width that fills a wrapped grid's row exactly: as many [`CARD_W`] columns as fit,
/// then each grown to share the remainder. Carousels keep [`CARD_W`] — they scroll, so there is
/// no row end to reach.
pub(crate) fn grid_card_w(ui: &egui::Ui) -> f32 {
    fill_width(ui.available_width(), ui.spacing().item_spacing.x)
}

fn fill_width(available: f32, gap: f32) -> f32 {
    let cols = ((available + gap) / (CARD_W + gap)).floor().max(1.0);
    // A hair under the exact share, so float rounding never wraps the last card of a row.
    let w = (available - gap * (cols - 1.0)) / cols - 0.5;
    w.clamp(CARD_W, CARD_W * CARD_STRETCH_MAX)
}

/// Everything a shelf's album/playlist cards need for their right-click menu: your playlists, and
/// the caches that hold a release's tracks once prefetched. Looked up by id, so a card can offer
/// the same queueing and saving items the page ··· menu does — filling in as the fetch lands.
pub(crate) struct ReleaseCtx<'a> {
    pub playlists: &'a [crate::model::PlaylistRef],
    pub signed_in: bool,
    pub rail: &'a std::collections::HashMap<String, crate::model::Playlist>,
    pub albums: &'a std::collections::HashMap<String, crate::model::Album>,
}

/// Rows kept on either side of the visible range, so a fast scroll never shows a gap.
const OVERSCAN: usize = 2;

/// How close to the end of a loaded list the viewport must get before the next page is requested.
///
/// Wider than the overscan on purpose: fetching a page takes a round trip, so asking only when the
/// very last row is drawn would show the user the bottom of the list before more arrived.
const PREFETCH_ROWS: usize = 10;

/// The pixel size to request for something drawn `points` across.
///
/// Rounded up to a multiple of 32 so a handful of stable sizes cover the whole app — the cache is
/// keyed on this number, and one entry per distinct float would defeat it.
pub(crate) fn art_px(ctx: &egui::Context, points: f32) -> u32 {
    let px = (points * ctx.pixels_per_point()).ceil().max(1.0) as u32;
    px.next_multiple_of(32).clamp(32, 512)
}

/// Look up — and trigger the fetch of — a cover texture for something drawn `points` across.
///
/// A free function rather than an `App` method so callers can hold a borrow on another `App`
/// field (e.g. the row being drawn) while it runs.
pub(crate) fn art_texture<'a>(
    images: &'a mut Images,
    backend: &Backend,
    ctx: &egui::Context,
    url: Option<&Arc<str>>,
    points: f32,
) -> Option<&'a egui::TextureHandle> {
    let px = art_px(ctx, points);
    url.and_then(|url| {
        images.texture(url, px, |key, sized, px, wide| {
            backend.fetch_image(key, sized, px, wide)
        })
    })
}

/// Look up — and trigger the fetch of — a 16:9 still for something drawn `points` across.
///
/// `points` is the drawn *width*; the height follows from it. The cache key is that width, so a
/// wide still and a square cover of the same URL never share an entry.
pub(crate) fn art_texture_wide<'a>(
    images: &'a mut Images,
    backend: &Backend,
    ctx: &egui::Context,
    url: Option<&Arc<str>>,
    points: f32,
) -> Option<&'a egui::TextureHandle> {
    let px = art_px(ctx, points);
    url.and_then(|url| {
        images.texture_wide(url, px, |key, sized, px, wide| {
            backend.fetch_image(key, sized, px, wide)
        })
    })
}

/// [`art_px`] for the artist hero: a banner is drawn across the whole card, so the 512pt cap the
/// cover sizes share would be visibly soft. Capped at 2048 — wider than any window the card is
/// likely to be drawn in, and a 2048×533 texture is ~4 MB, well inside the cache's budget.
pub(crate) fn art_px_hero(ctx: &egui::Context, points: f32) -> u32 {
    let px = (points * ctx.pixels_per_point()).ceil().max(1.0) as u32;
    px.next_multiple_of(32).clamp(256, 2048)
}

/// Look up — and trigger the fetch of — the artist hero banner for a card `points` wide. The
/// height follows [`crate::images::HERO_ASPECT`]; the artist page cover-crops the remainder.
pub(crate) fn art_texture_hero<'a>(
    images: &'a mut Images,
    backend: &Backend,
    ctx: &egui::Context,
    url: Option<&Arc<str>>,
    points: f32,
) -> Option<&'a egui::TextureHandle> {
    let px = art_px_hero(ctx, points);
    url.and_then(|url| {
        images.texture_hero(url, px, |key, sized, px, wide| {
            backend.fetch_image(key, sized, px, wide)
        })
    })
}

/// Which row indices intersect the viewport, plus overscan.
///
/// Split out from [`virtual_rows`] because it is the part that can be wrong, and it needs no egui
/// context to check. `top` is where the list starts and `clip_top`/`clip_h` are the viewport, both
/// in the same (screen) coordinate space — scrolling moves `top`, not the clip. `pitch` is the
/// distance between row starts: the row height *plus* the item spacing egui inserts after each
/// one (see [`row_pitch`]), not the height alone.
fn visible_range(
    top: f32,
    clip_top: f32,
    clip_h: f32,
    pitch: f32,
    count: usize,
) -> std::ops::Range<usize> {
    let first = (((clip_top - top) / pitch).floor().max(0.0) as usize).saturating_sub(OVERSCAN);
    let first = first.min(count);
    let visible = (clip_h / pitch).ceil() as usize + OVERSCAN * 2 + 1;
    first..(first + visible).min(count)
}

/// The distance between the tops of two consecutive rows.
///
/// A row allocates its own height, but egui then advances the cursor by `item_spacing` before the
/// next one, so the true pitch is `row_h + spacing`. Every part of the virtual layout — the index
/// math and both spacers — has to use this same number, or the computed extent changes with how
/// many rows happen to be visible. That mismatch is what made lists jitter and overshoot at the
/// bottom: the spacers reserved `row_h` per skipped row while the drawn rows each paid `spacing`.
fn row_pitch(row_h: f32, item_spacing: f32) -> f32 {
    row_h + item_spacing
}

/// The pair of spacers a virtual list reserves around the drawn slice, `[first, last)`.
///
/// Both are in pitch units, so the total the scroll area sees is always `count * pitch` — the
/// full list, irrespective of the viewport. Pure, so that invariant is testable.
fn virtual_spacers(first: usize, last: usize, count: usize, pitch: f32) -> (f32, f32) {
    let top = first as f32 * pitch;
    let bottom = if last < count {
        (count - last) as f32 * pitch
    } else {
        0.0
    };
    (top, bottom)
}

/// Draw a long list without paying for the rows nobody can see.
///
/// `ScrollArea::show_rows` would be the obvious tool, but these pages put a header and several
/// sections inside one scroll area, so the list is not the scroll area. This does the same job
/// from the inside: it works out which indices intersect the clip rect, reserves the space above
/// and below with two spacers so the scrollbar stays honest, and calls `row` only for the visible
/// range plus a little overscan.
///
/// Because art is requested inside `row`, this also stops a 500-song library from queueing 500
/// cover fetches the moment it loads.
pub(crate) fn virtual_rows(
    ui: &mut egui::Ui,
    row_h: f32,
    count: usize,
    mut row: impl FnMut(&mut egui::Ui, usize),
) -> std::ops::Range<usize> {
    if count == 0 {
        return 0..0;
    }
    // The gap egui adds after each row is part of the layout, not decoration: see `row_pitch`.
    let pitch = row_pitch(row_h, ui.spacing().item_spacing.y);
    let clip = ui.clip_rect();
    let top = ui.cursor().top();
    let range = visible_range(top, clip.top(), clip.height(), pitch, count);
    let (first, last) = (range.start, range.end);

    let (top_space, bottom_space) = virtual_spacers(first, last, count, pitch);
    if top_space > 0.0 {
        ui.add_space(top_space);
    }
    for i in first..last {
        row(ui, i);
    }
    if bottom_space > 0.0 {
        ui.add_space(bottom_space);
    }
    first..last
}

/// Everything a row shows, borrowed from the page's own data.
///
/// Deliberately carries the thumbnail *URL* rather than a texture: the texture has to be looked up
/// inside the row loop, where the mutable borrow of the cache lives and dies within one iteration.
/// A caller that handed back a `&TextureHandle` could not borrow the cache again for the next row.
pub(crate) struct RowSpec<'a> {
    pub number: Option<usize>,
    /// A chart rank, shown in place of the position number where the track names one.
    pub rank: Option<&'a str>,
    /// Which way the rank moved. Unknown paints nothing.
    pub trend: crate::model::ChartTrend,
    pub title: &'a str,
    /// See `widgets::TrackRow::album_id` — lets the title itself carry an album link.
    pub album_id: Option<&'a str>,
    pub subtitle: widgets::Subtitle<'a>,
    pub duration: Option<u64>,
    pub plays: Option<&'a str>,
    pub explicit: bool,
    pub active: bool,
    /// See `widgets::TrackRow::playing` — `active` and not paused.
    pub playing: bool,
    pub badge: Option<&'a str>,
    pub shape: widgets::ArtShape,
    /// See `widgets::TrackRow::hide_art` — the album page, whose rows would all repeat the
    /// header's own cover.
    pub hide_art: bool,
    pub thumbnail: Option<&'a std::sync::Arc<str>>,
    /// Right-click menu. `None` for rows that are not songs.
    pub menu: Option<widgets::RowMenu<'a>>,
    /// Whether the row offers rating thumbs. The queue turns them off.
    pub show_rating: bool,
    /// See `widgets::TrackRow::quiet_like` — Liked Music.
    pub quiet_like: bool,
    /// Your rating, for the hover thumbs. `Indifferent` where the source carries none.
    pub like: LikeStatus,
    /// See `widgets::TrackRow::add_to` — the Suggestions under your own playlist.
    pub add_to: Option<&'a str>,
}

/// A row's rating: its own when the source carried one, else the liked index's. Search results,
/// Home shelves and most browse pages never say whether a song is liked, but the index knows every
/// song the account has liked — asked at draw time, so a scan that lands after the page did still
/// marks it.
fn row_like(s: &RowSpec<'_>, liked: &crate::liked::Index) -> LikeStatus {
    if s.like != LikeStatus::Indifferent {
        return s.like;
    }
    s.menu
        .as_ref()
        .and_then(|m| liked.like_of(&m.track.video_id))
        .unwrap_or(s.like)
}

/// The subtitle of a track row: its artist, then its album, each a link where we have the id.
///
/// This is what makes a song a way in to its artist and its album rather than a dead end.
pub(crate) fn track_subtitle(t: &crate::model::Track) -> widgets::Subtitle<'_> {
    track_subtitle_inner(t, false)
}

/// The subtitle where the album would be redundant: the album page (every row is this
/// album) and the queue (a list of what happens next, not where each song is from).
/// Artists stay linked and the plays/Radio tail is unaffected — those ride the separate
/// `tail` parameter, not the subtitle slots.
pub(crate) fn track_subtitle_no_album(t: &crate::model::Track) -> widgets::Subtitle<'_> {
    track_subtitle_inner(t, true)
}

fn track_subtitle_inner(t: &crate::model::Track, hide_album: bool) -> widgets::Subtitle<'_> {
    let mut out = widgets::no_subtitle();
    // Each credited artist is its own link, joined the way YouTube writes them. The last slots
    // are left for the album and whatever a page adds after it, so a track credited to more
    // artists than there is room for keeps as many as fit rather than losing the album.
    let n = fill_artists(&mut out, &t.artists);
    if !hide_album && let Some(name) = t.album.as_deref() {
        out[n] = Some(match t.album_id.as_deref() {
            Some(id) => widgets::Part::link(name, widgets::LinkTarget::Album(id)),
            None => widgets::Part::plain(name),
        });
    }
    out
}

/// Fill `out` with the credited artists as linked parts, leaving the final slots free, and
/// return the next free slot. Shared by track rows and the album/playlist header so a
/// multi-artist credit is linked identically in both.
fn fill_artists<'a>(
    out: &mut widgets::Subtitle<'a>,
    artists: &'a [crate::model::ArtistLink],
) -> usize {
    let mut n = 0;
    for artist in artists.iter().take(widgets::SUBTITLE_PARTS - 2) {
        let part = match artist.id.as_deref() {
            Some(id) => widgets::Part::link(&artist.name, widgets::LinkTarget::Artist(id)),
            None => widgets::Part::plain(&artist.name),
        };
        out[n] = Some(if n == 0 {
            part
        } else {
            part.joined(widgets::NAME_SEP)
        });
        n += 1;
    }
    n
}

/// Turn a clicked link into the page it leads to.
pub(crate) fn link_route(link: widgets::LinkTarget<'_>) -> Route {
    match link {
        widgets::LinkTarget::Artist(id) => Route::Artist(id.to_owned()),
        widgets::LinkTarget::Album(id) => Route::Album(id.to_owned()),
    }
}

/// What a virtualised list reported: a row was activated, or one of its links was followed.
pub(crate) enum ListClick {
    Row(usize),
    Link(Route),
}

/// A song menu's choice, carrying everything it needs: the video id for mix and share, the
/// track itself for the queue, the linked ids for navigation. Owned, so it crosses the borrow
/// of the list it came from and every page forwards it with one line.
pub(crate) enum RowAction {
    /// Play whatever YouTube handed over — an artist's shuffle or mix.
    PlayTarget {
        seed: String,
        target: crate::model::WatchTarget,
    },
    Subscribe {
        channel_id: String,
        subscribe: bool,
        /// The page button's own token for this direction, when it carried one.
        params: Option<Arc<str>>,
    },
    DontRecommend {
        token: String,
        name: String,
    },
    StartMix(String),
    /// A page header's mix, seeded by the whole playlist/album (`RDAMPL` + its id) rather than
    /// by its first song.
    StartMixPlaylist(String),
    /// Save a song to `~/Music/refryt`.
    Download(Track),
    /// Save a whole page's tracks.
    DownloadMany(Vec<Track>),
    /// Delete a downloaded file by video id.
    RemoveDownload(String),
    PlayNext(Track),
    AddToQueue(Track),
    GoToAlbum(String),
    GoToArtist(String),
    /// Copy a song's link.
    CopyLink(String),
    Rate {
        video_id: String,
        status: LikeStatus,
    },
    /// Flip a song's library membership with its own bookmark token.
    ToggleLibrary(Track),
    SaveToPlaylist {
        playlist_id: String,
        video_id: String,
    },
    /// A page header's ••• menu acting on the whole loaded list: shuffle the page, queue it
    /// all, or save it all somewhere. Vecs, not slices — the choice crosses the borrow of the
    /// list it came from, same as every other `RowAction`.
    PageShuffle(Vec<Track>),
    PagePlayNext(Vec<Track>),
    PageQueue(Vec<Track>),
    PageSaveToPlaylist {
        playlist_id: String,
        title: String,
        video_ids: Vec<String>,
    },
    /// Not into an existing playlist — a brand new local one, seeded with the whole page (a
    /// folder's tracks, in the Local tab's browser). `title` is just the default name offered
    /// in the modal, editable before it commits.
    PageSaveAsNewPlaylist {
        title: String,
        video_ids: Vec<String>,
    },
    /// Flip an album or playlist header's library membership. `album` picks the page;
    /// `in_library` is the state being asked for, so a refusal reverts to `!in_library`.
    PageLibrary {
        album: bool,
        write: crate::backend::PageWrite,
        in_library: bool,
    },
    /// Copy a page's link (an album, playlist or channel URL, not a `watch?v=` one).
    PageCopyLink(String),
    /// Show the page's description in the Info popup. The text lives on the page, so the
    /// choice carries nothing.
    PageInfo,
    RemoveFromPlaylist {
        playlist_id: String,
        video_id: String,
        set_video_id: String,
    },
    /// Open the rename editor for a playlist. The app uses the loaded page when it is that
    /// playlist, else it navigates and opens the editor once the page arrives.
    EditPlaylist(String),
    /// Ask to delete a playlist (the app raises the confirm modal).
    DeletePlaylist(String),
    /// Remove a saved (not owned) playlist from the library.
    UnsavePlaylist(String),
    /// Drop a track from the queue by its position — only offered from the Queue panel, whose
    /// rows carry their own index.
    RemoveFromQueue(usize),
    /// A page menu item picked before the page's songs had loaded (a sidebar playlist or a card
    /// right-clicked a moment ago): done once they land.
    PageWhenLoaded {
        page_id: String,
        kind: PageKind,
        then: PageThen,
    },
}

/// Which kind of page a menu stands for, where the songs still have to be fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageKind {
    Album,
    Playlist,
}

/// What a page menu item does with the page's songs, once there are songs to do it with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PageThen {
    StartMix,
    Shuffle,
    PlayNext,
    Queue,
    DownloadAll,
    SaveToPlaylist { playlist_id: String, title: String },
}

/// What a list did this frame.
pub(crate) struct ListOutcome {
    pub click: Option<ListClick>,
    /// A right-click menu choice, if one was picked.
    pub action: Option<RowAction>,
    /// The viewport has reached the end of what is loaded — time to ask for the next page.
    ///
    /// Taken from the range the virtualiser actually drew rather than recomputed, so it cannot
    /// disagree with what is on screen.
    pub at_end: bool,
    /// A row was dragged onto another one and released — `(from, to)`, both indices into the
    /// list as it was *before* this frame's reorder. Only ever set for rows whose menu carries
    /// a playlist id (an editable playlist's own track list); every other list's rows are not
    /// draggable at all, so this is always `None` for them.
    pub reorder: Option<(usize, usize)>,
}

/// Draw a virtualised list of rows and report what was clicked.
///
/// Takes `App`'s fields individually rather than `&mut App` so callers can destructure once and
/// hand out disjoint borrows — which is what lets `spec` read the item it is describing while this
/// mutates the texture cache.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spec_rows<'d>(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    stats: &mut crate::stats::Stats,
    liked: &crate::liked::Index,
    count: usize,
    spec: impl Fn(usize) -> RowSpec<'d>,
) -> ListOutcome {
    let mut clicked = None;
    let mut action = None;
    let mut reorder = None;
    let drawn = virtual_rows(ui, theme::ROW_HEIGHT, count, |ui, i| {
        let s = spec(i);
        let like = row_like(&s, liked);
        // Only an editable playlist's own track list ever sets this — see `RowMenu::playlist_id`
        // and `ListOutcome::reorder`'s own doc comments.
        let draggable = s.menu.as_ref().and_then(|m| m.playlist_id).is_some();
        // A video still is 16:9; fetch its wide crop so the row draws it at its own aspect
        // instead of squeezing it into the square cover slot. A hidden slot fetches nothing.
        let art = if s.hide_art {
            None
        } else if s.shape == crate::widgets::ArtShape::Wide {
            art_texture_wide(
                images,
                backend,
                ui.ctx(),
                s.thumbnail,
                ROW_ART_PT * 16.0 / 9.0,
            )
        } else {
            art_texture(images, backend, ui.ctx(), s.thumbnail, ROW_ART_PT)
        };
        let hit = crate::widgets::track_row(
            ui,
            palette,
            crate::widgets::TrackRow {
                number: s.number,
                rank: s.rank,
                trend: s.trend,
                title: s.title,
                album_id: s.album_id,
                subtitle: s.subtitle,
                duration: s.duration,
                plays: s.plays,
                explicit: s.explicit,
                active: s.active,
                playing: s.playing,
                badge: s.badge,
                art,
                shape: s.shape,
                hide_art: s.hide_art,
                id_salt: i,
                menu: s.menu,
                like,
                show_rating: s.show_rating,
                quiet_like: s.quiet_like,
                add_to: s.add_to,
            },
        );
        // A link wins over the row: clicking the artist name must not also start the track.
        if let Some(link) = hit.link {
            clicked = Some(ListClick::Link(link_route(link)));
        } else if hit.activate {
            clicked = Some(ListClick::Row(i));
        }
        if action.is_none() {
            action = hit.action;
        }
        if draggable {
            if hit.response.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), i);
            }
            if egui::DragAndDrop::has_payload_of_type::<usize>(ui.ctx())
                && hit.response.contains_pointer()
            {
                if ui.ctx().input(|inp| inp.pointer.any_released()) {
                    if let Some(from) = egui::DragAndDrop::take_payload::<usize>(ui.ctx())
                        && *from != i
                    {
                        reorder = Some((*from, i));
                    }
                } else {
                    // Always "insert before this row" — a single, direction-independent rule,
                    // unlike the sidebar's own playlist-order drag (which flips edges because
                    // that one is a plain `remove`+`insert` with no server call to match). This
                    // one always becomes an `ACTION_MOVE_VIDEO_BEFORE`, which only ever means
                    // "before", so the indicator never promising "after" keeps the two honest.
                    ui.painter().hline(
                        egui::Rangef::new(hit.response.rect.left(), hit.response.rect.right()),
                        hit.response.rect.top(),
                        egui::Stroke::new(2.0, palette.accent),
                    );
                }
            }
        }
    });
    stats.rows(drawn.len(), count);
    ListOutcome {
        click: clicked,
        action,
        at_end: count == 0 || drawn.end + PREFETCH_ROWS >= count,
        reorder,
    }
}

/// One cell of a track grid: its own child `Ui` at an explicit rect, the same split
/// `page_header` uses for its control row. Used by the paged view (`track_shelf_pages`).
#[allow(clippy::too_many_arguments)]
fn paint_grid_cell<'d>(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    liked: &crate::liked::Index,
    cell_rect: egui::Rect,
    i: usize,
    s: RowSpec<'d>,
) -> (Option<ListClick>, Option<RowAction>) {
    let mut cell = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("shelf-grid-cell", i))
            .max_rect(cell_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let like = row_like(&s, liked);
    let art = if s.shape == crate::widgets::ArtShape::Wide {
        art_texture_wide(
            images,
            backend,
            cell.ctx(),
            s.thumbnail,
            ROW_ART_PT * 16.0 / 9.0,
        )
    } else {
        art_texture(images, backend, cell.ctx(), s.thumbnail, ROW_ART_PT)
    };
    let hit = crate::widgets::track_row(
        &mut cell,
        palette,
        crate::widgets::TrackRow {
            number: s.number,
            rank: s.rank,
            trend: s.trend,
            title: s.title,
            // The title links to its album here too: a song you cannot open from the shelf it
            // is shown on is a dead end, and the album is one click away everywhere else.
            album_id: s.album_id,
            subtitle: s.subtitle,
            duration: s.duration,
            plays: s.plays,
            explicit: s.explicit,
            active: s.active,
            playing: s.playing,
            badge: s.badge,
            art,
            shape: s.shape,
            hide_art: false,
            id_salt: i,
            menu: s.menu,
            like,
            show_rating: s.show_rating,
            quiet_like: s.quiet_like,
            add_to: s.add_to,
        },
    );
    // A link wins over the row: clicking the artist name must not also start the track.
    let click = if let Some(link) = hit.link {
        Some(ListClick::Link(link_route(link)))
    } else if hit.activate {
        Some(ListClick::Row(i))
    } else {
        None
    };
    (click, hit.action)
}

/// Rows in one page of the artist page's Top songs grid: three columns of five. Home's song
/// shelves page four rows at a time, the way YouTube Music's own home does; see
/// [`HOME_SONG_ROWS`].
const ROWS_PER_PAGE: usize = 5;

/// Rows in one page of a Home song shelf (Quick picks and the like).
pub(crate) const HOME_SONG_ROWS: usize = 4;

/// The queue position a paged grid cell shows. Columns fill top to bottom before the next
/// begins, so the numbers read 1-5, 6-10, 11-15 rather than across the rows.
fn grid_index(start: usize, col: usize, row: usize, rows: usize) -> usize {
    start + col * rows + row
}

/// A short, bounded track list — Top Songs — capped to 5 rows with ‹ › arrows to page through
/// the rest, rather than one open-ended wrap: at 3 columns the full 15 already
/// fits in those 5 rows with nothing left to page through; the arrows only earn their keep on a
/// narrower window, where fewer columns fit and 5 rows is not enough to show all 15 at once.
/// Each column fills top to bottom before the next begins, so the position numbers read down.
/// Not virtualised — bounded lists like this are cheap enough to lay out in full every frame —
/// so the page itself is the only thing that decides which slice is on screen.
#[allow(clippy::too_many_arguments)]
fn spec_pages<'d>(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    stats: &mut crate::stats::Stats,
    liked: &crate::liked::Index,
    heading: &str,
    strapline: Option<&str>,
    count: usize,
    cols: usize,
    rows: usize,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    spec: impl Fn(usize) -> RowSpec<'d>,
) -> ListOutcome {
    let per_page = (cols * rows).max(1);
    let pages = count.div_ceil(per_page).max(1);

    let id = ui.id().with(("track-shelf-page", id_salt));
    let mut page: usize = ui
        .ctx()
        .data(|d| d.get_temp(id))
        .unwrap_or(0)
        .min(pages - 1);

    ui.add_space(20.0);
    if let Some(line) = strapline {
        shelf_strapline(ui, palette, line);
    }
    let heading_label = || {
        egui::RichText::new(heading)
            .font(theme::bold(theme::SECTION_PT))
            .color(palette.text)
    };
    if pages > 1 {
        // The heading rides the same row as the arrows — same fix `artist_shelf`'s "Show all"
        // already relies on: a plain `ui.label` first, then `with_layout` nested inside this
        // `ui.horizontal` (never bare on the outer vertical `ui`, which would claim the rest of
        // the page's height for this "row" instead of bounding it to the row's own content).
        ui.horizontal(|ui| {
            ui.label(heading_label());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(page + 1 < pages, |ui| {
                    if widgets::icon_button(
                        ui,
                        palette,
                        crate::icons::Icon::ChevronRight,
                        14.0,
                        false,
                    )
                    .clicked()
                    {
                        page += 1;
                    }
                });
                ui.add_space(4.0);
                ui.add_enabled_ui(page > 0, |ui| {
                    if widgets::icon_button(
                        ui,
                        palette,
                        crate::icons::Icon::ChevronLeft,
                        14.0,
                        false,
                    )
                    .clicked()
                    {
                        page -= 1;
                    }
                });
            });
        });
        ui.ctx().data_mut(|d| d.insert_temp(id, page));
    } else {
        ui.label(heading_label());
    }
    ui.add_space(4.0);

    let mut clicked = None;
    let mut action = None;
    let gap = 12.0;
    let col_w = (ui.available_width() - gap * cols.saturating_sub(1) as f32) / cols as f32;
    let start = page * per_page;
    let end = (start + per_page).min(count);
    // Filled down each column, not across the rows: the numbers read 1-5 down the first
    // column, 6-10 down the second, and so on. A row-major fill put 1,2,3 across the top, so
    // reading a column gave 1,4,7,10.
    let rows_here = (end - start).min(rows);
    // Under the shelf's own id: every cell's links are keyed by its index, and Home stacks
    // several of these grids in one parent — the first song of each shelf shared one id, so a
    // click on a title or an artist was claimed by whichever shelf registered it last.
    ui.push_id(id, |ui| {
        for r in 0..rows_here {
            let top_left = ui.cursor().min;
            for c in 0..cols {
                let i = grid_index(start, c, r, rows);
                if i >= end {
                    break;
                }
                let x = top_left.x + c as f32 * (col_w + gap);
                let cell_rect = egui::Rect::from_min_size(
                    egui::pos2(x, top_left.y),
                    egui::vec2(col_w, theme::ROW_HEIGHT),
                );
                let (click, act) =
                    paint_grid_cell(ui, palette, images, backend, liked, cell_rect, i, spec(i));
                if click.is_some() {
                    clicked = click;
                }
                if action.is_none() {
                    action = act;
                }
            }
            ui.add_space(theme::ROW_HEIGHT);
        }
    });
    stats.rows(end - start, count);
    ListOutcome {
        click: clicked,
        action,
        at_end: true,
        // A paged shelf grid is never an editable playlist's own track list.
        reorder: None,
    }
}

/// [`spec_rows`] for a plain list of tracks, which is most of them.
///
/// Every track row carries its menu: Start Mix, queueing, navigation and share all derive
/// from the track itself, so no page has anything to add. `playlist_id` is the open playlist
/// when the list is its editable track list; `playlists` feeds the save picker. `hide_album`
/// drops the album from the subtitle where it would be redundant — the album page, where
/// every row is this album. `hide_art` drops the cover for the same reason: on that page it
/// would be the header's own art repeated down the list.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_rows(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    stats: &mut crate::stats::Stats,
    liked: &crate::liked::Index,
    now: &crate::player::NowPlaying,
    tracks: &[crate::model::Track],
    numbered: bool,
    playlist_id: Option<&str>,
    playlists: &[PlaylistRef],
    signed_in: bool,
    hide_album: bool,
    hide_art: bool,
) -> ListOutcome {
    track_rows_quiet(
        ui,
        palette,
        images,
        backend,
        stats,
        liked,
        now,
        tracks,
        numbered,
        playlist_id,
        playlists,
        signed_in,
        hide_album,
        hide_art,
        false,
    )
}

/// [`track_rows`], with `quiet_like` for a list where every row is liked — Liked Music.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_rows_quiet(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    stats: &mut crate::stats::Stats,
    liked: &crate::liked::Index,
    now: &crate::player::NowPlaying,
    tracks: &[crate::model::Track],
    numbered: bool,
    playlist_id: Option<&str>,
    playlists: &[PlaylistRef],
    signed_in: bool,
    hide_album: bool,
    hide_art: bool,
    quiet_like: bool,
) -> ListOutcome {
    spec_rows(
        ui,
        palette,
        images,
        backend,
        stats,
        liked,
        tracks.len(),
        |i| {
            let t = &tracks[i];
            RowSpec {
                number: numbered.then_some(i + 1),
                rank: t.rank.as_deref(),
                trend: t.trend,
                title: &t.title,
                album_id: t.album_id.as_deref(),
                subtitle: track_subtitle_inner(t, hide_album),
                duration: t.duration,
                plays: t.plays.as_deref(),
                explicit: t.explicit,
                active: is_active(now, &t.video_id),
                playing: is_active(now, &t.video_id) && !now.paused,
                badge: None,
                // Square, always: `t.wide` marks a music video for playback, but the art in a
                // track list is the album/playlist cover and a 16:9 slot only crops it.
                shape: widgets::ArtShape::Square,
                hide_art,
                thumbnail: t.thumbnail.as_ref(),
                menu: Some(widgets::RowMenu {
                    track: t,
                    playlist_id,
                    playlists,
                    signed_in,
                    queue_index: None,
                }),
                like: t.like,
                show_rating: true,
                quiet_like,
                add_to: None,
            }
        },
    )
}

/// How many columns an artist's Top songs take at `width`: one per 400 pt, up to three.
pub(crate) fn top_song_columns(width: f32) -> usize {
    ((width / 400.0).floor() as usize).clamp(1, 3)
}

/// Paged instead of wrapped — for a short, bounded list where growing indefinitely tall would be
/// worse than paging: an artist's Top Songs, expanded past the 5 the page embeds up to a cap of
/// 15 (see `App`'s handling of `Event::PlaylistLoaded` for the artist's own auto-generated
/// playlist — 15 is enough, so there is no "show all" beyond it). `id_salt` resets the page to 0
/// per list — the artist id, so paging through one artist's songs does not leave a later one's
/// opened mid-page.
///
/// 3 columns × 5 rows is the target: wide enough for that and the full 15 shows at once with no
/// arrows at all. Only a narrower window — fewer columns fitting — needs them. `numbered` draws
/// the position number in each row's leading slot, which the artist page's "Popular" list wants
/// and the shelves it sits beside do not; the columns fill downwards so those numbers run
/// 1-5, 6-10, 11-15 rather than across.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_shelf_pages(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    stats: &mut crate::stats::Stats,
    liked: &crate::liked::Index,
    heading: &str,
    numbered: bool,
    now: &crate::player::NowPlaying,
    tracks: &[crate::model::Track],
    playlists: &[PlaylistRef],
    signed_in: bool,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
) -> ListOutcome {
    let cols = top_song_columns(ui.available_width());
    spec_pages(
        ui,
        palette,
        images,
        backend,
        stats,
        liked,
        heading,
        None,
        tracks.len(),
        cols,
        ROWS_PER_PAGE,
        id_salt,
        |i| {
            shelf_track_spec(
                &tracks[i],
                numbered.then_some(i + 1),
                now,
                playlists,
                signed_in,
            )
        },
    )
}

/// One song in a paged song shelf — the artist page's Top songs and Home's song shelves draw
/// their rows the same way.
pub(crate) fn shelf_track_spec<'a>(
    t: &'a crate::model::Track,
    number: Option<usize>,
    now: &crate::player::NowPlaying,
    playlists: &'a [PlaylistRef],
    signed_in: bool,
) -> RowSpec<'a> {
    RowSpec {
        number,
        rank: t.rank.as_deref(),
        trend: t.trend,
        title: &t.title,
        album_id: t.album_id.as_deref(),
        // No album in this shelf's subtitle, at any column count: the list is drawn in
        // narrow columns (1–3 of them inside the artist page's own layout), where
        // "artist • album • plays" is clipped mid-word before the plays even show. The
        // title itself still links to the album.
        subtitle: track_subtitle_inner(t, true),
        duration: t.duration,
        plays: t.plays.as_deref(),
        explicit: t.explicit,
        active: is_active(now, &t.video_id),
        playing: is_active(now, &t.video_id) && !now.paused,
        badge: None,
        // Square, always: `t.wide` marks a music video for playback (the resolver
        // swaps in an audio-only id), but the cover art in a song list is the
        // album/playlist art and a 16:9 slot only crops it.
        shape: widgets::ArtShape::Square,
        hide_art: false,
        thumbnail: t.thumbnail.as_ref(),
        menu: Some(widgets::RowMenu {
            track: t,
            playlist_id: None,
            playlists,
            signed_in,
            queue_index: None,
        }),
        like: t.like,
        show_rating: true,
        quiet_like: false,
        add_to: None,
    }
}

/// A grid of cards for a list of entries. Returns the index that was clicked.
///
/// One renderer for search results, artist shelves and home shelves — they are all lists of
/// [`Entry`], and three near-identical grid loops is how the artist page ended up losing five of
/// its shelves in the first place.
#[allow(clippy::too_many_arguments)]
pub(crate) fn entry_grid(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    now: &crate::player::NowPlaying,
    items: &[Entry],
    ctx: &ReleaseCtx<'_>,
) -> GridOutcome {
    let mut clicked = None;
    let mut played = None;
    let mut link = None;
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        let card_w = grid_card_w(ui);
        for (i, item) in items.iter().enumerate() {
            let card = grid_card(ui, palette, images, backend, now, item, ctx, card_w);
            if action.is_none() {
                action = card.action;
            }
            if link.is_none() {
                link = card.link;
            }
            if card.play {
                played = Some(i);
            } else if card.clicked {
                clicked = Some(i);
            }
        }
    });
    GridOutcome {
        clicked,
        played,
        link,
        action,
        more: false,
    }
}

/// One card in a grid or carousel: shape, art, menu and click, shared so the two layouts
/// cannot drift apart.
#[allow(clippy::too_many_arguments)]
fn grid_card(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    now: &crate::player::NowPlaying,
    item: &Entry,
    ctx: &ReleaseCtx<'_>,
    width: f32,
) -> CardOutcome {
    // A video still is 16:9: it gets the wide card and a server-side 16:9 crop, so it
    // looks like a video instead of a squashed square.
    let shape = match item {
        Entry::Video(t) if t.wide => widgets::ArtShape::Wide,
        _ if item.is_artist() => widgets::ArtShape::Circle,
        _ => widgets::ArtShape::Square,
    };
    // A song/video card is active the same way a row is. An album has no id of its own on
    // `now.track` — but every track already carries the album it belongs to, which is exactly
    // the signal needed here. A playlist has no such per-track field (the same song can sit in
    // many), so its own branch below reads `now.playlist_playing` instead.
    let playing = match item {
        Entry::Song(t) | Entry::Video(t) => {
            now.track
                .as_ref()
                .is_some_and(|nt| nt.video_id == t.video_id)
                && !now.paused
        }
        Entry::Album(a) => {
            now.track
                .as_ref()
                .is_some_and(|nt| nt.album_id.as_deref() == Some(a.id.as_str()))
                && !now.paused
        }
        // No per-track field names a playlist the way `album_id` names an album, so this
        // reads the cache `NowPlaying::playlist_playing`'s own doc comment explains.
        Entry::Playlist(p) => {
            now.playlist_playing.as_ref().is_some_and(|(id, ids)| {
                id == &p.id
                    && now
                        .track
                        .as_ref()
                        .is_some_and(|nt| ids.contains(&nt.video_id))
            }) && !now.paused
        }
        _ => false,
    };
    // Grids are not virtualised — every card in the shelf runs this loop, on- or off-screen —
    // so skip the texture request *and* the `subtitle()` string build for the ones scrolled
    // out of view. The card's size is fixed regardless of its subtitle, so layout is unmoved.
    let visible = card_is_visible(ui, width, shape);
    let art_pt = width - 16.0;
    let art = visible
        .then(|| {
            if shape == widgets::ArtShape::Wide {
                art_texture_wide(images, backend, ui.ctx(), item.thumbnail(), art_pt)
            } else {
                art_texture(images, backend, ui.ctx(), item.thumbnail(), art_pt)
            }
        })
        .flatten();
    // Every card gets the menu its kind deserves. Cards used to have none, which is
    // why right-click worked on an artist page's Top songs and did nothing on its
    // Videos shelf — the same songs, drawn as cards.
    let menu = match item {
        Entry::Song(t) | Entry::Video(t) => Some(widgets::CardMenu::Track(widgets::RowMenu {
            track: t,
            playlist_id: None,
            playlists: ctx.playlists,
            signed_in: ctx.signed_in,
            queue_index: None,
        })),
        Entry::Artist(a) => Some(widgets::CardMenu::Artist(a)),
        // A release card builds the page menu from what it knows; the tracks fill in once the
        // right-click prefetch below lands them in the caches.
        Entry::Album(a) => {
            let cached = ctx.albums.get(&a.id);
            Some(widgets::CardMenu::Release(widgets::ReleaseMenu {
                page_id: &a.id,
                tracks: cached.map(|al| al.tracks.as_slice()).unwrap_or_default(),
                radio_id: cached.and_then(|al| al.like_id.as_deref()),
                artist_id: None,
                playlists: ctx.playlists,
                signed_in: ctx.signed_in,
                share_url: crate::model::album_url(&a.id),
                manage: false,
                remove_from_library: false,
                tracks_pending: cached.is_none().then_some(PageKind::Album),
            }))
        }
        Entry::Playlist(p) => {
            let cached = ctx.rail.get(&p.id);
            Some(widgets::CardMenu::Release(widgets::ReleaseMenu {
                page_id: &p.id,
                tracks: cached
                    .map(|pl| pl.tracks.items.as_slice())
                    .unwrap_or_default(),
                radio_id: matches!(p.source, crate::model::PlaylistSource::YouTube)
                    .then_some(p.id.as_str()),
                artist_id: p.author_id.as_deref(),
                playlists: ctx.playlists,
                signed_in: ctx.signed_in,
                share_url: crate::model::playlist_url(&p.id),
                manage: p.editable,
                remove_from_library: false,
                tracks_pending: (cached.is_none()
                    && matches!(p.source, crate::model::PlaylistSource::YouTube))
                .then_some(PageKind::Playlist),
            }))
        }
        Entry::Mood(_) => None,
    };
    let subtitle = if visible {
        item.subtitle()
    } else {
        String::new()
    };
    // A song/video card body obeys double-click-to-play: a single click selects, a double click
    // plays. Album, playlist and artist cards always open on a single click — navigation is not
    // playback — and every card's corner play button stays a single click.
    let track_id = match item {
        Entry::Song(t) | Entry::Video(t) => Some(t.video_id.as_str()),
        _ => None,
    };
    let selected = track_id.is_some_and(|id| widgets::is_selected(ui.ctx(), id));
    // The album a song card opens. The card body is the target now, so the title carries no
    // separate link of its own.
    let album_id = match item {
        Entry::Song(t) | Entry::Video(t) => t.album_id.as_deref(),
        _ => None,
    };
    let hit = widgets::art_card(
        ui,
        palette,
        width,
        art,
        item.title(),
        &subtitle,
        shape,
        menu,
        playing,
        selected,
    );
    // A song card opens the album (or single) it belongs to, the same way an album, playlist or
    // artist card opens its page: the body click navigates and the corner play button is what
    // plays it in place. Only a song with no album id — rare now that the menu fills them in —
    // keeps the old play/select click, because it has no page to open. Videos carry no album id,
    // so their behaviour is unchanged.
    let navigated = album_id.is_some() && hit.response.clicked();
    let link = if navigated {
        album_id.map(|id| Route::Album(id.to_owned()))
    } else {
        None
    };
    let clicked = if navigated {
        false
    } else {
        match track_id {
            Some(id) => widgets::activate(ui.ctx(), &hit.response, id),
            None => hit.response.clicked(),
        }
    };
    // Right-clicking a release opens its menu; fetch its tracks once so the mix, queue and save
    // items can appear without leaving the card — the same fill-in the rail's menu does.
    if hit.response.secondary_clicked() {
        match item {
            Entry::Album(a) if !ctx.albums.contains_key(&a.id) => backend.fetch_album(a.id.clone()),
            Entry::Playlist(p)
                if matches!(p.source, crate::model::PlaylistSource::YouTube)
                    && !ctx.rail.contains_key(&p.id) =>
            {
                backend.fetch_playlist(p.id.clone());
            }
            _ => {}
        }
    }
    CardOutcome {
        clicked,
        play: hit.play,
        link,
        action: hit.action,
    }
}

/// What one card reports.
struct CardOutcome {
    clicked: bool,
    /// See `widgets::CardClick::play` — wins over `clicked`.
    play: bool,
    /// The card's title link was clicked: navigate here. Wins over `clicked`.
    link: Option<Route>,
    action: Option<RowAction>,
}

/// What a card grid reports: the card that was clicked, and any menu choice.
pub(crate) struct GridOutcome {
    pub clicked: Option<usize>,
    /// The card whose corner play button specifically was clicked — wins over `clicked`, the
    /// same way a row's title link wins over its own click.
    pub played: Option<usize>,
    /// A card's title link was clicked: navigate here. Wins over `played` and `clicked`.
    pub link: Option<Route>,
    pub action: Option<RowAction>,
    /// The carousel's More pill was pressed. Grids have no More pill, so theirs is `false`.
    pub more: bool,
}

/// One page step inside a carousel: never leaves `0..=max`, so the arrows cannot strand the
/// row halfway past either end. Pure, so the bounds are unit-testable without an egui context.
///
/// Steps land on whole pages — `page` is exactly one screenful of cards, gap included — so
/// every page starts on a card's edge, and the last one is pulled back to end on the last card.
fn carousel_step(off: f32, dir: f32, page: f32, view: f32, total: f32) -> f32 {
    let max = (total - view).max(0.0);
    let from = if page > 0.0 {
        (off / page).round()
    } else {
        0.0
    };
    ((from + dir) * page).clamp(0.0, max)
}

/// Where a carousel scrolled to `off` belongs after its cards change size: the card that was
/// first in view (by the old `pitch`, card plus gap) keeps the left edge at the new `pitch`.
fn carousel_refit(off: f32, old_pitch: f32, new_pitch: f32, max: f32) -> f32 {
    let first = if old_pitch > 0.0 {
        (off / old_pitch).round()
    } else {
        0.0
    };
    (first * new_pitch).clamp(0.0, max)
}

/// How many cards a carousel shows across `view`, and how wide each is, so a whole number of
/// them fills the row exactly — no card cut in half at the right edge. The count is the one
/// nearest to cards of `base` width, so cards stay about their usual size and only stretch or
/// shrink by the fraction it takes to fit.
pub(crate) fn carousel_fit(view: f32, gap: f32, base: f32) -> (usize, f32) {
    let n = ((view + gap) / (base + gap)).round().max(1.0);
    (n as usize, ((view - (n - 1.0) * gap) / n).max(1.0))
}

/// A shelf as a horizontal carousel: the reference's shape, with ‹ › arrows scrolling by a
/// page and a More pill where the caller has somewhere for it to go.
///
/// The scroll offset lives in temp storage under `salt`, read back from the scroll area every
/// frame — so dragging the row by hand and stepping it with the arrows never disagree. The
/// offset is only *set* on an arrow frame; pinning it every frame would snap drags back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn carousel(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    now: &crate::player::NowPlaying,
    title: &str,
    strapline: Option<&str>,
    salt: impl std::hash::Hash + std::fmt::Debug + Copy,
    items: &[Entry],
    ctx: &ReleaseCtx<'_>,
    more: Option<Route>,
    more_button: MoreButton,
) -> GridOutcome {
    let mut clicked = None;
    let mut played = None;
    let mut link = None;
    let mut action = None;
    let mut more_pressed = false;

    let id = ui.id().with(("ytm-carousel-off", salt));
    let off: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    let view = ui.available_width();
    let gap = 8.0;
    // Cards sized so the row holds a whole number of them; the arrows then move by exactly
    // that many, so no page ever shows half a card at its edge.
    let (per_page, card_w) = carousel_fit(view, gap, CARD_W);
    let page = per_page as f32 * (card_w + gap);
    let total = items.len() as f32 * card_w + items.len().saturating_sub(1) as f32 * gap;
    let max = (total - view).max(0.0);

    // The row's width changed — the window resized, the queue or lyrics opened or closed — so
    // the cards did too, and an offset kept in points would now land partway into one. Keep the
    // card that was first in view at the left edge instead.
    let width_id = id.with("width");
    let last_card_w: Option<f32> = ui.ctx().data(|d| d.get_temp(width_id));
    ui.ctx().data_mut(|d| d.insert_temp(width_id, card_w));
    if let Some(old_w) = last_card_w
        && (old_w - card_w).abs() > 0.5
    {
        let refit = carousel_refit(off, old_w + gap, card_w + gap, max);
        ui.ctx().data_mut(|d| d.insert_temp(id, refit));
    }

    if let Some(line) = strapline {
        shelf_strapline(ui, palette, line);
    }
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .font(theme::bold(theme::SECTION_PT))
                .color(palette.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let left_on = off > 1.0;
            let right_on = off < max - 1.0;
            // No arrows when the whole shelf fits: there is nowhere for them to go. A narrower
            // window (or the queue or lyrics opening) that leaves cards out of view brings them
            // in; once there is somewhere to scroll, both show, the unusable one dimmed.
            // Not drawn at all (not merely hidden), so "Show all" moves to the edge they held.
            if max > 1.0 {
                ui.add_enabled_ui(right_on, |ui| {
                    if widgets::icon_button(
                        ui,
                        palette,
                        crate::icons::Icon::ChevronRight,
                        14.0,
                        false,
                    )
                    .clicked()
                    {
                        let next = carousel_step(off, 1.0, page, view, total);
                        ui.ctx().data_mut(|d| d.insert_temp(id, next));
                    }
                });
                ui.add_enabled_ui(left_on, |ui| {
                    if widgets::icon_button(
                        ui,
                        palette,
                        crate::icons::Icon::ChevronLeft,
                        14.0,
                        false,
                    )
                    .clicked()
                    {
                        let next = carousel_step(off, -1.0, page, view, total);
                        ui.ctx().data_mut(|d| d.insert_temp(id, next));
                    }
                });
            }
            if more.is_some() {
                let pressed = match more_button {
                    MoreButton::Pill(label) => widgets::pill_button(ui, palette, label).clicked(),
                    MoreButton::ShowAll => {
                        ui.add_space(6.0);
                        shelf_show_all(ui, palette).clicked()
                    }
                };
                if pressed {
                    more_pressed = true;
                }
            }
        });
    });
    ui.add_space(4.0);

    // The arrows write the target into temp storage; the scroll area below reads it back.
    // Setting `.scroll_offset` only on the click frame is what keeps hand drags working.
    let want: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    let mut area = egui::ScrollArea::horizontal().id_salt(("ytm-carousel", salt));
    // The scroll area's own offset is persisted with the rest of egui's memory, so a fresh
    // launch would reopen the row wherever last session left it — other shelves, other card
    // widths, usually partway into a card. The first frame this session sees the row, it
    // starts at the beginning.
    if last_card_w.is_none() || (want - off).abs() > f32::EPSILON {
        area = area.scroll_offset(egui::vec2(want, 0.0));
    }
    let out = area.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, item) in items.iter().enumerate() {
                let card = grid_card(ui, palette, images, backend, now, item, ctx, card_w);
                if action.is_none() {
                    action = card.action;
                }
                if link.is_none() {
                    link = card.link;
                }
                if card.play {
                    played = Some(i);
                } else if card.clicked {
                    clicked = Some(i);
                }
            }
        });
    });
    ui.ctx().data_mut(|d| d.insert_temp(id, out.state.offset.x));

    GridOutcome {
        clicked,
        played,
        link,
        action,
        more: more_pressed,
    }
}

/// Where an entry goes when clicked, for the kinds that navigate.
/// A mood shelf's entry as the chip grid wants it: its label and its stripe colour.
fn entry_chip(item: &Entry) -> (&str, Option<[u8; 3]>) {
    match item {
        Entry::Mood(m) => (m.title.as_str(), m.colour),
        other => (other.title(), None),
    }
}

pub(crate) fn entry_route(item: &Entry) -> Option<Route> {
    match item {
        Entry::Artist(a) => Some(Route::Artist(a.id.clone())),
        Entry::Album(a) => Some(Route::Album(a.id.clone())),
        Entry::Playlist(p) => Some(Route::Playlist(p.id.clone())),
        Entry::Mood(m) => Some(Route::Mood(m.params.clone())),
        Entry::Song(_) | Entry::Video(_) => None,
    }
}

/// The mood chip grid at `width`: columns, chip width, chip height and gutter. Six across on a
/// wide window, fewer when narrow — the reference's grid, not a wrap. Each chip is a hair under
/// its exact share, as [`grid_card_w`]'s cards are: at the exact share float rounding wrapped
/// the sixth chip of every row onto a line of its own, and wide windows got five.
pub(crate) fn mood_chip_grid(width: f32) -> (usize, f32, f32, f32) {
    let cols = ((width / 180.0).floor() as usize).clamp(2, 6);
    let gap = 8.0;
    let chip_w = (width - (cols - 1) as f32 * gap) / cols as f32 - 0.5;
    (cols, chip_w, 44.0, gap)
}

/// "Moods & genres" as a grid of coloured chips — the site's own shape, not cards and not
/// default buttons. Each chip carries its stripe on the left; a chip whose source named no
/// colour gets the dim outline instead of an invented one.
pub(crate) fn mood_chips<'a>(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    // Title and stripe, not entries: the moods page holds `MoodRef`s while a mood shelf holds
    // `Entry`s, and converting one into the other every frame copied every chip's text for a
    // widget that only ever reads it.
    items: impl Iterator<Item = (&'a str, Option<[u8; 3]>)>,
) -> Option<usize> {
    let mut clicked = None;
    let (_, chip_w, chip_h, gap) = mood_chip_grid(ui.available_width());
    ui.horizontal_wrapped(|ui| {
        // Wrapped, not a grid widget: every chip is identical, so wrapping lands on the
        // column count by itself.
        ui.spacing_mut().item_spacing.x = gap;
        for (i, (title, colour)) in items.enumerate() {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(chip_w, chip_h), egui::Sense::click());
            if ui.is_rect_visible(rect) {
                let t = widgets::lit(ui, &response);
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(theme::radii().control),
                    palette.glass(widgets::mix(palette.surface, palette.surface_hover, t)),
                );
                match colour {
                    Some([r, g, b]) => {
                        // The stripe is the chip's own left edge: the chip's shape, cut 4pt
                        // wide, so it follows the chip's curve. A bar with corners of its own
                        // squared off the chip's at any radius wider than the stripe.
                        let radius = theme::radii().control;
                        let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(4.0, chip_h));
                        ui.painter()
                            .with_clip_rect(stripe.intersect(ui.clip_rect()))
                            .rect_filled(
                                egui::Rect::from_min_size(
                                    rect.min,
                                    egui::vec2(2.0 * f32::from(radius) + 4.0, chip_h),
                                ),
                                egui::CornerRadius {
                                    nw: radius,
                                    sw: radius,
                                    ne: 0,
                                    se: 0,
                                },
                                egui::Color32::from_rgb(r, g, b),
                            );
                    }
                    None => {
                        ui.painter().rect(
                            rect,
                            egui::CornerRadius::same(theme::radii().control),
                            egui::Color32::TRANSPARENT,
                            egui::Stroke::new(1.0, palette.outline),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                let label = widgets::truncate_to(ui, title, theme::medium(13.0), chip_w - 24.0);
                ui.painter().text(
                    rect.left_center() + egui::vec2(14.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    label,
                    theme::medium(13.0),
                    widgets::mix(palette.secondary, palette.text, t),
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
            if response.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// The same shelf with its card grid as a carousel — the reference's shape. Track lists and
/// mood chips are unchanged; only grids scroll sideways. `more` is the carousel's More pill
/// destination, if the caller has somewhere for it to go.
pub(crate) fn shelf_carousel(
    app: &mut crate::app::App,
    ui: &mut egui::Ui,
    page_salt: &str,
    shelf: &Shelf,
    more: Option<Route>,
    more_button: MoreButton,
) {
    if shelf.items.is_empty() {
        return;
    }
    if shelf.is_mood_list() {
        section(ui, &app.palette, &shelf.title);
        if let Some(i) = mood_chips(ui, &app.palette, shelf.items.iter().map(entry_chip))
            && let Some(route) = shelf.items.get(i).and_then(entry_route)
        {
            app.navigate(route);
        }
        return;
    }

    let signed_in = app.signed_in();
    let grid = {
        let crate::app::App {
            palette,
            images,
            backend,
            now,
            library,
            rail_cache,
            album_cache,
            ..
        } = app;
        let ctx = ReleaseCtx {
            playlists: &library.playlists.items,
            signed_in,
            rail: rail_cache,
            albums: album_cache,
        };
        carousel(
            ui,
            palette,
            images,
            backend,
            now,
            &shelf.title,
            shelf.strapline.as_deref(),
            // Hashed into the carousel's scroll-offset id — a tuple hashes without the
            // per-shelf-per-frame `format!` a joined string needed.
            (page_salt, shelf.title.as_str()),
            &shelf.items,
            &ctx,
            more.clone(),
            more_button,
        )
    };
    if grid.more
        && let Some(route) = more
    {
        app.navigate(route);
    }
    finish_grid(app, shelf, grid);
}

/// One of an artist's shelves (Albums, Singles & EPs, Videos, Live performances, Featured on,
/// Playlists, Fans might also like) — never a mood or track list; Top Songs is its own
/// dedicated section, not one of these. Not yet expanded, it's the plain wrapped card grid with
/// a low-key "Show all" in the top right of its own heading, in place of a separate button or a
/// carousel's arrows — one flowing page, not a row of mini-carousels to operate individually.
/// Once "Show all" lands, the same grid draws under the same heading, "Show all" simply gone —
/// no renamed heading, no colour change, no box, so the row that was already on screen doesn't
/// move: both states build the heading the same way, and the title's own height is what sets
/// the row's height either way.
pub(crate) fn artist_shelf(app: &mut crate::app::App, ui: &mut egui::Ui, shelf: &Shelf) {
    if shelf.items.is_empty() {
        return;
    }
    let mut show_all = false;
    // A bit more room than a plain `section()` gets — these are separate categories on the
    // page, not paragraphs of the same one.
    ui.add_space(20.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(&shelf.title)
                .font(theme::bold(theme::SECTION_PT))
                .color(app.palette.text),
        );
        if !shelf.expanded && shelf.more_params.is_some() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if shelf_show_all(ui, &app.palette).clicked() {
                    show_all = true;
                }
            });
        }
    });
    ui.add_space(4.0);
    draw_shelf_grid(app, ui, shelf);
    if show_all {
        app.expand_artist_shelf(&shelf.title);
    }

    if shelf.expanded {
        return;
    }
    // Loading/error feedback for whichever shelf is currently expanding — only ever this one,
    // since `App::expand_artist_shelf` overwrites the single in-flight slot on a second click.
    let expanding = app
        .artist_expand
        .as_ref()
        .is_some_and(|(_, t)| *t == shelf.title);
    if expanding && app.artist_expand_loading {
        // `draw_shelf_grid` always renders through `entry_grid` — a card grid, never rows.
        ui.add_space(4.0);
        widgets::skeleton_grid(ui, &app.palette, 1, widgets::ArtShape::Square);
    }
    if expanding && let Some(err) = app.artist_expand_error.clone() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(err)
                .font(theme::regular(12.0))
                .color(app.palette.danger),
        );
    }
}

/// "Show all", low-key — plain text with a hover underline, not a button, so a shelf's heading
/// doesn't compete with its own title for attention. Kept shorter than the title's own line so
/// it never grows the heading row `artist_shelf` builds around it.
fn shelf_show_all(ui: &mut egui::Ui, palette: &theme::Palette) -> egui::Response {
    let font = theme::regular(12.0);
    let text = "Show all";
    let w = widgets::text_width(ui, text, &font);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, 14.0), egui::Sense::click());
    let colour = if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        palette.text
    } else {
        palette.secondary
    };
    crate::emoji::paint_line(
        ui,
        egui::pos2(rect.left(), rect.center().y),
        text,
        font,
        colour,
    );
    if response.hovered() {
        ui.painter().hline(
            rect.left()..=rect.right(),
            rect.bottom(),
            egui::Stroke::new(1.0, palette.text),
        );
    }
    response
}

fn draw_shelf_grid(app: &mut crate::app::App, ui: &mut egui::Ui, shelf: &Shelf) {
    let signed_in = app.signed_in();
    let grid = {
        let crate::app::App {
            palette,
            images,
            backend,
            now,
            library,
            rail_cache,
            album_cache,
            ..
        } = app;
        let ctx = ReleaseCtx {
            playlists: &library.playlists.items,
            signed_in,
            rail: rail_cache,
            albums: album_cache,
        };
        entry_grid(ui, palette, images, backend, now, &shelf.items, &ctx)
    };
    finish_grid(app, shelf, grid);
}

/// The colour of the wash at `t`, where 0 is its top edge and 1 is where it has gone.
///
/// Two segments, so the tint holds near the top and then falls away: a soft accent to a
/// fainter one over the first half, that to nothing over the second.
fn wash_stop(panel: egui::Color32, wash: egui::Color32, t: f32, alpha: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let strong = panel.lerp_to_gamma(wash, 0.38);
    let mid = panel.lerp_to_gamma(wash, 0.18);
    let stop = if t <= 0.5 {
        strong.lerp_to_gamma(mid, t * 2.0)
    } else {
        mid.lerp_to_gamma(egui::Color32::TRANSPARENT, (t - 0.5) * 2.0)
    };
    // As opaque as the card it sits on, so a translucent page does not wear an opaque band
    // across its header. At `Off` this is `gamma_multiply(1.0)` — an identity.
    stop.gamma_multiply(alpha)
}

/// A wash of the active scheme's accent across the top of the content card.
///
/// Painted first inside the page's scroll content, so the tint travels with the header and
/// scrolls away instead of lingering as a background. A smooth gradient mesh, full-bleed — it
/// reaches the card's own left and right edges, past the content's side padding, which needs a
/// painter whose clip rect is grown to match. The topmost slice is a rounded cap in the card's
/// own radius, so a square mesh cannot poke past the card's corners onto the gutter, and it is
/// pinned to the card's top edge rather than to the scrolled content — scroll and the *colour*
/// there fades, as the gradient passing upward under a fixed corner should. Tinted at the top,
/// gone a quarter of the way down. Always painted, so it no longer waits on the cover decoding
/// before it appears.
///
/// `wash` is the colour it blends from `panel` down to nothing — the fixed accent, or (see
/// [`header_wash_colour`]) the current cover's own dominant colour.
pub(crate) fn header_wash(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    content: egui::Rect,
    top: f32,
    wash: egui::Color32,
) {
    let h = content.height();
    if h <= 0.0 {
        return;
    }
    // The tint covers the header — cover, caption and the action row under them — and fades
    // out just past it, the band the reference sets its header in. Proportional within bounds,
    // so a short window still gets a band and a tall one does not tint half its list.
    let bottom = top + (h * 0.4).clamp(240.0, 360.0);
    // The card's own edges: the page's content rect grown by the side padding the card holds it
    // in. `content` comes from the panel itself (`App::content_rect`) rather than from this ui's
    // clip rect, which a scroll area builds from its viewport and its parent's clip and which
    // is several points out on every side.
    let card = egui::Rect::from_min_max(
        egui::pos2(content.min.x - theme::PAGE_INSET_X, content.min.y),
        egui::pos2(content.max.x + theme::PAGE_INSET_X, content.max.y),
    );
    // The card's top edge is the content's: there is no top margin left for the wash to cover,
    // so the tint starts where the card does. Above `top` the colour simply holds at full
    // strength; scroll and the gradient travels up under a cap that stays put.
    let start = card.top();
    if card.width() <= 0.0 || bottom <= start {
        return;
    }
    let span = bottom - top;
    let colour = |y: f32| wash_stop(palette.panel, wash, (y - top) / span, palette.card_alpha);

    // A painter built from scratch, not `ui.painter().with_clip_rect(card)`: that *intersects*
    // with the ui's clip, which inside a scroll area is the viewport, inset by the card's side
    // padding — so the wash was clipped at the sides and left a bare strip of panel colour down
    // each edge. This one is clipped to the card and nothing else, and the card never reaches
    // past the panel it lives in.
    let painter = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), card);
    // The cap's rounding, tessellated straight into the gradient mesh. A `rect_filled` cap
    // would not do: egui clamps a shape's corner radius to half its height, and a cap one
    // radius tall renders its corners at half the card's rounding — visibly squarer than the
    // panel it sits on. Walking the quarter-circle as mesh rows keeps the true radius and the
    // gradient continuous — no seam where a flat cap used to meet the mesh.
    let rad = (theme::radii().panel as f32).min(bottom - start);
    // (y, left x, right x) — pulled in along the arc, full width once past it.
    let mut rows: Vec<(f32, f32, f32)> = Vec::new();
    const ARC_STEPS: usize = 10;
    for i in 0..=ARC_STEPS {
        let y = start + rad * i as f32 / ARC_STEPS as f32;
        let inset = widgets::corner_inset(rad, y - start);
        rows.push((y, card.left() + inset, card.right() - inset));
    }
    let kink = top + span * 0.5;
    let mut lower: Vec<f32> = vec![start + rad];
    if kink > start + rad && kink < bottom {
        lower.push(kink);
    }
    if bottom > start + rad {
        lower.push(bottom);
    }
    for y in lower {
        rows.push((y, card.left(), card.right()));
    }
    let mut mesh = egui::Mesh::default();
    for &(y, xl, xr) in &rows {
        for x in [xl, xr] {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: egui::pos2(x, y),
                uv: egui::epaint::WHITE_UV,
                color: colour(y),
            });
        }
    }
    for row in 0..rows.len() - 1 {
        let a = (row * 2) as u32;
        mesh.add_triangle(a, a + 2, a + 1);
        mesh.add_triangle(a + 1, a + 2, a + 3);
    }
    painter.add(mesh);
}

/// The header wash's colour: the current page's cover-art dominant colour, when "Match header
/// colour to the cover art" is on and that art has finished decoding — the fixed accent
/// otherwise. Self-healing: on the frame a page opens the art usually hasn't decoded yet, so
/// this falls back to the accent that frame; once `page_header`'s own `art_texture()` call
/// finishes the fetch, `images.dominant(url)` starts returning `Some` and the wash picks up
/// the art's colour on the next frame, the same way the cover itself fades in once ready.
pub(crate) fn header_wash_colour(
    app: &crate::app::App,
    art_url: Option<&std::sync::Arc<str>>,
) -> egui::Color32 {
    if app.settings.adaptive_art_color
        && let Some(url) = art_url
        && let Some([r, g, b]) = app.images.dominant(url)
    {
        egui::Color32::from_rgb(r, g, b)
    } else {
        app.palette.accent
    }
}

/// What a detail header is built from. Album and playlist fill all of it; the artist page
/// fills art and texts and draws its own buttons, since Shuffle/Mix/Subscribe are not
/// Play/Save/•••.
pub(crate) struct PageHeader<'a> {
    pub art_url: Option<&'a std::sync::Arc<str>>,
    pub art_size: f32,
    pub circle: bool,
    /// The type line (`ALBUM · 1995`). Empty hides it.
    pub kind_label: &'a str,
    pub title: &'a str,
    /// A lowkey one-line description, between the title and the credits (Spotify-style). Only
    /// playlists fill it.
    pub description: Option<&'a str>,
    /// The credited artists: each is linked where the source gave an id, so a multi-artist
    /// album links every name rather than only the first. Empty for pages with no artist line.
    pub artists: &'a [crate::model::ArtistLink],
    /// The second line where there is no artist to name: the artist page's audience.
    pub second_plain: Option<&'a str>,
    /// The quiet third line.
    pub meta: Option<&'a str>,
    /// When set, prefixes the meta line with this linked name instead of drawing a separate
    /// artist/owner row above it — the playlist page's creator. `None` (album, artist) keeps
    /// the separate-line behaviour above.
    pub owner: Option<&'a crate::model::ArtistLink>,
    /// Air between the title's painted ink and the artist/owner/audience line's capitals,
    /// on top of the header's own base. The album's credit is the identity of the page in a
    /// way the playlist's stats aren't, so it hugs the name; a page whose credit line is a
    /// quieter aside (the playlist's owner, the artist's audience count) asks for more.
    pub credit_air: f32,
    /// Play button diameter. `None` draws no Play — the artist page brings its own buttons.
    pub play_size: Option<f32>,
    /// Whether the track currently loaded is one of this page's own and is actively playing
    /// (not paused) — paints the button as Pause rather than Play. Ignored when `play_size` is
    /// `None`.
    pub playing: bool,
    /// Save/Saved chip. `None` draws none; the label follows the state, as YouTube writes it.
    pub save_in_library: Option<bool>,
    /// Whether the ••• button is drawn. Its menu stays the page's own.
    pub dots: bool,
    /// Id salt for the artist link, so neighbouring headers never share an interaction id.
    pub salt: &'static str,
}

/// What `page_header` reports: the control row's clicks, the dots button for the page's own
/// menu, a clicked artist link, and the text anchor for pages with custom buttons.
pub(crate) struct HeaderOutcome {
    pub play: bool,
    pub save: bool,
    pub dots: Option<egui::Response>,
    pub link: Option<Route>,
    /// The art as it was actually drawn — shrunken from the request when the card is narrow.
    pub art_bottom: f32,
    /// Where the art was drawn, for a page that paints its own cover over it.
    pub art_rect: egui::Rect,
    /// The point just past the last button drawn (Play, Save or •••, whichever was last),
    /// vertically centred on the row — where a caller-specific extra control (the playlist
    /// page's sort icon) can paint itself on the same baseline. `None` when no control row was
    /// drawn at all (every one of `play_size`/`save_in_library`/`dots` was off).
    pub controls_end: Option<egui::Pos2>,
}

// The detail header's text metrics, shared by [`page_header`] and its placeholder
// [`skeleton_page_header`] so the two lay their lines out on the same centres.
//
// `KIND_CENTRE`: where the label's own line sits, from the art's top. `KIND_BASELINE`:
// that centre plus Inter's baseline, 0.364 of the size below it.
const KIND_CENTRE: f32 = 9.0;
const KIND_BASELINE: f32 = KIND_CENTRE + 12.0 * 0.364;
const KIND_TO_TITLE_AIR: f32 = 8.0;
// The base air under the title's ink, and half a 13pt Inter cap height: where the credit
// line's capitals sit above its own centre.
const CREDIT_AIR: f32 = 4.0;
const CREDIT_CAP_HALF: f32 = 4.7;
// A description is an aside, not the credit: at the credit's own air it read as a subtitle
// glued to a 56pt name.
const DESCRIPTION_AIR: f32 = 6.0;
// Centre gap between the two 13pt lines below the title — the artist line and the stats
// line on an album, a description and the credits on a playlist. A 13pt line's box carries
// ~9.4pt of lead, so 19pt of centres reads as ~10pt of space between the ink: one block,
// not two paragraphs.
const LINE_GAP: f32 = 19.0;

/// One header for the album, playlist and artist pages: wash, art, type line, title, artist
/// line, meta, and — for the releases — the Play / Save / ••• row on its own baseline.
///
/// Painted absolutely, like the headers it replaces, so callers reserve the space the same
/// way: `add_space(art_size + 12.0)` afterwards.
pub(crate) fn page_header(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    images: &mut Images,
    backend: &Backend,
    h: PageHeader<'_>,
) -> HeaderOutcome {
    let available = ui.available_width();
    // The art yields before the title does. A card narrowed by the queue panel (or a small
    // window) otherwise hands the name single-letter ellipsis: "sl…" under a full-size art and
    // a gutter it never asked to keep. The art shrinks — to its floor — first, so a shrunken
    // art and a readable name survive; only then does the title truncate, and only when the
    // card is honestly too small for both.
    let art_size = h
        .art_size
        .min((available - 20.0 - 8.0 - 140.0).max(HEADER_ART_FLOOR_PT));
    let art = art_texture(images, backend, ui.ctx(), h.art_url, h.art_size);
    let art_rect = egui::Rect::from_min_size(
        ui.cursor().min + egui::vec2(0.0, theme::HEADER_TOP_PAD),
        egui::vec2(art_size, art_size),
    );
    widgets::paint_art(
        ui,
        palette,
        art_rect,
        art,
        if h.circle {
            widgets::ArtShape::Circle
        } else {
            widgets::ArtShape::Square
        },
    );

    let gutter = 20.0;
    let tx = art_rect.right() + gutter;
    // Relative, not `available - tx`: `tx` is an absolute x, `available` a width — mixing
    // them undercounted the text by the page's whole left offset (~300pt), which is why
    // headers truncated while the window still had room, and lost their text entirely
    // once that subtraction went negative.
    let text_w = (available - art_size - gutter - 8.0).max(0.0);
    // The title takes the largest size that still fits its column, down to a floor: a long
    // album or playlist name steps the type down rather than ellipsizing, so a one-word name
    // stays loud and a sentence-long one stays whole.
    let title_size = widgets::fit_size(
        ui,
        h.title,
        text_w,
        DETAIL_TITLE_MAX,
        DETAIL_TITLE_MIN,
        theme::bold,
    );
    let title_font = theme::bold(title_size);
    // The type line costs a row where it shows; without it the title sits higher — and with a
    // large art it still starts low enough to sit in the circle rather than float above it.
    //
    // With a type line the title hangs off that line's baseline instead of off a size-scaled
    // constant: the cap line then sits the same distance below the label at any size, and a
    // bigger name grows *downward* from it rather than drifting away from the label it belongs
    // to. (The old `8 + size * 1.08` kept the line *box* centred, so the visible air between
    // label and name grew with the size — worst at the 40pt ceiling.)
    //
    let title_rel = if h.kind_label.is_empty() {
        title_size * 0.5
    } else {
        widgets::title_centre_under_label(KIND_BASELINE, KIND_TO_TITLE_AIR, title_size)
    };
    let shown_title = widgets::truncate_to(ui, h.title, title_font.clone(), text_w);
    // Where the painted title's ink actually ends, measured before anything is drawn so the
    // whole block can be placed first.
    let title_ink = widgets::ink_drop(ui, &shown_title, &title_font);
    // The text block sits on the art's bottom edge, as the reference sets it: label, name and
    // credits read as a caption to the cover rather than hanging off its top. Its height is
    // the same walk the painting below takes, run once without painting.
    let block_h = {
        let mut y = title_rel;
        let mut last = y;
        if h.description.is_some() {
            last = y + title_ink + CREDIT_AIR + DESCRIPTION_AIR + CREDIT_CAP_HALF + h.credit_air;
            y = last + LINE_GAP;
        } else {
            y += title_ink + CREDIT_AIR + CREDIT_CAP_HALF + h.credit_air;
        }
        if !h.artists.is_empty() || h.second_plain.is_some() {
            last = y;
            y += LINE_GAP;
        }
        if h.meta.is_some() {
            last = y;
        }
        // Half a 13pt line below the last centre: its descenders.
        last + 7.0
    };
    let text_top = art_rect.top() + (art_size - block_h).max(0.0);
    let mut y = text_top + title_rel;
    if !h.kind_label.is_empty() {
        ui.painter().text(
            egui::pos2(tx, text_top + KIND_CENTRE),
            egui::Align2::LEFT_CENTER,
            widgets::truncate_to(ui, h.kind_label, theme::semibold(12.0), text_w),
            theme::semibold(12.0),
            palette.dim,
        );
    }
    crate::emoji::paint_line(
        ui,
        egui::pos2(tx, y),
        &shown_title,
        title_font.clone(),
        palette.text,
    );
    // The last text line painted, so the controls' baseline stays under it whether or not
    // the header has an artist line and a meta line.
    let mut last_line = y;
    // The description, if any, sits between the title and the credits — one small line, the
    // way Spotify writes it, so it reads as an aside and never pushes the tracks down. Same
    // `secondary` colour as the merged owner/meta line below it: that line gets its own
    // distinction from a bold owner name, not from a separate colour tier here, so the two
    // read as one calm paragraph rather than clashing tones.
    //
    // `y` moves past the description's own row (not just the credits' own air, which is
    // measured for "title straight to the credits, no description in between") — playlists are
    // the only page that ever fills `description`, and with `y` left untouched here the
    // owner/meta line below used to land only 12pt under it, squeezed against a real line's
    // worth of text rather than given room for one.
    if let Some(description) = h.description {
        // The description hangs off the title's ink exactly as the credits do: the playlist is
        // the only page that fills it, and it reads as the first line of the same block.
        let dy = y + title_ink + CREDIT_AIR + DESCRIPTION_AIR + CREDIT_CAP_HALF + h.credit_air;
        ui.painter().text(
            egui::pos2(tx, dy),
            egui::Align2::LEFT_CENTER,
            widgets::truncate_to(ui, description, theme::regular(13.0), text_w),
            theme::regular(13.0),
            palette.secondary,
        );
        last_line = dy;
        y = dy + LINE_GAP;
    } else {
        // The credits hang off the title's ink, not its line box: the page's own air, but
        // never closer than the painted name allows. A name with no descenders ends at its
        // baseline, so the credits ride right up under it; a tail ("y", "p") reaches most of
        // the way down the box, and the credits keep that clearance. Hanging them off the box
        // instead floated them under a descent that was usually empty — and the bigger the
        // title, the more of it there was to float under.
        y += title_ink + CREDIT_AIR + CREDIT_CAP_HALF + h.credit_air;
    }
    let mut link = None;
    match (h.artists.is_empty(), h.second_plain) {
        (false, _) => {
            // Every credited artist is its own linked part, so a multi-artist album header
            // no longer collapses to one string with only the first name clickable.
            let mut subtitle = widgets::no_subtitle();
            fill_artists(&mut subtitle, h.artists);
            if let Some(target) = widgets::paint_subtitle(
                ui,
                palette,
                egui::pos2(tx, y),
                tx + text_w,
                &subtitle,
                None,
                h.salt,
                theme::regular(13.0),
                palette.secondary,
            ) {
                link = Some(link_route(target));
            }
            last_line = y;
            y += LINE_GAP;
        }
        (true, Some(plain)) => {
            ui.painter().text(
                egui::pos2(tx, y),
                egui::Align2::LEFT_CENTER,
                widgets::truncate_to(ui, plain, theme::regular(13.0), text_w),
                theme::regular(13.0),
                palette.secondary,
            );
            last_line = y;
            y += LINE_GAP;
        }
        // No line between title and meta: the meta takes the artist's slot, so the gap
        // below the title is the same 36 pt on a page that has no author to show. Before
        // this, the empty arm still advanced past a line that was never painted and a
        // playlist with no author wore a double gap under its name.
        (true, None) => {}
    }
    if let Some(meta) = h.meta {
        // The playlist page folds its creator into this line instead of a separate row above
        // it. The name is painted bold and full-brightness rather than routed through
        // `paint_subtitle` (which gives every part the same weight) — like Spotify's and Apple
        // Music's own playlist headers, the bold name is what makes this read as *the* credit
        // line rather than a continuation of the description above it.
        if let Some(owner) = h.owner {
            let name_font = theme::semibold(13.0);
            let stat_font = theme::regular(13.0);
            let shown_name = widgets::truncate_to(ui, &owner.name, name_font.clone(), text_w);
            if !shown_name.is_empty() {
                let name_w = widgets::text_width(ui, &shown_name, &name_font);
                if let Some(id) = owner.id.as_deref() {
                    let hit = egui::Rect::from_min_size(
                        egui::pos2(tx, y - 8.5),
                        egui::vec2(name_w, 17.0),
                    );
                    let r = ui.interact(hit, ui.id().with((h.salt, "owner")), egui::Sense::click());
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        ui.painter().hline(
                            tx..=(tx + name_w),
                            y + 7.5,
                            egui::Stroke::new(1.0, palette.text),
                        );
                    }
                    if r.clicked() {
                        link = Some(Route::Artist(id.to_owned()));
                    }
                }
                crate::emoji::paint_line(
                    ui,
                    egui::pos2(tx, y),
                    &shown_name,
                    name_font,
                    palette.text,
                );
                let sep_x = tx + name_w;
                let sep_w = widgets::text_width(ui, widgets::FIELD_SEP, &stat_font);
                let shown_meta = widgets::truncate_to(
                    ui,
                    meta,
                    stat_font.clone(),
                    (tx + text_w - sep_x - sep_w).max(0.0),
                );
                if !shown_meta.is_empty() {
                    ui.painter().text(
                        egui::pos2(sep_x, y),
                        egui::Align2::LEFT_CENTER,
                        widgets::FIELD_SEP,
                        stat_font.clone(),
                        palette.secondary,
                    );
                    crate::emoji::paint_line(
                        ui,
                        egui::pos2(sep_x + sep_w, y),
                        &shown_meta,
                        stat_font,
                        palette.secondary,
                    );
                }
            }
        } else {
            // `dim` is right when this line sits *below* a real credit line (Album's artists,
            // Artist's audience count) — a genuinely secondary stats row. But a playlist with
            // no owner (only ever Liked Music, whose header has no artist/audience line above
            // this at all — playlist always passes `artists: &[]`, `second_plain: None`) has
            // nothing else credit-shaped drawn, so this line *is* the credit line, same as the
            // bold-name branch above treats its own stats text: `secondary`, not the fainter
            // `dim`, or the same information reads visibly greyer than on every other playlist.
            let color = if h.artists.is_empty() && h.second_plain.is_none() {
                palette.secondary
            } else {
                palette.dim
            };
            ui.painter().text(
                egui::pos2(tx, y),
                egui::Align2::LEFT_CENTER,
                widgets::truncate_to(ui, meta, theme::regular(13.0), text_w),
                theme::regular(13.0),
                color,
            );
        }
        last_line = y;
    }

    // The bottom of the text column: the last row painted above, plus half a line of
    // breathing room.
    let text_bottom = last_line + 10.0;

    let mut outcome = HeaderOutcome {
        play: false,
        save: false,
        dots: None,
        link,
        art_bottom: art_rect.bottom(),
        art_rect,
        controls_end: None,
    };
    if h.play_size.is_none() && h.save_in_library.is_none() && !h.dots {
        return outcome;
    }
    // Play, Save, ••• on their own row under the art, at the content's left edge, as the
    // reference lays them out: the text beside the cover stays one caption, and the row never
    // has to squeeze into a text column the queue panel narrowed. The rect is the Play
    // button's own height plus its 1pt of air, so a bigger button grows the row.
    let control_h = h.play_size.map_or(46.0, |size| size + 2.0);
    let controls_rect = egui::Rect::from_min_size(
        egui::pos2(art_rect.left(), art_rect.bottom().max(text_bottom) + 16.0),
        egui::vec2(available - 8.0, control_h),
    );
    let mut controls = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(controls_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    if let Some(size) = h.play_size
        && widgets::play_button(&mut controls, palette, h.playing, size).clicked()
    {
        outcome.play = true;
    }
    controls.add_space(12.0);
    if let Some(in_library) = h.save_in_library {
        if widgets::chip(
            &mut controls,
            palette,
            if in_library { "Saved" } else { "Save" },
            in_library,
        )
        .clicked()
        {
            outcome.save = true;
        }
        controls.add_space(6.0);
    }
    // The ••• is the page's own menu trigger and is only drawn where the page has one to hang
    // off it — a page with controls but no menu (the liked list) draws Play and stops.
    let dots = h.dots.then(|| {
        widgets::icon_button(
            &mut controls,
            palette,
            crate::icons::Icon::Ellipsis,
            16.0,
            false,
        )
    });
    outcome.controls_end = Some(controls.min_rect().right_center());
    drop(controls);
    outcome.dots = dots;
    outcome.art_bottom = outcome.art_bottom.max(controls_rect.bottom() + 6.0);
    outcome
}

/// What a detail page's header will hold, for its placeholder: which lines the real header
/// paints and which controls follow Play. Whatever the route already settles is drawn for real —
/// a playlist's "Playlist", the liked page's "Liked songs" — and only what has to be fetched is
/// a grey shape.
pub(crate) struct HeaderSkeleton {
    /// The type line over the title.
    pub kind: HeaderKind,
    /// The title, where the page always has the same one. `None`: a bar, the name is coming.
    pub title: Option<&'static str>,
    /// The playlist's one-line description.
    pub description: bool,
    /// A credit line of its own under the title (the album's artists).
    pub credit_line: bool,
    /// [`PageHeader::credit_air`], so the lines land on the same centres.
    pub credit_air: f32,
    pub controls: HeaderControls,
}

/// The type line over a detail header's title, as far as the route knows it.
#[derive(Clone, Copy)]
pub(crate) enum HeaderKind {
    /// No type line (the liked-by-artist page).
    None,
    /// There is one and it isn't known yet: an album may turn out a single or an EP.
    Bar,
    /// Always this.
    Text(&'static str),
}

/// The controls after Play on a detail header's own row.
#[derive(Clone, Copy)]
pub(crate) enum HeaderControls {
    /// Play alone. Save and ••• come or go with the account and the page, so they arrive beside
    /// it rather than as grey shapes that may not turn into anything — an album or playlist.
    PlayOnly,
    /// Shuffle and sort, which the page always has, drawn for real but dimmed: the liked-by-artist
    /// page.
    ShuffleAndSort,
}

/// [`page_header`]'s placeholder: grey shapes on the same art, the same text centres (the block
/// bottom-aligned to the art) and the same control row under the art, then the same space
/// reserved, so the track rows below start where the real ones will.
pub(crate) fn skeleton_page_header(ui: &mut egui::Ui, palette: &theme::Palette, h: HeaderSkeleton) {
    let bar = |ui: &egui::Ui, rect: egui::Rect| {
        widgets::skeleton_fill(
            ui,
            rect,
            egui::CornerRadius::same(theme::radii().small),
            palette,
        );
    };
    let top = ui.cursor().min.y;
    let available = ui.available_width();
    let art_size = PAGE_ART_SIZE.min((available - 20.0 - 8.0 - 140.0).max(HEADER_ART_FLOOR_PT));
    let art_rect = egui::Rect::from_min_size(
        ui.cursor().min + egui::vec2(0.0, theme::HEADER_TOP_PAD),
        egui::vec2(art_size, art_size),
    );
    widgets::skeleton_fill(
        ui,
        art_rect,
        widgets::ArtShape::Square.radius(art_rect),
        palette,
    );

    let tx = art_rect.right() + 20.0;
    let text_w = (available - art_size - 20.0 - 8.0).max(0.0);
    // A known title is sized as the real header sizes it; an unknown one is a short name at the
    // largest size, as most are.
    let title_size = match h.title {
        Some(t) => widgets::fit_size(
            ui,
            t,
            text_w,
            DETAIL_TITLE_MAX,
            DETAIL_TITLE_MIN,
            theme::bold,
        ),
        None => DETAIL_TITLE_MAX,
    };
    let title_font = theme::bold(title_size);
    let title_ink = widgets::ink_drop(ui, h.title.unwrap_or("Title"), &title_font);
    let has_kind = !matches!(h.kind, HeaderKind::None);
    // Text is drawn only once the placeholders are, so a load that finishes quickly shows
    // nothing rather than a page of half its words.
    let visible = widgets::skeleton_visible(ui);
    let title_rel = if has_kind {
        widgets::title_centre_under_label(KIND_BASELINE, KIND_TO_TITLE_AIR, title_size)
    } else {
        title_size * 0.5
    };
    // The lines under the title, as centres relative to the block's top — the same walk
    // `page_header` takes.
    let mut lines = Vec::new();
    let mut y = title_rel;
    if h.description {
        let dy = y + title_ink + CREDIT_AIR + DESCRIPTION_AIR + CREDIT_CAP_HALF + h.credit_air;
        lines.push((dy, 0.5));
        y = dy + LINE_GAP;
    } else {
        y += title_ink + CREDIT_AIR + CREDIT_CAP_HALF + h.credit_air;
    }
    if h.credit_line {
        lines.push((y, 0.22));
        y += LINE_GAP;
    }
    lines.push((y, 0.36));
    let last = lines.last().map_or(title_rel, |l| l.0);
    let block_h = last + 7.0;
    let text_top = art_rect.top() + (art_size - block_h).max(0.0);

    match h.kind {
        HeaderKind::None => {}
        HeaderKind::Bar => bar(
            ui,
            egui::Rect::from_min_size(
                egui::pos2(tx, text_top + KIND_CENTRE - 4.5),
                egui::vec2(56.0_f32.min(text_w), 9.0),
            ),
        ),
        HeaderKind::Text(label) if visible => {
            ui.painter().text(
                egui::pos2(tx, text_top + KIND_CENTRE),
                egui::Align2::LEFT_CENTER,
                label,
                theme::semibold(12.0),
                palette.dim,
            );
        }
        HeaderKind::Text(_) => {}
    }
    match h.title {
        Some(title) => {
            if visible {
                crate::emoji::paint_line(
                    ui,
                    egui::pos2(tx, text_top + title_rel),
                    title,
                    title_font.clone(),
                    palette.text,
                );
            }
        }
        None => {
            // The title's capitals: from its baseline (the ink's end, for a name with no
            // descenders) up one cap height.
            let baseline = text_top + title_rel + title_ink;
            let cap = title_size * 0.727;
            bar(
                ui,
                egui::Rect::from_min_max(
                    egui::pos2(tx, baseline - cap),
                    egui::pos2(tx + (text_w * 0.3).min(320.0), baseline),
                ),
            );
        }
    }
    for (centre, w) in lines {
        bar(
            ui,
            egui::Rect::from_min_size(
                egui::pos2(tx, text_top + centre - 4.5),
                egui::vec2((text_w * w).clamp(60.0_f32.min(text_w), 380.0), 9.0),
            ),
        );
    }
    let text_bottom = text_top + last + 10.0;

    // The control row, where `page_header` puts it: its own row under the art.
    let control_h = DETAIL_PLAY_PT + 2.0;
    let row_top = art_rect.bottom().max(text_bottom) + 16.0;
    let mid = row_top + control_h / 2.0;
    let mut x = art_rect.left();
    widgets::skeleton_play_button(
        ui,
        palette,
        egui::Rect::from_center_size(
            egui::pos2(x + DETAIL_PLAY_PT / 2.0, mid),
            egui::vec2(DETAIL_PLAY_PT, DETAIL_PLAY_PT),
        ),
    );
    x += DETAIL_PLAY_PT + 12.0;
    match h.controls {
        HeaderControls::PlayOnly => {}
        HeaderControls::ShuffleAndSort => {
            // The page's two icons ride just past Play's row end, 34 pt boxes (20 pt glyphs),
            // as `artist_liked` places them.
            x -= 8.0;
            if visible {
                for (i, icon) in [crate::icons::Icon::Shuffle, crate::icons::Icon::ArrowUpDown]
                    .into_iter()
                    .enumerate()
                {
                    let centre = egui::pos2(x + 17.0 + i as f32 * (34.0 + 8.0), mid);
                    icon.image(palette.dim, 20.0).paint_at(
                        ui,
                        egui::Rect::from_center_size(centre, egui::vec2(20.0, 20.0)),
                    );
                }
            }
        }
    }
    let bottom = art_rect.bottom().max(row_top + control_h + 6.0);
    ui.add_space(bottom - top + 12.0);
}

/// The description behind a ••• menu's Info item. One copy: the album and playlist pages
/// showed the same window from the same state.
pub(crate) fn info_popup(app: &mut crate::app::App, ui: &mut egui::Ui) {
    let Some(text) = app.page_info.clone() else {
        return;
    };
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let palette = app.palette;

    // Size the card to the text, within bounds: a one-line bio gets a small card, a long one
    // scrolls instead of growing past the screen. Measured here, at the width the body will
    // actually wrap at, rather than guessed.
    let width = 480.0_f32.min(screen.width() - 40.0);
    let text_h = ui
        .fonts_mut(|f| {
            f.layout(
                text.clone(),
                theme::regular(13.0),
                palette.secondary,
                width - 32.0,
            )
        })
        .size()
        .y;
    let height = (text_h + 96.0).clamp(150.0, (screen.height() - 80.0).min(460.0));

    let mut dismissed = false;
    let mut close = false;
    egui::Area::new(egui::Id::new("ytm-info-modal"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            let (full, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(full, 0.0, egui::Color32::from_black_alpha(150));
            if response.clicked() {
                dismissed = true;
            }

            // Centered, a hair above true centre so it clears the player bar — the same modal
            // shape the new-playlist card uses, so the two cannot drift apart.
            let card = egui::Rect::from_center_size(
                full.center() - egui::vec2(0.0, 12.0),
                egui::vec2(width, height),
            );
            let mut card_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card)
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            {
                let painter = card_ui.painter();
                // The theme's soft shadow, stepped: three expanding translucent rects read as
                // one blur at these alphas.
                for (grow, alpha) in [(14.0, 24), (7.0, 40), (2.0, 60)] {
                    painter.rect_filled(
                        card.expand(grow),
                        egui::CornerRadius::same(theme::radii().popup + grow as u8),
                        egui::Color32::from_black_alpha(alpha),
                    );
                }
                painter.rect_filled(
                    card,
                    egui::CornerRadius::same(theme::radii().popup),
                    palette.overlay,
                );

                let mut body = card_ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(card.shrink(16.0))
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                body.label(
                    egui::RichText::new("Info")
                        .font(theme::semibold(15.0))
                        .color(palette.text),
                );
                body.add_space(6.0);
                let scroll_h = (card.height() - 90.0).max(60.0);
                egui::ScrollArea::vertical()
                    .max_height(scroll_h)
                    .show(&mut body, |ui| {
                        ui.label(
                            egui::RichText::new(text)
                                .font(theme::regular(13.0))
                                .color(palette.secondary),
                        );
                    });
                body.allocate_ui(egui::vec2(body.available_width(), 30.0), |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::chip(ui, &palette, "Close", true).clicked() {
                            close = true;
                        }
                    });
                });
            }
        });
    if dismissed || close || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        app.page_info = None;
    }
}

/// Every playable entry in `items`, in order — mirrors [`crate::model::SearchResults::playable`]
/// for a plain `Vec<Entry>` with no such method of its own.
pub(crate) fn playable_queue(items: &[Entry]) -> Vec<Track> {
    items.iter().filter_map(|e| e.track().cloned()).collect()
}

/// Where `items[index]` sits among [`playable_queue`] — mirrors
/// [`crate::model::SearchResults::playable_index`] for a plain `Vec<Entry>`.
pub(crate) fn playable_index(items: &[Entry], index: usize) -> Option<usize> {
    items.get(index)?.track()?;
    Some(
        items[..index]
            .iter()
            .filter(|e| e.track().is_some())
            .count(),
    )
}

/// A non-song row's subtitle, led by what it is, the way YouTube Music writes it: "Artist • 200M
/// monthly audience", "Album • Radiohead". The type sits here rather than in the duration column,
/// which then holds only durations. An empty detail leaves just the type.
fn typed_subtitle<'a>(kind: &'a str, detail: &'a str) -> widgets::Subtitle<'a> {
    let mut out = widgets::no_subtitle();
    out[0] = Some(widgets::Part::plain(kind));
    if !detail.trim().is_empty() {
        out[1] = Some(widgets::Part::plain(detail));
    }
    out
}

/// One entry's row, whatever kind it is — every field a song/video needs, plus the plainer
/// shape every other kind falls back to. Shared by Search's per-category tier and History's
/// dated shelves, so a mixed Song/Album/Playlist/Artist list reads and behaves identically
/// wherever it appears, rather than drifting between two copies of the same match.
pub(crate) fn entry_row_spec<'a>(
    item: &'a Entry,
    now: &crate::player::NowPlaying,
    library: &'a crate::model::Library,
    signed_in: bool,
) -> RowSpec<'a> {
    let (subtitle, duration, plays, explicit, active) = match item {
        // A song row's artist and album are links, which is what makes a result a way in to
        // the rest of the catalogue rather than a dead end.
        Entry::Song(t) | Entry::Video(t) => (
            track_subtitle(t),
            t.duration,
            t.plays.as_deref(),
            t.explicit,
            is_active(now, &t.video_id),
        ),
        Entry::Artist(a) => (
            typed_subtitle(item.kind(), a.subscribers.as_deref().unwrap_or("")),
            None,
            None,
            false,
            false,
        ),
        Entry::Album(a) => (
            typed_subtitle(item.kind(), &a.artist),
            None,
            a.year.as_deref(),
            false,
            false,
        ),
        Entry::Playlist(p) => (
            typed_subtitle(item.kind(), &p.author),
            None,
            None,
            false,
            false,
        ),
        // Neither Search nor History ever returns mood chips; this arm keeps the match total.
        Entry::Mood(_) => (widgets::plain_subtitle(""), None, None, false, false),
    };
    RowSpec {
        number: None,
        rank: match item {
            Entry::Song(t) | Entry::Video(t) => t.rank.as_deref(),
            _ => None,
        },
        trend: match item {
            Entry::Song(t) | Entry::Video(t) => t.trend,
            _ => crate::model::ChartTrend::Unknown,
        },
        title: item.title(),
        album_id: match item {
            Entry::Song(t) | Entry::Video(t) => t.album_id.as_deref(),
            _ => None,
        },
        subtitle,
        duration,
        plays,
        explicit,
        active,
        playing: active && !now.paused,
        // The type leads the subtitle instead (`typed_subtitle`): the right column is durations.
        badge: None,
        // Artists are round so a mixed list is scannable at a glance; a video still is 16:9
        // and gets the wide slot.
        shape: match item {
            Entry::Video(t) if t.wide => widgets::ArtShape::Wide,
            _ if item.is_artist() => widgets::ArtShape::Circle,
            _ => widgets::ArtShape::Square,
        },
        hide_art: false,
        thumbnail: item.thumbnail(),
        menu: match item {
            Entry::Song(t) | Entry::Video(t) => Some(widgets::RowMenu {
                track: t,
                playlist_id: None,
                playlists: &library.playlists.items,
                signed_in,
                queue_index: None,
            }),
            _ => None,
        },
        like: match item {
            Entry::Song(t) | Entry::Video(t) => t.like,
            _ => crate::model::LikeStatus::Indifferent,
        },
        show_rating: true,
        quiet_like: false,
        add_to: None,
    }
}

/// Every entry kind rendered as one plain full-width row list, never cards — History's dated
/// shelves want this even for a shelf that is entirely songs, unlike Home/Explore's shelves,
/// which render every kind as cards.
pub(crate) fn entry_rows(app: &mut crate::app::App, ui: &mut egui::Ui, items: &[Entry]) {
    let signed_in = app.signed_in();
    let clicked = {
        let crate::app::App {
            palette,
            images,
            backend,
            stats,
            liked,
            now,
            library,
            ..
        } = app;
        spec_rows(
            ui,
            palette,
            images,
            backend,
            stats,
            liked,
            items.len(),
            |i| entry_row_spec(&items[i], now, library, signed_in),
        )
    };
    if let Some(action) = clicked.action {
        app.row_action(action);
    }
    match clicked.click {
        Some(ListClick::Row(i)) => match &items[i] {
            Entry::Song(_) | Entry::Video(_) => {
                if let Some(index) = playable_index(items, i) {
                    app.play_row(playable_queue(items), index);
                }
            }
            Entry::Artist(a) => app.navigate(Route::Artist(a.id.clone())),
            Entry::Album(a) => app.navigate(Route::Album(a.id.clone())),
            Entry::Playlist(p) => app.navigate(Route::Playlist(p.id.clone())),
            Entry::Mood(m) => app.navigate(Route::Mood(m.params.clone())),
        },
        Some(ListClick::Link(route)) => app.navigate(route),
        None => {}
    }
}

/// What a card grid — wrapped or carousel — does with a click or a menu choice.
pub(crate) fn finish_grid(app: &mut crate::app::App, shelf: &Shelf, grid: GridOutcome) {
    if let Some(action) = grid.action {
        app.row_action(action);
    }
    // A song card's title link wins over everything else: the listener asked for the album.
    if let Some(route) = grid.link {
        app.navigate(route);
        return;
    }
    // The corner play button specifically: an album/playlist plays directly instead of opening
    // its page, matching what the button visually promises. A song/video card has no page to
    // open, so it queues the shelf at its own index — the same thing its body click does below,
    // which is why the button must be handled here rather than falling through: `played` wins
    // over `clicked` in `entry_grid`/`carousel`, so for a song there is no clicked click left
    // to fall back on. Any other kind (a mood chip has no play button to press) falls through.
    if let Some(i) = grid.played
        && let Some(item) = shelf.items.get(i)
    {
        match item {
            Entry::Album(a) => {
                app.play_album(a.id.clone());
                return;
            }
            Entry::Playlist(p) => {
                app.play_playlist(p.id.clone());
                return;
            }
            Entry::Song(_) | Entry::Video(_) => {
                let tracks = shelf.tracks();
                if let Some(index) = shelf.track_index(i) {
                    app.play_row(tracks, index);
                }
                return;
            }
            _ => {}
        }
    }
    if let Some(i) = grid.clicked
        && let Some(item) = shelf.items.get(i)
    {
        match entry_route(item) {
            Some(route) => app.navigate(route),
            // A video card in a card shelf still plays; it starts the song and its radio.
            None => {
                let tracks = shelf.tracks();
                if let Some(index) = shelf.track_index(i) {
                    app.play_row(tracks, index);
                }
            }
        }
    }
}

/// The scroll area a page's content lives in.
///
/// Two things it exists to get right:
///
/// * **Its own scroll position.** Every page used to build a bare `ScrollArea::vertical()`, which
///   takes its id from the `Ui` path — the same `CentralPanel` for every route — so all pages
///   shared one offset. Opening Home after scrolling the library dropped you into the middle of it.
///   An `id_salt` per route gives each page its own, so Back also returns you where you were.
/// * **Starting at the top of a page you just opened**, which is what [`crate::app::App`] sets
///   `scroll_to_top` for. Back and Forward deliberately do not set it. The flag is sticky
///   until a scroll area consumes it: a fresh fetch shows a loader first, and clearing the
///   flag on a loader frame would reopen the feed at the old offset once it arrives.
///
/// It also fades the page at an end with more of it past that end ([`PageScroll::show`]).
pub(crate) fn page_scroll(app: &mut crate::app::App, salt: &str) -> PageScroll {
    let mut area = egui::ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false]);
    if app.scroll_to_top {
        area = area.vertical_scroll_offset(0.0);
        app.scroll_to_top = false;
    }
    if let Some(offset) = app.autoscroll {
        area = area.vertical_scroll_offset(offset);
    }
    PageScroll(area, widgets::scroll_fade(&app.settings))
}

/// A page's scroll area, from [`page_scroll`], and how far it fades at its ends.
pub(crate) struct PageScroll(egui::ScrollArea, f32);

impl PageScroll {
    /// Show the page in it, and fade it at an end with more page past that end: nothing at the
    /// top of a page scrolled all the way up.
    pub(crate) fn show<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::scroll_area::ScrollAreaOutput<R> {
        let from = widgets::painted_so_far(ui);
        let output = self.0.show(ui, add_contents);
        widgets::fade_scroll_edges(ui, from, &output, self.1);
        output
    }
}

/// A page's own title at the top of the content card, e.g. "Settings" or "Your library".
///
/// Its *ink* sits [`theme::PAGE_INSET_X`] below the card's top edge — the same distance the
/// text has from the card's side — rather than its line box: a label's box carries leading
/// above the capitals, and padding the box by the side inset left the word visibly lower than
/// it was far from the edge.
pub(crate) fn page_title(ui: &mut egui::Ui, palette: &theme::Palette, text: &str) {
    ui.add_space((theme::PAGE_INSET_X - PAGE_TITLE_LEAD).max(0.0));
    ui.label(
        egui::RichText::new(text)
            .font(theme::bold(theme::PAGE_TITLE_PT))
            .color(palette.text),
    );
    ui.add_space(10.0);
}

/// The room a [`page_title`] line leaves above its capitals: the line box's top to the cap line.
/// Measured off a screenshot — ~4.6pt above the capitals of a 26pt Inter title, 0.18 em — and
/// kept as that fraction so a new title size stays optically in place.
const PAGE_TITLE_LEAD: f32 = theme::PAGE_TITLE_PT * 0.18;

/// A section heading, e.g. "Top songs" between blocks of a page.
/// How a carousel offers the rest of its shelf.
#[derive(Clone, Copy)]
pub(crate) enum MoreButton {
    /// A pill with this label — Explore's "More", a way on to a different page.
    Pill(&'static str),
    /// The quiet "Show all" link the artist page's shelves use: the whole of this shelf.
    ShowAll,
}

/// The small line over a shelf's title — "Similar to" over an artist's name.
fn shelf_strapline(ui: &mut egui::Ui, palette: &theme::Palette, line: &str) {
    ui.label(
        egui::RichText::new(line)
            .font(theme::medium(11.0))
            .color(palette.dim),
    );
    ui.add_space(-4.0);
}

/// A shelf of songs — Quick picks and the other song lists on Home — as pages of
/// [`HOME_SONG_ROWS`] rows in up to three columns, paged sideways with the heading's arrows, the
/// way YouTube Music's own home lays them out. The same rows as the artist page's Top songs.
pub(crate) fn song_shelf(
    app: &mut crate::app::App,
    ui: &mut egui::Ui,
    page_salt: &str,
    shelf: &Shelf,
) {
    let signed_in = app.signed_in();
    let out = {
        let crate::app::App {
            palette,
            images,
            backend,
            stats,
            liked,
            now,
            library,
            ..
        } = app;
        let cols = song_shelf_columns(ui.available_width());
        // Every item is a track here (the caller checks); a non-track, which cannot happen, is
        // skipped by drawing nothing rather than panicking mid-frame.
        let tracks: Vec<&crate::model::Track> =
            shelf.items.iter().filter_map(Entry::track).collect();
        spec_pages(
            ui,
            palette,
            images,
            backend,
            stats,
            liked,
            &shelf.title,
            shelf.strapline.as_deref(),
            tracks.len(),
            cols,
            HOME_SONG_ROWS,
            (page_salt, shelf.title.as_str()),
            |i| shelf_track_spec(tracks[i], None, now, &library.playlists.items, signed_in),
        )
    };
    match out.click {
        Some(ListClick::Row(i)) => app.play_row(shelf.tracks(), i),
        Some(ListClick::Link(route)) => app.navigate(route),
        None => {}
    }
    if let Some(action) = out.action {
        app.row_action(action);
    }
}

/// How many columns of songs a Home song shelf lays out at `width`: up to three, as YouTube Music
/// fits across a wide window, but never under [`SONG_COLUMN_MIN`] each.
pub(crate) fn song_shelf_columns(width: f32) -> usize {
    ((width / SONG_COLUMN_MIN).floor() as usize).clamp(1, 3)
}

/// The narrowest a Home song column may get. At 250 a portrait screen's ~770 pt card took three
/// columns, and after the cover, the gaps and the thumbs each title had about 150 pt — a few
/// words and an ellipsis. At 340 that card takes two, with room to read the name.
const SONG_COLUMN_MIN: f32 = 340.0;

pub(crate) fn section(ui: &mut egui::Ui, palette: &theme::Palette, name: &str) {
    ui.add_space(20.0);
    ui.label(
        egui::RichText::new(name)
            .font(theme::bold(theme::SECTION_PT))
            .color(palette.text),
    );
    ui.add_space(8.0);
}

/// A grid of cards laid out with `horizontal_wrapped`, skipping the art fetch for cards that are
/// scrolled out of view. Grids hold far fewer items than track lists, so they are not virtualised
/// — but an off-screen card must still not cost a network request.
pub(crate) fn card_is_visible(ui: &egui::Ui, width: f32, shape: widgets::ArtShape) -> bool {
    let cursor = ui.cursor();
    let card = egui::Rect::from_min_size(cursor.min, widgets::card_size(width, shape));
    ui.clip_rect().intersects(card)
}

/// Whether this track is the one currently loaded in the player.
pub(crate) fn is_active(now: &crate::player::NowPlaying, video_id: &str) -> bool {
    now.track.as_ref().is_some_and(|t| t.video_id == video_id)
}

/// A dim message centred in the remaining space, for loading / empty / error pages.
pub(crate) fn centered(app: &crate::app::App, ui: &mut egui::Ui, message: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(
            egui::RichText::new(message)
                .font(theme::regular(13.0))
                .color(app.palette.dim),
        );
    });
}

/// An error message with a "Try again" button. Takes the message by value so callers can pass
/// a clone and keep no borrow on `App` while the retry closure runs.
pub(crate) fn failed(
    app: &mut crate::app::App,
    ui: &mut egui::Ui,
    message: String,
    retry: impl FnOnce(&mut crate::app::App),
) {
    if crate::api::looks_offline(&message) {
        offline_page(app, ui, retry);
        return;
    }
    // A third of the way down rather than dead centre: centring the message in the whole page
    // with `centered_and_justified` took every point of height, and the Try again button under
    // it was pushed off the bottom of the card.
    ui.add_space((ui.available_height() * 0.3).max(24.0));
    let mut again = false;
    ui.vertical_centered(|ui| {
        ui.set_max_width(ui.available_width().min(460.0));
        ui.label(
            egui::RichText::new(message)
                .font(theme::regular(13.0))
                .color(app.palette.danger),
        );
        ui.add_space(14.0);
        again = crate::widgets::pill_button(ui, &app.palette, "Try again").clicked();
    });
    if again {
        retry(app);
    }
}

/// A page that could not load for want of a connection: an empty state rather than an error. The
/// raw chain ("error sending request for url …") says nothing the reader can act on, so this says
/// what is going on, that it will sort itself out, and where music still is — the downloads.
/// Draws in place of the top banner too (see [`offline_banner`]), so it is said once.
fn offline_page(
    app: &mut crate::app::App,
    ui: &mut egui::Ui,
    retry: impl FnOnce(&mut crate::app::App),
) {
    app.offline_page_shown = true;
    let p = app.palette;
    const ICON: f32 = 40.0;
    // Optically centred: a touch above the middle, where a lone block of content sits right.
    let block_h = ICON + 18.0 + 26.0 + 8.0 + 20.0 + 24.0 + 36.0;
    ui.add_space(((ui.available_height() - block_h) * 0.42).max(24.0));
    let (mut again, mut downloads) = (false, false);
    ui.vertical_centered(|ui| {
        ui.set_max_width(ui.available_width().min(420.0));
        // On the page's own background: just the glyph, nothing behind it.
        let (slot, _) = ui.allocate_exact_size(egui::vec2(ICON, ICON), egui::Sense::hover());
        crate::icons::Icon::WifiOff
            .image(p.dim, ICON)
            .paint_at(ui, slot);
        ui.add_space(18.0);
        ui.label(
            egui::RichText::new("You're offline")
                .font(theme::bold(22.0))
                .color(p.text),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "This page loads by itself once you're back online. Your downloads play without one.",
            )
            .font(theme::regular(13.0))
            .color(p.secondary),
        );
        ui.add_space(24.0);
        // The two buttons side by side, centred: a horizontal row does not centre itself, so it
        // is placed by the width it took last frame.
        let id = ui.id().with("offline-actions");
        let width: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            let start = ui.cursor().min.x;
            again = crate::widgets::pill_button(ui, &p, "Try again").clicked();
            ui.add_space(4.0);
            let last = crate::widgets::outline_button(ui, &p, "Downloads");
            downloads = last.clicked();
            let used = last.rect.right() - start;
            ui.ctx().data_mut(|d| d.insert_temp(id, used));
        });
    });
    if again {
        retry(app);
    } else if downloads {
        app.rail_filter = crate::app::RailFilter::Downloads;
        app.navigate(Route::Library);
    }
}

/// The strip across the top of the page while the app has no connection, on pages that did load
/// (or loaded before it went): what is going on, and the one place that still works. A page that
/// could not load at all says it in full instead, so the strip steps aside there.
pub(crate) fn offline_banner(app: &mut crate::app::App, ui: &mut egui::Ui) {
    let p = app.palette;
    let mut open = false;
    ui.add_space(12.0);
    // A fixed-height strip, painted by hand: a `Frame` takes the height its parent offers, and on
    // a page laid out to fill (Settings) it grew into a pale slab across the whole card.
    let (strip, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(strip, theme::pill(strip), p.glass(p.surface));
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(strip.shrink2(egui::vec2(14.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let (slot, _) = row.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    crate::icons::Icon::WifiOff
        .image(p.secondary, 16.0)
        .paint_at(&row, slot);
    row.add_space(4.0);
    row.label(
        egui::RichText::new("You're offline")
            .font(theme::semibold(13.0))
            .color(p.text),
    );
    row.label(
        egui::RichText::new("Downloaded songs still play.")
            .font(theme::regular(13.0))
            .color(p.secondary),
    );
    row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(-8.0);
        open = crate::widgets::outline_button(ui, &p, "Downloads").clicked();
    });
    ui.add_space(10.0);
    if open {
        app.rail_filter = crate::app::RailFilter::Downloads;
        app.navigate(Route::Library);
    }
}

/// What the transport's and panels' icon buttons say on hover: shared by the player bar and the
/// fullscreen player so both read the same, with the shortcut where there is one (as `help` lists
/// them).
pub(crate) mod tips {
    use crate::app::RepeatMode;

    /// The modifier the Ctrl shortcuts use: Cmd on macOS.
    fn ctrl() -> &'static str {
        if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        }
    }

    pub(crate) fn shuffle(on: bool) -> String {
        format!("Shuffle: {} ({}+S)", if on { "on" } else { "off" }, ctrl())
    }

    pub(crate) fn repeat(mode: RepeatMode) -> String {
        let mode = match mode {
            RepeatMode::Off => "off",
            RepeatMode::All => "all",
            RepeatMode::One => "one",
        };
        format!("Repeat: {mode} ({}+R)", ctrl())
    }

    pub(crate) fn play(playing: bool) -> &'static str {
        if playing {
            "Pause (Space)"
        } else {
            "Play (Space)"
        }
    }

    /// A rating thumb: what pressing it does, given whether that rating is set.
    pub(crate) fn rate(up: bool, set: bool) -> &'static str {
        match (up, set) {
            (true, false) => "Like",
            (true, true) => "Remove like",
            (false, false) => "Dislike",
            (false, true) => "Remove dislike",
        }
    }

    pub(crate) fn video(open: bool) -> &'static str {
        if open {
            "Show the song"
        } else {
            "Show the video"
        }
    }

    pub(crate) fn mute(muted: bool) -> &'static str {
        if muted { "Unmute (M)" } else { "Mute (M)" }
    }

    pub(crate) const PREVIOUS: &str = "Previous (Up)";
    pub(crate) const NEXT: &str = "Next (Down)";
    pub(crate) const QUEUE: &str = "Queue";
    pub(crate) const LYRICS: &str = "Lyrics";
    pub(crate) const HISTORY: &str = "History";
    pub(crate) const EXIT_FULLSCREEN: &str = "Exit fullscreen (Esc)";
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: f32 = 56.0;

    /// The wash's ramp: full strength where the header starts, gone where it ends, and never
    /// brightening on the way down. The top stop is what fills the card's top margin, so a
    /// value there that was already fading would put the seam back.
    #[test]
    fn the_wash_starts_at_full_strength_and_ends_at_nothing() {
        let panel = egui::Color32::from_rgb(20, 20, 24);
        let wash = egui::Color32::from_rgb(200, 40, 90);
        assert_eq!(
            wash_stop(panel, wash, 0.0, 1.0),
            panel.lerp_to_gamma(wash, 0.38)
        );
        assert_eq!(
            wash_stop(panel, wash, 1.0, 1.0).a(),
            0,
            "the foot must be gone"
        );
        // Clamped: above the header the colour holds rather than inverting.
        assert_eq!(
            wash_stop(panel, wash, -0.5, 1.0),
            wash_stop(panel, wash, 0.0, 1.0)
        );
        let mut last = 255;
        for i in 0..=10 {
            let a = wash_stop(panel, wash, i as f32 / 10.0, 1.0).a();
            assert!(a <= last, "alpha rose at {i}: {a} after {last}");
            last = a;
        }
    }

    /// A viewport that fits the whole list must draw the whole list — the case that made the
    /// first live measurement look like a bug when it was not.
    #[test]
    fn a_short_list_in_a_tall_window_draws_everything() {
        assert_eq!(visible_range(235.0, 106.0, 848.0, ROW, 20), 0..20);
    }

    /// Paging is driven off the drawn range: reaching the end of what is loaded is what asks for
    /// more, and the top of a long list must not.
    #[test]
    fn only_the_end_of_a_list_asks_for_another_page() {
        let at_end = |r: std::ops::Range<usize>, count: usize| r.end + PREFETCH_ROWS >= count;

        // A viewport at the top of 689 rows: nothing to fetch yet.
        assert!(!at_end(visible_range(0.0, 0.0, 560.0, ROW, 689), 689));
        // Scrolled to the last screen of them: fetch.
        assert!(at_end(
            visible_range(-680.0 * ROW, 0.0, 560.0, ROW, 689),
            689
        ));
        // A list that fits entirely on screen is already at its end.
        assert!(at_end(visible_range(0.0, 0.0, 848.0, ROW, 12), 12));
    }

    /// The case that matters: a long list costs the viewport, not the list.
    #[test]
    fn a_long_list_draws_only_the_viewport() {
        let r = visible_range(0.0, 0.0, 560.0, ROW, 5000);
        assert_eq!(r.start, 0);
        assert!(r.len() <= 16, "drew {} rows for a 560px viewport", r.len());
    }

    #[test]
    fn scrolling_moves_the_window_and_keeps_it_the_same_size() {
        let unscrolled = visible_range(0.0, 0.0, 560.0, ROW, 5000);
        // Scrolled down 100 rows: the list start has moved above the viewport.
        let scrolled = visible_range(-100.0 * ROW, 0.0, 560.0, ROW, 5000);
        assert_eq!(scrolled.start, 98, "100 rows up, minus overscan");
        assert_eq!(scrolled.len(), unscrolled.len());
    }

    #[test]
    fn the_end_of_the_list_is_not_overrun() {
        let r = visible_range(-4990.0 * ROW, 0.0, 560.0, ROW, 5000);
        assert_eq!(r.end, 5000);
        assert!(r.start < r.end);
    }

    /// A list scrolled entirely past the bottom of the viewport must not panic or invert.
    #[test]
    fn scrolling_far_past_the_end_yields_an_empty_range() {
        let r = visible_range(-9_000.0 * ROW, 0.0, 560.0, ROW, 100);
        assert!(r.start <= r.end, "{r:?}");
        assert_eq!(r.end, 100);
    }

    #[test]
    fn an_empty_list_has_an_empty_range() {
        assert!(visible_range(0.0, 0.0, 560.0, ROW, 0).is_empty());
    }

    /// The scroll extent must be the whole list no matter which slice is drawn — the mismatch
    /// between the spacers (`row_h` per skipped row) and the drawn rows (each paying an extra
    /// `item_spacing`) is what made the sidebar and queue jitter and overshoot at the bottom.
    #[test]
    fn the_scroll_extent_does_not_depend_on_the_drawn_slice() {
        let pitch = row_pitch(ROW, 6.0);
        let count = 5000;
        let whole = count as f32 * pitch;
        for (first, last) in [(0, 12), (98, 112), (100, 116), (4988, 5000), (0, 5000)] {
            let (top, bottom) = virtual_spacers(first, last, count, pitch);
            let drawn = (last - first) as f32 * pitch;
            let total = top + drawn + bottom;
            assert!(
                (total - whole).abs() < 0.01,
                "{first}..{last}: extent {total} != {whole}"
            );
        }
    }

    /// A full row of mood chips fits its row: at the exact share, rounding wrapped the last chip
    /// onto a line of its own, and a wide window showed five across instead of six.
    #[test]
    fn mood_chip_rows_hold_their_columns() {
        for width in [400.0, 777.0, 1080.0, 1333.3, 1600.0, 2400.0] {
            let (cols, chip_w, _, gap) = mood_chip_grid(width);
            assert!(
                chip_w * cols as f32 + gap * (cols - 1) as f32 <= width,
                "{width}"
            );
        }
        assert_eq!(mood_chip_grid(1600.0).0, 6);
    }

    /// A portrait screen's card gets two readable columns, not three cramped ones; a wide window
    /// still gets YouTube Music's three.
    #[test]
    fn song_shelves_only_split_where_titles_still_fit() {
        assert_eq!(song_shelf_columns(560.0), 1);
        assert_eq!(song_shelf_columns(770.0), 2);
        assert_eq!(song_shelf_columns(1000.0), 2);
        assert_eq!(song_shelf_columns(1100.0), 3);
        assert_eq!(song_shelf_columns(2400.0), 3);
    }

    /// Carousel paging never leaves its rails: a step past either end clamps, and a view
    /// wider than the row never moves at all.
    #[test]
    fn carousel_paging_stays_on_its_rails() {
        // A 500-wide page (a whole number of cards plus their gaps) in a 492-wide view.
        assert_eq!(carousel_step(0.0, 1.0, 500.0, 492.0, 1200.0), 500.0);
        // Pulled back at the end: the last page ends on the last card.
        assert_eq!(carousel_step(500.0, 1.0, 500.0, 492.0, 1200.0), 708.0);
        assert_eq!(carousel_step(0.0, -1.0, 500.0, 492.0, 1200.0), 0.0);
        // A drag left between pages: the arrow snaps to the next page's edge.
        assert_eq!(carousel_step(260.0, 1.0, 500.0, 492.0, 1200.0), 708.0);
        assert_eq!(carousel_step(0.0, 1.0, 1500.0, 1492.0, 1200.0), 0.0);
    }

    /// A resize keeps the first card in view at the left edge, on a card boundary.
    #[test]
    fn a_resized_carousel_keeps_its_first_card() {
        // Four cards in (pitch 140), the cards grow to a 150 pitch: still the fifth card first.
        assert_eq!(carousel_refit(560.0, 140.0, 150.0, 2000.0), 600.0);
        // Part-way into a card: the nearest one leads.
        assert_eq!(carousel_refit(600.0, 140.0, 150.0, 2000.0), 600.0);
        // Never past the end.
        assert_eq!(carousel_refit(1400.0, 140.0, 150.0, 900.0), 900.0);
        assert_eq!(carousel_refit(0.0, 140.0, 150.0, 900.0), 0.0);
    }

    /// A whole number of cards fills the row exactly, each close to its usual width.
    #[test]
    fn carousel_cards_fill_the_row_exactly() {
        for view in [500.0_f32, 764.0, 1000.0, 1333.0, 1900.0] {
            let (n, w) = carousel_fit(view, 8.0, CARD_W);
            let used = n as f32 * w + (n - 1) as f32 * 8.0;
            assert!((used - view).abs() < 0.01, "{view}: {n} × {w} uses {used}");
            assert!(
                (w / CARD_W - 1.0).abs() < 0.2,
                "{view}: card {w} drifted from {CARD_W}"
            );
        }
        assert_eq!(
            carousel_fit(50.0, 8.0, CARD_W).0,
            1,
            "never fewer than one card"
        );
    }

    fn subtitle_track() -> crate::model::Track {
        crate::model::Track {
            video_id: "vid".to_owned(),
            title: "Song".to_owned(),
            artists: vec![
                crate::model::ArtistLink {
                    name: "Artist".to_owned(),
                    id: Some("UCartist".to_owned()),
                },
                crate::model::ArtistLink {
                    name: "Guest".to_owned(),
                    id: None,
                },
            ],
            album: Some("Album".to_owned()),
            album_id: Some("MPREalbum".to_owned()),
            duration: Some(200),
            plays: Some("1M plays".to_owned()),
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

    fn part_texts<'a>(sub: &'a widgets::Subtitle<'a>) -> Vec<(&'a str, bool)> {
        sub.iter()
            .flatten()
            .map(|p| (p.text, p.target.is_some()))
            .collect()
    }

    /// The full subtitle names the album with its link, after the artists.
    #[test]
    fn full_subtitle_keeps_artists_and_linked_album() {
        let t = subtitle_track();
        assert_eq!(
            part_texts(&track_subtitle(&t)),
            [("Artist", true), ("Guest", false), ("Album", true)]
        );
    }

    /// The album page and the queue drop the album slot but keep everything else: linked
    /// artists stay linked, and the plays/Radio tail is unaffected (it rides `tail`, not
    /// the subtitle slots — asserted by its absence here, not its presence).
    #[test]
    fn no_album_subtitle_drops_only_the_album() {
        let t = subtitle_track();
        let sub = track_subtitle_no_album(&t);
        assert_eq!(part_texts(&sub), [("Artist", true), ("Guest", false)]);
        assert!(
            sub.iter().flatten().all(|p| p.text != "Album"),
            "the album must not appear in any slot"
        );
    }

    /// No album on the track, no empty slot left behind.
    #[test]
    fn no_album_subtitle_without_an_album_is_just_artists() {
        let mut t = subtitle_track();
        t.album = None;
        t.album_id = None;
        assert_eq!(
            part_texts(&track_subtitle(&t)),
            part_texts(&track_subtitle_no_album(&t))
        );
    }

    /// The paged grid numbers down its columns: 1-5 in the first, 6-10 in the second.
    #[test]
    fn the_paged_grid_numbers_down_its_columns() {
        assert_eq!(grid_index(0, 0, 0, ROWS_PER_PAGE), 0);
        assert_eq!(grid_index(0, 0, 4, ROWS_PER_PAGE), 4);
        assert_eq!(grid_index(0, 1, 0, ROWS_PER_PAGE), 5);
        assert_eq!(grid_index(0, 2, 4, ROWS_PER_PAGE), 14);
        // The next page continues where the last column ended.
        assert_eq!(grid_index(15, 0, 0, ROWS_PER_PAGE), 15);
    }

    /// A non-song row leads its subtitle with its type, and keeps the detail after it.
    #[test]
    fn a_mixed_row_names_its_type_in_the_subtitle() {
        let texts = |sub: &widgets::Subtitle| -> Vec<String> {
            sub.iter().flatten().map(|p| p.text.to_owned()).collect()
        };
        assert_eq!(
            texts(&typed_subtitle("Artist", "200M monthly audience")),
            ["Artist", "200M monthly audience"]
        );
        assert_eq!(
            texts(&typed_subtitle("Album", "Radiohead")),
            ["Album", "Radiohead"]
        );
        assert_eq!(
            texts(&typed_subtitle("Artist", "  ")),
            ["Artist"],
            "no detail"
        );
        let sub = typed_subtitle("Playlist", "Arafinwe");
        assert_eq!(sub[1].map(|p| p.sep), Some(widgets::FIELD_SEP));
    }
}
