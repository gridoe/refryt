//! Following the desktop's own toolkit theme, as Chrome's "GTK" and "Qt" appearance options do.
//!
//! Two entries at the top of the Theme list on Linux, each a theme built from the desktop's
//! colours rather than read from a file:
//!
//! - **Qt** reads KDE's colour scheme from `kdeglobals`, which is where Qt apps on Plasma get
//!   their palette. A scheme section missing there comes from the scheme's own `.colors` file.
//! - **GTK** asks GTK 3 itself: a child of this binary (`--gtk-colours`, like the sign-in
//!   window's child, since GTK cannot share the main thread with the app's window) starts GTK,
//!   looks up the theme's named colours (`theme_bg_color`, `theme_selected_bg_color`, …) once
//!   with the dark variant asked for and once without, prints them and exits.
//!
//! Either becomes ordinary theme CSS run through [`super::custom::parse`], so everything a file
//! leaves out is derived the same way, and low contrast is a warning under the dropdown rather
//! than a refusal: the colours are the desktop's, not refryt's to fix. Both are read again when
//! the window is focused, so changing the desktop's theme repaints the app on the way back.

use std::collections::HashMap;

use egui::Color32;

use super::custom::{CustomTheme, parse};

pub const GTK: &str = "GTK";
pub const QT: &str = "Qt";
/// Both, in the order the Theme list shows them.
pub const GTK_AND_QT: [&str; 2] = [GTK, QT];

/// Whether a theme name is one of these, rather than a file.
pub fn is_system(name: &str) -> bool {
    name == GTK || name == QT
}

/// The argument that makes the binary the GTK child.
pub const GTK_ARG: &str = "--gtk-colours";

/// The GTK colours a theme is built from, by GTK's own names.
const GTK_NAMES: [&str; 9] = [
    "theme_bg_color",
    "theme_base_color",
    "theme_fg_color",
    "theme_text_color",
    "theme_selected_bg_color",
    "theme_selected_fg_color",
    "borders",
    "error_color",
    "warning_color",
];

/// The slots one theme fills, by refryt's variable names. `base`, `text` and `accent` are
/// required; the rest fall back to what the parser derives.
#[derive(Clone, Debug, Default, PartialEq)]
struct Slots {
    window: Option<Color32>,
    view: Option<Color32>,
    text: Option<Color32>,
    accent: Option<Color32>,
    on_accent: Option<Color32>,
    surface: Option<Color32>,
    subtext: Option<Color32>,
    outline: Option<Color32>,
    danger: Option<Color32>,
    warning: Option<Color32>,
}

/// The theme these slots make, named `name`. The lighter of the window and view colours is the
/// cards and the darker the gap behind them, which is how both toolkits' default themes read in
/// refryt's layout; when they are the same, the gap is derived. Text on the accent is the
/// toolkit's own when it reads at 3:1, else black or white, whichever reads better.
fn theme(name: &str, s: &Slots) -> Option<CustomTheme> {
    let (window, view) = (s.window, s.view);
    let (base, gutter) = match (window, view) {
        (Some(w), Some(v)) if w != v => {
            if super::ramp::luminance(v) >= super::ramp::luminance(w) {
                (v, Some(w))
            } else {
                (w, Some(v))
            }
        }
        (Some(c), _) | (None, Some(c)) => (c, None),
        (None, None) => return None,
    };
    let text = s.text?;
    let accent = s.accent?;
    let on_accent = s
        .on_accent
        .filter(|c| super::ramp::contrast(*c, accent) >= 3.0)
        .unwrap_or_else(|| {
            if super::ramp::contrast(Color32::BLACK, accent)
                >= super::ramp::contrast(Color32::WHITE, accent)
            {
                Color32::BLACK
            } else {
                Color32::WHITE
            }
        });
    let mut css = String::from(":root {\n");
    let mut put = |var: &str, c: Option<Color32>| {
        if let Some(c) = c {
            css.push_str(&format!(
                "  --{var}: #{:02x}{:02x}{:02x};\n",
                c.r(),
                c.g(),
                c.b()
            ));
        }
    };
    put("base", Some(base));
    put("text", Some(text));
    put("accent", Some(accent));
    put("on-accent", Some(on_accent));
    put("gutter", gutter);
    put("surface", s.surface.filter(|c| *c != base));
    put("subtext", s.subtext.filter(|c| *c != text));
    put("outline", s.outline);
    put("danger", s.danger);
    put("warning", s.warning);
    css.push_str("}\n");
    parse(name, &css).ok()
}

/// `r,g,b` or `r,g,b,a` as KDE writes a colour.
fn kde_colour(value: &str) -> Option<Color32> {
    let parts: Vec<u8> = value
        .split(',')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    match parts[..] {
        [r, g, b] | [r, g, b, _] => Some(Color32::from_rgb(r, g, b)),
        _ => None,
    }
}

type Ini = HashMap<String, HashMap<String, String>>;

