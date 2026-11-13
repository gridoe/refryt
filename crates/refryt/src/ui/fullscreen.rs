//! The fullscreen now-playing view: the cover and the song's name, big lyrics beside (or, on a
//! vertical screen, under) them, and a quiet set of controls that fade after a few seconds
//! without the mouse moving.
//!
//! It replaces the whole shell while it is open — no sidebar, no bars — but playback, the queue
//! and the lyrics fetch keep running exactly as they do behind the normal window. Only the
//! cover and the song's name always show; Lyrics and Queue are toggles.
//!
//! Everything that has a size scales with the window ([`Metrics`]), and a vertical window gets a
//! stacked arrangement instead of a side-by-side one.

use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, Id, Rect, Sense, UiBuilder, pos2, vec2};

use crate::app::{App, RepeatMode, Route};
use crate::icons::Icon;
use crate::model::LikeStatus;
use crate::theme;
use crate::ui::RowAction;
use crate::widgets::Hint;
use crate::widgets::{self, EdgeFade, edge_fades, fade_painted, painted_so_far};

/// How far the lyrics fade out at the top and bottom of their column, at scale 1. Long and eased,
/// so the last line still showing has nearly gone by the column's edge: a short straight ramp left
/// it at half strength there, which read as a cut rather than a fade.
const LYRICS_FADE: f32 = 220.0;

/// How long the controls stay up after the last sign of life from the mouse or keyboard.
const HIDE_AFTER: Duration = Duration::from_secs(3);
/// Where the active line rests, as a fraction of the lyrics column's height from its top.
const ANCHOR: f32 = 0.40;
/// How fast the lyrics chase the active line: the offset closes `1 − e^−rate·dt` of the gap
/// each frame, which is a smooth glide of about a sixth of a second to settle.
const EASE_RATE: f32 = 6.0;
/// How long a wheel-scrolled view is left alone before the lyrics glide back to the song.
const BROWSE_FOR: Duration = Duration::from_secs(3);
/// The window size the fixed numbers below were drawn for, by its shorter side.
const REFERENCE: f32 = 1080.0;
/// The shorter side of the card that still holds the view at full size when it is kept in the
/// window; a smaller one scales it down.
const WINDOWED_FIT: f32 = 640.0;
/// The toggles' icon size in the title strip of a view kept in the window: a touch over the
/// window buttons' own, as they are the view's controls and the strip's two groups still read
/// as one row.
const STRIP_ICON: f32 = 16.0;

/// Every size in the view, already multiplied by how big the window is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    pub scale: f32,
    /// Taller than wide: the cover stacks above the lyrics and the queue becomes a sheet.
    pub portrait: bool,
    pub margin: f32,
    /// The bands the controls own whether shown or not, so nothing moves when they fade.
    pub top_band: f32,
    pub bottom_band: f32,
    /// Under the cover: the title and the credits.
    pub info_h: f32,
    pub title: f32,
    pub credits: f32,
    pub lyrics: f32,
    pub line_gap: f32,
    pub lyrics_max_w: f32,
    pub column_gap: f32,
    pub icon: f32,
    pub play: f32,
    pub controls_w: f32,
    /// Under this width the lyrics take the whole screen and the cover steps aside.
    pub narrow: f32,
    /// A phone is casting, so the song's name has a third line under it (`info_text`).
    pub casting: bool,
}

impl Metrics {
    pub fn new(screen: Rect) -> Self {
        let shorter = screen.width().min(screen.height());
        Self::scaled(screen, (shorter / REFERENCE).clamp(0.6, 1.4))
    }

    /// Kept in the window, the view is drawn at the size it has fullscreen on this monitor, so
    /// the two look alike, and only scaled down where the window is too small to hold it.
    pub fn in_window(card: Rect, monitor: Option<egui::Vec2>) -> Self {
        let full = monitor.map_or(REFERENCE, |m| m.x.min(m.y));
        let fits = card.width().min(card.height()) / WINDOWED_FIT;
        Self::scaled(card, (full / REFERENCE).min(fits).clamp(0.6, 1.4))
    }

    fn scaled(screen: Rect, s: f32) -> Self {
        Self {
            scale: s,
            portrait: screen.height() > screen.width(),
            margin: 56.0 * s,
            top_band: 76.0 * s,
            bottom_band: 150.0 * s,
            info_h: 84.0 * s,
            title: 30.0 * s,
            credits: 17.0 * s,
            lyrics: 32.0 * s,
            line_gap: 26.0 * s,
            lyrics_max_w: 640.0 * s,
            column_gap: 80.0 * s,
            icon: 18.0 * s,
            play: 60.0 * s,
            controls_w: 760.0 * s,
            narrow: 640.0 * s,
            casting: false,
        }
    }
}

/// What the view remembers between frames. Never saved: leaving the app closes it.
pub struct State {
    pub open: bool,
    /// Whether the lyrics column is asked for: seeded from the saved setting on opening, flipped by
    /// the Lyrics button (which saves it). A song with no lyrics hides the column without
    /// touching this.
    pub lyrics: bool,
    pub queue: bool,
    /// The queue was turned on after the lyrics, so where only one of them fits, it is the one
    /// shown ([`State::shown`]).
    pub queue_last: bool,
    /// Opened straight into fullscreen by F11, so F11 again goes back to the app rather than
    /// to the view in the window. Cleared once the window leaves fullscreen any other way.
    pub from_key: bool,
    /// Which of the two layouts is showing, through the window's change between them.
    pub frame: WindowFrame,
    /// The song the queue was last drawn under, while it is up.
    queue_song: Option<String>,
    /// The blurred cover behind everything, through a change of song.
    pub backdrop: Backdrop,
    /// The colour the background is washing toward.
    tint: Option<Color32>,
    /// The layout this frame holds only one of lyrics and queue: a vertical screen, where the
    /// queue is a sheet in the lyrics' place, or one too narrow for them side by side. Written by
    /// [`view`].
    pub exclusive: bool,
    last_motion: Instant,
    /// Whether this view asked the window for fullscreen, and so owns leaving it. A test run
    /// opens the view without touching the window.
    pub manages_window: bool,
    /// The window was already fullscreen when the view opened (the video view's own), so
    /// closing the view leaves it be.
    pub window_was_fullscreen: bool,
    /// Until when to wait for the window to report fullscreen before treating "not fullscreen"
    /// as the user having left it. Time, not frames: frames only run when something asks.
    pub grace_until: Instant,
    /// The eased lyric offset, in points from the first line's top, and the extra the wheel adds.
    scroll: f32,
    manual: f32,
    manual_at: Instant,
    /// The song the lyrics were last laid out for, so a new song snaps instead of gliding from
    /// the old one's place.
    scroll_key: Option<String>,
    /// The width the lyrics were last wrapped to. When it changes the lines re-flow and their
    /// heights change, so the offset is snapped to the active line rather than left pointing at
    /// where it used to be.
    wrap_width: f32,
    /// The cover and the song's name are drawn right above the queue this frame (at the head of
    /// its column, or over the sheet on a vertical screen); the queue then leaves out its own
    /// "Now playing" row, which would only say the same again.
    pub cover_in_drawer: bool,
    /// The queue list's scroll offset, content height and viewport height as of its last draw,
    /// written by `upnext::panel`.
    pub queue_scroll: (f32, f32, f32),
    /// When the lyrics request began (egui time), so their placeholder waits the same moment a
    /// page's does. A song's lyrics fetch is not a page load, so the page timer never covers it.
    lyrics_since: Option<f64>,
}

impl Default for State {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            open: false,
            lyrics: false,
            queue: false,
            queue_last: false,
            from_key: false,
            frame: WindowFrame::default(),
            queue_song: None,
            backdrop: Backdrop::default(),
            tint: None,
            exclusive: false,
            last_motion: now,
            manages_window: false,
            window_was_fullscreen: false,
            grace_until: now,
            scroll: 0.0,
            manual: 0.0,
            manual_at: now,
            scroll_key: None,
            wrap_width: 0.0,
            cover_in_drawer: false,
            queue_scroll: (0.0, 0.0, 0.0),
            lyrics_since: None,
        }
    }
}

impl State {
    /// Open straight away, for a screenshot run: no window change.
    pub fn opened(lyrics: bool) -> Self {
        Self {
            open: true,
            lyrics,
            ..Self::default()
        }
    }

    /// Open for real, in the window as it is, as the listener last left it: `lyrics` and `queue`
    /// say what starts showing, and `queue_front` which of them is in front where only one fits.
    /// Fullscreen is the listener's to ask for from inside the view (F11), so the view owns
    /// nothing about the window yet, even one already fullscreen (the video view's own).
    pub fn entering(
        lyrics: bool,
        queue: bool,
        queue_front: bool,
        window_was_fullscreen: bool,
    ) -> Self {
        Self {
            open: true,
            lyrics,
            queue,
            queue_last: queue_front,
            window_was_fullscreen,
            ..Self::default()
        }
    }

    /// The window has just been taken fullscreen (`on`) or out of it from inside the view. Going
    /// in, the view owns leaving again, and gets the same wait for the window to answer as on
    /// opening; coming out, nothing is left for it to undo.
    pub fn window_toggled(&mut self, on: bool) {
        self.manages_window = on;
        if !on {
            self.from_key = false;
        }
        self.window_was_fullscreen = false;
        self.grace_until = Instant::now() + Duration::from_millis(2000);
    }

    /// What is on screen of what the listener asked for, as `(lyrics, queue)`. Where the layout
    /// holds only one, it is the one turned on last; elsewhere both. Neither choice is dropped
    /// for it: widen the window and both are back, and a button only lights for what shows.
    /// `lyrics_available` is false for a song with no lyrics.
    pub fn shown(&self, lyrics_available: bool) -> (bool, bool) {
        let lyrics = self.lyrics && lyrics_available;
        if self.exclusive && lyrics && self.queue {
            return (!self.queue_last, self.queue_last);
        }
        (lyrics, self.queue)
    }

    /// The Queue button: hides a queue that shows, and otherwise brings it on, in front of the
    /// lyrics where only one fits.
    pub fn press_queue(&mut self, lyrics_available: bool) {
        if self.shown(lyrics_available).1 {
            self.queue = false;
        } else {
            self.queue = true;
            self.queue_last = true;
        }
    }

    /// The Lyrics button, the same way round. The new choice is in `lyrics`, for the caller to
    /// save.
    pub fn press_lyrics(&mut self, lyrics_available: bool) {
        if self.shown(lyrics_available).0 {
            self.lyrics = false;
        } else {
            self.lyrics = true;
            self.queue_last = false;
        }
    }

    /// A sign of life: the controls come back and the countdown starts over.
    pub fn touch(&mut self) {
        self.last_motion = Instant::now();
    }
}

/// Where the pieces sit in a window of this size.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub cover: Option<Rect>,
    /// The title and credits under the cover.
    pub info: Option<Rect>,
    /// The region the lyrics (or, on a vertical screen, the queue sheet) fill.
    pub region: Option<Rect>,
    /// Whether the info is centred under the cover or left-aligned.
    pub centred: bool,
}

impl Layout {
    /// Between two layouts: the cover and info glide, the rest takes `b`'s.
    pub fn lerp(a: &Layout, b: &Layout, t: f32) -> Layout {
        if t <= 0.0 {
            return *a;
        }
        if t >= 1.0 {
            return *b;
        }
        let rect = |x: Option<Rect>, y: Option<Rect>| match (x, y) {
            (Some(x), Some(y)) => Some(Rect::from_min_max(
                x.min + (y.min - x.min) * t,
                x.max + (y.max - x.max) * t,
            )),
            (x, y) => {
                if t < 0.5 {
                    x
                } else {
                    y
                }
            }
        };
        // A cover arriving or leaving (it steps aside for the words where there is no room for
        // both) grows from or shrinks to its own centre, as it resizes everywhere else, rather
        // than popping in or out halfway.
        let cover = match (a.cover, b.cover) {
            (None, Some(y)) => Some(Rect::from_center_size(y.center(), y.size() * t)),
            (Some(x), None) => Some(Rect::from_center_size(x.center(), x.size() * (1.0 - t))),
            (x, y) => rect(x, y),
        };
        Layout {
            cover,
            info: rect(a.info, b.info),
            region: b.region,
            centred: if t < 0.5 { a.centred } else { b.centred },
        }
    }
}

/// The cover's size within `max_w` by `max_h`: square, or 16:9 for a music video.
fn cover_size(max_w: f32, max_h: f32, wide: bool) -> (f32, f32) {
    if wide {
        let w = max_w.min(max_h * 16.0 / 9.0);
        (w, w * 9.0 / 16.0)
    } else {
        let side = max_w.min(max_h);
        (side, side)
    }
}

/// The room the view lays things out in: the screen less its margins and the controls' bands.
fn content_rect(screen: Rect, m: &Metrics) -> Rect {
    Rect::from_min_max(
        pos2(screen.left() + m.margin, screen.top() + m.top_band),
        pos2(screen.right() - m.margin, screen.bottom() - m.bottom_band),
    )
}

