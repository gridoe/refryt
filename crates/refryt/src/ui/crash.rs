//! The "refryt closed unexpectedly" report: one centered, dimmed modal shown once on the launch
//! after a crash, offering the details and a prefilled issue. Nothing is uploaded.

use crate::app::App;
use crate::theme;
use crate::widgets;

pub(crate) fn modal(app: &mut App, ui: &mut egui::Ui) {
    let Some(report) = app.crash_report.clone() else {
        return;
    };
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let p = app.palette;
    let width = 520.0_f32.min(screen.width() - 40.0);
    let height = 320.0_f32.min(screen.height() - 80.0);

    let (mut close, mut open, mut copy, mut dismissed) = (false, false, false, false);
    egui::Area::new(egui::Id::new("ytm-crash-modal"))
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
            // Only a click outside the card dismisses — a raw `clicked()` also fires for clicks
            // on non-interactive content inside the card (plain text, spacing), which would
            // otherwise close the modal instead of doing nothing.
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
                egui::RichText::new("refryt closed unexpectedly")
                    .font(theme::semibold(15.0))
                    .color(p.text),
            );
            body.add_space(6.0);
            body.label(
                egui::RichText::new(
                    "The previous run crashed. Copy the details or open an issue — nothing is \
                     sent automatically.",
                )
                .font(theme::regular(12.0))
                .color(p.secondary),
            );
            body.add_space(8.0);
            let summary = format!(
                "{}  ·  {}\n{}",
                report.message,
                report.location,
                summary_time(report.time_unix),
            );
            egui::ScrollArea::vertical()
                .max_height((height - 170.0).max(60.0))
                .show(&mut body, |ui| {
                    ui.label(
                        egui::RichText::new(&summary)
                            .font(theme::medium(12.0))
                            .color(p.text),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(&report.backtrace)
                            .font(theme::regular(11.0))
                            .color(p.dim),
                    );
                });
            body.add_space(8.0);
            body.allocate_ui(egui::vec2(body.available_width(), 30.0), |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::chip(ui, &p, "Dismiss", true).clicked() {
                        close = true;
                    }
                    if widgets::chip(ui, &p, "Open issue", false).clicked() {
                        open = true;
                    }
                    if widgets::chip(ui, &p, "Copy details", false).clicked() {
                        copy = true;
                    }
                });
            });
            if body.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        });

    if open {
        crate::open_url(&issue_url(&report));
    }
    if copy {
        app.backend.copy_text(details_text(&report));
    }
    if close || dismissed {
        app.crash_report = None;
        crate::crash::clear();
    }
}

fn details_text(c: &crate::crash::Crash) -> String {
    format!(
        "refryt {} crashed\nmessage: {}\nlocation: {}\ntime: {}\n\n{}",
        c.version,
        c.message,
        c.location,
        summary_time(c.time_unix),
        c.backtrace,
    )
}

fn issue_url(c: &crate::crash::Crash) -> String {
    let mut body = details_text(c);
    // GitHub URLs have a practical length cap; a stripped backtrace can be long, so trim it.
    const MAX: usize = 6000;
    if body.len() > MAX {
        let mut boundary = MAX;
        while boundary > 0 && !body.is_char_boundary(boundary) {
            boundary -= 1;
        }
        body.truncate(boundary);
        body.push_str("\n… (truncated)");
    }
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    ser.append_pair("title", &format!("Crash: {}", first_line(&c.message)));
    ser.append_pair("body", &body);
    ser.append_pair("labels", "bug");
    format!(
        "https://github.com/gridoe/refryt/issues/new?{}",
        ser.finish()
    )
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(80).collect()
}

fn summary_time(unix: u64) -> String {
    format!("unix {unix} · v{}", env!("CARGO_PKG_VERSION"))
}
