//! The system tray icon: Show/Hide and Quit, shown only while "Hide tray on close" is on.
//!
//! On Windows and macOS the icon is `tray-icon`'s, built directly on the calling thread — see the
//! crate's own `examples/egui.rs`, which this follows. On Linux it is a StatusNotifierItem from
//! `ksni`, spoken over D-Bus from a worker thread ksni runs itself. `tray-icon`'s ksni backend did
//! the same, but cannot register without a well-known bus name (`StatusNotifierItem-<pid>-<n>`),
//! and inside a Flatpak only an unnamed item is allowed: Flathub does not grant the name, and
//! without it the sandbox's bus proxy refused the tray outright. Before that, `tray-icon`'s
//! libappindicator backend needed a GTK loop on a thread of its own, never reported a click on
//! the icon (Plasma's left click got "No handler for Activate"), and showed no tooltip.
//!
//! [`spawn`] is called at most once per process, the first time "Hide tray on close" is turned
//! on — not once per toggle. An earlier version that called `spawn` again on every toggle left a
//! duplicate tray icon behind each time the setting was flipped off and back on.
//! [`Tray::set_visible`] is the fix: it shows or hides the one icon that was ever built, so it
//! lives for the rest of the run regardless of how many times the setting changes.
//!
//! macOS adds one constraint of its own. While the window is hidden, winit stops delivering
//! frames to it, and that includes the repaint requests that normally wake the UI. A click on
//! "Show" would therefore sit in the queue and never be handled. [`inject`] avoids this by
//! ordering the window back on screen through AppKit first (`bring_window_forward`), without
//! waiting for a frame; the next frame then reads the queued action as usual.
//!
//! Clicks arrive on whichever thread the backend runs on — ksni's D-Bus worker on Linux, the
//! icon's own message pump on Windows — never the render thread, and the crates' own channels do
//! not wake winit. So [`spawn`] installs process-wide event handlers (on Linux, ksni's callbacks)
//! instead of leaving the events to be polled: each handler queues a [`TrayAction`]
//! and calls `egui::Context::request_repaint`. The repaint is the half that matters: an idle
//! window schedules no frames of its own, so without it a tray click sat in the queue until
//! something unrelated happened to paint — which is what made Quit take as long as it took for
//! that something to arrive. [`poll`] drains the queue once a frame, the same way `Backend`'s
//! own event channel already is.

use std::collections::VecDeque;
use std::sync::Mutex;

#[cfg(not(target_os = "linux"))]
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
#[cfg(not(target_os = "linux"))]
use tray_icon::{MouseButton, TrayIconBuilder, TrayIconEvent};

#[cfg(not(target_os = "linux"))]
const SHOW_HIDE_ID: &str = "ytm-tray-show-hide";
#[cfg(not(target_os = "linux"))]
const QUIT_ID: &str = "ytm-tray-quit";

/// What choosing a tray menu item — or clicking the icon itself — means for the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Flip the window between hidden and shown.
    ToggleVisible,
    /// Unhide the window and focus it. A double-click's meaning; it never hides.
    Show,
    Quit,
}

/// Actions queued by the crate's event handlers, drained by [`poll`].
///
/// The handlers run on the tray backend's thread, never the render thread, so the handoff is a
/// plain queue; the `request_repaint` in [`inject`] is what makes the render thread look at it.
static ACTIONS: Mutex<VecDeque<TrayAction>> = Mutex::new(VecDeque::new());

/// Hand `action` to the render thread: queue it, then wake the UI.
///
/// `pub(crate)` for the `--tray-test` diagnostic, which has to reach the same queue a real
/// click does — there is no way to click a tray icon from outside the process.
pub(crate) fn inject(ctx: &egui::Context, action: TrayAction) {
    // A hidden or minimized window gets no frames on macOS — not even the heartbeat's — so the
    // frame that would act on this action never comes. Put the window back first; the next
    // frame then runs `App::poll_tray` as usual.
    #[cfg(target_os = "macos")]
    bring_window_forward();
    if let Ok(mut queue) = ACTIONS.lock() {
        queue.push_back(action);
    }
    ctx.request_repaint();
}

