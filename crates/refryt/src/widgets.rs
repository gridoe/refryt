//! Shared widgets, all drawn by hand.
//!
//! Hover and press are *felt*, not just shown: every control fades its fill over
//! [`theme::HOVER_FADE_SECS`] and sinks a pixel while it is held down. See [`lit`] for why that
//! does not cost an idle app any frames.

#![allow(dead_code)] // design system built ahead of the pages that consume it

use egui::{Color32, CornerRadius, Rect, Sense, Ui, vec2};

use crate::icons::Icon;
use crate::theme::{self, Palette};

/// How lit a control is: `0.0` at rest, `1.0` hovered, moving between them over
/// [`theme::HOVER_FADE_SECS`].
///
/// `animate_bool_with_time` asks for a repaint only while the value is actually travelling — a
/// handful of frames after the pointer arrives or leaves — so an app nobody is touching still
/// sits at zero. That is the whole basis on which the speed budget's "no animation" rule was
/// relaxed, and the idle phase of `scripts/perf.sh` is what holds it.
pub fn lit(ui: &Ui, response: &egui::Response) -> f32 {
    fade(ui, response.id.with("lit"), response.hovered())
}

/// [`lit`] for a condition of your own — a row lights on `contains_pointer` rather than
/// `hovered`, because the links and thumbs stacked on top of it take the hover flag and the
/// highlight would blink out as the pointer crossed them.
pub fn fade(ui: &Ui, id: egui::Id, on: bool) -> f32 {
    ui.ctx()
        .animate_bool_with_time(id, on, theme::HOVER_FADE_SECS)
}

/// Whether the pointer is being held down on this control. Instant, not faded: a press should
/// answer immediately or it reads as lag.
pub fn held(response: &egui::Response) -> bool {
    response.is_pointer_button_down_on()
}

/// The pixel a held control sinks by, so pressing something feels like pressing something.
pub fn press_offset(response: &egui::Response) -> egui::Vec2 {
    if held(response) {
        vec2(0.0, 1.0)
    } else {
        egui::Vec2::ZERO
    }
}

/// Blend `from` toward `to` by `t`.
pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(
        lerp(from.r(), to.r()),
        lerp(from.g(), to.g()),
        lerp(from.b(), to.b()),
    )
}

/// The background a control shows for its current state, faded in.
///
/// `None` at rest, so a control at rest paints nothing at all rather than a transparent rect.
fn state_fill(ui: &Ui, palette: &Palette, response: &egui::Response) -> Option<Color32> {
    if held(response) {
        return Some(palette.surface_active);
    }
    let t = lit(ui, response);
    (t > 0.001).then(|| palette.surface_hover.gamma_multiply(t))
}

/// [`state_fill`] for a control that sits on the shell itself — a nav entry, an icon button —
/// where the fill must stay see-through when the window is. Popups keep the solid one.
fn shell_state_fill(ui: &Ui, palette: &Palette, response: &egui::Response) -> Option<Color32> {
    if held(response) {
        return Some(palette.glass(palette.surface_active));
    }
    let t = lit(ui, response);
    (t > 0.001).then(|| palette.glass_state(palette.surface_hover, t))
}

/// The id the double-click-to-play flag is kept under in egui's memory.
const DOUBLE_CLICK_KEY: &str = "ytm-double-click-play";

/// The id the selected row/card's video id is kept under. Purely visual: a single click in
/// double-click mode marks the row, and nothing else reads it.
fn selection_key() -> egui::Id {
    egui::Id::new("ytm-selection")
}

/// Publish the Settings toggle to egui's memory once a frame, so the row and card widgets can
/// read it without every caller threading another bool through.
pub(crate) fn set_double_click_play(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(DOUBLE_CLICK_KEY), on));
}

fn double_click_play(ctx: &egui::Context) -> bool {
    ctx.data(|d| {
        d.get_temp::<bool>(egui::Id::new(DOUBLE_CLICK_KEY))
            .unwrap_or(false)
    })
}

fn selected(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<Option<String>>(selection_key()).flatten())
}

pub(crate) fn set_selected(ctx: &egui::Context, id: Option<String>) {
    ctx.data_mut(|d| d.insert_temp(selection_key(), id));
}

/// Whether a row/card body click should play `id`, applying the double-click-to-play setting.
///
/// Off: any primary click plays, exactly as before. On: a single click just selects (marks the
/// row, writes the selection) and a double click plays — so a stray click cannot start music.
/// egui fires `clicked()` on the *first* click of a double too, which is why the double is
/// tested before the single, and why callers must use this rather than `response.clicked()`.
pub(crate) fn activate(ctx: &egui::Context, response: &egui::Response, id: &str) -> bool {
    if !double_click_play(ctx) {
        return response.clicked();
    }
    if response.double_clicked() {
        return true;
    }
    if response.clicked() {
        set_selected(ctx, Some(id.to_owned()));
    }
    false
}

/// Whether a click landed on a row's cover — the hover play button's own control.
///
/// The overlay drawn on a row's artwork is a button in its own right, not decoration: pressing it
/// plays on a single click whatever the double-click-to-play setting says, the same way a card's
/// own button hit region already does. Only square art ever draws one (a 16:9 still has none), so
/// only square art is a button. `pos` is the click's own position, which `clicked` alone cannot
/// tell apart from a click on the row's title or its rating thumbs.
fn on_cover(shape: ArtShape, art: Rect, pos: Option<egui::Pos2>, clicked: bool) -> bool {
    clicked && shape == ArtShape::Square && pos.is_some_and(|p| art.contains(p))
}

/// Whether `id` is the row/card the listener last single-clicked (double-click-to-play mode).
pub(crate) fn is_selected(ctx: &egui::Context, id: &str) -> bool {
    double_click_play(ctx) && selected(ctx).as_deref() == Some(id)
}

/// The set of downloaded video ids, published once a frame by `App::ui` so the row menu can
/// offer Download or Remove download without every caller threading the index through.
/// An `Arc`, because egui's `get_temp` hands back an owned clone: a bare set would be copied
/// whole on every read.
pub(crate) fn set_downloaded(
    ctx: &egui::Context,
    ids: std::sync::Arc<std::collections::HashSet<String>>,
) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ytm-downloaded"), ids));
}

/// Whether the account holds YouTube Premium, published beside [`set_downloaded`] so the menus
/// can offer downloads only where YouTube itself would.
pub(crate) fn set_premium(ctx: &egui::Context, premium: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ytm-premium"), premium));
}

fn premium(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new("ytm-premium")))
        .unwrap_or(false)
}

/// What a disabled Download item says under the pointer.
const DOWNLOAD_NEEDS_PREMIUM: &str = "Downloads need YouTube Premium";

fn is_downloaded(ctx: &egui::Context, id: &str) -> bool {
    ctx.data(|d| {
        d.get_temp::<std::sync::Arc<std::collections::HashSet<String>>>(egui::Id::new(
            "ytm-downloaded",
        ))
        .is_some_and(|ids| ids.contains(id))
    })
}

/// A square icon button that lights up under the pointer and sinks when pressed.
///
/// `active` is for toggles that have a persistent on state — the queue's list button is the
/// first — and paints the accent glyph plus a small dot beneath it, so "on" reads at a glance.
/// The button keeps its resting look, so an open queue does not read as a pressed button.
pub fn icon_button(
    ui: &mut Ui,
    palette: &Palette,
    icon: Icon,
    size: f32,
    active: bool,
) -> egui::Response {
    let box_size = size + 14.0;
    let (rect, response) = ui.allocate_exact_size(vec2(box_size, box_size), Sense::click());

    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        if let Some(fill) = shell_state_fill(ui, palette, &response) {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(theme::radii().control), fill);
        }
        let fg = if active {
            palette.accent
        } else {
            mix(palette.secondary, palette.text, t)
        };
        let centre = rect.center() + press_offset(&response);
        icon.image(fg, size)
            .paint_at(ui, Rect::from_center_size(centre, vec2(size, size)));
        if active {
            // Inside the box, clear of the glyph: an indicator, not a layout citizen, so the
            // row's spacing does not move when it appears.
            ui.painter().circle_filled(
                egui::pos2(rect.center().x, rect.center().y + size / 2.0 + 3.0),
                2.0,
                palette.accent,
            );
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// The round accent play button in the player bar.
pub fn play_button(ui: &mut Ui, palette: &Palette, playing: bool, diameter: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, diameter), Sense::click());

    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        let fill = mix(palette.accent, palette.accent_hover, t);
        // Grows a little under the pointer and squashes when held — the one control worth
        // making feel like a physical button, since it is the one everybody presses.
        let scale = if held(&response) {
            0.94
        } else {
            1.0 + 0.04 * t
        };
        let r = diameter / 2.0 * scale;
        ui.painter().circle_filled(rect.center(), r, fill);

        let glyph = if playing {
            Icon::PauseFilled
        } else {
            Icon::PlayFilled
        };
        let glyph_size = diameter * 0.44 * scale;
        // The play triangle's visual centre sits left of its bounding box, so nudge it right.
        let offset = if playing { 0.0 } else { glyph_size * 0.06 };
        glyph.image(palette.on_accent, glyph_size).paint_at(
            ui,
            Rect::from_center_size(
                rect.center() + vec2(offset, 0.0),
                vec2(glyph_size, glyph_size),
            ),
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// The Play button of a page that is still loading: the real one, with nothing behind it yet, so
/// dimmed and inert. Play is on every one of these pages, so it needs no grey stand-in. Drawn
/// with the other placeholders, at `rect`.
pub fn skeleton_play_button(ui: &Ui, palette: &Palette, rect: Rect) {
    if !skeleton_visible(ui) || !ui.is_rect_visible(rect) {
        return;
    }
    let dim = 0.45;
    ui.painter().circle_filled(
        rect.center(),
        rect.width() / 2.0,
        palette.accent.gamma_multiply(dim),
    );
    let glyph = rect.width() * 0.44;
    Icon::PlayFilled
        .image(palette.on_accent.gamma_multiply(dim + 0.2), glyph)
        .paint_at(
            ui,
            Rect::from_center_size(rect.center() + vec2(glyph * 0.06, 0.0), vec2(glyph, glyph)),
        );
}

/// A tooltip to the right of the pointer, centred on it vertically.
///
/// egui's own tooltip sits below the widget, which in the icon rail is the next icon: the rail is
/// a column of small targets with nothing to its right, so the room is there and the pointer is
/// where the eye already is. The gap clears the cursor's own shape.
pub fn tooltip_right(
    response: egui::Response,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let text = text.into();
    let mut tooltip = egui::Tooltip::for_enabled(&response);
    tooltip.popup = tooltip
        .popup
        .at_pointer()
        .align(egui::RectAlign::RIGHT)
        .gap(TOOLTIP_GAP)
        .frame(hint_frame(&response.ctx.global_style()));
    tooltip.show(|ui| {
        // As egui's own text tooltips: the width fixed, so the area does not shrink round it.
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.add(egui::Label::new(text));
    });
    response
}

/// A hint's own frame: the popup's, with a lighter shadow than a menu's. A hint is a few words
/// held by the pointer, and a menu's lift read heavy on something that small.
fn hint_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::popup(style).shadow(egui::epaint::Shadow {
        offset: [0, 2],
        blur: 8,
        spread: 0,
        color: style.visuals.popup_shadow.color.gamma_multiply(0.5),
    })
}

/// egui's `on_hover_text` in a [`hint_frame`]. Every hover hint in the app goes through this.
pub trait Hint {
    fn hint(self, text: impl Into<egui::WidgetText>) -> Self;
}

impl Hint for egui::Response {
    fn hint(self, text: impl Into<egui::WidgetText>) -> Self {
        let text = text.into();
        let mut tooltip = egui::Tooltip::for_enabled(&self);
        tooltip.popup = tooltip.popup.frame(hint_frame(&self.ctx.global_style()));
        tooltip.show(|ui| {
            // As egui's own: the width fixed, so the area does not shrink round it.
            ui.set_max_width(ui.spacing().tooltip_width);
            ui.add(egui::Label::new(text));
        });
        self
    }
}

/// How far a [`tooltip_right`] sits from the pointer.
const TOOLTIP_GAP: f32 = 14.0;

/// A sidebar navigation entry: Home, Search, Library.
///
/// No pill behind the selected one — selection is carried by weight and colour alone, which is
/// what keeps a rail of three from reading as a row of buttons. Hover still gets a fill, because
/// a target that does not answer the pointer feels dead.
///
/// `compact` is the icon rail: the icon at the pill's centre and the name only as a tooltip.
pub fn nav_item(
    ui: &mut Ui,
    palette: &Palette,
    icon: Icon,
    label: &str,
    selected: bool,
    compact: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::NAV_HEIGHT),
        Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        // The page you are on wears the same pill a selected playlist in the rail below does,
        // so where you are reads at a glance rather than by comparing font weights.
        let fill = if selected {
            Some(palette.glass(palette.surface_active))
        } else {
            shell_state_fill(ui, palette, &response)
        };
        if let Some(fill) = fill {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(theme::radii().control), fill);
        }

        let fg = if selected {
            palette.text
        } else {
            mix(palette.secondary, palette.text, t)
        };
        let nudge = press_offset(&response);
        // The icon rail has no room for the name: the icon sits at the pill's centre, which in a
        // full-width pill is not where it goes (24 pt in), and the name moves into a tooltip.
        let icon_at = if compact {
            rect.center()
        } else {
            rect.left_center() + vec2(24.0, 0.0)
        };
        let icon_rect = Rect::from_center_size(icon_at + nudge, vec2(22.0, 22.0));
        icon.image(fg, 22.0).paint_at(ui, icon_rect);
        if compact {
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            return tooltip_right(response, label);
        }

        let font = if selected {
            theme::bold(15.0)
        } else {
            theme::semibold(15.0)
        };
        crate::emoji::paint_line(
            ui,
            rect.left_center() + vec2(48.0, 0.0) + nudge,
            label,
            font,
            fg,
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// How a piece of artwork is masked.
///
/// Artists are people and get circles; releases get rounded squares. That is how YouTube Music
/// itself separates the two, and in a mixed search list it is the difference between being able to
/// tell an artist from an album at a glance and not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtShape {
    Square,
    Circle,
    /// A music-video still: 16:9, drawn in a shorter card.
    Wide,
}

impl ArtShape {
    /// The corner radius that produces this shape for `rect`. A radius of half the side is a
    /// circle; `CornerRadius` is a `u8`, so very large art clamps (harmlessly — nothing we draw is
    /// anywhere near 510px across).
    pub(crate) fn radius(self, rect: Rect) -> CornerRadius {
        match self {
            // The curve grows with the art — an eighth of the edge, floored at the general
            // radius so 40px row covers keep today's corner, capped so a big card's corners
            // do not start eating the image.
            ArtShape::Square | ArtShape::Wide => CornerRadius::same(
                ((rect.width().min(rect.height()) * 0.125) as u8)
                    .clamp(theme::radii().art_min, theme::radii().art_max),
            ),
            ArtShape::Circle => {
                CornerRadius::same((rect.width().min(rect.height()) / 2.0).clamp(0.0, 255.0) as u8)
            }
        }
    }
}

/// Placeholder artwork: a filled shape with a glyph, shown until the real cover arrives so rows
/// never change height when images load.
pub fn art_placeholder(ui: &Ui, palette: &Palette, rect: Rect, shape: ArtShape) {
    ui.painter()
        .rect_filled(rect, shape.radius(rect), palette.glass(palette.surface));
    let g = rect.width() * 0.5;
    // A disc reads as a release; a person outline would read as an artist, but the icon set has no
    // person, so the artist placeholder is simply the empty circle.
    if shape != ArtShape::Circle {
        Icon::Disc3
            .image(palette.dim, g)
            .paint_at(ui, Rect::from_center_size(rect.center(), vec2(g, g)));
    }
}

/// Draw cover art, falling back to the placeholder so the slot is always filled.
pub fn paint_art(
    ui: &Ui,
    palette: &Palette,
    rect: Rect,
    texture: Option<&egui::TextureHandle>,
    shape: ArtShape,
) {
    match texture {
        Some(tex) => {
            let radius = shape.radius(rect);
            // The backing is the placeholder's own colour, never black: a light theme was
            // growing a dark rim wherever the image did not quite reach the rect's edges.
            ui.painter().add(egui::Shape::rect_filled(
                rect,
                radius,
                palette.glass(palette.surface),
            ));
            // Fit, never stretch: a crop that arrives at a different aspect than its slot
            // letterboxes instead of squashing. A square cover in a square slot is unchanged.
            let tex_size = tex.size_vec2();
            let scale = (rect.width() / tex_size.x).min(rect.height() / tex_size.y);
            let fitted = Rect::from_center_size(rect.center(), tex_size * scale);
            // A circle crops its art to the circle either way; anything else rounds the image
            // only where it actually reaches the slot's rounded corners.
            let image_radius = match shape {
                ArtShape::Circle => radius,
                _ => inner_radius(radius, rect, fitted),
            };
            egui::Image::new(tex)
                .corner_radius(image_radius)
                .paint_at(ui, fitted);
        }
        None => art_placeholder(ui, palette, rect, shape),
    }
}

/// The corner radius an image fitted inside a rounded slot needs: the slot's own where the
/// image fills it, less as the image sits further in from the slot's edges, and none once it is
/// a whole radius in.
///
/// A wide still letterboxed in a square card sits well clear of the card's rounded corners, so
/// its own corners are square — rounding it too drew a second, smaller set of curves inside the
/// first. An image that fills its slot keeps the full curve, and one inset a little keeps just
/// enough that its corners never poke past the slot's.
pub(crate) fn inner_radius(slot_radius: CornerRadius, slot: Rect, fitted: Rect) -> CornerRadius {
    let inset = ((slot.width() - fitted.width()) / 2.0)
        .max((slot.height() - fitted.height()) / 2.0)
        .max(0.0);
    let r = (f32::from(slot_radius.nw) - inset).max(0.0);
    CornerRadius::same(r.round() as u8)
}

/// The play affordance that rises over artwork under the pointer.
///
/// The single most-felt thing in this design system: a cover you can hover and press is a
/// control, a cover you cannot is a picture. `t` is the hover fade from [`lit`], so the button
/// grows and rises into place rather than appearing.
///
/// `corner` puts it at the bottom-right of a grid card, the way a shelf does; otherwise it is
/// centred, which is what a small row cover wants.
///
/// `playing` is this row/card's own track, actually playing right now (not paused) — paints
/// Pause rather than Play, the same convention the player bar's own big button follows, so
/// hovering the track that's already going shows you can press it to pause, not restart it.
///
/// `target` is where a press on it lands (the card's corner button, the row's cover): under the
/// pointer the button lightens and grows a touch, and held it sinks, as the player bar's play
/// button does, so it reads as a control and not only a sign.
pub fn play_overlay(
    ui: &Ui,
    palette: &Palette,
    art: Rect,
    t: f32,
    corner: bool,
    playing: bool,
    target: Rect,
) {
    if t <= 0.01 {
        return;
    }
    let pointed = t > 0.5 && ui.rect_contains_pointer(target);
    let held = pointed && ui.input(|i| i.pointer.primary_down());
    let full = if corner { 34.0 } else { art.width() * 0.55 };
    let press = if held {
        0.94
    } else if pointed {
        1.06
    } else {
        1.0
    };
    let d = full * (0.7 + 0.3 * t) * press;
    let fill = if pointed {
        palette.accent_hover
    } else {
        palette.accent
    };
    if pointed {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let centre = if corner {
        // Rises 8pt as it fades in.
        art.right_bottom() - vec2(full * 0.5 + 8.0, full * 0.5 + 8.0 - 8.0 * (1.0 - t))
    } else {
        // A scrim under a centred glyph, so it reads over a busy cover.
        ui.painter().rect_filled(
            art,
            ArtShape::Square.radius(art),
            Color32::from_black_alpha((150.0 * t) as u8),
        );
        art.center()
    };
    ui.painter()
        .circle_filled(centre, d / 2.0, fill.gamma_multiply(t));
    let glyph = d * 0.44;
    let icon = if playing {
        Icon::PauseFilled
    } else {
        Icon::PlayFilled
    };
    // The play triangle's visual centre sits left of its bounding box, so nudge it right; Pause
    // is symmetric and needs none.
    let offset = if playing { 0.0 } else { glyph * 0.06 };
    icon.image(palette.on_accent.gamma_multiply(t), glyph)
        .paint_at(
            ui,
            Rect::from_center_size(centre + vec2(offset, 0.0), vec2(glyph, glyph)),
        );
}

/// Format a duration the way a track list does.
pub fn fmt_duration(secs: u64) -> String {
    let (m, s) = (secs / 60, secs % 60);
    if m >= 60 {
        format!("{}:{:02}:{:02}", m / 60, m % 60, s)
    } else {
        format!("{m}:{s:02}")
    }
}

/// Where a clickable piece of a row's subtitle leads.
///
/// Deliberately not an `app::Route`: widgets stay independent of routing, and the caller turns
/// this into navigation. It also means no `String` is allocated per row per frame just to describe
/// a link nobody clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTarget<'a> {
    Artist(&'a str),
    Album(&'a str),
}

/// One piece of a row's subtitle. A piece with a target is a link.
///
/// `sep` is what precedes the piece: `" • "` between fields, `", "` between artists in the same
/// credit, and nothing before the first. Without it several artists would read `A • B • C` rather
/// than the `A, B` YouTube writes.
#[derive(Clone, Copy, Debug)]
pub struct Part<'a> {
    pub text: &'a str,
    pub target: Option<LinkTarget<'a>>,
    pub sep: &'a str,
}

/// What separates two fields of a subtitle.
pub const FIELD_SEP: &str = " • ";
/// What separates two artists inside one credit.
pub const NAME_SEP: &str = ", ";

impl<'a> Part<'a> {
    pub fn plain(text: &'a str) -> Self {
        Self {
            text,
            target: None,
            sep: FIELD_SEP,
        }
    }

    pub fn link(text: &'a str, target: LinkTarget<'a>) -> Self {
        Self {
            text,
            target: Some(target),
            sep: FIELD_SEP,
        }
    }

    /// The same piece, joined to the one before it by `sep`.
    pub fn joined(mut self, sep: &'a str) -> Self {
        self.sep = sep;
        self
    }
}

/// A row's subtitle: a fixed handful of pieces, so no allocation is needed to describe one.
///
/// Six rather than three because a track can credit several artists and each is its own link —
/// four artists, an album and the queue's "Radio" tail is the realistic ceiling.
pub const SUBTITLE_PARTS: usize = 6;
pub type Subtitle<'a> = [Option<Part<'a>>; SUBTITLE_PARTS];

/// An empty subtitle, to fill in.
pub fn no_subtitle<'a>() -> Subtitle<'a> {
    [None; SUBTITLE_PARTS]
}

/// Build a one-piece, unlinked subtitle — what most rows have.
pub fn plain_subtitle(text: &str) -> Subtitle<'_> {
    let mut out = no_subtitle();
    out[0] = Some(Part::plain(text));
    out
}

/// What a click on a row meant.
///
/// The link is reported separately from the row's own response so a click on the artist name does
/// not *also* start the track. The caller checks `link` first.
pub struct RowClick<'a> {
    pub response: egui::Response,
    pub link: Option<LinkTarget<'a>>,
    /// Whether the row's own body was activated to play — the double-click-to-play setting
    /// applied. Callers must use this rather than `response.clicked()`, which is also true on
    /// the single click of a double.
    pub activate: bool,
    pub(crate) action: Option<super::ui::RowAction>,
}

/// What a song's context menu is built from: the track everything derives from.
///
/// Later milestones add shape here — like state, playlist membership — rather than re-plumbing
/// every call site.
pub struct RowMenu<'a> {
    pub track: &'a crate::model::Track,
    /// The open playlist, when this row is one of its tracks and the playlist is editable
    /// (yours — not LM, not someone else's). Enables "Remove from playlist".
    pub playlist_id: Option<&'a str>,
    /// Your playlists, for the "Save to playlist" picker. Empty when signed out, which hides
    /// the item with no auth check — the library is only ever fetched on sign-in.
    pub playlists: &'a [crate::model::PlaylistRef],
    /// Whether there is a session. Gates the rating thumbs: without one the write is
    /// impossible, and a control that cannot do anything should not be drawn.
    pub signed_in: bool,
    /// This row's own position in the queue, only from the Queue panel. Enables
    /// "Remove from queue" — every other caller leaves it `None`.
    pub queue_index: Option<usize>,
}

/// The fields a [`track_row`] shows, grouped so the function stays under clippy's arity limit
/// and the call sites read like a row.
pub struct TrackRow<'a> {
    pub number: Option<usize>,
    /// A chart rank, shown in place of the position number where the track names one.
    pub rank: Option<&'a str>,
    /// Which way the rank moved. Unknown paints nothing.
    pub trend: crate::model::ChartTrend,
    pub title: &'a str,
    /// The album/single this track belongs to, when known — lets the title itself carry the
    /// same kind of link the artist credit below it already can. `None` just leaves the title
    /// plain, exactly as it always was.
    pub album_id: Option<&'a str>,
    pub subtitle: Subtitle<'a>,
    pub duration: Option<u64>,
    pub plays: Option<&'a str>,
    pub explicit: bool,
    pub active: bool,
    /// `active`, and actually playing right now rather than paused — the row's own hover play
    /// button shows Pause instead of Play when this is set, so you can tell (and press) it to
    /// pause the track that's already going instead of restarting it.
    pub playing: bool,
    /// A small right-aligned type label ("Artist", "Album"), for mixed lists where the rows are
    /// not all songs. Shown in place of the duration, which those rows do not have.
    pub badge: Option<&'a str>,
    pub art: Option<&'a egui::TextureHandle>,
    pub shape: ArtShape,
    /// Draw no cover in the leading slot: the album page, where every row would only repeat the
    /// header's own art. The slot collapses and the number becomes the row's leading element;
    /// while the pointer is over the row the number gives way to a play/pause glyph, since the
    /// cover's own hover play button is not there to carry that affordance.
    pub hide_art: bool,
    /// Distinguishes this row's interactive pieces from its neighbours'.
    pub id_salt: usize,
    /// Right-click menu. `None` for rows that are not songs.
    pub menu: Option<RowMenu<'a>>,
    /// Your rating. Drives the hover thumbs; `Indifferent` on rows without a menu.
    pub like: crate::model::LikeStatus,
    /// Whether this row offers rating thumbs at all. The queue sets it `false`: it is a list of
    /// what happens next, and a column of controls repeated down it is noise. Rating a queued
    /// song is still one right-click away.
    pub show_rating: bool,
    /// Keep a like out of sight until the row is hovered. Liked Music sets it: every row there
    /// is liked by definition, so a filled thumb down the whole list says nothing.
    pub quiet_like: bool,
    /// A playlist this song can be added to with one press: a round + at the row's end, left of
    /// the duration. The Suggestions below your own playlist, as YouTube Music draws them.
    pub add_to: Option<&'a str>,
}

