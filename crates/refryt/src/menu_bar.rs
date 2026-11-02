//! macOS: the app's own menus in the menu bar, beside the app menu: Playback, View, Window and
//! Help, the way a Mac music app is expected to carry them. Before this the bar held only the
//! app menu winit puts there (About, Hide, Quit), so nothing the app does could be found there or
//! given a ⌘ shortcut a Mac user would look for.
//!
//! Built with `muda`, which `tray-icon` already brings in (`tray_icon::menu`). Choosing an item
//! queues a [`Command`] and wakes the UI, and [`App::logic`](crate::app::App) drains the queue,
//! the same handoff the tray menu uses (see `tray`'s module doc). The two share muda's one
//! process-wide event handler, which `tray::install_menu_handler` installs and routes from.
//!
//! The Window menu's items are AppKit's own. Minimize goes through `miniaturize`, which keeps
//! egui's idea of the window intact (see `chrome::minimize`), and Close goes through the
//! window's close request, so "Hide tray on close" applies to ⌘W as it does to the red button.
//!
//! There is no Edit menu: egui's text fields take ⌘C, ⌘V, ⌘X and ⌘A as key presses, and a menu
//! holding those shortcuts would take the keys before the field could.

use std::collections::VecDeque;
use std::sync::Mutex;

use tray_icon::menu::accelerator::{Accelerator, Code, Modifiers};
use tray_icon::menu::{AboutMetadata, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

/// Where Help sends a reader who wants to know more.
pub const WEBSITE: &str = "https://github.com/gridoe/refryt";

/// What choosing one of the app's menu items asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    Mute,
    Shuffle,
    Repeat,
    Home,
    Explore,
    Library,
    History,
    Search,
    Queue,
    Lyrics,
    FullscreenPlayer,
    WindowFullscreen,
    Settings,
    Shortcuts,
    Website,
}

impl Command {
    const ALL: [Command; 20] = [
        Command::PlayPause,
        Command::Next,
        Command::Previous,
        Command::VolumeUp,
        Command::VolumeDown,
        Command::Mute,
        Command::Shuffle,
        Command::Repeat,
        Command::Home,
        Command::Explore,
        Command::Library,
        Command::History,
        Command::Search,
        Command::Queue,
        Command::Lyrics,
        Command::FullscreenPlayer,
        Command::WindowFullscreen,
        Command::Settings,
        Command::Shortcuts,
        Command::Website,
    ];

    /// The menu item's id: prefixed, so it can never be taken for one of the tray's.
    fn id(self) -> &'static str {
        match self {
            Command::PlayPause => "ytm-menu-play-pause",
            Command::Next => "ytm-menu-next",
            Command::Previous => "ytm-menu-previous",
            Command::VolumeUp => "ytm-menu-volume-up",
            Command::VolumeDown => "ytm-menu-volume-down",
            Command::Mute => "ytm-menu-mute",
            Command::Shuffle => "ytm-menu-shuffle",
            Command::Repeat => "ytm-menu-repeat",
            Command::Home => "ytm-menu-home",
            Command::Explore => "ytm-menu-explore",
            Command::Library => "ytm-menu-library",
            Command::History => "ytm-menu-history",
            Command::Search => "ytm-menu-search",
            Command::Queue => "ytm-menu-queue",
            Command::Lyrics => "ytm-menu-lyrics",
            Command::FullscreenPlayer => "ytm-menu-fullscreen-player",
            Command::WindowFullscreen => "ytm-menu-window-fullscreen",
            Command::Settings => "ytm-menu-settings",
            Command::Shortcuts => "ytm-menu-shortcuts",
            Command::Website => "ytm-menu-website",
        }
    }

    fn from_id(id: &str) -> Option<Command> {
        Command::ALL.into_iter().find(|c| c.id() == id)
    }

    /// Whether the command is about a page or the window rather than the music, so a window
    /// hidden to the tray comes back for it. Next or Mute from the menu bar can stay out of
    /// sight; Library cannot.
    pub fn shows_window(self) -> bool {
        !matches!(
            self,
            Command::PlayPause
                | Command::Next
                | Command::Previous
                | Command::VolumeUp
                | Command::VolumeDown
                | Command::Mute
                | Command::Shuffle
                | Command::Repeat
                | Command::Website
        )
    }
}

/// Commands chosen from the menus, drained by [`poll`].
static COMMANDS: Mutex<VecDeque<Command>> = Mutex::new(VecDeque::new());

/// Queue the command behind a menu id, if the id is one of these menus'. Called from the shared
/// menu event handler; returns whether the id was ours.
pub fn dispatch(id: &str, ctx: &egui::Context) -> bool {
    let Some(command) = Command::from_id(id) else {
        return false;
    };
    if let Ok(mut queue) = COMMANDS.lock() {
        queue.push_back(command);
    }
    ctx.request_repaint();
    true
}

