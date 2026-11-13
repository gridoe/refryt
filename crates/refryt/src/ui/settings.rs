//! Settings: a strip of tabs, and under it cards of rows for the open one.
//!
//! Sign-in lives on the Account tab. The primary path is importing cookies from a browser you are
//! already signed into. It is not a convenience: a cookie copied from the DevTools console
//! *cannot* work, because `document.cookie` is specified not to expose `HttpOnly` cookies and the
//! ones that authenticate a YouTube session — `HSID`, `SSID`, `LOGIN_INFO`, `__Secure-1PSID`,
//! `__Secure-3PSID` — are all HttpOnly. Pasting from the console produces a cookie that
//! `ytmapi-rs` accepts and YouTube ignores. The `Cookie` *request header* from the Network tab
//! does carry them, and that is what the Paste cookies window asks for (`ui::paste_cookies`).

use egui::RichText;

use crate::app::App;
use crate::auth::AuthState;
use crate::browser_cookies;
use crate::icons::Icon;
use crate::settings::Opacity;
use crate::settings::{CacheSize, Quality, Theme};
use crate::theme;
use crate::widgets;
use crate::widgets::Hint;

/// The tabs of the Settings page, in the order they are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Appearance,
    Playback,
    Library,
    System,
    Account,
    About,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 6] = [
        SettingsTab::Appearance,
        SettingsTab::Playback,
        SettingsTab::Library,
        SettingsTab::System,
        SettingsTab::Account,
        SettingsTab::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Playback => "Playback",
            SettingsTab::Library => "Library",
            SettingsTab::System => "System",
            SettingsTab::Account => "Account",
            SettingsTab::About => "About",
        }
    }
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    // The title scrolls away with the page rather than staying pinned over it.
    crate::ui::page_scroll(app, "settings").show(ui, |ui| {
        crate::ui::page_title(ui, &app.palette, "Settings");
        // Capped, and held clear of the card's right edge: with the queue or lyrics open the
        // card is narrower than the cap, and the switches sat on its edge, against the panel.
        ui.set_max_width(COLUMN_MAX_W.min(ui.available_width() - RIGHT_GUTTER));

        let labels = SettingsTab::ALL.map(SettingsTab::label);
        let current = SettingsTab::ALL
            .iter()
            .position(|t| *t == app.settings_tab)
            .unwrap_or(0);
        if let Some(i) = widgets::tab_strip(ui, &app.palette, &labels, current) {
            app.settings_tab = SettingsTab::ALL[i];
        }
        ui.add_space(20.0);

        match app.settings_tab {
            SettingsTab::Appearance => appearance(app, ui),
            SettingsTab::Playback => playback(app, ui),
            SettingsTab::Library => library(app, ui),
            SettingsTab::System => system(app, ui),
            SettingsTab::Account => account(app, ui),
            SettingsTab::About => about(app, ui),
        }
        ui.add_space(16.0);
    });
}

/// The About tab: the shortcuts window, then the app's own name, version and licence.
fn about(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let mut open_help = false;
    widgets::setting_group(ui, &p, Some("Help"), |ui, g| {
        g.row(
            ui,
            "Keyboard shortcuts",
            "Every key refryt answers to.",
            |ui| open_help = widgets::outline_button(ui, &p, "Show").clicked(),
        );
    });
    if open_help {
        app.help_open = true;
    }

    // Updates: the launch check's switch, and what the last check found.
    let mut check = app.settings.check_for_updates;
    let (mut check_now, mut show) = (false, false);
    let (title, detail) = match &app.update.available {
        Some(release) => (
            format!("refryt {} is available", release.version),
            format!("You have {}.", crate::update::current_version()),
        ),
        None if app.update.checking => (
            "Checking for updates…".to_owned(),
            "Asking GitHub for the latest release.".to_owned(),
        ),
        None if app.update.up_to_date => (
            "refryt is up to date".to_owned(),
            format!("{} is the latest release.", env!("CARGO_PKG_VERSION")),
        ),
        None => (
            "Latest version".to_owned(),
            "Asks GitHub for the latest release.".to_owned(),
        ),
    };
    widgets::setting_group(ui, &p, Some("Updates"), |ui, g| {
        toggle_row(
            g,
            ui,
            &p,
            "setting-check-updates",
            "Check for updates",
            "Looks for a new release on GitHub when refryt starts.",
            &mut check,
        );
        g.row(ui, &title, &detail, |ui| {
            if app.update.available.is_some() {
                show = widgets::outline_button(ui, &p, "Update").clicked();
            } else if !app.update.checking {
                check_now = widgets::outline_button(ui, &p, "Check now").clicked();
            }
        });
    });
    let ctx = ui.ctx().clone();
    app.update_settings(&ctx, |s| s.check_for_updates = check);
    if check_now {
        app.check_for_update(true);
    }
    if show {
        app.update.open = true;
    }

    if app.logo.is_none() {
        app.logo = crate::logo_texture(ui.ctx());
    }
    let logo = app.logo.clone();
    let (mut github, mut changes) = (false, false);
    let has_changes = crate::changelog::this_version().is_some();
    widgets::setting_group(ui, &p, Some("About"), |ui, g| {
        // An open-source app's own about-ish footer: icon, name, version, licence.
        g.free(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(logo) = &logo {
                    ui.add(egui::Image::new((logo.id(), egui::Vec2::splat(32.0))));
                    ui.add_space(8.0);
                }
                ui.vertical(|ui| {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("refryt")
                            .font(theme::semibold(14.0))
                            .color(p.text),
                    );
                    ui.label(
                        RichText::new(format!("v{} · GPL-3.0-or-later", env!("CARGO_PKG_VERSION")))
                            .font(theme::regular(11.0))
                            .color(p.dim),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    github = widgets::icon_button(ui, &p, Icon::Github, 18.0, false)
                        .hint("refryt on GitHub")
                        .clicked();
                    if has_changes {
                        changes = widgets::icon_button(ui, &p, Icon::CircleHelp, 18.0, false)
                            .hint("What's new in this version")
                            .clicked();
                    }
                });
            });
        });
    });
    if github {
        crate::open_url(crate::WEBSITE);
    }
    if changes {
        app.whats_new_open = true;
    }
}