/// Paint a subtitle left to right, each linked piece getting its own hit area, and report the
/// piece that was clicked.
///
/// Shared by track rows and the player bar. The bar used to paint its own single string with one
/// hit rect over the whole thing, which is why only one artist there was ever clickable and why
/// it had no album link and no hover underline.
#[allow(clippy::too_many_arguments)]
pub fn paint_subtitle<'a>(
    ui: &mut Ui,
    palette: &Palette,
    pos: egui::Pos2,
    right: f32,
    subtitle: &Subtitle<'a>,
    tail: Option<&'a str>,
    id_salt: impl std::hash::Hash + Clone + std::fmt::Debug,
    font: egui::FontId,
    base_colour: Color32,
) -> Option<LinkTarget<'a>> {
    let mut x = pos.x;
    let mut clicked = None;

    let parts = subtitle
        .iter()
        .flatten()
        .filter(|p| !p.text.is_empty())
        .copied()
        .chain(tail.map(Part::plain));

    for (i, part) in parts.enumerate() {
        if x >= right {
            break;
        }
        // Measure the piece before painting its separator: a trailing " • " with nothing
        // after it reads as broken, so both go when the piece has no room left.
        let sep_w = if i > 0 && !part.sep.is_empty() {
            text_width(ui, part.sep, &font)
        } else {
            0.0
        };
        let shown = truncate_to(ui, part.text, font.clone(), (right - x - sep_w).max(0.0));
        if shown.is_empty() {
            break;
        }
        if sep_w > 0.0 {
            ui.painter().text(
                egui::pos2(x, pos.y),
                egui::Align2::LEFT_CENTER,
                part.sep,
                font.clone(),
                palette.dim,
            );
            x += sep_w;
        }
        let w = text_width(ui, &shown, &font);

        let mut colour = base_colour;
        if let Some(target) = part.target {
            let hit = Rect::from_min_size(egui::pos2(x, pos.y - 8.0), vec2(w, 16.0));
            let r = ui.interact(hit, ui.id().with((id_salt.clone(), i)), Sense::click());
            if r.hovered() {
                colour = palette.text;
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                ui.painter().hline(
                    x..=(x + w),
                    pos.y + 7.0,
                    egui::Stroke::new(1.0, palette.text),
                );
            }
            if r.clicked() {
                clicked = Some(target);
            }
        }
        crate::emoji::paint_line(ui, egui::pos2(x, pos.y), &shown, font.clone(), colour);
        x += w;
    }
    clicked
}

/// How wide the dissolve at an overlong line's edge is.
pub const FADE_W: f32 = 24.0;
/// Slices a dissolve is painted in. The ramp is stepped rather than smooth; at this size a
/// line's own anti-aliasing hides the steps, and every slice is another paint of the line.
const FADE_STEPS: usize = 12;
/// Hover this long before an overlong line starts moving.
const MARQUEE_DWELL_SECS: f64 = 0.6;
/// Pixels per second once moving.
const MARQUEE_SPEED: f32 = 30.0;
/// Rest at each end of the travel before turning around.
const MARQUEE_HOLD_SECS: f64 = 0.4;
/// Rest at the far end, where the tail is in view: long enough to read it. A title a little too
/// wide for the player bar gets there in about a second, and with the short rest it turned
/// straight back before the end could be read.
const MARQUEE_END_HOLD_SECS: f64 = 1.5;
/// The click/hover height of a marquee line — the player bar's title uses it for its hit rect
/// when the text fits and `marquee_line` draws no viewport of its own.
pub(crate) const MARQUEE_HIT_H: f32 = 18.0;

/// How deep each edge of a scrolling line dissolves, `(lead, trail)` from 0 (solid) to 1 (fully
/// faded at the edge): in proportion to how much of the line is hidden past that edge, up to one
/// fade's width. So the trailing fade thins out as the tail comes into view and is gone once the
/// line has scrolled to its end, where it would otherwise sit over the last letters; the leading
/// one deepens the same way as the head scrolls off, instead of popping in.
fn dissolve_depths(offset: f32, max_off: f32) -> (f32, f32) {
    let depth = |hidden: f32| (hidden / FADE_W).clamp(0.0, 1.0);
    (depth(offset), depth(max_off - offset))
}

/// The strips a clipped line's viewport dissolves at, for the depths [`dissolve_depths`] gives:
/// a strip only where its edge fades at all. Returns `(main, lead, trail)`, where `main` is what
/// the solid paint keeps — on a viewport narrower than the fade the strips can overlap, which
/// leaves `main` inverted and the caller paints nothing solid.
fn dissolve_bands(
    clip: Rect,
    (lead_depth, trail_depth): (f32, f32),
    left: f32,
    right: f32,
) -> (Rect, Option<Rect>, Option<Rect>) {
    let lead = (lead_depth > 0.0).then(|| {
        Rect::from_min_max(
            clip.min,
            egui::pos2((left + FADE_W).min(clip.max.x), clip.max.y),
        )
    });
    let trail = (trail_depth > 0.0).then(|| {
        Rect::from_min_max(
            egui::pos2((right - FADE_W).max(clip.min.x), clip.min.y),
            clip.max,
        )
    });
    let mut main = clip;
    if let Some(lead) = lead {
        main.min.x = main.min.x.max(lead.max.x);
    }
    if let Some(trail) = trail {
        main.max.x = main.max.x.min(trail.min.x);
    }
    (main, lead, trail)
}

/// Dissolve a clipped line's cut edge by repainting it in slices, each fainter than the last,
/// so the text itself fades out instead of a plate of some background colour being laid over
/// it.
///
/// The dissolve this replaces painted the background colour over the line, which needed the
/// background to be known: opaque mode could supply it, acrylic mode could not (a live-blurred
/// desktop is unknowable ahead of painting), so it was skipped there and long titles hard-cut.
/// Multiplying the line's own alpha needs no such guess — whatever is behind the text shows
/// through the fade exactly as it shows through the text — so the dissolve works in every mode.
///
/// `band` is the strip the cut eats into; `cut_at_start` puts the transparent end on the
/// band's leading edge (the case once the line has scrolled) rather than its trailing one, and
/// `depth` is how far the cut edge fades: 1 to nothing, less to a faint line still there.
fn fade_slices(
    ui: &mut Ui,
    band: Rect,
    cut_at_start: bool,
    depth: f32,
    mut paint: impl FnMut(&mut Ui, Rect),
) {
    if band.width() < 1.0 {
        return;
    }
    // Relative to whatever opacity the caller's ui already carries (a disabled ui, say), and
    // restored after each slice so nothing downstream inherits a dissolve's alpha.
    let prev = ui.opacity();
    let step = band.width() / FADE_STEPS as f32;
    for i in 0..FADE_STEPS {
        let f = i as f32 / (FADE_STEPS - 1) as f32;
        let t = 1.0 - depth * (1.0 - if cut_at_start { f } else { 1.0 - f });
        let slice = Rect::from_min_max(
            egui::pos2(band.min.x + i as f32 * step, band.min.y),
            egui::pos2(band.min.x + (i + 1) as f32 * step, band.max.y),
        );
        ui.set_opacity(prev * t);
        paint(ui, slice);
        ui.set_opacity(prev);
    }
}

// ---------------------------------------------------------------------------------------------
// Scroll-edge fades
// ---------------------------------------------------------------------------------------------

/// How far a scrolled list fades at an end with more of it past that end.
pub(crate) const SCROLL_FADE: f32 = 48.0;

/// How far a card's scrolled lists fade with these settings: [`SCROLL_FADE`], or not at all
/// when Scroll fades is off.
pub(crate) fn scroll_fade(settings: &crate::settings::Settings) -> f32 {
    if settings.scroll_fades {
        SCROLL_FADE
    } else {
        0.0
    }
}

/// How many straight pieces an eased fade's curve is drawn in: colours only vary linearly across
/// a triangle, so the mesh is cut this many times over the band and the curve is exact at each cut.
const EASE_STEPS: usize = 6;

/// Fade a scroll area's contents at each end that has more content past it: none at the top of a
/// list scrolled all the way up, none at the bottom of one at its end. `from` is
/// [`painted_so_far`] taken just before the scroll area was shown. Transparent, so on a
/// see-through window the desktop shows through the fade, not a colour painted to look like it.
/// The floating scroll bar is left as it is.
pub(crate) fn fade_scroll_edges<R>(
    ui: &Ui,
    from: usize,
    output: &egui::scroll_area::ScrollAreaOutput<R>,
    band: f32,
) {
    let (above, below) = edge_fades(
        (
            output.state.offset.y,
            output.content_size.y,
            output.inner_rect.height(),
        ),
        band,
    );
    let scroll = &ui.spacing().scroll;
    let strip = scroll.bar_width + scroll.bar_inner_margin + scroll.bar_outer_margin;
    fade_painted_except(
        ui.ctx(),
        ui.layer_id(),
        from,
        output.inner_rect,
        EdgeFade::eased(band, above),
        EdgeFade::eased(band, below),
        output.inner_rect.right() - strip,
    );
}

/// Whether a shape lies wholly in the strip right of `strip_left`, where the scroll bar is.
fn in_bar_strip(bounds: Rect, strip_left: f32) -> bool {
    bounds.min.x >= strip_left
}

/// Fade everything painted into `layer` since `from` toward the top and bottom of `rect`: see
/// [`fade_painted_except`], with nothing left out.
pub(crate) fn fade_painted(
    ctx: &egui::Context,
    layer: egui::LayerId,
    from: usize,
    rect: Rect,
    above: EdgeFade,
    below: EdgeFade,
) {
    fade_painted_except(ctx, layer, from, rect, above, below, f32::INFINITY);
}

/// How one end of a region fades: over `band` points from the edge inward, and by `strength`
/// (0 not at all, 1 fully transparent at the edge), in a straight ramp or an eased one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EdgeFade {
    band: f32,
    strength: f32,
    eased: bool,
}

impl EdgeFade {
    pub(crate) fn new(band: f32, strength: f32) -> Self {
        Self {
            band: band.max(0.0),
            strength: strength.clamp(0.0, 1.0),
            eased: false,
        }
    }

    /// A fade that leaves the edge and arrives at full strength gently (smoothstep), rather than
    /// in a straight ramp that reads as a soft cut at a short band.
    pub(crate) fn eased(band: f32, strength: f32) -> Self {
        Self {
            eased: true,
            ..Self::new(band, strength)
        }
    }

    /// Where, measured in from the edge, the mesh is cut so the ramp is drawn exactly.
    fn cuts(self) -> impl Iterator<Item = f32> {
        let steps = if self.none() {
            0
        } else if self.eased {
            EASE_STEPS
        } else {
            1
        };
        let band = self.band;
        (0..=steps)
            .filter(move |_| steps > 0)
            .map(move |k| band * k as f32 / steps as f32)
    }

    fn none(self) -> bool {
        self.band <= 0.0 || self.strength <= 0.0
    }
}

/// How much of what is drawn at height `y` survives: 1 in the middle, falling linearly to
/// `1 - strength` at either edge of `top..bottom` (along a smoothstep for an eased fade).
fn fade_factor(y: f32, top: f32, bottom: f32, above: EdgeFade, below: EdgeFade) -> f32 {
    let end = |into: f32, fade: EdgeFade| {
        if fade.none() {
            1.0
        } else {
            let t = (into / fade.band).clamp(0.0, 1.0);
            let t = if fade.eased {
                t * t * (3.0 - 2.0 * t)
            } else {
                t
            };
            1.0 - fade.strength * (1.0 - t)
        }
    };
    end(y - top, above) * end(bottom - y, below)
}

/// Where the next shape painted into `ui`'s layer will go, to hand to [`fade_painted`].
pub(crate) fn painted_so_far(ui: &egui::Ui) -> usize {
    ui.ctx()
        .graphics(|g| g.get(ui.layer_id()).map_or(0, |l| l.next_idx().0))
}

/// Fade everything painted into `layer` since `from` toward the top and bottom of `rect`.
///
/// Real transparency: the shapes are tessellated and each vertex's colour (premultiplied, so its
/// alpha with it) is scaled by how close it is to an edge, so what shows through is whatever is
/// behind them. Triangles are first cut along the band's lines, so the linear ramp between those
/// lines is exact however tall a triangle was (a row's fill, a cover).
fn fade_painted_except(
    ctx: &egui::Context,
    layer: egui::LayerId,
    from: usize,
    rect: Rect,
    above: EdgeFade,
    below: EdgeFade,
    strip_left: f32,
) {
    if above.none() && below.none() {
        return;
    }
    let (top, bottom) = (rect.top(), rect.bottom());
    let cuts: Vec<f32> = above
        .cuts()
        .map(|d| top + d)
        .chain(below.cuts().map(|d| bottom - d))
        .collect();
    // What lies wholly between the two bands is untouched, and is left as it is.
    let untouched = Rect::from_min_max(
        egui::pos2(
            f32::NEG_INFINITY,
            if above.none() {
                f32::NEG_INFINITY
            } else {
                top + above.band
            },
        ),
        egui::pos2(
            f32::INFINITY,
            if below.none() {
                f32::INFINITY
            } else {
                bottom - below.band
            },
        ),
    );
    let shapes: Vec<(usize, egui::epaint::ClippedShape)> = ctx.graphics(|g| {
        g.get(layer).map_or_else(Vec::new, |list| {
            list.all_entries()
                .enumerate()
                .skip(from)
                .filter(|(_, c)| !matches!(c.shape, egui::Shape::Noop))
                .filter(|(_, c)| {
                    let bounds = c.shape.visual_bounding_rect();
                    !untouched.contains_rect(bounds) && !in_bar_strip(bounds, strip_left)
                })
                .map(|(i, c)| (i, c.clone()))
                .collect()
        })
    });
    if shapes.is_empty() {
        return;
    }
    let ppp = ctx.pixels_per_point();
    let mut faded = Vec::with_capacity(shapes.len());
    for (i, clipped) in shapes {
        let clip = clipped.clip_rect;
        let mut meshes = Vec::new();
        let mut callback = false;
        for primitive in ctx.tessellate(vec![clipped], ppp) {
            match primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => {
                    let mut mesh = cut_mesh(&mesh, &cuts);
                    for v in &mut mesh.vertices {
                        let f = fade_factor(v.pos.y, top, bottom, above, below);
                        v.color = v.color.gamma_multiply(f);
                    }
                    meshes.push(egui::Shape::mesh(mesh));
                }
                egui::epaint::Primitive::Callback(_) => callback = true,
            }
        }
        // A paint callback cannot be faded, and is left exactly as it was.
        if !callback {
            faded.push((i, clip, egui::Shape::Vec(meshes)));
        }
    }
    ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        for (i, clip, shape) in faded {
            list.set(egui::layers::ShapeIdx(i), clip, shape);
        }
    });
}

/// `mesh` with every triangle that crosses one of the horizontal lines `cuts` split along it, so
/// no triangle spans a line: a colour set per vertex then varies across a band exactly as it does
/// between its lines.
fn cut_mesh(mesh: &egui::Mesh, cuts: &[f32]) -> egui::Mesh {
    use egui::epaint::Vertex;
    let mut out = egui::Mesh::with_texture(mesh.texture_id);
    for tri in mesh.indices.as_chunks::<3>().0 {
        let mut pieces: Vec<Vec<Vertex>> =
            vec![tri.iter().map(|&i| mesh.vertices[i as usize]).collect()];
        for &y in cuts {
            let mut next = Vec::with_capacity(pieces.len() + 1);
            for piece in pieces {
                let (above, below) = split_polygon(&piece, y);
                next.extend([above, below].into_iter().filter(|p| p.len() >= 3));
            }
            pieces = next;
        }
        for piece in pieces {
            let base = out.vertices.len() as u32;
            out.vertices.extend_from_slice(&piece);
            for k in 1..piece.len() as u32 - 1 {
                out.add_triangle(base, base + k, base + k + 1);
            }
        }
    }
    out
}

/// A convex polygon split by the horizontal line at `y`: the part above it and the part below,
/// each still convex and in the same winding.
fn split_polygon(
    poly: &[egui::epaint::Vertex],
    y: f32,
) -> (Vec<egui::epaint::Vertex>, Vec<egui::epaint::Vertex>) {
    let (mut above, mut below) = (Vec::new(), Vec::new());
    for (k, &a) in poly.iter().enumerate() {
        let b = poly[(k + 1) % poly.len()];
        if a.pos.y < y {
            above.push(a);
        } else {
            below.push(a);
        }
        if (a.pos.y < y) != (b.pos.y < y) {
            let t = (y - a.pos.y) / (b.pos.y - a.pos.y);
            let mid = lerp_vertex(a, b, t);
            above.push(mid);
            below.push(mid);
        }
    }
    (above, below)
}

fn lerp_vertex(a: egui::epaint::Vertex, b: egui::epaint::Vertex, t: f32) -> egui::epaint::Vertex {
    let channel = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    let (ca, cb) = (a.color.to_array(), b.color.to_array());
    egui::epaint::Vertex {
        pos: a.pos + (b.pos - a.pos) * t,
        uv: a.uv + (b.uv - a.uv) * t,
        color: Color32::from_rgba_premultiplied(
            channel(ca[0], cb[0]),
            channel(ca[1], cb[1]),
            channel(ca[2], cb[2]),
            channel(ca[3], cb[3]),
        ),
    }
}

/// How strongly each end of a scrolled list should fade: 0 with nothing past that end, rising to
/// 1 once a full `fade` of list is hidden there. `scroll` is the offset, content height and
/// viewport height; a list not yet measured fades nowhere.
pub(crate) fn edge_fades(scroll: (f32, f32, f32), fade: f32) -> (f32, f32) {
    let (offset, content, view) = scroll;
    if content <= 0.0 || fade <= 0.0 {
        return (0.0, 0.0);
    }
    let above = (offset / fade).clamp(0.0, 1.0);
    let below = ((content - offset - view) / fade).clamp(0.0, 1.0);
    (above, below)
}

// ---------------------------------------------------------------------------------------------
// Card corners
// ---------------------------------------------------------------------------------------------

/// The x-inset of a rounded corner at `d` points in from the edge, for a corner of radius
/// `rad`: `rad` at the very edge, 0 once `d >= rad`. The quarter-circle both the header wash
/// and the corner repaint walk, so a square shape never pokes past a card's corners.
pub(crate) fn corner_inset(rad: f32, d: f32) -> f32 {
    let dy = (rad - d).max(0.0);
    rad - (rad * rad - dy * dy).max(0.0).sqrt()
}

/// A card's own rect, given the rect its content was laid out in and the frame's inner margin.
/// The header wash, the artist hero and the corner repaint all need the card's edges; the
/// content rect is all a `Ui` hands back.
pub(crate) fn card_rect(inner: egui::Rect, inset: egui::Margin) -> egui::Rect {
    egui::Rect::from_min_max(
        inner.min - vec2(f32::from(inset.left), f32::from(inset.top)),
        inner.max + vec2(f32::from(inset.right), f32::from(inset.bottom)),
    )
}

/// Lay `add_contents` out in `rect`, a card's content area, so that it can never make the card
/// taller.
///
/// A card is a `Frame`, and a frame is as tall as what is laid out in it. A page that is not in
/// a scroll area — a loader's placeholder rows and shelves, in a window shorter than they are —
/// ran past the card, the frame went with it, and its rounded bottom corners ended up below the
/// clip, leaving the fill square. In a child that is only handed `rect`, the content spills past
/// the bottom and the clip cuts it, as it does a scrolled page, and the frame stays the card.
pub(crate) fn contain<R>(ui: &mut Ui, rect: Rect, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
    add_contents(&mut child)
}

/// How far in from a card's top and bottom edge the rounded silhouette meets the content
/// column `inset_x` in from its sides. Content kept at least this far from both edges never
/// reaches outside the silhouette.
fn corner_clearance(rad: f32, inset_x: f32) -> f32 {
    let arm = rad - inset_x.clamp(0.0, rad);
    rad - (rad * rad - arm * arm).max(0.0).sqrt()
}

/// Keep a card's content inside its rounded silhouette when the shell is translucent.
///
/// Opaque, content runs to the card's top and bottom edges and [`paint_corner_notches`] covers
/// what pokes past the corners in the gutter colour. Translucent, no colour matches a gutter
/// the desktop shows through — an opaque one read as dark slivers at every corner — so the
/// clip stops [`corner_clearance`] short of both edges instead. Call before the card's content
/// is laid out; scroll areas inherit the clip.
pub(crate) fn clip_card_content(ui: &mut Ui, card: egui::Rect, inset_x: f32, palette: &Palette) {
    if !palette.translucent() {
        return;
    }
    let close = corner_clearance(crate::theme::radii().panel as f32, inset_x);
    let clip = ui.clip_rect().intersect(card.shrink2(vec2(0.0, close)));
    ui.set_clip_rect(clip);
}

/// The rows of the four corner notches a card's content can reach: `(y, x0, x1)` per row, from
/// the corner's edge inward, spanning from the content column's edge out to the card's rounded
/// silhouette. The content is clipped to its column, so the outer strip of each notch is left
/// alone; the samples stop where the silhouette comes back inside the column, and the last one
/// is degenerate there, tapering the mesh to a point rather than leaving a wedge unpainted.
fn notch_rows(card: egui::Rect, rad: f32, inset_x: f32) -> [Vec<(f32, f32, f32)>; 4] {
    let inset_x = inset_x.clamp(0.0, rad);
    let mut rows: [Vec<(f32, f32, f32)>; 4] = std::array::from_fn(|_| Vec::new());
    let close = corner_clearance(rad, inset_x);
    if close <= 0.0 {
        return rows;
    }
    const ARC_STEPS: usize = 10;
    for i in 0..=ARC_STEPS {
        let d = close * i as f32 / ARC_STEPS as f32;
        let inset = corner_inset(rad, d);
        let (top, bottom) = (card.top() + d, card.bottom() - d);
        rows[0].push((top, card.left() + inset_x, card.left() + inset));
        rows[1].push((top, card.right() - inset, card.right() - inset_x));
        rows[2].push((bottom, card.left() + inset_x, card.left() + inset));
        rows[3].push((bottom, card.right() - inset, card.right() - inset_x));
    }
    rows
}

/// Paint the four corner notches of a card — the region inside its square rect but outside its
/// rounded silhouette — in the gutter colour.
///
/// Cards have no top or bottom inner margin any more, so their scroll viewports run the card's
/// full height and content is clipped at the card's own top and bottom edges. The clip is a
/// rectangle and the card is not, so what the content leaves outside the silhouette is painted
/// over with `palette.window`, exactly the colour the gutter is in opaque mode, which makes
/// this the rounded clip egui cannot do. A translucent gutter has no such colour: there this
/// paints nothing, and [`clip_card_content`] keeps the content out of the corners instead.
pub(crate) fn paint_corner_notches(ui: &Ui, card: egui::Rect, inset_x: f32, palette: &Palette) {
    if palette.translucent() || card.width() <= 0.0 || card.height() <= 0.0 {
        return;
    }
    let colour = palette.window;
    let painter = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), card);
    for corner in notch_rows(card, crate::theme::radii().panel as f32, inset_x) {
        if corner.len() < 2 {
            continue;
        }
        let mut mesh = egui::Mesh::default();
        for &(y, x0, x1) in &corner {
            for x in [x0, x1] {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: egui::pos2(x, y),
                    uv: egui::epaint::WHITE_UV,
                    color: colour,
                });
            }
        }
        for row in 0..corner.len() - 1 {
            let a = (row * 2) as u32;
            mesh.add_triangle(a, a + 2, a + 1);
            mesh.add_triangle(a + 1, a + 2, a + 3);
        }
        painter.add(mesh);
    }
}

/// The edge round a solid card, drawn once, last.
///
/// A see-through card's frame draws its own edge. A solid card cannot let it: the corner notches
/// ([`paint_corner_notches`]) are painted after the page, in the gutter's colour, and cover the
/// corner pixels of anything drawn before them, and drawing the edge twice to get round that put
/// two lines a fraction of a pixel apart, thick and blurred. So a solid card's frame draws no
/// edge and its function holds one of these from the top: dropped as the function ends, on any
/// return, it draws the edge exactly once, after the notches, snapped to whole pixels. The
/// colour is the opaque one the translucent edge blends to over the card. Nothing when outlines
/// are off or the window is see-through.
pub(crate) struct CardEdge {
    ctx: egui::Context,
    layer: egui::LayerId,
    card: egui::Rect,
    colour: Option<Color32>,
}

impl CardEdge {
    pub(crate) fn new(ui: &Ui, card: egui::Rect, palette: &Palette) -> Self {
        let colour = (!palette.translucent() && palette.card_edge().is_some()).then(|| {
            palette
                .panel
                .lerp_to_gamma(palette.text, crate::theme::CARD_EDGE_STRENGTH)
        });
        Self {
            ctx: ui.ctx().clone(),
            layer: ui.layer_id(),
            card,
            colour,
        }
    }
}

impl Drop for CardEdge {
    fn drop(&mut self) {
        let Some(colour) = self.colour else { return };
        let painter = egui::Painter::new(self.ctx.clone(), self.layer, self.card);
        painter.add(
            egui::epaint::RectShape::stroke(
                self.card,
                CornerRadius::same(crate::theme::radii().panel),
                egui::Stroke::new(1.0, colour),
                egui::StrokeKind::Inside,
            )
            .with_round_to_pixels(true),
        );
    }
}

/// What a scrolling line remembers between frames. Lives in egui's temp storage under the
/// caller's id salt, so no scroll state threads through the app.
#[derive(Clone, Default)]
struct MarqueeState {
    /// The full width last measured. A different width means a different line — a new
    /// track on the same salt — so the offset resets rather than starting mid-scroll.
    full_w: f32,
    offset: f32,
    /// +1 moving away from the head, -1 coming back.
    dir: f32,
    hover_since: Option<f64>,
    last_t: f64,
    hold_until: f64,
}

/// One travel step with end flips: never leaves `0..=max_off`, and reports which way it is
/// heading afterwards. Pure, so the bounds are unit-testable without an egui context.
fn pingpong(offset: f32, dir: f32, max_off: f32, advance: f32) -> (f32, f32) {
    let mut offset = offset + dir * advance;
    let mut dir = dir;
    if offset >= max_off {
        offset = max_off;
        dir = -1.0;
    } else if offset <= 0.0 {
        offset = 0.0;
        dir = 1.0;
    }
    (offset, dir)
}

