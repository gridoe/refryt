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

use crate::app::{App, RepeatMode};
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
    pub drawer_w: f32,
    pub drawer_gap: f32,
    /// Under this width the lyrics take the whole screen and the cover steps aside.
    pub narrow: f32,
}

impl Metrics {
    pub fn new(screen: Rect) -> Self {
        let shorter = screen.width().min(screen.height());
        let s = (shorter / REFERENCE).clamp(0.6, 1.4);
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
            lyrics_max_w: 820.0 * s,
            column_gap: 80.0 * s,
            icon: 18.0 * s,
            play: 60.0 * s,
            controls_w: 760.0 * s,
            drawer_w: (screen.width() * 0.24).clamp(340.0 * s, 460.0 * s),
            drawer_gap: 48.0 * s,
            narrow: 640.0 * s,
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
            last_motion: now,
            manages_window: false,
            window_was_fullscreen: false,
            grace_until: now,
            scroll: 0.0,
            manual: 0.0,
            manual_at: now,
            scroll_key: None,
            wrap_width: 0.0,
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

    /// Open for real: the window was taken fullscreen for it (or already was), and `lyrics` says
    /// whether the lyrics column starts showing.
    pub fn entering(lyrics: bool, window_was_fullscreen: bool) -> Self {
        Self {
            open: true,
            lyrics,
            manages_window: true,
            window_was_fullscreen,
            grace_until: Instant::now() + Duration::from_millis(2000),
            ..Self::default()
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
        Layout {
            cover: rect(a.cover, b.cover),
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

/// `with_region` says whether there is a lyrics region (or queue sheet) to make room for; without
/// one the cover stands alone, centred. `reserve` is room kept free on the right, for the queue
/// drawer on a landscape screen. The cover and the region are one block centred in what remains,
/// so the margins on both sides match.
pub(crate) fn layout(
    screen: Rect,
    m: &Metrics,
    with_region: bool,
    reserve: f32,
    wide: bool,
) -> Layout {
    let content = Rect::from_min_max(
        pos2(screen.left() + m.margin, screen.top() + m.top_band),
        pos2(
            screen.right() - m.margin - reserve,
            screen.bottom() - m.bottom_band,
        ),
    );
    let gap = 20.0 * m.scale;
    let height_for_cover = (content.height() - m.info_h - gap).max(120.0 * m.scale);

    if with_region && m.portrait {
        // Stacked: the cover over its words, then the region for the lyrics.
        let (cw, ch) = cover_size(content.width(), content.height() * 0.38, wide);
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
        return Layout {
            cover: Some(cover),
            info: Some(info),
            region: Some(region),
            centred: true,
        };
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
        let column = (content.width() * 0.40).clamp(280.0 * m.scale, 560.0 * m.scale);
        let words = (content.width() - column - m.column_gap).clamp(0.0, m.lyrics_max_w);
        let left = content.left() + (content.width() - (column + m.column_gap + words)) / 2.0;
        let (cw, ch) = cover_size(column, height_for_cover, wide);
        let top = content.center().y - (ch + gap + m.info_h) / 2.0;
        let cover = Rect::from_min_size(pos2(left, top), vec2(cw, ch));
        let info = Rect::from_min_size(pos2(left, cover.bottom() + gap), vec2(column, m.info_h));
        let region = Rect::from_min_max(
            pos2(left + column + m.column_gap, content.top()),
            pos2(left + column + m.column_gap + words, content.bottom()),
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
    let ctx = ui.ctx().clone();
    let p = app.palette;
    let screen = test_screen().unwrap_or_else(|| ctx.content_rect());
    let m = Metrics::new(screen);
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

    let Some(track) = app.now.track.as_ref() else {
        background(ui, &p, screen, p.accent);
        return;
    };
    let art_url = track.thumbnail.clone();
    let title = track.title.clone();
    let credits = track.artist_line();
    let video_id = track.video_id.clone();
    let video = app.video_open && track.wide;

    let tint = animated(&ctx, crate::ui::header_wash_colour(app, art_url.as_ref()));
    background(ui, &p, screen, tint);

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
    let lyrics_t =
        ctx.animate_bool_with_time(Id::new("fs-lyrics-t"), app.fs.lyrics && !missing, 0.35);
    let queue_t = ctx.animate_bool_with_time(Id::new("fs-drawer"), app.fs.queue, 0.28);
    // On a vertical screen the queue is a sheet in the same region as the lyrics, so the region
    // has to exist for either.
    let region_t = if m.portrait {
        lyrics_t.max(queue_t)
    } else {
        lyrics_t
    };
    let reserve = if m.portrait {
        0.0
    } else {
        queue_t * (m.drawer_w + m.drawer_gap)
    };
    let alone = layout(screen, &m, false, reserve, video);
    let with = layout(screen, &m, true, reserve, video);
    let lay = Layout::lerp(&alone, &with, region_t);
    let centre_t = {
        let a = f32::from(u8::from(alone.centred));
        let b = f32::from(u8::from(with.centred));
        a + (b - a) * region_t
    };

    if let Some(cover) = lay.cover {
        if video {
            // A music video plays where the cover would, at its own 16:9.
            // The cover's own curve, so a video and a cover in the same place round alike.
            let radius = crate::widgets::ArtShape::Wide.radius(cover);
            ui.painter().rect_filled(cover, radius, Color32::BLACK);
            crate::ui::video::paint_frame(app, ui, cover, radius);
            if ui
                .interact(cover, Id::new("fs-video"), Sense::click())
                .clicked()
            {
                app.toggle_playback();
            }
        } else {
            // The cover at this size may not be fetched yet (the bar only ever asked for its small
            // one): until it lands, the small one stretched stands in, rather than an empty disc.
            let large = crate::ui::art_texture(
                &mut app.images,
                &app.backend,
                &ctx,
                art_url.as_ref(),
                cover.width(),
            )
            .cloned();
            let texture = large.or_else(|| {
                crate::ui::art_texture(&mut app.images, &app.backend, &ctx, art_url.as_ref(), 56.0)
                    .cloned()
            });
            widgets::paint_art(ui, &p, cover, texture.as_ref(), widgets::ArtShape::Square);
        }
    }
    if let Some(info) = lay.info {
        info_text(ui, &p, &m, info, &title, &credits, centre_t);
    }

    // Where the queue drawer sits. The block (cover and lyrics) and the drawer are one group
    // centred on the screen, so the drawer's margin on the right is the cover's on the left:
    // laid out with the drawer's room set aside, whichever way the animation is going. It slides
    // in by its whole width, so its left edge is never over the words it is making room beside.
    let drawer = (!m.portrait).then(|| {
        let room = m.drawer_w + m.drawer_gap;
        let open = Layout::lerp(
            &layout(screen, &m, false, room, video),
            &layout(screen, &m, true, room, video),
            region_t,
        );
        drawer_placement(screen, &m, &open, queue_t)
    });

    // The lyrics, fading in over their region; on a vertical screen they fade out as the queue
    // sheet takes the same place.
    if let Some(region) = with.region {
        let alpha = lyrics_t * if m.portrait { 1.0 - queue_t } else { 1.0 };
        if alpha > 0.01 {
            // Wrapped to the width they will settle at, not the one the drawer is animating
            // through: a width that changes every frame re-flows the lines every frame, and the
            // whole column shimmers and jumps. The position still glides; only the wrap is held.
            let settle = if m.portrait || !app.fs.queue {
                0.0
            } else {
                m.drawer_w + m.drawer_gap
            };
            let wrapped = layout(screen, &m, true, settle, video)
                .region
                .map_or(region.width(), |r| r.width());
            let region = Rect::from_min_size(region.min, vec2(wrapped, region.height()));
            // While the drawer is on its way out the words are wider than the room it has
            // left; they are revealed as it goes rather than drawn over it.
            let visible_right = drawer
                .filter(|_| queue_t > 0.001)
                .map_or(f32::INFINITY, |(rect, offset)| {
                    rect.left() + offset.x - m.drawer_gap * 0.5
                });
            let mut layer = ui.new_child(UiBuilder::new().max_rect(region));
            layer.multiply_opacity(alpha);
            lyrics_column(app, &mut layer, &m, region, visible_right, &video_id);
        }
    }

    if queue_t > 0.001 {
        let (rect, offset) = match drawer {
            Some(placed) => placed,
            None => (
                with.region.unwrap_or(screen),
                vec2(0.0, (1.0 - queue_t) * 40.0 * m.scale),
            ),
        };
        queue_region(app, ui, &m, rect.translate(offset), queue_t);
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
    let hovering = ctx
        .pointer_hover_pos()
        .is_some_and(|pos| bottom.contains(pos) || top.contains(pos));
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
    controls(app, &mut layer, &m, bottom, top, missing);
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
    let strength = if p.dark { (0.52, 0.08) } else { (0.34, 0.06) };
    (
        p.window
            .lerp_to_gamma(tint, strength.0)
            .gamma_multiply(p.gutter_alpha),
        p.window
            .lerp_to_gamma(tint, strength.1)
            .gamma_multiply(p.gutter_alpha),
    )
}

/// A base with the cover's tint washing down from the top. As opaque as the shell is: solid by
/// default, and see-through (and blurred, where the compositor does that) when the window is.
fn background(ui: &egui::Ui, p: &theme::Palette, screen: Rect, tint: Color32) {
    let (top, bottom) = wash(p, tint);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(screen.left_top(), top);
    mesh.colored_vertex(screen.right_top(), top);
    mesh.colored_vertex(screen.right_bottom(), bottom);
    mesh.colored_vertex(screen.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// The tint eased toward its target, so a song change washes into the next colour instead of
/// switching.
fn animated(ctx: &egui::Context, tint: Color32) -> Color32 {
    let channel = |i: usize, v: u8| {
        ctx.animate_value_with_time(Id::new(("fs-tint", i)), f32::from(v), 0.9)
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
fn info_text(
    ui: &egui::Ui,
    p: &theme::Palette,
    m: &Metrics,
    rect: Rect,
    title: &str,
    credits: &str,
    centre_t: f32,
) {
    let place = |text: &str, font: &egui::FontId| {
        let text = widgets::truncate_to(ui, text, font.clone(), rect.width()).into_owned();
        let w = widgets::text_width(ui, &text, font);
        let x = rect.left() + (rect.width() - w) / 2.0 * centre_t;
        (text, x)
    };
    let title_font = theme::bold(m.title);
    let (text, x) = place(title, &title_font);
    ui.painter().text(
        pos2(x, rect.top()),
        egui::Align2::LEFT_TOP,
        text,
        title_font,
        p.text,
    );
    let credits_font = theme::regular(m.credits);
    let (text, x) = place(credits, &credits_font);
    ui.painter().text(
        pos2(x, rect.top() + m.title * 1.4),
        egui::Align2::LEFT_TOP,
        text,
        credits_font,
        p.secondary,
    );
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
/// The lyrics in `rect`, wrapped to its width. `visible_right` is as far right as they may be
/// seen, which is less than `rect`'s edge only while the queue drawer is still moving over it.
fn lyrics_column(
    app: &mut App,
    ui: &mut egui::Ui,
    m: &Metrics,
    rect: Rect,
    visible_right: f32,
    video_id: &str,
) {
    let p = app.palette;
    let ctx = ui.ctx().clone();
    let visible = Rect::from_min_max(
        rect.min,
        pos2(
            rect.right().min(visible_right).max(rect.left()),
            rect.bottom(),
        ),
    );
    if app.lyrics_loading {
        lyrics_skeleton(ui, &p, m, rect);
        return;
    }
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
        let output = egui::ScrollArea::vertical()
            .id_salt("fs-plain-lyrics")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                for line in &lyrics.lines {
                    let text = if line.text.is_empty() {
                        "♪"
                    } else {
                        &line.text
                    };
                    ui.label(
                        egui::RichText::new(text)
                            .font(theme::semibold(m.lyrics * 0.75))
                            .color(p.text.gamma_multiply(0.85)),
                    );
                    ui.add_space(m.line_gap * 0.55);
                }
            });
        // The same band the timed lyrics melt over.
        let fade = LYRICS_FADE * m.scale;
        let (above, below) = edge_fades(
            (
                output.state.offset.y,
                output.content_size.y,
                output.inner_rect.height(),
            ),
            fade,
        );
        fade_painted(
            &ctx,
            child.layer_id(),
            from,
            output.inner_rect,
            EdgeFade::eased(fade, above),
            EdgeFade::eased(fade, below),
        );
        return;
    }

    // Where the active line's middle is, and the glide toward it.
    // Before the first line is sung nothing is active: the words wait near the top of their
    // region rather than leaving it empty above, then glide down to the anchor.
    let first = galleys[0].size().y / 2.0;
    let target = active
        .map(|a| tops[a] + galleys[a].size().y / 2.0)
        .unwrap_or(first + rect.height() * (ANCHOR - 0.12));
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
    if ctx
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

    let anchor = rect.top() + rect.height() * ANCHOR;
    let view = app.fs.scroll + app.fs.manual;
    let painter = ui.painter().with_clip_rect(visible);
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
            .intersect(visible);
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
    let fade = 36.0 * m.scale;
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

/// The seek bar and transport along the bottom, the toggles in the top-right corner.
/// Where the queue drawer sits on a landscape screen, and how far it is still to slide.
///
/// `open` is the layout with the drawer's room set aside. The block it holds and the drawer are
/// one group centred on the screen, so the drawer's margin on the right is the cover's on the
/// left, whatever the block's width. `offset` carries the whole drawer off the right edge at
/// `queue_t` 0 and nowhere at 1, so it moves as a unit rather than growing out of its place.
pub(crate) fn drawer_placement(
    screen: Rect,
    m: &Metrics,
    open: &Layout,
    queue_t: f32,
) -> (Rect, egui::Vec2) {
    let left_margin = open
        .cover
        .map(|c| c.left())
        .or_else(|| open.region.map(|r| r.left()))
        .unwrap_or(screen.left() + m.margin)
        - screen.left();
    let right = screen.right() - left_margin;
    let rect = Rect::from_min_max(
        pos2(right - m.drawer_w, screen.top() + m.top_band),
        pos2(right, screen.bottom() - m.bottom_band),
    );
    (
        rect,
        vec2((1.0 - queue_t) * (m.drawer_w + left_margin), 0.0),
    )
}

fn controls(app: &mut App, ui: &mut egui::Ui, m: &Metrics, bottom: Rect, top: Rect, missing: bool) {
    let p = app.palette;
    let s = m.scale;
    let column = Rect::from_center_size(
        bottom.center(),
        vec2(bottom.width().min(m.controls_w), bottom.height()),
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
        vec2((column.width() - 120.0 * s).max(60.0), 4.0),
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

    // Toggles, top-right: Exit sits in the corner, Queue and Lyrics to its left.
    let toggles = Rect::from_min_max(
        pos2(top.right() - m.margin - 160.0 * s, top.top() + 18.0 * s),
        pos2(
            top.right() - m.margin + 8.0,
            top.top() + 18.0 * s + m.icon + 14.0,
        ),
    );
    let mut tui = ui.new_child(
        UiBuilder::new()
            .max_rect(toggles)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    if widgets::icon_button(&mut tui, &p, Icon::Minimize2, m.icon, false)
        .hint(crate::ui::tips::EXIT_FULLSCREEN)
        .clicked()
    {
        app.fs_exit(ui.ctx());
    }
    if widgets::icon_button(&mut tui, &p, Icon::ListMusic, m.icon, app.fs.queue)
        .hint(crate::ui::tips::QUEUE)
        .clicked()
    {
        app.fs.queue = !app.fs.queue;
    }
    // A song with no lyrics greys the button out and says so; the saved choice is untouched.
    let lyrics_button = tui.add_enabled_ui(!missing, |ui| {
        widgets::icon_button(ui, &p, Icon::MicVocal, m.icon, app.fs.lyrics && !missing)
            .hint(crate::ui::tips::LYRICS)
    });
    if lyrics_button.inner.clicked() {
        app.fs.lyrics = !app.fs.lyrics;
        let on = app.fs.lyrics;
        let ctx = ui.ctx().clone();
        app.update_settings(&ctx, |s| s.fullscreen_lyrics = on);
    }
    if missing {
        lyrics_button
            .response
            .on_disabled_hover_text("No lyrics for this song");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h))
    }

    fn lay(w: f32, h: f32, region: bool, reserve: f32, wide: bool) -> Layout {
        layout(
            screen(w, h),
            &Metrics::new(screen(w, h)),
            region,
            reserve,
            wide,
        )
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
        let wide = lay(1920.0, 1080.0, true, 0.0, false);
        let (cover, region) = (wide.cover.unwrap(), wide.region.unwrap());
        assert!(cover.right() < region.left());
        assert!(cover.width() <= 560.0 && (cover.width() - cover.height()).abs() < 0.01);
        assert!(!wide.centred);
        // The block is centred: as much room to the left of the cover as to the right of the
        // words.
        assert!((cover.left() - (1920.0 - region.right())).abs() < 0.01);

        let alone = lay(1920.0, 1080.0, false, 0.0, false);
        assert!(alone.region.is_none());
        assert!((alone.cover.unwrap().center().x - 960.0).abs() < 0.01);
        assert!(alone.centred);
    }

    /// A narrow landscape window gives the lyrics everything rather than squeezing a cover in.
    #[test]
    fn a_narrow_window_drops_the_cover_for_the_lyrics() {
        let narrow = lay(320.0, 300.0, true, 0.0, false);
        assert!(narrow.cover.is_none() && narrow.info.is_none());
        assert!(narrow.region.is_some());
    }

    /// A vertical screen stacks: the cover over its words over the lyrics, everything inside the
    /// margins and nothing overlapping.
    #[test]
    fn a_vertical_screen_stacks_the_cover_over_the_lyrics() {
        for (w, h) in [(1080.0, 1920.0), (800.0, 1400.0)] {
            let m = Metrics::new(screen(w, h));
            let l = lay(w, h, true, 0.0, false);
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

    /// The block and the open drawer are one group centred on the screen: the drawer's margin on
    /// the right is the block's on the left, with the room the layout set aside between them,
    /// with or without lyrics, and the drawer starts fully off screen.
    #[test]
    fn the_open_drawer_is_symmetrical_with_the_block() {
        for (w, h) in [
            (1920.0, 1080.0),
            (2560.0, 1440.0),
            (1366.0, 768.0),
            (1100.0, 720.0),
        ] {
            let screen = screen(w, h);
            let m = Metrics::new(screen);
            let room = m.drawer_w + m.drawer_gap;
            for with_region in [true, false] {
                let open = layout(screen, &m, with_region, room, false);
                let (rect, offset) = drawer_placement(screen, &m, &open, 1.0);
                assert_eq!(offset, egui::Vec2::ZERO);
                let block_left = open
                    .cover
                    .map(|c| c.left())
                    .or_else(|| open.region.map(|r| r.left()))
                    .unwrap();
                assert!(
                    ((w - rect.right()) - (block_left - screen.left())).abs() < 0.5,
                    "{w}x{h} region {with_region}: margins differ"
                );
                let block_right = open
                    .region
                    .map(|r| r.right())
                    .or_else(|| open.cover.map(|c| c.right()))
                    .unwrap();
                assert!(
                    rect.left() >= block_right + m.drawer_gap - 0.5,
                    "{w}x{h} region {with_region}: the drawer crowds the block"
                );
                let (rect, offset) = drawer_placement(screen, &m, &open, 0.0);
                assert!(
                    rect.left() + offset.x >= screen.right() - 0.5,
                    "starts on screen"
                );
            }
        }
    }

    /// A vertical window too short to hold both gives the words the whole space.
    #[test]
    fn a_short_vertical_window_lets_the_cover_step_aside() {
        let l = lay(300.0, 320.0, true, 0.0, false);
        assert!(l.cover.is_none() && l.region.is_some());
    }

    /// With no region on a vertical screen the cover is as wide as the screen allows.
    #[test]
    fn a_vertical_cover_alone_fills_the_width() {
        let m = Metrics::new(screen(1080.0, 1920.0));
        let l = lay(1080.0, 1920.0, false, 0.0, false);
        let cover = l.cover.unwrap();
        assert!((cover.width() - (1080.0 - 2.0 * m.margin)).abs() < 0.01);
    }

    /// A short window keeps the cover on screen and above its own text.
    #[test]
    fn a_short_window_keeps_the_cover_clear_of_the_controls() {
        let m = Metrics::new(screen(1280.0, 520.0));
        let l = lay(1280.0, 520.0, false, 0.0, false);
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
            let l = lay(w, h, region, 0.0, true);
            let cover = l.cover.unwrap();
            assert!(
                (cover.width() / cover.height() - 16.0 / 9.0).abs() < 0.01,
                "{w}x{h} {region}: {cover:?}"
            );
            assert!(l.info.unwrap().top() >= cover.bottom());
        }
    }

    /// The drawer's reserved room squeezes the block from the right and keeps it centred in
    /// what is left.
    #[test]
    fn an_open_drawer_squeezes_the_block_and_recentres_it() {
        let m = Metrics::new(screen(1920.0, 1080.0));
        let reserve = m.drawer_w + m.drawer_gap;
        let l = lay(1920.0, 1080.0, true, reserve, false);
        let (cover, region) = (l.cover.unwrap(), l.region.unwrap());
        assert!(region.right() <= 1920.0 - m.margin - reserve + 0.01);
        assert!(
            (cover.left() - m.margin - (1920.0 - m.margin - reserve - region.right())).abs() < 0.01
        );
    }

    #[test]
    fn the_layout_glides_between_its_two_states() {
        let a = lay(1920.0, 1080.0, false, 0.0, false);
        let b = lay(1920.0, 1080.0, true, 0.0, false);
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
