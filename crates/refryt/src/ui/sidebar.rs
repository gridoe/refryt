//! The left rail: where you can go, and what you have saved.
//!
//! It runs the whole left side of the window now — top edge to player bar — because the shell
//! claims it before the top bar rather than after a full-width titlebar. Settings has moved to
//! the top right, so nothing is pinned to the bottom except an audio failure, and that only when
//! there is one.
//!
//! Only the library list scrolls. Scrolling the whole rail meant the nav items drifted off the
//! top as soon as you had more than a handful of playlists, and the version before that put a
//! 220px box around them — a scrollbar inside a scrollbar showing five of thirty-one.

use egui::{Rect, Sense, vec2};

use crate::app::{App, Route};
use crate::chrome;
use crate::icons::Icon;
use crate::theme;
use crate::ui::{ROW_ART_PT, art_texture, virtual_rows};
use crate::widgets::{self, ArtShape};

pub fn sidebar(app: &mut App, ui: &mut egui::Ui) {
    // The card's own rect, taken before any panel claims space: the corner repaint at the end
    // needs its rounded edges.
    let card = widgets::card_rect(ui.available_rect_before_wrap(), theme::CARD_INSET);
    let _edge = widgets::CardEdge::new(ui, card, &app.palette);
    widgets::clip_card_content(ui, card, theme::CARD_INSET_X, &app.palette);
    // Folded down to the icon rail: icons and covers only. Decided from the width as it is drawn,
    // so a sidebar being dragged narrower turns into the rail as it passes the snap point, and
    // lets go as one.
    let rail = theme::sidebar_is_rail(card.width() + theme::GUTTER);
    // Claimed first, so the scrolling middle gets whatever is left rather than pushing it off.
    if app.player_error.is_some() {
        egui::Panel::bottom("sidebar-footer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| audio_warning(app, ui, rail));
    }

    app.sidebar_rail = rail;
    if app.traffic_lights_shown && !app.lights_over_rail() {
        // macOS: the traffic lights sit in the card's top-left corner, centred on the top bar
        // (`App::place_traffic_lights`). The strip they sit in drags the window like a title
        // bar, and the nav starts below it, level with the top of the page card. Fullscreen
        // hides the lights, and the nav takes the corner back.
        let band = Rect::from_min_max(
            card.left_top(),
            egui::pos2(card.right(), theme::TITLEBAR_HEIGHT),
        );
        chrome::drag_surface(ui, band, app.decorated, "sidebar");
        ui.add_space((theme::TITLEBAR_HEIGHT - ui.cursor().top()).max(8.0));
    } else {
        ui.add_space(8.0);
    }
    for (route, icon, label) in [
        (Route::Home, Icon::House, "Home"),
        // Explore rather than Search: searching happens in the field at the top of the window,
        // and the nav slot is better spent on the page YouTube puts there.
        (Route::Explore, Icon::Compass, "Explore"),
        (Route::Library, Icon::Library, "Library"),
    ] {
        let selected = app.current_route() == route;
        if widgets::nav_item(ui, &app.palette, icon, label, selected, rail).clicked() {
            app.navigate(route);
        }
    }

    playlists(app, ui, rail);
    // The card's rounded corners, over everything the rail drew: nav rows and playlist rows
    // both run to the card's top and bottom edges now.
    widgets::paint_corner_notches(ui, card, theme::CARD_INSET_X, &app.palette);
}

/// Only shown when playback could not start: libmpv never came up, or an attempt stalled until
/// its retries ran out. The rail does not reserve space for a message that almost never appears.
fn audio_warning(app: &mut App, ui: &mut egui::Ui, rail: bool) {
    let Some(err) = app.player_error.clone() else {
        return;
    };
    if rail {
        // No room for the message: a warning sign, with the message on hover.
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
            Icon::CircleAlert
                .image(app.palette.danger, 22.0)
                .paint_at(ui, Rect::from_center_size(rect.center(), vec2(22.0, 22.0)));
            widgets::tooltip_right(response, format!("Audio unavailable: {err}"));
        });
        ui.add_space(8.0);
        return;
    }
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new("AUDIO UNAVAILABLE")
            .font(theme::semibold(10.0))
            .color(app.palette.danger),
    );
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(err)
            .font(theme::regular(11.0))
            .color(app.palette.secondary),
    );
    ui.add_space(10.0);
}

