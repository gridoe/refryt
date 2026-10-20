//! The frameless window: rounded background, custom titlebar, drag and resize.
//!
//! We ask the OS for a borderless window — transparent where the compositor blends it and opaque
//! on Windows — and paint everything ourselves. That buys
//! the look, and costs us the behaviour a titlebar normally provides for free — dragging,
//! resizing from the edges, double-click to maximise. All of that lives here.
//!
//! macOS is the exception to "borderless". There the window is a normal native one whose title
//! bar is hidden and whose content runs underneath it (`main.rs`), so the system draws the
//! rounded corners, the shadow and the blur clipping, and keeps them correct when the window is
//! zoomed. The app therefore paints a plain, square background on macOS; see `App::ui`. The
//! window buttons there are the system's own traffic lights too, moved into the shell's
//! top-left corner by [`TrafficLights`]; only the drag area in this module still applies.
//!
//! `--decorated` turns the whole thing off and hands the window back to the OS. That escape hatch
//! is not optional: X11 without a compositor paints the rounded corners black, and tiling window
//! managers have their own opinions.

use egui::{
    Color32, CornerRadius, Rect, ResizeDirection, Sense, Stroke, Ui, ViewportCommand, vec2,
};

use crate::icons::Icon;
use crate::theme::{self, Palette};

/// Paint the window background: a rounded, outlined rect, or a plain fill when the window is
/// maximised or decorated (rounded corners on a maximised window look like a mistake).
pub fn paint_background(ui: &Ui, palette: &Palette, rounded: bool) {
    let rect = ui.ctx().content_rect();
    let radius = if rounded {
        CornerRadius::same(theme::WINDOW_RADIUS)
    } else {
        CornerRadius::ZERO
    };
    ui.painter().rect(
        rect,
        radius,
        // The one place the shell's opacity is applied: everything else is painted on top of
        // this. The stroke below keeps full alpha — the 1px edge is what tells a translucent
        // window from a hole in the desktop.
        palette.window_fill(),
        if rounded {
            Stroke::new(1.0, palette.outline)
        } else {
            Stroke::NONE
        },
        egui::StrokeKind::Inside,
    );
}

/// Invisible grab bands around the window edge that start a native resize.
///
/// Corners are checked before edges so the diagonal directions win in the overlap, which is what
/// makes dragging a corner feel right.
pub fn resize_handles(ui: &Ui) {
    let ctx = ui.ctx();
    let rect = ctx.content_rect();
    let h = theme::RESIZE_HANDLE;

    let corners = [
        (
            Rect::from_min_size(rect.left_top(), vec2(h, h)),
            ResizeDirection::NorthWest,
            egui::CursorIcon::ResizeNwSe,
        ),
        (
            Rect::from_min_size(rect.right_top() - vec2(h, 0.0), vec2(h, h)),
            ResizeDirection::NorthEast,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            Rect::from_min_size(rect.left_bottom() - vec2(0.0, h), vec2(h, h)),
            ResizeDirection::SouthWest,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            Rect::from_min_size(rect.right_bottom() - vec2(h, h), vec2(h, h)),
            ResizeDirection::SouthEast,
            egui::CursorIcon::ResizeNwSe,
        ),
    ];
    let edges = [
        (
            Rect::from_min_size(rect.left_top(), vec2(rect.width(), h)),
            ResizeDirection::North,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            Rect::from_min_size(rect.left_bottom() - vec2(0.0, h), vec2(rect.width(), h)),
            ResizeDirection::South,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            Rect::from_min_size(rect.left_top(), vec2(h, rect.height())),
            ResizeDirection::West,
            egui::CursorIcon::ResizeHorizontal,
        ),
        (
            Rect::from_min_size(rect.right_top() - vec2(h, 0.0), vec2(h, rect.height())),
            ResizeDirection::East,
            egui::CursorIcon::ResizeHorizontal,
        ),
    ];

    for (i, (zone, dir, cursor)) in corners.iter().chain(edges.iter()).enumerate() {
        let id = egui::Id::new("ytm-resize").with(i);
        let r = ui.interact(*zone, id, Sense::drag());
        if r.hovered() || r.dragged() {
            ctx.set_cursor_icon(*cursor);
        }
        if r.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(*dir));
        }
    }
}

