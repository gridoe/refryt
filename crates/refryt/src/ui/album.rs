//! The album page: header plus a numbered track list.
//!
//! The header keeps the app's horizontal orientation — art left, words right — at the
//! reference's scale: a large cover, a linked artist, one meta line, and a control row of
//! Play, Save and •••. The ••• menu acts on the whole track list; "More by …" and the
//! About section are gone, the description living behind the menu's Info item instead.

use crate::app::App;
use crate::model::Album;
use crate::ui::{ListClick, PAGE_ART_SIZE, track_rows};
use crate::widgets;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    if app.album_loading {
        crate::ui::skeleton_page_header(
            ui,
            &app.palette,
            crate::ui::HeaderSkeleton {
                kind: crate::ui::HeaderKind::Bar,
                title: None,
                description: false,
                credit_line: true,
                credit_air: 5.6,
                controls: crate::ui::HeaderControls::PlayOnly,
            },
        );
        ui.add_space(10.0);
        widgets::skeleton_track_rows(ui, &app.palette, 8, true);
        return;
    }
    if let Some(err) = app.album_error.clone() {
        crate::ui::failed(app, ui, err, App::retry_route);
        return;
    }
    if app.album.is_none() {
        return;
    }

    let wash =
        crate::ui::header_wash_colour(app, app.album.as_ref().and_then(|a| a.thumbnail.as_ref()));
    crate::ui::page_scroll(app, "album").show(ui, |ui| {
        // The tint travels with the page: painted in scroll coordinates, it scrolls away
        // with the header instead of lingering as a background.
        crate::ui::header_wash(ui, &app.palette, app.content_rect, ui.cursor().min.y, wash);
        header(app, ui);
        crate::ui::info_popup(app, ui);
        // Breathing room between the header and the first row.
        ui.add_space(10.0);
        track_list(app, ui);
        shelves(app, ui);
    });
}

/// "Other versions" and "Releases for you" under the tracks, as YouTube Music shows them.
fn shelves(app: &mut App, ui: &mut egui::Ui) {
    // One snapshot for the frame: the carousel needs `&mut App`, which rules out borrowing the
    // album out of `app` across the call.
    let Some(shelves) = app.album.as_ref().map(|a| a.shelves.clone()) else {
        return;
    };
    if !shelves.is_empty() {
        // Clear of the last row: a shelf's title brings less air above it than a page section.
        ui.add_space(16.0);
    }
    for s in &shelves {
        crate::ui::shelf_carousel(
            app,
            ui,
            "album",
            s,
            None,
            crate::ui::MoreButton::Pill("More"),
        );
    }
}

