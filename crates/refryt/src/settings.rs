//! User preferences, persisted to `~/.config/refryt/settings.json`.
//!
//! Everything here changes something real. There is no setting that only writes itself to disk:
//! the theme and the colour themes repaint, the opacity changes what the window lets through,
//! normalisation changes the gain applied to the stream, the quality choice changes the format
//! `yt-dlp` is asked for,
//! and scrobbling reports plays to the account's watch history.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::theme::Palette;
use crate::theme::custom::{CustomTheme, Themes};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the desktop's light/dark preference.
    #[default]
    System,
    Dark,
    Light,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Dark, Theme::Light];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        }
    }

    /// Which polarity to paint, resolving `System` against what the desktop reports. The
    /// colours come from the theme chosen for that polarity; this only picks dark or light.
    pub fn is_dark(self, ctx: &egui::Context) -> bool {
        match self {
            Theme::Dark => true,
            Theme::Light => false,
            // What the desktop reports; egui falls back to dark when it cannot tell.
            Theme::System => ctx.system_theme().unwrap_or(egui::Theme::Dark) == egui::Theme::Dark,
        }
    }
}

/// How much of the desktop shows through the shell: a sheer amount, `0.0` (opaque, the
/// default) to [`Opacity::MAX`] (as much as the window can show through and still read).
///
/// A slider rather than levels, because the composite is a continuum. The four named levels the
/// setting shipped with still parse from an old `settings.json` or `--opacity`, and the curve
/// passes through their exact values — an old file repaints as it did. The old "strong" level is
/// simply no longer the end of the range; the slider continues past it.
///
/// Each amount names what the gutter and a card should *composite* to; the card's own fill alpha
/// is solved from the pair in [`Palette::with_opacity`], because the two are painted one over
/// the other and a single alpha would leave the card nearly opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opacity(f32);

impl Default for Opacity {
    fn default() -> Self {
        Self::OFF
    }
}

impl Opacity {
    /// Fully opaque — what every build before opacity existed painted.
    pub const OFF: Self = Self(0.0);
    /// The sheerest the window goes. The old "Strong" level sat at `1.0`, below it: the slider
    /// used to stop where that level did, and now continues past it for the times the maximum
    /// was not see-through enough.
    pub const MAX: f32 = 1.25;

    /// A sheer amount, clamped to what the window can paint. A non-finite amount is opaque.
    pub fn new(sheer: f32) -> Self {
        if sheer.is_finite() {
            Self(sheer.clamp(0.0, Self::MAX))
        } else {
            Self::OFF
        }
    }

    /// The sheer amount behind the slider.
    pub fn sheer(self) -> f32 {
        self.0
    }

    /// Whether nothing at all shows through.
    pub fn is_opaque(self) -> bool {
        self.0 <= 0.0
    }

    /// Parse an `--opacity` argument: a number between 0 and 1, or one of the four level names
    /// the setting used before the slider.
    pub fn parse(name: &str) -> Option<Self> {
        Self::level(name).or_else(|| {
            name.trim()
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite())
                .map(Self::new)
        })
    }

    /// The old named levels, on the curve where the slider's thirds sit and where its end used
    /// to be — "strong" is `1.0` now, not [`Self::MAX`], so a file that names it repaints as it
    /// did rather than jumping to the new end of the slider.
    fn level(name: &str) -> Option<Self> {
        [
            ("off", Self::OFF),
            ("subtle", Self::new(1.0 / 3.0)),
            ("medium", Self::new(2.0 / 3.0)),
            ("strong", Self::new(1.0)),
        ]
        .into_iter()
        .find(|(label, _)| label.eq_ignore_ascii_case(name.trim()))
        .map(|(_, level)| level)
    }

    /// What the gutter, the top bar and the player bar composite to. Piecewise linear through
    /// `(0, 1.00)`, `(1/3, 0.88)`, `(2/3, 0.74)` — the old levels — then on through
    /// `(1, 0.58)`, where the old "strong" sat, to `(MAX, 0.30)`.
    pub fn gutter(self) -> f32 {
        let s = self.0;
        if s <= 1.0 / 3.0 {
            lerp(1.00, 0.88, s * 3.0)
        } else if s <= 2.0 / 3.0 {
            lerp(0.88, 0.74, (s - 1.0 / 3.0) * 3.0)
        } else if s <= 1.0 {
            lerp(0.74, 0.58, (s - 2.0 / 3.0) * 3.0)
        } else {
            lerp(0.58, 0.30, (s - 1.0) / (Self::MAX - 1.0))
        }
    }

    /// What a card composites to, the gutter underneath it included. Through `(0, 1.00)`,
    /// `(1/3, 0.94)`, `(2/3, 0.86)`, `(1, 0.74)` and on to `(MAX, 0.50)`.
    pub fn card(self) -> f32 {
        let s = self.0;
        if s <= 1.0 / 3.0 {
            lerp(1.00, 0.94, s * 3.0)
        } else if s <= 2.0 / 3.0 {
            lerp(0.94, 0.86, (s - 1.0 / 3.0) * 3.0)
        } else if s <= 1.0 {
            lerp(0.86, 0.74, (s - 2.0 / 3.0) * 3.0)
        } else {
            lerp(0.74, 0.50, (s - 1.0) / (Self::MAX - 1.0))
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

impl Serialize for Opacity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f32(self.0)
    }
}