/// One window button. Close gets a red hover; the others a neutral one.
fn window_button(ui: &mut Ui, palette: &Palette, icon: Icon, danger: bool) -> egui::Response {
    let size = vec2(32.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        if hovered {
            let bg = if danger {
                palette.danger
            } else {
                palette.glass(palette.surface_hover)
            };
            ui.painter()
                .rect_filled(rect, CornerRadius::same(theme::RADIUS_SMALL), bg);
        }
        // At rest they follow the window's focus, as a native title bar's do: full strength on
        // the window you're using, quieter on one in the background.
        let focused = ui.ctx().input(|i| i.viewport().focused.unwrap_or(true));
        let fg = if hovered && danger {
            Color32::WHITE
        } else if hovered || focused {
            palette.text
        } else {
            // The faintest text tone, not the second: in several themes that one sits so close to
            // the text colour that a background window's buttons looked no different.
            palette.dim
        };
        icon.image(fg, 14.0)
            .paint_at(ui, Rect::from_center_size(rect.center(), vec2(14.0, 14.0)));
    }
    response
}

/// Whether the window buttons are the system's own rather than drawn by [`window_buttons`]:
/// on macOS the traffic lights stay, placed by [`TrafficLights`].
pub const NATIVE_WINDOW_BUTTONS: bool = cfg!(target_os = "macos");

/// Make a rect drag the window, and double-click maximise it.
///
/// Installed before any control in the bar, so widgets painted afterwards take the pointer
/// first — anything interactive drawn on top swallows the drag, which is what we want. `id_salt`
/// tells apart the surfaces drawn in the same frame (the top bar's and, on macOS, the strip
/// above the sidebar).
pub fn drag_surface(ui: &Ui, rect: Rect, decorated: bool, id_salt: &str) {
    if decorated {
        return;
    }
    let ctx = ui.ctx().clone();
    let drag = ui.interact(
        rect,
        egui::Id::new("ytm-titlebar-drag").with(id_salt),
        Sense::click_and_drag(),
    );
    if drag.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        // macOS lets the user pick what a title-bar double-click does (Desktop & Dock, "Double-
        // click a window's title bar to"); a title bar of our own should do the same.
        #[cfg(target_os = "macos")]
        if macos::double_click_minimizes() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            crate::pacing::minimize_requested();
            return;
        } else if !macos::double_click_zooms() {
            return;
        }
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
}

/// Minimise, maximise and close, for the current layout's leading edge.
///
/// Drawn into whatever `Ui` is passed, so the caller decides where they sit; in practice that is
/// the right-to-left end of the top bar, which is the window's top-right corner.
///
/// Returns whether the close button was clicked, rather than acting on it directly: whether
/// that means actually closing or hiding to the tray depends on a setting only the caller
/// (which has `&mut App`) can see — see `App::request_close`.
pub fn window_buttons(ui: &mut Ui, palette: &Palette) -> bool {
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let close_clicked = window_button(ui, palette, Icon::X, true).clicked();
    if window_button(ui, palette, Icon::Square, false).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if window_button(ui, palette, Icon::Minus, false).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        crate::pacing::minimize_requested();
    }
    close_clicked
}

/// The system's window buttons on macOS — close, minimise, zoom — moved from the stock 28pt
/// title bar down into the shell's top-left corner, so they line up with the top bar the way a
/// unified toolbar's do in Music or Finder. A no-op everywhere else.
///
/// AppKit lays the title bar out again whenever it likes (a resize, a change of key window), and
/// puts the buttons back where it wants them, so [`TrafficLights::place`] is called every frame;
/// it only touches AppKit when something is out of place. This is the same thing Electron's
/// `trafficLightPosition` does.
#[derive(Default)]
pub struct TrafficLights {
    #[cfg(target_os = "macos")]
    view: Option<objc2::rc::Retained<objc2_app_kit::NSView>>,
}

