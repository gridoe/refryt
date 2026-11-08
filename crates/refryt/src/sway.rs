//! Sway (and wlroots compositors driven by `swaymsg`): hide the app's own window to the tray
//! and bring it back, through the sway IPC.
//!
//! Wayland has no hidden window, and Sway ignores minimise requests entirely: the `--tray-test`
//! hide/show/quit run leaves the window mapped and visible throughout on Sway 1.12, where
//! Plasma at least minimises. `set_minimized(false)` is a no-op everywhere on Wayland, so
//! without help a tray click on Sway hides nothing and could show nothing back either.
//!
//! Sway's own answer to hiding is the scratchpad: `move scratchpad` takes the window off the
//! workspace, `scratchpad show` brings it back focused on the current one. That is exactly the
//! tray's hide/show, driven here through `swaymsg` with a `pid` criterion so only this
//! process's window is touched, whatever its `app_id` (the Flatpak's is its own id, not
//! "refryt"). It runs only for a native Wayland window on Sway: under XWayland the window
//! hides with `Visible(false)` like any other X11 one.
//!
//! Where `swaymsg` cannot run at all (not on `PATH`, or inside the Flatpak sandbox),
//! [`handles_hiding`] says no and the caller keeps the minimise best-effort, as before. A
//! single call that fails after that is logged at debug level and changes nothing.

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use anyhow::{Context, Result};

/// Whether this session is Sway's, where `swaymsg` can move the window.
fn is_sway() -> bool {
    std::env::var_os("SWAYSOCK").is_some_and(|sock| !sock.is_empty())
        || std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktop| desktop.split(':').any(|d| d.eq_ignore_ascii_case("sway")))
}

/// Whether hiding to the tray goes through the scratchpad here: a native Wayland window
/// (`wayland`) on Sway, with a `swaymsg` that runs.
pub fn handles_hiding(wayland: bool) -> bool {
    wayland && is_sway() && available()
}

/// Whether `swaymsg` runs from this process at all. Asked once: the answer cannot change while
/// the app runs.
fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("swaymsg")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// Hide this process's window to the tray (`true`), or bring it back focused (`false`).
/// Returns at once: the calls go to one worker thread in order, so a quick hide and show cannot
/// land the wrong way round. A failure is logged at debug level only, since off Sway it is
/// expected.
pub fn set_hidden(hidden: bool) {
    static WORKER: OnceLock<Sender<bool>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = channel::<bool>();
        std::thread::spawn(move || {
            // How the window left, so the way out can put it back: Sway drops fullscreen on
            // the way into the scratchpad, shows everything back floating, and forgets the
            // tiling slot — none of which survives without asking Sway itself.
            let mut left = HiddenState::default();
            while let Ok(hidden) = rx.recv() {
                if hidden {
                    left = HiddenState::read();
                }
                match run(hidden) {
                    Ok(()) if !hidden => {
                        let back = std::mem::take(&mut left);
                        if let Err(e) = restore(back) {
                            tracing::debug!(error = %format!("{e:#}"), "sway: no state back");
                        }
                    }
                    Ok(()) => {}
                    Err(e) => {
                        tracing::debug!(error = %format!("{e:#}"), hidden, "sway: no window change");
                    }
                }
            }
        });
        tx
    });
    let _ = worker.send(hidden);
}

/// The `swaymsg` criterion matching this process's window only. The pid, not the `app_id`:
/// that is the Flatpak id inside the sandbox, and another program could share "refryt".
fn criteria() -> String {
    criteria_for(std::process::id())
}

fn criteria_for(pid: u32) -> String {
    format!("[pid={pid}]")
}

/// The scratchpad operation for each direction: away on hide, back and focused on show.
fn op(hidden: bool) -> &'static str {
    if hidden {
        "move scratchpad"
    } else {
        "scratchpad show"
    }
}