/// Why the last sign-in attempt failed, shown wherever the user is.
fn sign_in_error(app: &App, ui: &mut egui::Ui) {
    let Some(msg) = &app.sign_in_error else {
        return;
    };
    ui.label(
        RichText::new(msg)
            .font(theme::regular(12.0))
            .color(app.palette.danger),
    );
    ui.add_space(10.0);
}

/// What needs saying under the Scheme row: the painting theme's warnings, a chosen theme that
/// is gone, the built-in stand-in when the folder has nothing of this mode, and files that
/// failed to read.
fn theme_notes(app: &App, ui: &mut egui::Ui, dark: bool) {
    let themes = &app.custom_themes;
    let mode = if dark { "dark" } else { "light" };
    let chosen = if dark {
        &app.settings.dark_theme
    } else {
        &app.settings.light_theme
    };
    match app.settings.painting_theme(dark, themes) {
        Some(t) => {
            if t.name != *chosen {
                problem(
                    app,
                    ui,
                    &format!("“{chosen}” is missing, using “{}”.", t.name),
                );
            }
            for warning in &t.warnings {
                note(app, ui, warning);
            }
        }
        None => problem(
            app,
            ui,
            &format!("No {mode} themes in the folder, using Classic."),
        ),
    }
    for (file, error) in &themes.errors {
        problem(app, ui, &format!("{file}: {error}"));
    }
}

/// A [`note`] that reports something wrong.
fn problem(app: &App, ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::regular(11.0))
            .color(app.palette.danger),
    );
}

fn note(app: &App, ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::regular(11.0))
            .color(app.palette.dim),
    );
}

/// How wide the settings column gets. Past this the switches, right-aligned, drifted a window's
/// width away from the words that name them.
const COLUMN_MAX_W: f32 = 760.0;
/// Room kept between the settings column and the card's right edge when the card is narrower
/// than [`COLUMN_MAX_W`].
const RIGHT_GUTTER: f32 = 40.0;

/// A titled row with a persisted on/off switch.
fn toggle_row(
    g: &mut widgets::Group,
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    id: &'static str,
    title: &str,
    detail: &str,
    value: &mut bool,
) {
    g.row(ui, title, detail, |ui| {
        widgets::toggle(ui, palette, ui.id().with(id), value);
    });
}