fn header(app: &mut App, ui: &mut egui::Ui) {
    let signed_in = app.signed_in();
    let playing = app
        .album
        .as_ref()
        .is_some_and(|a| app.is_playing_any(&a.tracks));

    // Everything the header and its ••• menu need is read here, while `app.album` and
    // `app.library` are borrowed — the menu takes the real track and playlist slices rather
    // than a per-frame deep clone of both. Only the outcomes cross back out to act on `app`.
    let (out, menu_action, save_like_id, in_library, menu_description) = {
        let crate::app::App {
            palette,
            images,
            backend,
            album,
            library,
            ..
        } = &mut *app;
        let p = *palette;
        let Some(a) = album.as_ref() else {
            return;
        };
        // The type line names the kind, as the reference does above the title. The year
        // belongs on the meta line below, with the rest of the facts about the release.
        let kind = if a.kind.is_empty() {
            "Album".to_owned()
        } else {
            a.kind.clone()
        };
        let kind_label = kind;
        let meta = meta_line(a);
        let like_id = a.like_id.clone();
        let in_library = a.in_library;
        let art_top = ui.cursor().top();
        let out = crate::ui::page_header(
            ui,
            &p,
            images,
            backend,
            crate::ui::PageHeader {
                art_url: a.thumbnail.as_ref(),
                art_size: PAGE_ART_SIZE,
                circle: false,
                kind_label: &kind_label,
                title: &a.title,
                description: None,
                artists: &a.artists,
                second_plain: None,
                meta: Some(&meta),
                owner: None,
                // The artist sits midway between the name and the info line: 5.6 puts the
                // same ~9 pt between the title's painted ink and the 15 pt capitals as between
                // those and the 13 pt info line's. The header measures from the ink, so a name
                // with no descenders pulls it up close and a tail keeps it clear.
                credit_air: 5.6,
                credit_pt: 15.0,
                play_size: Some(crate::ui::DETAIL_PLAY_PT),
                playing,
                // No like id, no button: the page gave no play button, and a control that
                // cannot do anything should not be drawn.
                save_in_library: signed_in
                    .then(|| like_id.is_some().then_some(in_library))
                    .flatten(),
                dots: true,
                salt: "album-artist",
            },
        );
        // The header's own bottom: the art's, or the controls' when the row dropped below.
        ui.add_space(out.art_bottom - art_top + 12.0);

        let menu_share = crate::model::album_url(&a.id);
        let menu_has_info = a
            .description
            .as_deref()
            .is_some_and(|d| !d.trim().is_empty());
        // A header-link click navigates away this frame; skip the menu then, as before.
        let menu_action = out
            .link
            .is_none()
            .then(|| {
                out.dots.as_ref().and_then(|dots| {
                    widgets::page_menu(
                        ui,
                        &p,
                        dots,
                        widgets::PageMenu {
                            page_id: &a.id,
                            tracks: &a.tracks,
                            artist_id: a.artist_id.as_deref(),
                            playlists: &library.playlists.items,
                            signed_in,
                            share_url: menu_share,
                            has_info: menu_has_info,
                            manage: false,
                            remove_from_library: false,
                            context_only: false,
                            // The album's own radio seeds from its audio playlist id.
                            radio_id: a.like_id.as_deref(),
                            save_as_new: None,
                            tracks_pending: None,
                        },
                    )
                })
            })
            .flatten();
        // Only clone the description if the menu actually asked to show it.
        let menu_description = matches!(menu_action, Some(crate::ui::RowAction::PageInfo))
            .then(|| a.description.clone())
            .flatten();
        (out, menu_action, like_id, in_library, menu_description)
    };

    if out.play {
        let tracks = app
            .album
            .as_ref()
            .map(|a| a.tracks.clone())
            .unwrap_or_default();
        // Already on this album: the button just showed Pause, so clicking it again means
        // resume/pause, not "start over from track 1".
        if app.queue_matches(&tracks) {
            app.toggle_playback();
        } else {
            app.start_from(tracks, 0);
        }
    }
    if out.save
        && let Some(id) = save_like_id
    {
        app.row_action(crate::ui::RowAction::PageLibrary {
            album: true,
            write: crate::backend::PageWrite::LikePlaylist(id),
            in_library: !in_library,
        });
    }
    if let Some(route) = out.link {
        app.navigate(route);
        return;
    }
    match menu_action {
        Some(crate::ui::RowAction::PageInfo) => {
            app.page_info = menu_description;
        }
        Some(other) => app.row_action(other),
        None => {}
    }
}
/// Songs, length and year: the type line above the title already names the kind, and
/// repeating it here would be the same fact twice.
fn meta_line(album: &Album) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{} songs", album.tracks.len()));
    if let Some(d) = &album.duration_text {
        parts.push(d.clone());
    }
    if let Some(y) = &album.year {
        parts.push(y.clone());
    }
    parts.join(" • ")
}