impl<'de> Deserialize<'de> for Opacity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Amount;
        impl<'de> serde::de::Visitor<'de> for Amount {
            type Value = Opacity;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a sheer amount between 0 and 1.25, or off/subtle/medium/strong")
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Opacity, E> {
                Ok(Opacity::new(v as f32))
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Opacity, E> {
                Ok(Opacity::new(v as f32))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Opacity, E> {
                Ok(Opacity::new(v as f32))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Opacity, E> {
                Opacity::parse(v).ok_or_else(|| E::custom(format!("unknown opacity {v:?}")))
            }
        }
        deserializer.deserialize_any(Amount)
    }
}

/// What to ask `yt-dlp` for.
///
/// YouTube Music's ceiling without the official player is Opus ~160k (itag 251); the lower
/// settings are for metered connections and genuinely fetch a smaller stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    #[default]
    High,
    Medium,
    Low,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::High, Quality::Medium, Quality::Low];

    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "High",
            Quality::Medium => "Medium",
            Quality::Low => "Low",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Quality::High => "Opus, ~160 kbps",
            Quality::Medium => "Up to 128 kbps",
            Quality::Low => "Smallest stream, for metered data",
        }
    }

    /// The `yt-dlp` format selector. Each falls back to *some* audio rather than failing.
    pub fn format(self) -> &'static str {
        match self {
            Quality::High => "bestaudio[acodec=opus]/bestaudio",
            Quality::Medium => "bestaudio[abr<=128]/bestaudio",
            Quality::Low => "worstaudio[abr>=48]/worstaudio/bestaudio",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// The colour theme painted in dark mode: a file name from the themes folder (see
    /// [`crate::theme::custom`]) whose `color-scheme` is dark.
    pub dark_theme: String,
    /// The colour theme painted in light mode.
    pub light_theme: String,
    /// Before themes were files: a family (`catppuccin`), its flavour (`Macchiato`) and a custom
    /// theme picked over them. Read from an older file, folded into the two names above by
    /// [`Settings::migrate_legacy`], and never written back.
    #[serde(default, skip_serializing)]
    scheme: Option<String>,
    #[serde(default, skip_serializing)]
    scheme_variant: Option<String>,
    #[serde(default, skip_serializing)]
    custom_theme: Option<String>,
    /// How much shows through the shell.
    pub opacity: Opacity,
    /// Ask the compositor to blur what shows through, where it offers it.
    pub blur: bool,
    /// A thin edge around each card, solid window or see-through.
    pub card_outline: bool,
    /// How round the corners are, everywhere.
    pub corners: crate::theme::Corners,
    /// Linux: the window draws no corner of its own and leaves it to the compositor. Off, it
    /// follows `corners`. Windows and macOS round the window themselves.
    pub compositor_corners: bool,
    /// Fade a scrolled list out at an end with more of it past that end, in the cards (not the
    /// fullscreen player, which always fades). Off by default; it is worth most on a see-through
    /// window, where a list otherwise stops at a hard line over the desktop.
    pub scroll_fades: bool,
    /// Tint an album/playlist header with a colour pulled from its own cover art, instead of
    /// the scheme's fixed accent. Off falls back to the accent everywhere, unconditionally.
    pub adaptive_art_color: bool,
    /// What the resolver asks for.
    pub quality: Quality,
    /// Keep playing related songs when a hand-picked queue nears its end. YouTube's own radio,
    /// fetched for the queue's last track. On by default, as on the site.
    pub autoplay: bool,
    /// Clicking a song further down Up next plays it straight away and keeps the songs before it
    /// still to come, after it, instead of passing over them. Off by default: the click jumps to
    /// that place in the queue.
    pub queue_click_keeps_order: bool,
    /// Whether the fullscreen player shows the lyrics beside the cover. On by default; the
    /// Lyrics button in that view changes it, and a song with no lyrics never does.
    pub fullscreen_lyrics: bool,
    /// The order of Home's sections, as [`crate::model::HomeSection::key`]s. Read through
    /// [`crate::model::normalise_order`], which makes any saved list complete.
    pub home_order: Vec<String>,
    /// Spend more of Home's load following the feed until the sections ranked highest have
    /// appeared, so the order is kept more closely, up to a time limit. Off, Home loads as
    /// quickly as it always did.
    pub home_strict_order: bool,
    /// Apply YouTube's own loudness measurement so tracks do not jump in level.
    pub normalise_volume: bool,
    /// Spotify-style playback: a single click on a song selects it and only a double click
    /// plays it. On by default; off restores single-click-to-play everywhere.
    pub double_click_to_play: bool,
    /// A song's title opens its album or single: in track rows the title is a link, and a song
    /// card opens it. Off by default; the player bar's title opens it either way.
    pub title_opens_album: bool,
    /// Report plays to the signed-in account's watch history. Anonymous listening never
    /// reports — there is no account to count on — so this only matters while signed in.
    pub scrobble: bool,
    /// The brand account to browse as, when the signed-in Google account has more than one.
    /// `None` is the personal channel.
    pub account_page_id: Option<String>,
    /// The rail's playlist order, by id, as the user dragged it. Ids YouTube no longer returns
    /// are ignored; new ones keep YouTube's order after the ones that are named.
    pub playlist_order: Vec<String>,
    /// Launch on login: an XDG autostart entry on Linux, a LaunchAgent on macOS and the per-user
    /// `Run` key on Windows (see the `autostart` module).
    pub start_on_boot: bool,
    /// Closing the window hides it to a tray icon instead of quitting. Off by default: a tray
    /// icon that appears the first time someone closes the window, with no prior warning that
    /// closing no longer quits, would look like the close button broke.
    pub hide_tray_on_close: bool,
    /// Publish what is playing to Discord as a Rich Presence. Off by default: it tells everyone
    /// who can see your profile what you are listening to, which is worth an explicit yes. It
    /// connects as refryt's own Discord application — the id is fixed in `crate::discord` — so
    /// turning it on is the only setup there is.
    pub discord_rich_presence: bool,
    /// Ask LRCLIB for synced lyrics. On by default; off, lyrics come from YouTube alone, as
    /// plain text. LRCLIB gets the song's title, artist, album and length while the lyrics are
    /// shown, so it is a switch: see PRIVACY.md.
    pub lrclib_lyrics: bool,
    /// Folders scanned for local audio files, shown in the Library's Local tab. Absolute paths,
    /// same shape as `playlist_order`.
    pub local_folders: Vec<String>,
    /// The order a playlist opens in. `Default` is the playlist's own; `Reverse` puts the
    /// newest additions first. The page's sort menu still changes it for that visit.
    pub playlist_default_sort: crate::app::PlaylistSort,
    /// The sidebar's width as last dragged, in points. `None` until it has been.
    pub sidebar_width: Option<f32>,
    /// The queue/lyrics panel's width as last dragged. Still capped by the window on the way in.
    pub panel_width: Option<f32>,
    /// Ask GitHub for the latest release at launch, and offer it when it is newer. One request,
    /// nothing installed without a click (see `crate::update`).
    pub check_for_updates: bool,
    /// A release the user chose to skip: the launch check does not offer it again, though About
    /// still shows it and a manual check still offers it.
    pub skipped_update: Option<String>,
    /// A release the user answered "Later" to, and when (unix seconds): the launch check keeps
    /// quiet about it for a day.
    pub update_later: Option<(String, i64)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            dark_theme: "Classic Dark".to_owned(),
            light_theme: "Classic Light".to_owned(),
            scheme: None,
            scheme_variant: None,
            custom_theme: None,
            opacity: Opacity::default(),
            blur: true,
            card_outline: true,
            corners: crate::theme::Corners::default(),
            compositor_corners: false,
            scroll_fades: false,
            adaptive_art_color: true,
            quality: Quality::default(),
            autoplay: true,
            queue_click_keeps_order: false,
            fullscreen_lyrics: true,
            home_order: crate::model::default_order_keys(),
            home_strict_order: false,
            normalise_volume: false,
            double_click_to_play: true,
            title_opens_album: false,
            scrobble: true,
            account_page_id: None,
            playlist_order: Vec::new(),
            start_on_boot: false,
            hide_tray_on_close: false,
            discord_rich_presence: false,
            lrclib_lyrics: true,
            local_folders: Vec::new(),
            playlist_default_sort: crate::app::PlaylistSort::Default,
            sidebar_width: None,
            panel_width: None,
            check_for_updates: true,
            skipped_update: None,
            update_later: None,
        }
    }
}

