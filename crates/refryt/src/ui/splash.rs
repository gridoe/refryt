//! The startup splash: a saved session is only a claim until YouTube confirms it, and the
//! signed-out shell is the wrong thing to show while that round trip is in flight — a chip wired
//! to Settings, "No playlists yet.", a "Sign in" prompt, all a moment away from being wrong. The
//! same goes for the half-built signed-in shell the moment after the session settles, with the
//! rail painted and the landing feed still a grid of skeletons, so the splash covers that too.
//!
//! Minimal by choice: the logo, centred, pulsing. No wordmark, no caption, no progress bar —
//! there is nothing to report beyond "not ready yet". `App::ui` returns after calling this, so
//! none of the shell is drawn; the window chrome is painted by hand here because the window is
//! undecorated, and skipping the shell would otherwise leave it undraggable and unclosable.

use std::time::Duration;

use egui::{Color32, Rect, Ui, Vec2, pos2};

use crate::app::App;
use crate::chrome;
use crate::theme;

/// The logo's edge, in points. Large enough to read as a splash rather than a footer icon.
const LOGO: f32 = 96.0;

/// Seconds per pulse. Opacity only — motion would be a second thing to look at, and the logo is
/// the whole screen.
const PULSE_PERIOD: f64 = 3.0;

pub(crate) fn screen(app: &mut App, ui: &mut Ui, maximized: bool) {
    let rect = ui.ctx().content_rect();

    // Installed first, exactly as the top bar does it, so the window buttons painted after win
    // the pointer instead of the drag surface swallowing their clicks.
    chrome::drag_surface(ui, rect, app.decorated, "splash");
    // macOS keeps the system's traffic lights, placed for every frame in `App::ui`.
    if !app.decorated && !chrome::NATIVE_WINDOW_BUTTONS {
        // The shell's top bar sits inside its frame's `GUTTER + 6` inner margin, so its window
        // buttons are that far in from the window edge; insetting the strip by the same amount
        // keeps them from jumping sideways when the splash hands off to the shell.
        let strip = Rect::from_min_size(
            rect.left_top(),
            Vec2::new(rect.width() - (theme::GUTTER + 6.0), theme::TITLEBAR_HEIGHT),
        );
        // A titlebar-height strip, so the buttons land in the window's top-right corner rather
        // than vertically centred in the whole splash.
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(strip)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if chrome::window_buttons(&mut right, &app.palette) {
            app.request_close(ui.ctx());
        }
    }
    if !app.decorated && !maximized {
        chrome::resize_handles(ui);
    }

    if app.logo.is_none() {
        app.logo = crate::logo_texture(ui.ctx());
    }
    if let Some(logo) = &app.logo {
        let t = ui.input(|i| i.time);
        let phase = (t * std::f64::consts::TAU / PULSE_PERIOD).sin();
        let alpha = ((0.55 + 0.45 * phase) * 255.0) as u8;
        ui.painter().image(
            logo.id(),
            Rect::from_center_size(rect.center(), Vec2::splat(LOGO)),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::from_white_alpha(alpha),
        );
    }

    // The pulse is the only thing moving; without a steady request the app would idle between
    // frames and the fade would step once per event instead.
    ui.ctx().request_repaint_after(Duration::from_millis(16));
}
