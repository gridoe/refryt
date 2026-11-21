//! An artist's or a song's stats page: everything the play log says about them, opened from
//! Stats' top lists or a song's "Your stats".

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use egui::{pos2, vec2};

use crate::app::{App, Route};
use crate::entity_stats::{EntityKey, EntityReport, Rank};
use crate::listening_stats::{listening_time, plays_label};
use crate::model::Track;
use crate::ui::listening::{Tile, bar_chart, quiet_line, tiles};
use crate::ui::{centered, failed, page_title, section};
use crate::widgets;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let key = match app.current_route() {
        Route::StatsArtist(name) => EntityKey::Artist(name),
        Route::StatsSong(id) => EntityKey::Song(id),
        _ => return,
    };
    if let Some(error) = app.entity_error.clone() {
        page_title(ui, &app.palette, "Stats");
        failed(app, ui, error, App::retry_route);
        return;
    }
    let Some(report) = app.entity_report.clone().filter(|r| r.key == key) else {
        widgets::skeleton_shelf(ui, &app.palette, widgets::SkeletonShelfShape::Rows, 8);
        return;
    };
    if report.plays == 0 {
        page_title(ui, &app.palette, "Stats");
        centered(
            app,
            ui,
            match key {
                EntityKey::Artist(_) => "No plays of this artist yet.",
                EntityKey::Song(_) => "No plays of this song yet.",
            },
        );
        return;
    }
    crate::ui::page_scroll(app, "entity-stats").show(ui, |ui| {
        header(app, ui, &report);
        totals(app, ui, &report);
        if let Some((track, at)) = &report.first_song {
            quiet_line(
                app,
                ui,
                &format!(
                    "Your first song by {} was “{}”, on {}.",
                    report.name,
                    track.title,
                    date(*at)
                ),
            );
        }
        if !report.top_songs.is_empty() {
            section(ui, &app.palette, "Your top songs");
            ui.push_id("entity-top-songs", |ui| {
                crate::ui::ranked_stats_rows(app, ui, &report.top_songs);
            });
        }
        if report.bars.len() > 1 {
            // Bars are years once the history is too long for months.
            let by_year = report.bars.iter().all(|b| b.label.len() == 4);
            section(
                ui,
                &app.palette,
                if by_year {
                    "Plays by year"
                } else {
                    "Plays by month"
                },
            );
            bar_chart(ui, &app.palette, &report.bars);
        }
        ui.add_space(24.0);
    });
}

/// The tracks the header's Play plays: the song, or the artist's top songs.
fn tracks(report: &EntityReport) -> Vec<Track> {
    match &report.track {
        Some(track) => vec![track.clone()],
        None => report
            .top_songs
            .iter()
            .filter_map(|e| match e {
                crate::model::Entry::Song(t) | crate::model::Entry::Video(t) => {
                    let mut t = t.clone();
                    t.plays = None;
                    Some(t)
                }
                _ => None,
            })
            .collect(),
    }
}

