//! The keyboard-shortcut cheatsheet: one centered, dimmed modal, opened by `?`/F1 or from the
//! account menu and Settings. A single list here is the source of truth, so the modal cannot
//! disagree with `App::handle_keys` for long.

use crate::app::App;
use crate::theme;
use crate::widgets;

/// `(keys, what it does)`, in reading order. `⌘` is drawn for the modifier egui calls `command`
/// (Ctrl on Windows/Linux, ⌘ on macOS) — the same cross-platform convention the shortcuts follow.
const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "Play / pause"),
    ("↑ / ↓", "Previous / next track"),
    ("M", "Mute"),
    ("F", "Fullscreen player"),
    ("Esc", "Leave fullscreen player"),
    ("⌘/Ctrl + S", "Shuffle"),
    ("⌘/Ctrl + R", "Repeat"),
    ("⌘/Ctrl + K", "Focus search"),
    ("F1  or  ?", "This help"),
    ("F2", "Performance overlay"),
];

/// What only this platform has, shown after the shared ones. macOS: the shortcuts the menu bar's
/// items hold (`menu_bar`).
#[cfg(target_os = "macos")]
const PLATFORM_SHORTCUTS: &[(&str, &str)] = &[
    ("⌘ + ← / →", "Previous / next track"),
    ("⌘ + ↑ / ↓", "Volume up / down"),
    ("⌘ + 1 / 2 / 3 / 4", "Home, Explore, Library, History"),
    ("⌘ + F", "Focus search"),
    ("⌘ + ,", "Settings"),
    ("⌘ + M  /  ⌘ + W", "Minimize / close window"),
];
/// Elsewhere: the volume keys, which macOS has as ⌘↑/↓ in its menu bar instead.
#[cfg(not(target_os = "macos"))]
const PLATFORM_SHORTCUTS: &[(&str, &str)] = &[("Ctrl + ↑ / ↓", "Volume up / down")];

pub(crate) fn modal(app: &mut App, ui: &mut egui::Ui) {
    if !app.help_open {
        return;
    }
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let p = app.palette;
    let row_h = 26.0;
    let width = 380.0_f32.min(screen.width() - 40.0);
    // The card height is not a constant we can compute: the real content height depends on egui's
    // item spacing between the title, every row and the button, which is exactly what a hand-summed
    // formula got wrong — the last row and Close spilled past the card's bottom edge. So the body
    // measures itself and writes the result here, and the next frame's card wraps it exactly.
    // The fallback is only ever seen on the very first frame the modal opens.
    let height_id = egui::Id::new("refryt-help-height");
    let fallback = 20.0
        + 10.0
        + (SHORTCUTS.len() + PLATFORM_SHORTCUTS.len()) as f32 * row_h
        + 6.0
        + 30.0
        + 32.0;
    let height = ui
        .ctx()
        .data(|d| d.get_temp::<f32>(height_id))
        .unwrap_or(fallback);

    let mut close = false;
    let mut dismissed = false;
    egui::Area::new(egui::Id::new("ytm-help-modal"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            let (full, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(full, 0.0, egui::Color32::from_black_alpha(150));

            let card = egui::Rect::from_center_size(
                full.center() - egui::vec2(0.0, 12.0),
                egui::vec2(width, height),
            );
            // Only a click outside the card dismisses — the shortcut rows below are painted with
            // `Sense::hover()` (they don't need clicks), so without this check a click anywhere
            // on one of them fell through to this backdrop and closed the modal.
            if response.clicked()
                && response
                    .interact_pointer_pos()
                    .is_some_and(|pos| !card.contains(pos))
            {
                dismissed = true;
            }
            let painter = ui.painter();
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
                p.overlay,
            );

            let mut body = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card.shrink(16.0))
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            body.label(
                egui::RichText::new("Keyboard shortcuts")
                    .font(theme::semibold(15.0))
                    .color(p.text),
            );
            body.add_space(10.0);
            for (keys, what) in SHORTCUTS.iter().chain(PLATFORM_SHORTCUTS) {
                let (rect, _) = body.allocate_exact_size(
                    egui::vec2(body.available_width(), row_h),
                    egui::Sense::hover(),
                );
                let painter = body.painter();
                painter.text(
                    rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    *keys,
                    theme::medium(12.5),
                    p.text,
                );
                painter.text(
                    rect.right_center(),
                    egui::Align2::RIGHT_CENTER,
                    *what,
                    theme::regular(12.5),
                    p.secondary,
                );
            }
            body.add_space(6.0);
            body.allocate_ui(egui::vec2(body.available_width(), 30.0), |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::chip(ui, &p, "Close", true).clicked() {
                        close = true;
                    }
                });
            });
            if body.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            // What the body actually needed, plus the card's own 16pt top and bottom padding.
            // Stored for the next frame (a height change needs a repaint to be seen).
            let measured = body.min_rect().height() + 32.0;
            if (measured - height).abs() > 0.5 {
                ui.ctx().data_mut(|d| d.insert_temp(height_id, measured));
                ui.ctx().request_repaint();
            }
        });
    if close || dismissed {
        app.help_open = false;
    }
}
