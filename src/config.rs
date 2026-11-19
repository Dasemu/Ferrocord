//! Paths, feature flags and environment overrides.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

/// Set by `main` as early as possible; used for `first_frame_ms`.
pub static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Stable application id: keyring service name, Wayland `app_id`,
/// notification app name.
pub const APP_ID: &str = "dev.ferro.ferrocord";
/// Human-readable application name.
pub const APP_NAME: &str = "Ferrocord";

/// Default endpoints. They can be overridden with environment variables so
/// the client can be pointed at a local mock server.
pub const DEFAULT_GATEWAY_URL: &str = "wss://gateway.discord.gg";
pub const DEFAULT_API_URL: &str = "https://discord.com/api/v9";
pub const DEFAULT_CDN_URL: &str = "https://cdn.discordapp.com";
pub const DEFAULT_REMOTE_AUTH_URL: &str = "wss://remote-auth-gateway.discord.gg/?v=2";

/// Which screen `--screenshot` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotScreen {
    Login,
    Guild,
    Dms,
    Embeds,
}

impl ScreenshotScreen {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "login" => Some(Self::Login),
            "guild" | "server" => Some(Self::Guild),
            "dms" | "dm" => Some(Self::Dms),
            "embeds" | "attachments" => Some(Self::Embeds),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    Dark,
    Light,
}

/// `https://` for any host, plain `http://` only for loopback (mock servers).
pub fn validate_http_url(url: &str) -> Result<(), String> {
    let u = url::Url::parse(url).map_err(|e| format!("invalid URL {url}: {e}"))?;
    let loopback = matches!(
        u.host(),
        Some(url::Host::Domain("localhost"))
            | Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST))
            | Some(url::Host::Ipv6(std::net::Ipv6Addr::LOCALHOST))
    );
    match u.scheme() {
        "https" if u.host().is_some() => Ok(()),
        "http" if loopback => Ok(()),
        _ => Err(format!("refusing insecure URL {url} (https required)")),
    }
}

/// Runtime configuration, built once in `main`.
#[derive(Debug, Clone)]
pub struct Config {
    /// `--mock`: use `mock.rs` data, an in-memory store and secret store,
    /// and never touch the network.
    pub mock: bool,
    /// `--screenshot <screen> <out.png>`.
    pub screenshot: Option<(ScreenshotScreen, PathBuf)>,
    pub theme: ThemeChoice,
    pub gateway_url: String,
    pub api_url: String,
    pub cdn_url: String,
    pub remote_auth_url: String,
    /// Use `compress=zlib-stream` on the gateway.
    pub zlib: bool,
    /// Notification privacy: when false only the sender is shown.
    pub notification_preview: bool,
    /// Log `first_frame_ms`.
    pub trace_startup: bool,
    pub data_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
}

impl Config {
    /// Builds the config from command-line arguments and the environment.
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut mock = false;
        let mut screenshot = None;
        let mut args = args.into_iter().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--mock" => mock = true,
                "--screenshot" => {
                    let screen = args
                        .next()
                        .and_then(|s| ScreenshotScreen::parse(&s))
                        .ok_or("--screenshot needs one of: login, guild, dms, embeds")?;
                    let out = args.next().ok_or("--screenshot needs an output path")?;
                    screenshot = Some((screen, PathBuf::from(out)));
                }
                "--help" | "-h" => {
                    return Err("usage: ferrocord [--mock] [--screenshot <login|guild|dms|embeds> <out.png>]".into());
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        if screenshot.is_some() {
            mock = true;
        }
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let theme = match env("FERRO_THEME").as_deref() {
            Some("light") => ThemeChoice::Light,
            _ => ThemeChoice::Dark,
        };
        let dirs = directories::ProjectDirs::from("dev", "ferro", "ferrocord");
        let gateway_url =
            env("FERROCORD_GATEWAY_URL").unwrap_or_else(|| DEFAULT_GATEWAY_URL.into());
        let remote_auth_url =
            env("FERROCORD_REMOTE_AUTH_URL").unwrap_or_else(|| DEFAULT_REMOTE_AUTH_URL.into());
        for u in [&gateway_url, &remote_auth_url] {
            crate::net::gateway::validate_url(u).map_err(|e| e.to_string())?;
        }
        let api_url = env("FERROCORD_API_URL").unwrap_or_else(|| DEFAULT_API_URL.into());
        let cdn_url = env("FERROCORD_CDN_URL").unwrap_or_else(|| DEFAULT_CDN_URL.into());
        for u in [&api_url, &cdn_url] {
            validate_http_url(u)?;
        }
        Ok(Self {
            mock,
            screenshot,
            theme,
            gateway_url,
            api_url,
            cdn_url,
            remote_auth_url,
            zlib: env("FERROCORD_ZLIB").as_deref() != Some("0"),
            notification_preview: env("FERROCORD_NOTIFY_PREVIEW").as_deref() != Some("0"),
            trace_startup: env("FERRO_TRACE_STARTUP").is_some(),
            data_dir: dirs.as_ref().map(|d| d.data_dir().to_path_buf()),
            state_dir: dirs
                .as_ref()
                .map(|d| d.state_dir().unwrap_or(d.data_dir()).to_path_buf()),
        })
    }

    /// Config used by tests and mock mode helpers.
    pub fn for_tests() -> Self {
        Self {
            mock: true,
            screenshot: None,
            theme: ThemeChoice::Dark,
            gateway_url: DEFAULT_GATEWAY_URL.into(),
            api_url: DEFAULT_API_URL.into(),
            cdn_url: DEFAULT_CDN_URL.into(),
            remote_auth_url: DEFAULT_REMOTE_AUTH_URL.into(),
            zlib: true,
            notification_preview: true,
            trace_startup: false,
            data_dir: None,
            state_dir: None,
        }
    }

    /// `$XDG_DATA_HOME/ferrocord/cache.sqlite3`, or `None` in mock mode.
    pub fn database_path(&self) -> Option<PathBuf> {
        if self.mock {
            return None;
        }
        self.data_dir.as_ref().map(|d| d.join("cache.sqlite3"))
    }

    pub fn log_path(&self) -> Option<PathBuf> {
        self.state_dir.as_ref().map(|d| d.join("log"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        std::iter::once("ferrocord")
            .chain(a.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn parses_mock_and_screenshot() {
        let c = Config::from_args(args(&["--screenshot", "dms", "/tmp/x.png"])).unwrap();
        assert!(c.mock);
        assert_eq!(c.screenshot.as_ref().unwrap().0, ScreenshotScreen::Dms);
        assert!(c.database_path().is_none());
    }

    #[test]
    fn url_validation() {
        assert!(validate_http_url("https://discord.com/api/v9").is_ok());
        assert!(validate_http_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_http_url("http://example.com").is_err());
        assert!(validate_http_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn rejects_unknown() {
        assert!(Config::from_args(args(&["--nope"])).is_err());
        assert!(Config::from_args(args(&["--screenshot", "bogus", "x"])).is_err());
    }
}
