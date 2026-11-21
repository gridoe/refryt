//! Custom themes: `.css` files of colour variables in the config directory's `themes/` folder.
//!
//! Not a CSS engine — the app paints itself, so there are no selectors to match. A theme file is
//! a list of custom properties in CSS syntax, read into the same [`Ramp`] the built-in families
//! are written as, and mapped through the same derivation: set `--base`, `--text` and `--accent`
//! and the other thirteen slots are filled in the way every published scheme's gaps are.
//!
//! ```css
//! /* ~/.config/refryt/themes/midnight.css */
//! :root {
//!   color-scheme: dark;
//!   --base: #16161e;
//!   --text: #c0caf5;
//!   --accent: #ff9e64;
//! }
//! ```
//!
//! The file name is the theme's name in Settings → Theme, listed while the app is in the mode
//! its `color-scheme` names. Files are re-read whenever one changes on disk, so a theme can be
//! edited with the app open and watched as it repaints.
//!
//! The themes that ship with the app ([`BUILTIN`]) are written into the folder the first time
//! there is no folder, and are ordinary files from then on: deleting one deletes it, editing one
//! edits it. A built-in added in a later release is written once into an existing folder (see
//! [`seed`]). Only when nothing usable is left does the compiled-in [`fallback`] paint.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::Color32;

use super::Palette;
use super::ramp::{RAMP, Ramp, contrast, luminance};

/// One theme file, parsed.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomTheme {
    /// The file stem: what Settings lists and what `settings.json` stores.
    pub name: String,
    /// The polarity: `color-scheme` when the file says, else read off `--base`.
    pub dark: bool,
    palette: Palette,
    /// Legible-but-worrying findings — low contrast, unknown properties. The theme still
    /// applies; Settings shows these under the dropdown.
    pub warnings: Vec<String>,
}

impl CustomTheme {
    /// The palette this theme paints, before opacity.
    pub fn palette(&self) -> Palette {
        self.palette
    }
}

/// Every theme in the folder, and the files that could not be read as one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Themes {
    pub themes: Vec<CustomTheme>,
    /// `(file name, reason)` for each file that failed to parse.
    pub errors: Vec<(String, String)>,
    /// The desktop's GTK and Qt themes ([`super::system`]), kept apart from the files: they
    /// are read from the desktop, not the folder, and offered in both modes.
    pub system: Vec<CustomTheme>,
}

impl Themes {
    /// A desktop theme by name: the variant of this polarity if there is one, else the only
    /// one, since following the desktop means its colours whatever the mode.
    pub fn system_theme(&self, name: &str, dark: bool) -> Option<&CustomTheme> {
        let named = || self.system.iter().filter(move |t| t.name == name);
        named().find(|t| t.dark == dark).or_else(|| named().next())
    }

    /// Replace the desktop themes called `name` with `fresh`.
    pub fn set_system(&mut self, name: &str, fresh: Vec<CustomTheme>) {
        self.system.retain(|t| t.name != name);
        self.system.extend(fresh);
    }

    pub fn get(&self, name: &str) -> Option<&CustomTheme> {
        self.themes.iter().find(|t| t.name == name)
    }

    /// A theme by name as someone types it on the command line: case, spaces, hyphens,
    /// underscores and accents do not matter, so `catppuccin-frappe` finds "Catppuccin Frappé".
    pub fn find_loose(&self, query: &str) -> Option<&CustomTheme> {
        let want = loose(query);
        self.themes.iter().find(|t| loose(&t.name) == want)
    }
}

fn loose(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| match c {
            'é' | 'É' | 'è' | 'ê' => 'e',
            'á' | 'à' | 'â' => 'a',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Where theme files live: `<config dir>/themes`.
pub fn dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "refryt").map(|d| d.config_dir().join("themes"))
}

/// Read every `*.css` file in `dir`, sorted by name. A missing folder is simply no themes.
pub fn load_from(dir: &Path) -> Themes {
    let mut out = Themes::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("css")))
        .collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(src) => match parse(&name, &src) {
                Ok(theme) => out.themes.push(theme),
                Err(e) => out.errors.push((file, e)),
            },
            Err(e) => out.errors.push((file, e.to_string())),
        }
    }
    out
}

