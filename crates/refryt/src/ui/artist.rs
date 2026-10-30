//! The artist page: full-bleed hero, control row, then Popular with the pages' own shelves.
//!
//! The header is the reference's shape built from YouTube's own data: the immersive header's
//! wide thumbnail (the channel art, ~2.4:1) is painted across the card with the name and the
//! monthly audience over a bottom scrim; a page whose art is only a square portrait — a channel's
//! visual header — keeps the circle-art fallback. Below it, Play leads a row of the app's own
//! controls (Mix, Shuffle, Subscribe, •••) and "Popular" numbers its rows beside the "You Liked"
//! tile, which drops under the list when the card is too narrow for both.
//!
//! The shelves are kept as YouTube sends them — Albums, Singles & EPs, Videos, Live performances,
//! Featured on, Playlists, Fans might also like — rather than flattened into one grid. That
//! flattening is what made albums and singles indistinguishable and threw the other five away.

use std::sync::Arc;

use egui::{pos2, vec2};

use crate::app::App;
use crate::ui::{HEADER_ART_PT, ListClick, track_shelf_pages};
use crate::widgets;
use crate::widgets::Hint;

/// The hero's height floor and ceiling. Below the floor the name would crowd the art; above the
/// ceiling a maximized window would spend most of the fold on a picture.
const HERO_MIN_H: f32 = 220.0;
const HERO_MAX_H: f32 = 440.0;
/// "Popular" and "You Liked" sit side by side from this content width up; below it the tile
/// drops under the list. The floor is the list's own 1-column minimum plus the tile's width.
const SIDE_COL_MIN: f32 = 740.0;
/// The "You Liked" tile's column.
const SIDE_COL_W: f32 = 300.0;
/// The You Liked tile's round cover, for its placeholder.
const LIKED_ART: f32 = 96.0;
/// How many segments approximate the hero's top corners. The same count the header wash uses.
const HERO_ARC_STEPS: usize = 10;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    if app.artist_loading {
        if app.loading_a_profile() {
            profile_skeleton(app, ui);
        } else {
            skeleton(app, ui);
        }
        return;
    }
    if let Some(err) = app.artist_error.clone() {
        crate::ui::failed(app, ui, err, App::retry_route);
        return;
    }
    if app.artist.is_none() {
        return;
    }

    crate::ui::page_scroll(app, "artist").show(ui, |ui| {
        header(app, ui);
        crate::ui::info_popup(app, ui);
        if app.artist.as_ref().is_some_and(|a| !a.top_songs.is_empty()) {
            popular_and_liked(app, ui);
        } else {
            you_liked(app, ui);
        }
        shelves(app, ui);
    });
}

/// The hero when the page carries wide channel art, the circle header otherwise, then the
/// controls on their own row below either.
fn header(app: &mut App, ui: &mut egui::Ui) {
    let has_hero = app.artist.as_ref().is_some_and(|a| a.hero.is_some());
    if has_hero {
        hero_header(app, ui);
    } else {
        circle_header(app, ui);
    }
    action_row(app, ui);
}