/// A line that scrolls itself on hover when it does not fit.
///
/// Fits and it is an ordinary painted line — no state, no cost. Overflows and the paint is
/// clipped to `max_width` with a dissolve on the trailing edge; hovering the text scrolls
/// it ping-pong after a short dwell, with a dissolve appearing on the leading edge once
/// the head is cut off. Returns the viewport's response when one was drawn — only the
/// overflow case creates one — so a caller can make the text itself clickable without
/// covering the hover that drives the scroll; `None` means it fit and was painted plainly.
///
/// The dissolves fade the line's own alpha ([`fade_slices`]), so they need no background
/// colour and hold up over the acrylic shell as well as the opaque one. Repaint discipline:
/// frames are asked for only while hovered-and-overflowing. At rest nothing is requested;
/// unhovering snaps home on the pointer-move frame that caused it.
/// Ask for the next marquee frame: continuous only while the text moves. Through the dwell
/// before it starts and the hold at each end nothing changes on screen, so one frame at the end
/// of the wait is enough.
fn marquee_repaint(ui: &Ui, now: f64, hover_since: f64, hold_until: f64) {
    let wake = (hover_since + MARQUEE_DWELL_SECS).max(hold_until);
    if wake > now {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(wake - now));
    } else {
        ui.ctx().request_repaint();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn marquee_line(
    ui: &mut Ui,
    pos: egui::Pos2,
    max_width: f32,
    text: &str,
    font: egui::FontId,
    color: Color32,
    id_salt: impl std::hash::Hash + Clone + std::fmt::Debug,
    sense: egui::Sense,
    // Whether the caller's wider row (art, duration, blank space — not just this text strip)
    // is under the pointer. A row's own hover already exists at the call site; without this,
    // the scroll only ever started from the narrow text itself, which felt inconvenient to
    // trigger from anywhere else on the row.
    row_hovered: bool,
) -> Option<egui::Response> {
    let full = text_width(ui, text, &font);
    if full <= max_width {
        // Clipped to the slot even though it measured as fitting: a measuring mistake must not
        // paint over whatever sits past the text's own room.
        let slot = Rect::from_min_max(
            egui::pos2(pos.x, pos.y - MARQUEE_HIT_H),
            egui::pos2(pos.x + max_width, pos.y + MARQUEE_HIT_H),
        )
        .intersect(ui.clip_rect());
        crate::emoji::paint_clipped_line(ui, slot, pos, text, font, color);
        return None;
    }
    let id = ui.id().with(id_salt);
    let viewport = Rect::from_min_size(
        egui::pos2(pos.x, pos.y - MARQUEE_HIT_H / 2.0),
        vec2(max_width, MARQUEE_HIT_H),
    );
    let response = ui.interact(viewport, id, sense);
    // Geometric, not `hovered()`: see `paint_subtitle_marquee`.
    let hovered = response.contains_pointer() || row_hovered;
    let now = ui.input(|i| i.time);
    let mut state: MarqueeState = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    if (state.full_w - full).abs() > 0.5 {
        state = MarqueeState {
            full_w: full,
            dir: 1.0,
            last_t: now,
            ..Default::default()
        };
    }
    if hovered {
        if state.hover_since.is_none() {
            state.hover_since = Some(now);
        }
        let since = state.hover_since.unwrap_or(now);
        if now - since >= MARQUEE_DWELL_SECS && now >= state.hold_until {
            // From the end of any wait, not the last frame: with no frames painted through the
            // dwell or a hold, the last one may be long before the text started moving again.
            let moving_since = state
                .last_t
                .max(since + MARQUEE_DWELL_SECS)
                .max(state.hold_until);
            let dt = (now - moving_since).clamp(0.0, 0.25) as f32;
            let max_off = (full - max_width).max(0.0);
            let (offset, dir) = pingpong(state.offset, state.dir, max_off, MARQUEE_SPEED * dt);
            if (dir - state.dir).abs() > f32::EPSILON {
                state.hold_until = now
                    + if dir < 0.0 {
                        MARQUEE_END_HOLD_SECS
                    } else {
                        MARQUEE_HOLD_SECS
                    };
            }
            state.offset = offset;
            state.dir = dir;
        }
        marquee_repaint(ui, now, since, state.hold_until);
    } else {
        state.hover_since = None;
        state.offset = 0.0;
        state.dir = 1.0;
        state.hold_until = 0.0;
    }
    state.last_t = now;
    state.full_w = full;
    ui.ctx().data_mut(|d| d.insert_temp(id, state.clone()));

    let clip = ui.clip_rect().intersect(viewport);
    let line_pos = egui::pos2(pos.x - state.offset, pos.y);
    let depths = dissolve_depths(state.offset, full - max_width);
    let (main, lead, trail) = dissolve_bands(clip, depths, pos.x, pos.x + max_width);
    let mut paint = |ui: &mut Ui, slice: Rect| {
        crate::emoji::paint_clipped_line(ui, slice, line_pos, text, font.clone(), color);
    };
    if main.width() > 0.5 {
        paint(ui, main);
    }
    if let Some(lead) = lead {
        fade_slices(ui, lead, true, depths.0, &mut paint);
    }
    if let Some(trail) = trail {
        fade_slices(ui, trail, false, depths.1, &mut paint);
    }
    Some(response)
}

/// The full laid-out width of a subtitle, separators included — what the marquee would measure
/// if nothing clipped it. The player bar's rating column hugs this, so the thumbs sit beside
/// the text rather than beside an empty field.
pub fn subtitle_width(ui: &Ui, subtitle: &Subtitle<'_>, tail: Option<&str>) -> f32 {
    let font = theme::regular(12.0);
    subtitle
        .iter()
        .flatten()
        .filter(|p| !p.text.is_empty())
        .copied()
        .chain(tail.map(Part::plain))
        .enumerate()
        .map(|(i, part)| {
            let sep = if i > 0 {
                text_width(ui, part.sep, &font)
            } else {
                0.0
            };
            sep + text_width(ui, part.text, &font)
        })
        .sum()
}

/// The player bar's credits line: like [`paint_subtitle`], but scrolling on hover when the
/// credits are wider than the room — which is exactly the short-title-long-artists case.
///
/// Fits and this *is* `paint_subtitle`: same paint, same hit rects, no state. Overflows and
/// the whole line scrolls ping-pong under a clipped viewport with a dissolve on each cut
/// edge, fading the line's own alpha ([`fade_slices`]) so it holds up over the acrylic shell
/// as well as the opaque one. Link hit rects ride the same offset as the paint, cut to the
/// viewport, so artists stay clickable mid-scroll and a link scrolled out of view cannot be
/// clicked. Whether the line is hovered is geometric — the pointer inside the viewport — not
/// egui's hover flag: the links stacked on the viewport take that flag from it, and the
/// last-frame bookkeeping that used to paper over it could miss a frame under an artist, and
/// one missed frame snapped the scroll home.
#[allow(clippy::too_many_arguments)]
pub fn paint_subtitle_marquee<'a>(
    ui: &mut Ui,
    palette: &Palette,
    pos: egui::Pos2,
    max_width: f32,
    subtitle: &Subtitle<'a>,
    tail: Option<&'a str>,
    id_salt: impl std::hash::Hash + Clone + std::fmt::Debug,
    // See `marquee_line`'s own doc on this parameter.
    row_hovered: bool,
    font: egui::FontId,
    base_colour: Color32,
) -> Option<LinkTarget<'a>> {
    let visible_parts = || {
        subtitle
            .iter()
            .flatten()
            .filter(|p| !p.text.is_empty())
            .copied()
            .chain(tail.map(Part::plain))
    };
    // Measure straight off the iterator: the common case is a subtitle that fits, and it
    // returns here without ever building the `Vec`.
    let mut full = 0.0;
    for (i, part) in visible_parts().enumerate() {
        if i > 0 && !part.sep.is_empty() {
            full += text_width(ui, part.sep, &font);
        }
        full += text_width(ui, part.text, &font);
    }
    if full <= max_width {
        return paint_subtitle(
            ui,
            palette,
            pos,
            pos.x + max_width,
            subtitle,
            tail,
            id_salt,
            font,
            base_colour,
        );
    }

    let parts: Vec<Part> = visible_parts().collect();

    let id = ui.id().with(id_salt);
    let viewport = Rect::from_min_size(
        egui::pos2(pos.x, pos.y - MARQUEE_HIT_H / 2.0),
        vec2(max_width, MARQUEE_HIT_H),
    );
    let over_view = ui.interact(viewport, id, Sense::hover()).contains_pointer();
    let now = ui.input(|i| i.time);
    let mut state: MarqueeState = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    if (state.full_w - full).abs() > 0.5 {
        state = MarqueeState {
            full_w: full,
            dir: 1.0,
            last_t: now,
            ..Default::default()
        };
    }
    let hovered = over_view || row_hovered;
    if hovered {
        if state.hover_since.is_none() {
            state.hover_since = Some(now);
        }
        let since = state.hover_since.unwrap_or(now);
        if now - since >= MARQUEE_DWELL_SECS && now >= state.hold_until {
            // From the end of any wait, not the last frame: with no frames painted through the
            // dwell or a hold, the last one may be long before the text started moving again.
            let moving_since = state
                .last_t
                .max(since + MARQUEE_DWELL_SECS)
                .max(state.hold_until);
            let dt = (now - moving_since).clamp(0.0, 0.25) as f32;
            let max_off = (full - max_width).max(0.0);
            let (offset, dir) = pingpong(state.offset, state.dir, max_off, MARQUEE_SPEED * dt);
            if (dir - state.dir).abs() > f32::EPSILON {
                state.hold_until = now
                    + if dir < 0.0 {
                        MARQUEE_END_HOLD_SECS
                    } else {
                        MARQUEE_HOLD_SECS
                    };
            }
            state.offset = offset;
            state.dir = dir;
        }
        marquee_repaint(ui, now, since, state.hold_until);
    } else {
        state.hover_since = None;
        state.offset = 0.0;
        state.dir = 1.0;
        state.hold_until = 0.0;
    }
    state.last_t = now;
    state.full_w = full;

    let clip = ui.clip_rect().intersect(viewport);
    let depths = dissolve_depths(state.offset, full - max_width);
    let (main, lead, trail) = dissolve_bands(clip, depths, pos.x, pos.x + max_width);
    // Layout and interaction first: the paint below re-runs the layout once per dissolve slice,
    // and the links must not be re-registered for each one. No early break — the clip rect does
    // the cutting, scrolled or not.
    let mut x = pos.x - state.offset;
    let mut clicked = None;
    // (x, text, width, colour, underlined): the underline only while its link is hovered.
    let mut pieces: Vec<(f32, &str, f32, Color32, bool)> = Vec::with_capacity(parts.len() * 2);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 && !part.sep.is_empty() {
            let w = text_width(ui, part.sep, &font);
            pieces.push((x, part.sep, w, palette.dim, false));
            x += w;
        }
        let w = text_width(ui, part.text, &font);
        let mut colour = base_colour;
        let mut underlined = false;
        if let Some(target) = part.target {
            let hit =
                Rect::from_min_size(egui::pos2(x, pos.y - 8.0), vec2(w, 16.0)).intersect(viewport);
            let r = ui.interact(hit, ui.id().with((id, i)), Sense::click());
            if r.hovered() {
                colour = palette.text;
                underlined = true;
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if r.clicked() {
                clicked = Some(target);
            }
        }
        pieces.push((x, part.text, w, colour, underlined));
        x += w;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, state));

    let mut paint = |ui: &mut Ui, slice: Rect| {
        for &(x, text, w, colour, underlined) in &pieces {
            crate::emoji::paint_clipped_line(
                ui,
                slice,
                egui::pos2(x, pos.y),
                text,
                font.clone(),
                colour,
            );
            if underlined {
                ui.painter().with_clip_rect(slice).hline(
                    x..=(x + w),
                    pos.y + 7.0,
                    egui::Stroke::new(1.0, palette.text),
                );
            }
        }
    };
    if main.width() > 0.5 {
        paint(ui, main);
    }
    if let Some(lead) = lead {
        fade_slices(ui, lead, true, depths.0, &mut paint);
    }
    if let Some(trail) = trail {
        fade_slices(ui, trail, false, depths.1, &mut paint);
    }
    clicked
}

/// How long a load may take before its skeleton shows. Anything faster — a page answered from
/// cache — goes straight to its content with no grey flash in between.
const SKELETON_DELAY: f64 = 0.15;
/// One pass of the shimmer across the window.
const SHIMMER_PERIOD: f64 = 1.4;
/// How long a song may take to start before its loading ring shows. A warm resolve (~0.4 s)
/// shows it briefly; a prefetched track (instant) never does.
pub const TRACK_LOADING_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
/// One eased turn of the loading ring.
const SPINNER_PERIOD: f64 = 1.2;

/// How much longer a skeleton waits before showing, given when its load began (`since`, egui
/// time) — `None` once it should show, and at once when no load is being timed.
pub(crate) fn skeleton_wait(now: f64, since: Option<f64>) -> Option<f64> {
    let wait = SKELETON_DELAY - (now - since?);
    (wait > 0.0).then_some(wait)
}

/// Whether a song that has been loading for `waited` should show its ring yet; `Err` carries how
/// long until it should, so the caller can wake up exactly then.
pub(crate) fn track_ring_due(waited: std::time::Duration) -> Result<(), std::time::Duration> {
    if waited >= TRACK_LOADING_DELAY {
        Ok(())
    } else {
        Err(TRACK_LOADING_DELAY - waited)
    }
}

/// Whether the desktop asks for less motion. Read once, at startup, by [`init_reduced_motion`].
static REDUCED_MOTION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn reduced_motion() -> bool {
    REDUCED_MOTION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Read the desktop's "reduce animations" preference once: KDE's global animation speed set to
/// instant (`AnimationDurationFactor=0` in `kdeglobals`), or GNOME's `enable-animations` off.
/// Other platforms keep the default, motion on.
pub(crate) fn init_reduced_motion() {
    #[cfg(target_os = "linux")]
    {
        let kde = directories::BaseDirs::new()
            .and_then(|dirs| std::fs::read_to_string(dirs.config_dir().join("kdeglobals")).ok())
            .is_some_and(|text| kde_animations_off(&text));
        let gnome = || {
            std::process::Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", "enable-animations"])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "false")
        };
        let reduced = kde || gnome();
        if reduced {
            tracing::info!("the desktop asks for less motion; skeletons stay still");
        }
        REDUCED_MOTION.store(reduced, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether `kdeglobals` sets the global animation speed to instant.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn kde_animations_off(kdeglobals: &str) -> bool {
    let mut in_kde = false;
    for line in kdeglobals.lines().map(str::trim) {
        if line.starts_with('[') {
            in_kde = line == "[KDE]";
        } else if in_kde && let Some(value) = line.strip_prefix("AnimationDurationFactor=") {
            return value.trim().parse::<f64>().is_ok_and(|f| f <= 0.0);
        }
    }
    false
}

/// Publish when the page's current load began (egui time), or `None` when nothing is loading,
/// for the skeletons' show delay. Set every frame by the app, like the click model.
pub(crate) fn set_skeleton_since(ctx: &egui::Context, since: Option<f64>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ytm-skeleton-since"), since));
}

fn skeleton_since(ctx: &egui::Context) -> Option<f64> {
    ctx.data(|d| {
        d.get_temp::<Option<f64>>(egui::Id::new("ytm-skeleton-since"))
            .flatten()
    })
}

/// Publish whether the current track has been loading long enough to say so; the active row and
/// the player bar's play button draw the ring off this.
pub(crate) fn set_track_loading(ctx: &egui::Context, loading: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ytm-track-loading"), loading));
}

pub(crate) fn track_loading(ctx: &egui::Context) -> bool {
    ctx.data(|d| {
        d.get_temp::<bool>(egui::Id::new("ytm-track-loading"))
            .unwrap_or(false)
    })
}

/// The ring's rotation at time `t`, in radians: one turn per [`SPINNER_PERIOD`], eased in and out
/// so it surges and settles rather than turning like a clock hand.
pub(crate) fn spinner_angle(t: f64) -> f32 {
    let x = (t / SPINNER_PERIOD).fract();
    let eased = if x < 0.5 {
        2.0 * x * x
    } else {
        1.0 - (-2.0 * x + 2.0).powi(2) / 2.0
    };
    (eased * std::f64::consts::TAU) as f32
}

/// A quarter-circle arc turning around `center`: the one loading glyph, on the play button and on
/// the row that asked to play. Asks for the next frame itself, so it only costs frames while it
/// is on screen.
pub fn paint_spinner(ui: &Ui, center: egui::Pos2, radius: f32, width: f32, color: Color32) {
    let start = spinner_angle(ui.input(|i| i.time));
    let sweep = std::f32::consts::FRAC_PI_2;
    let points: Vec<egui::Pos2> = (0..=16)
        .map(|i| {
            let a = start + sweep * i as f32 / 16.0;
            center + radius * vec2(a.cos(), a.sin())
        })
        .collect();
    ui.painter().add(egui::epaint::PathShape::line(
        points,
        egui::Stroke::new(width, color),
    ));
    ui.ctx().request_repaint();
}

/// Fill one skeleton shape: nothing until [`SKELETON_DELAY`] into a load, then the grey shape with
/// a soft highlight gliding across it. The highlight's position comes from time and the shape's
/// place on screen alone, so every skeleton on the page shimmers as one band, slanted by height.
pub(crate) fn skeleton_fill(
    ui: &Ui,
    rect: Rect,
    radius: impl Into<CornerRadius>,
    palette: &Palette,
) {
    skeleton_fill_in(ui, rect, radius, palette, ui.clip_rect());
}

/// Whether skeletons are showing yet — past [`SKELETON_DELAY`] into the load — for a
/// placeholder that paints something besides [`skeleton_fill`]'s own shapes.
pub fn skeleton_visible(ui: &Ui) -> bool {
    skeleton_wait(ui.input(|i| i.time), skeleton_since(ui.ctx())).is_none()
}

/// [`skeleton_fill`] against a clip of the caller's own — the artist hero's placeholder reaches
/// past the content column to the card's edges, as the hero itself does.
pub fn skeleton_fill_in(
    ui: &Ui,
    rect: Rect,
    radius: impl Into<CornerRadius>,
    palette: &Palette,
    clip: Rect,
) {
    let ctx = ui.ctx();
    let now = ui.input(|i| i.time);
    if let Some(wait) = skeleton_wait(now, skeleton_since(ctx)) {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        return;
    }
    ui.painter().with_clip_rect(clip).rect_filled(
        rect,
        radius,
        palette.glass(palette.surface_hover),
    );
    // Reduced motion: the shapes alone, still, and no frames asked for.
    if reduced_motion() {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(33));

    let screen = ctx.content_rect();
    let band = 240.0;
    let phase = ((now % SHIMMER_PERIOD) / SHIMMER_PERIOD) as f32;
    let centre = screen.left() - band + phase * (screen.width() + 2.0 * band)
        - (rect.center().y - screen.top()) * 0.35;
    let (left, right) = (centre - band / 2.0, centre + band / 2.0);
    if right < rect.left() || left > rect.right() {
        return;
    }
    let glow = palette.text.gamma_multiply(0.07);
    let mut mesh = egui::Mesh::default();
    for (x, colour) in [
        (left, Color32::TRANSPARENT),
        (centre, glow),
        (right, Color32::TRANSPARENT),
    ] {
        mesh.colored_vertex(egui::pos2(x, rect.top()), colour);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), colour);
    }
    for quad in 0..2u32 {
        let i = quad * 2;
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i + 1, i + 3, i + 2);
    }
    ui.painter().with_clip_rect(rect.intersect(clip)).add(mesh);
}

/// Grey placeholder lines of text, for a panel whose content is prose (the lyrics).
pub fn skeleton_lines(ui: &mut Ui, palette: &Palette, n: usize) {
    for i in 0..n {
        let width = ui.available_width() * [0.7, 0.55, 0.62, 0.45][i % 4];
        skeleton_bar(ui, palette, width, 12.0);
        ui.add_space(10.0);
    }
}

/// One grey placeholder bar — the raw material every skeleton below is built from.
fn skeleton_bar(ui: &mut Ui, palette: &Palette, w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    skeleton_fill(ui, rect, CornerRadius::same(theme::radii().small), palette);
}

/// Grey placeholder rows for an unnumbered list (search, history, library songs): the rows
/// [`track_row`] paints there, cover at the left edge and a duration at the right.
pub fn skeleton_rows(ui: &mut Ui, palette: &Palette, n: usize) {
    skeleton_list(ui, palette, n, false, false);
}

/// A song shelf's placeholder in the shape [`crate::ui::song_shelf`] and the artist's Top songs
/// draw it: rows filled down `cols` columns, `rows` deep, at the real grid's gutter.
pub fn skeleton_song_grid(
    ui: &mut Ui,
    palette: &Palette,
    rows: usize,
    cols: usize,
    numbered: bool,
) {
    let width = ui.available_width();
    // The real grid's own gutter (`spec_pages`).
    let gap = 12.0;
    let col_w = (width - gap * (cols - 1) as f32) / cols as f32;
    let (area, _) =
        ui.allocate_exact_size(vec2(width, theme::ROW_HEIGHT * rows as f32), Sense::hover());
    for c in 0..cols {
        for r in 0..rows {
            let rect = Rect::from_min_size(
                area.min + vec2(c as f32 * (col_w + gap), r as f32 * theme::ROW_HEIGHT),
                vec2(col_w, theme::ROW_HEIGHT),
            );
            skeleton_row_at(ui, palette, rect, c * rows + r, numbered, false, false);
        }
    }
}

/// Title and credit bar widths, as fractions of a row's text room, taken in turn down a list:
/// real titles are short and uneven, and one long identical bar per row read as a block rather
/// than as songs.
const SKELETON_TEXT_WIDTHS: [(f32, f32); 5] = [
    (0.18, 0.3),
    (0.12, 0.22),
    (0.24, 0.34),
    (0.15, 0.26),
    (0.2, 0.2),
];

/// One placeholder row painted into `rect`, where [`track_row`] paints its parts: the number
/// (when `numbered`), the cover (unless `hide_art`), title and credit, and the duration (when
/// `duration`). `i` picks the bars' widths.
fn skeleton_row_at(
    ui: &Ui,
    palette: &Palette,
    rect: Rect,
    i: usize,
    numbered: bool,
    hide_art: bool,
    duration: bool,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let small = CornerRadius::same(theme::radii().small);
    let duration_w = 34.0;
    let right_pad = 20.0;
    let num_w = if numbered { theme::ROW_NUMBER_W } else { 0.0 };
    // The row's number is known before its song is, so it is real — dim, as `track_row` sets it
    // — and not a dot. Painted with the placeholders it belongs to, not ahead of them.
    if numbered && skeleton_visible(ui) {
        ui.painter().text(
            egui::pos2(
                rect.left() + theme::ROW_LEFT_INSET + num_w - theme::ROW_NUMBER_GAP,
                rect.center().y,
            ),
            egui::Align2::RIGHT_CENTER,
            (i + 1).to_string(),
            theme::regular(12.0),
            palette.dim,
        );
    }
    let art = Rect::from_min_size(
        egui::pos2(
            rect.left() + theme::ROW_LEFT_INSET + num_w,
            rect.center().y - theme::ART_SIZE / 2.0,
        ),
        vec2(theme::ART_SIZE, theme::ART_SIZE),
    );
    let tx = if hide_art {
        art.left() + 12.0
    } else {
        skeleton_fill(ui, art, small, palette);
        art.right() + 12.0
    };
    let right = if duration {
        rect.right() - right_pad - duration_w - 8.0
    } else {
        rect.right() - 8.0
    };
    let text_w = (right - tx).max(40.0);
    let (title_w, credit_w) = SKELETON_TEXT_WIDTHS[i % SKELETON_TEXT_WIDTHS.len()];
    // On the title's and the credit's own centres (`track_row`: -9.5 and +9.5).
    skeleton_fill(
        ui,
        Rect::from_min_size(
            egui::pos2(tx, rect.center().y - 14.5),
            vec2((text_w * title_w).clamp(70.0, 260.0).min(text_w), 10.0),
        ),
        small,
        palette,
    );
    skeleton_fill(
        ui,
        Rect::from_min_size(
            egui::pos2(tx, rect.center().y + 5.5),
            vec2((text_w * credit_w).clamp(50.0, 380.0).min(text_w), 8.0),
        ),
        small,
        palette,
    );
    if duration {
        skeleton_fill(
            ui,
            Rect::from_min_size(
                egui::pos2(rect.right() - right_pad - duration_w, rect.center().y - 4.5),
                vec2(duration_w, 9.0),
            ),
            small,
            palette,
        );
    }
}

/// Grey placeholder track rows for a numbered list (album, playlist). `hide_art` mirrors the
/// real rows' own flag (the album page, where every cover would be the header's), so the
/// skeleton is the same shape the rows arrive in and the list does not shift when they do.
pub fn skeleton_track_rows(ui: &mut Ui, palette: &Palette, n: usize, hide_art: bool) {
    skeleton_list(ui, palette, n, true, hide_art);
}

fn skeleton_list(ui: &mut Ui, palette: &Palette, n: usize, numbered: bool, hide_art: bool) {
    for i in 0..n {
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), theme::ROW_HEIGHT),
            Sense::hover(),
        );
        skeleton_row_at(ui, palette, rect, i, numbered, hide_art, true);
    }
}

/// Grey placeholder stripe chips in [`crate::ui::mood_chips`]' own grid: `rows` full rows of
/// its column count, flat bars with no art slot.
pub fn skeleton_chips(ui: &mut Ui, palette: &Palette, rows: usize) {
    let (cols, chip_w, chip_h, gap) = crate::ui::mood_chip_grid(ui.available_width());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for _ in 0..cols * rows {
            let (rect, _) = ui.allocate_exact_size(vec2(chip_w, chip_h), Sense::hover());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            skeleton_fill(
                ui,
                rect,
                CornerRadius::same(theme::radii().control),
                palette,
            );
        }
    });
}

/// A row of placeholder filter chips — Home's mood bar and search's scope chips: pills at the
/// chips' own 32 pt height, in a spread of label widths.
pub fn skeleton_pills(ui: &mut Ui, palette: &Palette, n: usize) {
    const WIDTHS: [f32; 6] = [74.0, 66.0, 58.0, 72.0, 80.0, 62.0];
    ui.horizontal_wrapped(|ui| {
        for i in 0..n {
            let (rect, _) =
                ui.allocate_exact_size(vec2(WIDTHS[i % WIDTHS.len()], 32.0), Sense::hover());
            if ui.is_rect_visible(rect) {
                skeleton_fill(ui, rect, theme::pill(rect), palette);
            }
        }
    });
}

/// The shape a [`skeleton_shelf`] stands in for. Each takes the spacing its real shelf takes,
/// so a page of them lands on the real page's rows.
#[derive(Clone, Copy)]
pub enum SkeletonShelfShape {
    /// A carousel (`crate::ui::carousel`): its heading row, arrows included, then one row of
    /// cards fitted to the width. No air above — the caller adds what the page adds.
    Cards(ArtShape),
    /// A `section` heading over unnumbered rows (History's dated shelves).
    Rows,
    /// Home's song shelves (`crate::ui::song_shelf`): air, heading with arrows, rows down
    /// columns.
    SongGrid,
    /// A `section` heading over mood chips, `n` rows of them.
    Chips,
}

/// A whole titled shelf's placeholder: its heading where the real heading goes, then cards,
/// rows or chips. `n` is the rows' or chip rows' count; a card shelf is always one row.
pub fn skeleton_shelf(ui: &mut Ui, palette: &Palette, shape: SkeletonShelfShape, n: usize) {
    match shape {
        SkeletonShelfShape::Cards(art_shape) => {
            skeleton_heading(ui, palette, true);
            ui.add_space(4.0);
            skeleton_card_row(ui, palette, art_shape);
        }
        SkeletonShelfShape::Rows => {
            skeleton_title(ui, palette);
            skeleton_rows(ui, palette, n);
        }
        SkeletonShelfShape::SongGrid => {
            ui.add_space(20.0);
            skeleton_heading(ui, palette, true);
            ui.add_space(4.0);
            let cols = crate::ui::song_shelf_columns(ui.available_width());
            skeleton_song_grid(ui, palette, n, cols, false);
        }
        SkeletonShelfShape::Chips => {
            skeleton_title(ui, palette);
            skeleton_chips(ui, palette, n);
        }
    }
}

/// A heading the page always has, for real: [`skeleton_heading`]'s row with the words on it, drawn
/// with the placeholders around it.
pub fn skeleton_heading_text(ui: &mut Ui, palette: &Palette, text: &str) {
    let font = theme::bold(theme::SECTION_PT);
    let h = ui.fonts_mut(|f| f.row_height(&font));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    if skeleton_visible(ui) && ui.is_rect_visible(rect) {
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            text,
            font,
            palette.text,
        );
    }
}

/// A section heading's placeholder with [`crate::ui::section`]'s own spacing: 20 above, the
/// heading line, 8 below.
pub fn skeleton_title(ui: &mut Ui, palette: &Palette) {
    ui.add_space(20.0);
    skeleton_heading(ui, palette, false);
    ui.add_space(8.0);
}

/// A shelf heading's line: a bar over where a `SECTION_PT` label's capitals sit, in a row as
/// tall as the real one — a carousel's arrows make it an icon button tall.
pub fn skeleton_heading(ui: &mut Ui, palette: &Palette, arrows: bool) {
    let font = theme::bold(theme::SECTION_PT);
    let label_h = ui.fonts_mut(|f| f.row_height(&font));
    // `icon_button` at 14 pt is a 28 pt box.
    let row_h = if arrows { label_h.max(28.0) } else { label_h };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let cap = theme::SECTION_PT * 0.727;
    skeleton_fill(
        ui,
        Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - cap / 2.0 + 1.0),
            vec2(130.0_f32.min(rect.width()), cap),
        ),
        CornerRadius::same(theme::radii().small),
        palette,
    );
}

