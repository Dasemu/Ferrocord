//! Quit leaves on time, whatever is still running.
//!
//! v0.17.0 hung on "Quit" from the tray (KDE marked the window "not
//! responding"): iced drops its tokio runtime when the daemon exits, and a
//! tokio runtime waits for every `spawn_blocking` task when dropped. A
//! notification that was still open (KDE keeps them in its history) had a
//! blocking task waiting for its click, forever.
//!
//! Two tests:
//! * `open_notification_does_not_block_runtime_shutdown`: the real
//!   notification code against a server that never closes anything; the
//!   runtime must still drop at once (needs a session bus).
//! * `tray_quit_exits_within_deadline`: the real binary (`--mock`), a stuck
//!   blocking call and a settings save that never answers
//!   (`FERROCORD_MOCK_HANG=1`), Quit clicked in the tray menu over D-Bus
//!   like a panel does; the process must be gone within the deadline
//!   (needs a session bus and an X/Wayland display).
//!
//! Run: `dbus-run-session -- xvfb-run -a cargo test --test quit_deadline`.
//! Without a bus (or display) the tests skip themselves.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedValue, Value};

fn have_bus() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("skipped: no D-Bus session bus (use dbus-run-session)");
        return false;
    }
    true
}

/// A notification server that, like Plasma's history, never closes a
/// notification by itself.
struct NeverCloses {
    shown: Arc<AtomicU32>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NeverCloses {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app_name: &str,
        _replaces_id: u32,
        _app_icon: &str,
        _summary: &str,
        _body: &str,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        self.shown.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn close_notification(&self, _id: u32) {}

    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".into(), "body".into(), "persistence".into()]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("Plasma".into(), "KDE".into(), "5.27".into(), "1.2".into())
    }
}

#[test]
fn open_notification_does_not_block_runtime_shutdown() {
    if !have_bus() {
        return;
    }
    let shown = Arc::new(AtomicU32::new(0));
    let _server = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at(
            "/org/freedesktop/Notifications",
            NeverCloses {
                shown: shown.clone(),
            },
        )
        .unwrap()
        .build()
        .unwrap();

    // Everything on a helper thread: a regression fails the test instead of
    // hanging it.
    let (tx, rx) = std::sync::mpsc::channel();
    let seen = shown.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // As the app does: shown from inside the runtime.
        let clicked = {
            let _guard = rt.enter();
            ferrocord::app::notify::show("roberto vc".into(), "hi".into(), false)
        };
        rt.block_on(async {
            let start = Instant::now();
            while seen.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(5) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let start = Instant::now();
        drop(rt);
        let _ = tx.send(start.elapsed());
        drop(clicked);
    });
    let took = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the runtime shuts down while a notification is open");
    assert_eq!(
        shown.load(Ordering::SeqCst),
        1,
        "the notification was shown"
    );
    assert!(
        took < Duration::from_millis(500),
        "runtime drop took {took:?}"
    );
}

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

    async fn register_status_notifier_host(&self, _service: &str) {}

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

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// Finds a menu entry's id by label.
fn find(layout: Layout, want: &str) -> Option<i32> {
    let (id, props, children) = layout;
    let label = props
        .get("label")
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok());
    if label.as_deref() == Some(want) {
        return Some(id);
    }
    children
        .into_iter()
        .filter_map(|c| Layout::try_from(c).ok())
        .find_map(|c| find(c, want))
}

/// Starts `ferrocord --mock` with a tray, clicks "Quit Ferrocord" in its
/// menu and returns how long the process took to exit after the click.
fn quit_from_tray(env: &[(&str, &str)]) -> Duration {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let items = Arc::new(Mutex::new(Vec::new()));
        let _watcher = zbus::connection::Builder::session()
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

        let home = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ferrocord"));
        cmd.arg("--mock")
            .env("HOME", home.path())
            .env("XDG_RUNTIME_DIR", run.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_STATE_HOME", home.path().join("state"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("ICED_BACKEND", "tiny-skia")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();

        // Wait for the tray icon, then give the window a moment to open.
        let start = Instant::now();
        let service = loop {
            if let Some(s) = items.lock().unwrap().first().cloned() {
                break s;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("ferrocord exited early: {status}");
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "the tray never registered"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(child.try_wait().unwrap().is_none(), "still running");

        let client = zbus::Connection::session().await.unwrap();
        let menu = zbus::Proxy::new(
            &client,
            service.as_str(),
            "/MenuBar",
            "com.canonical.dbusmenu",
        )
        .await
        .unwrap();
        let (_rev, layout): (u32, Layout) = menu
            .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
            .await
            .unwrap();
        let quit = find(layout, "Quit Ferrocord").expect("Quit entry");
        let clicked = Instant::now();
        let _: () = menu
            .call("Event", &(quit, "clicked", Value::from(0i32), 0u32))
            .await
            .unwrap();
        loop {
            if child.try_wait().unwrap().is_some() {
                break clicked.elapsed();
            }
            if clicked.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("ferrocord still running 10 s after Quit");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

/// The process must be gone this soon after Quit (the exit watchdog fires
/// at 600 ms; the rest is slack for slow CI machines).
const DEADLINE: Duration = Duration::from_millis(1500);

#[test]
fn tray_quit_exits_within_deadline() {
    if !have_bus() {
        return;
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipped: no display (use xvfb-run)");
        return;
    }
    // Both in one test: they share the bus name of the fake watcher.
    let normal = quit_from_tray(&[]);
    assert!(normal < DEADLINE, "plain quit took {normal:?}");
    let stuck = quit_from_tray(&[("FERROCORD_MOCK_HANG", "1")]);
    assert!(
        stuck < DEADLINE,
        "quit with a stuck blocking call and an unanswered save took {stuck:?}"
    );
}