/// The width of the left-hand column beside the lyrics: the cover's, or the cover and queue's.
fn column_width(content: Rect, m: &Metrics) -> f32 {
    (content.width() * 0.40).clamp(280.0 * m.scale, 560.0 * m.scale)
}

/// Cover beside the lyrics: the cover as large as its column and the height allow, the words
/// at their own measure (`lyrics_max_w`), and the two centred as one block. The measure is fixed
/// rather than fitted to each song's lines: fitted, every song moved the whole view. It is set
/// for the lines songs mostly have, so the block sits balanced, and a longer line wraps. Returns
/// the block's left edge, the cover's size and the words' width.
fn side_by_side(content: Rect, m: &Metrics, wide: bool) -> (f32, f32, f32, f32) {
    let gap = 20.0 * m.scale;
    let height_for_cover = (content.height() - m.info_h - gap).max(120.0 * m.scale);
    let (cw, ch) = cover_size(column_width(content, m), height_for_cover, wide);
    let words = (content.width() - cw - m.column_gap).clamp(0.0, m.lyrics_max_w);
    let left = content.left() + (content.width() - (cw + m.column_gap + words)) / 2.0;
    (left, cw, ch, words)
}

/// How much of a vertical screen's height the cover takes over the lyrics, and over the queue
/// sheet: less there, so the list gets the rows, the cover resizing as one gives way to the other.
const LYRICS_COVER_SHARE: f32 = 0.38;
const QUEUE_COVER_SHARE: f32 = 0.24;

/// Stacked, for a vertical screen: the cover over the song's name, then the region for the
/// lyrics or the queue sheet. `share` is how much of the height the cover may take.
fn stacked(screen: Rect, m: &Metrics, wide: bool, share: f32) -> Layout {
    let content = content_rect(screen, m);
    let gap = 20.0 * m.scale;
    let (cw, ch) = cover_size(content.width(), content.height() * share, wide);
    let cover = Rect::from_min_size(
        pos2(content.center().x - cw / 2.0, content.top()),
        vec2(cw, ch),
    );
    let info = Rect::from_min_size(
        pos2(content.left(), cover.bottom() + gap),
        vec2(content.width(), m.info_h),
    );
    let region = Rect::from_min_max(
        pos2(content.left(), info.bottom() + gap * 0.6),
        content.right_bottom(),
    );
    // Too little left for words: they take everything and the cover steps aside.
    if region.height() < 160.0 * m.scale {
        return Layout {
            cover: None,
            info: None,
            region: Some(content),
            centred: false,
        };
    }
    Layout {
        cover: Some(cover),
        info: Some(info),
        region: Some(region),
        centred: true,
    }
}

/// `with_region` says whether there is a lyrics region (or queue sheet) to make room for; without
/// one the cover stands alone, centred. Beside the lyrics, see [`side_by_side`].
pub(crate) fn layout(screen: Rect, m: &Metrics, with_region: bool, wide: bool) -> Layout {
    let content = content_rect(screen, m);
    let gap = 20.0 * m.scale;
    let height_for_cover = (content.height() - m.info_h - gap).max(120.0 * m.scale);

    if with_region && m.portrait {
        return stacked(screen, m, wide, LYRICS_COVER_SHARE);
    }
    if with_region && content.width() < m.narrow {
        return Layout {
            cover: None,
            info: None,
            region: Some(content),
            centred: false,
        };
    }
    if with_region {
        let (left, cw, ch, words) = side_by_side(content, m, wide);
        let top = content.center().y - (ch + gap + m.info_h) / 2.0;
        let cover = Rect::from_min_size(pos2(left, top), vec2(cw, ch));
        let info = Rect::from_min_size(pos2(left, cover.bottom() + gap), vec2(cw, m.info_h));
        // The column lines up with the cover and name beside it. Under a video, which is short,
        // it keeps room to read, centred on them.
        let block = Rect::from_min_max(cover.min, info.max);
        let height = block.height().max(content.height() * 0.6);
        let region = Rect::from_center_size(
            pos2(left + cw + m.column_gap + words / 2.0, block.center().y),
            vec2(words, height),
        );
        return Layout {
            cover: Some(cover),
            info: Some(info),
            region: Some(region),
            centred: false,
        };
    }
    let widest = if m.portrait {
        content.width()
    } else if wide {
        1280.0 * m.scale
    } else {
        720.0 * m.scale
    };
    let (cw, ch) = cover_size(content.width().min(widest), height_for_cover, wide);
    let top = content.center().y - (ch + gap + m.info_h) / 2.0;
    let cover = Rect::from_center_size(pos2(content.center().x, top + ch / 2.0), vec2(cw, ch));
    let info = Rect::from_min_size(
        pos2(content.center().x - cw / 2.0, cover.bottom() + gap),
        vec2(cw, m.info_h),
    );
    Layout {
        cover: Some(cover),
        info: Some(info),
        region: None,
        centred: true,
    }
}

/// The layout with the queue open on a landscape screen: the left-hand column holds the cover
/// and the song's name at its head and the queue under them ([`drawer_split`]), and the lyrics
/// keep their place on the right, so opening the queue moves nothing but the cover. The column
/// is the cover's own width, laid out as [`layout`] lays out cover and lyrics. Without lyrics the
/// queue is on the right instead, in their place. With too little room for two columns, the
/// column stands alone, centred. Returns the layout and the queue list's rect.
pub(crate) fn queue_layout(
    screen: Rect,
    m: &Metrics,
    with_region: bool,
    wide: bool,
) -> (Layout, Rect) {
    let content = content_rect(screen, m);
    // No lyrics, with room for two columns: the queue takes their place on the right, and the
    // cover stays where it is beside them.
    if !with_region
        && content.width() >= m.narrow
        && let beside = layout(screen, m, true, wide)
        && let Some(list) = beside.region
    {
        return (
            Layout {
                region: None,
                ..beside
            },
            list,
        );
    }
    let beside = with_region && content.width() >= m.narrow;
    // Where the column is centred: on the closed cover, beside the lyrics, so the cover only
    // grows or shrinks in place; on the screen, alone.
    let (centre, room, region) = if beside {
        let (left, cw, _, _) = side_by_side(content, m, wide);
        (left + cw / 2.0, cw, layout(screen, m, true, wide).region)
    } else {
        let room = column_width(content, m).min(content.width());
        (content.center().x, room, None)
    };
    // As wide as the cover at its head, so the cover, the name and the list share their edges:
    // the cover is held to half the column's height, and on a wide screen that is narrower than
    // the room beside the lyrics.
    let inset = theme::CARD_INSET_X;
    let (cw, ch) = cover_size(
        room - 2.0 * inset,
        head_cover_height(content.height(), m),
        wide,
    );
    // But never so narrow that the rows lose their names: a short window holds the cover small,
    // and with no cover at all (too short for one) the list takes the whole room.
    let column = if ch < 120.0 * m.scale {
        room
    } else {
        (cw + 2.0 * inset).max(QUEUE_MIN_W * m.scale).min(room)
    };
    let left = centre - column / 2.0;
    let col = Rect::from_min_size(pos2(left, content.top()), vec2(column, content.height()));
    let (cover, info, list) = match drawer_split(col, m, wide) {
        Some((cover, info, list)) => (Some(cover), Some(info), list),
        None => (None, None, col),
    };
    (
        Layout {
            cover,
            info,
            region,
            centred: true,
        },
        list,
    )
}

/// How wide the cover and the column beside it are together, where there are two.
fn block_width(with: &Layout) -> Option<f32> {
    Some(with.region?.right() - with.cover?.left())
}

/// Whether the controls are showing: while something just moved, while paused (a still screen
/// with nothing to do should keep its buttons), or while the pointer is on them.
pub(crate) fn controls_visible(since_motion: Duration, paused: bool, hovering: bool) -> bool {
    paused || hovering || since_motion < HIDE_AFTER
}

/// One frame of the glide toward `target`: closes a fixed share of the gap, so it eases out
/// rather than stopping dead, and lands exactly once it is within a quarter of a point.
pub(crate) fn ease(offset: f32, target: f32, dt: f32) -> f32 {
    if (target - offset).abs() < 0.25 {
        return target;
    }
    offset + (target - offset) * (1.0 - (-dt * EASE_RATE).exp())
}

/// How bright line `index` wants to be: the active one in full, the ones behind it dim and
/// fading, the ones still to come a little brighter than that, since they are what you read next.
pub(crate) fn line_alpha(index: usize, active: Option<usize>) -> f32 {
    match active {
        None => 0.55,
        Some(a) if index == a => 1.0,
        Some(a) if index < a => (0.40 - 0.05 * (a - index - 1) as f32).max(0.18),
        Some(a) => (0.55 - 0.08 * (index - a - 1) as f32).max(0.22),
    }
}

/// Whether the song is known to have no lyrics: the request for *this* song has finished and
/// came back with nothing (or only empty lines). Not true before the request starts or while it
/// runs, so the column does not flicker shut as the view opens.
pub(crate) fn lyrics_missing(for_current_song: bool, loading: bool, lines: Option<usize>) -> bool {
    for_current_song && !loading && lines.unwrap_or(0) == 0
}

pub fn view(app: &mut App, ui: &mut egui::Ui) {
    let before = app.current_route();
    draw(app, ui);
    // A page can't show behind the view: whatever went somewhere (the title, an artist, a menu,
    // a row in the queue), the view gets out of the way for it.
    if app.current_route() != before {
        let ctx = ui.ctx().clone();
        app.fs_exit(&ctx);
    }
}