fn appearance(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;

    let mut theme_choice = app.settings.theme;
    // The mode on screen decides which themes are offered: a theme file says whether it is
    // dark or light, and picking one sets the theme for that mode only, so switching modes
    // (or following the desktop) swaps between two remembered choices.
    let dark = app.palette.dark;
    let mut dark_choice = app.settings.dark_theme.clone();
    let mut light_choice = app.settings.light_theme.clone();
    let offered: Vec<&crate::theme::custom::CustomTheme> = app
        .custom_themes
        .themes
        .iter()
        .filter(|t| t.dark == dark)
        .collect();
    let options: Vec<(egui::Color32, &str)> = offered
        .iter()
        .map(|t| (t.palette().accent, t.name.as_str()))
        .collect();
    let painting = app.settings.painting_theme(dark, &app.custom_themes);
    let (current_swatch, current_label) = match painting {
        Some(t) => (t.palette().accent, t.name.as_str()),
        None => (app.palette.accent, "Classic (built-in)"),
    };

    widgets::setting_group(ui, &p, Some("Colours"), |ui, g| {
        // Chips are laid out right to left, so the last option goes in first.
        g.row(
            ui,
            "Mode",
            "Follow the desktop, or stay dark or light.",
            |ui| {
                for option in Theme::ALL.into_iter().rev() {
                    if widgets::chip(ui, &p, option.label(), theme_choice == option).clicked() {
                        theme_choice = option;
                    }
                }
            },
        );
        g.row(ui, "Theme", "The colours used in this mode.", |ui| {
            if let Some(i) = widgets::dropdown(
                ui,
                &p,
                ui.id().with("scheme-dropdown"),
                current_swatch,
                current_label,
                &options,
            ) {
                let name = offered[i].name.clone();
                if dark {
                    dark_choice = name;
                } else {
                    light_choice = name;
                }
            }
        });
        g.row(
            ui,
            "Your own themes",
            "Drop a theme file in the folder and it appears above.",
            |ui| {
                if widgets::outline_button(ui, &p, "Open folder").clicked() {
                    match crate::theme::custom::dir() {
                        Some(dir) => {
                            if let Err(e) = std::fs::create_dir_all(&dir) {
                                tracing::warn!(%e, "could not create the themes folder");
                            }
                            crate::open_url(&dir.to_string_lossy());
                        }
                        None => tracing::warn!("no config directory for the themes folder"),
                    }
                }
            },
        );
    });
    theme_notes(app, ui, dark);

    let mut adaptive_art_color = app.settings.adaptive_art_color;
    let mut sheer = app.settings.opacity.sheer();
    let mut blur = app.settings.blur;
    let mut card_outline = app.settings.card_outline;
    let mut corners = app.settings.corners;
    let mut compositor_corners = app.settings.compositor_corners;
    let mut scroll_fades = app.settings.scroll_fades;
    widgets::setting_group(ui, &p, Some("Window"), |ui, g| {
        // A slider, not chips: transparency is a continuum, and four names only ever made
        // the setting look like a choice of four windows.
        let decorated = app.decorated;
        let percent = format!("{:.0}%", sheer / Opacity::MAX * 100.0);
        g.row(
            ui,
            "Transparency",
            if decorated {
                "Not available with `--decorated`."
            } else {
                "How much of the desktop shows through."
            },
            |ui| {
                // `--decorated` gives the window back to the OS, which paints its own opaque
                // background — the slider would do nothing, so it is dimmed and inert rather
                // than silently pretending.
                ui.add_enabled_ui(!decorated, |ui| {
                    // The number the slider only implies, in a fixed slot at the row's right
                    // edge so the changing digits move nothing: a percentage of the
                    // slider's whole range, which reaches past the old maximum — not of the
                    // composite alpha.
                    value_slot(ui, &p, &percent, 34.0);
                    ui.add_space(4.0);
                    widgets::slider(ui, &p, &mut sheer, 0.0..=Opacity::MAX, 150.0, 11);
                });
            },
        );

        let blur_available = app.blur.available();
        let blur_useful = sheer > 0.0;
        g.row(
            ui,
            "Background blur",
            if !blur_available {
                if cfg!(target_os = "windows") {
                    "Needs Windows 11 22H2 or later."
                } else {
                    "Your compositor doesn't support it."
                }
            } else if !blur_useful {
                "Turn up transparency to use it."
            } else {
                "Blurs the desktop behind the window."
            },
            |ui| {
                // Dimmed and inert rather than clickable-but-pointless when there is
                // nothing to turn on — no compositor support, or nothing set to show
                // through it yet.
                widgets::toggle_enabled(
                    ui,
                    &p,
                    ui.id().with("setting-blur"),
                    &mut blur,
                    blur_available && blur_useful,
                );
            },
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-card-outline",
            "Card outlines",
            "A thin edge around each panel.",
            &mut card_outline,
        );
        g.row(
            ui,
            "Corners",
            "How rounded the window, panels, menus and covers are.",
            |ui| {
                for option in crate::theme::Corners::ALL.into_iter().rev() {
                    if widgets::chip(ui, &p, option.label(), corners == option).clicked() {
                        corners = option;
                    }
                }
            },
        );
        // Only Linux draws the window's corner itself; Windows and macOS round it as they do.
        let linux = cfg!(target_os = "linux");
        g.row(
            ui,
            "Compositor window corners",
            if linux {
                "Lets your compositor round the window, instead of Corners. Linux only."
            } else {
                "Linux only. Windows and macOS always use the system's corners."
            },
            |ui| {
                widgets::toggle_enabled(
                    ui,
                    &p,
                    ui.id().with("setting-compositor-corners"),
                    &mut compositor_corners,
                    linux,
                );
            },
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-scroll-fades",
            "Scroll fades",
            "Fades lists out at their edges. Recommended with transparency.",
            &mut scroll_fades,
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-adaptive-art-color",
            "Adaptive colour",
            "Tints album and playlist headers and now-playing to match the cover.",
            &mut adaptive_art_color,
        );
    });

    let ctx = ui.ctx().clone();
    app.update_settings(&ctx, |s| {
        s.opacity = Opacity::new(sheer);
        s.blur = blur;
        s.theme = theme_choice;
        s.dark_theme = dark_choice;
        s.light_theme = light_choice;
        s.adaptive_art_color = adaptive_art_color;
        s.card_outline = card_outline;
        s.corners = corners;
        s.compositor_corners = compositor_corners;
        s.scroll_fades = scroll_fades;
    });
}

