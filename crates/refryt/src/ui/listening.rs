//! The Stats page: what you've been listening to, from the log kept on this machine.

use chrono::{Datelike, NaiveDate};
use egui::{Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

use crate::app::{App, Route};
use crate::icons::Icon;
use crate::listening_stats::{
    Bar, Delta, Report, StatsRange, TopList, listening_time, plays_label,
};
use crate::theme;
use crate::ui::{centered, failed, page_title, section};
use crate::widgets;

/// How many rows a top list shows on the Stats page.
const TOP_SHOWN: usize = 5;
/// Below this two lists side by side would cut off too much of each row, so they stack.
const TWO_COLUMNS_WIDTH: f32 = 680.0;
const COLUMN_GAP: f32 = 24.0;
const TILE_HEIGHT: f32 = 84.0;
const TILE_GAP: f32 = 12.0;
/// Below this a row of four tiles gets cramped, so they go two by two.
const FOUR_TILES_WIDTH: f32 = 720.0;
/// A tile's number, and how far it shrinks before it would overflow the tile.
const TILE_VALUE_PT: f32 = 22.0;
const TILE_VALUE_MIN_PT: f32 = 14.0;
const CHART_HEIGHT: f32 = 140.0;
const CALENDAR_GAP: f32 = 3.0;
const CALENDAR_MIN_CELL: f32 = 5.0;
const CALENDAR_MAX_CELL: f32 = 16.0;
const CALENDAR_WEEKS: usize = 53;
const CALENDAR_PADDING: f32 = 16.0;
/// The room for the line over the grid and the month names.
const CALENDAR_HEADER: f32 = 28.0;
const CALENDAR_MONTHS: f32 = 18.0;
/// The room left of the grid for Mon, Wed and Fri.
const CALENDAR_DAY_LABELS: f32 = 30.0;
/// About how wide a month's name is.
const CALENDAR_MONTH_WIDTH: f32 = 28.0;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    if let Some(error) = app.stats_error.clone() {
        page_title(ui, &app.palette, "Stats");
        failed(app, ui, error, App::retry_route);
        return;
    }
    let Some(report) = app.stats_report.clone() else {
        page_title(ui, &app.palette, "Stats");
        widgets::skeleton_shelf(ui, &app.palette, widgets::SkeletonShelfShape::Rows, 8);
        return;
    };
    if report.logged == 0 {
        page_title(ui, &app.palette, "Stats");
        let message = if app.settings.listening_stats {
            "Songs you listen to in Refryt will show up here."
        } else {
            "Listening stats are off. You can turn them on in Settings."
        };
        centered(app, ui, message);
        return;
    }
    crate::ui::page_scroll(app, "stats").show(ui, |ui| {
        page_title(ui, &app.palette, "Stats");
        range_chips(app, ui);
        if !app.settings.listening_stats {
            ui.add_space(8.0);
            quiet_line(
                app,
                ui,
                "Listening stats are off, so new plays aren't being added.",
            );
        }
        if report.plays == 0 {
            ui.add_space(24.0);
            quiet_line(app, ui, "Nothing played in this range.");
        } else {
            ui.add_space(16.0);
            totals(app, ui, &report);
            top_lists(app, ui, &report);
            if report.bars.len() > 1 {
                section(ui, &app.palette, "Plays over time");
                bar_chart(ui, &app.palette, &report.bars);
            }
        }
        section(ui, &app.palette, "Listening calendar");
        streaks(app, ui, &report);
        ui.add_space(12.0);
        calendar(ui, &app.palette, &report.calendar);
        ui.add_space(24.0);
    });
}

fn quiet_line(app: &App, ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::regular(13.0))
            .color(app.palette.secondary),
    );
}

fn range_chips(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        for range in StatsRange::ALL {
            if widgets::chip(ui, &p, &range.label(), app.stats_range == range).clicked() {
                picked = Some(range);
            }
        }
    });
    if let Some(range) = picked {
        app.set_stats_range(range);
    }
}

