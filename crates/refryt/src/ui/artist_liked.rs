//! "Liked songs" by one artist: the songs of that artist that are in the liked index.
//!
//! The list comes from [`crate::liked::Index`] — a background page-through of Liked Music,
//! cached to disk. There is no liked-by-artist endpoint to fetch, so this page renders what the
//! index knows; `ensure_route_loaded` only has to make sure the right artist is loaded for the
//! name, which is also what Back/Forward and `--page artistliked:<id>` ride.
//!
//! Laid out as a playlist, because that is what it is: a square cover — four of the songs' own
//! covers, the way YouTube tiles a playlist's — "Liked songs" over the artist's linked name and
//! the count, then Play, Shuffle and sort, and rows whose thumbs stay out of sight until
//! hovered, as on Liked Music. Sorting needs no snapshot to get back to Default: the index
//! always holds the served order, and the rows are rebuilt from it every frame.

use crate::app::{App, PlaylistSort};
use crate::ui::{ListClick, PAGE_ART_SIZE, failed, track_rows_quiet};
use crate::widgets;
use crate::widgets::Hint;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    if app.artist_loading {
        crate::ui::skeleton_page_header(
            ui,
            &app.palette,
            crate::ui::HeaderSkeleton {
                kind: crate::ui::HeaderKind::None,
                title: Some("Liked songs"),
                description: false,
                credit_line: false,
                credit_air: 4.0,
                controls: crate::ui::HeaderControls::ShuffleAndSort,
            },
        );
        ui.add_space(10.0);
        widgets::skeleton_track_rows(ui, &app.palette, 8, false);
        return;
    }
    if let Some(err) = app.artist_error.clone() {
        failed(app, ui, err, App::retry_route);
        return;
    }
    let Some(id) = app.artist_id().map(str::to_owned) else {
        return;
    };
    let Some(name) = app.artist.as_ref().map(|a| a.name.clone()) else {
        return;
    };
    let tracks = sorted_tracks(app, &id);
    if tracks.is_empty() {
        let message = if app.liked_scanning {
            "Still reading your liked songs…".to_owned()
        } else {
            format!("No liked songs by {name}.")
        };
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(message)
                    .font(crate::theme::regular(13.0))
                    .color(app.palette.secondary),
            );
        });
        return;
    }

    let covers = covers(&tracks);
    let wash = crate::ui::header_wash_colour(app, covers.first());
    crate::ui::page_scroll(app, "artist-liked").show(ui, |ui| {
        // Tinted from the songs' own covers, as a playlist's header is from its art.
        crate::ui::header_wash(ui, &app.palette, app.content_rect, ui.cursor().min.y, wash);
        header(app, ui, &tracks, &covers);
        ui.add_space(10.0);
        track_list(app, ui, &tracks);
    });
}

/// The artist's liked songs in the page's current order. `Default` is the index's own order —
/// recently saved first — and `Reverse` flips it; the rest re-sort by title or duration.
fn sorted_tracks(app: &App, id: &str) -> Vec<crate::model::Track> {
    let mut tracks = app.liked.tracks_for_artist(id);
    match app.artist_liked_sort {
        PlaylistSort::Reverse => tracks.reverse(),
        sort => sort.apply(&mut tracks),
    }
    tracks
}

/// The covers for the header: up to four from different albums, in list order. Four make the
/// tiled cover; fewer, and the first stands alone. Told apart by album *name* with any edition
/// in brackets dropped, not by image: "Kiss Land" and "Kiss Land (Deluxe)" are two links to
/// what reads as the same cover twice.
fn covers(tracks: &[crate::model::Track]) -> Vec<std::sync::Arc<str>> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<std::sync::Arc<str>> = Vec::new();
    for t in tracks {
        let Some(url) = &t.thumbnail else {
            continue;
        };
        let key = match t.album.as_deref() {
            Some(album) => album
                .split(['(', '['])
                .next()
                .unwrap_or(album)
                .trim()
                .to_lowercase(),
            None => url.to_string(),
        };
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(std::sync::Arc::clone(url));
        if out.len() == 4 {
            break;
        }
    }
    out
}

/// "7 hours, 12 minutes" / "36 minutes" — the playlist header's own wording for a length.
fn total_length(tracks: &[crate::model::Track]) -> Option<String> {
    let secs: u64 = tracks.iter().filter_map(|t| t.duration).sum();
    if secs == 0 {
        return None;
    }
    let (hours, minutes) = (secs / 3600, (secs % 3600) / 60);
    let plural = |n: u64, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    Some(match (hours, minutes) {
        (0, m) => plural(m.max(1), "minute"),
        (h, 0) => plural(h, "hour"),
        (h, m) => format!("{}, {}", plural(h, "hour"), plural(m, "minute")),
    })
}

