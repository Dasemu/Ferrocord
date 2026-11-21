//! "Log in with Discord's page": Discord's own login page in a small helper
//! window (`ferrocord-login`, a system webview: WebView2 on Windows,
//! WebKitGTK on Linux).
//!
//! Why a helper: Discord protects logins with hCaptcha, which only runs on
//! Discord's own domain, in a real browser engine. Rendering the challenge on
//! a page of our own (a local file, `127.0.0.1`) is refused by hCaptcha's
//! domain check, and faking the origin is out of the question. So the user
//! logs in on `https://discord.com/login` itself, in an engine the OS already
//! has; Discord shows any captcha, 2FA prompt or QR code there exactly as in a
//! browser. When the page has logged in, the helper reads the session token
//! the page stored (its own `localStorage`, in this private, throw-away
//! profile) and hands it to the app over a pipe. The app validates it,
//! stores it in the keyring only, and wipes the helper's data directory.
//!
//! The main binary links no browser engine: the helper is a separate process
//! that exits after login (the same pattern as `ferrocord-player`).
//!
//! Wire format, helper → app, one JSON object per stdout line:
//!
//! ```text
//! {"type":"ready"}                       window and page are up
//! {"type":"token","token":"…"}           the page logged in (then exits)
//! {"type":"closed"}                      the user closed the window
//! {"type":"error","code":"no-webview","message":"…"}
//! ```
//!
//! Lines that are not such objects are ignored. The data directory is given
//! in the `FERROCORD_LOGIN_DATA_DIR` environment variable.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::mpsc;

use super::secrets::Secret;

/// Name of the helper executable (without `.exe`).
pub const HELPER_NAME: &str = "ferrocord-login";
/// The only page the helper opens.
pub const LOGIN_URL: &str = "https://discord.com/login";
/// The only origin a token is accepted from.
pub const DISCORD_ORIGIN: &str = "https://discord.com";
/// Environment variable carrying the helper's profile directory.
pub const DATA_DIR_ENV: &str = "FERROCORD_LOGIN_DATA_DIR";
/// Longest accepted helper line (a token line is ~150 bytes).
const MAX_LINE: usize = 8 * 1024;

/// Script the helper injects at document start in every frame. It only
/// acts in the top-level `https://discord.com` document: it keeps a
/// reference to the page's `localStorage` (Discord's app removes the global
/// later) and, once the page has stored a session token after the user
/// logged in, posts it once to the helper. It changes nothing on the page.
pub const INIT_SCRIPT: &str = r#"(function () {
  if (window.top !== window || location.origin !== "https://discord.com") return;
  var store, ipc = window.ipc;
  try { store = window.localStorage; } catch (e) { return; }
  if (!store || !ipc || typeof ipc.postMessage !== "function") return;
  var post = ipc.postMessage.bind(ipc);
  var timer = setInterval(function () {
    var raw;
    try { raw = store.getItem("token"); } catch (e) { return; }
    if (!raw) return;
    var token = raw;
    try { token = JSON.parse(raw); } catch (e) {}
    if (typeof token !== "string" || !token) return;
    clearInterval(timer);
    post(JSON.stringify({ type: "token", token: token }));
  }, 500);
})();"#;

/// Helper → app message.
#[derive(Clone, PartialEq)]
pub enum HelperMsg {
    Ready,
    Token(Secret),
    Closed,
    Error { code: String, message: String },
}

impl std::fmt::Debug for HelperMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HelperMsg::Ready => f.write_str("Ready"),
            HelperMsg::Token(_) => f.write_str("Token([REDACTED])"),
            HelperMsg::Closed => f.write_str("Closed"),
            HelperMsg::Error { code, .. } => write!(f, "Error({code})"),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Wire {
    Ready,
    Token {
        token: String,
    },
    Closed,
    Error {
        #[serde(default)]
        code: String,
        #[serde(default)]
        message: String,
    },
}

