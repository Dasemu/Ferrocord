//! Main-window lifecycle and tray preferences (pure, unit-tested).
//!
//! The app runs as an `iced::daemon`: the main window is opened and closed
//! on demand. Hiding to the tray CLOSES the window so the renderer and its
//! surfaces are freed; the app state stays in memory and the network
//! connection stays up.

use iced::{Point, Size};

use crate::store::Store;
use crate::tray::Command;

use super::message::Ui;

/// Whether a StatusNotifierWatcher (tray host) accepted our icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayHost {
    /// Still registering.
    Unknown,
    Available,
    /// No tray: closing the window quits, as in a normal app.
    Unavailable,
}

/// What the app has to do to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Open,
    Close,
    /// Bring an already visible window to the front.
    Raise,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    /// The main window exists (is shown).
    pub visible: bool,
    pub host: TrayHost,
    /// Closing the window hides it instead of quitting (while a tray exists).
    pub close_to_tray: bool,
}

impl WindowState {
    /// Initial state. With `tray` and `start_hidden` no window is opened;
    /// it appears if no tray host turns up (never an unreachable process).
    pub fn boot(tray: bool, start_hidden: bool, close_to_tray: bool) -> (Self, Action) {
        let hidden = tray && start_hidden;
        let state = Self {
            visible: !hidden,
            host: if tray {
                TrayHost::Unknown
            } else {
                TrayHost::Unavailable
            },
            close_to_tray,
        };
        let action = if hidden { Action::None } else { Action::Open };
        (state, action)
    }

    pub fn tray_available(&self) -> bool {
        self.host == TrayHost::Available
    }

    /// The tray registered (or came back).
    pub fn tray_ready(&mut self) {
        self.host = TrayHost::Available;
    }

    /// The tray is gone: a hidden window must come back.
    pub fn tray_lost(&mut self) -> Action {
        self.host = TrayHost::Unavailable;
        if self.visible {
            Action::None
        } else {
            self.visible = true;
            Action::Open
        }
    }

    pub fn show(&mut self) -> Action {
        if self.visible {
            Action::Raise
        } else {
            self.visible = true;
            Action::Open
        }
    }

    /// Hide from the tray menu. Without a tray there is nothing to bring
    /// the window back, so it stays.
    pub fn hide(&mut self) -> Action {
        if self.visible && self.tray_available() {
            self.visible = false;
            Action::Close
        } else {
            Action::None
        }
    }

    pub fn toggle(&mut self) -> Action {
        if self.visible {
            self.hide()
        } else {
            self.show()
        }
    }

    /// The user closed the window (title bar, Alt+F4).
    pub fn close_requested(&mut self) -> Action {
        if self.visible && self.close_to_tray && self.tray_available() {
            self.visible = false;
            Action::Close
        } else {
            Action::Exit
        }
    }
}

/// Maps a tray command to the UI message that carries it out.
pub fn command_to_ui(c: Command) -> Ui {
    match c {
        Command::Toggle => Ui::ToggleWindow,
        Command::Show => Ui::ShowWindow,
        Command::Hide => Ui::HideWindow,
        Command::SetStatus(p) => Ui::SetStatus(p),
        Command::ToggleMute => Ui::ToggleMuted,
        Command::ToggleStartMinimized => Ui::ToggleStartMinimized,
        Command::Quit => Ui::Quit,
    }
}

/// Window size and position to restore when the window is shown again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub size: Size,
    /// Unknown on Wayland, where clients cannot read or set it.
    pub position: Option<Point>,
}

pub const DEFAULT_SIZE: Size = Size::new(1280.0, 800.0);
pub const MIN_SIZE: Size = Size::new(940.0, 500.0);