fn draw(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let p = app.palette;
    let window = test_screen().unwrap_or_else(|| ctx.content_rect());
    // Kept in the window, the view takes the shell's shape: a title strip the height of the top
    // bar, over a card inset by the gutter as the page card is. Fullscreen, it is the screen.
    let was = app.fs.frame.shown;
    let windowed = app.fs.frame.step(
        in_window(&ctx),
        ctx.content_rect().size(),
        ctx.input(|i| i.viewport().monitor_size),
        Instant::now(),
    );
    if app.fs.frame.holding() {
        ctx.request_repaint();
    }
    // The change is one jump, not a reason to glide.
    if was.is_some_and(|was| was != windowed) {
        ctx.clear_animations();
    }
    let (screen, m, radius) = if windowed {
        let card = Rect::from_min_max(
            pos2(
                window.left() + theme::GUTTER,
                window.top() + theme::TITLEBAR_HEIGHT,
            ),
            pos2(
                window.right() - theme::GUTTER,
                window.bottom() - theme::GUTTER,
            ),
        );
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        (
            card,
            Metrics::in_window(card, monitor),
            CornerRadius::same(theme::radii().panel),
        )
    } else {
        (window, Metrics::new(window), CornerRadius::ZERO)
    };
    // While a phone is casting, the song's name has a third line under it (`info_text`).
    let mut m = m;
    if app.casting_from().is_some() {
        m.casting = true;
        m.info_h += m.credits * 1.3;
    }
    let strip = Rect::from_min_max(
        window.min,
        pos2(window.right(), window.top() + theme::TITLEBAR_HEIGHT),
    );
    let now = Instant::now();

    // Any sign of life brings the controls back.
    let alive = ctx.input(|i| {
        i.pointer.delta() != egui::Vec2::ZERO
            || i.pointer.any_down()
            || i.smooth_scroll_delta != egui::Vec2::ZERO
            || !i.keys_down.is_empty()
    });
    if alive {
        app.fs.touch();
    }

    let adaptive = app.settings.adaptive_art_color;
    let Some(track) = app.now.track.as_ref() else {
        if adaptive {
            wash_background(ui, &p, screen, p.window, radius);
        } else {
            plain_background(ui, &p, screen, radius);
        }
        return;
    };
    let art_url = track.thumbnail.clone();
    let video_id = track.video_id.clone();
    let video = app.video_open && track.wide;

    // Starts from the window's own colour when the cover's isn't known yet, and through a change
    // of song keeps the last one until the next is known, rather than something bright between.
    let known = crate::ui::header_wash_colour(app, art_url.as_ref());
    app.fs.tint = tint_target(app.fs.tint, art_url.is_some(), known);
    let tint = animated(
        &ctx,
        app.fs.tint.unwrap_or(p.window),
        if was.is_none() { 0.0 } else { TINT_SECS },
    );
    if adaptive {
        wash_background(ui, &p, screen, tint, radius);
        paint_backdrop(app, ui, screen, radius, art_url.as_ref());
    } else {
        plain_background(ui, &p, screen, radius);
    }

    // This view's own placeholders time themselves off the lyrics request, not off a page load
    // (the shell is not drawn, so nothing else is using the timer this frame).
    let time = ctx.input(|i| i.time);
    app.fs.lyrics_since = app
        .lyrics_loading
        .then(|| app.fs.lyrics_since.unwrap_or(time));
    widgets::set_skeleton_since(&ctx, app.fs.lyrics_since);

    // What is shown, and how far each part has eased in. The lyrics are asked for by the saved
    // setting but only shown when the song has some.
    let missing = app.lyrics_missing();
    app.fs.exclusive = m.portrait || content_rect(screen, &m).width() < m.narrow;
    let (show_lyrics, show_queue) = app.fs.shown(!missing);
    let lyrics_t = ctx.animate_bool_with_time(Id::new("fs-lyrics-t"), show_lyrics, 0.35);
    // Whether the cover shares the screen with anything (the words, or the queue where it
    // takes their place), as its own value: where the two take turns it stays put through the
    // swap. Taken from the two fades instead, it dipped while one ran out and the other in, and
    // the cover wobbled toward standing alone and back.
    let shares = show_lyrics || (app.fs.exclusive && show_queue);
    let region_t = ctx.animate_bool_with_time(Id::new("fs-region-t"), shares, 0.35);
    let queue_t = ctx.animate_bool_with_time(Id::new("fs-drawer"), show_queue, 0.28);
    // Two arrangements, by the screen's shape. Landscape: the cover beside the lyrics, and with
    // the queue open the cover heads the queue in that same column ([`queue_layout`]), the lyrics
    // staying where they are. Portrait: the cover over the lyrics, the queue a sheet in their
    // place. Each glides between its states rather than switching.
    let alone = layout(screen, &m, false, video);
    let with = layout(screen, &m, true, video);
    let (lay, centre_t, info_m, region, region_alpha, wrap_w, list) = if m.portrait {
        // Over the queue the cover takes less of the height, so the list has the rows; it
        // resizes as the lyrics and the queue take turns.
        let sheet = stacked(screen, &m, video, QUEUE_COVER_SHARE);
        let stack = Layout::lerp(&with, &sheet, queue_t);
        let lay = Layout::lerp(&alone, &stack, region_t);
        let centre_t = mix(flag(alone.centred), flag(stack.centred), region_t);
        let list = sheet
            .region
            .unwrap_or(screen)
            .translate(vec2(0.0, (1.0 - queue_t) * 40.0 * m.scale));
        app.fs.cover_in_drawer = show_queue && sheet.cover.is_some();
        let small = drawer_type(&m);
        let info_m = Metrics {
            title: mix(m.title, small.title, queue_t),
            credits: mix(m.credits, small.credits, queue_t),
            ..m
        };
        (
            lay,
            centre_t,
            info_m,
            with.region,
            lyrics_t * (1.0 - queue_t),
            with.region.map(|r| r.width()),
            list,
        )
    } else {
        let closed = Layout::lerp(&alone, &with, region_t);
        let closed_centre = mix(flag(alone.centred), flag(with.centred), region_t);
        let (open_alone, list_alone) = queue_layout(screen, &m, false, video);
        let (open_with, list_with) = queue_layout(screen, &m, true, video);
        let open = Layout::lerp(&open_alone, &open_with, region_t);
        let lay = Layout::lerp(&closed, &open, queue_t);
        // The name settles at the queue head's smaller size only as the queue comes under the
        // cover. With the queue on the right it stays the name beside the words would be.
        let small = drawer_type(&m);
        let head_t = queue_t * region_t;
        let info_m = Metrics {
            title: mix(m.title, small.title, head_t),
            credits: mix(m.credits, small.credits, head_t),
            ..m
        };
        // The lyrics keep their place beside the column; where there was no room for them beside
        // the queue, they fade out as it opens instead. Wrapped to the width they settle at, not
        // one changing every frame, which would re-flow the lines as they glide.
        let region = match (with.region, open_with.region) {
            (Some(a), Some(b)) => Some(mix_rect(a, b, queue_t)),
            (a, _) => a,
        };
        let alpha = lyrics_t
            * if open_with.region.is_some() {
                1.0
            } else {
                1.0 - queue_t
            };
        let settle = if show_queue {
            open_with.region.or(with.region)
        } else {
            with.region
        };
        app.fs.cover_in_drawer = show_queue && open.cover.is_some();
        let list = mix_rect(list_alone, list_with, region_t)
            .translate(vec2(0.0, (1.0 - queue_t) * 24.0 * m.scale));
        (
            lay,
            mix(
                closed_centre,
                mix(flag(open_alone.centred), 1.0, region_t),
                queue_t,
            ),
            info_m,
            region,
            alpha,
            settle.map(|r| r.width()),
            list,
        )
    };

    let cover_hit = lay
        .cover
        .map(|cover| paint_cover(app, ui, cover, art_url.as_ref(), video));
    let song = lay
        .info
        .map(|info| info_text(app, ui, &info_m, info, centre_t));
    let (menu, go) = match (cover_hit, song) {
        (Some(cover), Some(song)) => (Some(cover.union(song.menu)), song.go),
        (cover, song) => {
            let go = song.as_ref().and_then(|s| s.go.clone());
            (cover.or(song.map(|s| s.menu)), go)
        }
    };
    if let Some(route) = go {
        app.navigate(route);
    }
    // The song's own menu, the one every row and the player bar has.
    if let Some(menu) = menu
        && let Some(track) = app.now.track.as_ref()
    {
        let action = widgets::row_menu(
            ui,
            &p,
            &menu,
            widgets::RowMenu {
                track,
                playlist_id: None,
                playlists: &app.library.playlists.items,
                signed_in: app.signed_in(),
                queue_index: None,
            },
        );
        if let Some(action) = action {
            app.row_action(action);
        }
    }
    if let Some(region) = region
        && region_alpha > 0.01
    {
        let region = Rect::from_min_size(
            region.min,
            vec2(wrap_w.unwrap_or(region.width()), region.height()),
        );
        let mut layer = ui.new_child(UiBuilder::new().max_rect(region));
        layer.multiply_opacity(region_alpha);
        lyrics_column(app, &mut layer, &m, region, &video_id);
    }
    if queue_t > 0.001 {
        // Asked here rather than in the panel: the side panel's queue keeps its place through a
        // change of song, where it sits beside a page you may be reading it against.
        let browsing = ctx.pointer_hover_pos().is_some_and(|at| list.contains(at));
        if queue_goes_to_top(app.fs.queue_song.as_deref(), &video_id, browsing) {
            app.queue_scroll_reset = true;
        }
        app.fs.queue_song = Some(video_id.clone());
        queue_region(app, ui, &m, list, queue_t);
    } else {
        app.fs.queue_song = None;
    }

    // The controls, as one layer that fades.
    let bottom = Rect::from_min_max(
        pos2(screen.left() + m.margin, screen.bottom() - m.bottom_band),
        pos2(screen.right() - m.margin, screen.bottom()),
    );
    let top = Rect::from_min_max(
        pos2(screen.left(), screen.top()),
        pos2(screen.right(), screen.top() + m.top_band),
    );
    if windowed {
        title_strip(app, ui, strip, missing);
    }
    // Two columns: the controls span them, so the bar shares their outer edges.
    let two_columns = if m.portrait {
        0.0
    } else {
        region_t.max(queue_t)
    };
    let controls_w = mix(
        m.controls_w,
        block_width(&with).unwrap_or(m.controls_w),
        two_columns,
    );
    let bar_inset = mix(120.0 * m.scale, 0.0, two_columns);
    let hovering = ctx.pointer_hover_pos().is_some_and(|pos| {
        bottom.contains(pos) || top.contains(pos) || (windowed && strip.contains(pos))
    });
    let since = now.saturating_duration_since(app.fs.last_motion);
    let paused = app.player_state.paused || !app.playback_started;
    let visible = controls_visible(since, paused, hovering);
    let alpha = ctx.animate_bool_with_time(Id::new("fs-controls"), visible, 0.25);
    if visible && !paused && !hovering {
        ctx.request_repaint_after(HIDE_AFTER.saturating_sub(since) + Duration::from_millis(30));
    }
    if alpha < 0.02 {
        // Nothing to click, and nothing to point at.
        ctx.set_cursor_icon(egui::CursorIcon::None);
        return;
    }
    let mut layer = ui.new_child(UiBuilder::new().max_rect(screen));
    layer.multiply_opacity(alpha);
    controls(
        app,
        &mut layer,
        &m,
        Band {
            bottom,
            top,
            width: controls_w,
            bar_inset,
        },
        missing,
        windowed,
    );
}

/// The sharp cover asked for ahead of the view: the most a cover is ever fetched at.
const PREPARED_COVER_PT: f32 = 512.0;
/// How long a sharper cover takes to fade in over the one standing in for it.
const COVER_FADE_SECS: f32 = 0.25;
/// How long lyrics that have just arrived take to fade in.
const LYRICS_ARRIVE_SECS: f32 = 0.3;

/// Ask for what the view shows of the current song as soon as it starts, so that opening the view
/// finds it ready instead of filling in while you watch. Cheap on every frame: both are lookups
/// once asked for.
pub fn prepare(app: &mut App, ctx: &egui::Context) {
    let Some(url) = app.now.track.as_ref().and_then(|t| t.thumbnail.clone()) else {
        return;
    };
    let url = Some(&url);
    crate::ui::art_texture(&mut app.images, &app.backend, ctx, url, PREPARED_COVER_PT);
    if app.settings.adaptive_art_color {
        crate::ui::art_texture_backdrop(&mut app.images, &app.backend, url);
    }
}

/// The cover, or the music video playing where it would be, at its own 16:9.
fn paint_cover(
    app: &mut App,
    ui: &mut egui::Ui,
    cover: Rect,
    art_url: Option<&std::sync::Arc<str>>,
    video: bool,
) -> egui::Response {
    let ctx = ui.ctx().clone();
    if video {
        // The cover's own curve, so a video and a cover in the same place round alike.
        let radius = crate::widgets::ArtShape::Wide.radius(cover);
        ui.painter().rect_filled(cover, radius, Color32::BLACK);
        crate::ui::video::paint_frame(app, ui, cover, radius);
        let hit = ui.interact(cover, Id::new("fs-video"), Sense::click());
        if hit.clicked() {
            app.toggle_playback();
        }
        return hit;
    }
    // The cover at this size may not be fetched yet: until it lands, the one [`prepare`] asked
    // for stands in, or the bar's small one if that hasn't come either, and it fades in over them.
    let large = crate::ui::art_texture(&mut app.images, &app.backend, &ctx, art_url, cover.width())
        .cloned();
    let stand_in = crate::ui::art_texture(
        &mut app.images,
        &app.backend,
        &ctx,
        art_url,
        PREPARED_COVER_PT,
    )
    .cloned()
    .or_else(|| {
        crate::ui::art_texture(&mut app.images, &app.backend, &ctx, art_url, 56.0).cloned()
    });
    let p = app.palette;
    let shape = widgets::ArtShape::Square;
    let shown = ctx.animate_bool_with_time(
        Id::new(("fs-cover", art_url)),
        large.is_some(),
        COVER_FADE_SECS,
    );
    match (&stand_in, &large) {
        (Some(_), Some(large)) => {
            widgets::paint_art(ui, &p, cover, stand_in.as_ref(), shape);
            widgets::paint_art_over(ui, cover, large, shape, shown);
        }
        (None, Some(_)) => {
            widgets::paint_art_faded(ui, &p, cover, large.as_ref(), shape, shown);
        }
        (_, None) => widgets::paint_art(ui, &p, cover, stand_in.as_ref(), shape),
    }
    ui.interact(cover, Id::new("fs-cover-hit"), Sense::click())
}

/// Whether the window is in the state for the in-window layout.
fn in_window(ctx: &egui::Context) -> bool {
    !crate::window_fullscreen::is(ctx)
}

/// How long a layout waits for the window's new size, when one could still come. A change between
/// two states of the same size never brings one; where that is known, nothing waits (see
/// [`WindowFrame::step`]).
const WINDOW_SIZE_WAIT: Duration = Duration::from_millis(500);

/// How recent a change of size must be to count as the window's answer to a change of state.
const WINDOW_SIZE_RECENT: Duration = Duration::from_millis(100);

/// Which layout is showing, as the window goes in or out of fullscreen. The new layout on the old
/// size puts the cover at a wrong size for those frames: the wiggle. So the layout changes with
/// the size. The two come in either order, and on Wayland in the same frame: X11 reports the state
/// first, so a size that has already come counts as well as one still to come.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowFrame {
    shown: Option<bool>,
    /// When the window's state changed, while the layout waits.
    held: Option<Instant>,
    /// The size last frame, and when it last changed.
    size: Option<egui::Vec2>,
    resized: Option<Instant>,
    /// The size while last in the window, which leaving fullscreen goes back to.
    windowed_size: Option<egui::Vec2>,
}

/// Two sizes the same to within a point, rounding aside.
fn same_size(a: egui::Vec2, b: egui::Vec2) -> bool {
    (a - b).abs().max_elem() < 1.0
}