/// Error codes the helper reports.
pub mod code {
    /// No usable system webview (WebView2 runtime / WebKitGTK missing).
    pub const NO_WEBVIEW: &str = "no-webview";
    /// Anything else (window creation, bad arguments).
    pub const FAILED: &str = "failed";
}

impl HelperMsg {
    /// One protocol line (without the newline).
    pub fn to_line(&self) -> String {
        let w = match self {
            HelperMsg::Ready => Wire::Ready,
            HelperMsg::Token(t) => Wire::Token {
                token: t.expose().to_owned(),
            },
            HelperMsg::Closed => Wire::Closed,
            HelperMsg::Error { code, message } => Wire::Error {
                code: code.clone(),
                message: message.clone(),
            },
        };
        serde_json::to_string(&w).unwrap_or_default()
    }

    /// Parses one helper line; `None` for anything else (noise, garbage,
    /// tokens that cannot be Discord tokens).
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.len() > MAX_LINE || !line.starts_with('{') {
            return None;
        }
        Some(match serde_json::from_str::<Wire>(line).ok()? {
            Wire::Ready => HelperMsg::Ready,
            Wire::Token { token } => HelperMsg::Token(valid_token(&token)?),
            Wire::Closed => HelperMsg::Closed,
            Wire::Error { code, message } => HelperMsg::Error {
                code: clean(&code, 32),
                message: clean(&message, 300),
            },
        })
    }
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// A token as Discord's page stores it: 3 dot-separated base64url parts
/// (or an `mfa.` token), visible ASCII, bounded.
pub fn valid_token(t: &str) -> Option<Secret> {
    let ok = (30..=512).contains(&t.len())
        && t.contains('.')
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    ok.then(|| Secret::new(t))
}

/// Checks an IPC message from the page: it must come from the top-level
/// `https://discord.com` document (`page_url` is the URL the webview
/// reports for the sender) and be `{"type":"token","token":"…"}`.
pub fn parse_page_message(page_url: &str, body: &str) -> Option<Secret> {
    if !is_discord_page(page_url) || body.len() > MAX_LINE {
        return None;
    }
    match serde_json::from_str::<Wire>(body).ok()? {
        Wire::Token { token } => valid_token(&token),
        _ => None,
    }
}