impl Settings {
    /// The theme chosen for this polarity, if it loaded and is of that polarity.
    pub fn chosen_theme<'a>(&self, dark: bool, themes: &'a Themes) -> Option<&'a CustomTheme> {
        let name = if dark {
            &self.dark_theme
        } else {
            &self.light_theme
        };
        themes.get(name).filter(|t| t.dark == dark)
    }

    /// The theme that paints in this polarity: the chosen one; else the first of that polarity
    /// in the folder, when the chosen file was deleted or broke; else `None`, and the
    /// compiled-in fallback paints.
    pub fn painting_theme<'a>(&self, dark: bool, themes: &'a Themes) -> Option<&'a CustomTheme> {
        self.chosen_theme(dark, themes)
            .or_else(|| themes.themes.iter().find(|t| t.dark == dark))
    }

    /// The palette to paint with: the theme for the polarity on screen, at the chosen opacity.
    /// The one place those meet.
    pub fn palette(&self, ctx: &egui::Context, themes: &Themes) -> Palette {
        // Not part of the palette, but rebuilt with it: whatever repaints the palette (and
        // re-applies egui's style, whose widget corners read it) picks the level up.
        crate::theme::set_corners(self.corners);
        crate::theme::set_compositor_corners(self.compositor_corners);
        crate::widgets::set_title_opens_album(self.title_opens_album);
        let dark = self.theme.is_dark(ctx);
        match self.painting_theme(dark, themes) {
            Some(t) => t.palette(),
            None => crate::theme::custom::fallback(dark).palette(),
        }
        .with_opacity(self.opacity)
        .with_card_outline(self.card_outline)
    }

    /// Fold a pre-files settings file into [`Self::dark_theme`]/[`Self::light_theme`]: a family
    /// and flavour become the theme files they were turned into, and a custom theme picked over
    /// them goes to the polarity it declares. Idempotent; the old fields are never saved.
    pub fn migrate_legacy(&mut self, themes: &Themes) {
        if let Some(family) = self.scheme.take()
            && let Some((dark, light)) = legacy_names(&family, self.scheme_variant.as_deref())
        {
            self.dark_theme = dark;
            self.light_theme = light;
        }
        self.scheme_variant = None;
        if let Some(name) = self.custom_theme.take()
            && let Some(t) = themes.get(&name)
        {
            if t.dark {
                self.dark_theme = name;
            } else {
                self.light_theme = name;
            }
        }
    }
}