fn playback(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let mut quality = app.settings.quality;
    let mut autoplay = app.settings.autoplay;
    let mut keep_order = app.settings.queue_click_keeps_order;
    let mut normalise = app.settings.normalise_volume;
    let mut scrobble = app.settings.scrobble;
    let mut cache_songs = app.settings.cache_songs;
    let mut cache_clear_on_close = app.settings.cache_clear_on_close;
    let mut cache_size = app.settings.cache_size;

    widgets::setting_group(ui, &p, Some("Sound"), |ui, g| {
        g.row(ui, "Audio quality", quality.detail(), |ui| {
            for option in Quality::ALL.into_iter().rev() {
                if widgets::chip(ui, &p, option.label(), quality == option).clicked() {
                    quality = option;
                }
            }
        });
        toggle_row(
            g,
            ui,
            &p,
            "setting-normalise",
            "Audio normaliser",
            "Keeps the volume consistent between tracks.",
            &mut normalise,
        );
    });

    widgets::setting_group(ui, &p, Some("Cache"), |ui, g| {
        toggle_row(
            g,
            ui,
            &p,
            "setting-cache-songs",
            "Cache songs",
            "Keeps songs you play on this computer, so they start at once and still play if the connection drops.",
            &mut cache_songs,
        );
        if cache_songs {
            toggle_row(
                g,
                ui,
                &p,
                "setting-cache-clear-on-close",
                "Delete the cache on restart",
                "Empties the cache each time Refryt closes.",
                &mut cache_clear_on_close,
            );
            g.row(
                ui,
                "Cache size",
                "The songs played longest ago make room for new ones.",
                |ui| {
                    // Like Transparency: the size in a slot at the right edge as wide as the
                    // widest size, so the slider never shifts under the pointer, and the
                    // slider to its left with a mark for each size. A drag only previews: the
                    // cache is trimmed to the size let go on, not to every size passed.
                    let font = theme::regular(12.0);
                    let slot = CacheSize::ALL
                        .iter()
                        .map(|size| widgets::text_width(ui, size.label(), &font))
                        .fold(0.0, f32::max)
                        .ceil();
                    let drag_id = ui.id().with("cache-size-drag");
                    let last = (CacheSize::ALL.len() - 1) as f32;
                    let mut stop = ui
                        .data(|d| d.get_temp::<f32>(drag_id))
                        .unwrap_or(cache_size.index() as f32);
                    let shown = CacheSize::ALL[stop.round().clamp(0.0, last) as usize];
                    value_slot(ui, &p, shown.label(), slot);
                    ui.add_space(4.0);
                    let response =
                        widgets::slider(ui, &p, &mut stop, 0.0..=last, 150.0, CacheSize::ALL.len());
                    let stop = stop.round().clamp(0.0, last);
                    if response.dragged() {
                        ui.data_mut(|d| d.insert_temp(drag_id, stop));
                    } else {
                        ui.data_mut(|d| d.remove::<f32>(drag_id));
                        cache_size = CacheSize::ALL[stop as usize];
                    }
                },
            );
        }
    });

    widgets::setting_group(ui, &p, Some("Queue and history"), |ui, g| {
        toggle_row(
            g,
            ui,
            &p,
            "setting-autoplay",
            "Autoplay",
            "Plays similar songs when your queue ends.",
            &mut autoplay,
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-queue-keep-order",
            "Keep the songs you jump over",
            "Playing a song further down Up next plays it now and keeps the songs before it.",
            &mut keep_order,
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-scrobble",
            "Save listening history",
            "Adds what you play to your YouTube history.",
            &mut scrobble,
        );
    });

    let ctx = ui.ctx().clone();
    app.update_settings(&ctx, |s| {
        s.quality = quality;
        s.autoplay = autoplay;
        s.queue_click_keeps_order = keep_order;
        s.normalise_volume = normalise;
        s.scrobble = scrobble;
        s.cache_songs = cache_songs;
        s.cache_clear_on_close = cache_clear_on_close;
        s.cache_size = cache_size;
    });
}

/// A slider's value at the right edge of a right-to-left row, in a slot exactly `width` wide
/// whatever the text: a slot that followed the text would move the slider beside it, so the
/// same pointer position would read as another value from one frame to the next.
fn value_slot(ui: &mut egui::Ui, p: &theme::Palette, text: &str, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
    ui.painter().text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        text,
        theme::regular(12.0),
        p.text,
    );
}

/// How songs, playlists and Home behave, and where local music is read from.
/// Ask for a Takeout export.
pub(crate) fn takeout_file() -> Option<std::path::PathBuf> {
    // A test aid for debug builds: a scripted run can't reach the system's file dialog.
    #[cfg(debug_assertions)]
    if let Some(file) = std::env::var_os("REFRYT_TAKEOUT_FILE") {
        return Some(file.into());
    }
    rfd::FileDialog::new()
        .add_filter("JSON", &["json"])
        .pick_file()
}

const TAKEOUT_HELP_WIDTH: f32 = 320.0;

/// A `?` that opens the steps for getting an export out of Google Takeout.
pub(crate) fn takeout_help(ui: &mut egui::Ui, p: &theme::Palette) {
    const STEPS: [&str; 5] = [
        "Open Google Takeout, press Deselect all, then tick YouTube and YouTube Music.",
        "Under All YouTube data included, keep only history.",
        "Under Multiple formats, set history to JSON.",
        "Create the export and download it when it's ready. This can take a while.",
        "Unzip it and choose watch-history.json in the history folder. Its name follows your account's language.",
    ];
    let button = widgets::icon_button(ui, p, Icon::CircleHelp, 16.0, false)
        .hint("How to get a Takeout export");
    egui::Popup::new(
        ui.id().with("takeout-help"),
        ui.ctx().clone(),
        &button,
        ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .frame(widgets::menu_frame(ui, p))
    .width(TAKEOUT_HELP_WIDTH)
    .gap(4.0)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .open_memory(button.clicked().then_some(egui::SetOpenCommand::Toggle))
    .show(|ui| {
        ui.set_width(TAKEOUT_HELP_WIDTH);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Getting your Takeout export")
                .font(theme::semibold(13.0))
                .color(p.text),
        );
        ui.add_space(4.0);
        for (i, step) in STEPS.iter().enumerate() {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (slot, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 15.0), egui::Sense::hover());
                ui.painter().text(
                    slot.left_top(),
                    egui::Align2::LEFT_TOP,
                    (i + 1).to_string(),
                    theme::regular(12.0),
                    p.dim,
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(*step)
                            .font(theme::regular(12.0))
                            .color(p.secondary),
                    )
                    .wrap(),
                );
            });
        }
        ui.add_space(6.0);
        if widgets::outline_button(ui, p, "Open Google Takeout").clicked() {
            crate::open_url("https://takeout.google.com");
            ui.close();
        }
        ui.add_space(2.0);
    });
}