/// Settings for (re)opening the main window.
pub fn window_settings(resizable: bool, last: Option<Geometry>) -> iced::window::Settings {
    let size = last.map_or(DEFAULT_SIZE, |g| {
        Size::new(
            g.size.width.max(MIN_SIZE.width),
            g.size.height.max(MIN_SIZE.height),
        )
    });
    let position = last.and_then(|g| g.position).map_or(
        iced::window::Position::Default,
        iced::window::Position::Specific,
    );
    iced::window::Settings {
        size,
        position,
        min_size: Some(MIN_SIZE),
        resizable,
        // The app decides between "hide to tray" and "quit".
        exit_on_close_request: false,
        platform_specific: iced::window::settings::PlatformSpecific {
            #[cfg(target_os = "linux")]
            application_id: crate::config::APP_ID.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// User preferences related to the tray. Persisted in the SQLite `meta`
/// table (keys survive logout, see `Store::clear`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefs {
    pub close_to_tray: bool,
    pub start_minimized: bool,
    /// Tray "Mute notifications": no desktop notifications (badges stay).
    pub muted: bool,
    /// "Reduce motion": avatars, emoji, decorations and profile effects
    /// stay still.
    pub reduce_motion: bool,
    /// "Play GIFs automatically": GIFs, animated emoji and stickers in
    /// messages play while the window is focused (the official client's
    /// default). Off: they animate only while their message is hovered.
    pub autoplay_gifs: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            start_minimized: false,
            muted: false,
            reduce_motion: false,
            autoplay_gifs: true,
        }
    }
}

pub const KEY_CLOSE_TO_TRAY: &str = "pref.close_to_tray";
pub const KEY_START_MINIMIZED: &str = "pref.start_minimized";
pub const KEY_MUTED: &str = "pref.muted";
pub const KEY_REDUCE_MOTION: &str = "pref.reduce_motion";
pub const KEY_AUTOPLAY_GIFS: &str = "pref.autoplay_gifs";