/// The page's placeholder, in the page's own shape: the hero band across the card with the same
/// scrim and the name where the name goes, the control row under it, Top songs' numbered columns
/// beside a You Liked tile where the page will have one, then a shelf of cards. Every piece
/// takes the real one's size and spacing, so nothing moves when the page lands.
fn skeleton(app: &App, ui: &mut egui::Ui) {
    let p = app.palette;
    let round = |d: f32| egui::CornerRadius::same((d / 2.0) as u8);
    let small = egui::CornerRadius::same(crate::theme::radii().small);
    let inset_x = crate::theme::PAGE_INSET_X;
    let top = ui.cursor().min;
    let width = ui.available_width() + inset_x * 2.0;
    let height = (width * crate::images::HERO_ASPECT).clamp(HERO_MIN_H, HERO_MAX_H);
    let hero = egui::Rect::from_min_size(pos2(top.x - inset_x, top.y), vec2(width, height));
    let card = egui::Rect::from_min_max(
        pos2(app.content_rect.min.x - inset_x, app.content_rect.min.y),
        pos2(app.content_rect.max.x + inset_x, app.content_rect.max.y),
    );
    let r = crate::theme::radii().panel;
    widgets::skeleton_fill_in(
        ui,
        hero,
        egui::CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        },
        &p,
        card,
    );
    // The real hero's scrim over it: a flat grey band read as a bigger block than the same band
    // of picture, whose lower half the scrim darkens into the card.
    if widgets::skeleton_visible(ui) {
        let scrim_top = hero_scrim_top(hero);
        let mut scrim = egui::Mesh::default();
        for y in [scrim_top, hero.bottom()] {
            let colour = egui::Color32::from_black_alpha(hero_scrim_alpha(hero, scrim_top, y));
            scrim.colored_vertex(pos2(hero.left(), y), colour);
            scrim.colored_vertex(pos2(hero.right(), y), colour);
        }
        scrim.add_triangle(0, 2, 1);
        scrim.add_triangle(1, 2, 3);
        ui.painter().with_clip_rect(card).add(scrim);
        // The name and the audience, on `hero_header`'s own lines.
        let (name_y, meta_y, name_size) = hero_text_lines(hero, width);
        let cap = name_size * 0.727;
        widgets::skeleton_fill_in(
            ui,
            egui::Rect::from_min_size(
                pos2(top.x, name_y - cap / 2.0 + 1.0),
                vec2((width * 0.25).clamp(160.0, 420.0), cap),
            ),
            small,
            &p,
            card,
        );
        widgets::skeleton_fill_in(
            ui,
            egui::Rect::from_min_size(pos2(top.x + 2.0, meta_y - 5.0), vec2(140.0, 10.0)),
            small,
            &p,
            card,
        );
    }
    ui.add_space(height);

    // The controls: Play, which the page has whenever it has songs. Mix, Shuffle, Subscribe and
    // ••• depend on the artist and the account, so they arrive beside it instead of standing in
    // as shapes that may not become anything.
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), egui::Sense::hover());
        widgets::skeleton_play_button(ui, &p, rect);
    });

    // Top songs, beside You Liked when the page will show it — known before the page is, from
    // the liked index — and the card is wide enough for both.
    let liked = app
        .artist_id()
        .map(|id| app.liked.count_for_artist(id))
        .unwrap_or(0);
    let top_songs = |ui: &mut egui::Ui| {
        // `track_shelf_pages`: a heading, then numbered rows down its columns.
        ui.add_space(20.0);
        widgets::skeleton_heading_text(ui, &p, "Top songs");
        ui.add_space(4.0);
        let cols = crate::ui::top_song_columns(ui.available_width());
        widgets::skeleton_song_grid(ui, &p, 5, cols, true);
    };
    if liked > 0 && ui.available_width() >= SIDE_COL_MIN {
        const GAP: f32 = 28.0;
        let spacing = ui.spacing().item_spacing.x;
        let left_w = (ui.available_width() - SIDE_COL_W - GAP).max(0.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(left_w, 0.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_min_width(left_w);
                    ui.set_max_width(left_w);
                    top_songs(ui);
                },
            );
            ui.add_space((GAP - spacing).max(0.0));
            ui.allocate_ui_with_layout(
                vec2(SIDE_COL_W, 0.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_min_width(SIDE_COL_W);
                    // `you_liked`: its heading on Top songs' line, then the round cover with
                    // the count and the credit beside it.
                    ui.add_space(20.0);
                    widgets::skeleton_heading_text(ui, &p, "You Liked");
                    ui.add_space(8.0);
                    let (tile, _) =
                        ui.allocate_exact_size(vec2(SIDE_COL_W, LIKED_ART), egui::Sense::hover());
                    let art = egui::Rect::from_min_size(tile.min, vec2(LIKED_ART, LIKED_ART));
                    widgets::skeleton_fill(ui, art, round(LIKED_ART), &p);
                    for (dy, w, h) in [(-12.0, 80.0, 11.0), (6.0, 90.0, 9.0)] {
                        widgets::skeleton_fill(
                            ui,
                            egui::Rect::from_min_size(
                                pos2(art.right() + 16.0, art.center().y + dy),
                                vec2(w, h),
                            ),
                            small,
                            &p,
                        );
                    }
                },
            );
        });
    } else {
        top_songs(ui);
    }
    // The first of the shelves (Albums, as a rule), as `artist_shelf` draws it.
    ui.add_space(20.0);
    widgets::skeleton_heading(ui, &p, false);
    ui.add_space(4.0);
    widgets::skeleton_grid(ui, &p, 1, widgets::ArtShape::Square);
}