/// Your playlists, and a way to make another.
///
/// No "Library" header and no Playlists/Albums/Artists chips: the rail said Library twice — once
/// as a nav item and once over chips that duplicated the page that item opens — and the chips now
/// live on the Library page where they belong. No divider either; the card's own edge is the
/// separation, which is what the divider was doing badly.
fn playlists(app: &mut App, ui: &mut egui::Ui, rail: bool) {
    ui.add_space(14.0);
    if rail {
        // No heading to carry: just the + , centred over the covers.
        ui.vertical_centered(|ui| {
            let plus = widgets::icon_button(ui, &app.palette, Icon::Plus, 18.0, false);
            if widgets::tooltip_right(plus, "New playlist").clicked() {
                toggle_new_playlist(app);
            }
        });
    } else {
        ui.horizontal(|ui| {
            // The + button's box, not the label's line, is the row's height — otherwise the
            // button hangs below the label instead of centering against it.
            ui.set_min_height(32.0);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Playlists")
                    .font(theme::semibold(13.0))
                    .color(app.palette.secondary),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, &app.palette, Icon::Plus, 18.0, false).clicked() {
                    toggle_new_playlist(app);
                }
            });
        });
    }
    ui.add_space(4.0);
    new_playlist_popup(app, ui);
    rail_list(app, ui, rail);
}

/// A toggle: the popup is a window, and the same press must be able to dismiss it.
fn toggle_new_playlist(app: &mut App) {
    let open = app.new_playlist.is_some();
    app.new_playlist = if open { None } else { Some(String::new()) };
    app.new_playlist_kind = crate::app::NewPlaylistKind::YouTube;
}