/// The small line over a shelf's title (`shelf_strapline`), at its own height and pull-up.
pub fn skeleton_strapline(ui: &mut Ui, palette: &Palette) {
    let h = ui.fonts_mut(|f| f.row_height(&theme::medium(11.0)));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    skeleton_fill(
        ui,
        Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - 3.5),
            vec2(40.0_f32.min(rect.width()), 7.0),
        ),
        CornerRadius::same(theme::radii().small),
        palette,
    );
    ui.add_space(-4.0);
}

/// A page title's placeholder, in [`crate::ui::page_title`]'s own spot, for a page whose title
/// arrives with its content (a mood's name, a Home shelf's).
pub fn skeleton_page_title(ui: &mut Ui, palette: &Palette) {
    let font = theme::bold(theme::PAGE_TITLE_PT);
    ui.add_space((theme::PAGE_INSET_X - theme::PAGE_TITLE_PT * 0.18).max(0.0));
    let h = ui.fonts_mut(|f| f.row_height(&font));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let cap = theme::PAGE_TITLE_PT * 0.727;
    skeleton_fill(
        ui,
        Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - cap / 2.0 + 1.0),
            vec2(220.0_f32.min(rect.width()), cap),
        ),
        CornerRadius::same(theme::radii().small),
        palette,
    );
    ui.add_space(10.0);
}

/// Placeholder cards in a wrapped grid, `rows` rows of them, each as wide as
/// [`crate::ui::grid_card_w`] makes the real cards — the library's grids, a mood's playlists, a
/// Home shelf's "Show all".
pub fn skeleton_grid(ui: &mut Ui, palette: &Palette, rows: usize, shape: ArtShape) {
    ui.horizontal_wrapped(|ui| {
        let card_w = crate::ui::grid_card_w(ui);
        let gap = ui.spacing().item_spacing.x;
        let cols = (((ui.available_width() + gap) / (card_w + gap)).floor() as usize).max(1);
        for _ in 0..cols * rows {
            skeleton_card(ui, palette, card_w, shape);
        }
    });
}

/// One row of placeholder cards, sized exactly as [`crate::ui::carousel`] sizes the real ones.
pub fn skeleton_card_row(ui: &mut Ui, palette: &Palette, shape: ArtShape) {
    let gap = 8.0;
    let (n, card_w) = crate::ui::carousel_fit(ui.available_width(), gap, super::ui::CARD_W);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for _ in 0..n {
            skeleton_card(ui, palette, card_w, shape);
        }
    });
}

/// One placeholder card: cover, then a title and a subtitle bar, in [`card_size`]'s footprint.
fn skeleton_card(ui: &mut Ui, palette: &Palette, width: f32, shape: ArtShape) {
    let size = card_size(width, shape);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let art = Rect::from_min_size(
        rect.min + vec2(8.0, 8.0),
        vec2(width - 16.0, card_art_height(width, shape)),
    );
    // The art's own curve, so the corner does not change when the cover lands.
    skeleton_fill(ui, art, shape.radius(art), palette);
    for (dy, w) in [(14.0, 0.7), (28.0, 0.45)] {
        skeleton_fill(
            ui,
            Rect::from_min_size(
                art.left_bottom() + vec2(0.0, dy),
                vec2((width - 16.0) * w, 9.0),
            ),
            CornerRadius::same(theme::radii().small),
            palette,
        );
    }
}

/// Whether a row's rating thumbs share the duration's corner, given the row's width and what it
/// carries. They do only where there is no room for their own zone *and* they can actually be
/// drawn: a ratable, signed-in row. A row with no rating (the queue) or no session has no thumbs
/// to put there, so it keeps its duration at any width — see `track_row`'s own comment.
fn corner_shares_duration(width: f32, show_rating: bool, has_menu: bool, signed_in: bool) -> bool {
    width < 520.0 && show_rating && has_menu && signed_in
}

/// The type label a mixed list's row carries ("Artist", "Playlist"): quiet, but readable.
fn badge_font() -> egui::FontId {
    theme::medium(11.0)
}

/// A single track row: number (optional), art, title with an explicit badge, artist with play
/// count, duration.
///
/// Returns the response so the caller decides what a click means.
pub fn track_row<'a>(ui: &mut Ui, palette: &Palette, row: TrackRow<'a>) -> RowClick<'a> {
    let TrackRow {
        number,
        rank,
        trend,
        title,
        album_id,
        subtitle,
        duration,
        plays,
        explicit,
        active,
        playing,
        badge,
        art,
        shape,
        hide_art,
        id_salt,
        menu,
        like,
        show_rating,
        quiet_like,
        add_to,
    } = row;
    let mut link = None;
    // `click_and_drag`, not `click`: a row inside the open playlist's own editable track list
    // can be dragged to reorder it (see `spec_rows`, the only place that ever reads a drag
    // start off this response). Harmless everywhere else — nothing reads it unless that row
    // came from an editable playlist, and egui only treats a press as a drag once the pointer
    // has actually moved, so an ordinary click is unaffected.
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT),
        Sense::click_and_drag(),
    );
    let right_pad = 20.0;
    // A narrow row — the open queue on a small window — compresses instead of starving:
    // the number column and the art yield first, and what they give back is the title's
    // room. A full-size art and a 30pt number column left 21pt of it on a 215pt card,
    // which is one ellipsis. Decided here, because the duration slot below leans on it.
    let narrow = rect.width() < 340.0;
    // Below this the hover thumbs stop getting a reserved zone of their own and are meant to take
    // the duration's corner instead, the same fallback the narrow queue rows already used. A zone
    // costs 68pt, and a row this size cannot pay it without clipping its own subtitle — the
    // artist page's side-by-side "Popular" column is exactly this width.
    let corner_thumbs = rect.width() < 520.0;
    // Whether the thumbs really do share that corner. They can only where they will be drawn:
    // a ratable, signed-in row. Where they cannot — the queue, a signed-out list — the duration
    // is not stepping aside for anything and stays put at every width.
    let share_corner = corner_shares_duration(
        rect.width(),
        show_rating,
        menu.is_some(),
        menu.as_ref().is_some_and(|m| m.signed_in),
    );
    // Measured, not constant: an hour-long mix ("1:02:05") is wider than the 44pt a
    // "3:33" needs, and a fixed slot grazes the title in the narrow queue. Nothing
    // painted, nothing reserved beyond the old minimum. Where the corner is shared the slot
    // is the thumbs' — the pair spans 62pt, so the text must clear it whether or not a thumb
    // is showing this frame.
    // A row with neither a duration nor a badge — Home's song shelves, whose songs come without
    // a length — keeps no empty slot for one: the 44pt it held, plus the thumbs' zone beside it,
    // left a single wide column with a gap at its right end and the title cut short before it.
    let duration_w = match (duration, badge) {
        (Some(d), _) => (text_width(ui, &fmt_duration(d), &theme::regular(12.0)) + 12.0).max(44.0),
        (None, Some(b)) => (text_width(ui, b, &badge_font()) + 12.0).max(44.0),
        (None, None) => 0.0,
    }
    .max(if share_corner { 76.0 } else { 0.0 });
    // Reserved whether or not the thumbs are drawn, so a row does not re-truncate its title as
    // the pointer crosses it — but only where they get a zone of their own. The queue turns
    // rating off, and giving that panel back its 68pt is the difference between "Jigsaw Fallin…"
    // and a readable title in a 300pt rail.
    // Wide enough for the pair: two 30pt buttons at offsets 50/18 span 62pt, and anything
    // narrower paints the Like button over the title's tail.
    let thumb_w = if add_to.is_some() {
        ADD_W
    } else if show_rating && !corner_thumbs {
        68.0
    } else {
        0.0
    };

    // A narrow row — the open queue on a small window — compresses instead of starving: the
    // number column and the art yield first, and what they give back is the title's room. A
    // full-size art and a 30pt number column left 21pt of it on a 215pt card, which is one
    // ellipsis. Computed out here, before the cover's hit region is decided and before anything
    // is painted, so the two cannot disagree about where the cover is.
    let num_w = if (number.is_some() || rank.is_some()) && !narrow {
        theme::ROW_NUMBER_W
    } else {
        0.0
    };
    let row_art = if narrow { 32.0 } else { theme::ART_SIZE };
    // A video still keeps its 16:9 aspect instead of being squeezed into the square slot.
    // Hidden art leaves a zero-width slot rather than its own branch: the number column and
    // the title's own 12pt gap then place the text exactly as they would beside a cover.
    let art_w = if hide_art {
        0.0
    } else if shape == ArtShape::Wide {
        row_art * 16.0 / 9.0
    } else {
        row_art
    };
    let art_rect = Rect::from_center_size(
        rect.left_center() + vec2(theme::ROW_LEFT_INSET + num_w + art_w / 2.0, 0.0),
        vec2(art_w, row_art),
    );

    // Geometric, not `hovered()`: children stacked on top (links, thumbs) steal the hover
    // flag, which would blink the highlight out as the pointer crosses them.
    let over = response.contains_pointer();
    let t = fade(ui, response.id.with("row"), over);
    // A hidden cover leaves its play control behind in the number's slot: the leading column
    // — where the hover glyph appears — plays on a single click, exactly as the cover did, so
    // double-click-to-play keeps the same one exception it always had.
    let number_play = hide_art && !narrow && number.is_some() && menu.is_some();
    let number_rect = Rect::from_min_size(
        egui::pos2(rect.left() + theme::ROW_LEFT_INSET, rect.top()),
        vec2(theme::ROW_NUMBER_W, rect.height()),
    );
    // Double-click-to-play: the row's own track is the id to select or play. A row without a
    // menu (the queue's current row) is not a selectable list item, so it keeps the plain click.
    // The cover is exempt: it is the hover play button's own control, so a click on it plays on a
    // single click whatever the setting says — and short-circuiting `activate` is what keeps that
    // click from also marking the row as merely selected. A hidden slot has no cover to press
    // (and a zero-width rect would otherwise swallow a click landing exactly on its line).
    let activate = if (!hide_art
        && on_cover(
            shape,
            art_rect,
            response.interact_pointer_pos(),
            response.clicked(),
        ))
        || (number_play
            && response.clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|pos| number_rect.contains(pos)))
    {
        true
    } else {
        match menu.as_ref() {
            Some(m) => activate(ui.ctx(), &response, m.track.video_id.as_str()),
            None => response.clicked(),
        }
    };
    let selected = menu
        .as_ref()
        .is_some_and(|m| is_selected(ui.ctx(), m.track.video_id.as_str()));
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(theme::radii().control);
        // The selection (a single click in double-click mode) sits under the hover fill, so the
        // marked row reads at rest and the pointer still lights it.
        if selected {
            ui.painter().rect_filled(
                rect,
                radius,
                palette.glass_state(palette.surface_active, 0.55),
            );
        }
        if t > 0.001 {
            ui.painter()
                .rect_filled(rect, radius, palette.glass_state(palette.surface_hover, t));
        }

        // The row that asked to play shows it loading: over its cover, or — on a page whose
        // rows have none (the album) — in place of its number.
        let loading = active && track_loading(ui.ctx());
        if !hide_art {
            paint_art(ui, palette, art_rect, art, shape);
            if loading {
                ui.painter().rect_filled(
                    art_rect,
                    shape.radius(art_rect),
                    Color32::from_black_alpha(120),
                );
                paint_spinner(
                    ui,
                    art_rect.center(),
                    art_rect.width() * 0.28,
                    2.0,
                    Color32::WHITE,
                );
            } else if shape == ArtShape::Square {
                // A cover you can press is a control; one you cannot is a picture.
                play_overlay(ui, palette, art_rect, t, false, playing, art_rect);
            }
        }

        if !narrow && let Some(rank) = rank {
            // A chart rank takes the number's slot; the movement rides beside it.
            let (arrow, fg) = match trend {
                crate::model::ChartTrend::Up => (Some(Icon::TrendingUp), palette.accent),
                crate::model::ChartTrend::Down => (Some(Icon::TrendingDown), palette.danger),
                crate::model::ChartTrend::Steady => (None, palette.dim),
                crate::model::ChartTrend::Unknown => (None, palette.dim),
            };
            let label_x = rect.left() + theme::ROW_LEFT_INSET + num_w - theme::ROW_NUMBER_GAP;
            ui.painter().text(
                egui::pos2(label_x, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                rank,
                theme::semibold(12.0),
                fg,
            );
            if let Some(arrow) = arrow {
                let w = text_width(ui, rank, &theme::semibold(12.0));
                arrow.image(fg, 11.0).paint_at(
                    ui,
                    Rect::from_min_size(
                        // Clamped: a rank's arrow sits left of the number, and the number now
                        // hugs the row's left edge, so without this a two-digit rank would push
                        // the arrow off the row.
                        egui::pos2(
                            (label_x - w - 15.0).max(rect.left() + 1.0),
                            rect.center().y - 5.5,
                        ),
                        vec2(11.0, 11.0),
                    ),
                );
            }
        } else if !narrow && let Some(n) = number {
            if loading && hide_art {
                paint_spinner(ui, number_rect.center(), 7.0, 2.0, palette.accent);
            } else if number_play && over {
                // The cover's own hover play button went with the cover, so the affordance
                // moves into the number's slot — the pointer turns the number into a play
                // (or pause, on the row that is already going) glyph. Painted in the accent
                // the cover's button used, so the two read as the same control.
                let glyph = 14.0;
                let icon = if playing {
                    Icon::PauseFilled
                } else {
                    Icon::PlayFilled
                };
                // The play triangle's visual centre sits left of its bounding box; Pause is
                // symmetric and needs no nudge.
                let nudge = if playing { 0.0 } else { glyph * 0.06 };
                icon.image(palette.accent, glyph).paint_at(
                    ui,
                    Rect::from_center_size(
                        egui::pos2(
                            rect.left() + theme::ROW_LEFT_INSET + num_w / 2.0 + nudge,
                            rect.center().y,
                        ),
                        vec2(glyph, glyph),
                    ),
                );
            } else {
                ui.painter().text(
                    egui::pos2(
                        rect.left() + theme::ROW_LEFT_INSET + num_w - theme::ROW_NUMBER_GAP,
                        rect.center().y,
                    ),
                    egui::Align2::RIGHT_CENTER,
                    n.to_string(),
                    theme::regular(12.0),
                    palette.dim,
                );
            }
        }

        let text_x = art_rect.right() + 12.0;

        // Explicit badge first, at a fixed spot so the title never shifts when art loads.
        let mut title_x = text_x;
        if explicit {
            let badge =
                Rect::from_min_size(egui::pos2(text_x, rect.center().y - 17.0), vec2(15.0, 15.0));
            ui.painter().rect(
                badge,
                CornerRadius::same(theme::radii().small),
                Color32::TRANSPARENT,
                egui::Stroke::new(1.0, palette.dim),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                "E",
                theme::semibold(9.0),
                palette.dim,
            );
            title_x += 19.0;
        }
        let text_w = (rect.right() - right_pad - duration_w - thumb_w - title_x).max(0.0);

        let title_color = if active {
            palette.accent_text()
        } else {
            palette.text
        };
        // A long title scrolls on hover instead of hard-truncating; a title that fits is painted
        // plainly. Hover-only sense, so the row's own click is never stolen. Its cut-off fades
        // the text's own alpha, so no background colour is needed — the row's hover highlight
        // shows through the dissolve instead of being guessed at.
        // A one-part "subtitle" so the title can carry the same album link the artist credit
        // below it already can, through the exact machinery that lets that link coexist with
        // clicking elsewhere on the row to play it (`ListClick::Link` vs `ListClick::Row`).
        let mut title_parts = no_subtitle();
        title_parts[0] = Some(match album_id {
            Some(id) => Part::link(title, LinkTarget::Album(id)),
            None => Part::plain(title),
        });
        if let Some(target) = paint_subtitle_marquee(
            ui,
            palette,
            egui::pos2(title_x, rect.center().y - 9.5),
            text_w,
            &title_parts,
            None,
            ("ytm-row-title", id_salt),
            over,
            theme::medium(theme::ROW_TITLE_PT),
            title_color,
        ) {
            link = Some(target);
        }
        // Subtitle pieces, laid out left to right so each linked piece gets its own hit area.
        // The play-count tail rides the same line and scrolls with it. The pair's ±9.5 split
        // (was ±9 at the old 13pt title) keeps the same gap between the two lines.
        let sub_right = rect.right() - right_pad - duration_w - thumb_w;
        let sub_w = (sub_right - text_x).max(0.0);
        if let Some(target) = paint_subtitle_marquee(
            ui,
            palette,
            egui::pos2(text_x, rect.center().y + 9.5),
            sub_w,
            &subtitle,
            plays,
            ("ytm-row-link", id_salt),
            over,
            theme::regular(12.0),
            palette.secondary,
        ) {
            link = Some(target);
        }

        // Duration when there is one, otherwise the type badge. They occupy the same slot because
        // a row has one or the other: only songs carry a duration, only mixed lists need a badge.
        // Where the corner is shared with the rating thumbs it holds the thumb and nothing else —
        // for *every* row, so a list never mixes rows that show a duration with rows that show a
        // thumb. (Letting unrated rows keep the duration was exactly that inconsistency: rated
        // rows lost it, unrated ones did not, purely by width.) With no corner to share — wide
        // rows, the queue, signed-out lists — the duration is always painted.
        if !share_corner {
            if let Some(d) = duration {
                ui.painter().text(
                    egui::pos2(rect.right() - right_pad, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    fmt_duration(d),
                    theme::regular(12.0),
                    palette.dim,
                );
            } else if let Some(badge) = badge {
                ui.painter().text(
                    egui::pos2(rect.right() - right_pad, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    badge,
                    badge_font(),
                    palette.dim,
                );
            }
        }
    }

    // The menu lives outside the visibility check: allocation and interaction happen whether
    // the row is painted or not, and a menu opened at the pointer must survive the row
    // scrolling out from under it.
    let video_id = add_to.and(menu.as_ref()).map(|m| m.track.video_id.clone());
    let ratable = menu
        .as_ref()
        .filter(|_| show_rating)
        .map(|m| (m.track.video_id.clone(), m.signed_in));
    let mut action = menu.and_then(|m| row_menu(ui, palette, &response, m));
    // Rating thumbs are interacted whenever the row is visible — never gated on drawn — for
    // the reason documented on `rate_thumbs`. A click is only reachable where they are
    // visible anyway, since their zone sits inside the row.
    if ui.is_rect_visible(rect)
        && let Some((id, true)) = ratable
    {
        // Wide rows reserve a 68pt zone left of the duration and anchor the thumbs there.
        // Corner-sharing rows have no zone to anchor to — the thumbs take the duration's corner
        // instead, where the duration itself has stepped aside for them.
        let thumb_right = if share_corner {
            rect.right() - 17.0
        } else {
            rect.right() - right_pad - duration_w
        };
        if let Some(rate) = rate_thumbs(
            ui,
            palette,
            rect,
            thumb_right,
            like,
            &id,
            id_salt,
            over,
            quiet_like,
        ) {
            action = Some(rate);
        }
    }
    if let (Some(playlist_id), Some(video_id)) = (add_to, video_id) {
        let hit_rect = Rect::from_center_size(
            egui::pos2(
                rect.right() - right_pad - duration_w - ADD_W / 2.0,
                rect.center().y,
            ),
            vec2(30.0, 30.0),
        );
        if add_button(
            ui,
            palette,
            hit_rect,
            ui.id().with(("ytm-row-add", id_salt)),
        )
        .clicked()
        {
            action = Some(super::ui::RowAction::SaveToPlaylist {
                playlist_id: playlist_id.to_owned(),
                video_id,
            });
        }
    }
    RowClick {
        response,
        link,
        activate,
        action,
    }
}

/// The room a row's + button takes, left of the duration.
const ADD_W: f32 = 40.0;

/// A row's round + button: always drawn, since adding is the whole point of the row it sits on.
fn add_button(ui: &mut Ui, palette: &Palette, rect: Rect, id: egui::Id) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click());
    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        let backing = if held(&response) {
            Some(palette.glass(palette.surface_active))
        } else {
            (t > 0.001).then(|| palette.glass_state(palette.surface_hover, t))
        };
        if let Some(fill) = backing {
            ui.painter().circle_filled(rect.center(), 15.0, fill);
        }
        let size = 18.0;
        Icon::Plus
            .image(mix(palette.secondary, palette.text, t), size)
            .paint_at(
                ui,
                Rect::from_center_size(rect.center() + press_offset(&response), vec2(size, size)),
            );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.hint("Add to playlist")
}

/// A song's right-click menu, reporting the choice. Shared by every row and the player bar.
///
/// The popup id is the video id, not the row index: indices shift as the virtualised range
/// moves, which would close the menu on scroll — or worse, re-anchor it onto another row.
/// Two rows for the same track share one menu, which is the same menu, so that is harmless.
pub(crate) fn row_menu(
    ui: &mut Ui,
    palette: &Palette,
    response: &egui::Response,
    menu: RowMenu<'_>,
) -> Option<super::ui::RowAction> {
    use super::ui::RowAction;
    let t = menu.track;
    let popup = egui::Popup::new(
        ui.id().with(("ytm-row-menu", t.video_id.as_str())),
        ui.ctx().clone(),
        response,
        ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .frame(menu_frame(ui, palette))
    .layout(egui::Layout::top_down_justified(egui::Align::Min))
    .width(MENU_WIDTH)
    .gap(0.0)
    // The default is `CloseOnClick`, which closes on *any* click anywhere — including one
    // landing inside the menu, so a click on the submenu's scrollbar tore the whole thing down.
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .open_memory(if response.secondary_clicked() {
        Some(egui::SetOpenCommand::Bool(true))
    } else if response.clicked() {
        // A primary click both plays and dismisses, as on the site.
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    })
    .at_pointer_fixed();
    popup
        .show(|mui| {
            let mut choice = None;

            // Block one: what to do with it now. No title header — you know which row you
            // right-clicked, and a truncated one only ate a row's worth of height.
            if menu_item(mui, palette, Icon::Radio, "Start mix", None).clicked() {
                choice = Some(RowAction::StartMix(t.video_id.clone()));
                mui.close();
            }
            if menu_item(mui, palette, Icon::ListStart, "Play next", None).clicked() {
                choice = Some(RowAction::PlayNext(t.clone()));
                mui.close();
            }
            if menu_item(mui, palette, Icon::ListPlus, "Add to queue", None).clicked() {
                choice = Some(RowAction::AddToQueue(t.clone()));
                mui.close();
            }
            if let Some(i) = menu.queue_index
                && menu_item(mui, palette, Icon::ListX, "Remove from queue", None).clicked()
            {
                choice = Some(RowAction::RemoveFromQueue(i));
                mui.close();
            }

            // Block two: where it lives. Save to library / Remove from library, when the row
            // carries the token for the direction it needs. Signed-out rows carry no tokens,
            // so the item hides itself.
            let saving = !t.in_library;
            let token = if saving {
                &t.library_add
            } else {
                &t.library_remove
            };
            // Only the playlists you can actually add to, and only ones of the same source as
            // this track — YouTube's API has no way to accept a local file, and a local
            // playlist has nowhere to send a YouTube video id, so offering the other source
            // would be an option that always fails. Liked Music is an auto playlist and
            // someone else's saved playlist is not yours to edit either; both would have been
            // offered and both would have been refused by YouTube.
            let wanted_source = if t.local_path.is_some() {
                crate::model::PlaylistSource::Local
            } else {
                crate::model::PlaylistSource::YouTube
            };
            let targets: Vec<&crate::model::PlaylistRef> = menu
                .playlists
                .iter()
                .filter(|p| p.editable && p.source == wanted_source)
                .collect();
            let removable = menu.playlist_id.zip(t.set_video_id.as_deref());
            // A download item shows for every YouTube song, so the block is there whenever the
            // song is one, or has a library or playlist move to offer.
            if token.is_some()
                || t.local_path.is_none()
                || !targets.is_empty()
                || removable.is_some()
            {
                menu_divider(mui, palette);
            }
            if token.is_some()
                && menu_item(
                    mui,
                    palette,
                    Icon::Bookmark,
                    if saving {
                        "Save to library"
                    } else {
                        "Remove from library"
                    },
                    None,
                )
                .clicked()
            {
                choice = Some(RowAction::ToggleLibrary(t.clone()));
                mui.close();
            }
            // Offline: a downloaded song offers removal, any other offers saving to disk. A
            // user's own local file is already on disk, so neither applies.
            if t.local_path.is_none() {
                let downloaded = is_downloaded(mui.ctx(), &t.video_id);
                // Saving needs Premium, as it does on YouTube; removing a file never does.
                let allowed = downloaded || premium(mui.ctx());
                let item = mui
                    .add_enabled_ui(allowed, |mui| {
                        menu_item(
                            mui,
                            palette,
                            if downloaded {
                                Icon::Trash2
                            } else {
                                Icon::Download
                            },
                            if downloaded {
                                "Remove download"
                            } else {
                                "Download"
                            },
                            None,
                        )
                    })
                    .inner
                    .on_disabled_hover_text(DOWNLOAD_NEEDS_PREMIUM);
                if item.clicked() {
                    choice = Some(if downloaded {
                        RowAction::RemoveDownload(t.video_id.clone())
                    } else {
                        RowAction::Download(t.clone())
                    });
                    mui.close();
                }
            }
            if !targets.is_empty()
                && let Some(playlist_id) = sub_menu(
                    mui,
                    palette,
                    Icon::Plus,
                    "Save to playlist",
                    mui.id().with(("ytm-save-to", t.video_id.as_str())),
                    |sui| {
                        let mut picked = None;
                        egui::ScrollArea::vertical()
                            .id_salt("save-to-scroll")
                            .max_height(320.0)
                            .show(sui, |sui| {
                                for pl in targets {
                                    if menu_item_plain(sui, palette, &pl.title, None).clicked() {
                                        picked = Some(pl.id.clone());
                                    }
                                }
                            });
                        picked
                    },
                )
            {
                choice = Some(RowAction::SaveToPlaylist {
                    playlist_id,
                    video_id: t.video_id.clone(),
                });
                mui.close();
            }
            // Remove from playlist, only on an editable page and only where the item id is
            // actually present — a wrong assumption here corrupts a playlist, so both gates
            // must pass.
            if let Some((playlist_id, set_video_id)) = removable
                && menu_item(mui, palette, Icon::ListX, "Remove from playlist", None).clicked()
            {
                choice = Some(RowAction::RemoveFromPlaylist {
                    playlist_id: playlist_id.to_owned(),
                    video_id: t.video_id.clone(),
                    set_video_id: set_video_id.to_owned(),
                });
                mui.close();
            }

            // Block three: where it came from, and sending it on.
            if t.album_id.is_some() || t.primary_artist_id().is_some() || t.local_path.is_none() {
                menu_divider(mui, palette);
            }
            if let Some(id) = &t.album_id
                && menu_item(mui, palette, Icon::Disc3, "Go to album", None).clicked()
            {
                choice = Some(RowAction::GoToAlbum(id.clone()));
                mui.close();
            }
            if let Some(id) = t.primary_artist_id()
                && menu_item(mui, palette, Icon::CircleUser, "Go to artist", None).clicked()
            {
                choice = Some(RowAction::GoToArtist(id.to_owned()));
                mui.close();
            }
            // There is no real YouTube URL for a local track to copy.
            if t.local_path.is_none()
                && menu_item(mui, palette, Icon::Link, "Copy link", None).clicked()
            {
                choice = Some(RowAction::CopyLink(t.video_id.clone()));
                mui.close();
            }
            choice
        })
        .and_then(|r| r.inner)
}

/// What a page header's ••• menu is built from: the whole loaded list, not one track.
///
/// Queueing and saving act on every loaded track — a paged playlist only has its loaded pages —
/// and every capability gates itself on the data, so a signed-out page offers no save picker
/// and a description-less page offers no Info.
pub struct PageMenu<'a> {
    /// Stable popup id: the album or playlist id. Row menus key on video id for the same
    /// reason — indices shift, ids do not.
    pub page_id: &'a str,
    pub tracks: &'a [crate::model::Track],
    /// The artist the page is by, when the source linked one. Enables "Go to artist".
    pub artist_id: Option<&'a str>,
    /// Your playlists, for the "Save to playlist" picker. Empty when signed out.
    pub playlists: &'a [crate::model::PlaylistRef],
    pub signed_in: bool,
    /// A `watch?v=` URL is wrong for a page; the caller builds the right one.
    pub share_url: String,
    /// The page has a description worth showing. Enables "Info".
    pub has_info: bool,
    /// Set on an owned playlist page: enables "Edit playlist" and "Delete playlist".
    pub manage: bool,
    /// A saved (not owned) playlist in the rail: enables "Remove from library". The pages
    /// already carry a Save/Remove chip, so they leave this false.
    pub remove_from_library: bool,
    /// Opened from a right-click (the sidebar rail) rather than a ••• button: only a secondary
    /// click opens it, and a left click (which navigates) closes it instead of toggling.
    pub context_only: bool,
    /// The playlist/album id to seed a page "Start mix" from (`RDAMPL`), when the page has one.
    /// For an album this is its `audioPlaylistId`, not the browse id. `None` falls back to the
    /// old first-song mix, which is all a page without a real playlist id (a folder) can do.
    pub radio_id: Option<&'a str>,
    /// Offers "Save as playlist" first, above everything else, seeded with the whole page and
    /// defaulting to this name — the Local tab's folder cards, which aren't playlists until
    /// this is actually chosen. `None` everywhere else; the existing "Save to playlist"
    /// (`targets`, below) already covers saving into a playlist that exists.
    pub save_as_new: Option<&'a str>,
    /// The page's songs are still being fetched (a sidebar playlist or a card, right-clicked a
    /// moment ago), and this is what kind of page they belong to. The menu shows every item it
    /// will have once they land, so it opens complete; an item picked before then is reported
    /// as [`super::ui::RowAction::PageWhenLoaded`] and done when they arrive.
    pub(crate) tracks_pending: Option<super::ui::PageKind>,
}