impl WindowFrame {
    /// The layout for this frame, `true` for in the window. `monitor` is the screen's size, where
    /// known.
    ///
    /// No size is coming when the window is already the size it is going to: the screen's going
    /// fullscreen (a window filling a screen with no panels, or a hidden one), or the size it had
    /// in the window coming back out. Then the layout changes at once instead of waiting.
    fn step(
        &mut self,
        windowed: bool,
        size: egui::Vec2,
        monitor: Option<egui::Vec2>,
        now: Instant,
    ) -> bool {
        if self.size.replace(size).is_some_and(|last| last != size) {
            self.resized = Some(now);
        }
        let shown = *self.shown.get_or_insert(windowed);
        if shown == windowed {
            self.held = None;
            if windowed {
                self.windowed_size = Some(size);
            }
            return shown;
        }
        let since = *self.held.get_or_insert(now);
        let answered = self
            .resized
            .is_some_and(|at| now.duration_since(at) < WINDOW_SIZE_RECENT);
        let already_there = if windowed {
            self.windowed_size.is_some_and(|w| same_size(w, size))
        } else {
            monitor.is_some_and(|m| same_size(m, size))
        };
        if answered || already_there || now.duration_since(since) >= WINDOW_SIZE_WAIT {
            self.shown = Some(windowed);
            self.held = None;
            return windowed;
        }
        shown
    }

    /// Whether the layout is waiting for the window, and frames must keep coming.
    fn holding(&self) -> bool {
        self.held.is_some()
    }
}

/// The title strip of a view kept in the window, laid out as the top bar is: the window is
/// dragged from it, its buttons sit where the top bar puts them (on macOS those are the traffic
/// lights, at the other end), and the view's toggles are centred between. It stays up while the
/// controls below fade, as the top bar would.
fn title_strip(app: &mut App, ui: &mut egui::Ui, strip: Rect, missing: bool) {
    let p = app.palette;
    crate::chrome::drag_surface(ui, strip, app.decorated, "now-playing");
    if !app.decorated && !crate::chrome::NATIVE_WINDOW_BUTTONS {
        // The top bar's own inset: the gutter and its frame's margin inside it.
        let inner = strip.shrink2(vec2(theme::GUTTER + 6.0, 0.0));
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if crate::chrome::window_buttons(&mut right, &p) {
            app.request_close(ui.ctx());
        }
    }
    let gap = ui.spacing().item_spacing.x;
    let width = 4.0 * (STRIP_ICON + 14.0) + 3.0 * gap;
    let row = Rect::from_center_size(strip.center(), vec2(width, STRIP_ICON + 14.0));
    let mut tui = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    toggles(app, &mut tui, STRIP_ICON, missing, true);
}

/// A test aid for debug builds: `REFRYT_FS_SIZE=600x1000` lays the view out in a rectangle that
/// size in the window's corner, so a tall screen can be checked on a machine whose window manager
/// will not give one.
#[cfg(debug_assertions)]
fn test_screen() -> Option<Rect> {
    let (w, h) = std::env::var("REFRYT_FS_SIZE")
        .ok()?
        .split_once('x')
        .map(|(w, h)| (w.parse::<f32>().ok(), h.parse::<f32>().ok()))?;
    Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(w?, h?)))
}

#[cfg(not(debug_assertions))]
fn test_screen() -> Option<Rect> {
    None
}

/// The two ends of the background's wash: the cover's tint over the window colour at the top,
/// nearly gone at the bottom. Painted at the shell's own opacity, so on a see-through window
/// the desktop shows through the wash the way it does through the gutter.
fn wash(p: &theme::Palette, tint: Color32) -> (Color32, Color32) {
    let strength = if p.dark { (0.47, 0.07) } else { (0.31, 0.055) };
    (
        p.window
            .lerp_to_gamma(tint, strength.0)
            .gamma_multiply(p.gutter_alpha),
        p.window
            .lerp_to_gamma(tint, strength.1)
            .gamma_multiply(p.gutter_alpha),
    )
}

/// A base with the cover's tint washing down from the top. As opaque as the shell is: solid by default, and see-through (and blurred, where the
/// compositor does that) when the window is.
fn wash_background(
    ui: &egui::Ui,
    p: &theme::Palette,
    screen: Rect,
    tint: Color32,
    radius: CornerRadius,
) {
    let (top, bottom) = wash(p, tint);
    gradient(ui, screen, radius, top, bottom);
    card_edge(ui, p, screen, radius);
}

/// With Adaptive colour off the view is a card like the pages, in the same colours.
fn plain_background(ui: &egui::Ui, p: &theme::Palette, screen: Rect, radius: CornerRadius) {
    ui.painter().rect_filled(screen, radius, p.window_fill());
    ui.painter().rect_filled(screen, radius, p.card_fill());
    card_edge(ui, p, screen, radius);
}

/// Kept in the window the view is a card, with the cards' own hairline where the theme draws one.
fn card_edge(ui: &egui::Ui, p: &theme::Palette, screen: Rect, radius: CornerRadius) {
    if radius != CornerRadius::ZERO
        && let Some(edge) = p.card_edge()
    {
        ui.painter().rect_stroke(
            screen,
            radius,
            egui::Stroke::new(1.0, edge),
            egui::StrokeKind::Inside,
        );
    }
}

/// How long a new backdrop takes to fade in over the last one: as long as the tint under it takes.
const BACKDROP_FADE_SECS: f64 = TINT_SECS as f64;

/// The blurred covers on screen: the current one, and while it fades in, the one before.
#[derive(Clone, Default)]
pub struct Backdrop {
    current: Option<egui::TextureHandle>,
    previous: Option<egui::TextureHandle>,
    /// When the current one arrived, in the input's seconds.
    since: f64,
    /// Whether a frame has passed. One ready on the first frame needs no fade: the view is
    /// fading nothing out, it just opened.
    started: bool,
}

impl std::fmt::Debug for Backdrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backdrop").finish_non_exhaustive()
    }
}

impl Backdrop {
    /// Take in this frame's cover, if it has loaded, and say how far in the current one is. A
    /// song whose backdrop hasn't arrived keeps the last one up rather than dropping to the wash,
    /// and one that arrives during a fade waits for its end: three covers can't crossfade.
    fn step(&mut self, want: Option<egui::TextureHandle>, now: f64) -> f32 {
        let done = now - self.since >= BACKDROP_FADE_SECS;
        if done {
            self.previous = None;
        }
        if let Some(want) = want
            && self.current.as_ref().map(|c| c.id()) != Some(want.id())
            && (done || self.current.is_none())
        {
            self.since = if self.started { now } else { f64::NEG_INFINITY };
            self.previous = self.current.replace(want);
        }
        self.started = true;
        ((now - self.since) / BACKDROP_FADE_SECS).clamp(0.0, 1.0) as f32
    }
}

/// What to paint the last cover and the next one at, in that order, to crossfade them `t` of the
/// way at `strength`. One over the other, each covers some of what is beneath: solved so that what
/// shows of the background stays the same throughout, and nothing is left to remove at the end.
fn crossfade(strength: f32, t: f32) -> (f32, f32) {
    let next = strength * t;
    (strength * (1.0 - t) / (1.0 - next), next)
}

/// The part of a texture that covers a rect without stretching: the middle of it, cropped.
pub(crate) fn cover_uv(texture: egui::Vec2, rect: egui::Vec2) -> Rect {
    let scale = (rect.x / texture.x).max(rect.y / texture.y);
    let shown = vec2(rect.x / (texture.x * scale), rect.y / (texture.y * scale));
    Rect::from_center_size(pos2(0.5, 0.5), shown)
}

/// How much of the blurred cover shows over the wash: a texture in the colour, not a picture.
const BACKDROP_STRENGTH: f32 = 0.225;

/// The window colour coming back over the cover, faint at the top and nearly whole at the bottom,
/// so the view keeps the wash's shape and settles into the app's own colour.
fn veil(p: &theme::Palette) -> (Color32, Color32) {
    (p.window.gamma_multiply(0.05), p.window.gamma_multiply(0.9))
}

/// The blurred cover, faintly over the wash and under its veil.
fn paint_backdrop(
    app: &mut App,
    ui: &egui::Ui,
    screen: Rect,
    radius: CornerRadius,
    art_url: Option<&std::sync::Arc<str>>,
) {
    let art_url = art_url.filter(|_| app.settings.adaptive_art_color);
    let want = crate::ui::art_texture_backdrop(&mut app.images, &app.backend, art_url).cloned();
    let now = ui.ctx().input(|i| i.time);
    let t = app.fs.backdrop.step(want, now);
    if t < 1.0 {
        ui.ctx().request_repaint();
    }
    let p = app.palette;
    let backdrop = &app.fs.backdrop;
    let image = |texture: &egui::TextureHandle, alpha: f32| {
        let uv = cover_uv(texture.size_vec2(), screen.size());
        let tint = Color32::WHITE.gamma_multiply(alpha);
        ui.painter().add(
            egui::epaint::RectShape::filled(screen, radius, tint).with_texture(texture.id(), uv),
        );
    };
    let Some(current) = &backdrop.current else {
        return;
    };
    // Eased, so the fade neither starts nor stops with a visible edge.
    let t = t * t * (3.0 - 2.0 * t);
    let (last, next) = crossfade(BACKDROP_STRENGTH * p.gutter_alpha, t);
    if let Some(previous) = &backdrop.previous {
        image(previous, last);
    }
    image(current, next);
    // The veil comes in with the first image, over the wash that has none.
    let veil_alpha = if backdrop.previous.is_some() { 1.0 } else { t } * p.gutter_alpha;
    let (top, bottom) = veil(&p);
    gradient(
        ui,
        screen,
        radius,
        top.gamma_multiply(veil_alpha),
        bottom.gamma_multiply(veil_alpha),
    );
}

