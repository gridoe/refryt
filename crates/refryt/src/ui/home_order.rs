//! The "Order of homepage" window: Home's sections in the order the listener wants them, moved by
//! dragging. Opened from Settings; every move is saved at once and the feed already loaded is
//! re-arranged with it.
//!
//! Shaped like the shortcuts window (a dimmed backdrop and a centred card), with the playlist
//! rail's drag-to-reorder for the rows. The list scrolls when there are more sections than the
//! screen has room for, and a short note under it says why Home does not always follow it.

use crate::app::App;
use crate::icons::Icon;
use crate::model::{HomeSection, default_order_keys, normalise_order};
use crate::theme;
use crate::widgets;

/// A row with a second line under its name, and one without.
const ROW_TALL: f32 = 46.0;
const ROW_SHORT: f32 = 38.0;
const WIDTH: f32 = 440.0;
/// How near the list's edge a dragged row has to be before the list scrolls toward it, and how
/// fast it goes.
const EDGE: f32 = 36.0;
const EDGE_SPEED: f32 = 9.0;

/// Why Home may not match the list, in the plainest words that are still true: YouTube decides
/// what exists and sends it in batches, and a late section goes below what is already showing.
pub(crate) const DISCLAIMER: &str = "Home may not always follow this exactly. YouTube chooses \
which sections you get and sends them in batches. A section that arrives late is added below \
what is already on screen, so the page does not jump around while you read, and shelves we do \
not recognise go under Everything else. Strict Home order, in Settings, waits longer so fewer \
arrive late.";

fn row_height(section: HomeSection) -> f32 {
    if section.hint().is_some() {
        ROW_TALL
    } else {
        ROW_SHORT
    }
}

/// The order after dragging row `from` onto row `to`: the row is taken out and put in `to`'s
/// place, which is what the drop line shown while dragging promises.
pub(crate) fn moved(order: &[HomeSection], from: usize, to: usize) -> Vec<HomeSection> {
    let mut out = order.to_vec();
    if from < out.len() && to < out.len() && from != to {
        let item = out.remove(from);
        out.insert(to, item);
    }
    out
}

pub(crate) fn modal(app: &mut App, ui: &mut egui::Ui) {
    if !app.home_order_open {
        return;
    }
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };
    let p = app.palette;
    let order = normalise_order(&app.settings.home_order);
    let width = WIDTH.min(screen.width() - 40.0);
    let inner_w = width - 32.0;

    // The note's height at this width, so the card can be sized before anything is drawn.
    let note = ui.painter().layout(
        DISCLAIMER.to_owned(),
        theme::regular(11.0),
        p.dim,
        inner_w - 24.0,
    );
    let note_h = note.size().y.max(16.0);
    // Everything but the list: padding, title, line, gaps, note, buttons.
    let chrome = 16.0 + 22.0 + 6.0 + 36.0 + 12.0 + 12.0 + note_h + 12.0 + 30.0 + 16.0;
    let natural: f32 = order.iter().map(|s| row_height(*s)).sum();
    let list_h = natural.min((screen.height() - 48.0 - chrome).max(160.0));
    let height = chrome + list_h;

    let mut close = false;
    let mut dismissed = false;
    let mut reorder: Option<(usize, usize)> = None;
    let mut reset = false;
    egui::Area::new(egui::Id::new("ytm-home-order-modal"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        // The rows are dragged; the window itself stays where it is.
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
            // Only a click outside the card dismisses: the rows are draggable, and a click on
            // one must not fall through to the backdrop.
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
            // Every gap below is explicit, so the card's height can be worked out up front.
            body.spacing_mut().item_spacing.y = 0.0;
            body.label(
                egui::RichText::new("Order of homepage")
                    .font(theme::semibold(15.0))
                    .color(p.text),
            );
            body.add_space(6.0);
            body.label(
                egui::RichText::new(
                    "Drag a section to move it. Shelves that are not named here go where \
                     Everything else is.",
                )
                .font(theme::regular(12.0))
                .color(p.secondary),
            );
            body.add_space(12.0);

            let list_top = body.cursor().top();
            egui::ScrollArea::vertical()
                .id_salt("home-order-list")
                .max_height(list_h)
                .auto_shrink([false, false])
                .show(&mut body, |list| {
                    list.spacing_mut().item_spacing.y = 0.0;
                    for (i, section) in order.iter().enumerate() {
                        let (rect, response) = list.allocate_exact_size(
                            egui::vec2(list.available_width(), row_height(*section)),
                            egui::Sense::click_and_drag(),
                        );
                        if response.drag_started() {
                            egui::DragAndDrop::set_payload(list.ctx(), i);
                        }
                        let dragging_this = egui::DragAndDrop::payload::<usize>(list.ctx())
                            .is_some_and(|from| *from == i);
                        if response.hovered() || dragging_this {
                            list.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(theme::radii().control),
                                if dragging_this {
                                    p.surface_active
                                } else {
                                    p.surface_hover
                                },
                            );
                            list.ctx().set_cursor_icon(if dragging_this {
                                egui::CursorIcon::Grabbing
                            } else {
                                egui::CursorIcon::Grab
                            });
                        }
                        paint_grip(list, rect, p.dim);
                        paint_label(list, &p, rect, *section);
                        // The drop lands on the row under the pointer; the line shows which edge
                        // of it, flipping with the direction exactly as the playlist rail does
                        // (`remove` then `insert` puts a row after the target going down, before
                        // it going up).
                        if egui::DragAndDrop::has_payload_of_type::<usize>(list.ctx())
                            && response.contains_pointer()
                        {
                            if list.ctx().input(|inp| inp.pointer.any_released()) {
                                if let Some(from) =
                                    egui::DragAndDrop::take_payload::<usize>(list.ctx())
                                {
                                    reorder = Some((*from, i));
                                }
                            } else {
                                let from = egui::DragAndDrop::payload::<usize>(list.ctx());
                                let up = from.is_some_and(|from| i < *from);
                                let y = if up { rect.top() } else { rect.bottom() };
                                list.painter().hline(
                                    egui::Rangef::new(rect.left() + 8.0, rect.right() - 8.0),
                                    y,
                                    egui::Stroke::new(2.0, p.accent),
                                );
                            }
                        }
                    }
                    // A long list: carry the scroll along while a row is dragged near an edge.
                    if egui::DragAndDrop::has_payload_of_type::<usize>(list.ctx())
                        && let Some(pos) = list.ctx().pointer_hover_pos()
                    {
                        let (top, bottom) = (list_top, list_top + list_h);
                        if pos.y < top + EDGE {
                            list.scroll_with_delta(egui::vec2(0.0, EDGE_SPEED));
                            list.ctx().request_repaint();
                        } else if pos.y > bottom - EDGE {
                            list.scroll_with_delta(egui::vec2(0.0, -EDGE_SPEED));
                            list.ctx().request_repaint();
                        }
                    }
                });

            // Why Home may differ from this list, quietly, under it.
            body.add_space(12.0);
            let (note_rect, _) = body.allocate_exact_size(
                egui::vec2(body.available_width(), note_h),
                egui::Sense::hover(),
            );
            Icon::Info.image(p.dim, 14.0).paint_at(
                &body,
                egui::Rect::from_min_size(
                    note_rect.min + egui::vec2(0.0, 1.0),
                    egui::vec2(14.0, 14.0),
                ),
            );
            body.painter()
                .galley(note_rect.min + egui::vec2(24.0, 0.0), note, p.dim);

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
    if let Some((from, to)) = reorder {
        let next = moved(&order, from, to);
        if next != order {
            app.update_settings(&ctx, |s| {
                s.home_order = next.iter().map(|x| x.key().to_owned()).collect();
            });
        }
    }
    if reset {
        app.update_settings(&ctx, |s| s.home_order = default_order_keys());
    }
    if close || dismissed {
        app.home_order_open = false;
    }
}