/// A page header's ••• menu, reporting the choice.
///
/// Four groups, divided: playing it (mix, shuffle, queueing), keeping it (save as playlist,
/// download, save to playlist), about it (artist, link, info), and changing it (edit, delete,
/// remove). Mix leads — starting a mix is the one worth reaching for first.
pub(crate) fn page_menu(
    ui: &mut Ui,
    palette: &Palette,
    response: &egui::Response,
    menu: PageMenu<'_>,
) -> Option<super::ui::RowAction> {
    use super::ui::RowAction;
    let mut popup = egui::Popup::new(
        ui.id().with(("ytm-page-menu", menu.page_id)),
        ui.ctx().clone(),
        response,
        ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .frame(menu_frame(ui, palette))
    .layout(egui::Layout::top_down_justified(egui::Align::Min))
    .width(MENU_WIDTH)
    .gap(0.0);
    // A right-click menu belongs at the pointer, like every row menu; a ••• button keeps the
    // popup anchored to itself. `context_only` is exactly "opened from a right-click".
    if menu.context_only {
        popup = popup.at_pointer_fixed();
    }
    // Toggle on primary click: a ••• button opens on a primary click, so its popup cannot use
    // the context-menu pattern (primary closes), which would force it shut on the opening click.
    popup
        .open_memory(if response.secondary_clicked() {
            Some(egui::SetOpenCommand::Bool(true))
        } else if menu.context_only {
            // A context menu: the left click navigates (or selects), so it only closes the menu.
            response
                .clicked()
                .then_some(egui::SetOpenCommand::Bool(false))
        } else {
            response.clicked().then_some(egui::SetOpenCommand::Toggle)
        })
        .show(|mui| {
            let mut choice = None;
            // Cloned on pick, not per frame: the menu borrows the page's tracks.
            let tracks = || menu.tracks.to_vec();
            // Songs still on their way count as songs: the items show now and wait for them.
            let pending = menu
                .tracks
                .is_empty()
                .then_some(menu.tracks_pending)
                .flatten();
            let later = |then: super::ui::PageThen| {
                pending.map(|kind| RowAction::PageWhenLoaded {
                    page_id: menu.page_id.to_owned(),
                    kind,
                    then,
                })
            };
            let has_tracks = !menu.tracks.is_empty() || pending.is_some();
            // Only the playlists you can actually add to, and only ones of the same source —
            // same rule as a row's picker, and for the same reason. Signed-out pages carry no
            // playlists, so the item hides itself with no auth check.
            let wanted_source = if menu.tracks.iter().any(|t| t.local_path.is_some()) {
                crate::model::PlaylistSource::Local
            } else {
                crate::model::PlaylistSource::YouTube
            };
            let targets: Vec<&crate::model::PlaylistRef> = menu
                .playlists
                .iter()
                .filter(|p| p.editable && p.source == wanted_source)
                .collect();
            // Offline: save the whole page to disk. Only for YouTube tracks — a folder of local
            // files is already on disk.
            let downloadable = has_tracks && menu.tracks.iter().all(|t| t.local_path.is_none());
            let mut groups = MenuGroups::default();

            // Playing it.
            groups.start(mui, palette, menu.radio_id.is_some() || has_tracks);
            // A page with a playlist/album id starts a mix of the whole thing; only a page
            // without one (a folder) falls back to seeding from the first song.
            if let Some(radio) = menu.radio_id {
                if menu_item(mui, palette, Icon::Radio, "Start mix", None).clicked() {
                    choice = Some(RowAction::StartMixPlaylist(radio.to_owned()));
                    mui.close();
                }
            } else if let Some(first) = menu.tracks.first() {
                if menu_item(mui, palette, Icon::Radio, "Start mix", None).clicked() {
                    choice = Some(RowAction::StartMix(first.video_id.clone()));
                    mui.close();
                }
            } else if pending.is_some()
                && menu_item(mui, palette, Icon::Radio, "Start mix", None).clicked()
            {
                // An album card's radio id comes with the album itself.
                choice = later(super::ui::PageThen::StartMix);
                mui.close();
            }
            if has_tracks {
                if menu_item(mui, palette, Icon::Shuffle, "Shuffle play", None).clicked() {
                    choice = later(super::ui::PageThen::Shuffle)
                        .or_else(|| Some(RowAction::PageShuffle(tracks())));
                    mui.close();
                }
                if menu_item(mui, palette, Icon::ListStart, "Play next", None).clicked() {
                    choice = later(super::ui::PageThen::PlayNext)
                        .or_else(|| Some(RowAction::PagePlayNext(tracks())));
                    mui.close();
                }
                if menu_item(mui, palette, Icon::ListPlus, "Add to queue", None).clicked() {
                    choice = later(super::ui::PageThen::Queue)
                        .or_else(|| Some(RowAction::PageQueue(tracks())));
                    mui.close();
                }
            }

            // Keeping it.
            let save_as_new = menu.save_as_new.filter(|_| has_tracks);
            groups.start(
                mui,
                palette,
                save_as_new.is_some() || downloadable || (has_tracks && !targets.is_empty()),
            );
            if let Some(title) = save_as_new
                && menu_item(mui, palette, Icon::ListPlus, "Save as playlist", None).clicked()
            {
                choice = Some(RowAction::PageSaveAsNewPlaylist {
                    title: title.to_owned(),
                    video_ids: menu.tracks.iter().map(|t| t.video_id.clone()).collect(),
                });
                mui.close();
            }
            if downloadable {
                let allowed = premium(mui.ctx());
                let item = mui
                    .add_enabled_ui(allowed, |mui| {
                        menu_item(mui, palette, Icon::Download, "Download all", None)
                    })
                    .inner
                    .on_disabled_hover_text(DOWNLOAD_NEEDS_PREMIUM);
                if item.clicked() {
                    choice = later(super::ui::PageThen::DownloadAll)
                        .or_else(|| Some(RowAction::DownloadMany(tracks())));
                    mui.close();
                }
            }
            if !targets.is_empty()
                && has_tracks
                && let Some((playlist_id, title)) = sub_menu(
                    mui,
                    palette,
                    Icon::Plus,
                    "Save to playlist",
                    mui.id().with(("ytm-page-save-to", menu.page_id)),
                    |sui| {
                        let mut picked = None;
                        egui::ScrollArea::vertical()
                            .id_salt(("ytm-page-save-scroll", menu.page_id))
                            .max_height(320.0)
                            .show(sui, |sui| {
                                for pl in targets {
                                    if menu_item_plain(sui, palette, &pl.title, None).clicked() {
                                        picked = Some((pl.id.clone(), pl.title.clone()));
                                    }
                                }
                            });
                        picked
                    },
                )
            {
                choice = match later(super::ui::PageThen::SaveToPlaylist {
                    playlist_id: playlist_id.clone(),
                    title: title.clone(),
                }) {
                    Some(deferred) => Some(deferred),
                    None => Some(RowAction::PageSaveToPlaylist {
                        playlist_id,
                        title,
                        video_ids: menu.tracks.iter().map(|t| t.video_id.clone()).collect(),
                    }),
                };
                mui.close();
            }

            // About it, and passing it on.
            groups.start(
                mui,
                palette,
                menu.artist_id.is_some() || !menu.share_url.is_empty() || menu.has_info,
            );
            if let Some(id) = menu.artist_id
                && menu_item(mui, palette, Icon::CircleUser, "Go to artist", None).clicked()
            {
                choice = Some(RowAction::GoToArtist(id.to_owned()));
                mui.close();
            }
            if !menu.share_url.is_empty()
                && menu_item(mui, palette, Icon::Link, "Copy link", None).clicked()
            {
                choice = Some(RowAction::PageCopyLink(menu.share_url.clone()));
                mui.close();
            }
            if menu.has_info && menu_item(mui, palette, Icon::Info, "Info", None).clicked() {
                choice = Some(RowAction::PageInfo);
                mui.close();
            }

            // Changing it.
            groups.start(mui, palette, menu.manage || menu.remove_from_library);
            if menu.manage {
                if menu_item(mui, palette, Icon::Pencil, "Edit playlist", None).clicked() {
                    choice = Some(RowAction::EditPlaylist(menu.page_id.to_owned()));
                    mui.close();
                }
                if menu_item(mui, palette, Icon::Trash2, "Delete playlist", None).clicked() {
                    choice = Some(RowAction::DeletePlaylist(menu.page_id.to_owned()));
                    mui.close();
                }
            }
            if menu.remove_from_library
                && menu_item(mui, palette, Icon::ListX, "Remove from library", None).clicked()
            {
                choice = Some(RowAction::UnsavePlaylist(menu.page_id.to_owned()));
                mui.close();
            }
            choice
        })
        .and_then(|r| r.inner)
}

/// A submenu row that opens a nested menu on hover, and the choice made in it.
///
/// **Never `Popup::open_id` from inside a popup.** `egui::Memory` keeps exactly one popup slot
/// per viewport, so `open_id` *replaces* whatever is open — which meant hovering this row evicted
/// the parent menu's own entry, and on the next frame the parent's `keep_popup_open` was a no-op
/// and the whole menu vanished. The menu opens at the pointer and this row sits some 150pt below
/// it, so any downward mouse travel swept across it and killed the menu: "moving the mouse over
/// the songs closes it". egui's own `SubMenuButton` keeps its state in `ctx.data` and passes
/// `.open(bool)`, which is what this does.
pub(crate) fn sub_menu<T>(
    ui: &mut Ui,
    palette: &Palette,
    icon: Icon,
    label: &str,
    id: egui::Id,
    content: impl FnOnce(&mut Ui) -> Option<T>,
) -> Option<T> {
    let row = menu_item(ui, palette, icon, label, Some("›"));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    if row.hovered() {
        open = true;
    }
    if !open {
        ui.ctx().data_mut(|d| d.insert_temp(id, false));
        return None;
    }

    let popup = egui::Popup::from_response(&row)
        .id(id)
        .kind(egui::PopupKind::Menu)
        .frame(menu_frame(ui, palette))
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .align(egui::RectAlign::RIGHT_START)
        .width(MENU_WIDTH)
        .gap(2.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .open(true);
    let Some(inner) = popup.show(content) else {
        ui.ctx().data_mut(|d| d.insert_temp(id, open));
        return None;
    };

    // Closed when the pointer is over neither the row nor the submenu. Nothing else would take
    // it down, and it must not be the memory slot that does it.
    let pointer = ui.ctx().pointer_latest_pos();
    let inside = pointer
        .is_some_and(|p| row.rect.expand(4.0).contains(p) || inner.response.rect.contains(p));
    ui.ctx().data_mut(|d| d.insert_temp(id, inside));
    inner.inner
}

/// How wide a song menu is. Fixed, so every row's labels line up and the popup does not
/// resize itself as items appear and disappear with the row's capabilities.
pub(crate) const MENU_WIDTH: f32 = 236.0;

/// How wide a [`dropdown`]'s popup is. Fixed for the same reason a menu is — nine scheme
/// labels should not make the box dance open to the width of whichever is longest.
const DROPDOWN_WIDTH: f32 = 216.0;

/// Like/Dislike thumbs in the row's right-hand slot, left of the duration.
///
/// Like/Dislike thumbs in the row's right-hand slot, left of the duration.
///
/// Drawn when the pointer is geometrically over the row *or* the track is already rated —
/// which is also what the site does. The draw condition must be geometric
/// (`contains_pointer`, not `hovered`): egui hovers only the topmost stacked widget, so a
/// condition on the row's own hover blinks the thumbs out the moment the pointer crosses
/// into their zone, and they oscillate every frame after that. Interaction is never gated
/// on drawn for the same reason; a click is only reachable where the thumbs are visible
/// anyway, since their zone sits inside the row. Pressing a thumb toggles: pressing Like
/// on a liked song un-likes it.
///
/// One known gap: a right-click landing exactly in the thumb zone does not open the row
/// menu — the thumbs sit on top and swallow it. The zone is 68px of a full-width row, so
/// right-clicking the title always works.
#[allow(clippy::too_many_arguments)]
fn rate_thumbs(
    ui: &mut Ui,
    palette: &Palette,
    rect: Rect,
    thumb_right: f32,
    like: crate::model::LikeStatus,
    video_id: &str,
    id_salt: usize,
    over: bool,
    quiet: bool,
) -> Option<super::ui::RowAction> {
    use super::ui::RowAction;
    use crate::model::LikeStatus;
    let size = 30.0;
    let mut choice = None;
    for (up, pressed, label, dx) in [
        (true, LikeStatus::Like, "like", 50.0),
        (false, LikeStatus::Dislike, "dislike", 18.0),
    ] {
        let hit_rect = Rect::from_center_size(
            egui::pos2(thumb_right - dx, rect.center().y),
            vec2(size, size),
        );
        // At rest a row shows only the rating it *has* — a liked song shows one filled thumb,
        // not a filled one beside an empty one. Both appear together under the pointer, which
        // is the only time the other one can be clicked anyway. A column of paired thumbs down
        // a library of liked songs was the single noisiest thing on the page.
        let set = like == pressed;
        let hit = rate_button(
            ui,
            palette,
            hit_rect,
            ui.id().with(("ytm-row-thumb", id_salt, label)),
            up,
            set,
            over || (set && !quiet),
        );
        if hit.clicked() {
            choice = Some(RowAction::Rate {
                video_id: video_id.to_owned(),
                status: like.toggled(pressed),
            });
        }
    }
    choice
}

/// How tall a card's cover is: full width less the inset, or 16:9 of that for a video still.
fn card_art_height(width: f32, shape: ArtShape) -> f32 {
    let w = width - 16.0;
    match shape {
        ArtShape::Wide => w * 9.0 / 16.0,
        _ => w,
    }
}

/// The space a card occupies: an 8pt top inset, the cover, and a 52pt two-line label band.
///
/// One function because two places must agree — [`art_card`] allocates it and
/// [`crate::ui::card_is_visible`] reserves it, and a card whose visibility math disagrees
/// with its allocation either fetches art it never draws or pops when the art lands.
pub fn card_size(width: f32, shape: ArtShape) -> egui::Vec2 {
    vec2(width, 8.0 + card_art_height(width, shape) + 52.0)
}

/// A card's cover: inset 8pt from the card's top and sides.
fn card_art(card: Rect, width: f32, shape: ArtShape) -> Rect {
    Rect::from_min_size(
        card.min + vec2(8.0, 8.0),
        vec2(width - 16.0, card_art_height(width, shape)),
    )
}

/// A card's hover and selection fill: the cover's curve grown by the 8pt it is inset, so the
/// two curves are concentric — at the general radius the fill rounded less than the cover inside
/// it once a big cover's corner had grown. Held to twice the cover's own curve, so a nearly
/// square cover (Sharp) keeps a nearly square card, and to the panel's, so a card never rounds
/// more than the card it sits on. A round cover (an artist) is measured as a square one of the
/// same size: the fill around a circle is still a card.
fn card_radius(art: Rect) -> CornerRadius {
    let cover = ArtShape::Square.radius(art).nw;
    CornerRadius::same(
        cover
            .saturating_add(8)
            .min(cover.saturating_mul(2))
            .min(theme::radii().panel),
    )
}

/// A clickable card for grids: rounded cover over a title and subtitle.
///
/// Allocates a square cover plus a two-line label band — or a 16:9 cover in a shorter card for
/// [`ArtShape::Wide`] — so rows never reflow when art lands.
#[allow(clippy::too_many_arguments)]
pub fn art_card(
    ui: &mut Ui,
    palette: &Palette,
    width: f32,
    texture: Option<&egui::TextureHandle>,
    title: &str,
    subtitle: &str,
    shape: ArtShape,
    menu: Option<CardMenu<'_>>,
    // The caller's own to compute (it has the entry `art_card` itself is never given) — see
    // `crate::ui::grid_card`'s own doc comment for how much of this it can actually tell today.
    playing: bool,
    // The card the listener last single-clicked in double-click-to-play mode. Purely visual.
    selected: bool,
) -> CardClick {
    let size = card_size(width, shape);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let mut play = false;
    let mut cut = false;

    if ui.is_rect_visible(rect) {
        let art = card_art(rect, width, shape);
        let radius = card_radius(art);
        // Geometric, not `hovered()`: the play button's own hit region sits stacked on top of
        // this card and would steal the hover flag the instant the pointer reached it, fading
        // the whole card — button included — out from directly under the pointer. See `fade`'s
        // own doc comment; this is exactly the case it warns about.
        let t = fade(ui, response.id.with("lit"), response.contains_pointer());
        // The single-click selection sits under the hover lift, same as a row's.
        if selected {
            ui.painter().rect_filled(
                rect,
                radius,
                palette.glass_state(palette.surface_active, 0.55),
            );
        }
        if t > 0.001 {
            // Lifts: a fill plus a shadow underneath, both fading with the hover.
            ui.painter().add(egui::epaint::RectShape::filled(
                rect,
                radius,
                palette.glass_state(palette.surface_hover, t),
            ));
        }
        paint_art(ui, palette, art, texture, shape);
        // Circles are people — an artist has no single thing to play, so no play button.
        if shape != ArtShape::Circle {
            // Loading, the corner button stays up whether hovered or not, ringed like the
            // player bar's — the card you pressed says it heard you.
            let loading = playing && track_loading(ui.ctx());
            let button = Rect::from_center_size(
                art.right_bottom() - vec2(17.0 + 8.0, 17.0 + 8.0),
                vec2(34.0, 34.0),
            );
            play_overlay(
                ui,
                palette,
                art,
                if loading { 1.0 } else { t },
                true,
                playing,
                button,
            );
            if loading {
                let centre = art.right_bottom() - vec2(17.0 + 8.0, 17.0 + 8.0);
                paint_spinner(ui, centre, 17.0 + 3.0, 2.0, palette.accent);
            }
            // A separate hit region over just the button, sized at its full (settled) diameter
            // regardless of the fade's current progress — a click only ever lands once it is
            // basically visible anyway, and a fixed target beats one that moves under the
            // pointer mid-animation. Independent of `response`'s own click below: the caller
            // checks this first and treats it as authoritative, the same way a row's title link
            // wins over the row's own click, so clicking the button plays instead of opening
            // the page while clicking anywhere else on the card still does.
            if t > 0.01 {
                let centre = art.right_bottom() - vec2(25.0, 25.0);
                let hit = Rect::from_center_size(centre, vec2(34.0, 34.0));
                play = ui
                    .interact(hit, response.id.with("play"), Sense::click())
                    .clicked();
            }
        }
        if response.contains_pointer() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let label_w = width - 16.0;
        let shown_title = truncate_to(ui, title, theme::medium(13.0), label_w);
        let shown_subtitle = truncate_to(ui, subtitle, theme::regular(12.0), label_w);
        // Either line cut short: the whole of both under the pointer.
        cut = matches!(shown_title, std::borrow::Cow::Owned(_))
            || matches!(shown_subtitle, std::borrow::Cow::Owned(_));
        crate::emoji::paint_line(
            ui,
            art.left_bottom() + vec2(0.0, 14.0),
            &shown_title,
            theme::medium(13.0),
            palette.text,
        );
        crate::emoji::paint_line(
            ui,
            art.left_bottom() + vec2(0.0, 28.0),
            &shown_subtitle,
            theme::regular(12.0),
            palette.secondary,
        );
    }
    let response = if cut {
        response.hint(if subtitle.is_empty() {
            title.to_owned()
        } else {
            format!("{title}\n{subtitle}")
        })
    } else {
        response
    };
    // Outside the visibility check, as the row menu is: a menu opened at the pointer must
    // survive its card scrolling out from under it.
    let action = menu.and_then(|m| card_menu(ui, palette, &response, m));
    CardClick {
        response,
        play,
        action,
    }
}

/// A folder card for the Local tab's file-explorer-style browser — always a plain folder
/// glyph (a folder has no cover to fetch), its name, and how many tracks sit inside it,
/// recursively. Left click opens it; right click offers `menu`, built by the caller from
/// [`PageMenu`] with `context_only: true` — the same right-click contract the sidebar rail's
/// own playlist rows use, so "Save as playlist" plus the ordinary shuffle/queue/save controls
/// are available without opening the folder first.
///
/// A sibling to [`art_card`] rather than a texture-carrying case of it — there is no thumbnail
/// concept for a folder, so this skips straight to painting the glyph, and skips the play
/// overlay `art_card` always draws: a folder isn't itself playable by clicking its cover, only
/// through the menu's own Shuffle/Play controls.
pub fn folder_card(
    ui: &mut Ui,
    palette: &Palette,
    width: f32,
    name: &str,
    track_count: usize,
    menu: PageMenu<'_>,
) -> CardClick {
    let shape = ArtShape::Square;
    let size = card_size(width, shape);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let art = card_art(rect, width, shape);
        let radius = card_radius(art);
        let t = lit(ui, &response);
        if t > 0.001 {
            ui.painter().add(egui::epaint::RectShape::filled(
                rect,
                radius,
                palette.glass_state(palette.surface_hover, t),
            ));
        }
        ui.painter()
            .rect_filled(art, shape.radius(art), palette.glass(palette.surface));
        let g = art.width() * 0.5;
        Icon::Folder
            .image(palette.dim, g)
            .paint_at(ui, Rect::from_center_size(art.center(), vec2(g, g)));
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let label_w = width - 16.0;
        crate::emoji::paint_line(
            ui,
            art.left_bottom() + vec2(0.0, 14.0),
            &truncate_to(ui, name, theme::medium(13.0), label_w),
            theme::medium(13.0),
            palette.text,
        );
        let subtitle = format!("{track_count} tracks");
        crate::emoji::paint_line(
            ui,
            art.left_bottom() + vec2(0.0, 28.0),
            &truncate_to(ui, &subtitle, theme::regular(12.0), label_w),
            theme::regular(12.0),
            palette.secondary,
        );
    }
    let action = page_menu(ui, palette, &response, menu);
    CardClick {
        response,
        play: false,
        action,
    }
}

/// What a card reports: the click, and any menu choice.
pub struct CardClick {
    pub response: egui::Response,
    /// The corner play button specifically, when the card has one — wins over `response`, the
    /// same way a row's title link wins over its own click: a caller that sees this set should
    /// act on it and skip treating `response.clicked()` as an ordinary card click.
    pub play: bool,
    pub(crate) action: Option<super::ui::RowAction>,
}

/// What a card's right-click menu is built from.
pub enum CardMenu<'a> {
    /// A song or video card: the same menu its row would have.
    Track(RowMenu<'a>),
    /// An artist card: what the artist's own menu on the site offers.
    Artist(&'a crate::model::ArtistRef),
    /// An album or playlist card: the same menu the sidebar rail gives a playlist — mix,
    /// queueing, saving, navigation and share.
    Release(ReleaseMenu<'a>),
}

/// What an album/playlist card's menu is built from.
///
/// The page ··· menu and this share [`page_menu`]; the card simply knows less until the release
/// is fetched. `tracks` starts empty and fills from the prefetch, at which point the queueing and
/// save items appear — the same fill-in the rail's own context menu does.
pub struct ReleaseMenu<'a> {
    pub page_id: &'a str,
    pub tracks: &'a [crate::model::Track],
    /// The playlist/album id a "Start mix" seeds from (`RDAMPL`). `None` for an album whose
    /// audio playlist id is not known yet.
    pub radio_id: Option<&'a str>,
    pub artist_id: Option<&'a str>,
    pub playlists: &'a [crate::model::PlaylistRef],
    pub signed_in: bool,
    pub share_url: String,
    /// An owned playlist: enables Edit/Delete.
    pub manage: bool,
    pub remove_from_library: bool,
    /// See [`PageMenu::tracks_pending`].
    pub(crate) tracks_pending: Option<super::ui::PageKind>,
}

/// A card's right-click menu, dispatched by what the card is.
///
/// Cards had no menu at all, which is why right-clicking worked on an artist page's Top songs
/// and did nothing on its Videos shelf — the same songs, drawn as cards.
fn card_menu(
    ui: &mut Ui,
    palette: &Palette,
    response: &egui::Response,
    menu: CardMenu<'_>,
) -> Option<super::ui::RowAction> {
    use super::ui::RowAction;
    match menu {
        CardMenu::Track(row) => row_menu(ui, palette, response, row),
        CardMenu::Artist(artist) => {
            let popup = card_popup(
                ui,
                palette,
                response,
                ("ytm-artist-menu", artist.id.as_str()),
            );
            popup
                .show(|mui| {
                    let mut choice = None;
                    if let Some(target) = &artist.actions.mix
                        && menu_item(mui, palette, Icon::Radio, "Start mix", None).clicked()
                    {
                        choice = Some(RowAction::PlayTarget {
                            seed: artist.id.clone(),
                            target: target.clone(),
                        });
                        mui.close();
                    }
                    if let Some(target) = &artist.actions.shuffle
                        && menu_item(mui, palette, Icon::Shuffle, "Shuffle play", None).clicked()
                    {
                        choice = Some(RowAction::PlayTarget {
                            seed: artist.id.clone(),
                            target: target.clone(),
                        });
                        mui.close();
                    }
                    let mut groups = MenuGroups::default();
                    groups.start(
                        mui,
                        palette,
                        artist.actions.mix.is_some() || artist.actions.shuffle.is_some(),
                    );
                    if let Some(subscribed) = artist.actions.subscribed {
                        groups.start(mui, palette, true);
                        let label = if subscribed {
                            "Unsubscribe"
                        } else {
                            "Subscribe"
                        };
                        if menu_item(mui, palette, Icon::CircleUser, label, None).clicked() {
                            choice = Some(RowAction::Subscribe {
                                channel_id: artist.id.clone(),
                                subscribe: !subscribed,
                                params: if subscribed {
                                    artist.actions.unsubscribe_params.clone()
                                } else {
                                    artist.actions.subscribe_params.clone()
                                },
                            });
                            mui.close();
                        }
                    }
                    groups.start(mui, palette, true);
                    if menu_item(mui, palette, Icon::Link, "Copy link", None).clicked() {
                        // Not `RowAction::CopyLink`: that builds a `watch?v=` URL, right for a
                        // track's video id but wrong for a channel id.
                        choice = Some(RowAction::PageCopyLink(crate::model::artist_url(
                            &artist.id,
                        )));
                        mui.close();
                    }
                    if let Some(token) = &artist.actions.dont_recommend
                        && menu_item(mui, palette, Icon::ListX, "Don't recommend artist", None)
                            .clicked()
                    {
                        choice = Some(RowAction::DontRecommend {
                            token: token.to_string(),
                            name: artist.name.clone(),
                        });
                        mui.close();
                    }
                    choice
                })
                .and_then(|r| r.inner)
        }
        CardMenu::Release(menu) => page_menu(
            ui,
            palette,
            response,
            PageMenu {
                page_id: menu.page_id,
                tracks: menu.tracks,
                artist_id: menu.artist_id,
                playlists: menu.playlists,
                signed_in: menu.signed_in,
                share_url: menu.share_url,
                has_info: false,
                manage: menu.manage,
                remove_from_library: menu.remove_from_library,
                context_only: true,
                radio_id: menu.radio_id,
                save_as_new: None,
                tracks_pending: menu.tracks_pending,
            },
        ),
    }
}

/// The popup a card menu opens in — same shape as a row's, keyed on the card's own id rather
/// than its index, which shifts as a wrapped grid reflows.
fn card_popup<'a>(
    ui: &Ui,
    palette: &Palette,
    response: &'a egui::Response,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> egui::Popup<'a> {
    egui::Popup::new(ui.id().with(id), ui.ctx().clone(), response, ui.layer_id())
        .kind(egui::PopupKind::Menu)
        .frame(menu_frame(ui, palette))
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .width(MENU_WIDTH)
        .gap(0.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .open_memory(if response.secondary_clicked() {
            Some(egui::SetOpenCommand::Bool(true))
        } else if response.clicked() {
            Some(egui::SetOpenCommand::Bool(false))
        } else {
            None
        })
        .at_pointer_fixed()
}

/// Characters that take no room and are never drawn: variation selectors, joiners, bidi marks,
/// tag characters. They must measure as nothing even though no face "has" them — `has_glyph`
/// answers no for everything in the emoji face (see `theme::tests::can_draw`), and counting
/// them as replacement boxes put a box-wide gap after every "↖️".
fn is_invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x034F
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xE0000..=0xE007F
            | 0xE0100..=0xE01EF
    )
}

/// How wide `c` is painted, after a character `prev` wide.
///
/// - A character no font has is drawn as a replacement box by egui, though it measures zero;
///   measuring the box keeps a title of them from measuring as fitting and running out of its
///   card.
/// - VARIATION SELECTOR-16 turns the character before it into a colour emoji, drawn as an
///   image of its own size ([`crate::emoji`]) whatever the text face's advance for it — Inter's
///   "↖" is 10.5 pt wide against a 15.5 pt image. The selector makes up the difference, plus a
///   point of air, so emoji neither overlap nor stand apart.
/// - Other invisible characters take no room.
fn painted_width(
    f: &mut egui::epaint::text::FontsView<'_>,
    font: &egui::FontId,
    c: char,
    prev: f32,
) -> f32 {
    if c == '\u{FE0F}' {
        return (crate::emoji::emoji_side(font.size) + 1.0 - prev).max(0.0);
    }
    if is_invisible(c) {
        return 0.0;
    }
    let w = f.glyph_width(font, c);
    if w == 0.0 && !f.has_glyph(font, c) {
        f.glyph_width(font, '◻')
    } else {
        w
    }
}

/// [`painted_width`] summed over `text`.
fn painted_text_width(
    f: &mut egui::epaint::text::FontsView<'_>,
    font: &egui::FontId,
    text: &str,
) -> f32 {
    let mut prev = 0.0;
    text.chars()
        .map(|c| {
            prev = painted_width(f, font, c, prev);
            prev
        })
        .sum()
}

/// How wide `text` renders in `font`. One font lock, as with [`truncate_to`].
pub fn text_width(ui: &Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.fonts_mut(|f| painted_text_width(f, font, text))
}

/// The largest size in `min..=max` — 2pt steps — at which `text` still fits `max_width`.
///
/// A detail header's title shrinks rather than ellipsizing, so a long album name keeps its one
/// line; `min` when even that overflows, and the caller's own `truncate_to` cuts it there.
/// `font` builds the `FontId` for a size, so the family stays the theme's business.
pub fn fit_size(
    ui: &Ui,
    text: &str,
    max_width: f32,
    max: f32,
    min: f32,
    font: impl Fn(f32) -> egui::FontId,
) -> f32 {
    let mut size = max;
    while size > min && text_width(ui, text, &font(size)) > max_width {
        size = (size - 2.0).max(min);
    }
    size
}

/// How far above its own centre a capital's ink reaches, as a fraction of the font size —
/// measured from Inter: a 40pt capital rises 14pt.
pub const TITLE_CAP_RISE: f32 = 0.35;

/// Where a detail header's title centre belongs when a type line ("ALBUM") sits above it.
///
/// Measured from that line's own baseline: the air, plus the title's cap rise. What matters is
/// what it does *not* do — the cap line, which is what the eye reads as the top of the name,
/// stays the same distance under the label at every title size, so a bigger name grows
/// downward from it instead of drifting away from the label it belongs to. Centring the line
/// box instead (`size * 1.08`, as this used to) made that air grow with the size.
pub fn title_centre_under_label(label_baseline: f32, air: f32, size: f32) -> f32 {
    label_baseline + air + size * TITLE_CAP_RISE
}

/// How far below its own centre a line's ink reaches when painted with `font`.
///
/// The line box is not the ink: a name with no descenders ends at its baseline, with the
/// box's whole descent left empty under it, while a tail ("y", "p") reaches most of the way
/// down. Anything that must sit under a title — a credit line — measures from here, so it
/// hugs a name that has nothing to clear and keeps its distance from one that does.
pub fn ink_drop(ui: &Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.fonts_mut(|f| {
        let galley = f.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::PLACEHOLDER);
        // `mesh_bounds` is the tight box around the painted glyphs, in the galley's own
        // coordinates; the galley's top sits half its height above the line's centre.
        // `Rect::NOTHING` (nothing painted) leaves a -inf, clamped to "no ink" here.
        (galley.mesh_bounds.max.y - galley.size().y / 2.0).max(0.0)
    })
}