/// The placeholder for a person's channel: the circle header, the control row and, on someone
/// else's page, the first shelf. It is [`circle_header`] and [`action_row`] laid out with grey
/// shapes, so nothing moves when the page lands. Your own page has no Subscribe and, as far as
/// the account list can tell, nothing to put under the row; another person's has Subscribe and
/// its shelves (Videos, Playlists), of which the first stands in. Their banner, when they have
/// one, is the one thing this cannot know and still arrives as a surprise.
fn profile_skeleton(app: &App, ui: &mut egui::Ui) {
    use crate::ui::{CREDIT_AIR, CREDIT_CAP_HALF, DETAIL_TITLE_MAX, HEADER_ART_FLOOR_PT};
    let p = app.palette;
    let small = egui::CornerRadius::same(crate::theme::radii().small);
    let own = app.artist_id().is_some() && app.artist_id() == app.own_channel_id();

    // `page_header`'s art, circle, and the text block on its bottom edge.
    let top = ui.cursor().top();
    let available = ui.available_width();
    let art_size = HEADER_ART_PT.min((available - 20.0 - 8.0 - 140.0).max(HEADER_ART_FLOOR_PT));
    let art = egui::Rect::from_min_size(
        ui.cursor().min + vec2(0.0, crate::theme::HEADER_TOP_PAD),
        vec2(art_size, art_size),
    );
    widgets::skeleton_fill(
        ui,
        art,
        egui::CornerRadius::same((art_size / 2.0) as u8),
        &p,
    );
    let tx = art.right() + 20.0;
    let text_w = (available - art_size - 20.0 - 8.0).max(0.0);
    let title_font = crate::theme::bold(DETAIL_TITLE_MAX);
    let title_rel = DETAIL_TITLE_MAX * 0.5;
    let title_ink = widgets::ink_drop(ui, "Title", &title_font);
    // One line under the title, the audience: `circle_header` passes it as `second_plain`.
    let audience = title_rel + title_ink + CREDIT_AIR + CREDIT_CAP_HALF + 4.0;
    let text_top = art.top() + (art_size - (audience + 7.0)).max(0.0);
    let baseline = text_top + title_rel + title_ink;
    let cap = DETAIL_TITLE_MAX * 0.727;
    widgets::skeleton_fill(
        ui,
        egui::Rect::from_min_max(
            pos2(tx, baseline - cap),
            pos2(tx + (text_w * 0.3).min(320.0), baseline),
        ),
        small,
        &p,
    );
    widgets::skeleton_fill(
        ui,
        egui::Rect::from_min_size(
            pos2(tx, text_top + audience - 4.5),
            vec2((text_w * 0.36).clamp(60.0_f32.min(text_w), 380.0), 9.0),
        ),
        small,
        &p,
    );
    ui.add_space(art.bottom() - top + 4.0);

    // The row: Subscribe, off your own page, then •••, which both have. The same calls as
    // `action_row`, so the spacing is its own.
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        if !own {
            let font = crate::theme::semibold(13.0);
            let label_w = ui.fonts_mut(|f| {
                f.layout_no_wrap("Subscribe".to_owned(), font, egui::Color32::PLACEHOLDER)
                    .size()
                    .x
            });
            let (rect, _) =
                ui.allocate_exact_size(vec2(label_w + 28.0, 32.0), egui::Sense::hover());
            widgets::skeleton_fill(ui, rect, crate::theme::pill(rect), &p);
            ui.add_space(6.0);
        }
        // `icon_button(.., 16.0, ..)`: a 30 pt box round a 16 pt glyph, drawn once the
        // placeholders are.
        let (rect, _) = ui.allocate_exact_size(vec2(30.0, 30.0), egui::Sense::hover());
        if widgets::skeleton_visible(ui) {
            crate::icons::Icon::Ellipsis.image(p.dim, 16.0).paint_at(
                ui,
                egui::Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            );
        }
    });

    if !own {
        // The first shelf, as `artist_shelf` draws it.
        ui.add_space(20.0);
        widgets::skeleton_heading(ui, &p, false);
        ui.add_space(4.0);
        widgets::skeleton_grid(ui, &p, 1, widgets::ArtShape::Square);
    }
}

/// Where the hero's scrim starts: its gradient covers the lower 60% of the band, and never less
/// than 96 pt, so the name has something dark under it on a short band too.
fn hero_scrim_top(rect: egui::Rect) -> f32 {
    rect.bottom() - (rect.height() * 0.6).max(96.0)
}

/// The scrim's darkness at `y`: clear at its top, near-black at the band's bottom.
fn hero_scrim_alpha(rect: egui::Rect, scrim_top: f32, y: f32) -> u8 {
    let t = ((y - scrim_top) / (rect.bottom() - scrim_top)).clamp(0.0, 1.0);
    (t * 205.0).round() as u8
}

/// The name's and the audience's line centres on a hero `rect` of card width `width`, and the
/// name's size — bottom-left, up off the band's bottom edge, by eye.
fn hero_text_lines(rect: egui::Rect, width: f32) -> (f32, f32, f32) {
    let name_size = (width * 0.058).clamp(32.0, 62.0);
    let meta_y = rect.bottom() - 25.0;
    (meta_y - name_size * 0.85, meta_y, name_size)
}