fn library(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let mut listening_stats = app.settings.listening_stats;
    let mut armed = app.stats_delete_armed;
    let mut delete_stats = false;
    let importing = app.takeout_importing;
    let mut pick_takeout = false;
    let mut double_click = app.settings.double_click_to_play;
    let mut title_album = app.settings.title_opens_album;
    let mut playlist_sort = app.settings.playlist_default_sort;
    let mut open_order = false;

    widgets::setting_group(ui, &p, Some("Listening stats"), |ui, g| {
        toggle_row(
            g,
            ui,
            &p,
            "setting-listening-stats",
            "Keep listening stats",
            "Logs what you play here, and what your account's history shows from elsewhere. The log stays on this computer.",
            &mut listening_stats,
        );
        g.row(
            ui,
            "Import from Google Takeout",
            "Adds your full history from a Google Takeout export.",
            |ui| {
                if importing {
                    ui.label(
                        egui::RichText::new("Importing…")
                            .font(theme::regular(13.0))
                            .color(p.secondary),
                    );
                } else if widgets::outline_button(ui, &p, "Choose file").clicked() {
                    pick_takeout = true;
                }
                ui.add_space(4.0);
                takeout_help(ui, &p);
            },
        );
        let detail = if armed {
            "This removes every play and can't be undone."
        } else {
            "Removes every play kept on this computer."
        };
        g.row(ui, "Delete stats", detail, |ui| {
            if armed {
                if widgets::outline_button(ui, &p, "Cancel").clicked() {
                    armed = false;
                }
                if widgets::pill_button(ui, &p, "Delete").clicked() {
                    delete_stats = true;
                }
            } else if widgets::outline_button(ui, &p, "Delete").clicked() {
                armed = true;
            }
        });
    });

    widgets::setting_group(ui, &p, Some("Songs and playlists"), |ui, g| {
        toggle_row(
            g,
            ui,
            &p,
            "setting-double-click",
            "Double-click to play",
            "One click selects, two clicks play.",
            &mut double_click,
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-title-album",
            "Song titles open their album",
            "Clicking a song's title in a list opens its album or single. Song cards and the player bar's title always do.",
            &mut title_album,
        );
        g.wide_row(
            ui,
            "Playlist order",
            "How playlists are ordered when you open them. The sort button on a playlist still changes it for that visit.",
            |ui| {
                for option in crate::app::PlaylistSort::ALL {
                    let label = match option {
                        // Named for what it shows in a playlist you add to: the latest songs on
                        // top.
                        crate::app::PlaylistSort::Default => "As saved",
                        crate::app::PlaylistSort::Reverse => "Newest added first",
                        other => other.label(),
                    };
                    if widgets::chip(ui, &p, label, playlist_sort == option).clicked() {
                        playlist_sort = option;
                    }
                }
            },
        );
    });

    // Which sections come first on Home: a window of its own, since it is a list to drag about.
    let mut strict = app.settings.home_strict_order;
    widgets::setting_group(ui, &p, Some("Home"), |ui, g| {
        g.row(
            ui,
            "Order of homepage",
            "Choose which sections come first on Home.",
            |ui| {
                open_order = widgets::outline_button(ui, &p, "Change…").clicked();
            },
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-home-strict",
            "Strict Home order",
            "Waits longer for Home so your top sections come first. Gives up after a few seconds.",
            &mut strict,
        );
    });
    if open_order {
        app.home_order_open = true;
    }

    let ctx = ui.ctx().clone();
    app.stats_delete_armed = armed;
    if pick_takeout && let Some(file) = takeout_file() {
        app.import_takeout(file);
    }
    if delete_stats {
        app.delete_stats();
    }
    app.update_settings(&ctx, |s| {
        s.listening_stats = listening_stats;
        s.double_click_to_play = double_click;
        s.title_opens_album = title_album;
        s.playlist_default_sort = playlist_sort;
        s.home_strict_order = strict;
    });

    local_music(app, ui);
}

/// Folders scanned for local audio files (mp3, wav, flac, and more), shown in the Library's
/// Local tab. Adding or removing a folder here is what triggers the rescan — see
/// `App::update_settings`'s own diff branch for `local_folders`.
fn local_music(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let mut folders = app.settings.local_folders.clone();
    let mut removed = None;
    let mut add = false;
    widgets::setting_group(ui, &p, Some("Local files"), |ui, g| {
        for (i, folder) in folders.iter().enumerate() {
            g.row(ui, folder, "", |ui| {
                if widgets::icon_button(ui, &p, crate::icons::Icon::X, 12.0, false).clicked() {
                    removed = Some(i);
                }
            });
        }
        g.row(
            ui,
            "Add a folder",
            "Folders to scan for music. They show up in Library → Local.",
            |ui| {
                add = widgets::outline_button(ui, &p, "Add folder").clicked();
            },
        );
    });
    if let Some(i) = removed {
        folders.remove(i);
    }
    if add && let Some(dir) = rfd::FileDialog::new().pick_folder() {
        let path = dir.to_string_lossy().into_owned();
        if !folders.contains(&path) {
            folders.push(path);
        }
    }

    let ctx = ui.ctx().clone();
    app.update_settings(&ctx, |s| {
        s.local_folders = folders;
    });
}