/// Whether `url` is a page on exactly `https://discord.com` (default port).
pub fn is_discord_page(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str() == Some("discord.com")
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

/// Top-level navigations the helper allows: HTTPS (Discord, its CDN,
/// hCaptcha, help pages) and `about:blank`. Everything else (file:, data:,
/// custom schemes, plain HTTP) is refused.
pub fn navigation_allowed(url: &str) -> bool {
    url == "about:blank" || url::Url::parse(url).is_ok_and(|u| u.scheme() == "https")
}

/// What the login screen shows for a helper run.
#[derive(Debug, Clone, PartialEq)]
pub enum WebLoginEvent {
    /// Discord's page is open in the helper window.
    Opened,
    /// The page logged in; validate and store this token.
    Token(Secret),
    /// The user closed the window without logging in.
    Closed,
    Failed(WebLoginFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebLoginFailure {
    /// The helper is not installed next to `ferrocord`.
    Missing,
    /// No system webview: WebKitGTK (Linux) / WebView2 runtime (Windows).
    NoWebEngine,
    /// The helper stopped unexpectedly (detail has no secrets).
    Crashed(String),
}

impl WebLoginFailure {
    pub fn user_message(&self) -> String {
        match self {
            WebLoginFailure::Missing => format!(
                "Discord's login page could not open: {HELPER_NAME} is missing next to \
                 Ferrocord. Reinstall Ferrocord, or scan the QR code instead."
            ),
            WebLoginFailure::NoWebEngine => no_web_engine_message().into(),
            WebLoginFailure::Crashed(d) => format!(
                "Discord's login page closed unexpectedly ({d}). Try again, or scan the QR code."
            ),
        }
    }
}

fn no_web_engine_message() -> &'static str {
    if cfg!(windows) {
        "Discord's login page needs the Microsoft Edge WebView2 Runtime (part of Windows 11). \
         Install it from Microsoft, then try again."
    } else {
        "Discord's login page needs WebKitGTK 4.1, which is not installed. Install it \
         (Debian/Ubuntu: libwebkit2gtk-4.1-0, Fedora: webkit2gtk4.1, Arch: webkit2gtk-4.1), \
         then try again."
    }
}

/// Maps how a helper ended without a terminal message to a failure.
/// `stderr_tail` is the last stderr output, `code` the exit code.
pub fn classify_exit(code: Option<i32>, stderr_tail: &str) -> WebLoginFailure {
    let s = stderr_tail.to_ascii_lowercase();
    if s.contains("libwebkit2gtk")
        || s.contains("libjavascriptcoregtk")
        || s.contains("libgtk-3")
        || s.contains("error while loading shared libraries")
        || code == Some(127)
    {
        return WebLoginFailure::NoWebEngine;
    }
    WebLoginFailure::Crashed(match code {
        Some(c) => format!("exit code {c}"),
        None => "terminated".into(),
    })
}

/// The helper inside `dir`, if it is there.
pub fn helper_in(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(format!("{HELPER_NAME}{}", std::env::consts::EXE_SUFFIX));
    p.is_file().then_some(p)
}

/// The helper executable: next to this binary, else on `PATH`.
pub fn helper_path() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        if let Some(p) = helper_in(dir) {
            return Some(p);
        }
        // `cargo test` runs from target/debug/deps.
        if let Some(p) = dir.parent().and_then(helper_in) {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| helper_in(&d))
}

/// A fresh, private profile directory for one helper run.
pub fn new_data_dir() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "ferrocord-login-{}-{:016x}",
        std::process::id(),
        fastrand::u64(..)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

/// Removes the profile directory. WebView2 child processes can hold files
/// for a moment after the helper exits, so this retries for a few seconds.
pub async fn wipe_data_dir(dir: &Path) {
    for _ in 0..20 {
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    tracing::warn!("could not remove the login window's data directory");
}

/// Runs the helper once. The stream ends after a terminal event (token,
/// closed, failure); dropping it kills the helper. The data directory is
/// wiped when the helper is gone, in every case.
pub fn spawn(
    program: Option<PathBuf>,
    args: Vec<OsString>,
) -> impl Stream<Item = WebLoginEvent> + Send + 'static {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        let Some(program) = program else {
            let _ = tx
                .send(WebLoginEvent::Failed(WebLoginFailure::Missing))
                .await;
            return;
        };
        let dir = match new_data_dir() {
            Ok(d) => d,
            Err(e) => {
                let _ = tx
                    .send(WebLoginEvent::Failed(WebLoginFailure::Crashed(format!(
                        "no temporary directory: {}",
                        e.kind()
                    ))))
                    .await;
                return;
            }
        };
        let end = run(&program, &args, &dir, &tx).await;
        wipe_data_dir(&dir).await;
        if let Some(ev) = end {
            let _ = tx.send(ev).await;
        }
    });
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) })
}