/// The theme files an old family and flavour became: `(dark, light)`. The stored flavour names
/// its own polarity; the other polarity takes the family's first of that kind, which is what
/// the flavour system painted there.
fn legacy_names(family: &str, flavour: Option<&str>) -> Option<(String, String)> {
    let (label, flavours): (&str, &[(&str, bool)]) = match family {
        "classic" | "ytm" => ("Classic", &[("Dark", true), ("Light", false)]),
        "catppuccin" => (
            "Catppuccin",
            &[
                ("Latte", false),
                ("Frappé", true),
                ("Macchiato", true),
                ("Mocha", true),
            ],
        ),
        "nord" => ("Nord", &[("Polar Night", true), ("Snow Storm", false)]),
        "gruvbox" => ("Gruvbox", &[("Dark", true), ("Light", false)]),
        "tokyo_night" => (
            "Tokyo Night",
            &[("Night", true), ("Storm", true), ("Day", false)],
        ),
        "dracula" => ("Dracula", &[("Dracula", true), ("Alucard", false)]),
        "rose_pine" => (
            "Rosé Pine",
            &[("Main", true), ("Moon", true), ("Dawn", false)],
        ),
        "everforest" => ("Everforest", &[("Dark", true), ("Light", false)]),
        "solarized" => ("Solarized", &[("Dark", true), ("Light", false)]),
        _ => return None,
    };
    let name = |flavour: &str| match (label, flavour) {
        ("Tokyo Night", "Night") | ("Rosé Pine", "Main") => label.to_owned(),
        (_, f) if f == label => label.to_owned(),
        (_, f) => format!("{label} {f}"),
    };
    let pick = |dark: bool| {
        flavours
            .iter()
            .find(|(f, d)| *d == dark && Some(*f) == flavour)
            .or_else(|| flavours.iter().find(|(_, d)| *d == dark))
            .map(|(f, _)| name(f))
    };
    Some((pick(true)?, pick(false)?))
}