/// The modal the **+** opens: the app dims behind a centered card — name field, Create,
/// Cancel — rather than a form wedged into the rail. Enter or Create commits; Escape, Cancel or
/// clicking the dim abandons.
///
/// One area paints both the dim and the card. Two layers would not hold: egui promotes a
/// layer on pointer interaction, so a dim layer moved above the window the moment the
/// pointer crossed it, dimming the card along with everything else. In one layer the paint
/// order is the code order, and the modal stays modal.
fn new_playlist_popup(app: &mut App, ui: &mut egui::Ui) {
    let Some(mut name) = app.new_playlist.clone() else {
        return;
    };
    let Some(screen) = ui.ctx().input(|i| i.raw.screen_rect) else {
        return;
    };

    let mut dismissed = false;
    egui::Area::new(egui::Id::new("ytm-new-playlist-modal"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            let (full, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(full, 0.0, egui::Color32::from_black_alpha(150));

            // Centered, a hair above true centre so it clears the player bar.
            let card = egui::Rect::from_center_size(
                full.center() - egui::vec2(0.0, 12.0),
                egui::vec2(320.0, 132.0),
            );
            // Only a click outside the card dismisses — a click landing on a blank gap between
            // fields inside the card (nothing there senses clicks) used to fall through to this
            // backdrop and discard whatever name had already been typed.
            if response.clicked()
                && response
                    .interact_pointer_pos()
                    .is_some_and(|pos| !card.contains(pos))
            {
                dismissed = true;
            }
            let mut card_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card)
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            {
                let painter = card_ui.painter();
                // The theme's soft shadow, stepped: three expanding translucent rects
                // read as one blur at these alphas.
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
                    app.palette.overlay,
                );

                let mut body = card_ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(card.shrink(16.0))
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                body.label(
                    egui::RichText::new("New playlist")
                        .font(theme::semibold(15.0))
                        .color(app.palette.text),
                );
                body.add_space(4.0);
                let field = egui::TextEdit::singleline(&mut name)
                    .id(egui::Id::new("ytm-new-playlist"))
                    .hint_text(egui::RichText::new("Playlist name").color(app.palette.dim))
                    .desired_width(f32::INFINITY)
                    .font(theme::regular(13.0));
                let response = body.add(field);
                response.request_focus();
                let commit =
                    response.lost_focus() && body.input(|i| i.key_pressed(egui::Key::Enter));
                let cancel = body.input(|i| i.key_pressed(egui::Key::Escape));

                body.add_space(8.0);
                let mut create = commit;
                let mut close = cancel || dismissed;
                // A fixed-height row: a bare right-to-left layout takes the window's
                // whole remaining height and centers the buttons in a card two hundred
                // points too tall.
                body.allocate_ui(egui::vec2(body.available_width(), 30.0), |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::chip(ui, &app.palette, "Create", true).clicked() {
                            create = true;
                        }
                        if widgets::chip(ui, &app.palette, "Cancel", false).clicked() {
                            close = true;
                        }
                    });
                });

                if create && !name.trim().is_empty() {
                    let title = name.trim().to_owned();
                    match std::mem::take(&mut app.new_playlist_kind) {
                        crate::app::NewPlaylistKind::YouTube => app.create_playlist(title),
                        crate::app::NewPlaylistKind::Local { track_paths } => {
                            app.create_local_playlist(title, track_paths)
                        }
                    }
                    app.new_playlist = None;
                } else if close {
                    app.new_playlist = None;
                } else {
                    app.new_playlist = Some(name);
                }
            }
        });
}

/// One entry of the rail: cover, title, and what it is.
struct RailRow<'a> {
    id: &'a str,
    title: &'a str,
    detail: String,
    thumbnail: Option<&'a std::sync::Arc<str>>,
    shape: ArtShape,
}