/// Order the app's main window back on screen and activate the app, without needing a frame.
///
/// AppKit objects may only be touched from the main thread, which is where the tray's own event
/// handlers already run; from any other thread (the `--tray-test` diagnostic) the work is handed
/// to the main queue. Only normal-level windows are touched: the status item's own window sits
/// above that level and is not ours to raise.
#[cfg(target_os = "macos")]
fn bring_window_forward() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    fn raise(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        for window in app.windows() {
            if window.level() == 0 {
                window.makeKeyAndOrderFront(None);
            }
        }
    }

    match MainThreadMarker::new() {
        Some(mtm) => raise(mtm),
        None => dispatch2::DispatchQueue::main().exec_async(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                raise(mtm);
            }
        }),
    }
}

/// Install the process-wide handlers that turn the crate's events into [`TrayAction`]s.
///
/// Called from [`spawn`], which runs at most once per process: the crate's handlers are
/// first-set-only, so a second call would silently keep the first closure — and with it the
/// first `egui::Context` — anyway.
#[cfg(not(target_os = "linux"))]
fn install_handlers(ctx: &egui::Context) {
    install_menu_handler(ctx);
    let icon_ctx = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let Some(action) = icon_action(&event) {
            inject(&icon_ctx, action);
        }
    }));
}

/// Install muda's process-wide menu event handler, once. Shared by the tray's menu and, on
/// macOS, the menu bar (`menu_bar`): muda keeps only the first handler ever set, so whichever of
/// the two comes first has to route for both.
#[cfg(not(target_os = "linux"))]
pub(crate) fn install_menu_handler(ctx: &egui::Context) {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id.0.as_str();
            match menu_action(id) {
                Some(action) => inject(&ctx, action),
                #[cfg(target_os = "macos")]
                None => {
                    crate::menu_bar::dispatch(id, &ctx);
                }
                #[cfg(not(target_os = "macos"))]
                None => {}
            }
        }));
    });
}

/// What a menu id means, if it is one of ours.
#[cfg(not(target_os = "linux"))]
fn menu_action(id: &str) -> Option<TrayAction> {
    match id {
        SHOW_HIDE_ID => Some(TrayAction::ToggleVisible),
        QUIT_ID => Some(TrayAction::Quit),
        _ => None,
    }
}

/// What a click on the icon itself means, if anything.
///
/// Only a left double-click: it shows the window. A single left click means nothing on Windows,
/// where it is the first half of a double-click — the crate emits `Click(Up)`, then
/// `DoubleClick`, then a second `Click(Up)` — so a toggle on the first release would fight the
/// double-click's own show (hide, show, hide). macOS opens the menu on a click by itself.
#[cfg(not(target_os = "linux"))]
fn icon_action(event: &TrayIconEvent) -> Option<TrayAction> {
    match event {
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => Some(TrayAction::Show),
        _ => None,
    }
}

/// The icon, decoded once per call from the same PNG [`crate::window_icon`] uses for the
/// titlebar/taskbar — one source of truth for what Refryt's icon looks like.
fn icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    let bytes = include_bytes!("../../../assets/icons/app/refryt-256.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some((img.into_raw(), width, height))
}