/// A rect, with rounded corners or without, shaded from `top` to `bottom`.
fn gradient(ui: &egui::Ui, screen: Rect, radius: CornerRadius, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    if radius == CornerRadius::ZERO {
        mesh.colored_vertex(screen.left_top(), top);
        mesh.colored_vertex(screen.right_top(), top);
        mesh.colored_vertex(screen.right_bottom(), bottom);
        mesh.colored_vertex(screen.left_bottom(), bottom);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        ui.painter().add(egui::Shape::mesh(mesh));
        return;
    }
    // Cut to the window's corners: each vertex coloured by its height and the anti-aliased edge
    // keeping the soft alpha the tessellator gave it.
    let mut tessellator = egui::epaint::Tessellator::new(
        ui.ctx().pixels_per_point(),
        egui::epaint::TessellationOptions::default(),
        [1, 1],
        Vec::new(),
    );
    tessellator.tessellate_shape(
        egui::Shape::rect_filled(screen, radius, Color32::WHITE),
        &mut mesh,
    );
    for vertex in &mut mesh.vertices {
        let t = ((vertex.pos.y - screen.top()) / screen.height()).clamp(0.0, 1.0);
        vertex.color = top
            .lerp_to_gamma(bottom, t)
            .gamma_multiply(f32::from(vertex.color.a()) / 255.0);
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// How long the tint takes to reach a new colour.
const TINT_SECS: f32 = 0.9;

/// The colour to wash toward: the cover's when known, otherwise the last one while a cover is on
/// its way. A song with no cover has none to wait for.
fn tint_target(last: Option<Color32>, has_cover: bool, known: Option<Color32>) -> Option<Color32> {
    known.or(last.filter(|_| has_cover))
}

/// The tint eased toward its target over `secs`, so a song change washes into the next colour
/// instead of switching. No time at all when the view opens, which has nothing to wash from.
fn animated(ctx: &egui::Context, tint: Color32, secs: f32) -> Color32 {
    let channel = |i: usize, v: u8| {
        ctx.animate_value_with_time(Id::new(("fs-tint", i)), f32::from(v), secs)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(
        channel(0, tint.r()),
        channel(1, tint.g()),
        channel(2, tint.b()),
    )
}

/// The title and credits, left-aligned in `rect` or centred in it, or anywhere between as the
/// layout glides: `centre_t` is 0 for left and 1 for centred.
fn info_text(app: &App, ui: &mut egui::Ui, m: &Metrics, rect: Rect, centre_t: f32) -> SongHits {
    let p = app.palette;
    let track = app.now.track.as_ref();
    let title = track.map_or("", |t| t.title.as_str());
    let album = track.is_some_and(|t| t.album_id.is_some());
    let at = |full: f32| rect.left() + ((rect.width() - full) / 2.0).max(0.0) * centre_t;

    // The title, as in the player bar: a link to its album, scrolling on hover when too long.
    let title_font = theme::bold(m.title);
    let title_h = m.title * 1.4;
    let full = widgets::text_width(ui, title, &title_font);
    let width = full.min(rect.width());
    let pos = pos2(at(full), rect.top() + title_h / 2.0);
    let hit_rect = Rect::from_center_size(
        pos2(pos.x + width / 2.0, pos.y),
        vec2(width.max(1.0), title_h),
    );
    let title_hit = widgets::marquee_line(
        ui,
        pos,
        rect.width(),
        title,
        title_font,
        p.text,
        "fs-title",
        Sense::click(),
        false,
    )
    .unwrap_or_else(|| ui.interact(hit_rect, Id::new("fs-title-hit"), Sense::click()));
    if album && title_hit.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().hline(
            pos.x..=(pos.x + width),
            pos.y + m.title * 0.58,
            egui::Stroke::new((m.title / 16.0).max(1.0), p.text),
        );
    }

    // The credits: one link per artist, over one hit rect for the menu. The rect first, so the
    // links are on top of it.
    let credits_font = theme::regular(m.credits);
    let y = rect.top() + title_h + m.credits * 0.8;
    let line = track.map(|t| t.artist_line()).unwrap_or_default();
    let full = widgets::text_width(ui, &line, &credits_font);
    let x = at(full);
    let credits_hit = ui.interact(
        Rect::from_center_size(
            pos2(x + full.min(rect.width()) / 2.0, y),
            vec2(full.min(rect.width()).max(1.0), m.credits * 1.6),
        ),
        Id::new("fs-credits-hit"),
        Sense::click(),
    );
    let link = track.and_then(|t| {
        widgets::paint_subtitle_marquee(
            ui,
            &p,
            pos2(x, y),
            rect.width(),
            &crate::ui::track_subtitle_no_album(t),
            None,
            "fs-credits",
            false,
            credits_font,
            p.secondary,
        )
        .map(crate::ui::link_route)
    });
    // While a phone is casting, a line under the credits says which.
    if let Some(casting) = app.casting_from().filter(|_| track.is_some()) {
        let font = theme::medium((m.credits * 0.85).max(11.0));
        let full = widgets::text_width(ui, &casting, &font);
        ui.painter().text(
            pos2(at(full), y + m.credits * 1.35),
            egui::Align2::LEFT_CENTER,
            widgets::truncate_to(ui, &casting, font.clone(), rect.width()),
            font,
            p.accent,
        );
    }
    let album_route = title_hit
        .clicked()
        .then(|| track.and_then(|t| t.album_id.clone()).map(Route::Album))
        .flatten();
    SongHits {
        menu: title_hit.union(credits_hit),
        go: link.or(album_route),
    }
}

/// What the song's name answered this frame: where a right-click opens its menu, and where a
/// click on a link wants to go.
struct SongHits {
    menu: egui::Response,
    go: Option<Route>,
}

/// Bars shaped like the lines that are about to arrive: the real lines' height, pitch and left
/// edge, the first resting where the words wait before a song's first line (see
/// [`lyrics_column`]), the widths varying as lines do and the whole fading toward the bottom the
/// way the words melt. Scaled and fitted to whichever region the layout gave, so it is the same
/// shape side by side or stacked.
fn lyrics_skeleton(ui: &mut egui::Ui, p: &theme::Palette, m: &Metrics, rect: Rect) {
    const WIDTHS: [f32; 8] = [0.88, 0.64, 0.80, 0.52, 0.84, 0.60, 0.74, 0.56];
    let line_h = ui
        .painter()
        .layout_no_wrap("Ag".to_owned(), theme::semibold(m.lyrics), Color32::WHITE)
        .size()
        .y;
    let pitch = line_h + m.line_gap;
    let bar_h = line_h * 0.52;
    // Against this view's tinted background the app's usual grey would all but vanish: lift the
    // fill from the text colour, as the shimmer's own glow already is.
    let mut fill = *p;
    fill.surface_hover = p.text.gamma_multiply(0.12);
    let first = rect.top() + rect.height() * 0.12;
    for i in 0..WIDTHS.len() * 2 {
        let middle = first + i as f32 * pitch;
        if middle + bar_h / 2.0 > rect.bottom() {
            break;
        }
        let bar = Rect::from_min_size(
            pos2(rect.left(), middle - bar_h / 2.0),
            vec2(rect.width() * WIDTHS[i % WIDTHS.len()], bar_h),
        );
        // Each bar fades with its place in the list, so the column trails off.
        let mut child = ui.new_child(UiBuilder::new().max_rect(bar));
        child.multiply_opacity((1.0 - 0.09 * i as f32).max(0.15));
        widgets::skeleton_fill(&child, bar, CornerRadius::same(theme::radii().small), &fill);
    }
}

/// The words, large, gliding to keep the line being sung in place.
/// The lyrics in `rect`, wrapped to its width.
fn lyrics_column(app: &mut App, ui: &mut egui::Ui, m: &Metrics, rect: Rect, video_id: &str) {
    let p = app.palette;
    let ctx = ui.ctx().clone();
    // An animation starts at its target the first time it is asked, so lyrics already here when
    // the view opens are simply there; only ones arriving while it watches fade in.
    let arrived = ctx.animate_bool_with_time(
        Id::new(("fs-lyrics-arrive", video_id)),
        !app.lyrics_loading,
        LYRICS_ARRIVE_SECS,
    );
    if app.lyrics_loading {
        lyrics_skeleton(ui, &p, m, rect);
        return;
    }
    ui.multiply_opacity(arrived);
    let Some(lyrics) = app.lyrics.as_ref() else {
        return;
    };
    if lyrics.lines.is_empty() {
        return;
    }

    let font = theme::semibold(m.lyrics);
    let galleys: Vec<_> = lyrics
        .lines
        .iter()
        .map(|line| {
            let text = if line.text.is_empty() {
                "♪"
            } else {
                line.text.as_str()
            };
            ui.painter().layout(
                text.to_owned(),
                font.clone(),
                Color32::PLACEHOLDER,
                rect.width(),
            )
        })
        .collect();
    let mut tops = Vec::with_capacity(galleys.len());
    let mut y = 0.0;
    for galley in &galleys {
        tops.push(y);
        y += galley.size().y + m.line_gap;
    }

    // A test aid for debug builds: `REFRYT_PLAIN_LYRICS=1` shows timed lyrics as plain text, the
    // way a song without timings is shown, to check that view without hunting for one.
    #[cfg(debug_assertions)]
    let synced = lyrics.synced && std::env::var_os("REFRYT_PLAIN_LYRICS").is_none();
    #[cfg(not(debug_assertions))]
    let synced = lyrics.synced;
    let position_ms = (app.player_state.position * 1000.0) as u64;
    let active = synced
        .then(|| lyrics.lines.iter().rposition(|l| l.at_ms <= position_ms))
        .flatten();

    // Unsynced words have nowhere to glide to: a plain scrolling column.
    if !synced {
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
        let from = painted_so_far(&child);
        let plain_font = theme::semibold(m.lyrics * 0.75);
        let gap = m.line_gap * 0.55;
        let total: f32 = lyrics
            .lines
            .iter()
            .map(|line| {
                let text = if line.text.is_empty() {
                    "♪".to_owned()
                } else {
                    line.text.clone()
                };
                ui.painter()
                    .layout(text, plain_font.clone(), Color32::PLACEHOLDER, rect.width())
                    .size()
                    .y
                    + gap
            })
            .sum();
        let output = egui::ScrollArea::vertical()
            .id_salt("fs-plain-lyrics")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                // Words that all fit sit in the middle of their column.
                ui.add_space(((rect.height() - total) / 2.0).max(0.0));
                for line in &lyrics.lines {
                    let text = if line.text.is_empty() {
                        "♪"
                    } else {
                        &line.text
                    };
                    ui.label(
                        egui::RichText::new(text)
                            .font(plain_font.clone())
                            .color(p.text.gamma_multiply(0.85)),
                    );
                    ui.add_space(gap);
                }
            });
        // The same band the timed lyrics melt over, once there is that much past the edge.
        let fade = LYRICS_FADE * m.scale;
        let offset = output.state.offset.y;
        let past = output.content_size.y - offset - output.inner_rect.height();
        fade_painted(
            &ctx,
            child.layer_id(),
            from,
            output.inner_rect,
            EdgeFade::for_hidden(offset, fade),
            EdgeFade::for_hidden(past, fade),
        );
        return;
    }

    // Where the active line's middle is, and the glide toward it.
    // Before the first line is sung nothing is active: the words wait near the top of their
    // region rather than leaving it empty above, then glide down to the anchor.
    let first = galleys[0].size().y / 2.0;
    // Words that all fit don't scroll: they sit in the middle of their column, as one block.
    let total = y - m.line_gap;
    let fits = total + 2.0 * LYRICS_FADE * m.scale <= rect.height();
    let anchor_y = rect.height() * ANCHOR;
    let target = if fits {
        anchor_y - (rect.height() - total) / 2.0
    } else {
        active
            .map(|a| tops[a] + galleys[a].size().y / 2.0)
            .unwrap_or(first + rect.height() * (ANCHOR - 0.12))
    };
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.05);
    let now = Instant::now();
    // A new wrap width re-flows every line, so offsets measured against the old one point at the
    // wrong place: snap to the active line, as for a new song, instead of gliding there.
    let rewrapped = (app.fs.wrap_width - rect.width()).abs() > 0.5;
    app.fs.wrap_width = rect.width();
    if app.fs.scroll_key.as_deref() != Some(video_id) || rewrapped {
        app.fs.scroll_key = Some(video_id.to_owned());
        app.fs.scroll = target;
        app.fs.manual = 0.0;
    } else {
        app.fs.scroll = ease(app.fs.scroll, target, dt);
    }
    // The wheel browses ahead or back; a few quiet seconds later the view glides home.
    if !fits
        && ctx
            .pointer_hover_pos()
            .is_some_and(|pos| rect.contains(pos))
    {
        let wheel = ctx.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            app.fs.manual -= wheel;
            app.fs.manual_at = now;
        }
    }
    if now.saturating_duration_since(app.fs.manual_at) > BROWSE_FOR {
        app.fs.manual = ease(app.fs.manual, 0.0, dt);
    }
    if app.fs.scroll != target || app.fs.manual != 0.0 {
        ctx.request_repaint();
    }

    let anchor = rect.top() + anchor_y;
    let view = app.fs.scroll + app.fs.manual;
    let painter = ui.painter().with_clip_rect(rect);
    let melt = LYRICS_FADE * m.scale;
    let from = painted_so_far(ui);
    let mut clicked = None;
    for (i, galley) in galleys.iter().enumerate() {
        let top_y = anchor + tops[i] - view;
        let height = galley.size().y;
        if top_y + height < rect.top() - 40.0 || top_y > rect.bottom() + 40.0 {
            continue;
        }
        let line_rect = Rect::from_min_size(pos2(rect.left(), top_y), vec2(rect.width(), height));
        let hit = line_rect
            .expand2(vec2(12.0, m.line_gap / 2.0))
            .intersect(rect);
        let response = ui.interact(hit, Id::new(("fs-lyric", i)), Sense::click());
        let mut want = line_alpha(i, active);
        if response.hovered() {
            want = want.max(0.85);
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let shown = ctx.animate_value_with_time(Id::new(("fs-lyric-a", i)), want, 0.28);
        painter.galley(line_rect.min, galley.clone(), p.text.gamma_multiply(shown));
        if response.clicked() {
            clicked = Some(lyrics.lines[i].at_ms);
        }
    }
    // The words melt away toward the top and bottom of their column, across each line's own
    // height, rather than a line being cut where the column ends.
    fade_painted(
        &ctx,
        ui.layer_id(),
        from,
        rect,
        EdgeFade::eased(melt, 1.0),
        EdgeFade::eased(melt, 1.0),
    );
    if let Some(at_ms) = clicked {
        app.seek_to(at_ms as f64 / 1000.0);
        app.fs.manual = 0.0;
    }
}

/// Whether the queue shows from its top this frame: when it has just come up, and when the song
/// has changed, unless the pointer is on the list, which may be someone reading further down it.
fn queue_goes_to_top(last: Option<&str>, song: &str, browsing: bool) -> bool {
    last.is_none_or(|last| last != song && !browsing)
}

/// The queue, borderless like the lyrics: its rows sit straight on the background, fading out at
/// the top and bottom. `t` is how far in it is.
fn queue_region(app: &mut App, ui: &mut egui::Ui, m: &Metrics, rect: Rect, t: f32) {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.multiply_opacity(t);
    let from = painted_so_far(&child);
    crate::ui::upnext::panel(app, &mut child);

    // The edges fade out — but only an edge with more list past it: at the top of the list the
    // first row must be seen in full. The rows themselves are made transparent toward the edge,
    // so whatever is behind them shows: the cover's wash, or the desktop through a see-through
    // window. A band painted in the background's colour only matched a flat, opaque background,
    // and anywhere else read as a box over the end of the list.
    let fade = 88.0 * m.scale;
    let (above, below) = edge_fades(app.fs.queue_scroll, fade);
    fade_painted(
        ui.ctx(),
        child.layer_id(),
        from,
        rect,
        EdgeFade::eased(fade, above),
        EdgeFade::eased(fade, below),
    );
}