/// The reference's hero: the banner painted across the card's own width, from its rounded top
/// edge down, with the name and audience over a bottom scrim. A painter clipped to the card is
/// what lets the image reach into the card's top margin — inside the scroll area the ui's own
/// clip starts below it (the same reason [`crate::ui::header_wash`] builds its painter by hand).
fn hero_header(app: &mut App, ui: &mut egui::Ui) {
    let content_top = ui.cursor().min.y;
    let content_left = ui.cursor().min.x;
    let inset_x = crate::theme::PAGE_INSET_X;
    // The content column is inset from the card's sides; the hero spans the card itself. There
    // is no top margin to add: the card's top edge is the content's.
    let width = ui.available_width() + inset_x * 2.0;
    let height = (width * crate::images::HERO_ASPECT).clamp(HERO_MIN_H, HERO_MAX_H);
    let rect = egui::Rect::from_min_size(
        pos2(content_left - inset_x, content_top),
        vec2(width, height),
    );

    let (name, audience, hero) = {
        let Some(a) = app.artist.as_ref() else {
            return;
        };
        (
            a.name.clone(),
            // The monthly audience alone, as the site shows it. A small artist has none — then
            // the subscriber line takes its place, complete with its word ("965 subscribers"),
            // never the bare button count.
            a.monthly_listeners
                .clone()
                .or_else(|| a.subscribers_long.clone())
                .unwrap_or_else(|| "Artist".to_owned()),
            a.hero.clone(),
        )
    };
    let Some(hero) = hero else {
        return;
    };

    let tex =
        crate::ui::art_texture_hero(&mut app.images, &app.backend, ui.ctx(), Some(&hero), width);
    // The card the hero belongs to, and the slice of the hero still inside it: the hero's own
    // rect travels up with the scrolled content, so halfway out it would reach above the card and
    // paint over the top bar — same layer, and the central panel draws last. Everything is drawn
    // against `slice`, rounding included: the rounded top edge has to be the *card's*, not the
    // banner's, or the frames where the banner is half gone cut it square.
    let card = egui::Rect::from_min_max(
        pos2(app.content_rect.min.x - inset_x, app.content_rect.min.y),
        pos2(app.content_rect.max.x + inset_x, app.content_rect.max.y),
    );
    let slice = rect.intersect(card);
    let painter = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), card);
    if slice.width() > 0.0 && slice.height() > 0.0 {
        let rows = hero_rows(rect, slice, slice.top());
        match tex {
            Some(tex) => {
                // Cover-crop: the drawn band is usually shorter than the fetched 26% (and taller
                // than it at the floor), so `uv` picks the centre slice — nothing is squashed, and
                // the 16:9-style sideways letterboxing `paint_art` avoids never happens.
                let ts = tex.size_vec2();
                let scale = (rect.width() / ts.x).max(rect.height() / ts.y);
                let band = egui::Rect::from_min_size(
                    pos2(
                        (1.0 - rect.width() / (ts.x * scale)) * 0.5,
                        (1.0 - rect.height() / (ts.y * scale)) * 0.5,
                    ),
                    vec2(
                        rect.width() / (ts.x * scale),
                        rect.height() / (ts.y * scale),
                    ),
                );
                // The slice keeps the band's own mapping: same texels, cut at the card's edge.
                let map = |p: egui::Pos2| {
                    pos2(
                        band.min.x + (p.x - rect.min.x) / rect.width() * band.width(),
                        band.min.y + (p.y - rect.min.y) / rect.height() * band.height(),
                    )
                };
                let mut image = egui::Mesh::with_texture(tex.id());
                add_rows(&mut image, &rows, &mut |p| (map(p), egui::Color32::WHITE));
                painter.add(image);
            }
            None => {
                let mut fill = egui::Mesh::default();
                add_rows(&mut fill, &rows, &mut |_| {
                    (
                        egui::epaint::WHITE_UV,
                        app.palette.glass(app.palette.surface),
                    )
                });
                painter.add(fill);
            }
        }
        // The scrim: transparent over the top of the banner, near-black at the bottom, so the
        // name is legible over any art — light channel art included — without per-image colour
        // work. Its rows follow the same edges, starting wherever its gradient does.
        let scrim_top = hero_scrim_top(rect);
        let alpha_at = |y: f32| hero_scrim_alpha(rect, scrim_top, y);
        let mut scrim = egui::Mesh::default();
        add_rows(&mut scrim, &hero_rows(rect, slice, scrim_top), &mut |p| {
            (
                egui::epaint::WHITE_UV,
                egui::Color32::from_black_alpha(alpha_at(p.y)),
            )
        });
        painter.add(scrim);
    }

    // The name and the audience, bottom-left, on the content's own left edge so they line up
    // with every other page's header. White, because the scrim is dark whatever the theme is.
    let (name_y, meta_y, name_size) = hero_text_lines(rect, width);
    let name_font = crate::theme::bold(name_size);
    let meta_font = crate::theme::medium(14.0);
    let text_w = (width - inset_x - 24.0).max(0.0);
    crate::emoji::paint_line(
        ui,
        pos2(content_left, name_y),
        &widgets::truncate_to(ui, &name, name_font.clone(), text_w),
        name_font,
        egui::Color32::WHITE,
    );
    ui.painter().text(
        pos2(content_left + 2.0, meta_y),
        egui::Align2::LEFT_CENTER,
        widgets::truncate_to(ui, &audience, meta_font.clone(), text_w),
        meta_font,
        egui::Color32::from_white_alpha(210),
    );

    // Reserve the hero on the content; the name row lives inside it.
    ui.add_space(height);
}

/// Append a horizontally-striped quad band to `mesh`: one vertex pair per `(y, left, right)`
/// row, filled between consecutive rows, each vertex's texture coordinate and colour decided by
/// `vertex`. The hero's image, its surface fallback and the scrim are all bands like this, so
/// they can share one edge.
fn add_rows(
    mesh: &mut egui::Mesh,
    rows: &[(f32, f32, f32)],
    vertex: &mut dyn FnMut(egui::Pos2) -> (egui::Pos2, egui::Color32),
) {
    // One row is a line, not a band: it would add vertices and no triangles, and the wgpu
    // renderer panics on a mesh like that ("buffer slice can not be empty") — it binds the
    // index buffer slice of every mesh that has vertices. `hero_rows` hands back a single row
    // for the last half point of a banner scrolling out of the card, where there is nothing to
    // see anyway.
    if rows.len() < 2 {
        return;
    }
    let base = mesh.vertices.len() as u32;
    for &(y, left, right) in rows {
        for x in [left, right] {
            let (uv, color) = vertex(pos2(x, y));
            mesh.vertices.push(egui::epaint::Vertex {
                pos: pos2(x, y),
                uv,
                color,
            });
        }
    }
    for row in 0..rows.len().saturating_sub(1) {
        let a = base + (row * 2) as u32;
        mesh.add_triangle(a, a + 2, a + 1);
        mesh.add_triangle(a + 1, a + 2, a + 3);
    }
}