/// Shorten text with an ellipsis so it fits `max_width`.
///
/// egui truncates inside its own layout, but these rows are painted directly, so the measuring
/// has to happen here.
pub fn truncate_to<'t>(
    ui: &Ui,
    text: &'t str,
    font: egui::FontId,
    max_width: f32,
) -> std::borrow::Cow<'t, str> {
    use std::borrow::Cow;
    // No room is not "no bound". A piece past its line's right edge — a subtitle whose
    // separator ate the last of the width — must vanish, not paint the whole string over
    // whatever sits beyond the row. Every caller computes this from real geometry, so a
    // zero here always means "nothing fits".
    if max_width <= 0.0 {
        return Cow::Borrowed("");
    }
    // One lock for the whole measurement. Taking it per glyph — as this used to — is tens of
    // thousands of acquisitions per frame on a long list, twice per row. The common case —
    // text that already fits — is returned borrowed, with no allocation at all.
    ui.fonts_mut(|f| {
        let total = painted_text_width(f, &font, text);
        if total <= max_width {
            return Cow::Borrowed(text);
        }
        let ellipsis_w = f.glyph_width(&font, '…');
        let mut out = String::with_capacity(text.len());
        let mut used = 0.0;
        let mut prev = 0.0;
        for ch in text.chars() {
            let w = painted_width(f, &font, ch, prev);
            prev = w;
            if used + w + ellipsis_w > max_width {
                break;
            }
            out.push(ch);
            used += w;
        }
        out.push('…');
        Cow::Owned(out)
    })
}

/// Text in the accent colour on a pill, used for primary actions.
pub fn pill_button(ui: &mut Ui, palette: &Palette, label: &str) -> egui::Response {
    pill(ui, palette, label, PillStyle::Accent)
}

/// An outlined pill, the quiet sibling of [`pill_button`] — the artist page's Subscribe
/// (a "Follow" in the reference it borrows its shape from).
pub fn outline_button(ui: &mut Ui, palette: &Palette, label: &str) -> egui::Response {
    pill(ui, palette, label, PillStyle::Outline)
}

/// A filter chip: a pill that carries a selected state.
///
/// Selected inverts — the label's colour becomes the fill and the window's becomes the label —
/// which is how a chip row reads at a glance without needing an outline or a checkmark.
pub fn chip(ui: &mut Ui, palette: &Palette, label: &str, selected: bool) -> egui::Response {
    pill(
        ui,
        palette,
        label,
        if selected {
            PillStyle::Selected
        } else {
            PillStyle::Quiet
        },
    )
}

/// The icon-only sibling of [`chip`]: a quiet pill the same height and roundness as the chips
/// beside it, holding a single glyph. The home feed's mood bar puts one after the chips so
/// clearing the filter is a visible choice rather than a second click on the lit chip.
pub fn icon_chip(ui: &mut Ui, palette: &Palette, icon: Icon, glyph: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());

    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        let fill = palette.glass(mix(palette.surface, palette.surface_hover, t));
        let fill = if held(&response) {
            fill.gamma_multiply(0.85)
        } else {
            fill
        };
        ui.painter().rect_filled(rect, theme::pill(rect), fill);
        icon.image(mix(palette.secondary, palette.text, t), glyph)
            .paint_at(
                ui,
                Rect::from_center_size(rect.center() + press_offset(&response), vec2(glyph, glyph)),
            );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// A chip with a colour dot ahead of its label — the scheme picker, where the label alone says
/// nothing about what choosing it does. Same two states as [`chip`], same sizing and press
/// feel, just wider by the dot.
pub fn swatch_chip(
    ui: &mut Ui,
    palette: &Palette,
    swatch: Color32,
    label: &str,
    selected: bool,
    chevron: bool,
) -> egui::Response {
    let font = theme::semibold(13.0);
    // A long option name would otherwise size the trigger to the raw text and push it out of
    // its row; ellipsise at a fixed maximum so the pill stays put.
    const MAX_CHIP_LABEL: f32 = 180.0;
    let label = truncate_to(ui, label, font.clone(), MAX_CHIP_LABEL);
    // See `pill`'s own comment: a galley laid out with a real colour ignores the colour handed
    // to `painter.galley`, which is how a selected chip once painted its label invisibly.
    let galley =
        ui.fonts_mut(|f| f.layout_no_wrap(label.into_owned(), font.clone(), Color32::PLACEHOLDER));
    const DOT: f32 = 9.0;
    // A chevron rides 20pt past the label's last letter and needs its own room — a dropdown
    // whose arrow lands on its label is not a dropdown, it is a smudge.
    const CHEVRON: f32 = 20.0;
    let size = vec2(
        galley.size().x + DOT + 34.0 + u8::from(chevron) as f32 * CHEVRON,
        32.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        let (fill, fg) = if selected {
            (mix(palette.text, Color32::WHITE, t * 0.4), palette.window)
        } else {
            (
                palette.glass(mix(palette.surface, palette.surface_hover, t)),
                mix(palette.secondary, palette.text, t),
            )
        };
        let fill = if held(&response) {
            fill.gamma_multiply(0.85)
        } else {
            fill
        };
        ui.painter().rect_filled(rect, theme::pill(rect), fill);
        let dot_center = rect.left_center() + vec2(16.0, 0.0);
        ui.painter().circle_filled(dot_center, DOT / 2.0, swatch);
        let text_pos = dot_center + vec2(DOT / 2.0 + 8.0, -galley.size().y / 2.0);
        ui.painter()
            .galley(text_pos + press_offset(&response), galley, fg);
        if chevron {
            // A drawn chevron rather than a glyph: the icon set carries no down-pointing one,
            // and a character that depends on which face serves "▾" is a font question this
            // need not ask.
            let c = rect.right_center() - vec2(16.0, 1.0) + press_offset(&response);
            ui.painter().add(egui::Shape::convex_polygon(
                vec![c - vec2(4.5, 2.0), c + vec2(4.5, -2.0), c + vec2(0.0, 2.5)],
                fg,
                egui::Stroke::NONE,
            ));
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// A dropdown: a pill button showing the current choice — its swatch, its label, a chevron —
/// opening our own styled popup with a swatch and label per option, a tick on the selected one.
///
/// Composed from the same pieces as every other control (the [`pill`] press feel, the
/// [`menu_item`]-shaped rows, the `egui::Popup` the track menus open through) rather than
/// egui's stock `ComboBox`, whose visuals would look like a visitor. Returns the index of the
/// option clicked this frame, if any; the popup toggles on the button and closes on a click
/// anywhere else.
pub fn dropdown(
    ui: &mut Ui,
    palette: &Palette,
    id: egui::Id,
    current_swatch: Color32,
    current: &str,
    options: &[(Color32, &str)],
) -> Option<usize> {
    let response = swatch_chip(ui, palette, current_swatch, current, false, true);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let mut choice = None;
    let popup = egui::Popup::from_response(&response)
        .id(id)
        .kind(egui::PopupKind::Menu)
        .frame(menu_frame(ui, palette))
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .align(egui::RectAlign::BOTTOM_START)
        .width(DROPDOWN_WIDTH)
        .gap(4.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        // Toggle, like the page's ••• menu: the button opens from a primary click, and the
        // context-menu pattern (primary closes) would kill it on the very click that opened it.
        .open_memory(if response.clicked() {
            Some(egui::SetOpenCommand::Toggle)
        } else {
            None
        });
    popup.show(|mui| {
        egui::ScrollArea::vertical()
            .id_salt(("ytm-dropdown-scroll", id))
            .max_height(320.0)
            .show(mui, |mui| {
                for (i, (swatch, label)) in options.iter().enumerate() {
                    let row = dropdown_row(mui, palette, *swatch, label, current == *label);
                    if row.clicked() {
                        choice = Some(i);
                        mui.close();
                    }
                }
            });
    });
    choice
}

/// One row inside a [`dropdown`]: a swatch dot, a label, and a tick on the choice in force.
fn dropdown_row(
    ui: &mut Ui,
    palette: &Palette,
    swatch: Color32,
    label: &str,
    selected: bool,
) -> egui::Response {
    let height = 32.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        if let Some(fill) = state_fill(ui, palette, &response) {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(theme::radii().item), fill);
        }
        let fg = mix(palette.secondary, palette.text, t);
        let nudge = press_offset(&response);
        ui.painter()
            .circle_filled(rect.left_center() + vec2(18.0, 0.0) + nudge, 4.5, swatch);
        crate::emoji::paint_line(
            ui,
            rect.left_center() + vec2(32.0, 0.0) + nudge,
            &truncate_to(
                ui,
                label,
                theme::medium(13.0),
                (rect.width() - 32.0 - 24.0).max(0.0),
            ),
            theme::medium(13.0),
            fg,
        );
        if selected {
            // A drawn tick rather than "✓": whether a face serves U+2713 is a font question
            // this need not ask, and two strokes are the mark everywhere else in the app too.
            let c = rect.right_center() - vec2(14.0, 1.0) + nudge;
            ui.painter().add(egui::Shape::line(
                vec![
                    c + vec2(-4.0, 0.0),
                    c + vec2(-1.0, 3.0),
                    c + vec2(4.0, -4.0),
                ],
                egui::Stroke::new(2.0, palette.accent),
            ));
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// A dropdown triggered by a plain icon rather than a labelled pill — for a choice with no
/// natural swatch colour to show (a playlist's sort order, say), where painting one anyway
/// would invent a meaning that isn't there. Options are plain text, ticked on the current
/// choice; no swatch dot in front of them either, for the same reason. Returns the index of
/// the option clicked this frame, same contract as [`dropdown`].
pub fn icon_dropdown(
    ui: &mut Ui,
    palette: &Palette,
    id: egui::Id,
    icon: Icon,
    active: bool,
    current: &str,
    options: &[&str],
) -> Option<usize> {
    // The size of the icons it stands beside — Shuffle, Mix — so the sort control never reads
    // as the small one of the row.
    let response = icon_button(ui, palette, icon, 20.0, active);

    let mut choice = None;
    let popup = egui::Popup::from_response(&response)
        .id(id)
        .kind(egui::PopupKind::Menu)
        .frame(menu_frame(ui, palette))
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .align(egui::RectAlign::BOTTOM_END)
        .width(DROPDOWN_WIDTH)
        .gap(4.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .open_memory(if response.clicked() {
            Some(egui::SetOpenCommand::Toggle)
        } else {
            None
        });
    popup.show(|mui| {
        for (i, label) in options.iter().enumerate() {
            let row = text_dropdown_row(mui, palette, label, current == *label);
            if row.clicked() {
                choice = Some(i);
                mui.close();
            }
        }
    });
    choice
}

/// One row inside an [`icon_dropdown`]: a label and a tick on the choice in force — no swatch,
/// unlike [`dropdown_row`], since there is nothing here for a colour to mean.
fn text_dropdown_row(
    ui: &mut Ui,
    palette: &Palette,
    label: &str,
    selected: bool,
) -> egui::Response {
    let height = 32.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        if let Some(fill) = state_fill(ui, palette, &response) {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(theme::radii().item), fill);
        }
        let fg = mix(palette.secondary, palette.text, t);
        let nudge = press_offset(&response);
        crate::emoji::paint_line(
            ui,
            rect.left_center() + vec2(14.0, 0.0) + nudge,
            &truncate_to(
                ui,
                label,
                theme::medium(13.0),
                (rect.width() - 14.0 - 24.0).max(0.0),
            ),
            theme::medium(13.0),
            fg,
        );
        if selected {
            // The same drawn tick `dropdown_row` uses, not "✓" — see its own comment for why.
            let c = rect.right_center() - vec2(14.0, 1.0) + nudge;
            ui.painter().add(egui::Shape::line(
                vec![
                    c + vec2(-4.0, 0.0),
                    c + vec2(-1.0, 3.0),
                    c + vec2(4.0, -4.0),
                ],
                egui::Stroke::new(2.0, palette.accent),
            ));
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// A toggle's knob: a disc in a pill track, and at the squarer [`theme::Corners`] levels the
/// same rounded square as its track, 3pt in, so the two curves nest.
fn toggle_knob(ui: &Ui, track: Rect, cx: f32, r: f32, colour: Color32) {
    ui.painter()
        .rect_filled(knob_rect(track, cx, r), knob_radius(track), colour);
}

fn knob_rect(track: Rect, cx: f32, r: f32) -> Rect {
    Rect::from_center_size(egui::pos2(cx, track.center().y), vec2(2.0 * r, 2.0 * r))
}

fn knob_radius(track: Rect) -> CornerRadius {
    CornerRadius::same(theme::pill(track).nw.saturating_sub(3))
}

/// [`toggle`], greyed and inert when `enabled` is false — for a setting that has nothing to
/// act on right now (no compositor blur, transparency off) rather than a switch that silently
/// does nothing when clicked.
pub fn toggle_enabled(
    ui: &mut Ui,
    palette: &Palette,
    id: egui::Id,
    on: &mut bool,
    enabled: bool,
) -> egui::Response {
    if enabled {
        return toggle(ui, palette, id, on);
    }
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(id, *on, 0.12);
        let track = palette.glass(palette.surface.lerp_to_gamma(palette.dim, t * 0.5));
        ui.painter().rect_filled(rect, theme::pill(rect), track);
        let r = (rect.height() - 6.0) / 2.0;
        let cx = rect.left() + 3.0 + r + t * (rect.width() - 6.0 - 2.0 * r);
        ui.painter()
            .rect_filled(knob_rect(rect, cx, r), knob_radius(rect), palette.dim);
    }
    response
}

/// A sliding switch. Flips `on` when clicked and returns the response.
///
/// The knob travels over 120ms through `animate_bool_with_time` — bounded, pointer-started,
/// idle-safe like every M15 animation — and shrinks slightly while held.
pub fn toggle(ui: &mut Ui, palette: &Palette, id: egui::Id, on: &mut bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(id, *on, 0.12);
        let track = palette
            .glass(palette.surface_hover)
            .lerp_to_gamma(palette.accent, t);
        ui.painter().rect_filled(
            rect,
            theme::pill(rect),
            if response.is_pointer_button_down_on() {
                track.lerp_to_gamma(palette.surface_active, 0.35)
            } else {
                track
            },
        );
        let r = (rect.height() - 6.0) / 2.0;
        let cx = rect.left() + 3.0 + r + t * (rect.width() - 6.0 - 2.0 * r);
        toggle_knob(
            ui,
            rect,
            cx,
            if response.is_pointer_button_down_on() {
                r * 0.9
            } else {
                r
            },
            palette.on_accent,
        );
    }
    response
}

/// A strip of text tabs under a page title: the open one at full brightness with an accent bar
/// under it, the rest quiet, and a hairline under the whole strip. Returns the tab clicked.
pub fn tab_strip(
    ui: &mut Ui,
    palette: &Palette,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    const HEIGHT: f32 = 38.0;
    const GAP: f32 = 22.0;
    let mut clicked = None;
    let left = ui.cursor().min.x;
    let right = ui.max_rect().right();
    let mut x = left;
    let top = ui.cursor().min.y;
    for (i, label) in labels.iter().enumerate() {
        let font = if i == selected {
            theme::semibold(14.0)
        } else {
            theme::medium(14.0)
        };
        let galley =
            ui.fonts_mut(|f| f.layout_no_wrap((*label).to_owned(), font, Color32::PLACEHOLDER));
        let rect = Rect::from_min_size(egui::pos2(x, top), vec2(galley.size().x, HEIGHT));
        let response = ui.interact(rect, ui.id().with(("tab", i)), Sense::click());
        let t = lit(ui, &response);
        let colour = if i == selected {
            palette.text
        } else {
            mix(palette.secondary, palette.text, t)
        };
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if ui.is_rect_visible(rect) {
            let at = egui::pos2(rect.min.x, rect.center().y - galley.size().y / 2.0 - 1.0);
            ui.painter().galley(at, galley, colour);
        }
        if response.clicked() {
            clicked = Some(i);
        }
        x = rect.right() + GAP;
    }
    let base = top + HEIGHT;
    ui.painter().hline(
        left..=right,
        base,
        egui::Stroke::new(1.0, palette.outline.gamma_multiply(0.6)),
    );
    // The bar under the open tab, over the hairline.
    let mut x = left;
    for (i, label) in labels.iter().enumerate() {
        let w = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                (*label).to_owned(),
                if i == selected {
                    theme::semibold(14.0)
                } else {
                    theme::medium(14.0)
                },
                Color32::PLACEHOLDER,
            )
            .size()
            .x
        });
        if i == selected {
            ui.painter().rect_filled(
                Rect::from_min_size(egui::pos2(x, base - 2.0), vec2(w, 2.0)),
                CornerRadius::same(1),
                palette.accent,
            );
        }
        x += w + GAP;
    }
    ui.allocate_space(vec2(right - left, HEIGHT + 1.0));
    clicked
}

/// The height of a [`Group`] row that holds one line of control.
const ROW_H: f32 = 58.0;

/// Settings rows inside one card: each a title and a line under it on the left, its control on
/// the right, with a hairline between rows. Build one with [`setting_group`].
pub struct Group<'a> {
    palette: &'a Palette,
    first: bool,
}

impl Group<'_> {
    /// The hairline above every row but the first.
    fn divide(&mut self, ui: &mut Ui) {
        if !self.first {
            let y = ui.cursor().min.y;
            let r = ui.max_rect();
            ui.painter().hline(
                r.left()..=r.right(),
                y,
                egui::Stroke::new(1.0, self.palette.outline.gamma_multiply(0.45)),
            );
            ui.add_space(1.0);
        }
        self.first = false;
    }

    fn text(&self, ui: &mut Ui, title: &str, detail: &str) {
        ui.label(
            egui::RichText::new(title)
                .font(theme::medium(13.0))
                .color(self.palette.text),
        );
        if !detail.is_empty() {
            ui.label(
                egui::RichText::new(detail)
                    .font(theme::regular(11.0))
                    .color(self.palette.dim),
            );
        }
    }

    /// A title and its line on the left, one control on the right. A chip group is added
    /// right to left, so it goes in last-first.
    pub fn row(&mut self, ui: &mut Ui, title: &str, detail: &str, control: impl FnOnce(&mut Ui)) {
        self.divide(ui);
        let width = ui.available_width();
        ui.allocate_ui_with_layout(
            vec2(width, ROW_H),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_min_height(ROW_H);
                control(ui);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    // Centred by measure: a `vertical` starts at the top of the row's rect.
                    let lines = if detail.is_empty() { 1.0 } else { 2.0 };
                    ui.add_space(((ROW_H - lines * 15.5) / 2.0 - 1.0).max(8.0));
                    self.text(ui, title, detail);
                });
            },
        );
    }

    /// A title and its line, then the control on a line of its own: for a row of chips too wide
    /// to sit beside the words.
    pub fn wide_row(
        &mut self,
        ui: &mut Ui,
        title: &str,
        detail: &str,
        control: impl FnOnce(&mut Ui),
    ) {
        self.divide(ui);
        ui.add_space(12.0);
        self.text(ui, title, detail);
        ui.add_space(8.0);
        ui.horizontal_wrapped(control);
        ui.add_space(12.0);
    }

    /// Free content with the same side and top padding as a row.
    pub fn free(&mut self, ui: &mut Ui, contents: impl FnOnce(&mut Ui)) {
        self.divide(ui);
        ui.add_space(12.0);
        contents(ui);
        ui.add_space(12.0);
    }
}

/// A card of settings under a small caption. `contents` adds its rows through the [`Group`].
pub fn setting_group(
    ui: &mut Ui,
    palette: &Palette,
    caption: Option<&str>,
    contents: impl FnOnce(&mut Ui, &mut Group),
) {
    if let Some(caption) = caption {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(caption)
                .font(theme::semibold(12.0))
                .color(palette.secondary),
        );
        ui.add_space(8.0);
    }
    let inner = ui.available_width() - 32.0;
    egui::Frame::NONE
        // Between the page and a chip's own fill, so an unselected chip still shows on it.
        .fill(palette.glass(mix(palette.panel, palette.surface, 0.4)))
        .corner_radius(CornerRadius::same(theme::radii().popup))
        .inner_margin(egui::Margin::symmetric(16, 0))
        .show(ui, |ui| {
            ui.set_width(inner);
            let mut group = Group {
                palette,
                first: true,
            };
            contents(ui, &mut group);
        });
    ui.add_space(24.0);
}

enum PillStyle {
    /// The call to action: accent fill.
    Accent,
    /// An unselected chip.
    Quiet,
    /// A selected chip.
    Selected,
    /// No fill at rest, a real outline, the label at full brightness — the artist page's
    /// Subscribe, mirroring the reference's outlined "Follow". Hover fills it quietly, the way
    /// the Quiet chip already does, so it still reads as a button rather than a label.
    Outline,
}