fn rail_list(app: &mut App, ui: &mut egui::Ui, rail: bool) {
    let count = app.library.playlists.len();
    if count == 0 {
        // No sentence fits in the icon rail, and an empty one says enough by being empty.
        if rail {
            return;
        }
        ui.add_space(6.0);
        // In line with the "Playlists" heading above it, which sits 6 pt in.
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("No playlists")
                    .font(theme::regular(12.0))
                    .color(app.palette.dim),
            );
        });
        return;
    }

    let selected_id = match app.current_route() {
        Route::Playlist(id) => Some(id),
        _ => None,
    };
    let signed_in = app.signed_in();
    let mut clicked = None;
    let mut reorder = None;
    let mut menu_action = None;
    let from = widgets::painted_so_far(ui);
    let output = egui::ScrollArea::vertical()
        .id_salt("rail")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let App {
                library,
                palette,
                images,
                backend,
                playlist,
                rail_cache,
                local_playlists,
                local_tracks,
                ..
            } = app;
            let row_height = if rail {
                theme::RAIL_ROW_HEIGHT_COMPACT
            } else {
                theme::RAIL_ROW_HEIGHT
            };
            virtual_rows(ui, row_height, count, |ui, i| {
                let p = &library.playlists.items[i];
                let row = RailRow {
                    id: &p.id,
                    title: &p.title,
                    // The section header already says Playlists; the row says who made it,
                    // when it is someone else's list, and how much is in it when it is yours.
                    detail: p.detail(),
                    thumbnail: p.thumbnail.as_ref(),
                    shape: ArtShape::Square,
                };
                let art = art_texture(images, backend, ui.ctx(), row.thumbnail, ROW_ART_PT);
                let selected = selected_id.as_deref() == Some(row.id);
                let response = if rail {
                    rail_row_compact(ui, palette, &row, art, selected)
                } else {
                    rail_row(ui, palette, &row, art, selected)
                };
                // Drag a playlist to reorder the rail. The payload is this row's index; the
                // drop lands on whichever row is under the pointer when the button is released.
                if response.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), i);
                }
                if egui::DragAndDrop::has_payload_of_type::<usize>(ui.ctx())
                    && response.contains_pointer()
                {
                    if ui.ctx().input(|inp| inp.pointer.any_released()) {
                        if let Some(from) = egui::DragAndDrop::take_payload::<usize>(ui.ctx()) {
                            reorder = Some((*from, i));
                        }
                    } else {
                        // Show where it would land: `remove(from)` then `insert(i, _)` puts the
                        // dragged row immediately *after* `i` when dragging downward (removing
                        // from earlier in the list shifts `i` itself left by one first), but
                        // immediately *before* `i` when dragging upward (nothing before `from`
                        // moves, so the insert lands right where `i` used to start). The line
                        // has to flip edges to match, or an upward drag lands one row off from
                        // what it showed.
                        let from = egui::DragAndDrop::payload::<usize>(ui.ctx());
                        let dragging_up = from.is_some_and(|from| i < *from);
                        let y = if dragging_up {
                            response.rect.top()
                        } else {
                            response.rect.bottom()
                        };
                        ui.painter().hline(
                            egui::Rangef::new(response.rect.left(), response.rect.right()),
                            y,
                            egui::Stroke::new(2.0, palette.accent),
                        );
                    }
                }
                if response.clicked() {
                    clicked = Some(Route::Playlist(row.id.to_owned()));
                }
                // Right-click: the same ••• menu the playlist page offers. The track actions
                // (shuffle, queue, save) need the playlist's tracks, so a first right-click
                // kicks off a fetch; the menu shows those items at once and an early pick waits
                // for the songs (`tracks_pending`). A local
                // playlist never needs this — it's already fully in memory, so `local_view`
                // below is built fresh every time instead of going through `rail_cache` at all.
                let is_local = p.source == crate::model::PlaylistSource::Local;
                if response.secondary_clicked()
                    && !is_local
                    && !rail_cache.contains_key(&p.id)
                    && playlist.as_ref().map(|pl| pl.id.as_str()) != Some(p.id.as_str())
                {
                    backend.fetch_playlist(p.id.clone());
                }
                let local_view = is_local
                    .then(|| local_playlists.iter().find(|lp| lp.id == p.id))
                    .flatten()
                    .map(|lp| crate::app::build_local_playlist(lp, local_tracks));
                let cached = local_view.as_ref().or_else(|| {
                    rail_cache
                        .get(&p.id)
                        .or_else(|| playlist.as_ref().filter(|pl| pl.id == p.id))
                });
                let tracks: &[crate::model::Track] =
                    cached.map(|pl| pl.tracks.items.as_slice()).unwrap_or(&[]);
                if let Some(action) = widgets::page_menu(
                    ui,
                    palette,
                    &response,
                    widgets::PageMenu {
                        page_id: &p.id,
                        tracks,
                        artist_id: p.author_id.as_deref(),
                        playlists: &library.playlists.items,
                        signed_in,
                        // There is no real YouTube URL for a local playlist to share.
                        share_url: if is_local {
                            String::new()
                        } else {
                            crate::model::playlist_url(&p.id)
                        },
                        // Not here: the description only arrives with the fetch, so Info would
                        // pop into an open menu. The playlist page's own ••• offers it.
                        has_info: false,
                        manage: p.editable,
                        remove_from_library: !p.editable && !crate::model::is_liked_auto(&p.id),
                        context_only: true,
                        // A local playlist has no YouTube radio to start.
                        radio_id: (!is_local).then_some(p.id.as_str()),
                        save_as_new: None,
                        // The songs a right-click has only just asked for: the menu opens
                        // complete, and an item picked before they land waits for them.
                        tracks_pending: cached.is_none().then_some(crate::ui::PageKind::Playlist),
                    },
                ) {
                    menu_action = Some(action);
                }
            });
        });
    widgets::fade_scroll_edges(ui, from, &output, widgets::scroll_fade(&app.settings));
    if let Some((from, to)) = reorder {
        app.move_playlist(from, to);
    }
    if let Some(action) = menu_action {
        app.row_action(action);
    }
    if let Some(route) = clicked {
        app.navigate(route);
    }
}