/// The horizontal rows of the hero's visible `slice`: `(y, left, right)` per row, from `start`
/// (the scrim starts lower than the image) down to the slice's bottom edge.
///
/// The slice's left/right edges follow the *card's* top corner arc, kept at its full radius
/// however short the slice is: `epaint` clamps a rounded `RectShape`'s radius to half a short
/// rect (`clamp_corner_radius`), which visibly un-rounded the corner as the last sliver of
/// banner scrolled out. Image and scrim build their meshes from the same rows, so their edges
/// can never disagree.
fn hero_rows(rect: egui::Rect, slice: egui::Rect, start: f32) -> Vec<(f32, f32, f32)> {
    let rad = crate::theme::radii().panel as f32;
    let inset_at = |y: f32| {
        let dy = (slice.top() + rad - y).clamp(0.0, rad);
        rad - (rad * rad - dy * dy).max(0.0).sqrt()
    };
    let arc_end = (slice.top() + rad).min(slice.bottom());
    let start = start.max(slice.top());
    let mut rows = Vec::new();
    if start < arc_end - 0.5 {
        for i in 0..=HERO_ARC_STEPS {
            let y = start + (arc_end - start) * i as f32 / HERO_ARC_STEPS as f32;
            let inset = inset_at(y);
            rows.push((y, rect.left() + inset, rect.right() - inset));
        }
    } else {
        let inset = inset_at(start);
        rows.push((start, rect.left() + inset, rect.right() - inset));
    }
    if rows.last().is_none_or(|row| row.0 < slice.bottom() - 0.5) {
        rows.push((slice.bottom(), rect.left(), rect.right()));
    }
    rows
}

/// The fallback for a page with no wide art (a channel's visual header): the square portrait in
/// a circle, name and audience beside it — what every artist page looked like before the hero.
fn circle_header(app: &mut App, ui: &mut egui::Ui) {
    let (art_top, art_bottom) = {
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
        let audience = a
            .monthly_listeners
            .clone()
            .or_else(|| a.subscribers_long.clone())
            .unwrap_or_else(|| "Artist".to_owned());
        let art_top = ui.cursor().top();
        let out = crate::ui::page_header(
            ui,
            &p,
            images,
            backend,
            crate::ui::PageHeader {
                art_url: a.thumbnail.as_ref(),
                art_size: HEADER_ART_PT,
                circle: true,
                // No type line on an artist: the name is the page.
                kind_label: "",
                title: &a.name,
                description: None,
                artists: &[],
                second_plain: Some(&audience),
                meta: None,
                owner: None,
                credit_air: 4.0,
                credit_pt: 13.0,
                // Play, Mix, Shuffle, Subscribe and ••• are this page's own row below, not
                // `page_header`'s Play/Save/•••.
                play_size: None,
                playing: false,
                save_in_library: None,
                dots: false,
                salt: "artist-name",
            },
        );
        (art_top, out.art_bottom)
    };
    ui.add_space(art_bottom - art_top + 4.0);
}