fn system(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;

    // Every platform this ships to has a mechanism — an XDG autostart entry on Linux, a
    // LaunchAgent on macOS, the per-user `Run` key on Windows (`autostart`'s own doc comment) —
    // so the row exists wherever one does, and nowhere else.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    let mut start_on_boot = app.settings.start_on_boot;
    let mut hide_tray = app.settings.hide_tray_on_close;
    // Wayland has no way to hide a window and show it again from code — only a real minimize,
    // and only in the hide direction (`app::hide_command`'s own doc comment has the specifics)
    // — so the promise made here has to be the one Wayland can actually keep, or the toggle
    // reads as broken the first time someone clicks the tray's Show and nothing happens.
    let hide_tray_detail = if crate::app::is_wayland() {
        "Closing minimises instead of quitting. Quit from the tray."
    } else {
        "Closing hides to the tray instead of quitting."
    };
    widgets::setting_group(ui, &p, Some("Desktop"), |ui, g| {
        // Not inside a Flatpak: its autostart entry would land in the sandbox's own config
        // folder, where nothing reads it.
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        if !crate::flatpak::sandboxed() {
            toggle_row(
                g,
                ui,
                &p,
                "setting-start-on-boot",
                "Launch on startup",
                "Opens refryt when you log in.",
                &mut start_on_boot,
            );
        }
        toggle_row(
            g,
            ui,
            &p,
            "setting-hide-tray",
            "Close to tray",
            hide_tray_detail,
            &mut hide_tray,
        );
    });

    // Discord Rich Presence. Nothing to configure: the app connects as its own Discord
    // application (the id is fixed in `crate::discord`), so the toggle is the whole setting.
    let mut discord = app.settings.discord_rich_presence;
    let mut lrclib = app.settings.lrclib_lyrics;
    let mut cast = app.settings.cast_receiver;
    let cast_detail = cast_detail(app);
    let cast_port = match app.cast_status {
        crate::cast::Status::Ready { port } => port,
        _ => crate::cast::dial::PORT,
    };
    widgets::setting_group(ui, &p, Some("Integrations"), |ui, g| {
        g.row(ui, "Cast from YouTube Music", &cast_detail, |ui| {
            widgets::toggle(ui, &p, ui.id().with("setting-cast"), &mut cast);
            ui.add_space(4.0);
            cast_help(ui, &p, cast_port);
        });
        toggle_row(
            g,
            ui,
            &p,
            "setting-discord",
            "Discord Rich Presence",
            "Shows what you're playing on Discord.",
            &mut discord,
        );
        toggle_row(
            g,
            ui,
            &p,
            "setting-lrclib",
            "Synced lyrics from LRCLIB",
            "Asks lrclib.net for lyrics with timing. Off, lyrics come from YouTube without it.",
            &mut lrclib,
        );
    });

    let ctx = ui.ctx().clone();
    app.update_settings(&ctx, |s| {
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        {
            s.start_on_boot = start_on_boot;
        }
        s.hide_tray_on_close = hide_tray;
        s.discord_rich_presence = discord;
        s.lrclib_lyrics = lrclib;
        s.cast_receiver = cast;
    });
}

const CAST_HELP_WIDTH: f32 = 440.0;

/// A `?` beside the cast switch: how a phone finds this computer, and what a firewall has to let
/// in for it to. A computer that never shows up in the phone's Cast menu gives no other clue.
fn cast_help(ui: &mut egui::Ui, p: &theme::Palette, port: u16) {
    let button = widgets::icon_button(ui, p, Icon::CircleHelp, 16.0, false)
        .hint("If your computer doesn't show up");
    egui::Popup::new(
        ui.id().with("cast-help"),
        ui.ctx().clone(),
        &button,
        ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .frame(widgets::menu_frame(ui, p))
    .width(CAST_HELP_WIDTH)
    .gap(4.0)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .open_memory(button.clicked().then_some(egui::SetOpenCommand::Toggle))
    .show(|ui| {
        ui.set_width(CAST_HELP_WIDTH);
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(text)
                    .font(theme::semibold(13.0))
                    .color(p.text),
            );
            ui.add_space(2.0);
        };
        let line = |ui: &mut egui::Ui, text: &str| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(theme::regular(12.0))
                        .color(p.secondary),
                )
                .wrap(),
            );
        };
        heading(ui, "Casting from your phone");
        line(
            ui,
            "Put the phone on the same network as this computer, open YouTube Music, \
             tap Cast and pick this computer.",
        );
        heading(ui, "If it doesn't show up");
        line(
            ui,
            &format!(
                "The phone finds this computer on UDP port 1900 and pairs on TCP port {port}. \
                 A firewall has to let both in."
            ),
        );
        if port != crate::cast::dial::PORT {
            line(
                ui,
                &format!(
                    "Port {} was in use, so this run pairs on {port}. A firewall rule for {} \
                     doesn't cover it.",
                    crate::cast::dial::PORT,
                    crate::cast::dial::PORT
                ),
            );
        }
        #[cfg(target_os = "linux")]
        {
            let from = local_range().map_or(String::new(), |r| format!("from {r} "));
            ui.add_space(4.0);
            line(ui, "With ufw:");
            command(
                ui,
                p,
                "cast-ufw",
                &format!(
                    "sudo ufw allow {from}to any port 1900 proto udp\n\
                     sudo ufw allow {from}to any port {port} proto tcp"
                ),
            );
            line(ui, "With firewalld:");
            command(
                ui,
                p,
                "cast-firewalld",
                &format!(
                    "sudo firewall-cmd --permanent --add-port=1900/udp\n\
                     sudo firewall-cmd --permanent --add-port={port}/tcp\n\
                     sudo firewall-cmd --reload"
                ),
            );
        }
        #[cfg(target_os = "windows")]
        line(
            ui,
            "The first time this is on, Windows asks whether refryt may use the network. Allow \
             it on private networks. If you said no, allow refryt under Windows Security, \
             Firewall & network protection, Allow an app through firewall.",
        );
        #[cfg(target_os = "macos")]
        line(
            ui,
            "macOS may ask whether refryt may accept incoming connections. Allow it. If you \
             said no, allow it under System Settings, Network, Firewall, Options.",
        );
        ui.add_space(2.0);
    });
}