/// A row of the icon rail: the cover, centred, with the title and what it is as a tooltip. The pill
/// for the selected and the hovered row is the same as the full row's.
fn rail_row_compact(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    row: &RailRow<'_>,
    art: Option<&egui::TextureHandle>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::RAIL_ROW_HEIGHT_COMPACT),
        Sense::click_and_drag(),
    );
    if ui.is_rect_visible(rect) {
        let t = widgets::lit(ui, &response);
        if selected || t > 0.001 {
            let fill = if selected {
                palette.glass(palette.surface_active)
            } else {
                palette.glass_state(palette.surface_hover, t)
            };
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(theme::radii().control), fill);
        }
        let art_rect = Rect::from_center_size(
            rect.center(),
            vec2(theme::RAIL_ART_SIZE, theme::RAIL_ART_SIZE),
        );
        widgets::paint_art(ui, palette, art_rect, art, row.shape);
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    widgets::tooltip_right(response, rail_tooltip(row.title, &row.detail))
}

/// What a rail row says on hover: its title, and under it what it is when there is something to say.
fn rail_tooltip(title: &str, detail: &str) -> String {
    if detail.is_empty() {
        title.to_owned()
    } else {
        format!("{title}\n{detail}")
    }
}

/// A rail row: cover, title, and what it is — the shape the reference uses, and the reason the
/// rail is worth 280pt instead of 240.
fn rail_row(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    row: &RailRow<'_>,
    art: Option<&egui::TextureHandle>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::RAIL_ROW_HEIGHT),
        Sense::click_and_drag(),
    );
    if ui.is_rect_visible(rect) {
        let t = widgets::lit(ui, &response);
        if selected || t > 0.001 {
            let fill = if selected {
                palette.glass(palette.surface_active)
            } else {
                palette.glass_state(palette.surface_hover, t)
            };
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(theme::radii().control), fill);
        }

        let art_rect = Rect::from_center_size(
            rect.left_center() + vec2(6.0 + theme::RAIL_ART_SIZE / 2.0, 0.0),
            vec2(theme::RAIL_ART_SIZE, theme::RAIL_ART_SIZE),
        );
        widgets::paint_art(ui, palette, art_rect, art, row.shape);

        let x = art_rect.right() + 10.0;
        let w = (rect.right() - 8.0 - x).max(0.0);
        let title_font = theme::medium(13.0);
        crate::emoji::paint_line(
            ui,
            egui::pos2(x, rect.center().y - 9.0),
            &widgets::truncate_to(ui, row.title, title_font.clone(), w),
            title_font,
            if selected {
                palette.accent_text()
            } else {
                palette.text
            },
        );
        let detail_font = theme::regular(11.0);
        crate::emoji::paint_line(
            ui,
            egui::pos2(x, rect.center().y + 9.0),
            &widgets::truncate_to(ui, &row.detail, detail_font.clone(), w),
            detail_font,
            palette.dim,
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::rail_tooltip;

    /// What the icon rail says on hover: the title, and under it what the playlist is when there
    /// is something to say; a bare title, not a title and an empty line, when there is not.
    #[test]
    fn a_rail_tooltip_is_the_title_and_the_detail_when_there_is_one() {
        assert_eq!(rail_tooltip("Gym", "12 tracks"), "Gym\n12 tracks");
        assert_eq!(rail_tooltip("Gym", ""), "Gym");
    }
}
