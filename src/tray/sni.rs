//! StatusNotifierItem tray (KDE Plasma and other modern panels) via `ksni`.

use std::sync::Arc;
use std::time::Duration;

use futures::Stream;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Category, MenuItem, Status, ToolTip, TrayMethods};
use tokio::sync::{mpsc, watch};

use super::icon::Icons;
use super::model::{Command, Control, Event, STATUSES, TrayModel};

/// How long registering with the StatusNotifierWatcher may take before the
/// tray counts as unavailable.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);

struct SniTray {
    model: TrayModel,
    icons: Arc<Icons>,
    tx: mpsc::UnboundedSender<Event>,
}

impl SniTray {
    fn send(&self, c: Command) {
        let _ = self.tx.send(Event::Command(c));
    }
}

impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        crate::config::APP_ID.to_owned()
    }

    fn title(&self) -> String {
        crate::config::APP_NAME.to_owned()
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    fn status(&self) -> Status {
        if self.model.needs_attention() {
            Status::NeedsAttention
        } else {
            Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.normal.clone()
    }

    /// Red-badge variant shown while there are unread mentions or DMs.
    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.attention.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: self.icons.normal.clone(),
            title: self.model.tooltip_title(),
            description: self.model.tooltip_description(),
        }
    }

    /// Left click.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let m = &self.model;
        let selected = STATUSES.iter().position(|s| *s == m.status).unwrap_or(0);
        vec![
            StandardItem {
                label: m.toggle_label().into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Toggle)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: "Status".into(),
                enabled: m.logged_in,
                submenu: vec![
                    RadioGroup {
                        selected,
                        select: Box::new(|t: &mut Self, i| {
                            if let Some(s) = STATUSES.get(i) {
                                t.send(Command::SetStatus(*s));
                            }
                        }),
                        options: STATUSES
                            .iter()
                            .map(|s| RadioItem {
                                label: s.label().into(),
                                ..Default::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Mute notifications".into(),
                checked: m.muted,
                activate: Box::new(|t: &mut Self| t.send(Command::ToggleMute)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Start minimized to tray".into(),
                checked: m.start_minimized,
                activate: Box::new(|t: &mut Self| t.send(Command::ToggleStartMinimized)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Ferrocord".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_online(&self) {
        let _ = self.tx.send(Event::Available);
    }

    /// The panel went away: tell the app so a hidden window comes back
    /// instead of leaving an unreachable process. The service keeps running
    /// and re-registers when a watcher returns.
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        let _ = self.tx.send(Event::Unavailable);
        true
    }
}

/// Starts the tray on the current tokio runtime and streams its events.
///
/// The first event is `Ready` (registered with a StatusNotifierWatcher) or
/// `Unavailable` (no D-Bus session, no watcher, or a timeout). Dropping the
/// stream shuts the tray down.
pub fn events() -> impl Stream<Item = Event> + Send + 'static {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(tx));
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) })
}