/// The controls below the header, led by the big Play: Mix and Shuffle as round icon buttons
/// (from the header's own targets), Subscribe as an outlined pill carrying its count, and the
/// page's ••• menu — Share and, when there is a bio, Info.
fn action_row(app: &mut App, ui: &mut egui::Ui) {
    let playing = app
        .artist
        .as_ref()
        .is_some_and(|a| app.is_playing_any(&a.top_songs));
    let signed_in = app.signed_in();
    // `Artist::id`, not `App::artist_id()`: the subscribe endpoint targets the channel the
    // response's own subscribe button names, which can differ from the page's browse id (see
    // `App::artist_id`'s doc). Mix/Shuffle seeds have always travelled this way too.
    let id = app
        .artist
        .as_ref()
        .map(|a| a.id.clone())
        .unwrap_or_default();
    let mut play_clicked = false;
    let mut play_target = None;
    let mut subscribe_to = None;
    let (menu_action, menu_description) = {
        let crate::app::App {
            palette,
            artist,
            library,
            accounts,
            ..
        } = app;
        let p = *palette;
        let Some(a) = artist.as_ref() else {
            return;
        };
        ui.add_space(14.0);
        let mut dots = None;
        ui.horizontal(|ui| {
            // No songs, no Play: a channel page can carry a banner and a shelf of videos but no
            // top-songs shelf at all, and a leading button with nothing behind it is a dead end.
            if !a.top_songs.is_empty() {
                if widgets::play_button(ui, &p, playing, 56.0).clicked() {
                    play_clicked = true;
                }
                ui.add_space(6.0);
            }
            // Mix leads the pair: when both are on offer, starting a mix is the one worth
            // reaching for first.
            if let Some(target) = &a.actions.mix
                && widgets::icon_button(ui, &p, crate::icons::Icon::Radio, 20.0, false)
                    .hint("Mix")
                    .clicked()
            {
                play_target = Some(target.clone());
            }
            if let Some(target) = &a.actions.shuffle
                && widgets::icon_button(ui, &p, crate::icons::Icon::Shuffle, 20.0, false)
                    .hint("Shuffle")
                    .clicked()
            {
                play_target = Some(target.clone());
            }
            ui.add_space(6.0);
            // Your own channel has no subscription to toggle — YouTube refuses the write with a
            // 400 either way, and its own page still marks the channel "Subscribed". The button
            // is hidden rather than offered and then reverted.
            let own_channel = accounts
                .iter()
                .find(|account| account.selected)
                .and_then(|account| account.channel_id.as_deref());
            if let Some(subscribed) = a
                .actions
                .subscribed
                .filter(|_| own_channel != Some(a.id.as_str()))
            {
                // The count rides the button only while the audience line above shows something
                // else (a monthly listener total). When the line *is* the subscriber count,
                // repeating it here read as a bug: "965 subscribers" over "Subscribe · 965".
                let label = match (
                    subscribed,
                    a.monthly_listeners.is_some(),
                    a.subscribers.as_deref(),
                ) {
                    (true, _, _) => "Subscribed".to_owned(),
                    (false, true, Some(count)) => format!("Subscribe · {count}"),
                    _ => "Subscribe".to_owned(),
                };
                if widgets::outline_button(ui, &p, &label).clicked() {
                    // The token the button carries is per direction, so the one taken is the
                    // one the click will actually use.
                    subscribe_to = Some((
                        !subscribed,
                        if subscribed {
                            a.actions.unsubscribe_params.clone()
                        } else {
                            a.actions.subscribe_params.clone()
                        },
                    ));
                }
                ui.add_space(6.0);
            }
            // Trails the row, same as the reference's Play/Follow/•••. Just Share and — when
            // there's a bio — Info live behind it: `tracks: &[]`/`artist_id: None` make the
            // menu's own Shuffle/Mix/queueing/"Go to artist" items self-hide, since this page
            // already has its own buttons and linking to itself is meaningless.
            dots = Some(widgets::icon_button(
                ui,
                &p,
                crate::icons::Icon::Ellipsis,
                16.0,
                false,
            ));
        });
        // The horizontal above always draws it; the Option exists only to smuggle the response
        // out of the closure's borrow.
        let dots = dots.expect("the control row always draws its •••");
        let has_info = a
            .description
            .as_deref()
            .is_some_and(|d| !d.trim().is_empty());
        let action = widgets::page_menu(
            ui,
            &p,
            &dots,
            widgets::PageMenu {
                page_id: &a.id,
                tracks: &[],
                artist_id: None,
                playlists: &library.playlists.items,
                signed_in,
                share_url: crate::model::artist_url(&a.id),
                has_info,
                manage: false,
                remove_from_library: false,
                context_only: false,
                // The page has its own Mix button; a menu mix would be a second one.
                radio_id: None,
                save_as_new: None,
                tracks_pending: None,
            },
        );
        let description =
            matches!(&action, Some(crate::ui::RowAction::PageInfo)).then(|| a.description.clone());
        (action, description.flatten())
    };

    if play_clicked {
        // Already on this list: the button showed Pause, so this is resume/pause, not a restart.
        let tracks = app
            .artist
            .as_ref()
            .map(|a| a.top_songs.clone())
            .unwrap_or_default();
        if app.queue_matches(&tracks) {
            app.toggle_playback();
        } else {
            app.start_from(tracks, 0);
        }
    }
    if let Some(target) = play_target {
        app.play_target(id.clone(), target);
    }
    if let Some((subscribe, params)) = subscribe_to {
        app.toggle_subscribed(&id, subscribe, params);
    }
    match menu_action {
        Some(crate::ui::RowAction::PageInfo) => app.page_info = menu_description,
        Some(other) => app.row_action(other),
        None => {}
    }
}

/// "Popular" and the "You Liked" tile: side by side while the card can hold both, stacked
/// otherwise. With no liked songs for the artist the tile is empty, so the list takes the row.
fn popular_and_liked(app: &mut App, ui: &mut egui::Ui) {
    const GAP: f32 = 28.0;
    let liked = app
        .artist_id()
        .map(|id| app.liked.count_for_artist(id))
        .unwrap_or(0);
    if liked > 0 && ui.available_width() >= SIDE_COL_MIN {
        // The row's own item spacing lands between the gap and the second column — `add_space`
        // takes none itself — and a card's frame is painted around the content's *used* width:
        // with the whole gap allocated on top of that spacing the row overran the content by it,
        // and the frame grew over the window's right gutter (seen signed in, where the tile
        // exists). Only the gap's remainder is explicit, so the two columns plus the visual gap
        // sum to exactly the content width.
        let spacing = ui.spacing().item_spacing.x;
        let left_w = (ui.available_width() - SIDE_COL_W - GAP).max(0.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(left_w, 0.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    // `allocate_ui_with_layout` advances the row by what the child *used*, not
                    // by the width it was offered — and a paged list paints its rows into rects
                    // without allocating them, so its used width can be as little as the
                    // heading's. Without this the tile lands a heading-width away, on top of the
                    // first songs (seen at full screen, where 15 rows fit one page and the
                    // heading is a bare label).
                    ui.set_min_width(left_w);
                    top_songs(app, ui);
                },
            );
            ui.add_space((GAP - spacing).max(0.0));
            ui.allocate_ui_with_layout(
                vec2(SIDE_COL_W, 0.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_min_width(SIDE_COL_W);
                    you_liked(app, ui);
                },
            );
        });
    } else {
        top_songs(app, ui);
        you_liked(app, ui);
    }
}

