//! The "a new version is available" window: one centered, dimmed modal, opened by the update
//! check (at launch, or Settings → About → Check now). It installs the release where this copy
//! can update itself, and links the release page where it cannot. See `crate::update`.

use crate::app::App;
use crate::theme;
use crate::update::Install;
use crate::widgets;

/// What the update check found, and how far an install has got. Kept for the session only;
/// a skipped version is the one part that persists, in the settings.
#[derive(Default)]
pub(crate) struct State {
    /// A release newer than this build, once a check has found one.
    pub(crate) available: Option<crate::update::Release>,
    /// The window is open.
    pub(crate) open: bool,
    /// A check is in flight.
    pub(crate) checking: bool,
    /// A check found nothing newer, which About says until the next one.
    pub(crate) up_to_date: bool,
    /// The download, 0–100, while an install is under way. The window cannot be closed then:
    /// the app closes by itself once the update is ready.
    pub(crate) progress: Option<f32>,
    /// Why the last install failed.
    pub(crate) error: Option<String>,
}

pub(crate) fn modal(app: &mut App, ui: &mut egui::Ui) {
    if !app.update.open {
        return;
    }
    let Some(release) = app.update.available.clone() else {
        app.update.open = false;
        return;
    };
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let p = app.palette;
    let installable = release.installable();
    let busy = app.update.progress.is_some();
    let width = 580.0_f32.min(screen.width() - 40.0);
    // Four buttons need about 540 points. Below that, "Skip this version" takes its own line
    // above the others rather than running off the card's left edge.
    let one_line = width >= 560.0;
    let height = (if one_line { 210.0_f32 } else { 250.0 }).min(screen.height() - 80.0);

    let (mut install, mut page, mut skip, mut later) = (false, false, false, false);
    egui::Area::new(egui::Id::new("ytm-update-modal"))
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
            // Only a click outside the card closes it, as in the crash report.
            if !busy
                && response.clicked()
                && response
                    .interact_pointer_pos()
                    .is_some_and(|pos| !card.contains(pos))
            {
                later = true;
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
                egui::RichText::new(format!("refryt {} is available", release.version))
                    .font(theme::semibold(15.0))
                    .color(p.text),
            );
            body.add_space(6.0);
            body.label(
                egui::RichText::new(format!("You have {}.", crate::update::current_version()))
                    .font(theme::regular(12.0))
                    .color(p.secondary),
            );
            body.add_space(4.0);
            // An install that would have worked but for a release still missing its file or
            // checksums falls back to the manual text, which is what the user can do now.
            let how = if installable {
                Install::current().how()
            } else {
                Install::Manual.how()
            };
            body.label(
                egui::RichText::new(how)
                    .font(theme::regular(12.0))
                    .color(p.secondary),
            );
            body.add_space(8.0);
            if let Some(percent) = app.update.progress {
                body.label(
                    egui::RichText::new(format!("Downloading… {percent:.0}%"))
                        .font(theme::medium(12.0))
                        .color(p.text),
                );
            } else if let Some(error) = &app.update.error {
                body.label(
                    egui::RichText::new(format!("The update failed: {error}"))
                        .font(theme::regular(12.0))
                        .color(p.danger),
                );
            }

            if busy {
                return;
            }
            let buttons = egui::Rect::from_min_max(
                egui::pos2(card.left() + 16.0, card.bottom() - 46.0),
                egui::pos2(card.right() - 16.0, card.bottom() - 16.0),
            );
            // The way out of this version sits apart, on the left: it is not one of the answers
            // to "update now?", and it is the one a stray click should be furthest from.
            let skip_line = if one_line {
                buttons
            } else {
                buttons.translate(egui::vec2(0.0, -40.0))
            };
            let mut left = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(skip_line)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            skip = widgets::chip(&mut left, &p, "Skip this version", false).clicked();
            let mut row = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(buttons)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            if installable {
                install = widgets::chip(&mut row, &p, "Update and restart", true).clicked();
                page = widgets::chip(&mut row, &p, "Release notes", false).clicked();
            } else {
                page = widgets::chip(&mut row, &p, "Open release page", true).clicked();
            }
            later |= widgets::chip(&mut row, &p, "Later", false).clicked();
            if row.input(|i| i.key_pressed(egui::Key::Escape)) {
                later = true;
            }
        });

    if page {
        crate::open_url(&release.page);
    }
    if install {
        app.install_update();
    }
    if skip {
        let ctx = ui.ctx().clone();
        app.update_settings(&ctx, |s| s.skipped_update = Some(release.version.clone()));
    }
    if later {
        let ctx = ui.ctx().clone();
        app.update_later(&ctx, &release.version);
    }
    if later || skip {
        app.update.open = false;
        app.update.error = None;
    }
}