fn run(hidden: bool) -> Result<()> {
    let out = std::process::Command::new("swaymsg")
        .arg(criteria())
        .arg(op(hidden))
        .output()
        .context("could not run swaymsg")?;
    if !out.status.success() {
        anyhow::bail!(
            "swaymsg refused the window change: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// How the window looked going into the scratchpad, read from Sway itself: the app is never
/// told the compositor-side state (there is no event for it), and only Sway knows it.
#[derive(Default)]
struct HiddenState {
    fullscreen: bool,
    floating: bool,
}

impl HiddenState {
    fn read() -> Self {
        let Ok(out) = std::process::Command::new("swaymsg")
            .arg("-t")
            .arg("get_tree")
            .output()
        else {
            return Self::default();
        };
        if !out.status.success() {
            return Self::default();
        }
        serde_json::from_slice(&out.stdout)
            .map(|tree| find_window(&tree, std::process::id()))
            .unwrap_or_default()
    }
}

/// The state of the window `pid` owns, if it is anywhere in the tree: fullscreen when its
/// `fullscreen_mode` is anything but off, floating when it sits under a `floating_nodes` list.
fn find_window(tree: &serde_json::Value, pid: u32) -> HiddenState {
    fn walk(node: &serde_json::Value, pid: u32, floating: bool) -> Option<HiddenState> {
        if node.get("pid").and_then(|p| p.as_u64()) == Some(u64::from(pid)) {
            return Some(HiddenState {
                fullscreen: node
                    .get("fullscreen_mode")
                    .and_then(|mode| mode.as_u64())
                    .unwrap_or(0)
                    != 0,
                floating,
            });
        }
        for key in ["nodes", "floating_nodes"] {
            if let Some(kids) = node.get(key).and_then(|k| k.as_array()) {
                for kid in kids {
                    if let Some(found) = walk(kid, pid, floating || key == "floating_nodes") {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    walk(tree, pid, false).unwrap_or_default()
}

/// Put the window back the way it left: fullscreen again when it left fullscreen (the
/// scratchpad drops that on the way in), tiled again when it left tiled (the scratchpad
/// shows everything back floating). A window that left floating keeps the geometry Sway
/// stored with it, so it needs nothing.
fn restore(back: HiddenState) -> Result<()> {
    if back.fullscreen {
        return set_fullscreen();
    }
    if !back.floating {
        retile()?;
    }
    Ok(())
}

/// Fullscreen the window again after the scratchpad dropped it on the way in.
fn set_fullscreen() -> Result<()> {
    sway_op("fullscreen enable")
}

/// Retile the window after the scratchpad showed it back floating.
fn retile() -> Result<()> {
    sway_op("floating disable")
}

fn sway_op(op: &str) -> Result<()> {
    let out = std::process::Command::new("swaymsg")
        .arg(criteria())
        .arg(op)
        .output()
        .context("could not run swaymsg")?;
    if !out.status.success() {
        anyhow::bail!(
            "swaymsg refused {op}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_criterion_targets_this_process_only() {
        assert_eq!(criteria_for(4242), "[pid=4242]");
        assert_eq!(criteria(), format!("[pid={}]", std::process::id()));
    }

    #[test]
    fn an_x11_window_never_goes_through_sway() {
        assert!(!handles_hiding(false));
    }

    #[test]
    fn hiding_moves_away_and_showing_brings_back() {
        assert_eq!(op(true), "move scratchpad");
        assert_eq!(op(false), "scratchpad show");
    }

    #[test]
    fn fullscreen_is_read_off_our_window_only() {
        let tree = serde_json::json!({
            "nodes": [
                {"pid": 7, "app_id": "foot", "fullscreen_mode": 1, "nodes": [], "floating_nodes": []},
                {"pid": 42, "app_id": "io.github.gridoe.refryt", "fullscreen_mode": 0, "nodes": [], "floating_nodes": []},
                {"pid": 9, "app_id": "refryt", "fullscreen_mode": 1, "nodes": [], "floating_nodes": []},
            ],
            "floating_nodes": [],
        });
        let left = find_window(&tree, 42);
        assert!(!left.fullscreen && !left.floating);
        let tree = serde_json::json!({
            "nodes": [],
            "floating_nodes": [
                {"pid": 42, "app_id": "refryt", "fullscreen_mode": 1, "nodes": [], "floating_nodes": []},
            ],
        });
        let left = find_window(&tree, 42);
        assert!(left.fullscreen && left.floating);
    }
}
