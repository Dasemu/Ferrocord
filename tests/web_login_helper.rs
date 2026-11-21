//! The `ferrocord-login` helper protocol against fake helpers.
//!
//! The fake helper is this test binary itself: `web_login::spawn` runs it
//! with a libtest filter naming one `fake_*` test, which acts as a helper
//! (only when `FERROCORD_LOGIN_DATA_DIR` is set, i.e. never in a normal run).

use std::ffi::OsString;
use std::io::Write;
use std::time::Duration;

use ferrocord::auth::secrets::Secret;
use ferrocord::auth::web_login::{self, HelperMsg, WebLoginEvent, WebLoginFailure, code};
use futures::StreamExt;

fn token() -> String {
    [
        "MTIzNDU2Nzg5MDEyMzQ1Njc4",
        "GabCdE",
        "x".repeat(27).as_str(),
    ]
    .join(".")
}

/// Runs only inside a spawned fake: returns the profile directory.
fn as_helper() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os(web_login::DATA_DIR_ENV)?;
    let dir = std::path::PathBuf::from(dir);
    assert!(dir.is_dir(), "the app creates the profile directory");
    // Something for the wipe to remove, like a real webview profile.
    std::fs::create_dir_all(dir.join("storage")).unwrap();
    std::fs::write(dir.join("storage").join("localstorage.sqlite3"), b"x").unwrap();
    // libtest has printed "test fake_x ... " without a newline.
    println!();
    Some(dir)
}

fn say(msg: &HelperMsg) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", msg.to_line()).unwrap();
    out.flush().unwrap();
}

#[test]
fn fake_token() {
    if as_helper().is_none() {
        return;
    }
    say(&HelperMsg::Ready);
    println!("some noise the app must ignore");
    say(&HelperMsg::Token(Secret::new(token())));
    std::process::exit(0);
}

#[test]
fn fake_closed() {
    if as_helper().is_none() {
        return;
    }
    say(&HelperMsg::Ready);
    say(&HelperMsg::Closed);
    std::process::exit(0);
}

#[test]
fn fake_no_webview() {
    if as_helper().is_none() {
        return;
    }
    say(&HelperMsg::Error {
        code: code::NO_WEBVIEW.into(),
        message: "WebView2 runtime not found".into(),
    });
    std::process::exit(1);
}

#[test]
fn fake_crash() {
    if as_helper().is_none() {
        return;
    }
    say(&HelperMsg::Ready);
    eprintln!("boom");
    std::process::exit(3);
}

#[test]
fn fake_hang() {
    if as_helper().is_none() {
        return;
    }
    say(&HelperMsg::Ready);
    std::thread::sleep(Duration::from_secs(60));
    std::process::exit(0);
}

fn fake(name: &str) -> (Option<std::path::PathBuf>, Vec<OsString>) {
    (
        Some(std::env::current_exe().unwrap()),
        [name, "--exact", "--nocapture", "--test-threads=1"]
            .into_iter()
            .map(OsString::from)
            .collect(),
    )
}

async fn run(name: &str) -> Vec<WebLoginEvent> {
    let (p, a) = fake(name);
    let events: Vec<_> = tokio::time::timeout(
        Duration::from_secs(60),
        web_login::spawn(p, a).collect::<Vec<_>>(),
    )
    .await
    .expect("the stream ends after a terminal event");
    events
}

/// Profile directories this process created that still exist.
fn leftover_dirs() -> Vec<std::path::PathBuf> {
    let prefix = format!("ferrocord-login-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect()
}

async fn wait_wiped() {
    for _ in 0..100 {
        if leftover_dirs().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("profile directory left behind: {:?}", leftover_dirs());
}

// One test drives every scenario, in order, so the leftover-directory check
// is not confused by parallel runs.
#[tokio::test]
async fn helper_protocol_end_to_end() {
    // Token: Opened, then Token; the profile directory is wiped first.
    let ev = run("fake_token").await;
    assert_eq!(
        ev,
        vec![
            WebLoginEvent::Opened,
            WebLoginEvent::Token(Secret::new(token()))
        ]
    );
    assert!(
        leftover_dirs().is_empty(),
        "wiped before the token is handed over"
    );

    // The user closed the window.
    assert_eq!(
        run("fake_closed").await,
        vec![WebLoginEvent::Opened, WebLoginEvent::Closed]
    );
    wait_wiped().await;

    // No system webview.
    assert_eq!(
        run("fake_no_webview").await,
        vec![WebLoginEvent::Failed(WebLoginFailure::NoWebEngine)]
    );
    wait_wiped().await;

    // Crash after opening: reported with the exit code.
    assert_eq!(
        run("fake_crash").await,
        vec![
            WebLoginEvent::Opened,
            WebLoginEvent::Failed(WebLoginFailure::Crashed("exit code 3".into()))
        ]
    );
    wait_wiped().await;

    // Cancel: dropping the stream kills the helper and wipes the profile.
    let (p, a) = fake("fake_hang");
    let mut s = Box::pin(web_login::spawn(p, a));
    let first = tokio::time::timeout(Duration::from_secs(60), s.next())
        .await
        .unwrap();
    assert_eq!(first, Some(WebLoginEvent::Opened));
    drop(s);
    wait_wiped().await;

    // Missing helper.
    assert_eq!(
        web_login::spawn(None, Vec::new()).collect::<Vec<_>>().await,
        vec![WebLoginEvent::Failed(WebLoginFailure::Missing)]
    );
    let nowhere = std::env::temp_dir().join("ferrocord-no-such-helper-binary");
    assert_eq!(
        web_login::spawn(Some(nowhere), Vec::new())
            .collect::<Vec<_>>()
            .await,
        vec![WebLoginEvent::Failed(WebLoginFailure::Missing)]
    );
    wait_wiped().await;
}

#[cfg(feature = "web-login")]
#[test]
fn the_real_helper_is_built_next_to_the_app() {
    // `cargo test` builds every binary of the package.
    assert!(web_login::helper_path().is_some());
}