/// Offer an import once, in a strip at the top of the Stats page, while nothing has been imported.
/// It stays until it's answered and isn't made again after that: Settings keeps the way to do it.
pub(crate) fn takeout_offer(app: &mut App, ui: &mut egui::Ui) {
    if app.settings.takeout_offer_answered || app.takeout_importing || !app.settings.listening_stats
    {
        return;
    }
    let Some(imported) = app.stats_report.as_ref().map(|report| report.imported) else {
        return;
    };
    let (mut import, mut dismiss) = (false, imported);
    if !imported {
        let p = app.palette;
        crate::ui::page_strip(
            ui,
            &p,
            Icon::History,
            p.accent,
            "Import your history",
            "From Google Takeout",
            |buttons| {
                import = widgets::pill_button(buttons, &p, "Choose file").clicked();
                dismiss = widgets::outline_button(buttons, &p, "Dismiss").clicked();
                crate::ui::settings::takeout_help(buttons, &p);
            },
        );
    }
    if import || dismiss {
        let ctx = ui.ctx().clone();
        app.update_settings(&ctx, |s| s.takeout_offer_answered = true);
    }
    if import && let Some(file) = crate::ui::settings::takeout_file() {
        app.import_takeout(file);
    }
}

struct Tile {
    label: &'static str,
    value: String,
    delta: Option<Delta>,
}

fn totals(app: &App, ui: &mut egui::Ui, report: &Report) {
    tiles(
        ui,
        &app.palette,
        &[
            Tile {
                label: "Plays",
                value: report.plays.to_string(),
                delta: report.plays_delta,
            },
            Tile {
                label: "Listening time",
                // Partly an estimate once an import is in it.
                value: if report.seconds_estimated {
                    format!("~{}", listening_time(report.seconds))
                } else {
                    listening_time(report.seconds)
                },
                delta: report.seconds_delta,
            },
            Tile {
                label: "Songs",
                value: report.songs.to_string(),
                delta: report.songs_delta,
            },
            Tile {
                label: "Artists",
                value: report.artists.to_string(),
                delta: report.artists_delta,
            },
        ],
    );
}

fn streaks(app: &App, ui: &mut egui::Ui, report: &Report) {
    let days = |n: u32| {
        if n == 1 {
            "1 day".to_owned()
        } else {
            format!("{n} days")
        }
    };
    tiles(
        ui,
        &app.palette,
        &[
            Tile {
                label: "Current streak",
                value: days(report.current_streak),
                delta: None,
            },
            Tile {
                label: "Longest streak",
                value: days(report.longest_streak),
                delta: None,
            },
        ],
    );
}

/// A row of tiles, wrapping two by two where four would be cramped.
fn tiles(ui: &mut egui::Ui, p: &theme::Palette, tiles: &[Tile]) {
    let width = ui.available_width();
    let columns = if width < FOUR_TILES_WIDTH {
        tiles.len().min(2)
    } else {
        tiles.len()
    };
    let tile_width = (width - TILE_GAP * (columns - 1) as f32) / columns as f32;
    for row in tiles.chunks(columns) {
        let (rect, _) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::hover());
        for (i, tile) in row.iter().enumerate() {
            let left = rect.left() + i as f32 * (tile_width + TILE_GAP);
            paint_tile(
                ui,
                p,
                Rect::from_min_size(pos2(left, rect.top()), vec2(tile_width, TILE_HEIGHT)),
                tile,
            );
        }
        ui.add_space(TILE_GAP);
    }
}

fn paint_tile(ui: &egui::Ui, p: &theme::Palette, rect: Rect, tile: &Tile) {
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(theme::radii().popup),
        widgets::group_fill(p),
    );
    let inner = rect.shrink(16.0);
    painter.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        tile.label,
        theme::semibold(12.0),
        p.secondary,
    );
    let size = widgets::fit_size(
        ui,
        &tile.value,
        inner.width(),
        TILE_VALUE_PT,
        TILE_VALUE_MIN_PT,
        theme::bold,
    );
    painter.text(
        inner.left_bottom(),
        Align2::LEFT_BOTTOM,
        &tile.value,
        theme::bold(size),
        p.text,
    );
    let Some(delta) = tile.delta else {
        return;
    };
    let text = delta.percent.map_or("New".to_owned(), |n| format!("{n}%"));
    // The change is the extra here: with no room beside the label it isn't shown.
    let label_font = theme::semibold(12.0);
    let delta_width = widgets::text_width(ui, &text, &label_font)
        + if delta.percent.is_some() { 18.0 } else { 0.0 };
    if widgets::text_width(ui, tile.label, &label_font) + 8.0 + delta_width > inner.width() {
        return;
    }
    let shown = painter.text(
        inner.right_top(),
        Align2::RIGHT_TOP,
        text,
        theme::semibold(12.0),
        p.secondary,
    );
    if delta.percent.is_some() {
        let icon = if delta.up {
            Icon::TrendingUp
        } else {
            Icon::TrendingDown
        };
        let size = 14.0;
        icon.image(p.secondary, size).paint_at(
            ui,
            Rect::from_min_size(
                pos2(shown.left() - size - 4.0, shown.center().y - size / 2.0),
                vec2(size, size),
            ),
        );
    }
}