/// [`load_from`] on the real folder.
pub fn load() -> Themes {
    dir().map(|d| load_from(&d)).unwrap_or_default()
}

/// The themes that ship with the app, as `(name, file contents)`. Written into the themes
/// folder the first time the app finds no folder at all; from then on the folder is the only
/// source. Deleting a file there removes the theme, editing one edits it, and nothing here
/// puts it back.
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "Catppuccin Frappé",
        include_str!("../../themes/Catppuccin Frappé.css"),
    ),
    (
        "Catppuccin Latte",
        include_str!("../../themes/Catppuccin Latte.css"),
    ),
    (
        "Catppuccin Macchiato",
        include_str!("../../themes/Catppuccin Macchiato.css"),
    ),
    (
        "Catppuccin Mocha",
        include_str!("../../themes/Catppuccin Mocha.css"),
    ),
    (
        "Classic Dark",
        include_str!("../../themes/Classic Dark.css"),
    ),
    (
        "Classic Light",
        include_str!("../../themes/Classic Light.css"),
    ),
    ("Dracula", include_str!("../../themes/Dracula.css")),
    (
        "Dracula Alucard",
        include_str!("../../themes/Dracula Alucard.css"),
    ),
    (
        "Everforest Dark",
        include_str!("../../themes/Everforest Dark.css"),
    ),
    (
        "Everforest Light",
        include_str!("../../themes/Everforest Light.css"),
    ),
    (
        "Gruvbox Dark",
        include_str!("../../themes/Gruvbox Dark.css"),
    ),
    (
        "Gruvbox Light",
        include_str!("../../themes/Gruvbox Light.css"),
    ),
    (
        "Nord Polar Night",
        include_str!("../../themes/Nord Polar Night.css"),
    ),
    (
        "Nord Snow Storm",
        include_str!("../../themes/Nord Snow Storm.css"),
    ),
    ("Rosé Pine", include_str!("../../themes/Rosé Pine.css")),
    (
        "Rosé Pine Dawn",
        include_str!("../../themes/Rosé Pine Dawn.css"),
    ),
    (
        "Rosé Pine Moon",
        include_str!("../../themes/Rosé Pine Moon.css"),
    ),
    (
        "Solarized Dark",
        include_str!("../../themes/Solarized Dark.css"),
    ),
    (
        "Solarized Light",
        include_str!("../../themes/Solarized Light.css"),
    ),
    ("Tokyo Night", include_str!("../../themes/Tokyo Night.css")),
    (
        "Tokyo Night Day",
        include_str!("../../themes/Tokyo Night Day.css"),
    ),
    (
        "Tokyo Night Storm",
        include_str!("../../themes/Tokyo Night Storm.css"),
    ),
    ("Spotify", include_str!("../../themes/Spotify.css")),
    ("macOS Dark", include_str!("../../themes/macOS Dark.css")),
    ("macOS Light", include_str!("../../themes/macOS Light.css")),
    (
        "Windows Dark",
        include_str!("../../themes/Windows Dark.css"),
    ),
    (
        "Windows Light",
        include_str!("../../themes/Windows Light.css"),
    ),
    ("Breeze Dark", include_str!("../../themes/Breeze Dark.css")),
    (
        "Breeze Light",
        include_str!("../../themes/Breeze Light.css"),
    ),
];

/// The themes a fresh install starts on, `(dark, light)`: the system's own colours on macOS and
/// Windows, Classic elsewhere. Only a fresh install: a settings file that already exists keeps
/// what it names, so nobody's look changes on an update.
pub const fn platform_default() -> (&'static str, &'static str) {
    if cfg!(target_os = "macos") {
        ("macOS Dark", "macOS Light")
    } else if cfg!(target_os = "windows") {
        ("Windows Dark", "Windows Light")
    } else {
        ("Classic Dark", "Classic Light")
    }
}

/// Built-in themes added after the first release that shipped themes. A folder made before the
/// app kept its [`OFFERED`] record was given every other built-in, so these are the ones it still
/// needs offering.
const ADDED_LATER: &[&str] = &[
    "Spotify",
    "macOS Dark",
    "macOS Light",
    "Windows Dark",
    "Windows Light",
    "Breeze Dark",
    "Breeze Light",
];

