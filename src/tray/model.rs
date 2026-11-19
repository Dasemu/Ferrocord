//! Platform-independent tray model: state shown by the tray and the
//! commands it can send back.

use std::sync::Arc;

use tokio::sync::watch;

use crate::net::model::Presence;

/// Something the user asked for through the tray icon or its menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Left click.
    Toggle,
    Show,
    Hide,
    SetStatus(Presence),
    ToggleMute,
    ToggleStartMinimized,
    Quit,
}

/// Everything the tray service tells the app.
#[derive(Debug, Clone)]
pub enum Event {
    /// A StatusNotifierWatcher accepted the icon. Carries the handle used to
    /// push state to the tray.
    Ready(Control),
    /// The watcher came back after having gone away.
    Available,
    /// No tray host (there never was one, or the panel went away).
    Unavailable,
    Command(Command),
}

/// What the tray displays. Cheap to compare so republishing an unchanged
/// model costs nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayModel {
    /// Main window currently shown.
    pub visible: bool,
    pub logged_in: bool,
    pub status: Presence,
    pub muted: bool,
    pub start_minimized: bool,
    /// Unread mentions plus unread DMs (the red badge number).
    pub mentions: u32,
}

impl Default for TrayModel {
    fn default() -> Self {
        Self {
            visible: true,
            logged_in: false,
            status: Presence::Online,
            muted: false,
            start_minimized: false,
            mentions: 0,
        }
    }
}

impl TrayModel {
    /// Whether the tray should ask for attention (SNI `NeedsAttention`).
    pub fn needs_attention(&self) -> bool {
        self.mentions > 0
    }

    pub fn tooltip_title(&self) -> String {
        match self.mentions {
            0 => crate::config::APP_NAME.to_owned(),
            n => format!("{} - {}", crate::config::APP_NAME, mentions_label(n)),
        }
    }

    pub fn tooltip_description(&self) -> String {
        if !self.logged_in {
            return "Not signed in".to_owned();
        }
        let mentions = match self.mentions {
            0 => "No unread mentions".to_owned(),
            n => mentions_label(n),
        };
        format!("{mentions}\n{}", self.status.label())
    }

    pub fn toggle_label(&self) -> &'static str {
        if self.visible {
            "Hide Ferrocord"
        } else {
            "Show Ferrocord"
        }
    }
}

fn mentions_label(n: u32) -> String {
    match n {
        1 => "1 unread mention".to_owned(),
        n => format!("{n} unread mentions"),
    }
}

/// The statuses offered by the tray submenu, in menu order.
pub const STATUSES: [Presence; 4] = [
    Presence::Online,
    Presence::Idle,
    Presence::Dnd,
    Presence::Offline,
];

/// App-side handle to the running tray: publishes the latest model.
#[derive(Clone)]
pub struct Control {
    tx: Arc<watch::Sender<TrayModel>>,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Control")
    }
}

impl Control {
    pub fn new(tx: Arc<watch::Sender<TrayModel>>) -> Self {
        Self { tx }
    }

    /// Publishes `model`; a no-op when nothing changed.
    pub fn publish(&self, model: TrayModel) {
        self.tx.send_if_modified(|cur| {
            if *cur == model {
                false
            } else {
                *cur = model;
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_follows_mentions() {
        let mut m = TrayModel::default();
        assert!(!m.needs_attention());
        m.mentions = 3;
        assert!(m.needs_attention());
    }

    #[test]
    fn tooltip_shows_the_mention_count() {
        let mut m = TrayModel {
            logged_in: true,
            ..Default::default()
        };
        assert_eq!(m.tooltip_title(), "Ferrocord");
        assert!(m.tooltip_description().contains("No unread mentions"));
        m.mentions = 1;
        assert_eq!(m.tooltip_title(), "Ferrocord - 1 unread mention");
        m.mentions = 12;
        assert_eq!(m.tooltip_title(), "Ferrocord - 12 unread mentions");
        assert!(m.tooltip_description().contains("12 unread mentions"));
        assert!(m.tooltip_description().contains("Online"));
        m.logged_in = false;
        assert_eq!(m.tooltip_description(), "Not signed in");
    }

    #[test]
    fn toggle_label_follows_visibility() {
        let mut m = TrayModel::default();
        assert_eq!(m.toggle_label(), "Hide Ferrocord");
        m.visible = false;
        assert_eq!(m.toggle_label(), "Show Ferrocord");
    }

    #[test]
    fn publish_only_wakes_receivers_on_change() {
        let (tx, mut rx) = watch::channel(TrayModel::default());
        let c = Control::new(Arc::new(tx));
        rx.mark_unchanged();
        c.publish(TrayModel::default());
        assert!(!rx.has_changed().unwrap());
        c.publish(TrayModel {
            mentions: 2,
            ..Default::default()
        });
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow_and_update().mentions, 2);
    }

    #[test]
    fn statuses_match_the_status_menu() {
        assert_eq!(STATUSES.len(), 4);
        assert_eq!(STATUSES[2].label(), "Do Not Disturb");
        assert_eq!(STATUSES[3].label(), "Invisible");
    }
}
