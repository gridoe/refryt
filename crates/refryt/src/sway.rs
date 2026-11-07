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
            while let Ok(hidden) = rx.recv() {
                if let Err(e) = run(hidden) {
                    tracing::debug!(error = %format!("{e:#}"), hidden, "sway: no window change");
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
}
