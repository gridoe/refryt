//! The "Paste cookies" window: sign in with the `Cookie` header a signed-in browser sends to
//! YouTube Music, copied from its developer tools. Opened from Settings' Account tab.
//!
//! The way in when neither the sign-in window nor the browser import can be used — macOS before
//! 14, a Linux without WebKitGTK, a browser whose store cannot be read. It is the Network tab's
//! request header that works here, not `document.cookie` from the Console: the cookies that
//! authenticate are HttpOnly, so only the request carries them (see `ui::settings`' module doc).
//! What was pasted is parsed by `auth::extract_cookie` and checked with YouTube by
//! `Api::install_cookie` before anything is saved.
//!
//! Shaped like the other windows over the app: a dimmed backdrop and a centred card.

use crate::app::App;
use crate::theme;
use crate::widgets;

const WIDTH: f32 = 520.0;
const SITE: &str = "https://music.youtube.com";

/// Where to find the header, in the order the developer tools are clicked through.
const STEPS: [&str; 4] = [
    "Sign in at music.youtube.com.",
    "Open your browser's developer tools (F12, or ⌥⌘I on a Mac), switch to Network and refresh.",
    "In the list of requests, pick one called \"browse\" or \"next\".",
    "In its Request Headers, look for \"cookie\". Right-click it and choose Copy value, or \
     select the value and copy it.",
];

const NOTE: &str = "Copy all of it; SAPISID has to be in there. A session added this way cannot \
renew itself, so when Google ends it you will be asked to sign in again.";

pub(crate) fn modal(app: &mut App, ui: &mut egui::Ui) {
    if !app.paste_cookies_open {
        return;
    }
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let p = app.palette;
    let id = egui::Id::new("ytm-paste-cookies-modal");
    let width = WIDTH.min(screen.width() - 40.0);
    // The card's height is only known once its contents are laid out, so it is centred on the
    // height the previous frame measured; the first frame guesses.
    let last_h = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(440.0);

    let mut close = false;
    let mut submit = false;
    egui::Area::new(id)
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .movable(false)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            let (full, backdrop) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(full, 0.0, egui::Color32::from_black_alpha(150));
            let top = (full.center().y - 12.0 - last_h / 2.0).max(full.top() + 20.0);
            let card_min = egui::pos2(full.center().x - width / 2.0, top);
            // Painted once the height is known, but under the contents: reserved here.
            let backgrounds: Vec<egui::layers::ShapeIdx> = (0..4)
                .map(|_| ui.painter().add(egui::Shape::Noop))
                .collect();

            let mut body = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_max(
                        card_min + egui::vec2(16.0, 16.0),
                        egui::pos2(card_min.x + width - 16.0, full.bottom() - 20.0),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            body.spacing_mut().item_spacing.y = 0.0;
            body.label(
                egui::RichText::new("Paste cookies")
                    .font(theme::semibold(15.0))
                    .color(p.text),
            );
            body.add_space(6.0);
            body.label(
                egui::RichText::new(
                    "Sign in with the cookies of a browser that is signed in to YouTube Music.",
                )
                .font(theme::regular(12.0))
                .color(p.secondary),
            );
            body.add_space(12.0);
            if widgets::outline_button(&mut body, &p, "Open YouTube Music").clicked() {
                crate::open_url(SITE);
            }
            body.add_space(12.0);
            for (i, step) in STEPS.iter().enumerate() {
                body.horizontal_top(|row| {
                    row.spacing_mut().item_spacing.x = 0.0;
                    let (num, _) =
                        row.allocate_exact_size(egui::vec2(22.0, 16.0), egui::Sense::hover());
                    row.painter().text(
                        num.left_top(),
                        egui::Align2::LEFT_TOP,
                        format!("{}.", i + 1),
                        theme::medium(12.0),
                        p.dim,
                    );
                    row.add(
                        egui::Label::new(
                            egui::RichText::new(*step)
                                .font(theme::regular(12.0))
                                .color(p.text),
                        )
                        .wrap(),
                    );
                });
                body.add_space(6.0);
            }
            body.add_space(4.0);
            body.add(
                egui::Label::new(
                    egui::RichText::new(NOTE)
                        .font(theme::regular(11.0))
                        .color(p.dim),
                )
                .wrap(),
            );
            body.add_space(12.0);
            // Masked: what goes in here is the account itself.
            let field = body.add(
                egui::TextEdit::singleline(&mut app.pasted_cookie)
                    .password(true)
                    .hint_text("Paste the cookie value")
                    .font(theme::regular(13.0))
                    .desired_width(f32::INFINITY),
            );
            let ready = !app.pasted_cookie.trim().is_empty();
            if ready && field.lost_focus() && body.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            body.add_space(16.0);
            body.allocate_ui(egui::vec2(body.available_width(), 30.0), |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::chip(ui, &p, "Sign in", ready).clicked() && ready {
                        submit = true;
                    }
                    if widgets::chip(ui, &p, "Cancel", false).clicked() {
                        close = true;
                    }
                });
            });
            if body.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }

            let height = body.min_rect().bottom() + 16.0 - top;
            let card = egui::Rect::from_min_size(card_min, egui::vec2(width, height));
            let shadows = [(14.0, 24), (7.0, 40), (2.0, 60)];
            for (idx, (grow, alpha)) in backgrounds.iter().zip(shadows) {
                ui.painter().set(
                    *idx,
                    egui::Shape::rect_filled(
                        card.expand(grow),
                        egui::CornerRadius::same(theme::radii().popup + grow as u8),
                        egui::Color32::from_black_alpha(alpha),
                    ),
                );
            }
            ui.painter().set(
                backgrounds[3],
                egui::Shape::rect_filled(
                    card,
                    egui::CornerRadius::same(theme::radii().popup),
                    p.overlay,
                ),
            );
            // Only a click outside the card dismisses, so a click in the field cannot close it.
            if backdrop.clicked()
                && backdrop
                    .interact_pointer_pos()
                    .is_some_and(|pos| !card.contains(pos))
            {
                close = true;
            }
            if (height - last_h).abs() > 0.5 {
                ui.ctx().data_mut(|d| d.insert_temp(id, height));
                ui.ctx().request_repaint();
            }
        });

    if submit {
        // Handed over and cleared at once: the app keeps no copy of the paste beyond the
        // session `install_cookie` saves.
        let raw = std::mem::take(&mut app.pasted_cookie);
        app.sign_in_error = None;
        app.backend.sign_in_with_cookie(raw);
        app.paste_cookies_open = false;
    } else if close {
        app.pasted_cookie.clear();
        app.paste_cookies_open = false;
    }
}
