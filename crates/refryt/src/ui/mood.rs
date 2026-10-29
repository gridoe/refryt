//! One mood's playlists, reached from the home page's Moods & genres grid.

use crate::app::{App, Route};
use crate::ui::{art_texture, card_is_visible, failed, section};
use crate::widgets;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    if app.mood_playlists_loading {
        // The mood's name arrives with its playlists; the section heading is always this one.
        widgets::skeleton_page_title(ui, &app.palette);
        section(ui, &app.palette, "Playlists");
        widgets::skeleton_grid(ui, &app.palette, 2, widgets::ArtShape::Square);
        return;
    }
    if let Some(err) = app.mood_playlists_error.clone() {
        failed(app, ui, err, App::retry_route);
        return;
    }
    let Some(mood) = app.mood_playlists.as_ref() else {
        return;
    };

    let title = mood.title.clone();
    crate::ui::page_scroll(app, "mood").show(ui, |ui| {
        // Inside the scroll, so the title leaves with the page rather than sticking.
        crate::ui::page_title(ui, &app.palette, &title);
        section(ui, &app.palette, "Playlists");
        let mut clicked = None;
        let mut played = None;
        ui.horizontal_wrapped(|ui| {
            let card_w = crate::ui::grid_card_w(ui);
            for (i, pl) in app
                .mood_playlists
                .as_ref()
                .map(|m| m.playlists.as_slice())
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                let art = card_is_visible(ui, card_w, widgets::ArtShape::Square)
                    .then(|| {
                        art_texture(
                            &mut app.images,
                            &app.backend,
                            ui.ctx(),
                            pl.thumbnail.as_ref(),
                            card_w - 16.0,
                        )
                    })
                    .flatten();
                let card = widgets::art_card(
                    ui,
                    &app.palette,
                    card_w,
                    art,
                    &pl.title,
                    &pl.author,
                    widgets::ArtShape::Square,
                    None,
                    crate::ui::playlist_active(&app.now, &pl.id) && !app.now.paused,
                    false,
                );
                // The corner play button wins over the card click, the same contract every
                // other card grid follows: play the playlist, don't open it.
                if card.play {
                    played = Some(i);
                } else if card.response.clicked() {
                    clicked = Some(i);
                }
            }
        });
        if let Some(i) = played {
            let id = app
                .mood_playlists
                .as_ref()
                .and_then(|m| m.playlists.get(i))
                .map(|pl| pl.id.clone());
            if let Some(id) = id {
                app.play_playlist(id);
            }
        }
        if let Some(i) = clicked {
            let id = app
                .mood_playlists
                .as_ref()
                .and_then(|m| m.playlists.get(i))
                .map(|pl| pl.id.clone());
            if let Some(id) = id {
                app.navigate(Route::Playlist(id));
            }
        }
    });
}