/// Commands to paste into a terminal, in a box with a Copy button.
#[cfg(target_os = "linux")]
fn command(ui: &mut egui::Ui, p: &theme::Palette, id: &str, text: &str) {
    let copied_id = ui.id().with((id, "copied"));
    let copied_at: Option<f64> = ui.data(|d| d.get_temp(copied_id));
    let now = ui.input(|i| i.time);
    egui::Frame::new()
        .fill(p.window)
        .corner_radius(egui::CornerRadius::same(theme::radii().item))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(egui::FontId::monospace(11.0))
                        .color(p.text),
                )
                .wrap(),
            );
            ui.add_space(2.0);
            let recent = copied_at.is_some_and(|at| now - at < 2.0);
            let label = if recent { "Copied" } else { "Copy" };
            if widgets::outline_button(ui, p, label).clicked() {
                ui.ctx().copy_text(text.to_owned());
                ui.data_mut(|d| d.insert_temp(copied_id, now));
            }
            if recent {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
            }
        });
    ui.add_space(4.0);
}

/// The private range this computer's network address is in, for a firewall rule that lets in
/// only this network. Asks the system which address it would send from, without sending.
#[cfg(target_os = "linux")]
fn local_range() -> Option<&'static str> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("239.255.255.250", 1900)).ok()?;
    let std::net::IpAddr::V4(ip) = socket.local_addr().ok()?.ip() else {
        return None;
    };
    match ip.octets() {
        [192, 168, ..] => Some("192.168.0.0/16"),
        [10, ..] => Some("10.0.0.0/8"),
        [172, b, ..] if (16..32).contains(&b) => Some("172.16.0.0/12"),
        _ => None,
    }
}

/// What the cast receiver's row says under its title: what the switch does while it is off,
/// and where the receiver stands while it is on, since a device that does not show up in the
/// phone's menu gives no other sign of why.
fn cast_detail(app: &App) -> String {
    use crate::cast::Status;
    let Some(name) = app.cast_name() else {
        return "Lists this computer in the Cast menu of YouTube Music on phones on the same \
                network."
            .to_owned();
    };
    if let Some(phone) = app.cast_senders.first() {
        return format!("Casting from {phone}.");
    }
    match &app.cast_status {
        Status::Ready { .. } => format!("In the Cast menu as “{name}”."),
        Status::Failed(error) => format!("Not in the Cast menu yet: {error}. Still trying."),
        Status::Off | Status::Connecting => "Connecting to YouTube…".to_owned(),
    }
}

fn account(app: &mut App, ui: &mut egui::Ui) {
    sign_in_error(app, ui);
    match &app.auth {
        AuthState::SignedIn { .. } => signed_in(app, ui),
        AuthState::Checking => {
            ui.label(
                RichText::new("Checking your saved session…")
                    .font(theme::regular(13.0))
                    .color(app.palette.secondary),
            );
        }
        _ => sign_in(app, ui),
    }
}

fn sign_in(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    if let AuthState::Failed(msg) = &app.auth {
        ui.label(
            RichText::new(msg)
                .font(theme::regular(13.0))
                .color(p.danger),
        );
        ui.add_space(10.0);
    }

    widgets::setting_group(ui, &p, Some("Sign in"), |ui, g| {
        g.free(ui, |ui| {
            ui.label(
                RichText::new("Sign in to see your library and playlists.")
                    .font(theme::medium(13.0))
                    .color(p.text),
            );
            ui.label(
                RichText::new("Search and playback work without one.")
                    .font(theme::regular(11.0))
                    .color(p.dim),
            );
        });
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        window_sign_in(app, ui, g);
        // The sandbox cannot read a browser's profile folder, so there would be nothing to find.
        if !crate::flatpak::sandboxed() {
            browser_import(app, ui, g);
        }
        paste_cookies(app, ui, g);
    });
}

/// The way in when neither of the others can be used: the cookie header from a browser's
/// developer tools, pasted into [`crate::ui::paste_cookies`].
fn paste_cookies(app: &mut App, ui: &mut egui::Ui, g: &mut widgets::Group) {
    let p = app.palette;
    let mut clicked = false;
    g.row(
        ui,
        "Or paste cookies",
        "Copies the session from a browser's developer tools.",
        |ui| clicked = widgets::outline_button(ui, &p, "Paste").clicked(),
    );
    if clicked {
        app.paste_cookies_open = true;
    }
}