fn header(
    app: &mut App,
    ui: &mut egui::Ui,
    tracks: &[crate::model::Track],
    covers: &[std::sync::Arc<str>],
) {
    let playing = app.is_playing_any(tracks);
    let out = {
        let crate::app::App {
            palette,
            images,
            backend,
            artist,
            ..
        } = app;
        let p = *palette;
        let Some(a) = artist.as_ref() else {
            return;
        };
        // The artist rides the meta line as a link, the way a playlist names its owner.
        let owner = crate::model::ArtistLink {
            name: a.name.clone(),
            id: Some(a.id.clone()),
        };
        let mut meta = if tracks.len() == 1 {
            "1 song".to_owned()
        } else {
            format!("{} songs", tracks.len())
        };
        if let Some(length) = total_length(tracks) {
            meta = format!("{meta} • {length}");
        }
        let art_top = ui.cursor().top();
        let out = crate::ui::page_header(
            ui,
            &p,
            images,
            backend,
            crate::ui::PageHeader {
                art_url: covers.first(),
                art_size: PAGE_ART_SIZE,
                circle: false,
                kind_label: "",
                title: "Liked songs",
                description: None,
                artists: &[],
                second_plain: None,
                meta: Some(&meta),
                owner: Some(&owner),
                credit_air: 4.0,
                play_size: Some(crate::ui::DETAIL_PLAY_PT),
                playing,
                save_in_library: None,
                dots: false,
                salt: "artist-liked-name",
            },
        );
        if covers.len() == 4 {
            paint_tiled_cover(ui, &p, images, backend, out.art_rect, covers);
        }
        ui.add_space(out.art_bottom - art_top + 12.0);
        out
    };
    // Shuffle and sort as icons on Play's own row — the artist page's Shuffle and the playlist
    // page's sort, rather than a grey pill no other page has.
    let mut shuffled = false;
    if let Some(anchor) = out.controls_end {
        let rect =
            egui::Rect::from_min_size(anchor + egui::vec2(-8.0, -16.0), egui::vec2(300.0, 32.0));
        let mut cui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let p = app.palette;
        if widgets::icon_button(&mut cui, &p, crate::icons::Icon::Shuffle, 20.0, false)
            .hint("Shuffle")
            .clicked()
        {
            shuffled = true;
        }
        let labels: Vec<&'static str> = PlaylistSort::ALL.iter().map(|s| s.label()).collect();
        if let Some(i) = widgets::icon_dropdown(
            &mut cui,
            &p,
            ui.id().with("artist-liked-sort-dropdown"),
            crate::icons::Icon::ArrowUpDown,
            false,
            app.artist_liked_sort.label(),
            &labels,
        ) {
            app.artist_liked_sort = PlaylistSort::ALL[i];
        }
    }
    if out.play {
        // Already on this list: the button showed Pause, so this is resume/pause, not a restart.
        if app.queue_matches(tracks) {
            app.toggle_playback();
        } else {
            app.start_from(tracks.to_vec(), 0);
        }
    }
    if shuffled {
        app.shuffle_play(tracks.to_vec());
    }
    if let Some(route) = out.link {
        app.navigate(route);
    }
}

/// Four covers in a 2×2 tile over the header's art, rounded only at the outer corners so the
/// four read as one cover. A cover still loading leaves its quarter on the placeholder.
fn paint_tiled_cover(
    ui: &egui::Ui,
    palette: &crate::theme::Palette,
    images: &mut crate::images::Images,
    backend: &crate::backend::Backend,
    rect: egui::Rect,
    covers: &[std::sync::Arc<str>],
) {
    let r = widgets::ArtShape::Square.radius(rect).nw;
    let half = rect.size() / 2.0;
    ui.painter().rect_filled(
        rect,
        widgets::ArtShape::Square.radius(rect),
        palette.glass(palette.surface),
    );
    for (i, url) in covers.iter().take(4).enumerate() {
        let quarter = egui::Rect::from_min_size(
            rect.min + egui::vec2((i % 2) as f32 * half.x, (i / 2) as f32 * half.y),
            half,
        );
        let mut corner = egui::CornerRadius::ZERO;
        match i {
            0 => corner.nw = r,
            1 => corner.ne = r,
            2 => corner.sw = r,
            _ => corner.se = r,
        }
        if let Some(tex) = crate::ui::art_texture(images, backend, ui.ctx(), Some(url), half.x) {
            egui::Image::new(tex)
                .corner_radius(corner)
                .paint_at(ui, quarter);
        }
    }
}

fn track_list(app: &mut App, ui: &mut egui::Ui, tracks: &[crate::model::Track]) {
    let signed_in = app.signed_in();
    let clicked = {
        let App {
            palette,
            images,
            backend,
            stats,
            liked,
            now,
            library,
            ..
        } = app;
        track_rows_quiet(
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
            false,
            // Songs from across the artist's albums, so each row keeps its own cover.
            false,
            // Every row here is liked; the thumb shows under the pointer, as on Liked Music.
            true,
        )
    };
    match clicked.click {
        Some(ListClick::Row(i)) => app.start_from(tracks.to_vec(), i),
        Some(ListClick::Link(route)) => app.navigate(route),
        None => {}
    }
    if let Some(action) = clicked.action {
        app.row_action(action);
    }
}