/// Top songs and artists, next to each other when both have something to show and there's room.
fn top_lists(app: &mut App, ui: &mut egui::Ui, report: &Report) {
    let (songs, artists) = (&report.top_songs, &report.top_artists);
    if songs.is_empty() || artists.is_empty() || ui.available_width() < TWO_COLUMNS_WIDTH {
        top_list(app, ui, TopList::Songs, songs);
        top_list(app, ui, TopList::Artists, artists);
        return;
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = COLUMN_GAP;
        ui.columns(2, |columns| {
            // The rows inside lay out with the page's own spacing.
            for column in columns.iter_mut() {
                column.spacing_mut().item_spacing.x = 8.0;
            }
            top_list(app, &mut columns[0], TopList::Songs, songs);
            top_list(app, &mut columns[1], TopList::Artists, artists);
        });
    });
}

/// The start of a top list, with the way to the rest of it in the heading, as on other shelves.
fn top_list(app: &mut App, ui: &mut egui::Ui, list: TopList, items: &[crate::model::Entry]) {
    if items.is_empty() {
        return;
    }
    let title = list.title();
    ui.add_space(20.0);
    let show_all = ui
        .horizontal(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .font(theme::bold(theme::SECTION_PT))
                    .color(app.palette.text),
            );
            items.len() > TOP_SHOWN
                && ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        crate::ui::shelf_show_all(ui, &app.palette).clicked()
                    })
                    .inner
        })
        .inner;
    ui.add_space(8.0);
    ui.push_id(title, |ui| {
        crate::ui::ranked_entry_rows(app, ui, &items[..items.len().min(TOP_SHOWN)]);
    });
    if show_all {
        app.navigate(Route::StatsTop(list));
    }
}

/// A top list on a page of its own.
pub fn top_page(app: &mut App, ui: &mut egui::Ui, list: TopList) {
    let title = list.title();
    if let Some(error) = app.stats_error.clone() {
        page_title(ui, &app.palette, title);
        failed(app, ui, error, App::retry_route);
        return;
    }
    let Some(report) = app.stats_report.clone() else {
        page_title(ui, &app.palette, title);
        widgets::skeleton_shelf(ui, &app.palette, widgets::SkeletonShelfShape::Rows, 8);
        return;
    };
    crate::ui::page_scroll(app, title).show(ui, |ui| {
        page_title(ui, &app.palette, title);
        range_chips(app, ui);
        ui.add_space(12.0);
        let items = match list {
            TopList::Songs => &report.top_songs,
            TopList::Artists => &report.top_artists,
        };
        if items.is_empty() {
            quiet_line(app, ui, "Nothing played in this range.");
        } else {
            crate::ui::ranked_entry_rows(app, ui, items);
        }
        ui.add_space(24.0);
    });
}

/// Plays over time. A bar's date and count show on hovering it; the ends are labelled below.
fn bar_chart(ui: &mut egui::Ui, p: &theme::Palette, bars: &[Bar]) {
    let width = ui.available_width();
    let label_height = 20.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, CHART_HEIGHT + label_height), Sense::hover());
    let plot = Rect::from_min_size(rect.min, vec2(width, CHART_HEIGHT));
    let most = bars.iter().map(|b| b.plays).max().unwrap_or(0).max(1);
    let slot = width / bars.len() as f32;
    let bar_width = (slot * 0.72).clamp(1.0, 40.0);
    let hovered = response
        .hover_pos()
        .map(|at| (((at.x - rect.left()) / slot) as usize).min(bars.len() - 1));
    let painter = ui.painter();
    for (i, bar) in bars.iter().enumerate() {
        let x = plot.left() + slot * (i as f32 + 0.5);
        let height = if bar.plays == 0 {
            2.0
        } else {
            (bar.plays as f32 / most as f32 * plot.height()).max(4.0)
        };
        let shape = Rect::from_min_max(
            pos2(x - bar_width / 2.0, plot.bottom() - height),
            pos2(x + bar_width / 2.0, plot.bottom()),
        );
        let colour = if bar.plays == 0 {
            p.outline
        } else if hovered == Some(i) {
            p.accent_hover
        } else if hovered.is_some() {
            p.accent.gamma_multiply(0.6)
        } else {
            p.accent
        };
        let radius = (bar_width / 3.0).min(4.0) as u8;
        painter.rect_filled(
            shape,
            CornerRadius {
                nw: radius,
                ne: radius,
                sw: 0,
                se: 0,
            },
            colour,
        );
    }
    let label_y = plot.bottom() + 6.0;
    if let Some(first) = bars.first() {
        painter.text(
            pos2(plot.left(), label_y),
            Align2::LEFT_TOP,
            &first.label,
            theme::regular(11.0),
            p.secondary,
        );
    }
    if let Some(last) = bars.last() {
        painter.text(
            pos2(plot.right(), label_y),
            Align2::RIGHT_TOP,
            &last.label,
            theme::regular(11.0),
            p.secondary,
        );
    }
    if let Some(bar) = hovered.and_then(|i| bars.get(i)) {
        response.on_hover_text_at_pointer(format!(
            "{}: {}",
            bar.label,
            plays_label(u64::from(bar.plays))
        ));
    }
}