/// The section's name, and under it the hint for those that carry one.
fn paint_label(ui: &egui::Ui, p: &theme::Palette, row: egui::Rect, section: HomeSection) {
    let x = row.left() + 38.0;
    let painter = ui.painter();
    match section.hint() {
        Some(hint) => {
            painter.text(
                egui::pos2(x, row.center().y - 8.0),
                egui::Align2::LEFT_CENTER,
                section.label(),
                theme::medium(13.0),
                p.text,
            );
            painter.text(
                egui::pos2(x, row.center().y + 9.0),
                egui::Align2::LEFT_CENTER,
                hint,
                theme::regular(11.0),
                p.dim,
            );
        }
        None => {
            painter.text(
                egui::pos2(x, row.center().y),
                egui::Align2::LEFT_CENTER,
                section.label(),
                theme::medium(13.0),
                p.text,
            );
        }
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

    #[test]
    fn dragging_a_row_down_puts_it_after_the_target() {
        use HomeSection::*;
        let order = vec![ListenAgain, QuickPicks, MixedForYou];
        assert_eq!(moved(&order, 0, 2), [QuickPicks, MixedForYou, ListenAgain]);
    }

    #[test]
    fn dragging_a_row_up_puts_it_before_the_target() {
        use HomeSection::*;
        let order = vec![ListenAgain, QuickPicks, MixedForYou];
        assert_eq!(moved(&order, 2, 0), [MixedForYou, ListenAgain, QuickPicks]);
    }

    #[test]
    fn dropping_a_row_on_itself_or_off_the_list_changes_nothing() {
        use HomeSection::*;
        let order = vec![ListenAgain, QuickPicks];
        assert_eq!(moved(&order, 1, 1), order);
        assert_eq!(moved(&order, 0, 9), order);
        assert_eq!(moved(&order, 9, 0), order);
    }

    /// The note names the two real reasons and the fallback group, in plain words.
    #[test]
    fn the_note_explains_batches_late_sections_and_the_fallback() {
        assert!(DISCLAIMER.contains("batches"));
        assert!(DISCLAIMER.contains("arrives late"));
        assert!(DISCLAIMER.contains("Everything else"));
    }
}