fn top_songs(app: &mut App, ui: &mut egui::Ui) {
    let signed_in = app.signed_in();
    let clicked = {
        let App {
            palette,
            images,
            backend,
            stats,
            liked,
            now,
            artist,
            library,
            ..
        } = app;
        let tracks = artist
            .as_ref()
            .map(|a| a.top_songs.as_slice())
            .unwrap_or(&[]);
        let artist_id = artist.as_ref().map(|a| a.id.as_str()).unwrap_or_default();
        // YouTube's own heading — "Top songs" on every capture so far — with "Popular" only
        // where the response had none (the typed fallback).
        let heading = artist
            .as_ref()
            .and_then(|a| a.top_songs_title.as_deref())
            .unwrap_or("Popular");
        track_shelf_pages(
            ui,
            palette,
            images,
            backend,
            stats,
            liked,
            heading,
            true,
            now,
            tracks,
            &library.playlists.items,
            signed_in,
            artist_id,
        )
    };
    match clicked.click {
        Some(ListClick::Row(i)) => {
            if let Some(songs) = app.artist.as_ref().map(|a| a.top_songs.clone()) {
                app.start_from(songs, i);
            }
        }
        Some(ListClick::Link(route)) => app.navigate(route),
        None => {}
    }
    if let Some(action) = clicked.action {
        app.row_action(action);
    }
}

/// The "You Liked" tile: the songs of this artist in the liked index, drawn Spotify-style — the
/// artist's portrait with a thumbs-up badge, how many songs and whose they are — beside Popular
/// (or under it on a narrow card). The whole tile opens `Route::ArtistLiked`, the full list;
/// hidden while the index knows none for this artist (including before the first scan reaches
/// them).
fn you_liked(app: &mut App, ui: &mut egui::Ui) {
    // The page id, not `Artist::id` — see `App::artist_id`'s own doc: the subscribe button's id
    // can name a different channel, and the liked route must reload the page we are looking at.
    let Some(id) = app.artist_id().map(str::to_owned) else {
        return;
    };
    let count = app.liked.count_for_artist(&id);
    let Some((name, thumbnail)) = app
        .artist
        .as_ref()
        .map(|a| (a.name.clone(), a.thumbnail.clone()))
    else {
        return;
    };
    if count == 0 {
        return;
    }

    /// The portrait's diameter. Small on purpose: this is a tile, not the page header.
    const ART: f32 = 96.0;
    let p = app.palette;
    let title_font = crate::theme::semibold(15.0);
    let by_font = crate::theme::regular(12.0);

    // The same 20pt the shelf headings above use, so "You Liked" sits on Popular's baseline.
    ui.add_space(20.0);
    ui.label(
        egui::RichText::new("You Liked")
            .font(crate::theme::bold(crate::theme::SECTION_PT))
            .color(p.text),
    );
    ui.add_space(8.0);

    let count_text = if count == 1 {
        "1 song".to_owned()
    } else {
        format!("{count} songs")
    };
    let by_text = format!("By {name}");
    let text_w = widgets::text_width(ui, &count_text, &title_font)
        .max(widgets::text_width(ui, &by_text, &by_font))
        .min((ui.available_width() - ART - 16.0).max(0.0));
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2((ART + 16.0 + text_w).min(ui.available_width()), ART),
        egui::Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        // A soft pad behind the whole tile on hover, so it reads as one clickable thing.
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.painter().rect_filled(
                rect.expand2(egui::vec2(8.0, 6.0)),
                egui::CornerRadius::same(crate::theme::radii().control),
                p.glass(p.surface),
            );
        }
        let art_rect = egui::Rect::from_min_size(rect.min, egui::vec2(ART, ART));
        let art = crate::ui::art_texture(
            &mut app.images,
            &app.backend,
            ui.ctx(),
            thumbnail.as_ref(),
            ART,
        );
        widgets::paint_art(ui, &p, art_rect, art, widgets::ArtShape::Circle);
        // The thumbs-up badge, bottom-left of the portrait, on a disc of the page's own colour
        // so it stays legible over whatever the cover art is. The filled thumb is the app's own
        // liked glyph — the same one a rated row shows.
        let badge = egui::pos2(art_rect.left() + 18.0, art_rect.bottom() - 16.0);
        ui.painter().circle_filled(badge, 13.0, p.window_fill());
        crate::icons::Icon::ThumbsUpFilled
            .image(p.accent, 20.0)
            .paint_at(
                ui,
                egui::Rect::from_center_size(badge, egui::vec2(20.0, 20.0)),
            );

        let text_x = art_rect.right() + 16.0;
        ui.painter().text(
            egui::pos2(text_x, art_rect.center().y - 10.0),
            egui::Align2::LEFT_CENTER,
            widgets::truncate_to(ui, &count_text, title_font.clone(), text_w),
            title_font,
            p.text,
        );
        ui.painter().text(
            egui::pos2(text_x, art_rect.center().y + 12.0),
            egui::Align2::LEFT_CENTER,
            widgets::truncate_to(ui, &by_text, by_font.clone(), text_w),
            by_font,
            p.secondary,
        );
    }
    if response.clicked() {
        app.navigate(crate::app::Route::ArtistLiked(id));
    }
}