/// An INI file as sections of keys. Later keys win, as KDE reads them.
fn ini(text: &str) -> Ini {
    let mut out = Ini::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_owned();
        } else if let Some((k, v)) = line.split_once('=')
            && !line.starts_with('#')
        {
            out.entry(section.clone())
                .or_default()
                .insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    out
}

/// The Qt theme from `kdeglobals`, with `scheme` (the `.colors` file `[General] ColorScheme`
/// names) filling whatever colour `kdeglobals` itself leaves out.
fn from_kde(kdeglobals: &str, scheme: Option<&str>) -> Option<CustomTheme> {
    let mut colours = scheme.map(ini).unwrap_or_default();
    for (section, keys) in ini(kdeglobals) {
        colours.entry(section).or_default().extend(keys);
    }
    let get = |section: &str, key: &str| {
        colours
            .get(&format!("Colors:{section}"))
            .and_then(|s| s.get(key))
            .and_then(|v| kde_colour(v))
    };
    theme(
        QT,
        &Slots {
            window: get("Window", "BackgroundNormal"),
            view: get("View", "BackgroundNormal"),
            text: get("View", "ForegroundNormal").or(get("Window", "ForegroundNormal")),
            accent: get("Selection", "BackgroundNormal"),
            on_accent: get("Selection", "ForegroundNormal"),
            surface: get("Button", "BackgroundNormal"),
            subtext: get("View", "ForegroundInactive"),
            outline: None,
            danger: get("View", "ForegroundNegative"),
            warning: get("View", "ForegroundNeutral"),
        },
    )
}

/// The GTK themes from the child's answer: one per variant it reported, the dark one first, and
/// only one when the theme has no separate dark variant.
fn from_gtk(answer: &str) -> Vec<CustomTheme> {
    let Ok(variants) = serde_json::from_str::<HashMap<String, HashMap<String, String>>>(answer)
    else {
        return Vec::new();
    };
    let mut out: Vec<CustomTheme> = Vec::new();
    for variant in ["dark", "light"] {
        let Some(c) = variants.get(variant) else {
            continue;
        };
        let colour = |name: &str| {
            c.get(name)
                .and_then(|v| super::custom::parse_colour(v))
                .map(super::custom::hex)
        };
        if let Some(t) = theme(
            GTK,
            &Slots {
                window: colour("theme_bg_color"),
                view: colour("theme_base_color"),
                text: colour("theme_text_color").or(colour("theme_fg_color")),
                accent: colour("theme_selected_bg_color"),
                on_accent: colour("theme_selected_fg_color"),
                surface: None,
                subtext: None,
                outline: colour("borders"),
                danger: colour("error_color"),
                warning: colour("warning_color"),
            },
        ) && !out.iter().any(|o| o == &t)
        {
            out.push(t);
        }
    }
    out
}

/// The Qt theme as KDE has it now, if this is a KDE desktop.
#[cfg(target_os = "linux")]
pub fn qt() -> Option<CustomTheme> {
    let config = directories::BaseDirs::new()?;
    let kdeglobals = std::fs::read_to_string(config.config_dir().join("kdeglobals")).ok()?;
    let name = ini(&kdeglobals)
        .get("General")
        .and_then(|g| g.get("ColorScheme"))
        .cloned();
    let scheme = name.and_then(|name| {
        let file = format!("{name}.colors");
        let mut places = vec![config.data_local_dir().join("color-schemes").join(&file)];
        let data_dirs =
            std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        places.extend(
            data_dirs
                .split(':')
                .filter(|d| !d.is_empty())
                .map(|d| std::path::Path::new(d).join("color-schemes").join(&file)),
        );
        places
            .into_iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
    });
    from_kde(&kdeglobals, scheme.as_deref())
}

#[cfg(not(target_os = "linux"))]
pub fn qt() -> Option<CustomTheme> {
    None
}

/// Run the GTK child and read its answer: the GTK themes, or none when GTK could not start
/// (no display, no GTK) within a few seconds.
#[cfg(target_os = "linux")]
pub fn gtk() -> Vec<CustomTheme> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Ok(mut child) = Command::new(exe)
        .arg(GTK_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("the GTK colour lookup did not answer in time");
                return Vec::new();
            }
        }
    }
    let mut answer = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut answer);
    }
    from_gtk(&answer)
}

#[cfg(not(target_os = "linux"))]
pub fn gtk() -> Vec<CustomTheme> {
    Vec::new()
}

