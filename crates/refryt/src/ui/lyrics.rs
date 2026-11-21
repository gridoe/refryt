//! The lyrics panel: the now-playing song's words, synced to playback when we have timings.
//!
//! Shares the right panel with the queue — opening one closes the other — so it takes the same
//! width and insets the queue uses.

use crate::app::App;
use crate::lyrics::{Answer, Source};
use crate::theme;
use crate::widgets;

pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let picker = picker(app);
    // The card's own rect, before the header and list consume the ui: the corner repaint at
    // the end needs its rounded edges.
    let card = widgets::card_rect(ui.available_rect_before_wrap(), theme::CARD_INSET);
    let _edge = widgets::CardEdge::new(ui, card, &p);
    widgets::clip_card_content(ui, card, theme::CARD_INSET_X, &p);

    if app.lyrics_loading {
        // The card is as wide as what it holds, and the skeleton's ragged lines reach only part
        // of the way: without this the card's fill stopped short of its right edge. The loaded
        // lyrics fill it through their scroll area.
        ui.set_min_width(ui.available_width());
        if let Some(source) = title(ui, &p, picker.as_ref()) {
            app.choose_lyrics_source(source);
        }
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.vertical(|ui| widgets::skeleton_lines(ui, &p, 8));
        });
        return;
    }
    let Some(lyrics) = app.lyrics.as_ref() else {
        if let Some(source) = title(ui, &p, picker.as_ref()) {
            app.choose_lyrics_source(source);
            return;
        }
        let message = app
            .lyrics_error
            .clone()
            .unwrap_or_else(|| "Play a song to see its lyrics".to_owned());
        crate::ui::centered(app, ui, &message);
        return;
    };

    // Which line is playing. Only synced lyrics know; plain text has no highlight.
    let position_ms = (app.player_state.position * 1000.0) as u64;
    let active = lyrics
        .synced
        .then(|| lyrics.lines.iter().rposition(|l| l.at_ms <= position_ms))
        .flatten();

    let mut area = egui::ScrollArea::vertical()
        .id_salt("lyrics")
        .auto_shrink([false, false]);
    if app.lyrics_scroll_reset {
        area = area.vertical_scroll_offset(0.0);
        app.lyrics_scroll_reset = false;
        // Land on the top of the panel, not wherever the active line happens to be — the
        // sync below then treats this frame's line as already seen, so the scroll-to-active
        // below doesn't immediately undo the reset.
        app.lyrics_active = active;
    }
    // Only scroll when the active line actually changed since last frame — not every repaint,
    // which used to snap the view back the instant a paused listener scrolled away to read
    // ahead. Since `active` only advances as `position_ms` crosses a new line, this is also
    // exactly "only while playback advances": paused, position never changes, so neither does
    // `active`.
    let scroll = active != app.lyrics_active;
    app.lyrics_active = active;

    let synced = lyrics.synced;
    let romanise = app.settings.lyrics_romanisation;
    let from = widgets::painted_so_far(ui);
    let output = area.show(ui, |ui| {
        // Inside the scroll: the title scrolls away with the lyrics rather than staying
        // pinned.
        let chosen = title(ui, &p, picker.as_ref());
        let mut clicked = None;
        for (i, line) in lyrics.lines.iter().enumerate() {
            // The gap goes before each line but the first, not after each: after the last
            // one it was blank space below the lyrics, inside a card whose content runs to
            // its bottom edge.
            if i > 0 {
                ui.add_space(4.0);
            }
            let is_active = Some(i) == active;
            let font = if is_active {
                theme::semibold(15.0)
            } else {
                theme::regular(14.0)
            };
            // With romanisation on, the romanised line leads and the original sits under it,
            // smaller: the one most people read is the one that lights up.
            let (lead, under) = match line.romanised.as_ref().filter(|_| romanise) {
                Some(roman) => (roman.as_str(), Some(line.text.as_str())),
                None => (line.text.as_str(), None),
            };
            let text = if lead.is_empty() { "♪" } else { lead };
            // Laid out before the colour is known: a synced line lights up under the
            // pointer, which needs the row's rect first. The whole row is the target, so a
            // short line is as easy to hit as a long one.
            let galley = ui.painter().layout(
                text.to_owned(),
                font,
                egui::Color32::PLACEHOLDER,
                ui.available_width(),
            );
            // The line in Latin letters, under it, when Apple gave one and it's wanted.
            let roman = under.map(|r| {
                ui.painter().layout(
                    r.to_owned(),
                    theme::regular(12.0),
                    egui::Color32::PLACEHOLDER,
                    ui.available_width(),
                )
            });
            let roman_height = roman.as_ref().map_or(0.0, |r| r.size().y + 1.0);
            let sense = if synced {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), galley.size().y + roman_height),
                sense,
            );
            let hovered = synced && response.hovered();
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let color = if is_active {
                p.accent_text()
            } else if hovered {
                p.text
            } else {
                p.secondary
            };
            let line_height = galley.size().y;
            ui.painter().galley(rect.min, galley, color);
            if let Some(roman) = roman {
                ui.painter().galley(
                    rect.min + egui::vec2(0.0, line_height + 1.0),
                    roman,
                    color.gamma_multiply(0.6),
                );
            }
            if synced && response.clicked() {
                clicked = Some(line.at_ms);
            }
            if is_active && scroll {
                ui.scroll_to_rect(
                    rect.expand2(egui::vec2(0.0, 48.0)),
                    Some(egui::Align::Center),
                );
            }
        }
        (clicked, chosen)
    });
    // The title scrolls with the words, so at the top of the lyrics nothing fades there.
    widgets::fade_scroll_edges(ui, from, &output, widgets::scroll_fade(&app.settings));
    let (clicked, chosen) = output.inner;
    if let Some(source) = chosen {
        app.choose_lyrics_source(source);
    }
    // A click on a synced line plays from where it starts.
    if let Some(at_ms) = clicked {
        app.seek_to(at_ms as f64 / 1000.0);
    }
    // The card's rounded corners, over everything the lyrics drew: the list runs to the card's
    // bottom edge now.
    widgets::paint_corner_notches(ui, card, theme::CARD_INSET_X, &p);
}

