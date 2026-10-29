//! Charts, behind Explore's second button: ranked sections first, then card shelves.
//!
//! A ranked section is a three-column grid of cells — rank, movement, still, title, credits —
//! as the reference lays it out. Clicking a cell plays it, queuing its section's playables;
//! the row menu does not come along (v1), because the cells are custom paint, not track rows.

use egui::{Rect, Sense, vec2};

use crate::app::App;
use crate::icons::Icon;
use crate::model::{ChartSection, ChartTrend};
use crate::theme;
use crate::ui::{failed, section, shelf_carousel};
use crate::widgets;

/// The page's title, drawn by each state so it scrolls away with the list rather than sticking.
fn title(app: &App, ui: &mut egui::Ui) {
    crate::ui::page_title(ui, &app.palette, "Charts");
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    if app.charts_loading && app.charts.is_none() {
        title(app, ui);
        // Card shelves — the daily and weekly charts and the ones by language, too short to
        // scroll so their headings carry no arrows, then the top artists' round cards. Where a
        // region's charts open with ranked grids those arrive first, but the shelves are what
        // every account gets.
        for _ in 0..3 {
            widgets::skeleton_heading(ui, &app.palette, false);
            ui.add_space(4.0);
            widgets::skeleton_card_row(ui, &app.palette, widgets::ArtShape::Square);
        }
        widgets::skeleton_shelf(
            ui,
            &app.palette,
            widgets::SkeletonShelfShape::Cards(widgets::ArtShape::Circle),
            1,
        );
        return;
    }
    if let Some(err) = app.charts_error.clone() {
        title(app, ui);
        failed(app, ui, err, App::retry_route);
        return;
    }

    crate::ui::page_scroll(app, "charts").show(ui, |ui| {
        title(app, ui);
        // One snapshot for the frame — a refcount bump, not a copy of every section. The
        // cells need `&mut App` for art and playback, which rules out borrowing the feed
        // out of `app` across the call.
        let Some(feed) = app.charts.clone() else {
            return;
        };
        for s in feed.ranked.iter() {
            section(ui, &app.palette, &s.title);
            ranked(app, ui, s);
            ui.add_space(8.0);
        }
        for s in feed.shelves.iter() {
            shelf_carousel(
                app,
                ui,
                "charts",
                s,
                None,
                crate::ui::MoreButton::Pill("More"),
            );
        }
    });
}

/// One ranked section as a three-column grid. The cells are one queue: clicking queues the
/// section's playables from the clicked cell on.
fn ranked(app: &mut App, ui: &mut egui::Ui, section: &ChartSection) {
    let gap = 12.0;
    let cols = 3;
    let cell_w = (ui.available_width() - (cols - 1) as f32 * gap) / cols as f32;
    let cell_h = 56.0;
    // The clicked cell, acted on after the loop: building the queue up front cloned every
    // track in the section on every frame, for a click that mostly does not come.
    let mut play: Option<String> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for row in section.rows.iter() {
            let Some(track) = row.entry.track() else {
                continue;
            };
            let (rect, response) = ui.allocate_exact_size(vec2(cell_w, cell_h), Sense::click());
            // Double-click-to-play applies here too: a single click marks the cell, a double
            // click plays it — `activate` is the flag, not `response.clicked()`.
            let activate = widgets::activate(ui.ctx(), &response, track.video_id.as_str());
            if ui.is_rect_visible(rect) {
                let p = app.palette;
                let t = widgets::lit(ui, &response);
                if widgets::is_selected(ui.ctx(), track.video_id.as_str()) {
                    ui.painter().rect_filled(
                        rect,
                        egui::CornerRadius::same(theme::radii().small),
                        p.glass_state(p.surface_active, 0.55),
                    );
                }
                if t > 0.001 {
                    ui.painter().rect_filled(
                        rect,
                        egui::CornerRadius::same(theme::radii().small),
                        p.glass_state(p.surface_hover, t),
                    );
                }
                let art_rect =
                    Rect::from_min_size(rect.left_center() + vec2(4.0, -20.0), vec2(40.0, 40.0));
                let crate::app::App {
                    images, backend, ..
                } = app;
                let art = crate::ui::art_texture(
                    images,
                    backend,
                    ui.ctx(),
                    track.thumbnail.as_ref(),
                    40.0,
                );
                widgets::paint_art(ui, &p, art_rect, art, widgets::ArtShape::Square);
                // Rank and movement, then title over credits.
                let mut x = art_rect.right() + 8.0;
                // Steady is a state YouTube reports (`ARROW_CHART_NEUTRAL`), so it says so with
                // a dash. Unknown is the absence of one and paints nothing.
                let (icon, fg) = match row.trend {
                    ChartTrend::Up => (Some(Icon::TrendingUp), p.accent),
                    ChartTrend::Down => (Some(Icon::TrendingDown), p.danger),
                    ChartTrend::Steady => (Some(Icon::Minus), p.dim),
                    ChartTrend::Unknown => (None, p.dim),
                };
                if let Some(icon) = icon {
                    icon.image(fg, 12.0).paint_at(
                        ui,
                        Rect::from_min_size(
                            egui::pos2(x, rect.center().y - 15.0),
                            vec2(12.0, 12.0),
                        ),
                    );
                    x += 15.0;
                }
                if !row.rank.is_empty() {
                    ui.painter().text(
                        egui::pos2(x, rect.center().y - 9.0),
                        egui::Align2::LEFT_CENTER,
                        &row.rank,
                        theme::semibold(12.0),
                        fg,
                    );
                    x += widgets::text_width(ui, &row.rank, &theme::semibold(12.0)) + 8.0;
                }
                let text_w = (rect.right() - 4.0 - x).max(0.0);
                crate::emoji::paint_line(
                    ui,
                    egui::pos2(x, rect.center().y + 6.0),
                    &widgets::truncate_to(ui, &track.title, theme::medium(13.0), text_w),
                    theme::medium(13.0),
                    widgets::mix(p.secondary, p.text, t),
                );
                let credits = track.artist_line();
                crate::emoji::paint_line(
                    ui,
                    egui::pos2(x, rect.center().y + 20.0),
                    &widgets::truncate_to(ui, &credits, theme::regular(11.0), text_w),
                    theme::regular(11.0),
                    p.dim,
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
            if activate {
                play = Some(track.video_id.clone());
            }
        }
    });
    if let Some(video_id) = play {
        let queue: Vec<crate::model::Track> = section
            .rows
            .iter()
            .filter_map(|r| r.entry.track().cloned())
            .collect();
        if let Some(index) = queue.iter().position(|t| t.video_id == video_id) {
            app.start_from(queue, index);
        }
    }
}