#[cfg(not(target_os = "linux"))]
fn icon(ctx: &egui::Context) -> Option<tray_icon::Icon> {
    // Windows: the .exe's own icon resource (`refryt.rc`'s `1 ICON`, `refryt.ico` with 16 to
    // 256 px drawn), loaded at the tray's size. Handed the 256 px PNG instead, the shell shrank
    // it to 16-32 px itself, and the result was pixelated. 16 px is the tray's size at 100%
    // scaling; Windows picks the nearest drawn size and scales it well from there.
    #[cfg(target_os = "windows")]
    {
        let size = (16.0 * ctx.pixels_per_point()).round() as u32;
        match tray_icon::Icon::from_resource(1, Some((size, size))) {
            Ok(icon) => return Some(icon),
            Err(e) => tracing::warn!(%e, "tray: no icon resource, using the PNG"),
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = ctx;
    let (rgba, width, height) = icon_rgba()?;
    tray_icon::Icon::from_rgba(rgba, width, height).ok()
}

#[cfg(not(target_os = "linux"))]
fn menu() -> Menu {
    let show_hide = MenuItem::with_id(SHOW_HIDE_ID, "Show/Hide", true, None);
    let quit = MenuItem::with_id(QUIT_ID, "Quit", true, None);
    Menu::with_items(&[&show_hide, &quit]).unwrap_or_else(|_| Menu::new())
}

/// A live tray icon, kept for the rest of the run once built. `set_visible` is the only thing
/// this is for after construction — see the module doc comment for why toggling visibility, not
/// rebuilding the icon, is what a setting toggle should do.
pub struct Tray {
    #[cfg(not(target_os = "linux"))]
    icon: Option<tray_icon::TrayIcon>,
    #[cfg(target_os = "linux")]
    icon: Option<ksni::blocking::Handle<StatusItem>>,
}

impl Tray {
    /// Show or hide the icon. A no-op if [`spawn`] never managed to build one — reported already,
    /// at construction time, so this stays quiet.
    pub fn set_visible(&self, visible: bool) {
        #[cfg(not(target_os = "linux"))]
        if let Some(icon) = &self.icon {
            let _ = icon.set_visible(visible);
        }
        // A passive item is one the host hides, as `tray-icon`'s own ksni backend did it.
        #[cfg(target_os = "linux")]
        if let Some(icon) = &self.icon {
            icon.update(|item| item.status = status(visible));
        }
    }
}

/// Build the tray icon. Call once, the first time "Hide tray on close" is turned on — not on
/// every toggle after that, which is exactly the mistake this module's doc comment describes;
/// use [`Tray::set_visible`] for every toggle from then on.
#[cfg(not(target_os = "linux"))]
pub fn spawn(ctx: &egui::Context) -> Tray {
    // Installed even if the icon itself never gets built: the handlers are what wake the UI,
    // and they are harmless with no icon to click.
    install_handlers(ctx);
    let icon = icon(ctx).and_then(|icon| {
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu()))
            .with_icon(icon)
            .with_tooltip("Refryt");
        // Windows opens the context menu on left-click by default; left as-is, the menu would
        // open on the first click of a double-click, which now means "show". Right click still
        // opens the menu, the Windows convention, and a single left click does nothing — see
        // `icon_action`.
        #[cfg(target_os = "windows")]
        let builder = builder.with_menu_on_left_click(false);
        builder
            .build()
            .inspect_err(|e| tracing::warn!(%e, "could not create the tray icon"))
            .ok()
    });
    Tray { icon }
}

/// Build the tray icon: see the other platforms' [`spawn`]. Inside a Flatpak the item registers
/// under the connection's unique name only, the one way the sandbox lets it onto the bus.
#[cfg(target_os = "linux")]
pub fn spawn(ctx: &egui::Context) -> Tray {
    use ksni::blocking::TrayMethods;
    let item = StatusItem {
        ctx: ctx.clone(),
        status: status(true),
        icon: icon_rgba()
            .map(|(rgba, width, height)| ksni::Icon {
                width: width as i32,
                height: height as i32,
                data: argb(rgba),
            })
            .into_iter()
            .collect(),
    };
    let icon = item
        .disable_dbus_name(crate::flatpak::sandboxed())
        .spawn()
        .inspect_err(|e| tracing::warn!(%e, "could not create the tray icon"))
        .ok();
    Tray { icon }
}

/// The Linux tray item: what ksni asks of it, and the actions its clicks queue.
#[cfg(target_os = "linux")]
struct StatusItem {
    ctx: egui::Context,
    status: ksni::Status,
    icon: Vec<ksni::Icon>,
}

#[cfg(target_os = "linux")]
fn status(visible: bool) -> ksni::Status {
    if visible {
        ksni::Status::Active
    } else {
        ksni::Status::Passive
    }
}

