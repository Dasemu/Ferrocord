//! The StatusNotifierItem service on a private session bus.
//!
//! Needs a bus: run as `dbus-run-session -- cargo test --test tray_sni`.
//! Without `DBUS_SESSION_BUS_ADDRESS` the test skips itself, so a plain
//! `cargo test` stays green on machines without a session bus.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrocord::net::model::Presence;
use ferrocord::tray::{self, Command, Event, TrayModel};
use futures::StreamExt;
use zbus::zvariant::{OwnedValue, Value};

const WAIT: Duration = Duration::from_secs(10);

/// Minimal `org.kde.StatusNotifierWatcher`.
struct Watcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(&self, service: &str) {
        if let Ok(mut items) = self.items.lock() {
            items.push(service.to_owned());
        }
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().map(|i| i.clone()).unwrap_or_default()
    }
}

async fn next(stream: &mut (impl futures::Stream<Item = Event> + Unpin)) -> Event {
    tokio::time::timeout(WAIT, stream.next())
        .await
        .expect("tray event within timeout")
        .expect("tray stream open")
}

type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
type ToolTip = (String, Pixmaps, String, String);
type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn label(props: &HashMap<String, OwnedValue>) -> Option<String> {
    props
        .get("label")
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
}

#[tokio::test(flavor = "multi_thread")]
async fn tray_registers_updates_and_reports_clicks() {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("skipped: no D-Bus session bus (use dbus-run-session)");
        return;
    }

    // 1. No watcher on the bus: the tray reports "unavailable", so the app
    //    keeps "closing the window quits".
    let mut stream = Box::pin(tray::events());
    assert!(matches!(next(&mut stream).await, Event::Unavailable));
    drop(stream);

    // 2. A watcher appears: the item registers.
    let items = Arc::new(Mutex::new(Vec::new()));
    let watcher_conn = zbus::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at(
            "/StatusNotifierWatcher",
            Watcher {
                items: items.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut stream = Box::pin(tray::events());
    let Event::Ready(control) = next(&mut stream).await else {
        panic!("expected Ready");
    };
    let service = items.lock().unwrap().first().cloned().expect("registered");
    assert!(service.starts_with("org.kde.StatusNotifierItem-"));

    // 3. The exported item has the right properties.
    let client = zbus::Connection::session().await.unwrap();
    // SNI signals (NewStatus, ...) do not carry property values, so the
    // proxy must not cache them.
    let item = zbus::proxy::Builder::<zbus::Proxy>::new(&client)
        .destination(service.as_str())
        .unwrap()
        .path("/StatusNotifierItem")
        .unwrap()
        .interface("org.kde.StatusNotifierItem")
        .unwrap()
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    let id: String = item.get_property("Id").await.unwrap();
    assert_eq!(id, "dev.ferro.ferrocord");
    let category: String = item.get_property("Category").await.unwrap();
    assert_eq!(category, "Communications");
    let status: String = item.get_property("Status").await.unwrap();
    assert_eq!(status, "Active");
    let pixmaps: Pixmaps = item.get_property("IconPixmap").await.unwrap();
    assert_eq!(pixmaps.len(), 3);
    assert_eq!(
        pixmaps[0].2.len(),
        (pixmaps[0].0 * pixmaps[0].1 * 4) as usize
    );

    // 4. Unread mentions: attention status, badge icon, count in the tooltip.
    control.publish(TrayModel {
        logged_in: true,
        mentions: 3,
        ..Default::default()
    });
    let mut attention = false;
    for _ in 0..50 {
        let status: String = item.get_property("Status").await.unwrap();
        if status == "NeedsAttention" {
            attention = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(attention, "status becomes NeedsAttention");
    let attention_icons: Pixmaps = item.get_property("AttentionIconPixmap").await.unwrap();
    assert_eq!(attention_icons.len(), 3);
    assert_ne!(
        attention_icons[2].2, pixmaps[2].2,
        "badge differs from icon"
    );
    let tip: ToolTip = item.get_property("ToolTip").await.unwrap();
    assert!(tip.2.contains("3 unread mentions"), "tooltip: {}", tip.2);

    // Reading everything returns to the normal state.
    control.publish(TrayModel {
        logged_in: true,
        ..Default::default()
    });
    let mut active = false;
    for _ in 0..50 {
        let status: String = item.get_property("Status").await.unwrap();
        if status == "Active" {
            active = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(active, "status returns to Active");

    // 5. Left click toggles the window.
    item.call_method("Activate", &(0i32, 0i32)).await.unwrap();
    assert!(matches!(
        next(&mut stream).await,
        Event::Command(Command::Toggle)
    ));

    // 6. The menu (dbusmenu) has the expected entries and they send commands.
    let menu = zbus::Proxy::new(
        &client,
        service.as_str(),
        "/MenuBar",
        "com.canonical.dbusmenu",
    )
    .await
    .unwrap();
    let (_rev, (_root, _props, children)): (u32, Layout) = menu
        .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
        .await
        .unwrap();
    let mut entries: Vec<(i32, String, Vec<OwnedValue>)> = Vec::new();
    for c in children {
        let (cid, props, sub): Layout = c.try_into().unwrap();
        if let Some(l) = label(&props) {
            entries.push((cid, l, sub));
        }
    }
    let names: Vec<&str> = entries.iter().map(|(_, l, _)| l.as_str()).collect();
    assert_eq!(
        names,
        [
            "Hide Ferrocord",
            "Status",
            "Mute notifications",
            "Start minimized to tray",
            "Quit Ferrocord"
        ]
    );
    let click = |id: i32| {
        let menu = menu.clone();
        async move {
            let _: () = menu
                .call("Event", &(id, "clicked", Value::from(0i32), 0u32))
                .await
                .unwrap();
        }
    };
    let id_of = |name: &str| entries.iter().find(|(_, l, _)| l == name).unwrap().0;
    click(id_of("Mute notifications")).await;
    assert!(matches!(
        next(&mut stream).await,
        Event::Command(Command::ToggleMute)
    ));
    click(id_of("Quit Ferrocord")).await;
    assert!(matches!(
        next(&mut stream).await,
        Event::Command(Command::Quit)
    ));
    // The status radio entries are the Status submenu's children.
    let status_children = &entries.iter().find(|(_, l, _)| l == "Status").unwrap().2;
    assert_eq!(status_children.len(), 4);
    let (dnd_id, dnd_props, _): Layout =
        status_children[2].try_clone().unwrap().try_into().unwrap();
    assert_eq!(label(&dnd_props).as_deref(), Some("Do Not Disturb"));
    click(dnd_id).await;
    assert!(matches!(
        next(&mut stream).await,
        Event::Command(Command::SetStatus(Presence::Dnd))
    ));

    // 7. The panel goes away: the app is told, so a hidden window returns.
    watcher_conn.graceful_shutdown().await;
    assert!(matches!(next(&mut stream).await, Event::Unavailable));
}