/// Drives one helper process; returns the terminal event to send after the
/// data directory is wiped (or `None` when the app stopped listening).
async fn run(
    program: &Path,
    args: &[OsString],
    dir: &Path,
    tx: &mpsc::Sender<WebLoginEvent>,
) -> Option<WebLoginEvent> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .env(DATA_DIR_ENV, dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(WebLoginEvent::Failed(WebLoginFailure::Missing));
        }
        Err(e) => {
            return Some(WebLoginEvent::Failed(WebLoginFailure::Crashed(format!(
                "cannot start: {}",
                e.kind()
            ))));
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill().await;
        return Some(WebLoginEvent::Failed(WebLoginFailure::Crashed(
            "no pipes".into(),
        )));
    };
    // Keep the end of stderr (loader errors name the missing library).
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.take(64 * 1024).read_to_end(&mut buf).await;
        let s = String::from_utf8_lossy(&buf);
        let tail: String = s.chars().rev().take(600).collect();
        tail.chars().rev().collect::<String>()
    });
    let mut lines = BufReader::new(stdout.take(1 << 20)).lines();
    let terminal = loop {
        tokio::select! {
            _ = tx.closed() => {
                let _ = child.kill().await;
                return None;
            }
            line = lines.next_line() => match line {
                Ok(Some(l)) => match HelperMsg::parse(&l) {
                    Some(HelperMsg::Ready) => {
                        let _ = tx.send(WebLoginEvent::Opened).await;
                    }
                    Some(HelperMsg::Token(t)) => break Some(WebLoginEvent::Token(t)),
                    Some(HelperMsg::Closed) => break Some(WebLoginEvent::Closed),
                    Some(HelperMsg::Error { code: c, message }) => {
                        tracing::warn!(code = %c, detail = %message, "login helper failed");
                        break Some(WebLoginEvent::Failed(if c == code::NO_WEBVIEW {
                            WebLoginFailure::NoWebEngine
                        } else {
                            WebLoginFailure::Crashed(if message.is_empty() { c } else { message })
                        }));
                    }
                    None => {}
                },
                // EOF or a broken pipe: the helper is gone.
                Ok(None) | Err(_) => break None,
            }
        }
    };
    // Give the helper a moment to exit on its own (it closes its window
    // after a token), then make sure it is gone.
    let status = match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
        Ok(Ok(s)) => Some(s),
        _ => {
            let _ = child.kill().await;
            None
        }
    };
    if terminal.is_some() {
        return terminal;
    }
    let tail = tokio::time::timeout(Duration::from_secs(2), stderr_task)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let code = status.and_then(|s| s.code());
    Some(WebLoginEvent::Failed(classify_exit(code, &tail)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok() -> String {
        // Assembled at runtime: push protection rejects token-shaped literals.
        [
            "MTIzNDU2Nzg5MDEyMzQ1Njc4",
            "GabCdE",
            "x".repeat(27).as_str(),
        ]
        .join(".")
    }

    #[test]
    fn helper_lines_round_trip() {
        for m in [
            HelperMsg::Ready,
            HelperMsg::Closed,
            HelperMsg::Token(Secret::new(tok())),
            HelperMsg::Error {
                code: code::NO_WEBVIEW.into(),
                message: "WebView2 not found".into(),
            },
        ] {
            assert_eq!(HelperMsg::parse(&m.to_line()), Some(m));
        }
    }

    #[test]
    fn helper_noise_and_hostile_lines_are_ignored() {
        for l in [
            "",
            "running 1 test",
            "{",
            "{\"type\":\"nope\"}",
            "{\"type\":\"token\"}",
            "{\"type\":\"token\",\"token\":42}",
            "{\"type\":\"token\",\"token\":\"short.x\"}",
            "{\"type\":\"token\",\"token\":\"has space.and.more-than-thirty-characters\"}",
        ] {
            assert_eq!(HelperMsg::parse(l), None, "{l}");
        }
        let long = format!(
            "{{\"type\":\"error\",\"message\":\"{}\"}}",
            "a".repeat(MAX_LINE)
        );
        assert_eq!(HelperMsg::parse(&long), None);
        // Control characters never reach the UI.
        let m = HelperMsg::parse(r#"{"type":"error","code":"failed","message":"a\nb\u001b[31m"}"#)
            .unwrap();
        assert_eq!(
            m,
            HelperMsg::Error {
                code: "failed".into(),
                message: "ab[31m".into()
            }
        );
    }

    #[test]
    fn debug_never_prints_tokens() {
        let m = HelperMsg::Token(Secret::new(tok()));
        assert!(!format!("{m:?}").contains("MTIz"));
        let e = WebLoginEvent::Token(Secret::new(tok()));
        assert!(!format!("{e:?}").contains("MTIz"));
    }

    #[test]
    fn page_messages_only_from_discord() {
        let body = format!(r#"{{"type":"token","token":"{}"}}"#, tok());
        assert_eq!(
            parse_page_message("https://discord.com/channels/@me", &body),
            Some(Secret::new(tok()))
        );
        assert!(parse_page_message("https://discord.com/login", &body).is_some());
        for bad in [
            "http://discord.com/login",
            "https://discord.com.evil.example/",
            "https://evil.example/?https://discord.com",
            "https://canary.discord.com/login",
            "https://discord.com:8443/login",
            "https://user@discord.com/login",
            "file:///tmp/x.html",
            "about:blank",
            "",
        ] {
            assert!(parse_page_message(bad, &body).is_none(), "{bad}");
        }
        for bad_body in [
            "",
            "token",
            r#"{"type":"ready"}"#,
            r#"{"type":"token","token":""}"#,
            r#"{"type":"token","token":"<script>alert(1)</script>.x.y.zzzzzzzzzzzzzzzzzz"}"#,
        ] {
            assert!(
                parse_page_message("https://discord.com/login", bad_body).is_none(),
                "{bad_body}"
            );
        }
    }

    #[test]
    fn navigation_is_https_only() {
        assert!(navigation_allowed("https://discord.com/login"));
        assert!(navigation_allowed(
            "https://newassets.hcaptcha.com/captcha/v1/x"
        ));
        assert!(navigation_allowed("about:blank"));
        assert!(!navigation_allowed("http://discord.com/login"));
        assert!(!navigation_allowed("file:///etc/passwd"));
        assert!(!navigation_allowed("data:text/html,hi"));
        assert!(!navigation_allowed("discord://-/channels/@me"));
    }

    #[test]
    fn exits_are_classified() {
        assert_eq!(
            classify_exit(
                Some(127),
                "ferrocord-login: error while loading shared libraries: libwebkit2gtk-4.1.so.0: cannot open shared object file"
            ),
            WebLoginFailure::NoWebEngine
        );
        assert_eq!(classify_exit(Some(127), ""), WebLoginFailure::NoWebEngine);
        assert_eq!(
            classify_exit(Some(101), "thread 'main' panicked"),
            WebLoginFailure::Crashed("exit code 101".into())
        );
        assert_eq!(
            classify_exit(None, ""),
            WebLoginFailure::Crashed("terminated".into())
        );
        assert!(
            WebLoginFailure::NoWebEngine
                .user_message()
                .contains("then try again")
        );
        assert!(
            WebLoginFailure::Missing
                .user_message()
                .contains(HELPER_NAME)
        );
    }

    #[test]
    fn init_script_is_scoped_to_discord() {
        assert!(INIT_SCRIPT.contains("location.origin !== \"https://discord.com\""));
        assert!(INIT_SCRIPT.contains("window.top !== window"));
        assert!(INIT_SCRIPT.contains("getItem(\"token\")"));
        // Read-only: it never writes to the page or its storage.
        assert!(!INIT_SCRIPT.contains("setItem"));
        assert!(!INIT_SCRIPT.contains("fetch"));
        assert!(!INIT_SCRIPT.contains("XMLHttpRequest"));
    }

    #[test]
    fn data_dirs_are_fresh_and_private() {
        let a = new_data_dir().unwrap();
        let b = new_data_dir().unwrap();
        assert_ne!(a, b);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        std::fs::write(a.join("Cookies"), b"x").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            wipe_data_dir(&a).await;
            wipe_data_dir(&b).await;
            wipe_data_dir(&b).await; // already gone: fine
        });
        assert!(!a.exists() && !b.exists());
    }
}