/// The path that needs no browser already signed in: the app's own window owns the session it
/// creates. On Windows that is also the only path that works at all — a modern Chromium's cookie
/// store is sealed with App-Bound Encryption, so the only process that can read it is the browser
/// itself, and this window is one. See [`crate::webview_login`].
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn window_sign_in(app: &mut App, ui: &mut egui::Ui, g: &mut widgets::Group) {
    let p = app.palette;
    let mut clicked = false;
    g.row(
        ui,
        "Sign-in window",
        "Signs in through Google in a small window of its own.",
        |ui| clicked = widgets::pill_button(ui, &p, "Sign in").clicked(),
    );
    if clicked {
        // Deliberately not `auth = Checking`: that state means "verifying a stored session at
        // startup", and `AuthChanged` reads it to tell an optimistically personalised feed from
        // an anonymous one a fresh sign-in has just invalidated. The state changes when the
        // attempt actually lands.
        app.backend.sign_in_from_window();
    }
}

/// The path that works: read the cookies out of a browser that is already signed in.
fn browser_import(app: &mut App, ui: &mut egui::Ui, g: &mut widgets::Group) {
    let title = if cfg!(any(
        target_os = "linux",
        target_os = "windows",
        target_os = "macos"
    )) {
        "Or import from a browser"
    } else {
        "Import from a browser"
    };
    let browsers = app
        .detected_browsers
        .get_or_insert_with(browser_cookies::detected)
        .clone();
    if browsers.is_empty() {
        g.row(
            ui,
            title,
            if cfg!(target_os = "windows") {
                "No browser found that refryt can read: Firefox-based ones and Helium work, Chrome and Edge don't. Sign in to music.youtube.com there, or use the window above."
            } else {
                "No browser found. Sign in to music.youtube.com in your browser first."
            },
            |_| {},
        );
        return;
    }
    // No "close the browser first" hint up front: Firefox-based browsers neither lock their store
    // nor seal it, and the sealed Chromium ones are not shown on Windows at all (see
    // `browser_cookies::readable_on`). A Chromium one that is shown there, Helium for one, does
    // lock its store while it runs; the import's error says to close it when that happens.
    let detail = "Uses the session from a browser you're signed in to.";
    g.row(ui, title, detail, |ui| import_controls(app, ui, &browsers));
}

/// The browser import's button, on Home and in Settings. With one browser found it imports from
/// it; with several it opens a menu of them, the one used most recently first.
pub(crate) fn import_controls(
    app: &mut App,
    ui: &mut egui::Ui,
    browsers: &[browser_cookies::Browser],
) {
    let p = app.palette;
    let busy = app.browser_importing;
    let label = if busy {
        "Importing…"
    } else {
        "Import from browser"
    };
    let button = ui
        .add_enabled_ui(!busy, |ui| widgets::outline_button(ui, &p, label))
        .inner;
    let mut chosen = None;
    if let [only] = browsers {
        if button.clicked() {
            chosen = Some(only.arg.clone());
        }
    } else {
        egui::Popup::new(
            ui.id().with("import-choose-browser"),
            ui.ctx().clone(),
            &button,
            ui.layer_id(),
        )
        .kind(egui::PopupKind::Menu)
        .frame(widgets::menu_frame(ui, &p))
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .width(widgets::MENU_WIDTH)
        .gap(4.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .open_memory(button.clicked().then_some(egui::SetOpenCommand::Toggle))
        .show(|mui| {
            for b in browsers {
                if widgets::menu_item_plain(mui, &p, b.label, None).clicked() {
                    chosen = Some(b.arg.clone());
                    mui.close();
                }
            }
        });
    }
    if let Some(arg) = chosen {
        app.import_from(arg);
    }
}

fn signed_in(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let account = match &app.auth {
        AuthState::SignedIn { account } => account.clone(),
        _ => String::new(),
    };
    let premium = app.premium;
    let mut sign_out = false;
    widgets::setting_group(ui, &p, Some("Account"), |ui, g| {
        // Naming the account is the point: it is YouTube confirming the session, not us
        // assuming it.
        g.row(
            ui,
            &if account.is_empty() {
                "Signed in".to_owned()
            } else {
                format!("Signed in as {account}")
            },
            "Your library and Home feed are synced.",
            |ui| sign_out = widgets::outline_button(ui, &p, "Sign out").clicked(),
        );
        // The Premium badge rides its own row: downloads follow it, so it should be seen.
        g.row(
            ui,
            "YouTube Premium",
            if premium {
                "Downloads are on."
            } else {
                "Downloads need YouTube Premium."
            },
            |ui| {
                if premium {
                    premium_badge(ui, &p);
                }
            },
        );
    });
    if sign_out {
        app.backend.sign_out();
    }
}

/// "Active": a small accent-tinted pill, painted by hand so a tall row cannot stretch it.
fn premium_badge(ui: &mut egui::Ui, palette: &crate::theme::Palette) {
    let galley = ui.fonts_mut(|f| {
        f.layout_no_wrap(
            "Active".to_owned(),
            theme::semibold(11.0),
            egui::Color32::PLACEHOLDER,
        )
    });
    let size = galley.size() + egui::vec2(18.0, 6.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        crate::theme::pill(rect),
        palette.accent.gamma_multiply(0.16),
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, palette.accent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_has_its_own_label_and_the_first_is_the_default() {
        let labels: Vec<&str> = SettingsTab::ALL.iter().map(|t| t.label()).collect();
        let mut unique = labels.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), SettingsTab::ALL.len());
        assert_eq!(SettingsTab::default(), SettingsTab::ALL[0]);
    }
}