/// The child's half: start GTK, look the colours up with the dark variant asked for and
/// without, and print them as JSON on one line. `false` when GTK could not start.
#[cfg(target_os = "linux")]
pub fn gtk_child() -> bool {
    use gtk::prelude::*;

    if gtk::init().is_err() {
        return false;
    }
    let Some(settings) = gtk::Settings::default() else {
        return false;
    };
    let mut answer: HashMap<&str, HashMap<&str, String>> = HashMap::new();
    for (variant, dark) in [("dark", true), ("light", false)] {
        settings.set_gtk_application_prefer_dark_theme(dark);
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
        // A fresh widget, so its style is the variant just asked for.
        let probe = gtk::Window::new(gtk::WindowType::Toplevel);
        let style = probe.style_context();
        let mut colours = HashMap::new();
        for name in GTK_NAMES {
            if let Some(c) = style.lookup_color(name) {
                let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                colours.insert(
                    name,
                    format!(
                        "#{:02x}{:02x}{:02x}",
                        byte(c.red()),
                        byte(c.green()),
                        byte(c.blue())
                    ),
                );
            }
        }
        answer.insert(variant, colours);
        // SAFETY: the probe was never shown and nothing else holds it.
        unsafe { probe.destroy() };
    }
    match serde_json::to_string(&answer) {
        Ok(line) => {
            println!("{line}");
            true
        }
        Err(_) => false,
    }
}

/// Reads the GTK colours off the UI thread: the child takes a moment, and the app keeps
/// painting meanwhile. The answer is picked up by [`GtkWatch::take`].
#[derive(Default)]
pub struct GtkWatch {
    slot: std::sync::Arc<std::sync::Mutex<Option<Vec<CustomTheme>>>>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl GtkWatch {
    /// Ask again, unless an answer is already on its way.
    pub fn refresh(&self, ctx: &egui::Context) {
        if !cfg!(target_os = "linux")
            || self.running.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let slot = self.slot.clone();
        let running = self.running.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let themes = gtk();
            if let Ok(mut s) = slot.lock() {
                *s = Some(themes);
            }
            running.store(false, std::sync::atomic::Ordering::Release);
            ctx.request_repaint();
        });
    }

    /// The answer that came in since the last call, if one did.
    pub fn take(&self) -> Option<Vec<CustomTheme>> {
        self.slot.lock().ok()?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BREEZE_DARK: &str = "[Colors:Window]\nBackgroundNormal=32,35,38\nForegroundNormal=252,252,252\n\
        [Colors:View]\nBackgroundNormal=20,22,24\nForegroundNormal=252,252,252\nForegroundInactive=161,169,177\n\
        ForegroundNegative=218,68,83\nForegroundNeutral=246,116,0\n\
        [Colors:Selection]\nBackgroundNormal=61,174,233\nForegroundNormal=252,252,252\n\
        [Colors:Button]\nBackgroundNormal=41,44,48\n";

    #[test]
    fn kde_colours_make_a_qt_theme() {
        let t = from_kde(BREEZE_DARK, None).unwrap();
        assert_eq!(t.name, QT);
        assert!(t.dark);
        let p = t.palette();
        assert_eq!(
            p.panel,
            Color32::from_rgb(32, 35, 38),
            "the lighter one is the cards"
        );
        assert_eq!(
            p.window,
            Color32::from_rgb(20, 22, 24),
            "the darker one the gap"
        );
        assert_eq!(p.accent, Color32::from_rgb(61, 174, 233));
    }

    /// `kdeglobals` that only names its scheme gets the colours from the scheme's file, and
    /// its own keys win over the file's.
    #[test]
    fn a_scheme_file_fills_what_kdeglobals_leaves_out() {
        let kdeglobals =
            "[General]\nColorScheme=BreezeDark\n[Colors:Selection]\nBackgroundNormal=200,0,100\n";
        let t = from_kde(kdeglobals, Some(BREEZE_DARK)).unwrap();
        assert_eq!(t.palette().accent, Color32::from_rgb(200, 0, 100));
        assert!(from_kde("[General]\nColorScheme=Gone\n", None).is_none());
    }

    #[test]
    fn unreadable_text_on_the_accent_is_replaced() {
        // KDE's own white on Breeze blue is 2.4:1.
        let t = from_kde(BREEZE_DARK, None).unwrap();
        assert!(super::super::ramp::contrast(t.palette().on_accent, t.palette().accent) >= 3.0);
    }

    #[test]
    fn gtk_variants_become_themes_and_a_missing_dark_one_is_not_repeated() {
        let adwaita = r##"{"dark":{"theme_bg_color":"#353535","theme_base_color":"#2d2d2d","theme_fg_color":"#eeeeec","theme_selected_bg_color":"#15539e","theme_selected_fg_color":"#ffffff"},
            "light":{"theme_bg_color":"#f6f5f4","theme_base_color":"#ffffff","theme_fg_color":"#2e3436","theme_selected_bg_color":"#3584e4","theme_selected_fg_color":"#ffffff"}}"##;
        let themes = from_gtk(adwaita);
        assert_eq!(themes.len(), 2);
        assert!(themes[0].dark && !themes[1].dark);

        let one = r##"{"dark":{"theme_bg_color":"#f6f5f4","theme_base_color":"#ffffff","theme_fg_color":"#2e3436","theme_selected_bg_color":"#3584e4"},
            "light":{"theme_bg_color":"#f6f5f4","theme_base_color":"#ffffff","theme_fg_color":"#2e3436","theme_selected_bg_color":"#3584e4"}}"##;
        assert_eq!(from_gtk(one).len(), 1);
        assert!(from_gtk("not json").is_empty());
    }
}