/// The drawer split for a cover at its head: the cover (or video) as wide as the drawer, down to
/// about half its height, the song's name under it, and the list in what is left. `None` when the
/// drawer is too short for a cover worth showing and a list under it.
pub(crate) fn drawer_split(drawer: Rect, m: &Metrics, wide: bool) -> Option<(Rect, Rect, Rect)> {
    // Lined up with the rows' own art, which sit in from the drawer's edge by the card inset.
    let inner = drawer.shrink2(vec2(theme::CARD_INSET_X, 0.0));
    let (cw, ch) = cover_size(inner.width(), head_cover_height(drawer.height(), m), wide);
    if ch < 120.0 * m.scale {
        return None;
    }
    // Centred, with the name under it: when the drawer is taller than wide enough the cover
    // fills it, and when it is held to half the height the head stays one balanced block rather
    // than a cover hugging one edge with a gap beside it.
    let cover = Rect::from_min_size(pos2(inner.center().x - cw / 2.0, inner.top()), vec2(cw, ch));
    let small = drawer_type(m);
    let info = Rect::from_min_size(
        pos2(inner.left(), cover.bottom() + 14.0 * m.scale),
        vec2(inner.width(), head_name_height(&small)),
    );
    let list = Rect::from_min_max(pos2(drawer.left(), info.bottom()), drawer.max);
    Some((cover, info, list))
}

/// The rows the queue keeps under a cover at its head, whatever the height: its header and
/// three songs. In the queue's own sizes, which do not scale with the view.
/// The narrowest the queue column gets under a small cover, so its rows keep readable names.
const QUEUE_MIN_W: f32 = 300.0;

const HEAD_LIST_MIN: f32 = 30.0 + 3.0 * theme::ROW_HEIGHT;

/// How tall the cover at the head of a column `height` tall may be: half of it, and never so
/// much that the queue under it is left fewer rows than [`HEAD_LIST_MIN`]. On a short window
/// the cover gives way to the list, not the list to the cover.
fn head_cover_height(height: f32, m: &Metrics) -> f32 {
    let name = 14.0 * m.scale + head_name_height(&drawer_type(m));
    (height * 0.5).min(height - name - HEAD_LIST_MIN)
}

/// The name under the cover at the queue's head: title, credits, and the casting line if any.
fn head_name_height(small: &Metrics) -> f32 {
    small.title * 1.4 + small.credits * if small.casting { 2.9 } else { 1.6 }
}

/// `a` to `b` by `t`.
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn flag(on: bool) -> f32 {
    f32::from(u8::from(on))
}

/// One rect gliding into another by `t`.
fn mix_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min + (b.min - a.min) * t, a.max + (b.max - a.max) * t)
}

/// The song's name at the head of the drawer: a size between the big view's and the queue
/// rows' own, so it heads the list without crowding it out.
fn drawer_type(m: &Metrics) -> Metrics {
    Metrics {
        title: 21.0 * m.scale,
        credits: 15.0 * m.scale,
        ..*m
    }
}

/// Where the controls go: the bands they own, how wide they run, and how far in from that the
/// seek bar stops, leaving its time labels inside.
struct Band {
    bottom: Rect,
    top: Rect,
    width: f32,
    bar_inset: f32,
}

