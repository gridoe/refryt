//! A fast, native YouTube Music client.
//!
//! The application is a library with a thin binary on top, so the diagnostics in `src/bin/` can
//! exercise the *same* code paths the app uses. That matters most for `api-check`: search now runs
//! through our own parser (see [`innertube`]), and a canary that tested `ytmapi-rs` directly would
//! be watching the fallback rather than the path anyone actually takes.

/// Wayland: bringing the window back with the tray's activation token.
#[cfg(target_os = "linux")]
pub mod activation;
pub mod api;
pub mod app;
pub mod auth;
/// "Start on boot" — an XDG autostart entry on Linux, a LaunchAgent on macOS, the per-user `Run`
/// key on Windows; see the
/// module's own doc comment.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub mod autostart;
pub mod backend;
pub mod blur;
pub mod browser_cookies;
pub mod chrome;
pub mod crash;
pub mod discord;
pub mod downloads;
pub mod emoji;
pub mod flatpak;
pub mod history_poll;
pub mod icons;
pub mod images;
pub mod innertube;
/// One instance per profile — a second launch shows the running one's window instead of starting
/// a second player; see the module's own doc comment.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub mod instance;
/// Plasma on Wayland: restoring the app's own minimised window through KWin.
#[cfg(target_os = "linux")]
pub mod kwin;
pub mod liked;
pub mod listening_stats;
pub mod local;
pub mod local_playlists;
pub mod media;
pub mod memory;
/// macOS: the app's menus in the menu bar; see the module's own doc comment.
#[cfg(target_os = "macos")]
pub mod menu_bar;
pub mod model;
pub mod pacing;
pub mod persist;
pub mod play_log;
pub mod playback;
pub mod player;
pub mod queue;
pub mod search_history;
pub mod settings;
pub mod song_cache;
pub mod stats;
/// Sway on Wayland: hiding the app's own window to the tray and back through the scratchpad.
#[cfg(target_os = "linux")]
pub mod sway;
pub mod takeout;
pub mod theme;
pub mod tray;
pub mod ui;
pub mod update;
pub mod video;
/// The in-app sign-in window — WebView2 on Windows, WebKitGTK on Linux, WKWebView on macOS; see
/// the module's own
/// doc comment.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
pub mod webview_login;
pub mod widgets;
pub mod window_fullscreen;

use std::sync::Arc;

use eframe::egui;

/// Set as early as possible so `--startup-timing` measures from process entry, not from the
/// point eframe hands us a context.
static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn mark_start() {
    let _ = START.set(std::time::Instant::now());
}

/// Where the app's own log goes.
///
/// A GUI binary started from the Start Menu on Windows has no console, and a desktop-launched
/// AppImage on Linux has its stdout and stderr pointed at `/dev/null` by the launcher — so
/// without a file the one warning that explains a failed playback (the resolver error, yt-dlp's
/// own stderr) is invisible. Next to the credential and the crash record, in the config
/// directory. macOS needs it for the same reason: a bundle launched from Finder has no terminal
/// to print to.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn log_path() -> Option<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "refryt")?;
    Some(dirs.config_dir().join("refryt.log"))
}

/// Append-only writer for the log file.
///
/// Opened per event rather than held: a stale handle after a crash or an external rotation is
/// worse than the tiny cost, and a file that cannot be opened silences only the file layer.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
struct LogFile(std::path::PathBuf);

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogFile {
    type Writer = Box<dyn std::io::Write + Send + Sync>;

    fn make_writer(&'a self) -> Self::Writer {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .map(|file| Box::new(file) as Self::Writer)
            .unwrap_or_else(|_| Box::new(std::io::sink()))
    }
}