/// The play counts where a day moves up a shade: the quarters of the days with any plays, so one
/// huge day doesn't leave the rest of the year looking the same.
fn calendar_steps(days: &[(NaiveDate, u32)]) -> [u32; 3] {
    let mut played: Vec<u32> = days
        .iter()
        .map(|(_, plays)| *plays)
        .filter(|plays| *plays > 0)
        .collect();
    played.sort_unstable();
    let at = |share: usize| played.get(played.len() * share / 4).copied().unwrap_or(1);
    [at(1), at(2), at(3)]
}

/// How strongly a day's cell shows: 0 for no plays, then 1 to 4.
fn calendar_level(plays: u32, steps: [u32; 3]) -> usize {
    if plays == 0 {
        0
    } else {
        1 + steps.iter().filter(|step| plays >= **step).count()
    }
}

fn calendar_colour(p: &theme::Palette, level: usize) -> egui::Color32 {
    match level {
        0 => p.glass(p.surface),
        1 => p.glass(widgets::mix(p.panel, p.accent, 0.28)),
        2 => p.glass(widgets::mix(p.panel, p.accent, 0.5)),
        3 => p.glass(widgets::mix(p.panel, p.accent, 0.75)),
        _ => p.accent,
    }
}

/// How many weeks fit beside the day labels, and how big their squares can be. Where a whole year
/// won't fit even at the smallest square, the latest weeks are the ones that stay.
fn calendar_fit(inner_width: f32) -> (usize, f32) {
    let room = (inner_width - CALENDAR_DAY_LABELS + CALENDAR_GAP).max(0.0);
    let weeks = ((room / (CALENDAR_MIN_CELL + CALENDAR_GAP)) as usize).clamp(1, CALENDAR_WEEKS);
    let cell = (room / weeks as f32 - CALENDAR_GAP).clamp(CALENDAR_MIN_CELL, CALENDAR_MAX_CELL);
    (weeks, cell)
}