async fn run(tx: mpsc::UnboundedSender<Event>) {
    let Some(icons) = tokio::task::spawn_blocking(Icons::render)
        .await
        .ok()
        .flatten()
    else {
        tracing::warn!("tray icon could not be rendered");
        let _ = tx.send(Event::Unavailable);
        return;
    };
    let (model_tx, mut model_rx) = watch::channel(TrayModel::default());
    let model_tx = Arc::new(model_tx);
    let tray = SniTray {
        model: TrayModel::default(),
        icons: Arc::new(icons),
        tx: tx.clone(),
    };
    let handle = match tokio::time::timeout(REGISTER_TIMEOUT, tray.spawn()).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            tracing::info!(error = %e, "no system tray available");
            let _ = tx.send(Event::Unavailable);
            return;
        }
        Err(_) => {
            tracing::info!("system tray registration timed out");
            let _ = tx.send(Event::Unavailable);
            return;
        }
    };
    tracing::info!("system tray registered");
    let _ = tx.send(Event::Ready(Control::new(model_tx.clone())));
    loop {
        tokio::select! {
            changed = model_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let m = model_rx.borrow_and_update().clone();
                if handle.update(move |t| t.model = m).await.is_none() {
                    let _ = tx.send(Event::Unavailable);
                    break;
                }
            }
            // The subscription was dropped.
            _ = tx.closed() => {
                handle.shutdown().await;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray;

    fn tray(model: TrayModel) -> (SniTray, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let icons = Icons::render().unwrap();
        (
            SniTray {
                model,
                icons: Arc::new(icons),
                tx,
            },
            rx,
        )
    }

    fn next(rx: &mut mpsc::UnboundedReceiver<Event>) -> Command {
        match rx.try_recv().unwrap() {
            Event::Command(c) => c,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn labels(items: &[MenuItem<SniTray>]) -> Vec<String> {
        items
            .iter()
            .filter_map(|i| match i {
                MenuItem::Standard(s) => Some(s.label.clone()),
                MenuItem::Checkmark(c) => Some(c.label.clone()),
                MenuItem::SubMenu(s) => Some(s.label.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn menu_has_the_discord_tray_entries() {
        let (t, _rx) = tray(TrayModel::default());
        assert_eq!(
            labels(&t.menu()),
            [
                "Hide Ferrocord",
                "Status",
                "Mute notifications",
                "Start minimized to tray",
                "Quit Ferrocord"
            ]
        );
    }

    #[test]
    fn menu_items_send_their_commands() {
        let (mut t, mut rx) = tray(TrayModel {
            logged_in: true,
            ..Default::default()
        });
        let menu = t.menu();
        let mut seen = Vec::new();
        for item in &menu {
            match item {
                MenuItem::Standard(s) => (s.activate)(&mut t),
                MenuItem::Checkmark(c) => (c.activate)(&mut t),
                MenuItem::SubMenu(s) => {
                    assert!(s.enabled);
                    let MenuItem::RadioGroup(g) = &s.submenu[0] else {
                        panic!("status submenu is a radio group");
                    };
                    assert_eq!(g.selected, 0);
                    assert_eq!(
                        g.options
                            .iter()
                            .map(|o| o.label.as_str())
                            .collect::<Vec<_>>(),
                        ["Online", "Idle", "Do Not Disturb", "Invisible"]
                    );
                    for i in 0..g.options.len() {
                        (g.select)(&mut t, i);
                    }
                }
                _ => {}
            }
        }
        while let Ok(Event::Command(c)) = rx.try_recv() {
            seen.push(c);
        }
        use crate::net::model::Presence::*;
        assert_eq!(
            seen,
            [
                Command::Toggle,
                Command::SetStatus(Online),
                Command::SetStatus(Idle),
                Command::SetStatus(Dnd),
                Command::SetStatus(Offline),
                Command::ToggleMute,
                Command::ToggleStartMinimized,
                Command::Quit,
            ]
        );
    }

    #[test]
    fn left_click_toggles() {
        let (mut t, mut rx) = tray(TrayModel::default());
        t.activate(0, 0);
        assert_eq!(next(&mut rx), Command::Toggle);
    }

    #[test]
    fn status_submenu_reflects_state() {
        let (t, _rx) = tray(TrayModel {
            logged_in: true,
            status: crate::net::model::Presence::Dnd,
            muted: true,
            ..Default::default()
        });
        let menu = t.menu();
        let MenuItem::SubMenu(s) = &menu[2] else {
            panic!("status submenu")
        };
        let MenuItem::RadioGroup(g) = &s.submenu[0] else {
            panic!("radio group")
        };
        assert_eq!(g.selected, 2);
        let MenuItem::Checkmark(mute) = &menu[3] else {
            panic!("mute")
        };
        assert!(mute.checked);
        let (logged_out, _rx) = tray(TrayModel::default());
        let MenuItem::SubMenu(s) = &logged_out.menu()[2] else {
            panic!("status submenu")
        };
        assert!(!s.enabled);
    }

    #[test]
    fn attention_status_and_tooltip() {
        let (mut t, _rx) = tray(TrayModel::default());
        assert_eq!(t.status(), Status::Active);
        t.model.mentions = 4;
        assert_eq!(t.status(), Status::NeedsAttention);
        assert!(t.tool_tip().title.contains("4 unread mentions"));
        assert_eq!(t.attention_icon_pixmap().len(), 3);
    }

    #[test]
    fn watcher_loss_is_reported_but_keeps_the_service() {
        let (t, mut rx) = tray(TrayModel::default());
        assert!(t.watcher_offline(ksni::OfflineReason::No));
        assert!(matches!(rx.try_recv(), Ok(Event::Unavailable)));
        t.watcher_online();
        assert!(matches!(rx.try_recv(), Ok(Event::Available)));
    }
}