/// The file in the themes folder that names every built-in theme already written there once. A
/// newer release writes only the built-ins missing from it, so a new theme reaches an existing
/// install and a theme someone deleted stays deleted. Not a `.css` file, so never a theme itself.
const OFFERED: &str = ".offered";

/// Write the built-in themes into the themes folder: all of them the first time there is no
/// folder, and afterwards only a built-in this folder has never been given (a theme added in a
/// newer release), as [`OFFERED`] records. A theme someone deleted is never written back, and an
/// empty folder stays empty unless a new theme arrives.
pub fn seed() {
    let Some(dir) = dir() else { return };
    if let Err(e) = seed_into(&dir) {
        tracing::warn!(%e, "could not write the built-in themes");
    }
}

fn seed_into(dir: &Path) -> std::io::Result<()> {
    let record = dir.join(OFFERED);
    let mut offered: Vec<String> = if !dir.exists() {
        Vec::new()
    } else if let Ok(text) = std::fs::read_to_string(&record) {
        text.lines().map(str::to_owned).collect()
    } else {
        // A folder from before the record: it was given every built-in of its day.
        BUILTIN
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !ADDED_LATER.contains(name))
            .map(str::to_owned)
            .collect()
    };
    let fresh: Vec<_> = BUILTIN
        .iter()
        .filter(|(name, _)| !offered.iter().any(|o| o == name))
        .collect();
    if fresh.is_empty() && record.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    for (name, css) in fresh {
        let path = dir.join(format!("{name}.css"));
        // A file of that name already there is the user's own: left alone.
        if !path.exists() {
            std::fs::write(&path, css)?;
        }
        offered.push((*name).to_owned());
    }
    std::fs::write(&record, offered.join("\n") + "\n")
}

/// The palette to paint when the folder holds no usable theme of the polarity on screen:
/// Classic, compiled in so the app can always draw. Not listed anywhere; Settings says when it
/// is standing in.
pub fn fallback(dark: bool) -> CustomTheme {
    let name = if dark {
        "Classic Dark"
    } else {
        "Classic Light"
    };
    let css = BUILTIN
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, css)| *css)
        .expect("Classic ships with the app");
    parse(name, css).expect("the built-in Classic parses")
}

