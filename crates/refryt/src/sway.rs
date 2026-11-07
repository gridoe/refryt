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
//! tray's hide/show, driven here through `swaymsg` with an `app_id` criterion so only this
//! process's window is touched. It runs only on Sway, and only on this process's own window.
//!
//! Best-effort throughout: off Sway, without `swaymsg` on `PATH`, or from a sandbox without
//! the IPC socket, the call fails quietly and the caller falls back to the minimise
//! best-effort, as it was before.

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use anyhow::{Context, Result};

/// Whether this session is Sway's, where `swaymsg` can move the window.
pub fn is_sway() -> bool {
    std::env::var_os("SWAYSOCK").is_some_and(|sock| !sock.is_empty())
        || std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktop| desktop.split(':').any(|d| d.eq_ignore_ascii_case("sway")))
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

/// The `swaymsg` criterion matching this process's main window only: eframe names the
/// window's `app_id` after the crate (`"app_id": "refryt"` in `swaymsg -t get_tree`).
fn criteria() -> String {
    "[app_id=\"refryt\"]".to_owned()
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
            .map(|tree| find_window(&tree))
            .unwrap_or_default()
    }
}

/// The window's state, if it is anywhere in the tree: fullscreen when its `fullscreen_mode`
/// is anything but off, floating when it sits under a `floating_nodes` list.
fn find_window(tree: &serde_json::Value) -> HiddenState {
    fn walk(node: &serde_json::Value, floating: bool) -> Option<HiddenState> {
        if node.get("app_id").and_then(|id| id.as_str()) == Some("refryt") {
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
                    if let Some(found) = walk(kid, floating || key == "floating_nodes") {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    walk(tree, false).unwrap_or_default()
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
        untilt()?;
    }
    Ok(())
}

/// Fullscreen the window again after the scratchpad dropped it on the way in.
fn set_fullscreen() -> Result<()> {
    sway_op("fullscreen enable")
}

/// Retile the window after the scratchpad showed it back floating.
fn untilt() -> Result<()> {
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
    fn the_criterion_targets_this_app_id_only() {
        assert_eq!(criteria(), "[app_id=\"refryt\"]");
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
                {"app_id": "foot", "fullscreen_mode": 1, "nodes": [], "floating_nodes": []},
                {"app_id": "refryt", "fullscreen_mode": 0, "nodes": [], "floating_nodes": []},
            ],
            "floating_nodes": [],
        });
        let left = find_window(&tree);
        assert!(!left.fullscreen && !left.floating);
        let tree = serde_json::json!({
            "nodes": [],
            "floating_nodes": [
                {"app_id": "refryt", "fullscreen_mode": 1, "nodes": [], "floating_nodes": []},
            ],
        });
        let left = find_window(&tree);
        assert!(left.fullscreen && left.floating);
    }
}
