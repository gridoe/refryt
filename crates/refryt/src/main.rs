//! Binary entry point. Everything of substance lives in the library — see `lib.rs`.

// Windows-only, ignored elsewhere: without it a GUI build still launches with a console window
// behind it, since Windows otherwise defaults every binary to the console subsystem.
#![windows_subsystem = "windows"]

use eframe::egui;
use refryt::{app, mark_start, window_icon};

fn main() -> eframe::Result {
    mark_start();
    // Before any other thread exists: how glibc treats freed memory (see `memory.rs`).
    refryt::memory::tune();

    // The Discord crate's one warning is "Discord isn't running", repeated on every retry while
    // Rich Presence is on: a third of a long log, saying nothing.
    refryt::init_tracing("warn,refryt=info,discord_rich_presence=error");
    // Record a panic before the abort so the next launch can offer it.
    refryt::crash::install_hook();

    // Frameless is the default look, but it is the first thing to fail on an unusual setup:
    // X11 without a compositor paints the rounded corners black, and some tiling WMs ignore
    // client-side decorations. `--decorated` hands the window back to the OS.
    let decorated = std::env::args().any(|a| a == "--decorated");
    // Print the time to first painted frame and exit. This is the only honest way to measure
    // cold start: everything before the first frame is invisible to the user, and everything
    // after it is not startup.
    let startup_timing = std::env::args().any(|a| a == "--startup-timing");

    // Launch straight into a search, and optionally start playing its first hit. Handy on its
    // own for a keyboard-first app, and the only way to exercise the whole chain without a mouse.
    let args: Vec<String> = std::env::args().collect();
    let search = args
        .iter()
        .position(|a| a == "--search")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let autoplay = args.iter().any(|a| a == "--autoplay");

    // Live counters. `--stats` shows the overlay (F2 toggles it at runtime); `--stats-after N`
    // prints one machine-readable line after N seconds and exits, which is how the perf script
    // measures an app that has actually loaded something.
    // Start on a named page. `--page search` is a page that fetches nothing, which is how the
    // perf script measures the app's floor now that the default launch loads the home feed.
    let page = args
        .iter()
        .position(|a| a == "--page")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| app::Route::parse(p));

    let stats = args.iter().any(|a| a == "--stats");
    // `--autoscroll-max <pt>` stops there, so a screenshot can land on the end of a page: an
    // offset past it draws nothing, since egui only clamps it after the frame is laid out.
    let autoscroll_max = args
        .iter()
        .position(|a| a == "--autoscroll-max")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<f32>().ok());
    let autoscroll = autoscroll_max.is_some() || args.iter().any(|a| a == "--autoscroll");
    let queue = args.iter().any(|a| a == "--queue");
    let lyrics = args.iter().any(|a| a == "--lyrics");
    let now_playing = args.iter().any(|a| a == "--now-playing");

    // Open at a given desktop coordinate, without taking focus. For testing on a second screen
    // while someone is working on the first.
    let position = args
        .iter()
        .position(|a| a == "--position")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.split_once(','))
        .and_then(|(x, y)| Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?)));
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(|p| {
            let value = |flag: &str| {
                args.iter()
                    .position(|a| a == flag)
                    .and_then(|i| args.get(i + 1))
                    .cloned()
            };
            let when = match value("--screenshot-on").as_deref() {
                Some("ring") => app::ShotWhen::Ring,
                Some("skeleton") => app::ShotWhen::Skeleton,
                _ => app::ShotWhen::After(std::time::Duration::from_secs_f64(
                    value("--screenshot-after")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(8.0),
                )),
            };
            (std::path::PathBuf::from(p), when)
        });
    let stats_after = args
        .iter()
        .position(|a| a == "--stats-after")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<f64>().ok())
        .map(std::time::Duration::from_secs_f64);

    // Override the saved theme and opacity for this run only — never written to
    // `settings.json` — so the whole theme × opacity matrix can be screenshotted without
    // clicking through Settings for each one. `--scheme` takes a theme's name however it is
    // typed (`catppuccin-frappe`); it is matched once the themes folder has been read.
    let scheme = args
        .iter()
        .position(|a| a == "--scheme")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let opacity = args
        .iter()
        .position(|a| a == "--opacity")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| refryt::settings::Opacity::parse(s));
    // A safety hatch, not a test aid: `blur` persists to disk, and a Wayland protocol error in
    // the blur binding is fatal to the whole connection — a session that dies on it needs a way
    // back in that does not depend on reaching the Settings page.
    let no_blur = args.iter().any(|a| a == "--no-blur");
    // Autoplay off for one run, without touching `settings.json`: the switch is in Settings, and
    // the A/B of a queue with and without its tail should not need a click to set up.
    let no_autoplay = args.iter().any(|a| a == "--no-autoplay");
    // Drive the tray without a mouse: hide after N seconds, show three seconds later, quit three
    // seconds after that — each injected through the same queue-and-wake path a real tray click
    // takes. A test aid, like `autoscroll`: a tray icon cannot be clicked from outside the
    // process, and this is what makes quit latency measurable from a script.
    let tray_test = args
        .iter()
        .position(|a| a == "--tray-test")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<f64>().ok())
        .map(std::time::Duration::from_secs_f64);
    // Passed by the autostart entry (`autostart::ARG`): login launched this, so once the window
    // is up it should ask the compositor to bring it forward — Wayland starts it behind whatever
    // window already has focus, and nothing can raise a window that has not asked.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    let autostart = args.iter().any(|a| a == refryt::autostart::ARG);
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    let autostart = false;

    // The child half of the in-app sign-in window: `webview_login::capture` starts this same
    // binary with `--sign-in-window <file>`, because tao's event loop cannot be created beside
    // eframe's winit one in the same process (it faults natively on Windows). Handled here —
    // before any of the app is set up — so the child is nothing but the sign-in window.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    if let Some(at) = args.iter().position(|a| a == refryt::webview_login::ARG) {
        let path = args.get(at + 1).cloned().unwrap_or_default();
        let silent =
            args.get(at + 2).map(String::as_str) == Some(refryt::webview_login::SILENT_ARG);
        return match refryt::webview_login::capture_here(std::path::Path::new(&path), silent) {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("{e:#}");
                std::process::exit(1);
            }
        };
    }

    // One instance per profile: a plain second launch — the desktop entry, the AppImage file —
    // tells the running instance to show its window and exits instead of starting a second
    // player. A launch with any other argument is scripted (`--page`, `--search`, a test aid),
    // which is not a user clicking the app again, so it runs unguarded. See `instance`'s own
    // module doc.
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    {
        let plain = args.iter().skip(1).all(|a| a == refryt::autostart::ARG);
        if plain
            && matches!(
                refryt::instance::claim(),
                refryt::instance::Claim::Secondary
            )
        {
            return Ok(());
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Refryt")
        .with_app_id(refryt::flatpak::app_id())
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([refryt::theme::WINDOW_MIN_W, refryt::theme::WINDOW_MIN_H]);

    // A test aid for debug builds: `REFRYT_WINDOW_SIZE=800x600` opens at that size, for checking
    // a layout at a width the window manager will not hand over by itself.
    #[cfg(debug_assertions)]
    if let Some((w, h)) = std::env::var("REFRYT_WINDOW_SIZE")
        .ok()
        .and_then(|v| {
            v.split_once('x')
                .map(|(w, h)| (w.parse::<f32>(), h.parse::<f32>()))
        })
        .and_then(|(w, h)| Some((w.ok()?, h.ok()?)))
    {
        viewport = viewport.with_inner_size([w, h]);
    }

    // A test aid: open maximized, for checking layouts at full-screen widths.
    if args.iter().any(|a| a == "--maximized") {
        viewport = viewport.with_maximized(true);
    }
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    if let Some((x, y)) = position {
        viewport = viewport
            .with_position([x, y])
            // Do not raise or focus: the point is to leave the other screen alone.
            .with_active(false);
    }

    if !decorated {
        // macOS: a native window with its title bar hidden and the content running under it,
        // rather than a borderless one. The system then owns the corner shape — rounded when
        // windowed *and* when zoomed, with the shadow and the background blur clipped to it —
        // where a borderless window is a plain rectangle that only looks rounded because the
        // app paints it so (and so shows blur and shadow in the square corners behind that).
        // The system's own traffic lights stay, moved down into the top-left corner of the shell
        // each frame (`chrome::TrafficLights`); the app draws no window buttons of its own here.
        #[cfg(target_os = "macos")]
        {
            viewport = viewport
                .with_fullsize_content_view(true)
                .with_titlebar_shown(false)
                .with_title_shown(false)
                .with_titlebar_buttons_shown(true);
        }
        #[cfg(not(target_os = "macos"))]
        {
            viewport = viewport.with_decorations(false);
        }
        // Every compositor is asked for the see-through shell it can blend. On Windows that
        // takes the DirectComposition swapchain set up below as well — without it, the request
        // alone composites the shell over a flat grey.
        viewport = viewport.with_transparent(true);
    }

    // On macOS the transparent request above only gives a see-through window. The blur is applied
    // later, from `App::ui`, once the window exists (`blur.rs`).
    #[allow(unused_mut)]
    let mut native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    // Windows: DX12 with a DirectComposition swapchain, the one presentation path there that
    // carries per-pixel alpha. An HWND swapchain — DX12's default, and Vulkan's only one — is
    // opaque to DWM. Always on, not only when the slider is up, so moving it needs no restart;
    // an opaque shell looks the same through either. What sits under the shell is `blur.rs`'s
    // business: the window's redirection surface, painted black so it reads as glass.
    // `WGPU_BACKEND` and `WGPU_DX12_PRESENTATION_SYSTEM` still override both, for A/B-ing a driver
    // problem.
    #[cfg(target_os = "windows")]
    {
        use eframe::egui_wgpu::{WgpuSetup, WgpuSetupCreateNew, wgpu};
        let mut setup = WgpuSetupCreateNew::without_display_handle();
        setup.instance_descriptor.backends =
            wgpu::Backends::from_env().unwrap_or(wgpu::Backends::DX12);
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .presentation_system =
            wgpu::Dx12SwapchainKind::from_env().unwrap_or(wgpu::Dx12SwapchainKind::DxgiFromVisual);
        native_options.wgpu_options.wgpu_setup = WgpuSetup::CreateNew(setup);
    }
    // A test aid for debug builds: `REFRYT_NO_VSYNC=1` stops the swap from waiting for the screen,
    // to tell a window that is blocked in the swap from one that is not being asked for frames.
    #[cfg(debug_assertions)]
    if std::env::var_os("REFRYT_NO_VSYNC").is_some() {
        native_options.glow_options.vsync = false;
    }
    let result = eframe::run_native(
        "refryt",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(
                app::App::new(
                    cc,
                    app::Options {
                        decorated,
                        startup_timing,
                        search: search.clone(),
                        autoplay,
                        stats,
                        stats_after,
                        screenshot: screenshot.clone(),
                        page: page.clone(),
                        autoscroll,
                        autoscroll_max,
                        queue,
                        lyrics,
                        now_playing,
                        scheme,
                        opacity,
                        no_blur,
                        no_autoplay,
                        tray_test,
                        autostart,
                    },
                )
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format!("{e:#}").into()
                })?,
            ))
        }),
    );
    // The app itself was dropped inside `run_native` (eframe destroys it on `LoopExiting`), so
    // this line is the log's proof that teardown finished: a slow quit that never reaches it is
    // stuck in one of the drops `App::drop` starts.
    tracing::info!("event loop exited");
    result
}