fn pill(ui: &mut Ui, palette: &Palette, label: &str, style: PillStyle) -> egui::Response {
    let font = theme::semibold(13.0);
    // Laid out with `PLACEHOLDER` so the colour passed to `painter.galley` is the one that
    // wins — a galley built with a real colour ignores the override, which is how the selected
    // chip ended up painting white text on a white pill.
    let galley =
        ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), Color32::PLACEHOLDER));
    let size = vec2(galley.size().x + 28.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        let (fill, fg) = match style {
            PillStyle::Accent => (
                mix(palette.accent, palette.accent_hover, t),
                palette.on_accent,
            ),
            PillStyle::Quiet => (
                palette.glass(mix(palette.surface, palette.surface_hover, t)),
                mix(palette.secondary, palette.text, t),
            ),
            PillStyle::Selected => (mix(palette.text, Color32::WHITE, t * 0.4), palette.window),
            PillStyle::Outline => (
                // Spelled out rather than mixed from `TRANSPARENT`: `mix` builds opaque colours,
                // so a zero-`t` mix comes back opaque black — a filled pill, not an outline.
                if t <= 0.001 {
                    Color32::TRANSPARENT
                } else {
                    palette.glass_state(palette.surface_hover, t)
                },
                mix(palette.text, Color32::WHITE, t * 0.4),
            ),
        };
        let fill = if held(&response) {
            fill.gamma_multiply(0.85)
        } else {
            fill
        };
        ui.painter().rect_filled(rect, theme::pill(rect), fill);
        if matches!(style, PillStyle::Outline) {
            ui.painter().rect_stroke(
                rect,
                theme::pill(rect),
                egui::Stroke::new(1.0, mix(palette.outline, palette.text, t * 0.5)),
                egui::StrokeKind::Inside,
            );
        }
        ui.painter().galley(
            rect.center() - galley.size() / 2.0 + press_offset(&response),
            galley,
            fg,
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// A thumbs-up / thumbs-down button, wherever a rating is offered.
///
/// Four states, and they are the point: at rest a dim outline with no backing; under the pointer
/// the glyph brightens and a round backing fades in; **set** it is a filled glyph in the accent
/// colour, which is what makes a rating readable across a room; held, it sinks and darkens.
/// A control that only ever changed colour is what made the old pair look bolted on.
pub fn rate_button(
    ui: &mut Ui,
    palette: &Palette,
    rect: Rect,
    id: egui::Id,
    up: bool,
    set: bool,
    draw: bool,
) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click());
    if draw && ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        let backing = if held(&response) {
            Some(palette.glass(palette.surface_active))
        } else {
            (t > 0.001).then(|| palette.glass_state(palette.surface_hover, t))
        };
        if let Some(fill) = backing {
            ui.painter().circle_filled(rect.center(), 15.0, fill);
        }
        let icon = match (up, set) {
            (true, true) => Icon::ThumbsUpFilled,
            (true, false) => Icon::ThumbsUp,
            (false, true) => Icon::ThumbsDownFilled,
            (false, false) => Icon::ThumbsDown,
        };
        let fg = if set {
            palette.accent
        } else {
            mix(palette.dim, palette.text, t)
        };
        let size = 16.0;
        // The thumbs-down glyph hangs from its top edge; nudging it down centres the pair.
        let bias = if up { 0.0 } else { 1.0 };
        icon.image(fg, size).paint_at(
            ui,
            Rect::from_center_size(
                rect.center() + vec2(0.0, bias) + press_offset(&response),
                vec2(size, size),
            ),
        );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The frame every menu and dropdown opens in. On a dark scheme it takes the chips' `surface`
/// — a step lighter than the card it opens over, which is what reads as nearer on a dark page;
/// the darker popup fill it used to have looked sunk into the page. A light scheme keeps its
/// popup fill, already the lighter one there. A faint edge at the dividers' strength gives it a
/// crisp outline, and a short, tight shadow grounds that edge instead of haloing the menu.
/// Corners at 12, so the rows' 6 pt highlight sits evenly inside the 6 pt margin.
pub(crate) fn menu_frame(ui: &Ui, palette: &Palette) -> egui::Frame {
    egui::Frame::popup(ui.style())
        .fill(if palette.dark {
            palette.surface
        } else {
            palette.overlay
        })
        .stroke(egui::Stroke::new(1.0, palette.outline.gamma_multiply(0.55)))
        .corner_radius(CornerRadius::same(theme::radii().popup))
        .inner_margin(egui::Margin::same(6))
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: palette.shadow.gamma_multiply(0.7),
        })
}

/// One row of a context menu: a leading glyph, a label, and an optional trailing mark.
///
/// Hand-drawn rather than `ui.button` so every row is the same height and the icon column lines
/// up — a menu of default buttons is what made the song menu read as an undifferentiated stack.
pub fn menu_item(
    ui: &mut Ui,
    palette: &Palette,
    icon: Icon,
    label: &str,
    trailing: Option<&str>,
) -> egui::Response {
    menu_row(ui, palette, Some(icon), label, trailing)
}

/// [`menu_item`] with no leading icon — the playlists in a "Save to playlist" picker, where the
/// same glyph on every row says nothing the list's own heading hasn't already said.
pub fn menu_item_plain(
    ui: &mut Ui,
    palette: &Palette,
    label: &str,
    trailing: Option<&str>,
) -> egui::Response {
    menu_row(ui, palette, None, label, trailing)
}

/// A faint hairline between a menu's groups of items. The line is what separates them now, so
/// it takes almost no room: the rows' own spacing, pulled in a little on each side, leaves the
/// groups only a few points further apart than two rows. At half the outline's strength it
/// separates without drawing the eye.
pub fn menu_divider(ui: &mut Ui, palette: &Palette) {
    ui.add_space(-2.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().hline(
            (rect.left() + 12.0)..=(rect.right() - 12.0),
            rect.center().y,
            egui::Stroke::new(1.0, palette.outline.gamma_multiply(0.55)),
        );
    }
    ui.add_space(-2.0);
}

/// Dividers for a menu whose groups come and go with the data: each group that has items says
/// so as it starts, and gets a divider only when a group above it drew something — never one at
/// the top, at the bottom, or two in a row.
#[derive(Default)]
struct MenuGroups {
    drawn: bool,
}

impl MenuGroups {
    fn start(&mut self, ui: &mut Ui, palette: &Palette, visible: bool) {
        if !visible {
            return;
        }
        if self.drawn {
            menu_divider(ui, palette);
        }
        self.drawn = true;
    }
}