/// The next command chosen from the menus, if any.
pub fn poll() -> Option<Command> {
    COMMANDS.lock().ok()?.pop_front()
}

/// What the menus show of the app, kept so that only a change reaches AppKit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub has_track: bool,
    pub playing: bool,
    pub muted: bool,
    pub shuffle: bool,
    /// The repeat mode's name: Off, All or One.
    pub repeat: &'static str,
    pub queue_open: bool,
    pub lyrics_open: bool,
    pub fullscreen_player: bool,
    /// The window is fullscreen, by any means.
    pub window_fullscreen: bool,
    pub signed_in: bool,
    /// A text field has the keyboard. ⌘← ⌘→ ⌘↑ ⌘↓ move the caret there, so the items holding
    /// those shortcuts are disabled until typing stops.
    pub typing: bool,
}

/// The menus' changing items, kept to update them.
pub struct MenuBar {
    /// Holds the menus alive: AppKit's bar refers to them.
    _menu: Menu,
    play: MenuItem,
    next: MenuItem,
    previous: MenuItem,
    volume_up: MenuItem,
    volume_down: MenuItem,
    mute: MenuItem,
    shuffle: CheckMenuItem,
    repeat: MenuItem,
    history: MenuItem,
    queue: CheckMenuItem,
    lyrics: CheckMenuItem,
    fullscreen_player: MenuItem,
    window_fullscreen: MenuItem,
    shown: Option<State>,
}

fn cmd(code: Code) -> Option<Accelerator> {
    Some(Accelerator::new(Modifiers::META, code))
}

fn item(command: Command, text: &str, accelerator: Option<Accelerator>) -> MenuItem {
    MenuItem::with_id(command.id(), text, true, accelerator)
}

fn check(command: Command, text: &str, accelerator: Option<Accelerator>) -> CheckMenuItem {
    CheckMenuItem::with_id(command.id(), text, true, false, accelerator)
}

impl MenuBar {
    /// Put the menus in the menu bar, replacing the app menu winit made. Call on the main
    /// thread once the event loop is running (from `App::new`); `None` if AppKit would not take
    /// them, in which case winit's own app menu stays.
    pub fn install(ctx: &egui::Context) -> Option<MenuBar> {
        crate::tray::install_menu_handler(ctx);
        let separator = PredefinedMenuItem::separator;

        let about = PredefinedMenuItem::about(
            Some("About Refryt"),
            Some(AboutMetadata {
                name: Some("Refryt".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                ..Default::default()
            }),
        );
        let settings = item(Command::Settings, "Settings…", cmd(Code::Comma));
        let app_menu = Submenu::with_items(
            "Refryt",
            true,
            &[
                &about,
                &separator(),
                &settings,
                &separator(),
                &PredefinedMenuItem::services(None),
                &separator(),
                &PredefinedMenuItem::hide(Some("Hide Refryt")),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &separator(),
                &PredefinedMenuItem::quit(Some("Quit Refryt")),
            ],
        )
        .ok()?;

        // Space, the arrows on their own, M and F stay the app's own keys and are not given to
        // the menus: a menu holding a plain key takes it from every text field.
        let play = item(Command::PlayPause, "Play", None);
        let next = item(Command::Next, "Next", cmd(Code::ArrowRight));
        let previous = item(Command::Previous, "Previous", cmd(Code::ArrowLeft));
        let volume_up = item(Command::VolumeUp, "Volume Up", cmd(Code::ArrowUp));
        let volume_down = item(Command::VolumeDown, "Volume Down", cmd(Code::ArrowDown));
        let mute = item(Command::Mute, "Mute", None);
        let shuffle = check(Command::Shuffle, "Shuffle", cmd(Code::KeyS));
        let repeat = item(Command::Repeat, "Repeat: Off", cmd(Code::KeyR));
        let playback = Submenu::with_items(
            "Playback",
            true,
            &[
                &play,
                &separator(),
                &next,
                &previous,
                &separator(),
                &volume_up,
                &volume_down,
                &mute,
                &separator(),
                &shuffle,
                &repeat,
            ],
        )
        .ok()?;

        let history = item(Command::History, "History", cmd(Code::Digit4));
        let queue = check(Command::Queue, "Show Queue", None);
        let lyrics = check(Command::Lyrics, "Show Lyrics", None);
        let fullscreen_player = item(Command::FullscreenPlayer, "Now Playing", None);
        // Our own item rather than AppKit's: AppKit's takes the window to a Space of its own,
        // black behind a see-through window, where everything else in the app uses the simple
        // fullscreen of `window_fullscreen`. ⌃⌘F, as in every Mac app.
        let window_fullscreen = item(
            Command::WindowFullscreen,
            "Enter Full Screen",
            Some(Accelerator::new(
                Modifiers::CONTROL | Modifiers::META,
                Code::KeyF,
            )),
        );
        // With an item of our own, AppKit would add its own Enter Full Screen beside it.
        objc2_foundation::NSUserDefaults::standardUserDefaults().setBool_forKey(
            false,
            objc2_foundation::ns_string!("NSFullScreenMenuItemEverywhere"),
        );
        let view = Submenu::with_items(
            "View",
            true,
            &[
                &item(Command::Home, "Home", cmd(Code::Digit1)),
                &item(Command::Explore, "Explore", cmd(Code::Digit2)),
                &item(Command::Library, "Library", cmd(Code::Digit3)),
                &history,
                &separator(),
                &item(Command::Search, "Search", cmd(Code::KeyF)),
                &separator(),
                &queue,
                &lyrics,
                &fullscreen_player,
                &separator(),
                &window_fullscreen,
            ],
        )
        .ok()?;

        let window = Submenu::with_items(
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::zoom(None),
                &separator(),
                &PredefinedMenuItem::close_window(None),
                &separator(),
                &PredefinedMenuItem::bring_all_to_front(None),
            ],
        )
        .ok()?;

        let help = Submenu::with_items(
            "Help",
            true,
            &[
                &item(Command::Shortcuts, "Keyboard Shortcuts", None),
                &item(Command::Website, "Refryt on GitHub", None),
            ],
        )
        .ok()?;

        let menu = Menu::with_items(&[&app_menu, &playback, &view, &window, &help]).ok()?;
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();

        Some(MenuBar {
            _menu: menu,
            play,
            next,
            previous,
            volume_up,
            volume_down,
            mute,
            shuffle,
            repeat,
            history,
            queue,
            lyrics,
            fullscreen_player,
            window_fullscreen,
            shown: None,
        })
    }