/// Parse a theme file. `Err` names the line and what was wrong with it.
pub fn parse(name: &str, src: &str) -> Result<CustomTheme, String> {
    let src = strip_comments(src);
    let mut ramp = Ramp { ..RAMP };
    let (mut base, mut text, mut accent) = (None, None, None);
    let (mut on_accent, mut red, mut yellow) = (None, None, None);
    let mut polarity = None;
    let mut warnings = Vec::new();

    for (line_no, decl) in declarations(&src) {
        let Some((key, value)) = decl.split_once(':') else {
            return Err(format!(
                "line {line_no}: expected `name: value`, found `{decl}`"
            ));
        };
        let (key, value) = (
            key.trim(),
            value.trim().trim_end_matches("!important").trim(),
        );
        if key == "color-scheme" {
            polarity = match value {
                "dark" => Some(true),
                "light" => Some(false),
                other => {
                    return Err(format!(
                        "line {line_no}: color-scheme is `dark` or `light`, not `{other}`"
                    ));
                }
            };
            continue;
        }
        let Some(var) = key.strip_prefix("--") else {
            warnings.push(format!(
                "line {line_no}: `{key}` ignored — only --variables apply"
            ));
            continue;
        };
        let colour = parse_colour(value)
            .ok_or_else(|| format!("line {line_no}: `{value}` is not a colour"))?;
        let slot: &mut Option<u32> = match var {
            "base" => &mut base,
            "text" => &mut text,
            "accent" => &mut accent,
            "on-accent" => &mut on_accent,
            "danger" => &mut red,
            "warning" => &mut yellow,
            "gutter" => &mut ramp.crust,
            "popup" => &mut ramp.mantle,
            "surface" => &mut ramp.surface,
            "surface-hover" => &mut ramp.surface_hi,
            "surface-active" => &mut ramp.surface_hi2,
            "outline" => &mut ramp.outline,
            "subtext" => &mut ramp.subtext,
            "muted" => &mut ramp.muted,
            "accent-hover" => &mut ramp.accent_hi,
            other => {
                warnings.push(format!(
                    "line {line_no}: unknown variable `--{other}` ignored"
                ));
                continue;
            }
        };
        *slot = Some(colour);
    }

    let missing: Vec<&str> = [("--base", base), ("--text", text), ("--accent", accent)]
        .into_iter()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k)
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing {}", missing.join(", ")));
    }
    let (base, text, accent) = (base.unwrap_or(0), text.unwrap_or(0), accent.unwrap_or(0));
    let dark = polarity.unwrap_or_else(|| luminance(hex(base)) < 0.4);
    ramp.base = base;
    ramp.text = text;
    ramp.accent = accent;
    // Unlike a published family, a hand-written theme is allowed to leave these out: black or
    // white, whichever reads better on the accent, and a red and amber that hold up on either
    // polarity.
    ramp.on_accent = on_accent.unwrap_or_else(|| {
        if contrast(Color32::BLACK, hex(accent)) >= contrast(Color32::WHITE, hex(accent)) {
            0x000000
        } else {
            0xffffff
        }
    });
    ramp.red = red.unwrap_or(0xe5484d);
    ramp.yellow = yellow.unwrap_or(0xe5a50a);

    let palette = ramp.palette(dark);
    for (what, fg, bg, min) in [
        ("--text on --base", palette.text, palette.panel, 4.5),
        (
            "--on-accent on --accent",
            palette.on_accent,
            palette.accent,
            3.0,
        ),
    ] {
        let ratio = contrast(fg, bg);
        if ratio < min {
            warnings.push(format!(
                "{what} is {ratio:.1}:1, below {min}:1 — hard to read"
            ));
        }
    }
    Ok(CustomTheme {
        name: name.to_owned(),
        dark,
        palette,
        warnings,
    })
}

pub(super) fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// `/* … */` removed, newlines inside a comment kept so line numbers still point at the file.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => {
                let comment = &rest[start..start + 2 + end + 2];
                out.extend(comment.chars().filter(|&c| c == '\n'));
                rest = &rest[start + 2 + end + 2..];
            }
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `;`-separated declarations, with the line each starts on. Selectors and braces are
/// dropped: `:root { … }`, `* { … }` or no block at all all read the same, since there is only
/// one thing a declaration here can apply to.
fn declarations(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut line = 1;
    let mut current = String::new();
    let mut start_line = 1;
    for c in src.chars() {
        match c {
            '{' => current.clear(),
            ';' | '}' => {
                let decl = current.trim();
                if !decl.is_empty() {
                    out.push((start_line, decl.to_owned()));
                }
                current.clear();
            }
            _ => {
                if current.trim().is_empty() && !c.is_whitespace() {
                    start_line = line;
                }
                current.push(c);
            }
        }
        if c == '\n' {
            line += 1;
        }
    }
    // A last declaration with no `;` still counts — unless it is a selector waiting for its
    // block, which a colon-free line is.
    let tail = current.trim();
    if tail.contains(':') && !tail.starts_with(':') {
        out.push((start_line, tail.to_owned()));
    }
    out
}

/// `#rgb`, `#rrggbb`, or `rgb(r, g, b)` / `rgb(r g b)`.
pub(super) fn parse_colour(value: &str) -> Option<u32> {
    let v = value.trim();
    if let Some(h) = v.strip_prefix('#') {
        let h: String = match h.len() {
            3 => h.chars().flat_map(|c| [c, c]).collect(),
            6 => h.to_owned(),
            _ => return None,
        };
        return u32::from_str_radix(&h, 16).ok();
    }
    let inner = v.strip_prefix("rgb(")?.strip_suffix(')')?;
    let parts: Vec<u32> = inner
        .split([',', ' '])
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().parse::<u32>().ok().filter(|&n| n <= 255))
        .collect::<Option<_>>()?;
    match parts[..] {
        [r, g, b] => Some((r << 16) | (g << 8) | b),
        _ => None,
    }
}