impl TrafficLights {
    pub fn new(handles: &impl raw_window_handle::HasWindowHandle) -> Self {
        #[cfg(target_os = "macos")]
        {
            use raw_window_handle::RawWindowHandle;
            let view = handles.window_handle().ok().and_then(|h| match h.as_raw() {
                // SAFETY: eframe hands out the window's live content `NSView`, on the main
                // thread, while the window exists.
                RawWindowHandle::AppKit(handle) => unsafe {
                    objc2::rc::Retained::retain(
                        handle.ns_view.as_ptr().cast::<objc2_app_kit::NSView>(),
                    )
                },
                _ => None,
            });
            Self { view }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = handles;
            Self {}
        }
    }

    /// Put the buttons with the close button's left edge at `left` and all three centred on
    /// `center_y`, both in points from the window's top-left; or hide them, for a fullscreen
    /// window that has nowhere to put them.
    pub fn place(&self, left: f32, center_y: f32, visible: bool) {
        #[cfg(target_os = "macos")]
        if let Some(view) = &self.view {
            macos::place_traffic_lights(view, left.into(), center_y.into(), visible);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (left, center_y, visible);
    }

    /// How wide the three buttons are together, in points, for laying out around them.
    pub const WIDTH: f32 = 14.0 + 2.0 * 20.0;
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_app_kit::{NSView, NSWindowButton, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSUserDefaults, ns_string};

    /// The user's "Double-click a window's title bar to" choice. Unset means zoom.
    fn double_click_action() -> Option<String> {
        NSUserDefaults::standardUserDefaults()
            .stringForKey(ns_string!("AppleActionOnDoubleClick"))
            .map(|s| s.to_string())
    }

    pub fn double_click_minimizes() -> bool {
        double_click_action().as_deref() == Some("Minimize")
    }

    pub fn double_click_zooms() -> bool {
        double_click_action().as_deref() != Some("None")
    }

    pub fn place_traffic_lights(content: &NSView, left: f64, center_y: f64, visible: bool) {
        let Some(window) = content.window() else {
            return;
        };
        // In native fullscreen the system shows its own title bar on hover, buttons and all;
        // moving them there would only fight it.
        if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
            return;
        }
        let buttons = [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ]
        .map(|b| window.standardWindowButton(b));
        let [Some(close), Some(mini), Some(zoom)] = buttons else {
            return;
        };
        for button in [&close, &mini, &zoom] {
            if button.isHidden() == visible {
                button.setHidden(!visible);
            }
        }
        if !visible {
            return;
        }

        // The buttons live in the title bar view, which lives in the title bar container; the
        // container is what has to grow for the buttons to sit lower than the stock 28pt.
        // SAFETY: plain view-hierarchy reads on the main thread (`App::ui` runs there).
        let Some(container) =
            unsafe { close.superview() }.and_then(|titlebar| unsafe { titlebar.superview() })
        else {
            return;
        };
        let size = close.frame().size;
        // The spacing AppKit chose, kept as is: it differs between macOS releases.
        let step = mini.frame().origin.x - close.frame().origin.x;
        let height = (2.0 * center_y).max(size.height);
        let window_size = window.frame().size;
        // The container sits in the window's frame view, whose origin is the bottom-left.
        let frame = NSRect::new(
            NSPoint::new(0.0, window_size.height - height),
            NSSize::new(window_size.width, height),
        );
        if container.frame() != frame {
            container.setFrame(frame);
        }
        let y = ((height - size.height) / 2.0).round();
        for (i, button) in [&close, &mini, &zoom].into_iter().enumerate() {
            let origin = NSPoint::new(left + i as f64 * step, y);
            if button.frame().origin != origin {
                button.setFrameOrigin(origin);
            }
        }
    }
}