impl Prefs {
    pub fn load(store: &Store) -> Self {
        let d = Self::default();
        let flag = |key: &str, default: bool| match store.meta(key) {
            Ok(Some(v)) => v == "1",
            _ => default,
        };
        Self {
            close_to_tray: flag(KEY_CLOSE_TO_TRAY, d.close_to_tray),
            start_minimized: flag(KEY_START_MINIMIZED, d.start_minimized),
            muted: flag(KEY_MUTED, d.muted),
            reduce_motion: flag(KEY_REDUCE_MOTION, d.reduce_motion),
            autoplay_gifs: flag(KEY_AUTOPLAY_GIFS, d.autoplay_gifs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::Presence;

    fn visible_with_tray() -> WindowState {
        let (mut w, a) = WindowState::boot(true, false, true);
        assert_eq!(a, Action::Open);
        w.tray_ready();
        w
    }

    #[test]
    fn visible_hidden_shown() {
        let mut w = visible_with_tray();
        assert_eq!(w.close_requested(), Action::Close);
        assert!(!w.visible);
        assert_eq!(w.show(), Action::Open);
        assert!(w.visible);
        assert_eq!(w.show(), Action::Raise);
        assert_eq!(w.toggle(), Action::Close);
        assert_eq!(w.toggle(), Action::Open);
    }

    #[test]
    fn closing_quits_without_a_tray_host() {
        let (mut w, _) = WindowState::boot(true, false, true);
        // Host not registered yet.
        assert_eq!(w.close_requested(), Action::Exit);
        w.tray_ready();
        w.tray_lost();
        assert_eq!(w.close_requested(), Action::Exit);
        let (mut w, a) = WindowState::boot(false, true, true);
        assert_eq!(a, Action::Open, "no tray: --minimized cannot hide");
        assert_eq!(w.close_requested(), Action::Exit);
    }

    #[test]
    fn close_to_tray_can_be_turned_off() {
        let (mut w, _) = WindowState::boot(true, false, false);
        w.tray_ready();
        assert_eq!(w.close_requested(), Action::Exit);
        // The tray menu can still hide it.
        assert_eq!(w.hide(), Action::Close);
    }

    #[test]
    fn minimized_start_opens_no_window() {
        let (mut w, a) = WindowState::boot(true, true, true);
        assert_eq!(a, Action::None);
        assert!(!w.visible);
        w.tray_ready();
        assert!(!w.visible);
        assert_eq!(w.show(), Action::Open);
    }

    #[test]
    fn minimized_start_without_a_tray_host_shows_the_window() {
        let (mut w, _) = WindowState::boot(true, true, true);
        assert_eq!(w.tray_lost(), Action::Open);
        assert!(w.visible);
        assert_eq!(w.host, TrayHost::Unavailable);
    }

    #[test]
    fn losing_the_tray_brings_back_a_hidden_window() {
        let mut w = visible_with_tray();
        assert_eq!(w.hide(), Action::Close);
        assert_eq!(w.tray_lost(), Action::Open);
        assert!(w.visible);
        // A visible window just stays when the tray disappears.
        let mut v = visible_with_tray();
        assert_eq!(v.tray_lost(), Action::None);
        // The panel returning makes hiding possible again.
        v.tray_ready();
        assert_eq!(v.hide(), Action::Close);
    }

    #[test]
    fn hide_needs_a_tray() {
        let (mut w, _) = WindowState::boot(false, false, true);
        assert_eq!(w.hide(), Action::None);
        assert_eq!(w.toggle(), Action::None);
        assert!(w.visible);
    }

    #[test]
    fn tray_commands_map_to_messages() {
        use Command::*;
        assert!(matches!(command_to_ui(Toggle), Ui::ToggleWindow));
        assert!(matches!(command_to_ui(Show), Ui::ShowWindow));
        assert!(matches!(command_to_ui(Hide), Ui::HideWindow));
        assert!(matches!(
            command_to_ui(SetStatus(Presence::Dnd)),
            Ui::SetStatus(Presence::Dnd)
        ));
        assert!(matches!(command_to_ui(ToggleMute), Ui::ToggleMuted));
        assert!(matches!(
            command_to_ui(ToggleStartMinimized),
            Ui::ToggleStartMinimized
        ));
        assert!(matches!(command_to_ui(Quit), Ui::Quit));
    }

    #[test]
    fn reopening_restores_geometry_within_limits() {
        let s = window_settings(true, None);
        assert_eq!(s.size, DEFAULT_SIZE);
        assert!(matches!(s.position, iced::window::Position::Default));
        assert!(!s.exit_on_close_request);
        let g = Geometry {
            size: Size::new(1000.0, 650.0),
            position: Some(Point::new(40.0, 60.0)),
        };
        let s = window_settings(true, Some(g));
        assert_eq!(s.size, g.size);
        assert!(matches!(
            s.position,
            iced::window::Position::Specific(p) if p == Point::new(40.0, 60.0)
        ));
        let tiny = Geometry {
            size: Size::new(10.0, 10.0),
            position: None,
        };
        assert_eq!(window_settings(true, Some(tiny)).size, MIN_SIZE);
    }

    #[test]
    fn prefs_default_and_roundtrip() {
        let store = Store::open(None).unwrap();
        assert_eq!(Prefs::load(&store), Prefs::default());
        assert!(Prefs::default().close_to_tray);
        store.set_meta(KEY_CLOSE_TO_TRAY, "0").unwrap();
        store.set_meta(KEY_START_MINIMIZED, "1").unwrap();
        store.set_meta(KEY_MUTED, "1").unwrap();
        let p = Prefs::load(&store);
        assert!(!p.close_to_tray && p.start_minimized && p.muted);
    }

    #[test]
    fn prefs_survive_logout() {
        let store = Store::open(None).unwrap();
        store.set_meta(KEY_MUTED, "1").unwrap();
        store.set_meta("selected_channel", "42").unwrap();
        store.clear().unwrap();
        assert!(Prefs::load(&store).muted);
        assert_eq!(store.meta("selected_channel").unwrap(), None);
    }
}