fn track_list(app: &mut App, ui: &mut egui::Ui) {
    let signed_in = app.signed_in();
    land_on_focus(app, ui);
    let clicked = {
        let App {
            palette,
            images,
            backend,
            stats,
            liked,
            now,
            album,
            library,
            ..
        } = app;
        let tracks = album.as_ref().map(|a| a.tracks.as_slice()).unwrap_or(&[]);
        track_rows(
            ui,
            palette,
            images,
            backend,
            stats,
            liked,
            now,
            tracks,
            true,
            None,
            &library.playlists.items,
            signed_in,
            true,
            // No cover on any row: every one of them is the header's own art.
            true,
        )
    };
    if clicked.click.is_some() || clicked.action.is_some() {
        widgets::set_focused_row(ui.ctx(), None);
    }
    match clicked.click {
        Some(ListClick::Row(i)) => {
            if let Some(tracks) = app.album.as_ref().map(|a| a.tracks.clone()) {
                app.start_from(tracks, i);
            }
        }
        Some(ListClick::Link(route)) => app.navigate(route),
        None => {}
    }
    if let Some(action) = clicked.action {
        app.row_action(action);
    }
}

/// Mark the song a card opened this album on, and bring its row into view: once, on the first
/// frame the album is on screen. Called where the list is about to start, so its top is the
/// cursor.
fn land_on_focus(app: &App, ui: &mut egui::Ui) {
    let Some(focus) = widgets::album_focus(ui.ctx()) else {
        return;
    };
    let Some(album) = app.album.as_ref().filter(|a| a.id == focus.album_id) else {
        return;
    };
    widgets::set_album_focus(ui.ctx(), None);
    // A single is the song: there is nothing on its page to point out.
    if album.kind.eq_ignore_ascii_case("single") || album.tracks.len() == 1 {
        return;
    }
    let Some(i) = focus_index(&album.tracks, &focus) else {
        return;
    };
    widgets::set_focused_row(ui.ctx(), Some(album.tracks[i].video_id.clone()));
    let row_h = widgets::row_height(false, ui.available_width());
    let pitch = crate::ui::row_pitch(row_h, ui.spacing().item_spacing.y);
    let top = ui.cursor().top() + i as f32 * pitch;
    let row = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + row_h);
    ui.scroll_to_rect(row, Some(egui::Align::Center));
}

/// The album row a card's song is: the same video, or failing that the same title, since a
/// card can carry a music video or another upload of the album's track.
fn focus_index(tracks: &[crate::model::Track], focus: &widgets::AlbumFocus) -> Option<usize> {
    tracks
        .iter()
        .position(|t| t.video_id == focus.video_id)
        .or_else(|| {
            let title = focus.title.trim().to_lowercase();
            tracks
                .iter()
                .position(|t| t.title.trim().to_lowercase() == title)
        })
}

#[cfg(test)]
mod tests {
    use super::focus_index;
    use crate::model::Track;
    use crate::widgets::AlbumFocus;

    fn track(id: &str, title: &str) -> Track {
        Track {
            video_id: id.to_owned(),
            title: title.to_owned(),
            artists: Vec::new(),
            album: None,
            album_id: None,
            duration: None,
            plays: None,
            explicit: false,
            like: crate::model::LikeStatus::Indifferent,
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            thumbnail: None,
            rank: None,
            trend: crate::model::ChartTrend::Unknown,
            wide: false,
            local_path: None,
        }
    }

    /// The card's own video is found first; a card carrying another upload of the song (a music
    /// video) still lands on the album's track by its title, and a song not on the album marks
    /// nothing.
    #[test]
    fn the_card_song_is_found_by_id_then_by_title() {
        let tracks = [track("a", "Intro"), track("b", "Nova"), track("c", "Outro")];
        let focus = |id: &str, title: &str| AlbumFocus {
            album_id: "MPREb_x".to_owned(),
            video_id: id.to_owned(),
            title: title.to_owned(),
        };
        assert_eq!(focus_index(&tracks, &focus("c", "Nova")), Some(2));
        assert_eq!(focus_index(&tracks, &focus("omv", " nova ")), Some(1));
        assert_eq!(focus_index(&tracks, &focus("omv", "Elsewhere")), None);
    }
}
