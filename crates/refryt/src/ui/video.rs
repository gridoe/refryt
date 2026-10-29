//! The music-video view: a black content card with the software-rendered frame.
//!
//! The frame is rendered at the video's own size — the software renderer scales on the CPU,
//! single-threaded, so letting the GPU fit it into the card is the cheap direction — and
//! centred inside the card with its own aspect.

use crate::app::App;
use crate::player::Player;
use crate::theme;

/// The longest side of the frame buffer, in physical pixels. The 720p selector already meets
/// it; this bounds anything unusual that slips through.
const MAX_PX: f32 = 1280.0;

pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    let radius = egui::CornerRadius::same(theme::radii().panel);
    ui.painter().rect_filled(rect, radius, egui::Color32::BLACK);
    paint_frame(app, ui, rect, radius);

    // The surface is a transport control, like the player bar's play button: click to pause,
    // double-click for fullscreen, Escape back out of it.
    let response = ui.interact(rect, ui.id().with("video-surface"), egui::Sense::click());
    let fullscreen = crate::window_fullscreen::is(ui.ctx());
    if response.double_clicked() {
        crate::window_fullscreen::set(ui.ctx(), !fullscreen);
    } else if response.clicked() {
        app.toggle_playback();
    } else if fullscreen && ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        crate::window_fullscreen::set(ui.ctx(), false);
    }
}

/// The video frame (or the track's still while it loads) fitted inside `rect`, rounded to
/// `radius` where it reaches the corners, with the loading caption. Paints no backdrop: the
/// caller owns what is behind it.
pub(crate) fn paint_frame(
    app: &mut App,
    ui: &mut egui::Ui,
    rect: egui::Rect,
    radius: egui::CornerRadius,
) {
    // The track's wide still: the placeholder while the video resolves, and the backdrop if no
    // frame ever arrives. Cloned out of the cache so the surface below can borrow `app`.
    let url = app.now.track.as_ref().and_then(|t| t.thumbnail.clone());
    let still = super::art_texture_wide(
        &mut app.images,
        &app.backend,
        ui.ctx(),
        url.as_ref(),
        rect.width(),
    )
    .cloned();

    // Read the size before borrowing the surface: the video's own when one is loaded, the
    // card's otherwise (mpv letterboxes into it until the first frame reports a size).
    let native = app.player.as_ref().and_then(Player::video_size);
    let ppp = ui.ctx().pixels_per_point();
    let size = match native {
        Some((w, h)) => capped(w, h),
        None => {
            let width = (rect.width() * ppp).clamp(2.0, MAX_PX);
            let height = (width * (rect.height() / rect.width().max(1.0))).clamp(2.0, MAX_PX);
            (width.round() as usize, height.round() as usize)
        }
    };

    let frame = app
        .video
        .as_mut()
        .and_then(|surface| surface.frame(ui, size));
    // While the next video resolves, the previous frame is still in the texture; the still of
    // the track that is actually loading is the honest thing to show.
    let painted = if app.now.resolving {
        still.or(frame)
    } else {
        frame.or(still)
    };
    if let Some(texture) = painted {
        let fitted = fit_rect(rect, texture.size_vec2());
        egui::Image::from_texture(&texture)
            .fit_to_exact_size(fitted.size())
            // The card is already rounded; the video only rounds where it reaches the card's
            // corners. Letterboxed, with black above and below, its own corners are square.
            .corner_radius(crate::widgets::inner_radius(radius, rect, fitted))
            .paint_at(ui, fitted);
    }

    if app.now.resolving {
        loading_caption(app, ui, rect);
    }
}

/// A quiet caption while the video is being fetched, low in the card so the still stays readable.
fn loading_caption(app: &App, ui: &egui::Ui, rect: egui::Rect) {
    let galley = ui.painter().layout_no_wrap(
        "Loading video…".to_owned(),
        theme::regular(12.0),
        app.palette.dim,
    );
    let pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.bottom() - 20.0 - galley.size().y,
    );
    ui.painter().galley(pos, galley, app.palette.dim);
}

/// Scale a video size down so neither side exceeds [`MAX_PX`], keeping its aspect.
fn capped(w: usize, h: usize) -> (usize, usize) {
    let longest = w.max(h) as f32;
    if longest <= MAX_PX {
        return (w, h);
    }
    let scale = MAX_PX / longest;
    (
        ((w as f32 * scale).round() as usize).max(2),
        ((h as f32 * scale).round() as usize).max(2),
    )
}

/// The largest rect with `texture`'s aspect that fits inside `card`, centred.
fn fit_rect(card: egui::Rect, texture: egui::Vec2) -> egui::Rect {
    if texture.x <= 0.0 || texture.y <= 0.0 {
        return card;
    }
    let scale = (card.width() / texture.x).min(card.height() / texture.y);
    egui::Rect::from_center_size(card.center(), texture * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A landscape video fills the card's width and letterboxes; a portrait one fills its
    /// height. Either way the aspect is the video's, never the card's.
    #[test]
    fn the_frame_keeps_its_aspect_inside_the_card() {
        let card = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));

        let landscape = fit_rect(card, egui::vec2(1280.0, 720.0));
        assert_eq!(landscape.width(), 800.0);
        assert_eq!(landscape.height(), 450.0);
        assert_eq!(landscape.center(), card.center());

        let portrait = fit_rect(card, egui::vec2(720.0, 1280.0));
        assert_eq!(portrait.height(), 600.0);
        assert!(portrait.width() < 400.0);
        assert_eq!(portrait.center(), card.center());
    }

    #[test]
    fn a_frame_with_no_size_falls_back_to_the_card() {
        let card = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        assert_eq!(fit_rect(card, egui::Vec2::ZERO), card);
    }

    /// The render buffer never exceeds the cap on either side, whichever way the video is.
    #[test]
    fn the_frame_buffer_is_capped_on_its_longest_side() {
        assert_eq!(capped(1920, 1080), (1280, 720));
        assert_eq!(capped(2560, 1440), (1280, 720));
        // Already within the cap: untouched, portrait or not.
        assert_eq!(capped(640, 360), (640, 360));
        assert_eq!(capped(720, 1280), (720, 1280));
    }
}
