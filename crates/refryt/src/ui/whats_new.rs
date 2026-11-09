//! What's new: this version's changes in a window, shown once after an update and from the
//! "?" beside the version in Settings → About. The notes come from `crate::changelog`.

use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat};

use crate::app::App;
use crate::changelog::{self, Change, Style};
use crate::theme::{self, Palette};
use crate::widgets;

/// One change as two lines of type: its lead in the text colour, then the rest a step quieter,
/// with any bold or code in it set apart.
fn change_row(ui: &mut egui::Ui, p: &Palette, change: &Change) {
    ui.horizontal_top(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(egui::pos2(dot.left() + 4.0, dot.top() + 9.0), 2.0, p.dim);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if let Some(title) = &change.title {
                ui.label(
                    egui::RichText::new(title)
                        .font(theme::semibold(13.0))
                        .color(p.text),
                );
            }
            let mut job = LayoutJob::default();
            job.wrap.max_width = ui.available_width();
            for (style, text) in changelog::spans(&change.text) {
                let (font, color) = match style {
                    Style::Plain => (theme::regular(12.5), p.secondary),
                    Style::Bold => (theme::medium(12.5), p.text),
                    Style::Code => (FontId::monospace(11.5), p.text),
                };
                job.append(text, 0.0, TextFormat::simple(font, color));
            }
            ui.label(job);
        });
    });
}

/// A release's changes, one under the other.
fn change_list(ui: &mut egui::Ui, p: &Palette, changes: &[Change]) {
    for (i, change) in changes.iter().enumerate() {
        if i > 0 {
            ui.add_space(10.0);
        }
        change_row(ui, p, change);
    }
}

/// The window with this version's changes. It also records the version as seen, so it opens by
/// itself only once after an update.
pub(crate) fn modal(app: &mut App, ui: &mut egui::Ui) {
    let current = env!("CARGO_PKG_VERSION");
    if app.settings.seen_version.as_deref() != Some(current) {
        let ctx = ui.ctx().clone();
        app.update_settings(&ctx, |s| s.seen_version = Some(current.to_owned()));
    }
    // Something more pressing has the screen: the update and crash windows go first.
    if !app.whats_new_open || app.update.open {
        return;
    }
    let Some(release) = changelog::this_version() else {
        app.whats_new_open = false;
        return;
    };
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let p = app.palette;
    let width = 560.0_f32.min(screen.width() - 40.0);
    let height = 520.0_f32.min(screen.height() - 80.0);

    let mut close = false;
    egui::Area::new(egui::Id::new("whats-new-modal"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            let (full, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(full, 0.0, Color32::from_black_alpha(150));
            let card = egui::Rect::from_center_size(
                full.center() - egui::vec2(0.0, 12.0),
                egui::vec2(width, height),
            );
            if response.clicked()
                && response
                    .interact_pointer_pos()
                    .is_some_and(|pos| !card.contains(pos))
            {
                close = true;
            }
            let painter = ui.painter();
            for (grow, alpha) in [(14.0, 24), (7.0, 40), (2.0, 60)] {
                painter.rect_filled(
                    card.expand(grow),
                    egui::CornerRadius::same(theme::radii().popup + grow as u8),
                    Color32::from_black_alpha(alpha),
                );
            }
            painter.rect_filled(
                card,
                egui::CornerRadius::same(theme::radii().popup),
                p.overlay,
            );

            let mut body = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card.shrink2(egui::vec2(20.0, 18.0)))
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            body.label(
                egui::RichText::new(format!("What's new in refryt {}", release.version))
                    .font(theme::semibold(16.0))
                    .color(p.text),
            );
            body.add_space(4.0);
            body.label(
                egui::RichText::new("What changed in this version.")
                    .font(theme::regular(12.0))
                    .color(p.secondary),
            );
            body.add_space(14.0);
            egui::ScrollArea::vertical()
                .max_height((height - 140.0).max(80.0))
                .auto_shrink([false, true])
                .show(&mut body, |ui| {
                    change_list(ui, &p, &release.changes);
                });
            let buttons = egui::Rect::from_min_max(
                egui::pos2(card.left() + 16.0, card.bottom() - 46.0),
                egui::pos2(card.right() - 16.0, card.bottom() - 16.0),
            );
            let mut row = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(buttons)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            close |= widgets::chip(&mut row, &p, "Close", true).clicked();
            if row.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        });

    if close {
        app.whats_new_open = false;
    }
}