/// Install the tracing subscriber with a `RUST_LOG`-driven target filter.
///
/// This uses `filter::Targets` rather than `EnvFilter` on purpose: `EnvFilter`'s dynamic
/// directive matching pulls in `regex-automata` + `regex-syntax` (~14 MB of rlib), which was
/// the only regex anywhere in the default build. `Targets` parses the same
/// `warn,refryt=info`-style syntax without it. An unparseable `RUST_LOG` falls back to `default`.
pub fn init_tracing(default: &str) {
    use tracing_subscriber::{filter::Targets, fmt, prelude::*};

    let filter = || {
        std::env::var("RUST_LOG")
            .ok()
            .and_then(|s| s.parse::<Targets>().ok())
            .unwrap_or_else(|| {
                default
                    .parse::<Targets>()
                    .expect("valid default log filter")
            })
    };

    let console = fmt::layer().with_filter(filter());

    // The same lines also go to a file, so a Start Menu launch (Windows) or a launcher-started
    // AppImage (Linux) is diagnosable. Kept to a couple of megabytes: the previous run is
    // dropped rather than the disk growing without bound, and there is nothing in here worth
    // archiving.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    if let Some(path) = log_path() {
        // A first run has no config directory yet — the credential and the settings file make
        // it later — and the writer opens per event, so without this every event would fall
        // back to the sink and the file would never appear.
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::metadata(&path)
            .map(|meta| meta.len() > 2 * 1024 * 1024)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&path);
        }
        tracing_subscriber::registry()
            .with(console)
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .with_writer(LogFile(path))
                    .with_filter(filter()),
            )
            .init();
        return;
    }

    tracing_subscriber::registry().with(console).init();
}

pub fn elapsed_since_start() -> std::time::Duration {
    START.get().map(|t| t.elapsed()).unwrap_or_default()
}

/// Hand a URL to the desktop's own handler.
///
/// eframe opens links through its `links` feature, which is part of its default set — and this
/// build turns the defaults off to keep the dependency tree small, so hyperlinks silently did
/// nothing. Spawning the platform opener is the same thing that feature does on Linux, costs no
/// dependency, and matches how the app already shells out to `yt-dlp`. macOS uses `open`, which
/// hands the URL to the default browser.
pub fn open_url(url: &str) {
    // Windows goes through `ShellExecuteW` rather than spawning `cmd /C start`: the shell parses
    // the URL itself, so an `&` or `^` in a watch link cannot be mangled by cmd's own parser,
    // and nothing console-shaped is spawned at all. The return is an HINSTANCE-style code where
    // anything at or below 32 is a failure.
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let operation: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        let file: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                0,
                operation.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as isize <= 32 {
            tracing::warn!(%url, code = result as isize, "could not open a link");
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "linux")]
        let (program, args): (&str, &[&str]) = ("xdg-open", &[]);
        #[cfg(target_os = "macos")]
        let (program, args): (&str, &[&str]) = ("open", &[]);

        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(_) => {}
            Err(e) => tracing::warn!(%url, error = %e, "could not open a link"),
        }
    }
}

/// The app's own artwork: the window/taskbar/launcher icon, and (as an egui texture instead of
/// an OS-level `IconData`) the branding shown in Settings. One embed, two decodes, so a change to
/// the master PNG only has one place to update.
const LOGO_PNG: &[u8] = include_bytes!("../../../assets/icons/app/refryt-256.png");

/// The window / taskbar / launcher icon, decoded from the same preprocessed artwork that
/// `just install` places into hicolor. `None` when the bytes cannot be decoded — the app still
/// runs, just without an icon.
pub fn window_icon() -> Option<Arc<egui::IconData>> {
    let img = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(Arc::new(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }))
}

/// The same artwork as a texture, for the app's own UI (currently just the Settings page)
/// rather than the OS chrome `window_icon` is for. Loaded once and cached by the caller — a
/// fresh `egui::Context::load_texture` call is cheap to repeat, but there is no reason to.
pub fn logo_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    let image =
        egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], img.as_raw());
    Some(ctx.load_texture("refryt-logo", image, egui::TextureOptions::LINEAR))
}