/// Notices edits to the themes folder: it remembers the folder's `.css` files and their
/// modification times, and [`Watcher::check`] compares them. There is no thread and no timer —
/// it used to poll once a second for as long as the app ran, a wakeup a second forever to catch
/// a change that happens when someone edits a theme. The app checks when its window gains focus
/// (the way back from an editor), once at startup, and on entering Settings, where the folder is
/// offered.
pub struct Watcher {
    dir: Option<PathBuf>,
    last: Vec<(PathBuf, Option<SystemTime>)>,
    changed: bool,
}

impl Watcher {
    /// Remember the folder as it is now.
    pub fn new() -> Self {
        Self::at(dir())
    }

    fn at(dir: Option<PathBuf>) -> Self {
        let last = dir.as_deref().map(snapshot).unwrap_or_default();
        Self {
            dir,
            last,
            changed: false,
        }
    }

    /// Compare the folder with what was last seen, and remember a difference until it is taken.
    pub fn check(&mut self) {
        let Some(dir) = &self.dir else { return };
        let now = snapshot(dir);
        if now != self.last {
            self.last = now;
            self.changed = true;
        }
    }

    /// Whether the folder changed since this was last asked, clearing the answer.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

impl Default for Watcher {
    fn default() -> Self {
        Self::new()
    }
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("css"))
        })
        .map(|e| (e.path(), e.metadata().and_then(|m| m.modified()).ok()))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    /// The watcher sees a theme file added, edited and removed, and only when asked: it keeps
    /// no thread of its own, and a quiet folder reports nothing.
    #[test]
    fn the_watcher_sees_changes_only_when_checked() {
        let dir = std::env::temp_dir().join(format!("refryt-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut watcher = Watcher::at(Some(dir.clone()));

        watcher.check();
        assert!(!watcher.take_changed(), "an untouched folder is quiet");

        std::fs::write(dir.join("mine.css"), "a: b").unwrap();
        assert!(
            !watcher.take_changed(),
            "nothing is noticed until it is checked"
        );
        watcher.check();
        assert!(watcher.take_changed(), "a new theme file is seen");
        assert!(!watcher.take_changed(), "and reported once");

        std::fs::write(dir.join("notes.txt"), "not a theme").unwrap();
        watcher.check();
        assert!(!watcher.take_changed(), "other files are not themes");

        std::fs::remove_file(dir.join("mine.css")).unwrap();
        watcher.check();
        assert!(watcher.take_changed(), "a removed theme file is seen");

        assert!(
            !Watcher::at(None).take_changed(),
            "no folder, nothing to watch"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    #[test]
    fn the_three_required_colours_make_a_whole_palette() {
        let t = parse(
            "midnight",
            ":root {\n  color-scheme: dark;\n  --base: #16161e;\n  --text: #c0caf5;\n  --accent: #ff9e64;\n}\n",
        )
        .expect("parses");
        let p = t.palette();
        assert!(t.dark);
        assert_eq!(p.panel, Color32::from_rgb(0x16, 0x16, 0x1e));
        assert_eq!(p.accent, Color32::from_rgb(0xff, 0x9e, 0x64));
        // Derived, not left black: the gutter sits behind the card, popups between.
        assert_ne!(p.window, p.panel);
        assert!(luminance(p.window) < luminance(p.panel));
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
    }

    #[test]
    fn a_stated_slot_wins_over_its_derivation() {
        let t = parse(
            "x",
            "--base:#ffffff; --text:#111; --accent: rgb(0, 90, 200); --gutter: #eeeeee;",
        )
        .expect("parses");
        assert!(!t.dark, "a white base guesses light");
        assert_eq!(t.palette().window, Color32::from_rgb(0xee, 0xee, 0xee));
        // White on that blue reads better than black.
        assert_eq!(t.palette().on_accent, Color32::WHITE);
    }

    #[test]
    fn a_missing_required_colour_is_an_error_naming_it() {
        let e = parse("x", ":root { --base: #000; --text: #fff; }").unwrap_err();
        assert_eq!(e, "missing --accent");
    }

    #[test]
    fn a_bad_value_points_at_its_line() {
        let e = parse(
            "x",
            "/* header\n   comment */\n:root {\n  --base: tomato;\n}",
        )
        .unwrap_err();
        assert_eq!(e, "line 4: `tomato` is not a colour");
    }

    #[test]
    fn unknown_variables_and_unreadable_text_warn_but_still_apply() {
        let t = parse(
            "x",
            "--base:#222; --text:#333; --accent:#f00; --sparkle:#fff;",
        )
        .expect("parses");
        assert_eq!(t.warnings.len(), 2, "{:?}", t.warnings);
        assert!(t.warnings[0].contains("--sparkle"));
        assert!(t.warnings[1].contains("--text on --base"));
    }

    #[test]
    fn colours_parse_in_every_accepted_form() {
        assert_eq!(parse_colour("#abc"), Some(0xaabbcc));
        assert_eq!(parse_colour("#A1B2C3"), Some(0xa1b2c3));
        assert_eq!(parse_colour("rgb(1, 2, 3)"), Some(0x010203));
        assert_eq!(parse_colour("rgb(1 2 3)"), Some(0x010203));
        assert_eq!(parse_colour("rgb(256, 0, 0)"), None);
        assert_eq!(parse_colour("#abcd"), None);
    }

    fn builtins() -> Vec<CustomTheme> {
        BUILTIN
            .iter()
            .map(|(name, css)| {
                let t = parse(name, css).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(t.warnings.is_empty(), "{name}: {:?}", t.warnings);
                t
            })
            .collect()
    }

    /// Twenty-two themes copied by hand from nine upstreams is twenty-two chances to pick a
    /// text colour that cannot be read on its own background.
    #[test]
    fn every_builtin_theme_is_readable() {
        for t in builtins() {
            let p = t.palette();
            let name = &t.name;
            let check = |what: &str, fg: Color32, bg: Color32, min: f32| {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= min,
                    "{name}: {what} is {ratio:.2}:1, under {min}:1 ({fg:?} on {bg:?})"
                );
            };
            check("text on the card", p.text, p.panel, 4.5);
            check("text on the gutter", p.text, p.window, 4.5);
            check("text on a surface", p.text, p.surface, 4.5);
            // Second-rank text is small prose: AA, with room for the tone under it.
            check(
                "secondary on the card",
                p.secondary,
                p.panel,
                super::super::ramp::SECONDARY_MIN_CONTRAST.min(contrast(p.text, p.panel)),
            );
            // The player bar's song line and times sit on the gutter.
            check(
                "secondary on the gutter",
                p.secondary,
                p.window,
                super::super::ramp::SECONDARY_MIN_CONTRAST.min(contrast(p.text, p.window)) - 0.01,
            );
            check(
                "dim on the gutter",
                p.dim,
                p.window,
                super::super::ramp::DIM_MIN_CONTRAST.min(contrast(p.secondary, p.window) - 0.3)
                    - 0.01,
            );
            // Accent-coloured text (the playing title) reads at AA, whatever the accent.
            check(
                "accent text on the card",
                p.accent_text(),
                p.panel,
                super::super::ramp::ACCENT_TEXT_MIN_CONTRAST.min(contrast(p.text, p.panel)),
            );
            // The faintest tone still has to be read: track counts, help lines.
            check(
                "dim on the card",
                p.dim,
                p.panel,
                super::super::ramp::DIM_MIN_CONTRAST.min(contrast(p.secondary, p.panel) - 0.3),
            );
            // Lifting it must not overtake the tone above it, or the hierarchy inverts.
            assert!(
                contrast(p.dim, p.panel) <= contrast(p.secondary, p.panel) + 0.01,
                "{name}: dim reads stronger than secondary"
            );
            // The accent carries the play button's glyph and the primary pill's label. 3:1 is
            // WCAG's bar for a UI component, and the bar the published accents actually meet.
            check("on_accent on the accent", p.on_accent, p.accent, 3.0);
            // A chip inverts: its label is painted in the window colour on a text-coloured
            // pill, which only works while those two are far apart.
            check("a selected chip's label", p.window, p.text, 4.5);
        }
    }

    /// A theme that paints the same as another is a menu entry that does nothing.
    #[test]
    fn no_two_builtin_themes_are_the_same() {
        let all = builtins();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(
                    a.palette(),
                    b.palette(),
                    "{} and {} are identical",
                    a.name,
                    b.name
                );
            }
        }
    }

    /// Every file is what its `color-scheme` says — the polarity drives `Visuals` and the
    /// shadows, and which list the theme appears in — and its gutter reads as a recess.
    #[test]
    fn every_builtin_theme_is_the_polarity_it_declares() {
        for t in builtins() {
            let p = t.palette();
            if t.dark {
                assert!(luminance(p.panel) < 0.2, "{}: not dark", t.name);
            } else {
                assert!(luminance(p.panel) > 0.5, "{}: not light", t.name);
            }
            assert!(
                luminance(p.window) < luminance(p.panel),
                "{}: the gutter is not behind the card",
                t.name
            );
        }
    }

    /// The fallback is exactly what the app painted before themes existed.
    #[test]
    fn the_fallback_is_classic() {
        let dark = fallback(true).palette();
        assert!(dark.dark);
        assert_eq!(dark.window, Color32::from_rgb(0x07, 0x08, 0x0a));
        assert_eq!(dark.panel, Color32::from_rgb(0x14, 0x16, 0x19));
        assert_eq!(dark.accent, Color32::from_rgb(0xff, 0x33, 0x3d));
        assert_eq!(dark.accent_hover, Color32::from_rgb(0xff, 0x55, 0x5e));
        let light = fallback(false).palette();
        assert!(!light.dark);
        assert_eq!(light.panel, Color32::WHITE);
        assert_eq!(light.accent, Color32::from_rgb(0xe0, 0x21, 0x2b));
    }

    #[test]
    fn a_theme_is_found_however_its_name_is_typed() {
        let themes = Themes {
            themes: builtins(),
            ..Themes::default()
        };
        for q in [
            "catppuccin-frappe",
            "Catppuccin Frappé",
            "CATPPUCCIN_FRAPPE",
        ] {
            assert_eq!(
                themes.find_loose(q).unwrap().name,
                "Catppuccin Frappé",
                "{q}"
            );
        }
        assert_eq!(
            themes.find_loose("rose pine dawn").unwrap().name,
            "Rosé Pine Dawn"
        );
        assert!(themes.find_loose("not a theme").is_none());
    }

    /// Seeding writes every built-in once, into a folder that did not exist.
    #[test]
    fn seeding_writes_every_builtin_theme() {
        let dir = std::env::temp_dir().join(format!("refryt-seed-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        seed_into(&dir).unwrap();
        let loaded = load_from(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.themes.len(), BUILTIN.len());
        assert!(loaded.get("Catppuccin Frappé").is_some_and(|t| t.dark));
        assert!(loaded.get("Nord Snow Storm").is_some_and(|t| !t.dark));
    }

    /// A folder made before the record gets the themes added since, once; a deleted one stays
    /// deleted; and a later run writes nothing.
    #[test]
    fn a_new_builtin_reaches_an_existing_folder_once() {
        let dir = std::env::temp_dir().join(format!("refryt-offer-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Classic Dark.css"), BUILTIN[4].1).unwrap();
        seed_into(&dir).unwrap();
        assert!(dir.join("Spotify.css").exists(), "the new theme arrives");
        assert!(
            !dir.join("Nord Polar Night.css").exists(),
            "an old one deleted stays deleted"
        );
        std::fs::remove_file(dir.join("Spotify.css")).unwrap();
        seed_into(&dir).unwrap();
        assert!(
            !dir.join("Spotify.css").exists(),
            "and so does the new one, once deleted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_loads_css_files_by_name_and_reports_broken_ones() {
        let dir = std::env::temp_dir().join(format!("refryt-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.css"), "--base:#000;--text:#fff;--accent:#f00;").unwrap();
        std::fs::write(dir.join("a.css"), "--base:#000;").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a theme").unwrap();
        let loaded = load_from(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(loaded.themes.len(), 1);
        assert_eq!(loaded.themes[0].name, "b");
        assert_eq!(loaded.errors.len(), 1);
        assert_eq!(loaded.errors[0].0, "a.css");
    }
}
