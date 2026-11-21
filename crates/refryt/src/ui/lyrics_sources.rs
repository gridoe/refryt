//! The "Lyrics sources" window: where lyrics are looked for, in the order they're tried, each
//! with a switch. Opened from Settings; every change is saved at once, and the song on screen
//! is looked up again with it.
//!
//! The same card and drag-to-reorder as the "Order of homepage" window (`home_order`), with a
//! switch at each row's right. Priority and whether a source is asked are separate questions:
//! dragging moves a row whether it is on or off.

use crate::app::App;
use crate::lyrics::{self, Source};
use crate::theme;
use crate::widgets;

const ROW: f32 = 64.0;
const WIDTH: f32 = 480.0;

/// The list after dragging row `from` onto row `to`, as `home_order::moved` does it.
fn moved(list: &[(Source, bool)], from: usize, to: usize) -> Vec<(Source, bool)> {
    let mut out = list.to_vec();
    if from < out.len() && to < out.len() && from != to {
        let item = out.remove(from);
        out.insert(to, item);
    }
    out
}

pub(crate) fn modal(app: &mut App, ui: &mut egui::Ui) {
    if !app.lyrics_sources_open {
        return;
    }
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let p = app.palette;
    let list = lyrics::normalise(&app.settings.lyrics_sources);
    let width = WIDTH.min(screen.width() - 40.0);
    // Padding, title, line, gaps and buttons around the rows; the rows scroll when the window
    // is too short for all of them.
    let chrome = 16.0 + 22.0 + 6.0 + 36.0 + 12.0 + 12.0 + 30.0 + 16.0;
    let height = (chrome + ROW * list.len() as f32).min(screen.height() - 40.0);
    let rows_height = (height - chrome).max(ROW);

    let mut close = false;
    let mut dismissed = false;
    let mut reorder: Option<(usize, usize)> = None;
    let mut flipped: Option<usize> = None;
    let mut reset = false;
    egui::Area::new(egui::Id::new("ytm-lyrics-sources-modal"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .movable(false)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            let (full, backdrop) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(full, 0.0, egui::Color32::from_black_alpha(150));
            let card = egui::Rect::from_center_size(
                full.center() - egui::vec2(0.0, 12.0),
                egui::vec2(width, height),
            );
            if backdrop.clicked()
                && backdrop
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
            body.spacing_mut().item_spacing.y = 0.0;
            body.label(
                egui::RichText::new("Lyrics sources")
                    .font(theme::semibold(15.0))
                    .color(p.text),
            );
            body.add_space(6.0);
            body.label(
                egui::RichText::new(
                    "All the sources that are on are asked at once. The first one in this list \
                     with timed lyrics is shown. Drag a source to move it. A source that's off \
                     is never contacted.",
                )
                .font(theme::regular(12.0))
                .color(p.secondary),
            );
            body.add_space(12.0);

            egui::ScrollArea::vertical()
                .id_salt("lyrics-sources-rows")
                .max_height(rows_height)
                .auto_shrink([false, true])
                .show(&mut body, |body| {
                    for (i, (source, on)) in list.iter().enumerate() {
                        let (rect, response) = body.allocate_exact_size(
                            egui::vec2(body.available_width(), ROW),
                            egui::Sense::click_and_drag(),
                        );
                        if response.drag_started() {
                            egui::DragAndDrop::set_payload(body.ctx(), i);
                        }
                        let dragging_this = egui::DragAndDrop::payload::<usize>(body.ctx())
                            .is_some_and(|from| *from == i);
                        if response.hovered() || dragging_this {
                            body.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(theme::radii().control),
                                if dragging_this {
                                    p.surface_active
                                } else {
                                    p.surface_hover
                                },
                            );
                            body.ctx().set_cursor_icon(if dragging_this {
                                egui::CursorIcon::Grabbing
                            } else {
                                egui::CursorIcon::Grab
                            });
                        }
                        paint_grip(body, rect, p.dim);
                        let text = if *on { p.text } else { p.dim };
                        let x = rect.left() + 38.0;
                        body.painter().text(
                            egui::pos2(x, rect.center().y - 15.0),
                            egui::Align2::LEFT_CENTER,
                            source.label(),
                            theme::medium(13.0),
                            text,
                        );
                        // The detail, two lines at most, left of the switch.
                        let detail = body.painter().layout(
                            source.detail().to_owned(),
                            theme::regular(11.0),
                            p.dim,
                            rect.width() - 38.0 - 60.0,
                        );
                        let detail_rect = egui::Rect::from_min_size(
                            egui::pos2(x, rect.center().y - 5.0),
                            egui::vec2(rect.width() - 38.0 - 60.0, 30.0),
                        );
                        body.painter().with_clip_rect(detail_rect).galley(
                            detail_rect.min,
                            detail,
                            p.dim,
                        );

                        let switch = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 28.0, rect.center().y),
                            egui::vec2(40.0, 22.0),
                        );
                        let mut value = *on;
                        body.scope_builder(egui::UiBuilder::new().max_rect(switch), |ui| {
                            widgets::toggle(ui, &p, ui.id().with(("lyrics-source", i)), &mut value);
                        });
                        if value != *on {
                            flipped = Some(i);
                        }

                        if egui::DragAndDrop::has_payload_of_type::<usize>(body.ctx())
                            && response.contains_pointer()
                        {
                            if body.ctx().input(|inp| inp.pointer.any_released()) {
                                if let Some(from) =
                                    egui::DragAndDrop::take_payload::<usize>(body.ctx())
                                {
                                    reorder = Some((*from, i));
                                }
                            } else {
                                let from = egui::DragAndDrop::payload::<usize>(body.ctx());
                                let up = from.is_some_and(|from| i < *from);
                                let y = if up { rect.top() } else { rect.bottom() };
                                body.painter().hline(
                                    egui::Rangef::new(rect.left() + 8.0, rect.right() - 8.0),
                                    y,
                                    egui::Stroke::new(2.0, p.accent),
                                );
                            }
                        }
                    }
                });

            body.add_space(12.0);
            body.allocate_ui(egui::vec2(body.available_width(), 30.0), |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::chip(ui, &p, "Done", true).clicked() {
                        close = true;
                    }
                    if widgets::chip(ui, &p, "Reset to default", false).clicked() {
                        reset = true;
                    }
                });
            });
            if body.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        });

    let ctx = ui.ctx().clone();
    let mut next = list.clone();
    if let Some((from, to)) = reorder {
        next = moved(&next, from, to);
    }
    if let Some(i) = flipped {
        next[i].1 = !next[i].1;
    }
    if reset {
        next = lyrics::normalise(&[]);
    }
    if next != list {
        app.update_settings(&ctx, |s| s.lyrics_sources = lyrics::to_choices(&next));
    }
    if close || dismissed {
        app.lyrics_sources_open = false;
    }
}

/// Six dots in two columns at the row's left: the usual "this can be dragged".
fn paint_grip(ui: &egui::Ui, row: egui::Rect, colour: egui::Color32) {
    let x = row.left() + 16.0;
    let y = row.center().y;
    for dx in [-3.0, 3.0] {
        for dy in [-6.0, 0.0, 6.0] {
            ui.painter()
                .circle_filled(egui::pos2(x + dx, y + dy), 1.4, colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Source::*;

    #[test]
    fn dragging_moves_a_source_and_keeps_its_switch() {
        let list = vec![(BetterLyrics, true), (Lrclib, false), (YouTube, true)];
        assert_eq!(
            moved(&list, 1, 0),
            vec![(Lrclib, false), (BetterLyrics, true), (YouTube, true)]
        );
        assert_eq!(moved(&list, 0, 9), list, "off the list changes nothing");
    }
}