fn path() -> Result<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "refryt")
        .context("could not find the config directory")?;
    Ok(dirs.config_dir().join("settings.json"))
}

/// Load saved settings, falling back to the defaults for anything unreadable — a broken settings
/// file should not stop the app starting.
pub fn load() -> Settings {
    let Ok(path) = path() else {
        return Settings::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Settings::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "settings file could not be parsed; using defaults");
        Settings::default()
    })
}

pub fn save(settings: &Settings) -> Result<()> {
    let path = path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(settings).context("could not encode the settings")?;
    crate::persist::write_json_atomic(&path, &json)
        .with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings file from an older build, or a hand-edited one, must not stop the app.
    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        let partial: Settings =
            serde_json::from_str(r#"{"theme":"light","something_new":42}"#).unwrap();
        assert_eq!(partial.theme, Theme::Light);
        assert_eq!(partial.quality, Quality::High);
        assert_eq!(
            partial.dark_theme, "Classic Dark",
            "no theme named yet defaults to ours"
        );
        assert_eq!(partial.light_theme, "Classic Light");
        assert_eq!(
            partial.opacity,
            Opacity::OFF,
            "opaque until asked otherwise"
        );
        assert!(
            partial.blur,
            "blur defaults on; it only ever does anything once opacity is too"
        );
        assert!(
            partial.scrobble,
            "an older settings file without the toggle must still scrobble"
        );
        assert!(
            partial.autoplay,
            "an older settings file without the toggle must still autoplay"
        );
        assert!(
            partial.lrclib_lyrics,
            "an older settings file without the toggle keeps synced lyrics"
        );
    }

    /// The literal shape of every file on disk before this milestone: a theme and the tint
    /// toggle that no longer exists. It must still parse, and land on today's defaults for the
    /// three fields it never mentioned — this is the actual migration every user's file takes.
    #[test]
    fn a_settings_file_from_before_schemes_still_loads() {
        let migrated: Settings =
            serde_json::from_str(r#"{"theme":"dark","tint_from_art":false}"#).unwrap();
        assert_eq!(migrated.theme, Theme::Dark);
        assert_eq!(migrated.dark_theme, "Classic Dark");
        assert_eq!(migrated.opacity, Opacity::OFF);

        // The default family was called "ytm" before it was renamed; an old file must land on
        // Classic, not fall back to the whole default struct.
        let mut old_name: Settings = serde_json::from_str(r#"{"scheme":"ytm"}"#).unwrap();
        old_name.migrate_legacy(&Themes::default());
        assert_eq!(old_name.dark_theme, "Classic Dark");
    }

    /// The slider is new, but a file written while the setting was four named levels must
    /// repaint exactly as it did — and a hand-edited number must reach the slider.
    #[test]
    fn an_opacity_from_before_the_slider_keeps_its_exact_pair() {
        let old: Settings = serde_json::from_str(r#"{"opacity":"medium"}"#).unwrap();
        assert_eq!(old.opacity.gutter(), 0.74);
        assert_eq!(old.opacity.card(), 0.86);

        let number: Settings = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
        assert_eq!(number.opacity.sheer(), 0.5);
        // An integer is a number too.
        let zero: Settings = serde_json::from_str(r#"{"opacity":0}"#).unwrap();
        assert_eq!(zero.opacity, Opacity::OFF);
    }

    /// Strict Home order is opt-in: off for a new install and for a file from before it existed,
    /// and a choice to turn it on survives a save.
    #[test]
    fn strict_home_order_is_off_until_chosen() {
        assert!(!Settings::default().home_strict_order);
        let old: Settings = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
        assert!(!old.home_strict_order);
        let on = Settings {
            home_strict_order: true,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_slice(&serde_json::to_vec(&on).unwrap()).unwrap();
        assert!(back.home_strict_order);
    }

    /// Keeping the songs you jump over is off for anyone whose file predates the setting, and a
    /// choice to turn it on survives a save.
    #[test]
    fn queue_click_keeps_order_is_off_until_chosen() {
        let old: Settings = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
        assert!(!old.queue_click_keeps_order);
        let on = Settings {
            queue_click_keeps_order: true,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_slice(&serde_json::to_vec(&on).unwrap()).unwrap();
        assert!(back.queue_click_keeps_order);
    }

    /// Card outlines are on for anyone whose file predates the setting, and a choice to turn them
    /// off survives a save.
    #[test]
    fn card_outlines_default_on_and_remember_off() {
        let old: Settings = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
        assert!(old.card_outline);
        let off = Settings {
            card_outline: false,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_slice(&serde_json::to_vec(&off).unwrap()).unwrap();
        assert!(!back.card_outline);
    }

    /// Corners are Round for anyone whose file predates the setting, and another level survives
    /// a save.
    #[test]
    fn corners_default_round_and_remember_the_choice() {
        use crate::theme::Corners;
        let old: Settings = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
        assert_eq!(old.corners, Corners::Round);
        for corners in Corners::ALL {
            let set = Settings {
                corners,
                ..Settings::default()
            };
            let back: Settings =
                serde_json::from_slice(&serde_json::to_vec(&set).unwrap()).unwrap();
            assert_eq!(back.corners, corners);
        }
    }

    /// Both switches are off for anyone whose file predates them.
    #[test]
    fn new_switches_default_off() {
        let old: Settings = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
        assert!(!old.compositor_corners);
        assert!(!old.title_opens_album);
        let set = Settings {
            compositor_corners: true,
            title_opens_album: true,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_slice(&serde_json::to_vec(&set).unwrap()).unwrap();
        assert!(back.compositor_corners && back.title_opens_album);
    }

    /// Scroll fades are off for anyone whose file predates the setting, and a choice to turn them
    /// on survives a save.
    #[test]
    fn scroll_fades_default_off_and_remember_on() {
        let old: Settings = serde_json::from_str(r#"{"opacity":0.5}"#).unwrap();
        assert!(!old.scroll_fades);
        let on = Settings {
            scroll_fades: true,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_slice(&serde_json::to_vec(&on).unwrap()).unwrap();
        assert!(back.scroll_fades);
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            theme: Theme::Dark,
            dark_theme: "Catppuccin Macchiato".into(),
            light_theme: "Nord Snow Storm".into(),
            opacity: Opacity::new(0.5),
            blur: false,
            quality: Quality::Low,
            account_page_id: Some("page-1".into()),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    /// A family and flavour from before themes were files land on the files they became: the
    /// flavour in its own polarity, the family's first in the other — what used to paint there.
    #[test]
    fn a_family_and_flavour_migrate_to_theme_names() {
        let mut s: Settings =
            serde_json::from_str(r#"{"scheme":"catppuccin","scheme_variant":"Macchiato"}"#)
                .unwrap();
        s.migrate_legacy(&Themes::default());
        assert_eq!(s.dark_theme, "Catppuccin Macchiato");
        assert_eq!(s.light_theme, "Catppuccin Latte");

        let mut s: Settings = serde_json::from_str(r#"{"scheme":"tokyo_night"}"#).unwrap();
        s.migrate_legacy(&Themes::default());
        assert_eq!(
            (s.dark_theme.as_str(), s.light_theme.as_str()),
            ("Tokyo Night", "Tokyo Night Day")
        );

        let mut s: Settings =
            serde_json::from_str(r#"{"scheme":"rose_pine","scheme_variant":"Dawn"}"#).unwrap();
        s.migrate_legacy(&Themes::default());
        assert_eq!(
            (s.dark_theme.as_str(), s.light_theme.as_str()),
            ("Rosé Pine", "Rosé Pine Dawn")
        );

        // The old fields are never written back.
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("scheme"), "{json}");
    }

    /// Every family the old enum knew maps to theme files that actually ship.
    #[test]
    fn every_legacy_family_maps_to_shipped_themes() {
        let shipped: Vec<&str> = crate::theme::custom::BUILTIN
            .iter()
            .map(|(n, _)| *n)
            .collect();
        for family in [
            "classic",
            "ytm",
            "catppuccin",
            "nord",
            "gruvbox",
            "tokyo_night",
            "dracula",
            "rose_pine",
            "everforest",
            "solarized",
        ] {
            let (dark, light) = legacy_names(family, None).expect(family);
            assert!(shipped.contains(&dark.as_str()), "{family}: {dark}");
            assert!(shipped.contains(&light.as_str()), "{family}: {light}");
        }
    }

    /// A chosen theme that is gone falls to another of its polarity, and only an empty
    /// polarity falls to the compiled-in Classic.
    #[test]
    fn a_missing_choice_falls_back_within_its_polarity() {
        let css = |base: &str, scheme: &str| {
            format!("color-scheme:{scheme};--base:{base};--text:#888888;--accent:#ff0000;")
        };
        let themes = Themes {
            themes: vec![
                crate::theme::custom::parse("Night", &css("#101010", "dark")).unwrap(),
                crate::theme::custom::parse("Paper", &css("#fafafa", "light")).unwrap(),
            ],
            errors: Vec::new(),
        };
        let s = Settings {
            dark_theme: "Deleted".into(),
            light_theme: "Night".into(),
            ..Settings::default()
        };
        assert_eq!(s.painting_theme(true, &themes).unwrap().name, "Night");
        // "Night" is dark, so it is not a light choice; the light polarity takes Paper.
        assert_eq!(s.painting_theme(false, &themes).unwrap().name, "Paper");
        assert!(s.painting_theme(true, &Themes::default()).is_none());
    }

    /// Every quality must ask for something, and they must differ — a quality setting that does
    /// not change the request has no effect.
    #[test]
    fn each_quality_selects_a_different_format() {
        let formats: std::collections::HashSet<&str> =
            Quality::ALL.iter().map(|q| q.format()).collect();
        assert_eq!(formats.len(), 3);
        assert!(Quality::ALL.iter().all(|q| !q.format().is_empty()));
    }

    /// `Off` is not "a little bit less opaque" — it must be exactly opaque, so the default
    /// build paints exactly what it painted before opacity existed.
    #[test]
    fn opacity_off_is_fully_opaque() {
        assert_eq!(Opacity::OFF.gutter(), 1.0);
        assert_eq!(Opacity::OFF.card(), 1.0);
    }

    /// Every step of the slider lets through at least as much as the last, and a card is never
    /// *less* opaque than the gutter beneath it — the arithmetic in `Palette::with_opacity`
    /// divides by `1 - gutter` and would go negative if it ever were.
    #[test]
    fn the_opacity_curve_is_ordered() {
        let mut last_gutter = 2.0;
        let mut last_card = 2.0;
        for i in 0..=(Opacity::MAX * 100.0) as u32 {
            let o = Opacity::new(i as f32 / 100.0);
            assert!(o.gutter() <= last_gutter, "{o:?} gutter rose");
            assert!(o.card() <= last_card, "{o:?} card rose");
            assert!(
                o.gutter() <= o.card(),
                "{o:?}: the gutter must read as behind the card"
            );
            last_gutter = o.gutter();
            last_card = o.card();
        }
    }

    /// The old "strong" is no longer the end of the slider: the window keeps opening up past it
    /// — and a file that names "strong" still lands exactly where it always did.
    #[test]
    fn the_slider_reaches_past_the_old_strong() {
        let old_strong = Opacity::new(1.0);
        assert_eq!(old_strong.gutter(), 0.58);
        assert_eq!(old_strong.card(), 0.74);
        assert_eq!(Opacity::parse("strong"), Some(old_strong));

        let max = Opacity::new(Opacity::MAX);
        assert_eq!(max.gutter(), 0.30);
        assert_eq!(max.card(), 0.50);
        assert!(
            max.gutter() < old_strong.gutter() && max.card() < old_strong.card(),
            "the new maximum must let through more than the old one"
        );
    }

    #[test]
    fn opacity_parses_number_or_name_case_insensitively() {
        assert_eq!(Opacity::parse("strong"), Some(Opacity::new(1.0)));
        assert_eq!(Opacity::parse("STRONG"), Some(Opacity::new(1.0)));
        assert_eq!(Opacity::parse("1.25"), Some(Opacity::new(Opacity::MAX)));
        assert_eq!(Opacity::parse("off"), Some(Opacity::OFF));
        assert_eq!(Opacity::parse("0.4"), Some(Opacity::new(0.4)));
        assert_eq!(Opacity::parse("nonsense"), None);
    }
}