/// RGBA, as the PNG decodes, to the ARGB a StatusNotifierItem pixmap is.
#[cfg(target_os = "linux")]
fn argb(mut rgba: Vec<u8>) -> Vec<u8> {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.rotate_right(1);
    }
    rgba
}

#[cfg(target_os = "linux")]
impl ksni::Tray for StatusItem {
    fn id(&self) -> String {
        crate::flatpak::app_id()
    }

    /// What the host names the icon by; without it Plasma shows the process name, lowercase.
    fn title(&self) -> String {
        "Refryt".to_owned()
    }

    fn status(&self) -> ksni::Status {
        self.status
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Refryt".to_owned(),
            ..Default::default()
        }
    }

    /// A left click toggles the window: a status-notifier host may offer no other interaction.
    fn activate(&mut self, _x: i32, _y: i32) {
        keep_token();
        inject(&self.ctx, TrayAction::ToggleVisible);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let item = |label: &str, action: TrayAction| {
            StandardItem {
                label: label.to_owned(),
                activate: Box::new(move |item: &mut Self| {
                    keep_token();
                    inject(&item.ctx, action);
                }),
                ..Default::default()
            }
            .into()
        };
        vec![
            item("Show/Hide", TrayAction::ToggleVisible),
            item("Quit", TrayAction::Quit),
        ]
    }
}

/// The activation token the tray host sent with the latest click, for `activation` to bring the
/// window back with on Wayland. Read once by the frame that acts on the click.
#[cfg(target_os = "linux")]
static TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// Keep the token of the click being handled. Called inside ksni's callback, the only place
/// [`ksni::current_xdg_activation_token`] answers. KDE may send the token a moment after the
/// click itself, so a missing one is asked for once more after a millisecond, as ksni advises.
#[cfg(target_os = "linux")]
fn keep_token() {
    let token = ksni::current_xdg_activation_token().or_else(|| {
        std::thread::sleep(std::time::Duration::from_millis(1));
        ksni::current_xdg_activation_token()
    });
    if let Ok(mut kept) = TOKEN.lock() {
        *kept = token;
    }
}

/// The token of the latest tray click, if one came with it; taken, so it is used at most once.
#[cfg(target_os = "linux")]
pub fn take_activation_token() -> Option<String> {
    TOKEN.lock().ok()?.take()
}

/// Drain one tray action, if any arrived since the last call.
pub fn poll() -> Option<TrayAction> {
    ACTIONS.lock().ok()?.pop_front()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn menu_ids_map_to_their_actions() {
        assert_eq!(menu_action(SHOW_HIDE_ID), Some(TrayAction::ToggleVisible));
        assert_eq!(menu_action(QUIT_ID), Some(TrayAction::Quit));
        assert_eq!(menu_action("some-other-item"), None);
    }

    #[cfg(not(target_os = "linux"))]
    fn double_click(button: MouseButton) -> TrayIconEvent {
        TrayIconEvent::DoubleClick {
            id: tray_icon::TrayIconId::new("test"),
            position: tray_icon::Rect::default().position,
            rect: tray_icon::Rect::default(),
            button,
        }
    }

    /// The gesture this module's Windows rule exists for: a left double-click shows the window,
    /// and a right one — the context menu's own double-click, if a host sends one — does not.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_left_double_click_shows_the_window() {
        assert_eq!(
            icon_action(&double_click(MouseButton::Left)),
            Some(TrayAction::Show)
        );
        assert_eq!(icon_action(&double_click(MouseButton::Right)), None);
        assert_eq!(icon_action(&double_click(MouseButton::Middle)), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_pixmap_is_argb() {
        assert_eq!(
            argb(vec![1, 2, 3, 4, 5, 6, 7, 8]),
            vec![4, 1, 2, 3, 8, 5, 6, 7]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_icon_decodes() {
        let (rgba, width, height) = icon_rgba().expect("the bundled icon decodes");
        assert_eq!(rgba.len(), (width * height * 4) as usize);
    }
}