/// Every shelf YouTube sent, in its order: a grid with "Show all" in the top right where it
/// offered one, or — once that's been pressed — the full list, boxed off so it reads as its
/// own block rather than the preview quietly growing in place.
fn shelves(app: &mut App, ui: &mut egui::Ui) {
    // One snapshot for the frame — a refcount bump, not a copy — the same pattern every sibling
    // feed (home/explore/releases/moods/history) already uses, so `artist_shelf` below can take
    // `&mut App` for navigation and the texture cache without holding a borrow on `app.artist`.
    let Some(shelves) = app.artist.as_ref().map(|a| Arc::clone(&a.shelves)) else {
        return;
    };
    for s in shelves.iter() {
        crate::ui::artist_shelf(app, ui, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hero's full rect, and the card it lives in — one and the same on a page that is not
    /// scrolled, which is all these rows care about.
    fn rect(y0: f32, y1: f32) -> egui::Rect {
        egui::Rect::from_min_max(pos2(100.0, y0), pos2(900.0, y1))
    }

    fn radius() -> f32 {
        crate::theme::radii().panel as f32
    }

    #[test]
    fn a_short_slice_keeps_the_cards_corner_radius() {
        // The last 10 points of a banner leaving the card's top edge. A rounded `RectShape`
        // would draw a 5-point corner here (epaint clamps to half the height); the rows must
        // stay on the card's own 20-point arc.
        let hero = rect(50.0, 450.0);
        let slice = rect(50.0, 60.0);
        let rows = hero_rows(hero, slice, slice.top());
        let (first_y, first_l, first_r) = rows[0];
        assert_eq!(first_y, slice.top());
        assert!((first_l - (hero.left() + radius())).abs() < 0.01);
        assert!((first_r - (hero.right() - radius())).abs() < 0.01);
        let (last_y, last_l, last_r) = *rows.last().unwrap();
        assert_eq!(last_y, slice.bottom());
        assert!(last_l > hero.left() && last_l < hero.left() + radius());
        assert!(last_r < hero.right() && last_r > hero.right() - radius());
    }

    #[test]
    fn a_slice_at_least_the_radius_tall_reaches_the_card_edges() {
        let hero = rect(50.0, 450.0);
        let slice = rect(50.0, 80.0);
        let rows = hero_rows(hero, slice, slice.top());
        let (last_y, last_l, last_r) = *rows.last().unwrap();
        assert_eq!(last_y, slice.bottom());
        assert!((last_l - hero.left()).abs() < 0.01);
        assert!((last_r - hero.right()).abs() < 0.01);
        // The arc, then the straight bottom edge.
        assert_eq!(rows.len(), HERO_ARC_STEPS + 2);
    }

    /// The last sliver of a banner scrolling out — under half a point of image, or of scrim —
    /// comes back from `hero_rows` as a single row. A mesh built from it must be empty, not
    /// vertices without triangles: wgpu panics on that ("buffer slice can not be empty"), which
    /// is how 0.1.10 crashed on Windows.
    #[test]
    fn the_last_sliver_of_a_banner_paints_no_half_built_mesh() {
        let hero = rect(50.0, 450.0);
        for (slice, start) in [
            (rect(449.7, 450.0), 449.7), // the image's last 0.3 pt
            (rect(50.0, 450.0), 449.8),  // a scrim starting at the very bottom
        ] {
            let rows = hero_rows(hero, slice, start);
            let mut mesh = egui::Mesh::default();
            add_rows(&mut mesh, &rows, &mut |_| {
                (egui::epaint::WHITE_UV, egui::Color32::WHITE)
            });
            assert_eq!(
                mesh.vertices.is_empty(),
                mesh.indices.is_empty(),
                "{} rows gave {} vertices and {} indices",
                rows.len(),
                mesh.vertices.len(),
                mesh.indices.len()
            );
        }
    }

    #[test]
    fn a_scrim_starting_below_the_arc_keeps_straight_sides() {
        let hero = rect(50.0, 450.0);
        let slice = rect(50.0, 450.0);
        let rows = hero_rows(hero, slice, 300.0);
        let (first_y, first_l, first_r) = rows[0];
        assert_eq!(first_y, 300.0);
        assert!((first_l - hero.left()).abs() < 0.01);
        assert!((first_r - hero.right()).abs() < 0.01);
        assert_eq!(rows.last().unwrap().0, slice.bottom());
    }

    #[test]
    fn rows_never_leave_the_slice_or_the_card() {
        let hero = rect(50.0, 450.0);
        for height in [1.0, 7.0, 20.0, 39.0, 120.0, 400.0] {
            let slice = rect(50.0, 50.0 + height);
            for start in [slice.top(), slice.top() + height * 0.5] {
                for (y, l, r) in hero_rows(hero, slice, start) {
                    assert!(y >= slice.top() - 0.01 && y <= slice.bottom() + 0.01);
                    assert!(l >= hero.left() - 0.01 && r <= hero.right() + 0.01);
                    assert!(r - l > 0.0);
                }
            }
        }
    }
}