/// The source picker beside the title: which source is shown, and the others that found
/// lyrics for this song. Worked out before the lyrics are borrowed for drawing, so the pick
/// can be applied to the app after.
pub(crate) struct Picker {
    current: String,
    options: Vec<(Source, String)>,
    busy: bool,
}

/// The picker for the song on screen, when more than one source was asked.
pub(crate) fn picker(app: &App) -> Option<Picker> {
    if app.lyrics_order.len() < 2 {
        return None;
    }
    let current = match app.lyrics_shown {
        Some(source) => source.short_label().to_owned(),
        None if app.lyrics_loading => "Looking".to_owned(),
        None => "Sources".to_owned(),
    };
    // Only sources that found lyrics: one with none, or still looking, isn't a choice. The
    // spinner says some are still out.
    let romanise = app.settings.lyrics_romanisation;
    let options = app
        .lyrics_order
        .iter()
        .filter_map(|source| match app.lyrics_answers.get(source) {
            Some(Answer::Found(l)) if l.synced && romanise && crate::lyrics::has_romanised(l) => {
                Some((*source, format!("{} · romanised", source.short_label())))
            }
            Some(Answer::Found(l)) if l.synced => Some((*source, source.short_label().to_owned())),
            Some(Answer::Found(_)) => {
                Some((*source, format!("{} · no timing", source.short_label())))
            }
            _ => None,
        })
        .collect();
    let busy = app.lyrics_answers.values().any(|a| *a == Answer::Pending);
    Some(Picker {
        current,
        options,
        busy,
    })
}

/// The panel's own title, a hair right of the lyrics below it by eye — the same nudge the
/// queue's own header takes — and the source picker at the right. Returns a source picked.
fn title(ui: &mut egui::Ui, p: &theme::Palette, picker: Option<&Picker>) -> Option<Source> {
    let mut chosen = None;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Lyrics")
                .font(theme::semibold(14.0))
                .color(p.text),
        );
        if let Some(picker) = picker {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The same quiet "label ▾" as the queue's radio filter.
                ui.add_space(2.0);
                let labels: Vec<&str> = picker.options.iter().map(|o| o.1.as_str()).collect();
                if let Some(i) = widgets::text_dropdown(
                    ui,
                    p,
                    egui::Id::new("lyrics-source"),
                    &picker.current,
                    &labels,
                    picker.busy,
                ) && let Some((source, _)) = picker.options.get(i)
                {
                    chosen = Some(*source);
                }
            });
        }
    });
    ui.add_space(8.0);
    chosen
}