/// The last year in a card: a square a day, a column a week, Monday at the top. Centred in the
/// card once the squares have reached their largest and there's room to spare.
fn calendar(ui: &mut egui::Ui, p: &theme::Palette, days: &[(NaiveDate, u32)]) {
    let Some(&(first, _)) = days.first() else {
        return;
    };
    let width = ui.available_width();
    let (weeks, cell) = calendar_fit(width - CALENDAR_PADDING * 2.0);
    let step = cell + CALENDAR_GAP;
    let grid_height = step * 7.0 - CALENDAR_GAP;
    let height = CALENDAR_PADDING * 2.0 + CALENDAR_HEADER + CALENDAR_MONTHS + grid_height;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(theme::radii().popup),
        widgets::group_fill(p),
    );
    let inner = rect.shrink(CALENDAR_PADDING);
    let content_width = CALENDAR_DAY_LABELS + step * weeks as f32 - CALENDAR_GAP;
    let inner = Rect::from_center_size(
        inner.center(),
        vec2(content_width.min(inner.width()), inner.height()),
    );
    let total: u64 = days.iter().map(|(_, plays)| u64::from(*plays)).sum();
    painter.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        format!("{} in the last year", plays_label(total)),
        theme::semibold(13.0),
        p.text,
    );

    let grid_left = inner.left() + CALENDAR_DAY_LABELS;
    let grid_top = inner.top() + CALENDAR_HEADER + CALENDAR_MONTHS;
    for (weekday, name) in [(0.0, "Mon"), (2.0, "Wed"), (4.0, "Fri")] {
        painter.text(
            pos2(inner.left(), grid_top + weekday * step + cell / 2.0),
            Align2::LEFT_CENTER,
            name,
            theme::regular(11.0),
            p.secondary,
        );
    }

    let steps = calendar_steps(days);
    let radius = CornerRadius::same((cell / 4.0).round() as u8);
    let first_weekday = first.weekday().num_days_from_monday() as usize;
    let all_weeks = (days.len() + first_weekday).div_ceil(7);
    let first_week = all_weeks.saturating_sub(weeks);
    let hover = response.hover_pos();
    let mut hovered = None;
    let mut month_left = f32::NEG_INFINITY;
    let mut last_month = None;
    for (i, &(date, plays)) in days.iter().enumerate() {
        let index = i + first_weekday;
        let (week, weekday) = (index / 7, index % 7);
        if week < first_week {
            continue;
        }
        let x = grid_left + (week - first_week) as f32 * step;
        let cell_rect =
            Rect::from_min_size(pos2(x, grid_top + weekday as f32 * step), vec2(cell, cell));
        painter.rect_filled(
            cell_rect,
            radius,
            calendar_colour(p, calendar_level(plays, steps)),
        );
        if hover.is_some_and(|at| cell_rect.expand(CALENDAR_GAP / 2.0).contains(at)) {
            painter.rect_stroke(
                cell_rect,
                radius,
                Stroke::new(1.5, p.text),
                StrokeKind::Outside,
            );
            hovered = Some((date, plays));
        }
        // A month's name over its first full week, where it fits between its neighbours and the
        // card's edge.
        if weekday == 0 && last_month != Some(date.month()) {
            last_month = Some(date.month());
            if x >= month_left && x + CALENDAR_MONTH_WIDTH <= inner.right() {
                painter.text(
                    pos2(x, grid_top - CALENDAR_MONTHS),
                    Align2::LEFT_TOP,
                    date.format("%b").to_string(),
                    theme::regular(11.0),
                    p.secondary,
                );
                month_left = x + CALENDAR_MONTH_WIDTH;
            }
        }
    }

    if let Some((date, plays)) = hovered {
        response.on_hover_text_at_pointer(format!(
            "{}: {}",
            date.format("%b %-d, %Y"),
            plays_label(u64::from(plays))
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn year(plays: &[u32]) -> Vec<(NaiveDate, u32)> {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        plays
            .iter()
            .zip(start.iter_days())
            .map(|(plays, date)| (date, *plays))
            .collect()
    }

    #[test]
    fn the_calendar_keeps_a_whole_year_until_even_the_smallest_squares_wont_fit() {
        let (weeks, cell) = calendar_fit(2000.0);
        assert_eq!((weeks, cell), (CALENDAR_WEEKS, CALENDAR_MAX_CELL));
        assert_eq!(calendar_fit(600.0).0, CALENDAR_WEEKS);
        let inner = 330.0;
        let (weeks, cell) = calendar_fit(inner);
        assert!(weeks < CALENDAR_WEEKS);
        assert_eq!(cell.max(CALENDAR_MIN_CELL), cell);
        let grid = (cell + CALENDAR_GAP) * weeks as f32 - CALENDAR_GAP;
        assert!(
            CALENDAR_DAY_LABELS + grid <= inner + 0.5,
            "stays in its card"
        );
    }

    #[test]
    fn a_day_with_no_plays_stays_empty_and_any_play_shows() {
        let steps = calendar_steps(&year(&[0, 1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(calendar_level(0, steps), 0);
        assert_eq!(calendar_level(1, steps), 1);
        assert_eq!(calendar_level(8, steps), 4);
    }

    /// The shades follow how you usually listen, so one huge day doesn't wash out the rest.
    #[test]
    fn one_huge_day_leaves_the_others_their_shades() {
        let steps = calendar_steps(&year(&[1, 2, 3, 4, 5, 6, 7, 500]));
        assert_eq!(calendar_level(2, steps), 1);
        assert_eq!(calendar_level(4, steps), 2);
        assert_eq!(calendar_level(6, steps), 3);
        assert_eq!(calendar_level(500, steps), 4);
    }

    #[test]
    fn a_year_with_nothing_played_has_steps_anyway() {
        assert_eq!(calendar_steps(&year(&[0, 0])), [1, 1, 1]);
    }
}