fn controls(
    app: &mut App,
    ui: &mut egui::Ui,
    m: &Metrics,
    band: Band,
    missing: bool,
    windowed: bool,
) {
    let p = app.palette;
    let s = m.scale;
    let Band {
        bottom,
        top,
        width,
        bar_inset,
    } = band;
    let column = Rect::from_center_size(
        bottom.center(),
        vec2(bottom.width().min(width), bottom.height()),
    );

    // Seek.
    let (position, duration) = crate::ui::player_bar::clock(app);
    let fraction = if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let bar = Rect::from_center_size(
        pos2(column.center().x, column.top() + 24.0 * s),
        vec2((column.width() - bar_inset).max(60.0), 4.0),
    );
    if let Some(to) = widgets::seek_bar(ui, &p, bar, fraction as f32, duration, position, true) {
        app.seek_to(to);
    }

    // Transport, centred under the bar.
    let row_w = 4.0 * (m.icon + 14.0) + m.play + 4.0 * ui.spacing().item_spacing.x;
    let row = Rect::from_center_size(
        pos2(column.center().x, column.top() + 50.0 * s + m.play / 2.0),
        vec2(row_w, m.play),
    );
    let mut cui = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    if widgets::icon_button(&mut cui, &p, Icon::Shuffle, m.icon, app.q.is_shuffled())
        .hint(crate::ui::tips::shuffle(app.q.is_shuffled()))
        .clicked()
    {
        app.toggle_shuffle();
    }
    let mut skip = 0isize;
    if widgets::icon_button(&mut cui, &p, Icon::SkipBackFilled, m.icon, false)
        .hint(crate::ui::tips::PREVIOUS)
        .clicked()
    {
        skip = -1;
    }
    let playing = app.playback_started && !app.player_state.paused;
    let loading = widgets::track_loading(cui.ctx());
    let play = widgets::play_button(&mut cui, &p, playing || loading, m.play)
        .hint(crate::ui::tips::play(playing || loading));
    if loading {
        widgets::paint_spinner(
            &cui,
            play.rect.center(),
            play.rect.width() / 2.0 + 3.0,
            2.0,
            p.accent,
        );
    }
    if play.clicked() {
        app.toggle_playback();
    }
    if widgets::icon_button(&mut cui, &p, Icon::SkipForwardFilled, m.icon, false)
        .hint(crate::ui::tips::NEXT)
        .clicked()
    {
        skip = 1;
    }
    let repeat = if app.repeat == RepeatMode::One {
        Icon::Repeat1
    } else {
        Icon::Repeat
    };
    if widgets::icon_button(&mut cui, &p, repeat, m.icon, app.repeat != RepeatMode::Off)
        .hint(crate::ui::tips::repeat(app.repeat))
        .clicked()
    {
        app.cycle_repeat();
    }
    if skip != 0 {
        app.skip(skip);
    }

    // Thumbs at the left of the row, volume at the right. Both give way to the transport on a
    // narrow window rather than sitting on it.
    let side_room = (column.width() - row_w) / 2.0;
    let mut rate = None;
    if app.signed_in()
        && side_room >= 72.0
        && let Some(track) = app.now.track.as_ref()
    {
        let thumbs =
            Rect::from_min_size(pos2(column.left(), row.center().y - 16.0), vec2(72.0, 32.0));
        let mut tui = ui.new_child(
            UiBuilder::new()
                .max_rect(thumbs)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        for (up, pressed) in [(true, LikeStatus::Like), (false, LikeStatus::Dislike)] {
            let (hit, _) = tui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
            if widgets::rate_button(
                &mut tui,
                &p,
                hit,
                Id::new(("fs-thumb", up)),
                up,
                track.like == pressed,
                true,
            )
            .hint(crate::ui::tips::rate(up, track.like == pressed))
            .clicked()
            {
                rate = Some(RowAction::Rate {
                    video_id: track.video_id.clone(),
                    status: track.like.toggled(pressed),
                });
            }
        }
    }
    if let Some(action) = rate {
        app.row_action(action);
    }
    if side_room >= 140.0 {
        let volume_rect = Rect::from_min_max(
            pos2(column.right() - 140.0, row.top()),
            pos2(column.right(), row.bottom()),
        );
        let mut vui = ui.new_child(
            UiBuilder::new()
                .max_rect(volume_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let volume = if app.muted { 0.0 } else { app.last_volume };
        if let Some(v) = widgets::volume_slider(&mut vui, &p, volume, app.last_volume)
            && let Some(pl) = &mut app.player
        {
            app.last_volume = v;
            app.muted = v <= 0.0;
            pl.set_volume(v);
        }
    }

    // Toggles, top-right: Exit sits in the corner, then Fullscreen, Queue and Lyrics to its
    // left. Kept in the window they are in the title strip instead (`title_strip`).
    if windowed {
        return;
    }
    let toggles_rect = Rect::from_min_max(
        pos2(top.right() - m.margin - 210.0 * s, top.top() + 18.0 * s),
        pos2(
            top.right() - m.margin + 8.0,
            top.top() + 18.0 * s + m.icon + 14.0,
        ),
    );
    let mut tui = ui.new_child(
        UiBuilder::new()
            .max_rect(toggles_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    toggles(app, &mut tui, m.icon, missing, false);
}

/// Exit, Fullscreen, Queue and Lyrics, into a right-to-left `ui`, so Exit is the rightmost.
fn toggles(app: &mut App, ui: &mut egui::Ui, icon: f32, missing: bool, windowed: bool) {
    let p = app.palette;
    let exit_hint = if windowed {
        crate::ui::tips::CLOSE_PLAYER
    } else {
        crate::ui::tips::EXIT_FULLSCREEN
    };
    let (glyph, hint) = if windowed {
        (Icon::Maximize, crate::ui::tips::FULLSCREEN)
    } else {
        (Icon::Minimize, crate::ui::tips::LEAVE_FULLSCREEN)
    };
    if widgets::icon_button(ui, &p, glyph, icon, false)
        .hint(hint)
        .clicked()
    {
        app.fs_toggle_window(ui.ctx());
    }
    let (show_lyrics, show_queue) = app.fs.shown(!missing);
    if widgets::icon_button(ui, &p, Icon::ListMusic, icon, show_queue)
        .hint(crate::ui::tips::QUEUE)
        .clicked()
    {
        app.fs.press_queue(!missing);
        app.fs_save_choice(ui.ctx());
    }
    // A song with no lyrics greys the button out and says so; the saved choice is untouched.
    let lyrics_button = ui.add_enabled_ui(!missing, |ui| {
        widgets::icon_button(ui, &p, Icon::MicVocal, icon, show_lyrics)
            .hint(crate::ui::tips::LYRICS)
    });
    if lyrics_button.inner.clicked() {
        app.fs.press_lyrics(!missing);
        app.fs_save_choice(ui.ctx());
    }
    if missing {
        lyrics_button
            .response
            .on_disabled_hover_text("No lyrics for this song");
    }
    // Laid out right to left: closing the view comes first, window fullscreen last.
    if widgets::icon_button(ui, &p, Icon::Minimize2, icon, false)
        .hint(exit_hint)
        .clicked()
    {
        app.fs_exit(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the queue open beside the lyrics, the cover heads the column it had, the lyrics keep
    /// their place, and the queue list runs down the column under the cover.
    #[test]
    fn the_queue_opens_under_the_cover_and_the_lyrics_stay() {
        for (w, h) in [
            (1920.0, 1080.0),
            (2560.0, 1440.0),
            (1366.0, 768.0),
            (1100.0, 720.0),
        ] {
            let screen = screen(w, h);
            let m = Metrics::new(screen);
            let closed = layout(screen, &m, true, false);
            let (open, list) = queue_layout(screen, &m, true, false);
            let (cover, region) = (open.cover.unwrap(), open.region.unwrap());
            assert_eq!(region, closed.region.unwrap(), "{w}x{h}: the lyrics moved");
            let was = closed.cover.unwrap();
            assert!(
                cover.left() >= was.left() - 0.5 && cover.right() <= region.left(),
                "{w}x{h}"
            );
            assert!(list.top() >= open.info.unwrap().bottom() && list.left() <= cover.left());
            assert!(
                list.right() < region.left(),
                "{w}x{h}: the list runs under the words"
            );
        }
    }

    /// The cover and the words are one block centred on the screen, the same for every song:
    /// the space left of the cover is the space right of the words' measure.
    #[test]
    fn the_block_is_centred_and_the_same_for_every_song() {
        for (w, h) in [(1920.0, 1080.0), (2560.0, 1440.0), (1366.0, 768.0)] {
            let screen = screen(w, h);
            let m = Metrics::new(screen);
            let l = layout(screen, &m, true, false);
            let (cover, region) = (l.cover.unwrap(), l.region.unwrap());
            assert!(
                (region.width() - m.lyrics_max_w).abs() < 0.5,
                "{w}x{h}: measure"
            );
            assert!(
                (cover.left() - (w - region.right())).abs() < 0.5,
                "{w}x{h}: the sides differ"
            );
        }
    }

    /// With the queue open the column is the cover's width and centred where the closed cover
    /// was: the cover shrinks in place and its edges are the list's.
    #[test]
    fn the_queue_column_shares_the_covers_edges() {
        for (w, h) in [(1920.0, 1080.0), (2560.0, 1440.0), (1100.0, 720.0)] {
            let screen = screen(w, h);
            let m = Metrics::new(screen);
            let closed = layout(screen, &m, true, false).cover.unwrap();
            let (open, list) = queue_layout(screen, &m, true, false);
            let cover = open.cover.unwrap();
            assert!(
                (cover.center().x - closed.center().x).abs() < 0.5,
                "{w}x{h}: moved"
            );
            let want = (cover.width() + 2.0 * theme::CARD_INSET_X).max(QUEUE_MIN_W * m.scale);
            assert!(
                (list.width() - want).abs() < 0.5,
                "{w}x{h}: the list is not the cover's width"
            );
        }
    }

    /// On a window too narrow for two columns, the column stands alone in the middle, with
    /// lyrics or without.
    #[test]
    fn the_queue_column_stands_alone_without_room_for_two() {
        for region in [true, false] {
            let screen = screen(420.0, 300.0);
            let m = Metrics::new(screen);
            let (open, list) = queue_layout(screen, &m, region, false);
            assert!(open.region.is_none());
            assert!((list.center().x - screen.center().x).abs() < 0.5);
        }
    }

    /// With lyrics off the queue takes their place on the right, and the cover and name keep
    /// the place they have beside the lyrics.
    #[test]
    fn without_lyrics_the_queue_goes_where_they_were() {
        for (w, h) in [(1920.0, 1080.0), (1366.0, 768.0), (1100.0, 720.0)] {
            let screen = screen(w, h);
            let m = Metrics::new(screen);
            let beside = layout(screen, &m, true, false);
            let (open, list) = queue_layout(screen, &m, false, false);
            assert_eq!(
                list,
                beside.region.unwrap(),
                "{w}x{h}: not the lyrics' place"
            );
            assert_eq!(open.cover, beside.cover, "{w}x{h}: the cover moved");
            assert_eq!(open.info, beside.info, "{w}x{h}: the name moved");
            assert!(open.region.is_none() && !open.centred);
        }
    }

    /// Beside the lyrics the column lines up with the cover and name, on every landscape size.
    #[test]
    fn the_column_lines_up_with_the_cover_and_name() {
        for (w, h) in [(1920.0, 1080.0), (1366.0, 768.0), (1100.0, 720.0)] {
            let screen = screen(w, h);
            let m = Metrics::new(screen);
            let lay = layout(screen, &m, true, false);
            let (cover, info, region) =
                (lay.cover.unwrap(), lay.info.unwrap(), lay.region.unwrap());
            assert!((region.top() - cover.top()).abs() < 0.5, "{w}x{h}: top");
            assert!(
                (region.bottom() - info.bottom()).abs() < 0.5,
                "{w}x{h}: bottom"
            );
        }
    }

    /// A video is short, and the column beside it keeps room to read.
    #[test]
    fn the_column_beside_a_video_keeps_its_room() {
        let screen = screen(1920.0, 1080.0);
        let m = Metrics::new(screen);
        let lay = layout(screen, &m, true, true);
        let room = content_rect(screen, &m).height() * 0.6;
        assert!(lay.region.unwrap().height() >= room - 0.5);
    }

    /// With two columns the controls span them and stay centred; alone there is no block.
    #[test]
    fn the_controls_span_the_two_columns() {
        let screen = screen(1920.0, 1080.0);
        let m = Metrics::new(screen);
        let with = layout(screen, &m, true, false);
        let width = block_width(&with).unwrap();
        let region = with.region.unwrap();
        let left = with.cover.unwrap().left();
        assert!((left + width - region.right()).abs() < 0.5);
        assert!(((left + region.right()) / 2.0 - screen.center().x).abs() < 0.5);
        assert!(block_width(&layout(screen, &m, false, false)).is_none());
    }

    /// The backdrop fills the view without stretching: a square texture on a wide window shows
    /// its middle band at full width.
    #[test]
    fn the_backdrop_covers_without_stretching() {
        let uv = cover_uv(vec2(96.0, 96.0), vec2(1920.0, 1080.0));
        assert!((uv.width() - 1.0).abs() < 1e-4);
        assert!((uv.height() - 0.5625).abs() < 1e-4);
        assert!((uv.center().y - 0.5).abs() < 1e-4);
        let tall = cover_uv(vec2(96.0, 96.0), vec2(600.0, 900.0));
        assert!((tall.height() - 1.0).abs() < 1e-4);
        assert!((tall.width() - 2.0 / 3.0).abs() < 1e-4);
    }

    /// A backdrop ready on the first frame is simply there; a later one fades in over the last,
    /// which then lets go of its texture.
    #[test]
    fn a_new_backdrop_fades_in_over_the_last() {
        let ctx = egui::Context::default();
        let texture = |name: &str| {
            ctx.load_texture(
                name,
                egui::ColorImage::example(),
                egui::TextureOptions::LINEAR,
            )
        };
        let mut backdrop = Backdrop::default();
        assert_eq!(backdrop.step(Some(texture("a")), 10.0), 1.0);
        assert!(backdrop.previous.is_none());
        let b = texture("b");
        assert_eq!(backdrop.step(Some(b.clone()), 11.0), 0.0);
        assert!(backdrop.previous.is_some());
        assert!(backdrop.step(Some(b.clone()), 11.3) > 0.2);
        assert_eq!(backdrop.step(None, 11.1 + BACKDROP_FADE_SECS), 1.0);
        assert!(backdrop.previous.is_none());
        assert_eq!(backdrop.current.map(|c| c.id()), Some(b.id()));
    }

    /// A cover that comes during a fade waits for it to end: another song skipped to mid-fade
    /// would otherwise put the half-faded one on at full strength.
    #[test]
    fn a_backdrop_that_comes_during_a_fade_waits_for_its_end() {
        let ctx = egui::Context::default();
        let texture = |name: &str| {
            ctx.load_texture(
                name,
                egui::ColorImage::example(),
                egui::TextureOptions::LINEAR,
            )
        };
        let (a, b, c) = (texture("a"), texture("b"), texture("c"));
        let mut backdrop = Backdrop::default();
        backdrop.step(Some(a), 10.0);
        backdrop.step(Some(b.clone()), 11.0);
        assert!(backdrop.step(Some(c.clone()), 11.2) > 0.0);
        assert_eq!(backdrop.current.as_ref().map(|t| t.id()), Some(b.id()));
        assert_eq!(
            backdrop.step(Some(c.clone()), 11.0 + BACKDROP_FADE_SECS),
            0.0
        );
        assert_eq!(backdrop.current.as_ref().map(|t| t.id()), Some(c.id()));
    }

    /// A crossfade shows the same amount of the background throughout, and ends with the last
    /// cover at nothing, so taking it away changes nothing on screen.
    #[test]
    fn a_crossfade_has_nothing_left_to_remove_at_its_end() {
        let strength = BACKDROP_STRENGTH;
        assert_eq!(crossfade(strength, 0.0), (strength, 0.0));
        assert_eq!(crossfade(strength, 1.0), (0.0, strength));
        for i in 0..=20 {
            let (last, next) = crossfade(strength, i as f32 / 20.0);
            assert!((((1.0 - last) * (1.0 - next)) - (1.0 - strength)).abs() < 1e-5);
        }
    }

    /// The queue starts from its top whenever it comes up and with each new song, but doesn't
    /// move under a pointer on the list.
    #[test]
    fn the_queue_goes_to_its_top_unless_it_is_being_read() {
        assert!(queue_goes_to_top(None, "a", false));
        assert!(queue_goes_to_top(None, "a", true));
        assert!(!queue_goes_to_top(Some("a"), "a", false));
        assert!(queue_goes_to_top(Some("a"), "b", false));
        assert!(!queue_goes_to_top(Some("a"), "b", true));
    }

    /// The tint holds through a change of song, and nothing bright stands in for a missing one.
    #[test]
    fn the_tint_holds_until_the_next_colour_is_known() {
        let (a, b) = (Color32::RED, Color32::BLUE);
        assert_eq!(tint_target(None, true, None), None);
        assert_eq!(tint_target(None, true, Some(a)), Some(a));
        assert_eq!(tint_target(Some(a), true, None), Some(a));
        assert_eq!(tint_target(Some(a), true, Some(b)), Some(b));
        assert_eq!(tint_target(Some(a), false, None), None);
    }

    /// X11 says fullscreen before it has the size. The layout changes with the size, in both
    /// directions.
    #[test]
    fn the_layout_changes_with_the_windows_size() {
        let now = Instant::now();
        let later = now + Duration::from_secs(5);
        let (small, big) = (vec2(1100.0, 720.0), vec2(1920.0, 1080.0));
        let mut frame = WindowFrame::default();
        assert!(frame.step(true, small, None, now));
        assert!(
            frame.step(false, small, None, now),
            "fullscreen on the old size"
        );
        assert!(frame.holding());
        assert!(!frame.step(false, big, None, now));
        assert!(!frame.holding());
        assert!(
            !frame.step(true, big, None, later),
            "in the window on the old size"
        );
        assert!(frame.step(true, small, None, later));
    }

    /// Wayland brings the state and the size in the same frame, which leaves nothing to wait for.
    #[test]
    fn the_layout_changes_at_once_when_the_size_comes_with_the_state() {
        let now = Instant::now();
        let later = now + Duration::from_secs(5);
        let (small, big) = (vec2(1100.0, 720.0), vec2(1920.0, 1080.0));
        let mut frame = WindowFrame::default();
        assert!(frame.step(true, small, None, now));
        assert!(!frame.step(false, big, None, now));
        assert!(!frame.holding());
        assert!(frame.step(true, small, None, later));
        assert!(!frame.holding());
    }

    /// A size just ahead of the state was the answer already. One from long before wasn't.
    #[test]
    fn a_size_just_ahead_of_the_state_counts() {
        let now = Instant::now();
        let soon = now + Duration::from_millis(30);
        let (small, big) = (vec2(1100.0, 720.0), vec2(1920.0, 1080.0));
        let mut frame = WindowFrame::default();
        assert!(frame.step(true, small, None, now));
        assert!(frame.step(true, big, None, now));
        assert!(!frame.step(false, big, None, soon));

        let mut frame = WindowFrame::default();
        assert!(frame.step(true, small, None, now));
        assert!(frame.step(true, big, None, now));
        let long_after = now + Duration::from_secs(5);
        assert!(frame.step(false, big, None, long_after), "an old resize");
        assert!(frame.holding());
    }

    /// A change that never brings a new size doesn't hold the layout for good.
    #[test]
    fn the_layout_stops_waiting_for_a_size_that_never_comes() {
        let now = Instant::now();
        let size = vec2(1920.0, 1080.0);
        let mut frame = WindowFrame::default();
        assert!(frame.step(true, size, None, now));
        assert!(frame.step(false, size, None, now));
        assert!(!frame.step(false, size, None, now + WINDOW_SIZE_WAIT));
        assert!(!frame.holding());
    }

    /// A window already the size of the screen gets no new size going fullscreen, and goes
    /// straight to the fullscreen layout rather than waiting for one.
    #[test]
    fn a_window_the_size_of_the_screen_goes_fullscreen_at_once() {
        let now = Instant::now();
        let screen = vec2(1920.0, 1080.0);
        let mut frame = WindowFrame::default();
        assert!(frame.step(true, screen, Some(screen), now));
        assert!(!frame.step(false, screen, Some(screen), now));
        assert!(!frame.holding());
        // And back out to the same size, at once too.
        assert!(frame.step(true, screen, Some(screen), now));
        assert!(!frame.holding());
    }

    /// A smaller window still waits for its new size, and comes back out once it has it.
    #[test]
    fn a_smaller_window_still_waits_for_its_size() {
        let now = Instant::now();
        let (small, screen) = (vec2(1100.0, 720.0), vec2(1920.0, 1080.0));
        let mut frame = WindowFrame::default();
        assert!(frame.step(true, small, Some(screen), now));
        assert!(frame.step(false, small, Some(screen), now));
        assert!(frame.holding());
        let later = now + Duration::from_secs(1);
        assert!(!frame.step(false, screen, Some(screen), later));
        assert!(!frame.step(true, screen, Some(screen), later + Duration::from_secs(1)));
        assert!(frame.holding(), "out of fullscreen on the screen's size");
    }

    /// A drawer heading with the cover stacks cover, name and list top to bottom inside it, the
    /// cover square and no taller than half the drawer; a drawer too short for one gets none.
    #[test]
    fn drawer_split_stacks_inside_the_drawer() {
        let m = Metrics::new(screen(1100.0, 720.0));
        let drawer = Rect::from_min_size(pos2(700.0, 70.0), vec2(320.0, 480.0));
        let (cover, info, list) = drawer_split(drawer, &m, false).expect("room for a cover");
        assert!(drawer.contains_rect(cover) && drawer.contains_rect(list));
        assert_eq!(cover.width(), cover.height());
        assert!(cover.height() <= drawer.height() * 0.5);
        assert!(cover.bottom() < info.top() && info.bottom() <= list.top());
        assert_eq!(list.bottom(), drawer.bottom());
        let short = Rect::from_min_size(pos2(700.0, 70.0), vec2(320.0, 120.0));
        assert!(drawer_split(short, &m, false).is_none());
    }

    /// Where both fit, both show and each button flips its own.
    #[test]
    fn lyrics_and_queue_both_show_where_they_fit() {
        let mut fs = State::entering(true, false, false, false);
        fs.press_queue(true);
        assert_eq!(fs.shown(true), (true, true));
        fs.press_lyrics(true);
        assert_eq!(fs.shown(true), (false, true));
        assert!(!fs.lyrics && fs.queue);
    }

    /// Where only one fits, turning one on puts it in front, a button lights only for what
    /// shows, and pressing the hidden one brings it forward rather than turning it off. Both
    /// choices survive: with room again, both show.
    #[test]
    fn where_only_one_fits_the_last_turned_on_shows() {
        let mut fs = State::entering(true, false, false, false);
        fs.exclusive = true;
        fs.press_queue(true);
        assert_eq!(fs.shown(true), (false, true), "the queue goes in front");
        fs.press_lyrics(true);
        assert_eq!(
            fs.shown(true),
            (true, false),
            "the hidden lyrics come forward"
        );
        assert!(fs.lyrics && fs.queue, "nothing was turned off");
        fs.press_lyrics(true);
        assert_eq!(
            fs.shown(true),
            (false, true),
            "hiding the lyrics leaves the queue"
        );
        fs.press_lyrics(true);
        fs.exclusive = false;
        assert_eq!(
            fs.shown(true),
            (true, true),
            "a wider window shows both again"
        );
    }

    /// A song without lyrics leaves the queue to show alone, whichever came last.
    #[test]
    fn without_lyrics_the_queue_shows_alone() {
        let mut fs = State::entering(true, false, false, false);
        fs.exclusive = true;
        fs.queue = true;
        assert_eq!(fs.shown(false), (false, true));
    }

    /// However short the screen, the queue under a cover keeps its header and three rows; a
    /// screen too short for both drops the cover rather than the rows.
    #[test]
    fn the_queue_keeps_its_rows_under_the_cover() {
        for (w, h) in [
            (1920.0, 1080.0),
            (1366.0, 768.0),
            (900.0, 600.0),
            (760.0, 480.0),
        ] {
            let screen = screen(w, h);
            let m = Metrics::new(screen);
            for region in [true, false] {
                let (open, list) = queue_layout(screen, &m, region, false);
                if open.cover.is_some() {
                    assert!(list.height() >= HEAD_LIST_MIN - 0.5, "{w}x{h}: rows lost");
                }
                assert!(
                    list.width() >= QUEUE_MIN_W * m.scale - 0.5,
                    "{w}x{h}: rows too narrow for their names"
                );
            }
        }
    }

    /// A cover arriving grows from its own centre, and one leaving shrinks to it.
    #[test]
    fn a_cover_coming_or_going_resizes_in_place() {
        let cover = Rect::from_min_size(pos2(100.0, 100.0), vec2(400.0, 400.0));
        let with = Layout {
            cover: Some(cover),
            info: None,
            region: None,
            centred: true,
        };
        let without = Layout {
            cover: None,
            ..with
        };
        let half = Layout::lerp(&without, &with, 0.5).cover.unwrap();
        assert_eq!(half.center(), cover.center());
        assert!((half.width() - 200.0).abs() < 0.01);
        let going = Layout::lerp(&with, &without, 0.75).cover.unwrap();
        assert!((going.width() - 100.0).abs() < 0.01);
    }

    /// On a vertical screen the cover takes less of the height over the queue than over the
    /// lyrics, centred the same, so taking turns resizes it in place.
    #[test]
    fn over_the_queue_a_vertical_cover_is_smaller() {
        let screen = screen(1080.0, 1920.0);
        let m = Metrics::new(screen);
        let lyrics = stacked(screen, &m, false, LYRICS_COVER_SHARE)
            .cover
            .unwrap();
        let queue = stacked(screen, &m, false, QUEUE_COVER_SHARE).cover.unwrap();
        assert!(queue.height() < lyrics.height());
        assert!((queue.center().x - lyrics.center().x).abs() < 0.5);
        assert_eq!(queue.top(), lyrics.top());
    }

    /// The view opens in the window and leaves it alone, fullscreen or not.
    #[test]
    fn entering_leaves_the_window_alone() {
        for was_fullscreen in [false, true] {
            let fs = State::entering(true, false, false, was_fullscreen);
            assert!(fs.open && fs.lyrics && !fs.manages_window);
            assert_eq!(fs.window_was_fullscreen, was_fullscreen);
        }
    }

    /// The view opens as it was left: here with lyrics and queue both on and the lyrics in
    /// front, which is how it first opens.
    #[test]
    fn opening_shows_both_and_lyrics_first_where_one_fits() {
        let mut fs = State::entering(true, true, false, false);
        assert_eq!(fs.shown(true), (true, true));
        fs.exclusive = true;
        assert_eq!(fs.shown(true), (true, false));
        assert_eq!(fs.shown(false), (false, true));
    }

    /// With the queue turned on last, it is the one in front again where only one fits.
    #[test]
    fn reopening_keeps_the_queue_in_front() {
        let mut fs = State::entering(true, true, true, false);
        fs.exclusive = true;
        assert_eq!(fs.shown(true), (false, true));
    }

    /// A view F11 opened fullscreen stays one F11 closes only while it is fullscreen: left
    /// another way, it is the view in the window, and F11 is its fullscreen switch again.
    #[test]
    fn leaving_fullscreen_forgets_the_key_opened_it() {
        let mut fs = State::entering(true, true, false, false);
        fs.window_toggled(true);
        fs.from_key = true;
        fs.window_toggled(false);
        assert!(fs.open && !fs.from_key);
    }

    /// F11 inside the view: going fullscreen hands the view the leaving, even when the window
    /// was fullscreen before it opened; coming out leaves it nothing to undo.
    #[test]
    fn toggling_the_window_moves_who_leaves_fullscreen() {
        let mut fs = State::entering(true, false, false, true);
        fs.window_toggled(true);
        assert!(fs.open && fs.manages_window && !fs.window_was_fullscreen);
        assert!(fs.grace_until > Instant::now());
        fs.window_toggled(false);
        assert!(fs.open && !fs.manages_window && !fs.window_was_fullscreen);
    }

    fn screen(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h))
    }

    fn lay(w: f32, h: f32, region: bool, wide: bool) -> Layout {
        layout(screen(w, h), &Metrics::new(screen(w, h)), region, wide)
    }

    #[test]
    fn metrics_scale_with_the_shorter_side_within_bounds() {
        assert_eq!(Metrics::new(screen(1920.0, 1080.0)).scale, 1.0);
        assert_eq!(
            Metrics::new(screen(1080.0, 1920.0)).scale,
            1.0,
            "vertical, same short side"
        );
        assert_eq!(Metrics::new(screen(760.0, 480.0)).scale, 0.6, "floor");
        assert_eq!(Metrics::new(screen(5000.0, 3000.0)).scale, 1.4, "ceiling");
        assert!(Metrics::new(screen(1080.0, 1920.0)).portrait);
        assert!(!Metrics::new(screen(1920.0, 1080.0)).portrait);
        let small = Metrics::new(screen(800.0, 600.0));
        assert!(small.lyrics < 32.0 && small.margin < 56.0);
    }

    /// With lyrics, the cover and the words sit side by side and never overlap; without, the
    /// cover is centred and there is no region.
    #[test]
    fn the_cover_and_the_lyrics_share_the_screen() {
        let wide = lay(1920.0, 1080.0, true, false);
        let (cover, region) = (wide.cover.unwrap(), wide.region.unwrap());
        assert!(cover.right() < region.left());
        assert!(cover.width() <= 560.0 && (cover.width() - cover.height()).abs() < 0.01);
        assert!(!wide.centred);
        // The block is centred: as much room to the left of the cover as to the right of the
        // words.
        assert!((cover.left() - (1920.0 - region.right())).abs() < 0.01);

        let alone = lay(1920.0, 1080.0, false, false);
        assert!(alone.region.is_none());
        assert!((alone.cover.unwrap().center().x - 960.0).abs() < 0.01);
        assert!(alone.centred);
    }

    /// A narrow landscape window gives the lyrics everything rather than squeezing a cover in.
    #[test]
    fn a_narrow_window_drops_the_cover_for_the_lyrics() {
        let narrow = lay(320.0, 300.0, true, false);
        assert!(narrow.cover.is_none() && narrow.info.is_none());
        assert!(narrow.region.is_some());
    }

    /// A vertical screen stacks: the cover over its words over the lyrics, everything inside the
    /// margins and nothing overlapping.
    #[test]
    fn a_vertical_screen_stacks_the_cover_over_the_lyrics() {
        for (w, h) in [(1080.0, 1920.0), (800.0, 1400.0)] {
            let m = Metrics::new(screen(w, h));
            let l = lay(w, h, true, false);
            let (cover, info, region) = (l.cover.unwrap(), l.info.unwrap(), l.region.unwrap());
            assert!(cover.bottom() <= info.top(), "{w}x{h}");
            assert!(info.bottom() <= region.top(), "{w}x{h}");
            assert!(cover.left() >= m.margin - 0.01 && cover.right() <= w - m.margin + 0.01);
            assert!(region.left() >= m.margin - 0.01 && region.right() <= w - m.margin + 0.01);
            assert!(region.bottom() <= h - m.bottom_band + 0.01);
            assert!(region.height() > 300.0, "{w}x{h}: room for lyrics");
            assert!(l.centred);
        }
    }

    /// A vertical window too short to hold both gives the words the whole space.
    #[test]
    fn a_short_vertical_window_lets_the_cover_step_aside() {
        let l = lay(300.0, 320.0, true, false);
        assert!(l.cover.is_none() && l.region.is_some());
    }

    /// With no region on a vertical screen the cover is as wide as the screen allows.
    #[test]
    fn a_vertical_cover_alone_fills_the_width() {
        let m = Metrics::new(screen(1080.0, 1920.0));
        let l = lay(1080.0, 1920.0, false, false);
        let cover = l.cover.unwrap();
        assert!((cover.width() - (1080.0 - 2.0 * m.margin)).abs() < 0.01);
    }

    /// A short window keeps the cover on screen and above its own text.
    #[test]
    fn a_short_window_keeps_the_cover_clear_of_the_controls() {
        let m = Metrics::new(screen(1280.0, 520.0));
        let l = lay(1280.0, 520.0, false, false);
        assert!(l.info.unwrap().bottom() <= 520.0 - m.bottom_band + 1.0);
    }

    /// A music video keeps its own shape: 16:9, within the column, with the credits under it.
    #[test]
    fn a_video_is_sixteen_by_nine() {
        for (w, h, region) in [
            (1920.0, 1080.0, true),
            (1920.0, 1080.0, false),
            (1080.0, 1920.0, true),
            (1080.0, 1920.0, false),
        ] {
            let l = lay(w, h, region, true);
            let cover = l.cover.unwrap();
            assert!(
                (cover.width() / cover.height() - 16.0 / 9.0).abs() < 0.01,
                "{w}x{h} {region}: {cover:?}"
            );
            assert!(l.info.unwrap().top() >= cover.bottom());
        }
    }

    #[test]
    fn the_layout_glides_between_its_two_states() {
        let a = lay(1920.0, 1080.0, false, false);
        let b = lay(1920.0, 1080.0, true, false);
        let at = |t| Layout::lerp(&a, &b, t).cover.unwrap();
        assert_eq!(at(0.0), a.cover.unwrap());
        assert_eq!(at(1.0), b.cover.unwrap());
        let mid = at(0.5);
        assert!(mid.left() < a.cover.unwrap().left() && mid.left() > b.cover.unwrap().left());
    }

    #[test]
    fn lyrics_are_missing_only_once_the_request_has_settled_empty() {
        assert!(!lyrics_missing(false, false, None), "not asked yet");
        assert!(!lyrics_missing(true, true, None), "still loading");
        assert!(
            lyrics_missing(true, false, None),
            "asked, nothing came back"
        );
        assert!(
            lyrics_missing(true, false, Some(0)),
            "an empty list is none"
        );
        assert!(!lyrics_missing(true, false, Some(12)));
    }

    #[test]
    fn the_controls_hide_only_when_idle_playing_and_unhovered() {
        let idle = HIDE_AFTER + Duration::from_millis(1);
        assert!(controls_visible(Duration::ZERO, false, false));
        assert!(!controls_visible(idle, false, false));
        assert!(controls_visible(idle, true, false), "paused keeps them up");
        assert!(controls_visible(idle, false, true), "hover keeps them up");
    }

    #[test]
    fn the_glide_closes_the_gap_without_overshooting() {
        let mut offset = 0.0;
        for _ in 0..600 {
            let next = ease(offset, 100.0, 1.0 / 60.0);
            assert!(next >= offset && next <= 100.0);
            offset = next;
        }
        assert_eq!(offset, 100.0, "it lands exactly");
        assert!(
            ease(0.0, 100.0, 1.0 / 60.0) < 50.0,
            "one frame is a fraction of the way"
        );
    }

    #[test]
    fn the_active_line_is_brightest_and_the_rest_fall_away() {
        assert_eq!(line_alpha(3, Some(3)), 1.0);
        assert!(
            line_alpha(2, Some(3)) < line_alpha(4, Some(3)),
            "next is brighter than last"
        );
        assert!(line_alpha(0, Some(30)) >= 0.18, "never fully gone");
        assert!(line_alpha(40, Some(3)) >= 0.22);
        assert_eq!(line_alpha(0, None), line_alpha(5, None));
    }
}