    /// Bring the items' labels, checks and shortcuts in line with the app. Cheap when nothing
    /// changed, so it is called every frame; only what changed reaches AppKit.
    pub fn update(&mut self, state: State) {
        let old = self.shown.take();
        let changed = |f: fn(&State) -> bool| old.as_ref().is_none_or(|o| f(o) != f(&state));

        if changed(|s| s.playing) {
            self.play
                .set_text(if state.playing { "Pause" } else { "Play" });
        }
        if changed(|s| s.has_track) || changed(|s| s.typing) {
            // While typing, the items holding ⌘← ⌘→ ⌘↑ ⌘↓ are disabled, which is what makes AppKit
            // pass those keys on to the field: muda cannot take a shortcut off an item once set.
            let keys = !state.typing;
            self.play.set_enabled(state.has_track);
            self.next.set_enabled(state.has_track && keys);
            self.previous.set_enabled(state.has_track && keys);
            self.volume_up.set_enabled(keys);
            self.volume_down.set_enabled(keys);
            self.fullscreen_player.set_enabled(state.has_track);
        }
        if changed(|s| s.muted) {
            self.mute
                .set_text(if state.muted { "Unmute" } else { "Mute" });
        }
        if changed(|s| s.shuffle) {
            self.shuffle.set_checked(state.shuffle);
        }
        if old.as_ref().is_none_or(|o| o.repeat != state.repeat) {
            self.repeat.set_text(format!("Repeat: {}", state.repeat));
        }
        if changed(|s| s.signed_in) {
            self.history.set_enabled(state.signed_in);
        }
        if changed(|s| s.queue_open) {
            self.queue.set_checked(state.queue_open);
        }
        if changed(|s| s.lyrics_open) {
            self.lyrics.set_checked(state.lyrics_open);
        }
        if changed(|s| s.fullscreen_player) {
            // Not "full screen": that is the window's own, Enter Full Screen just below.
            self.fullscreen_player.set_text(if state.fullscreen_player {
                "Close Now Playing"
            } else {
                "Now Playing"
            });
        }
        if changed(|s| s.window_fullscreen) {
            self.window_fullscreen.set_text(if state.window_fullscreen {
                "Exit Full Screen"
            } else {
                "Enter Full Screen"
            });
        }
        self.shown = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_round_trips_through_its_id() {
        for command in Command::ALL {
            assert_eq!(Command::from_id(command.id()), Some(command));
        }
    }

    #[test]
    fn the_ids_are_unique_and_never_a_trays() {
        let mut ids: Vec<_> = Command::ALL.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Command::ALL.len());
        assert!(ids.iter().all(|id| id.starts_with("ytm-menu-")));
        assert_eq!(Command::from_id("ytm-tray-quit"), None);
    }

    #[test]
    fn music_commands_leave_a_hidden_window_hidden() {
        assert!(!Command::Next.shows_window());
        assert!(!Command::Mute.shows_window());
        assert!(Command::Library.shows_window());
        assert!(Command::Settings.shows_window());
    }
}