fn menu_row(
    ui: &mut Ui,
    palette: &Palette,
    icon: Option<Icon>,
    label: &str,
    trailing: Option<&str>,
) -> egui::Response {
    let height = 32.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());

    if ui.is_rect_visible(rect) {
        let t = lit(ui, &response);
        if let Some(fill) = state_fill(ui, palette, &response) {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(theme::radii().item), fill);
        }
        let fg = mix(palette.secondary, palette.text, t);
        let nudge = press_offset(&response);
        let label_x = if let Some(icon) = icon {
            icon.image(fg, 15.0).paint_at(
                ui,
                Rect::from_center_size(
                    rect.left_center() + vec2(18.0, 0.0) + nudge,
                    vec2(15.0, 15.0),
                ),
            );
            38.0
        } else {
            14.0
        };
        // Ellipsise rather than run under the trailing mark (or out of the popup): a long
        // playlist name in the save picker used to overflow the fixed menu width.
        let reserve = if trailing.is_some() { 44.0 } else { 24.0 };
        crate::emoji::paint_line(
            ui,
            rect.left_center() + vec2(label_x, 0.0) + nudge,
            &truncate_to(
                ui,
                label,
                theme::medium(13.0),
                (rect.width() - label_x - reserve).max(0.0),
            ),
            theme::medium(13.0),
            fg,
        );
        if let Some(mark) = trailing {
            ui.painter().text(
                rect.right_center() - vec2(10.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                mark,
                theme::regular(12.0),
                palette.dim,
            );
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// How tall the seek bar's hit area is: WCAG's 24-point minimum for a target, around a line a few
/// points thick.
const SEEK_HIT_HEIGHT: f32 = 24.0;

/// A seek bar. Returns the position to seek to, in seconds, when the user scrubs.
///
/// Drawn rather than composed from egui widgets so the hit area can be taller than the visible
/// 4px line — a hairline you have to hit exactly is miserable to use. `labels` hangs the elapsed
/// time left of the bar and the duration right of it; the compact player bar's edge-to-edge line
/// has no room beside it for either.
pub fn seek_bar(
    ui: &mut Ui,
    palette: &Palette,
    bar: Rect,
    fraction: f32,
    duration: f64,
    position: f64,
    labels: bool,
) -> Option<f64> {
    // At least `SEEK_HIT_HEIGHT` tall, centred on the line, whatever the line's own thickness.
    let hit = bar.expand2(vec2(0.0, ((SEEK_HIT_HEIGHT - bar.height()) / 2.0).max(0.0)));
    let response = ui.interact(hit, egui::Id::new("ytm-seek"), Sense::click_and_drag());
    let active = response.hovered() || response.dragged();

    let radius = CornerRadius::same(2);
    ui.painter()
        .rect_filled(bar, radius, palette.glass(palette.surface_active));
    if fraction > 0.0 {
        ui.painter().rect_filled(
            Rect::from_min_size(bar.min, vec2(bar.width() * fraction, bar.height())),
            radius,
            if active {
                palette.accent_hover
            } else {
                palette.accent
            },
        );
    }
    if active && duration > 0.0 {
        ui.painter().circle_filled(
            egui::pos2(bar.min.x + bar.width() * fraction, bar.center().y),
            5.0,
            palette.accent,
        );
    }

    if labels && duration > 0.0 {
        for (x, align, secs) in [
            (
                bar.min.x - 10.0,
                egui::Align2::RIGHT_CENTER,
                position as u64,
            ),
            (bar.max.x + 10.0, egui::Align2::LEFT_CENTER, duration as u64),
        ] {
            ui.painter().text(
                egui::pos2(x, bar.center().y),
                align,
                fmt_duration(secs),
                theme::regular(11.0),
                palette.dim,
            );
        }
    }

    if duration <= 0.0 {
        return None;
    }
    if response.clicked() || response.dragged() {
        let pos = response.interact_pointer_pos()?;
        let t = ((pos.x - bar.min.x) / bar.width()).clamp(0.0, 1.0) as f64;
        return Some(t * duration);
    }
    None
}

/// One scroll notch, in the same units as `smooth_scroll_delta`. Kept equal to the
/// `line_scroll_speed` the app sets, so a notch on the volume bar is one step by definition.
const SCROLL_PER_NOTCH: f32 = 120.0;

/// Speaker icon plus a track. Click, drag, or scroll to set the volume (0-100); unmuting
/// restores `unmute_to` — the volume you had, kept by the caller — rather than a hardcoded
/// guess.
/// A flat horizontal slider: a 4pt track with a filled portion and a knob that appears under
/// the pointer. The Settings page's transparency control — `volume_slider` keeps its own icon,
/// mute and scroll behaviour rather than growing flags.
pub fn slider(
    ui: &mut Ui,
    palette: &Palette,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    width: f32,
) -> egui::Response {
    let (lo, hi) = (*range.start(), *range.end());
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());

    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        *value = lo + t * (hi - lo);
    }

    if ui.is_rect_visible(rect) {
        let bar = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
        let t = if hi > lo {
            ((*value - lo) / (hi - lo)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let radius = CornerRadius::same(2);
        ui.painter()
            .rect_filled(bar, radius, palette.glass(palette.surface_active));
        ui.painter().rect_filled(
            Rect::from_min_size(bar.min, vec2(bar.width() * t, bar.height())),
            radius,
            if response.hovered() || response.dragged() {
                palette.text
            } else {
                palette.secondary
            },
        );
        if response.hovered() || response.dragged() {
            ui.painter().circle_filled(
                egui::pos2(bar.min.x + bar.width() * t, bar.center().y),
                5.0,
                palette.text,
            );
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

pub fn volume_slider(ui: &mut Ui, palette: &Palette, volume: f64, unmute_to: f64) -> Option<f64> {
    let icon = match volume {
        v if v <= 0.5 => Icon::VolumeX,
        v if v < 50.0 => Icon::Volume1,
        _ => Icon::Volume2,
    };

    let width = 76.0;
    let (track_rect, response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
    let bar = Rect::from_center_size(track_rect.center(), vec2(width, 4.0));

    let fraction = (volume / 100.0).clamp(0.0, 1.0) as f32;
    let radius = CornerRadius::same(2);
    ui.painter()
        .rect_filled(bar, radius, palette.glass(palette.surface_active));
    ui.painter().rect_filled(
        Rect::from_min_size(bar.min, vec2(bar.width() * fraction, bar.height())),
        radius,
        if response.hovered() || response.dragged() {
            palette.text
        } else {
            palette.secondary
        },
    );
    if response.hovered() || response.dragged() {
        ui.painter().circle_filled(
            egui::pos2(bar.min.x + bar.width() * fraction, bar.center().y),
            5.0,
            palette.text,
        );
    }

    // Mute toggle on the icon itself.
    let mut new_volume = None;
    if icon_button(ui, palette, icon, 16.0, false)
        .hint(crate::ui::tips::mute(volume <= 0.5))
        .clicked()
    {
        new_volume = Some(if volume > 0.5 { 0.0 } else { unmute_to });
    }

    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let t = ((pos.x - bar.min.x) / bar.width()).clamp(0.0, 1.0);
        new_volume = Some(f64::from(t) * 100.0);
    }

    // Scroll on the bar: one notch is one step. Normalised against the line-scroll speed so
    // the step does not inherit the faster list scrolling — and read off the bar only, so
    // the page underneath never moves.
    if response.hovered() {
        let delta = ui.input(|i| i.smooth_scroll_delta.y);
        if delta != 0.0 {
            new_volume =
                Some((volume + f64::from(5.0 * delta / SCROLL_PER_NOTCH)).clamp(0.0, 100.0));
        }
    }
    new_volume
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fade runs from fully transparent at an edge to untouched a band in, scaled by how
    /// strongly that end fades, and an end that does not fade is left alone.
    #[test]
    fn the_fade_ramps_from_the_edge_over_its_band() {
        let full = EdgeFade::new(40.0, 1.0);
        let off = EdgeFade::new(40.0, 0.0);
        assert_eq!(fade_factor(0.0, 0.0, 400.0, full, full), 0.0);
        assert_eq!(fade_factor(20.0, 0.0, 400.0, full, full), 0.5);
        assert_eq!(fade_factor(40.0, 0.0, 400.0, full, full), 1.0);
        assert_eq!(fade_factor(200.0, 0.0, 400.0, full, full), 1.0);
        assert_eq!(fade_factor(390.0, 0.0, 400.0, full, full), 0.25);
        assert_eq!(
            fade_factor(0.0, 0.0, 400.0, off, full),
            1.0,
            "no fade at the top"
        );
        let half = EdgeFade::new(40.0, 0.5);
        assert_eq!(fade_factor(0.0, 0.0, 400.0, half, off), 0.5);
        assert!(EdgeFade::new(0.0, 1.0).none() && EdgeFade::new(40.0, 0.0).none());
    }

    /// An eased fade meets the straight one at the ends and the middle, but leaves the edge and
    /// reaches full strength flat; its mesh is cut at every step so the curve is drawn, not a line.
    #[test]
    fn an_eased_fade_leaves_and_arrives_gently() {
        let eased = EdgeFade::eased(36.0, 1.0);
        let off = EdgeFade::new(36.0, 0.0);
        let at = |y: f32| fade_factor(y, 0.0, 400.0, eased, off);
        assert_eq!(at(0.0), 0.0);
        assert_eq!(at(18.0), 0.5);
        assert_eq!(at(36.0), 1.0);
        assert!(
            at(6.0) < 6.0 / 36.0,
            "fainter than a straight ramp near the edge"
        );
        assert!(
            at(30.0) > 30.0 / 36.0,
            "stronger than a straight ramp near the band's end"
        );
        let cuts: Vec<f32> = eased.cuts().collect();
        assert_eq!(cuts.len(), EASE_STEPS + 1);
        assert_eq!((cuts[0], cuts[EASE_STEPS]), (0.0, 36.0));
        assert_eq!(
            EdgeFade::new(36.0, 1.0).cuts().collect::<Vec<_>>(),
            [0.0, 36.0]
        );
        assert_eq!(off.cuts().count(), 0, "no fade, no cuts");
    }

    /// A triangle crossing the band's lines is cut along them, so every piece lies between two
    /// lines and keeps the area and texture mapping of the original.
    #[test]
    fn a_mesh_is_cut_along_the_fade_lines() {
        let v = |x: f32, y: f32| egui::epaint::Vertex {
            pos: egui::pos2(x, y),
            uv: egui::pos2(x / 100.0, y / 100.0),
            color: Color32::WHITE,
        };
        let mesh = egui::Mesh {
            vertices: vec![v(0.0, 0.0), v(100.0, 0.0), v(0.0, 100.0)],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        let cuts = [10.0, 50.0];
        let out = cut_mesh(&mesh, &cuts);
        let area = |a: egui::Pos2, b: egui::Pos2, c: egui::Pos2| {
            ((b - a).x * (c - a).y - (c - a).x * (b - a).y).abs() / 2.0
        };
        let mut total: f32 = 0.0;
        for tri in out.indices.as_chunks::<3>().0 {
            let [a, b, c] = [0, 1, 2].map(|k| out.vertices[tri[k] as usize]);
            total += area(a.pos, b.pos, c.pos);
            let (lo, hi) = (
                a.pos.y.min(b.pos.y).min(c.pos.y),
                a.pos.y.max(b.pos.y).max(c.pos.y),
            );
            for cut in cuts {
                assert!(!(lo < cut - 1e-3 && hi > cut + 1e-3), "a piece spans {cut}");
            }
            for p in [a, b, c] {
                assert!(
                    (p.uv.x - p.pos.x / 100.0).abs() < 1e-4
                        && (p.uv.y - p.pos.y / 100.0).abs() < 1e-4
                );
            }
        }
        assert!((total - 5000.0).abs() < 0.01, "area kept: {total}");
    }

    /// A list at its top does not fade there (the first row is seen in full), and one scrolled
    /// to its end does not fade at the bottom.
    #[test]
    fn a_list_fades_only_toward_what_is_hidden() {
        assert_eq!(
            edge_fades((0.0, 800.0, 400.0), 36.0),
            (0.0, 1.0),
            "at the top"
        );
        assert_eq!(
            edge_fades((400.0, 800.0, 400.0), 36.0),
            (1.0, 0.0),
            "at the end"
        );
        let (above, below) = edge_fades((18.0, 800.0, 400.0), 36.0);
        assert!(
            above > 0.0 && above < 1.0 && below == 1.0,
            "easing in as it scrolls"
        );
        assert_eq!(
            edge_fades((0.0, 300.0, 400.0), 36.0),
            (0.0, 0.0),
            "a short list"
        );
        assert_eq!(
            edge_fades((0.0, 0.0, 0.0), 36.0),
            (0.0, 0.0),
            "not measured yet"
        );
    }

    /// The scroll bar's strip is left alone, a row across the whole width is not.
    #[test]
    fn the_scroll_bar_is_left_out_of_the_fade() {
        let strip_left = 290.0;
        let bar = Rect::from_min_max(egui::pos2(292.0, 0.0), egui::pos2(298.0, 60.0));
        let row = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(298.0, 56.0));
        assert!(in_bar_strip(bar, strip_left));
        assert!(!in_bar_strip(row, strip_left));
    }

    /// A character no font has is painted as a replacement box, so it must measure as one: a
    /// title of them used to measure zero wide, skip truncation, and run out of its card.
    #[test]
    fn a_missing_glyph_measures_as_the_box_it_paints() {
        let ctx = egui::Context::default();
        let font = egui::FontId::proportional(13.0);
        // U+E000 is private use: no face covers it, bundled or ours.
        let (missing, cut) = {
            let mut out = (0.0, String::new());
            let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
                out = (
                    text_width(ui, "\u{E000}\u{E000}\u{E000}", &font),
                    truncate_to(ui, &"\u{E000}".repeat(40), font.clone(), 60.0).into_owned(),
                );
            });
            frame.textures_delta.clear();
            out
        };
        assert!(missing > 10.0, "three boxes measured {missing}");
        assert!(
            cut.ends_with('…') && cut.chars().count() < 40,
            "not truncated: {cut:?}"
        );
    }

    /// A skeleton waits 150 ms into its load and then shows; one with no timed load shows at once.
    #[test]
    fn skeletons_wait_out_fast_loads() {
        assert_eq!(skeleton_wait(10.0, None), None);
        assert!((skeleton_wait(10.05, Some(10.0)).unwrap() - 0.10).abs() < 1e-9);
        assert_eq!(skeleton_wait(10.0 + SKELETON_DELAY, Some(10.0)), None);
        assert_eq!(skeleton_wait(12.0, Some(10.0)), None);
    }

    /// The ring shows 250 ms into a load, and until then says how long is left.
    #[test]
    fn the_loading_ring_waits_a_quarter_second() {
        use std::time::Duration;
        assert_eq!(track_ring_due(Duration::ZERO), Err(TRACK_LOADING_DELAY));
        assert_eq!(
            track_ring_due(Duration::from_millis(100)),
            Err(Duration::from_millis(150))
        );
        assert_eq!(track_ring_due(TRACK_LOADING_DELAY), Ok(()));
        assert_eq!(track_ring_due(Duration::from_secs(2)), Ok(()));
    }

    #[test]
    fn kde_instant_animations_read_as_reduced_motion() {
        assert!(kde_animations_off(
            "[General]\nfoo=1\n[KDE]\nAnimationDurationFactor=0\n"
        ));
        assert!(!kde_animations_off("[KDE]\nAnimationDurationFactor=0.5\n"));
        assert!(!kde_animations_off("[Other]\nAnimationDurationFactor=0\n"));
        assert!(!kde_animations_off(""));
    }

    /// The ring turns once per period, eased: slow out of each start, fastest halfway, slow into
    /// the end — and back at its start when the next turn begins.
    #[test]
    fn the_loading_ring_eases_through_one_turn_per_period() {
        let turn = std::f32::consts::TAU;
        assert!(spinner_angle(0.0).abs() < 1e-6);
        assert!((spinner_angle(SPINNER_PERIOD / 2.0) - turn / 2.0).abs() < 1e-4);
        assert!(
            spinner_angle(SPINNER_PERIOD).abs() < 1e-4,
            "a new turn starts at 0"
        );
        let step = SPINNER_PERIOD / 20.0;
        let early = spinner_angle(step) - spinner_angle(0.0);
        let middle = spinner_angle(SPINNER_PERIOD / 2.0 + step / 2.0)
            - spinner_angle(SPINNER_PERIOD / 2.0 - step / 2.0);
        assert!(
            middle > 3.0 * early,
            "eased: {early} at the start, {middle} midway"
        );
        let mut last = -1.0;
        for i in 0..100 {
            let a = spinner_angle(SPINNER_PERIOD * f64::from(i) / 100.0);
            assert!(a >= last, "never turns backwards");
            last = a;
        }
    }

    /// The trap that closed the song menu whenever the mouse moved: `egui::Memory` keeps
    /// **one** popup slot per viewport, so opening a second popup by id silently evicts the
    /// first. A submenu that used `Popup::open_id` therefore killed its own parent — and since
    /// the parent stopped calling `keep_popup_open`, the whole menu was gone the next frame.
    ///
    /// This asserts egui's behaviour rather than ours, because it is egui's behaviour that made
    /// the bug: anything reaching for `open_id` from inside a popup will hit it again.
    #[test]
    fn opening_a_popup_by_id_evicts_the_one_already_open() {
        let ctx = egui::Context::default();
        let parent = egui::Id::new("parent");
        let child = egui::Id::new("child");

        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::Popup::open_id(ui.ctx(), parent);
        });
        frame.textures_delta.clear();
        assert!(egui::Popup::is_id_open(&ctx, parent));

        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::Popup::open_id(ui.ctx(), child);
        });
        frame.textures_delta.clear();
        assert!(egui::Popup::is_id_open(&ctx, child));
        assert!(
            !egui::Popup::is_id_open(&ctx, parent),
            "one slot: the second open evicted the first"
        );
    }

    /// So no menu may reach for it. Submenus keep their own flag in `ctx.data` — see
    /// [`sub_menu`] — and the source is the only place this invariant can be checked, since a
    /// reintroduced call would look perfectly reasonable at the call site.
    #[test]
    fn no_menu_opens_a_popup_by_id() {
        let source = include_str!("widgets.rs");
        // Only the code, and only real calls: this file necessarily names the function in the
        // comments that explain why not to call it.
        let code = source.split("#[cfg(test)]").next().unwrap_or_default();
        let offender = code
            .lines()
            .find(|l| !l.trim_start().starts_with("//") && l.contains("Popup::open_id("));
        assert!(
            offender.is_none(),
            "a menu is opening a popup through egui's single memory slot again: {offender:?}"
        );
    }

    #[test]
    fn durations_format_like_a_track_list() {
        assert_eq!(fmt_duration(0), "0:00");
        assert_eq!(fmt_duration(9), "0:09");
        assert_eq!(fmt_duration(62), "1:02");
        assert_eq!(fmt_duration(213), "3:33");
        assert_eq!(fmt_duration(3600), "1:00:00");
        assert_eq!(fmt_duration(3725), "1:02:05");
    }

    /// The scroll travel never leaves its rails, turns at both ends, and stands still on a
    /// zero advance — the frame that would otherwise jitter the line by a rounding error.
    #[test]
    fn marquee_travel_stays_on_its_rails() {
        // Mid-travel: advances, same heading.
        assert_eq!(pingpong(10.0, 1.0, 100.0, 5.0), (15.0, 1.0));
        // Past the far end: clamps and turns.
        assert_eq!(pingpong(98.0, 1.0, 100.0, 5.0), (100.0, -1.0));
        // Past the head: clamps and turns.
        assert_eq!(pingpong(2.0, -1.0, 100.0, 5.0), (0.0, 1.0));
        // Zero travel: still.
        assert_eq!(pingpong(0.0, 1.0, 0.0, 0.0), (0.0, -1.0));
    }

    /// The dissolve strips: one at each edge with text hidden past it, and the solid paint keeps
    /// whatever is between them.
    #[test]
    fn a_dissolve_keeps_the_middle_solid() {
        let clip = egui::Rect::from_min_max(egui::pos2(100.0, 0.0), egui::pos2(300.0, 18.0));
        // At rest: trailing only, one fade wide.
        let (main, lead, trail) = dissolve_bands(clip, (0.0, 1.0), 100.0, 300.0);
        assert!(lead.is_none());
        assert_eq!(trail.unwrap().min.x, 276.0);
        assert_eq!(main.min.x, 100.0);
        assert_eq!(main.max.x, 276.0);
        // Mid-scroll: both, and the solid strip is exactly what is left between them.
        let (main, lead, trail) = dissolve_bands(clip, (1.0, 1.0), 100.0, 300.0);
        assert_eq!(lead.unwrap().max.x, 124.0);
        assert_eq!(trail.unwrap().min.x, 276.0);
        assert_eq!(
            main,
            egui::Rect::from_min_max(egui::pos2(124.0, 0.0), egui::pos2(276.0, 18.0))
        );
        // Scrolled to the end: leading only, and the tail paints solid.
        let (main, lead, trail) = dissolve_bands(clip, (1.0, 0.0), 100.0, 300.0);
        assert!(lead.is_some() && trail.is_none());
        assert_eq!(main.max.x, 300.0);
        // A viewport narrower than one fade: the strips meet and the solid strip inverts,
        // which the painters read as "nothing solid to draw".
        let narrow = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(30.0, 18.0));
        let (main, _, _) = dissolve_bands(narrow, (1.0, 1.0), 0.0, 30.0);
        assert!(main.width() <= 0.0);
    }

    /// Each edge fades as deep as the text hidden past it, up to one fade's width: none at the
    /// head at rest, none at the tail at the end of travel.
    #[test]
    fn a_dissolve_fades_only_where_text_is_hidden() {
        assert_eq!(dissolve_depths(0.0, 100.0), (0.0, 1.0));
        assert_eq!(dissolve_depths(100.0, 100.0), (1.0, 0.0));
        assert_eq!(dissolve_depths(50.0, 100.0), (1.0, 1.0));
        assert_eq!(dissolve_depths(12.0, 100.0), (0.5, 1.0));
        assert_eq!(dissolve_depths(94.0, 100.0), (1.0, 0.25));
        // Overflowing by less than a fade: the trailing fade is only as deep as the overflow.
        assert_eq!(dissolve_depths(0.0, 6.0), (0.0, 0.25));
    }

    /// The corner walk the header wash and the corner repaint share: full radius at the edge,
    /// zero once past it, and monotonic in between — the shape that keeps a square shape off
    /// the gutter at a card's rounded corners.
    #[test]
    fn a_corner_walk_starts_at_the_radius_and_ends_flat() {
        let rad = 20.0;
        assert_eq!(corner_inset(rad, 0.0), rad);
        assert_eq!(corner_inset(rad, rad), 0.0);
        assert_eq!(corner_inset(rad, rad + 5.0), 0.0);
        let mut last = rad;
        for i in 0..=20 {
            let inset = corner_inset(rad, i as f32);
            assert!(inset <= last + f32::EPSILON, "the walk doubled back at {i}");
            last = inset;
        }
    }

    /// `card_rect` grows a content rect by exactly the frame's margin, side by side.
    #[test]
    fn a_content_rect_grows_back_to_its_card() {
        let inner = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 220.0));
        let card = card_rect(
            inner,
            egui::Margin {
                left: 8,
                right: 16,
                top: 10,
                bottom: 4,
            },
        );
        assert_eq!(card.min, egui::pos2(2.0, 10.0));
        assert_eq!(card.max, egui::pos2(126.0, 224.0));
    }

    /// Run one `Ui` pass and hand back every mesh it painted, in paint order — how the corner
    /// test below reads what actually landed in the frame rather than trusting the call.
    fn painted_meshes(
        ctx: &egui::Context,
        paint: impl FnOnce(&mut Ui),
    ) -> Vec<std::sync::Arc<egui::Mesh>> {
        let mut painted = Vec::new();
        let mut paint = Some(paint);
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            // `run_ui` takes an `FnMut`, so the one-shot paint is taken rather than moved.
            if let Some(paint) = paint.take() {
                paint(ui);
            }
            ui.ctx().graphics(|layers| {
                if let Some(layer) = layers.get(ui.layer_id()) {
                    for entry in layer.all_entries() {
                        if let egui::Shape::Mesh(mesh) = &entry.shape {
                            painted.push(mesh.clone());
                        }
                    }
                }
            });
        });
        frame.textures_delta.clear();
        painted
    }

    /// The corner repaint covers only what content can reach: the outer strip of each notch,
    /// where the content column stops, is left to the gutter, the edge row runs from the
    /// column's edge out to the card's own corner, and the last row is degenerate where the
    /// silhouette crosses back inside the column. An inset that already clears the arc paints
    /// nothing at all.
    #[test]
    fn a_corner_repaint_covers_the_notch_the_content_can_reach() {
        let card = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 300.0));
        let rows = notch_rows(card, 20.0, 8.0);
        assert_eq!(rows.len(), 4, "one set of rows per corner");
        assert_eq!(rows[0][0], (0.0, 8.0, 20.0));
        assert_eq!(rows[1][0], (0.0, 180.0, 192.0));
        assert_eq!(rows[2][0], (300.0, 8.0, 20.0));
        assert_eq!(rows[3][0], (300.0, 180.0, 192.0));
        assert!(rows[0].iter().all(|&(y, _, _)| (0.0..20.0).contains(&y)));
        assert!(rows[0].iter().all(|&(_, x0, x1)| x1 >= x0));
        let last = *rows[0].last().unwrap();
        assert_eq!(last.0, 4.0, "the silhouette crosses the column at d = 4");
        assert_eq!(last.1, last.2, "the last row tapers to a point");
        assert!(notch_rows(card, 20.0, 20.0).iter().all(Vec::is_empty));
    }

    /// An image that fills its slot keeps the slot's curve; one letterboxed well inside it is
    /// square; one inset a little keeps only what stops its corners poking past the slot's.
    #[test]
    fn a_fitted_image_rounds_only_where_it_meets_the_slot_corners() {
        let slot = Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(116.0, 116.0));
        let r = CornerRadius::same(14);
        assert_eq!(inner_radius(r, slot, slot), r, "fills the slot");
        // A 16:9 still in a square card: 32pt of letterbox above and below.
        let wide = Rect::from_center_size(slot.center(), vec2(116.0, 116.0 * 9.0 / 16.0));
        assert_eq!(
            inner_radius(r, slot, wide),
            CornerRadius::ZERO,
            "letterboxed"
        );
        let nearly = Rect::from_center_size(slot.center(), vec2(116.0, 106.0));
        assert_eq!(
            inner_radius(r, slot, nearly),
            CornerRadius::same(9),
            "5pt in"
        );
    }

    /// The repaint paints a mesh per corner, in the gutter colour — and nothing over a
    /// translucent gutter, where that colour would read as a dark sliver.
    #[test]
    fn a_corner_repaint_paints_a_mesh_per_corner() {
        let ctx = egui::Context::default();
        let card = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 300.0));
        let palette = Palette::dark();
        let meshes = painted_meshes(&ctx, |ui| {
            paint_corner_notches(ui, card, 8.0, &palette);
        });
        assert_eq!(meshes.len(), 4, "one mesh per corner");
        let top_left = &meshes[0];
        assert_eq!(top_left.vertices[0].pos, egui::pos2(8.0, 0.0));
        assert_eq!(top_left.vertices[1].pos, egui::pos2(20.0, 0.0));
        assert!(top_left.vertices.iter().all(|v| v.color == palette.window));

        let acrylic = Palette::dark().with_opacity(crate::settings::Opacity::new(1.0));
        assert!(acrylic.translucent());
        let meshes = painted_meshes(&ctx, |ui| {
            paint_corner_notches(ui, card, 8.0, &acrylic);
        });
        assert!(meshes.is_empty(), "no notches over a translucent gutter");
    }

    /// A card's frame is as tall as its content, so content taller than the card must not reach
    /// the card's own `Ui`: contained, it overflows its child and the parent stays as it was.
    #[test]
    fn contained_content_cannot_grow_the_card() {
        let ctx = egui::Context::default();
        let mut seen = None;
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            if seen.is_some() {
                return;
            }
            let max = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(300.0, 100.0));
            let mut loose = ui.new_child(egui::UiBuilder::new().max_rect(max));
            loose.allocate_space(vec2(300.0, 500.0));
            let mut card = ui.new_child(egui::UiBuilder::new().max_rect(max));
            let inner = contain(&mut card, max, |ui| {
                ui.allocate_space(vec2(300.0, 500.0));
                ui.min_rect().bottom()
            });
            seen = Some((loose.min_rect().bottom(), card.min_rect().bottom(), inner));
        });
        frame.textures_delta.clear();
        let (loose, card, inner) = seen.expect("the pass ran");
        assert!(loose >= 500.0, "without it the card grows: {loose}");
        assert!(card <= 100.0, "contained, the card stays put: {card}");
        assert!(
            inner >= 500.0,
            "the content itself is not squeezed: {inner}"
        );
    }

    /// The translucent clip stops exactly where the notch rows do, so no content the opaque
    /// repaint would have covered is left showing.
    #[test]
    fn the_translucent_clip_clears_the_corners() {
        assert_eq!(corner_clearance(20.0, 8.0), 4.0);
        assert_eq!(corner_clearance(20.0, 20.0), 0.0);
        let card = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 300.0));
        let rows = notch_rows(card, 20.0, 8.0);
        assert_eq!(rows[0].last().unwrap().0, corner_clearance(20.0, 8.0));
    }

    /// A switch flips on click and only on click: hovering and pressing without releasing
    /// must leave it alone.
    #[test]
    fn a_toggle_flips_only_on_a_clean_click() {
        use egui::{Event, PointerButton, Pos2};
        use std::cell::Cell;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let palette = crate::theme::Palette::dark();
        let on = Cell::new(false);
        let at = Cell::new(Pos2::ZERO);
        let mut t = 0.0;
        let mut pass = |events: Vec<Event>| {
            t += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
                time: Some(t),
                events,
                ..Default::default()
            };
            let mut flipped = on.get();
            ctx.run_ui(input, |ui| {
                let resp = toggle(ui, &palette, ui.id().with("test-toggle"), &mut flipped);
                at.set(resp.rect.center());
            })
            .drop_without_applying_deltas();
            on.set(flipped);
        };
        let click = |pos: Pos2, pressed: bool| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        pass(vec![]);
        pass(vec![Event::PointerMoved(at.get())]);
        assert!(!on.get(), "a hover flipped the switch");
        pass(vec![click(at.get(), true)]);
        assert!(!on.get(), "a press without release flipped the switch");
        pass(vec![click(at.get(), false)]);
        assert!(on.get(), "a clean click did not flip the switch");
        pass(vec![click(at.get(), true)]);
        pass(vec![click(at.get(), false)]);
        assert!(!on.get(), "a second click did not flip it back");
    }

    /// The ••• menu must open *visibly* on a primary click: open state alone is not enough.
    ///
    /// This is the regression test for a menu that toggled open on left click yet never
    /// rendered. `Toggle` flips the open flag but records no pointer position, so a
    /// pointer-anchored popup has no rect and `show` skips it — logically open, visibly
    /// absent. Button-anchored, both hold. A headless context with synthetic press/release
    /// events drives the real `page_menu`, and the test asserts open state *and* a resolved
    /// position.
    #[test]
    fn page_menu_opens_visibly_on_primary_click() {
        use egui::{Event, PointerButton, Pos2};
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        egui_extras::install_image_loaders(&ctx);
        crate::icons::register(&ctx);
        let palette = crate::theme::Palette::dark();
        let button_rect = egui::Rect::from_min_size(Pos2::new(600.0, 220.0), vec2(30.0, 30.0));
        let at = button_rect.center();
        let mut panel_id = None;
        let mut t = 0.0;
        let mut pass = |events: Vec<Event>| {
            t += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
                time: Some(t),
                events,
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                panel_id = Some(ui.id());
                let response = ui.interact(button_rect, ui.id().with("dots"), egui::Sense::click());
                let _ = page_menu(
                    ui,
                    &palette,
                    &response,
                    PageMenu {
                        page_id: "test",
                        tracks: &[],
                        artist_id: None,
                        playlists: &[],
                        signed_in: false,
                        share_url: String::new(),
                        has_info: false,
                        manage: false,
                        remove_from_library: false,
                        context_only: false,
                        radio_id: None,
                        save_as_new: None,
                        tracks_pending: None,
                    },
                );
            })
            .drop_without_applying_deltas();
        };
        let click = |pressed: bool| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        pass(vec![Event::PointerMoved(at)]);
        pass(vec![click(true)]);
        pass(vec![click(false)]);
        // One more quiet pass: the release frame toggles it open, and it must stay open.
        pass(vec![]);

        let id = panel_id
            .expect("the panel ran")
            .with(("ytm-page-menu", "test"));
        assert!(
            egui::Popup::is_id_open(&ctx, id),
            "a primary click on ••• must open the page menu"
        );
        // Rendered, not just open: a pointer-anchored popup with no recorded position
        // bails out of `show` before creating its area, so only a shown area leaves a
        // response behind.
        assert!(
            ctx.read_response(id).is_some(),
            "an open page menu that never renders is the bug"
        );
    }

    /// The duration is a list-wide decision, not a per-row one. Where the rating thumbs share its
    /// corner — a narrow, ratable, signed-in row — no row in that state paints a duration, so a
    /// list cannot mix rows that show one with rows that do not. Everywhere else it stays.
    #[test]
    fn only_a_narrow_ratable_signed_in_row_shares_the_duration_corner() {
        assert!(corner_shares_duration(400.0, true, true, true));
        // Wide enough for the thumbs' own zone: the duration is always painted.
        assert!(!corner_shares_duration(600.0, true, true, true));
        // The queue turns rating off, so there are no thumbs to step aside for.
        assert!(!corner_shares_duration(400.0, false, true, true));
        // Signed out there is nothing to draw in the corner.
        assert!(!corner_shares_duration(400.0, true, true, false));
        // No menu, no rating (history's album and playlist rows).
        assert!(!corner_shares_duration(400.0, true, false, true));
        // Exactly at the threshold the zone still fits; sharing starts below it.
        assert!(corner_shares_duration(519.0, true, true, true));
        assert!(!corner_shares_duration(520.0, true, true, true));
    }

    /// The cover's play button is a control of its own: a click inside a square row's artwork
    /// plays, and it must read that way even in double-click-to-play mode, where the row's own
    /// single click only selects. A click anywhere else on the row is not the button's, and a
    /// 16:9 still has no button to press.
    #[test]
    fn only_a_click_on_a_square_rows_cover_presses_its_play_button() {
        let art = Rect::from_min_size(egui::pos2(10.0, 4.0), vec2(40.0, 40.0));
        let inside = Some(egui::pos2(30.0, 24.0));
        let outside = Some(egui::pos2(300.0, 24.0));
        assert!(on_cover(ArtShape::Square, art, inside, true));
        // A click that was not a click at all — a drag, or a hover — never presses it.
        assert!(!on_cover(ArtShape::Square, art, inside, false));
        // Somewhere else on the row: the row's own click, double-click setting and all.
        assert!(!on_cover(ArtShape::Square, art, outside, true));
        // A video's still carries no overlay, so it has nothing to press.
        assert!(!on_cover(ArtShape::Wide, art, inside, true));
        // A click with no recorded position cannot be attributed to the cover.
        assert!(!on_cover(ArtShape::Square, art, None, true));
    }

    /// The cover's button, driven for real: a row is laid out in a headless `Context`, the pointer
    /// is moved onto the cover and clicked once, and the row must play. Then a single click on the
    /// row's body — the same frame sequence, the same setting — must only select. This is the half
    /// `on_cover` cannot prove: that the click's position and the row's own clicked flag line up on
    /// the frame the release lands.
    #[test]
    fn a_single_click_on_the_cover_plays_in_double_click_mode() {
        use std::cell::Cell;

        use egui::{Event, PointerButton, Pos2};

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let palette = crate::theme::Palette::dark();
        // The setting the cover exists to be an exception to.
        set_double_click_play(&ctx, true);

        let track = crate::model::Track {
            video_id: "cover-test".into(),
            title: "A Song".into(),
            artists: vec![crate::model::ArtistLink::plain("Someone")],
            album: None,
            album_id: None,
            duration: Some(200),
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
        };
        let playlists: [crate::model::PlaylistRef; 0] = [];

        let mut t = 0.0;
        let activate = Cell::new(false);
        let cover = Cell::new(Pos2::ZERO);
        let body = Cell::new(Pos2::ZERO);
        let mut pass = |events: Vec<Event>| {
            t += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
                time: Some(t),
                events,
                ..Default::default()
            };
            let mut fired = false;
            ctx.run_ui(input, |ui| {
                let hit = track_row(
                    ui,
                    &palette,
                    TrackRow {
                        number: None,
                        rank: None,
                        trend: crate::model::ChartTrend::Unknown,
                        title: &track.title,
                        album_id: None,
                        subtitle: plain_subtitle("Someone"),
                        duration: track.duration,
                        plays: None,
                        explicit: false,
                        active: false,
                        playing: false,
                        badge: None,
                        art: None,
                        shape: ArtShape::Square,
                        hide_art: false,
                        id_salt: 0,
                        menu: Some(RowMenu {
                            track: &track,
                            playlist_id: None,
                            playlists: &playlists,
                            signed_in: true,
                            queue_index: None,
                        }),
                        like: track.like,
                        show_rating: false,
                        quiet_like: false,
                        add_to: None,
                    },
                );
                fired = hit.activate;
                let rect = hit.response.rect;
                cover.set(egui::pos2(
                    rect.left() + crate::theme::ART_SIZE / 2.0,
                    rect.center().y,
                ));
                body.set(egui::pos2(rect.right() - 40.0, rect.center().y));
            })
            .drop_without_applying_deltas();
            activate.set(fired);
        };
        let click = |pos: Pos2, pressed: bool| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };

        // Lay the row out once so the cover's position is known.
        pass(vec![]);
        // One clean click, on the cover.
        pass(vec![Event::PointerMoved(cover.get())]);
        pass(vec![click(cover.get(), true)]);
        pass(vec![click(cover.get(), false)]);
        assert!(
            activate.get(),
            "the cover's own button did not play on a single click in double-click mode"
        );
        assert!(
            !is_selected(&ctx, "cover-test"),
            "clicking the cover also marked the row as merely selected"
        );

        // The same click elsewhere on the row: double-click mode means select, not play.
        pass(vec![Event::PointerMoved(body.get())]);
        pass(vec![click(body.get(), true)]);
        pass(vec![click(body.get(), false)]);
        assert!(
            !activate.get(),
            "a body click played, but double-click mode should only select it"
        );
        assert!(
            is_selected(&ctx, "cover-test"),
            "a single body click did not select the row"
        );
    }

    /// The ink a title's line ends at: its baseline for a name with no descenders, lower for
    /// one with a tail, and never past the line box it sits in. This is what a credit line
    /// hangs off, so a name that has nothing to clear pulls its credits up close.
    #[test]
    fn a_titles_ink_stops_at_its_baseline_unless_it_has_a_tail() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let measured = std::cell::Cell::new((0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32));
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = crate::theme::bold(40.0);
            let caps = ink_drop(ui, "OASIS", &font);
            let tailed = ink_drop(ui, "Bunny", &font);
            // Half the line box is where a fully-descended glyph would reach; the caps stop
            // well short of it, and the tail comes most of the way down.
            let half_box =
                ui.fonts_mut(|f| {
                    f.layout_no_wrap("OASIS".to_owned(), font.clone(), Color32::PLACEHOLDER)
                })
                .size()
                .y / 2.0;
            measured.set((caps, tailed, half_box, ink_drop(ui, "", &font)));
        })
        .drop_without_applying_deltas();
        let (caps, tailed, half_box, empty) = measured.get();
        assert!(
            caps > 0.0 && caps < half_box - 4.0,
            "capitals should stop above the box's descent: {caps} of {half_box}"
        );
        assert!(
            tailed > caps + 3.0,
            "a tail should reach lower than capitals: {tailed} vs {caps}"
        );
        assert!(
            tailed <= half_box,
            "nothing should reach past the line box: {tailed} of {half_box}"
        );
        assert_eq!(
            empty, 0.0,
            "nothing painted is no ink, not a negative reach"
        );
    }

    /// The type line above a title: what stays put across title sizes is the *cap line* — the
    /// top of the name as the eye reads it — so the air between label and name never grows
    /// with the name. (Centring the line box instead let the air grow with the size, which is
    /// what the 40pt ceiling made obvious.)
    #[test]
    fn a_titles_cap_line_stays_under_its_label_at_any_size() {
        let baseline = 12.37;
        let air = 8.0;
        let cap_line =
            |size: f32| title_centre_under_label(baseline, air, size) - size * TITLE_CAP_RISE;
        assert_eq!(cap_line(24.0), baseline + air);
        assert_eq!(cap_line(40.0), baseline + air);
        assert_eq!(cap_line(32.0), cap_line(24.0));
        // And the name grows downward from it: a bigger size puts its centre lower.
        assert!(
            title_centre_under_label(baseline, air, 40.0)
                > title_centre_under_label(baseline, air, 24.0)
        );
    }

    /// The detail header's title: the full size while it fits, stepping down while it does not,
    /// and the floor once even that is too wide — where the caller's own truncation takes over.
    #[test]
    fn a_title_fits_by_shrinking_before_it_ellipsizes() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let long = "when we all fall asleep, where do we go?";
        let measured = std::cell::Cell::new((0.0_f32, 0.0_f32, 0.0_f32, false));
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let short = fit_size(ui, "OASIS", 400.0, 32.0, 24.0, crate::theme::bold);
            // Two columns measured off the font itself rather than guessed: one that only a
            // 26pt title fits, and one that nothing does.
            let at_26 = text_width(ui, long, &crate::theme::bold(26.0));
            let at_24 = text_width(ui, long, &crate::theme::bold(24.0));
            let stepped = fit_size(ui, long, at_26 + 5.0, 32.0, 24.0, crate::theme::bold);
            let floored = fit_size(ui, long, at_24 - 5.0, 32.0, 24.0, crate::theme::bold);
            // The size it picked is the *largest* that fits: one step up does not.
            let fits = text_width(ui, long, &crate::theme::bold(stepped)) <= at_26 + 5.0;
            let one_up = text_width(ui, long, &crate::theme::bold(stepped + 2.0)) > at_26 + 5.0;
            measured.set((short, stepped, floored, fits && one_up));
        })
        .drop_without_applying_deltas();
        let (short, stepped, floored, largest) = measured.get();
        assert_eq!(short, 32.0, "a short name takes the full size");
        assert_eq!(
            stepped, 26.0,
            "a name that only fits two steps down should land there, not at the floor"
        );
        assert!(largest, "the step chosen was not the largest that fits");
        assert_eq!(
            floored, 24.0,
            "past the floor the caller's own truncation takes over"
        );
    }

    /// A hidden cover leaves its play control in the number's slot: a single click on the
    /// leading column plays even in double-click mode and does not mark the row as merely
    /// selected, while a click just past it — where the cover's own hit rect used to sit — is
    /// an ordinary body click that only selects.
    #[test]
    fn an_artless_row_plays_from_the_number_slot_the_cover_left_behind() {
        use std::cell::Cell;

        use egui::{Event, PointerButton, Pos2};

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let palette = crate::theme::Palette::dark();
        set_double_click_play(&ctx, true);

        let track = crate::model::Track {
            video_id: "artless-test".into(),
            title: "A Song".into(),
            artists: vec![crate::model::ArtistLink::plain("Someone")],
            album: None,
            album_id: None,
            duration: Some(200),
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
        };
        let playlists: [crate::model::PlaylistRef; 0] = [];

        let mut t = 0.0;
        let activate = Cell::new(false);
        let number = Cell::new(Pos2::ZERO);
        let past = Cell::new(Pos2::ZERO);
        let mut pass = |events: Vec<Event>| {
            t += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
                time: Some(t),
                events,
                ..Default::default()
            };
            let mut fired = false;
            ctx.run_ui(input, |ui| {
                let hit = track_row(
                    ui,
                    &palette,
                    TrackRow {
                        number: Some(1),
                        rank: None,
                        trend: crate::model::ChartTrend::Unknown,
                        title: &track.title,
                        album_id: None,
                        subtitle: plain_subtitle("Someone"),
                        duration: track.duration,
                        plays: None,
                        explicit: false,
                        active: false,
                        playing: false,
                        badge: None,
                        art: None,
                        shape: ArtShape::Square,
                        hide_art: true,
                        id_salt: 0,
                        menu: Some(RowMenu {
                            track: &track,
                            playlist_id: None,
                            playlists: &playlists,
                            signed_in: true,
                            queue_index: None,
                        }),
                        like: track.like,
                        show_rating: false,
                        quiet_like: false,
                        add_to: None,
                    },
                );
                fired = hit.activate;
                let rect = hit.response.rect;
                number.set(egui::pos2(
                    rect.left() + crate::theme::ROW_LEFT_INSET + crate::theme::ROW_NUMBER_W / 2.0,
                    rect.center().y,
                ));
                // Just past the number's column: inside the slot the cover used to occupy,
                // and outside the control that replaced it.
                past.set(egui::pos2(
                    rect.left() + crate::theme::ROW_LEFT_INSET + crate::theme::ROW_NUMBER_W + 6.0,
                    rect.center().y,
                ));
            })
            .drop_without_applying_deltas();
            activate.set(fired);
        };
        let click = |pos: Pos2, pressed: bool| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };

        pass(vec![]);
        // The number's own slot: the row's play button, double-click mode or not.
        pass(vec![Event::PointerMoved(number.get())]);
        pass(vec![click(number.get(), true)]);
        pass(vec![click(number.get(), false)]);
        assert!(
            activate.get(),
            "the number slot did not play on a single click"
        );
        assert!(
            !is_selected(&ctx, "artless-test"),
            "playing from the number slot also marked the row as merely selected"
        );

        // A click past the control is ordinary row body: select, do not play.
        pass(vec![Event::PointerMoved(past.get())]);
        pass(vec![click(past.get(), true)]);
        pass(vec![click(past.get(), false)]);
        assert!(
            !activate.get(),
            "a click past the number slot played in double-click mode"
        );
        assert!(
            is_selected(&ctx, "artless-test"),
            "a body click did not select the row"
        );
    }

    /// An art-less row's number gives way to the play glyph while the pointer is over it — the
    /// hover play button the cover would otherwise carry. Read off what was painted: at rest the
    /// number is the only text in its column, and on hover it is gone.
    #[test]
    fn an_artless_row_gives_its_number_to_the_play_glyph_on_hover() {
        use std::cell::Cell;

        use egui::{Event, Pos2};

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let palette = crate::theme::Palette::dark();

        let track = crate::model::Track {
            video_id: "hover-glyph-test".into(),
            title: "A Song".into(),
            artists: vec![crate::model::ArtistLink::plain("Someone")],
            album: None,
            album_id: None,
            duration: Some(200),
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
        };
        let playlists: [crate::model::PlaylistRef; 0] = [];

        let mut t = 0.0;
        let numbers = Cell::new(0usize);
        let centre = Cell::new(Pos2::ZERO);
        let mut pass = |events: Vec<Event>| {
            t += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
                time: Some(t),
                events,
                ..Default::default()
            };
            let mut found = 0;
            let mut at = Pos2::ZERO;
            let mut frame = ctx.run_ui(input, |ui| {
                let hit = track_row(
                    ui,
                    &palette,
                    TrackRow {
                        number: Some(7),
                        rank: None,
                        trend: crate::model::ChartTrend::Unknown,
                        title: &track.title,
                        album_id: None,
                        subtitle: plain_subtitle("Someone"),
                        duration: track.duration,
                        plays: None,
                        explicit: false,
                        active: false,
                        playing: false,
                        badge: None,
                        art: None,
                        shape: ArtShape::Square,
                        hide_art: true,
                        id_salt: 0,
                        menu: Some(RowMenu {
                            track: &track,
                            playlist_id: None,
                            playlists: &playlists,
                            signed_in: true,
                            queue_index: None,
                        }),
                        like: track.like,
                        show_rating: false,
                        quiet_like: false,
                        add_to: None,
                    },
                );
                at = hit.response.rect.center();
                ui.ctx().graphics(|layers| {
                    if let Some(layer) = layers.get(ui.layer_id()) {
                        for entry in layer.all_entries() {
                            if let egui::Shape::Text(text) = &entry.shape {
                                let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                                if rect.right()
                                    <= hit.response.rect.left()
                                        + crate::theme::ROW_LEFT_INSET
                                        + crate::theme::ROW_NUMBER_W
                                {
                                    found += 1;
                                }
                            }
                        }
                    }
                });
            });
            frame.textures_delta.clear();
            numbers.set(found);
            centre.set(at);
        };

        pass(vec![]);
        assert_eq!(numbers.get(), 1, "the number should lead the row at rest");
        pass(vec![Event::PointerMoved(centre.get())]);
        pass(vec![Event::PointerMoved(centre.get())]);
        assert_eq!(
            numbers.get(),
            0,
            "the number stayed under the pointer instead of giving way to the play glyph"
        );
    }
}