fn header(app: &mut App, ui: &mut egui::Ui, report: &EntityReport) {
    let p = app.palette;
    let artist = matches!(report.key, EntityKey::Artist(_));
    let queue = tracks(report);
    let playing = app.is_playing_any(&queue);
    let meta = format!(
        "{} · {}",
        plays_label(report.plays),
        time_text(report.seconds, report.seconds_estimated)
    );
    let second = report.rank.map(|r| {
        format!(
            "Your #{} {}",
            r.place,
            if artist { "artist" } else { "song" }
        )
    });
    let no_artists = [];
    let artists = report
        .track
        .as_ref()
        .map_or(&no_artists[..], |t| &t.artists[..]);
    let art_top = ui.cursor().top();
    let out = {
        let App {
            images, backend, ..
        } = &mut *app;
        crate::ui::page_header(
            ui,
            &p,
            images,
            backend,
            crate::ui::PageHeader {
                art_url: report.art.as_ref(),
                art_size: crate::ui::PAGE_ART_SIZE,
                circle: artist,
                kind_label: if artist { "Artist stats" } else { "Song stats" },
                title: &report.name,
                description: None,
                artists,
                second_plain: if artist { second.as_deref() } else { None },
                meta: Some(&meta),
                owner: None,
                credit_air: 5.6,
                credit_pt: 15.0,
                play_size: (!queue.is_empty()).then_some(crate::ui::DETAIL_PLAY_PT),
                playing,
                save_in_library: None,
                dots: false,
                salt: "entity-stats-artist",
            },
        )
    };
    // Beside Play: the way to the artist's own page, or from a song to its artist's stats.
    let link = if artist {
        Some(("Artist page", None))
    } else {
        report
            .track
            .as_ref()
            .and_then(|t| t.artists.first())
            .map(|a| ("Artist stats", Some(a.name.to_lowercase())))
    };
    let mut link_clicked = false;
    if let (Some((label, _)), Some(end)) = (&link, out.controls_end) {
        // `controls_end` already holds the header's gap after Play and the spacing after that;
        // 12 pt from Play, as the artist page's own buttons sit.
        let rect = egui::Rect::from_min_size(pos2(end.x - 8.0, end.y - 18.0), vec2(200.0, 36.0));
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        link_clicked = widgets::outline_button(&mut cell, &p, label).clicked();
    }
    ui.add_space(out.art_bottom - art_top + 16.0);

    if out.play {
        if app.queue_matches(&queue) {
            app.toggle_playback();
        } else {
            app.start_from(queue, 0);
        }
    }
    if let Some(route) = out.link {
        app.navigate(route);
    }
    if link_clicked {
        match link {
            Some((_, Some(name))) => app.navigate(Route::StatsArtist(name)),
            Some((_, None)) => match &report.artist_id {
                Some(id) => app.navigate(Route::Artist(id.clone())),
                // Known only from an import: no id, so their name is searched instead.
                None => app.search_again(report.name.clone()),
            },
            None => {}
        }
    }
}

fn totals(app: &App, ui: &mut egui::Ui, report: &EntityReport) {
    let p = app.palette;
    let rank =
        |r: Option<Rank>| r.map_or("–".to_owned(), |r| format!("#{} of {}", r.place, r.of));
    tiles(
        ui,
        &p,
        &[
            Tile {
                label: "Plays",
                value: report.plays.to_string(),
                delta: None,
            },
            Tile {
                label: "Listening time",
                value: time_text(report.seconds, report.seconds_estimated),
                delta: None,
            },
            Tile {
                label: "First played",
                value: report.first.map_or("–".to_owned(), date),
                delta: None,
            },
            Tile {
                label: "Last played",
                value: report.last.map_or("–".to_owned(), date),
                delta: None,
            },
        ],
    );
    let mut second = vec![
        Tile {
            label: "Rank, all time",
            value: rank(report.rank),
            delta: None,
        },
        Tile {
            label: "Rank, 6 months",
            value: rank(report.rank_recent),
            delta: None,
        },
    ];
    if matches!(report.key, EntityKey::Artist(_)) {
        second.push(Tile {
            label: "Songs played",
            value: report.songs.to_string(),
            delta: None,
        });
    }
    second.push(Tile {
        label: "Most in a day",
        value: report.best_day.map_or("–".to_owned(), |(day, plays)| {
            format!("{plays} on {}", short_date(day))
        }),
        delta: None,
    });
    tiles(ui, &p, &second);
}

/// "5 h 12 min", with a "~" once part of it is an estimate.
fn time_text(seconds: u64, estimated: bool) -> String {
    if estimated {
        format!("~{}", listening_time(seconds))
    } else {
        listening_time(seconds)
    }
}

/// "Mar 3, 2026", in local time.
fn date(at: i64) -> String {
    Local
        .timestamp_opt(at, 0)
        .earliest()
        .map_or_else(String::new, |at| at.format("%b %-d, %Y").to_string())
}

/// "Mar 3" this year, "Mar 3, 2025" before it.
fn short_date(day: NaiveDate) -> String {
    if day.year() == Local::now().year() {
        day.format("%b %-d").to_string()
    } else {
        day.format("%b %-d, %Y").to_string()
    }
}
