//! Explore: what YouTube Music puts where the rail used to say Search.
//!
//! The three buttons across the top come from the feed itself — their browse ids and params
//! ride the response, so they never go stale in code — and the shelves below scroll sideways
//! as carousels, which is what makes this page the reference's rather than another wrapped
//! grid.

use egui::{Rect, Sense, vec2};

use crate::app::{App, Route};
use crate::icons::Icon;
use crate::theme;
use crate::ui::{failed, shelf_carousel};
use crate::widgets;

/// Which button a feed link is: icon and destination, by the browse id YouTube sent.
fn link_target(link: &crate::model::ExploreLink) -> Option<(Icon, Route)> {
    match link.browse_id.as_str() {
        "FEmusic_new_releases" => Some((Icon::Disc3, Route::NewReleases)),
        "FEmusic_charts" => Some((Icon::TrendingUp, Route::Charts(link.params.clone()))),
        "FEmusic_moods_and_genres" => Some((Icon::Smile, Route::Moods)),
        _ => None,
    }
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    if app.explore_loading && app.explore.is_none() {
        crate::ui::page_title(ui, &app.palette, "Explore");
        // The three buttons are known before the feed is (their static fallbacks), so they are
        // real from the start; then the feed's usual shape: new albums, top songs, and the
        // Moods & genres chips.
        top_buttons(app, ui);
        widgets::skeleton_shelf(
            ui,
            &app.palette,
            widgets::SkeletonShelfShape::Cards(widgets::ArtShape::Square),
            1,
        );
        widgets::skeleton_shelf(
            ui,
            &app.palette,
            widgets::SkeletonShelfShape::Cards(widgets::ArtShape::Square),
            1,
        );
        widgets::skeleton_shelf(ui, &app.palette, widgets::SkeletonShelfShape::Chips, 3);
        return;
    }
    if let Some(err) = app.explore_error.clone() {
        crate::ui::page_title(ui, &app.palette, "Explore");
        failed(app, ui, err, App::retry_route);
        return;
    }

    // The title scrolls away with the page rather than staying pinned over it.
    crate::ui::page_scroll(app, "explore").show(ui, |ui| {
        crate::ui::page_title(ui, &app.palette, "Explore");
        top_buttons(app, ui);

        // One snapshot for the frame — a refcount bump, not a copy of every shelf. The
        // carousel needs `&mut App` for navigation and the texture cache, which is why the
        // feed cannot simply be borrowed out of `app` across the call.
        let Some(shelves) = app.explore.clone() else {
            return;
        };
        for s in shelves.iter() {
            // The Moods & genres shelf knows its full list; the rest scroll with arrows.
            let more = s.is_mood_list().then_some(Route::Moods);
            shelf_carousel(
                app,
                ui,
                "explore",
                s,
                more,
                crate::ui::MoreButton::Pill("More"),
            );
        }
    });
}

/// The row YouTube puts above the shelves. The feed names the buttons and carries their
/// destinations; with no links parsed — an old response shape — three static cards keep the
/// navigation working.
fn top_buttons(app: &mut App, ui: &mut egui::Ui) {
    let cards: Vec<(Icon, String, Route)> = if app.explore_links.is_empty() {
        vec![
            (Icon::Disc3, "New releases".to_owned(), Route::NewReleases),
            (Icon::TrendingUp, "Charts".to_owned(), Route::Charts(None)),
            (Icon::Smile, "Moods & genres".to_owned(), Route::Moods),
        ]
    } else {
        app.explore_links
            .iter()
            .filter_map(|link| {
                link_target(link).map(|(icon, route)| (icon, link.title.clone(), route))
            })
            .collect()
    };
    if cards.is_empty() {
        return;
    }
    let gap = 12.0;
    let card_w =
        (ui.available_width() - (cards.len().saturating_sub(1)) as f32 * gap) / cards.len() as f32;
    let card_h = 64.0;
    let mut go = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (icon, title, route) in &cards {
            let (rect, response) = ui.allocate_exact_size(vec2(card_w, card_h), Sense::click());
            if ui.is_rect_visible(rect) {
                let p = app.palette;
                let t = widgets::lit(ui, &response);
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(theme::radii().control),
                    p.glass(widgets::mix(p.surface, p.surface_hover, t)),
                );
                let nudge = widgets::press_offset(&response);
                let fg = widgets::mix(p.secondary, p.text, t);
                // Icon and title sit on the card's vertical centre as one row. They used to be
                // pinned to the top and bottom edges respectively, which left the title hanging
                // low with a dead gap above it.
                let center = rect.center().y;
                icon.image(fg, 22.0).paint_at(
                    ui,
                    Rect::from_center_size(
                        egui::pos2(rect.left() + 16.0 + 11.0, center),
                        vec2(22.0, 22.0),
                    )
                    .translate(nudge),
                );
                ui.painter().text(
                    egui::pos2(rect.left() + 16.0 + 22.0 + 12.0, center) + nudge,
                    egui::Align2::LEFT_CENTER,
                    widgets::truncate_to(ui, title, theme::semibold(16.0), card_w - 66.0),
                    theme::semibold(16.0),
                    widgets::mix(p.text, p.text, t),
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
            if response.clicked() {
                go = Some(route.clone());
            }
        }
    });
    ui.add_space(10.0);
    if let Some(route) = go {
        app.navigate(route);
    }
}
