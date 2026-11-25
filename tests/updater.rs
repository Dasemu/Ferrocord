//! In-app updater against a local HTTP server: a fake `releases/latest`,
//! archives with dummy binaries and their checksums. Installs go into temp
//! folders laid out like `install.sh` / the Windows zip.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use ferrocord::update::{
    self, CheckOutcome, Endpoint, InstallEvent, InstallMethod, Platform, Release, ScriptLayout,
    UpdateError, Version,
};

const NEW: &str = "0.99.0";

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![],
            body: body.into(),
        }
    }
}

type Seen = Arc<Mutex<Vec<(String, HashMap<String, String>)>>>;

/// One request per connection; `routes` maps a path to a reply, and
/// `If-None-Match: "e1"` on `/latest` gets a 304.
async fn server(routes: HashMap<String, Reply>) -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let seen2 = seen.clone();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            let seen = seen2.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut headers = HashMap::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).await.unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
                    }
                }
                seen.lock().unwrap().push((path.clone(), headers.clone()));
                let reply = if path == "/latest"
                    && headers.get("if-none-match").map(String::as_str) == Some("\"e1\"")
                {
                    Reply {
                        status: 304,
                        headers: vec![],
                        body: vec![],
                    }
                } else {
                    routes.get(&path).cloned().unwrap_or(Reply {
                        status: 404,
                        headers: vec![],
                        body: b"nope".to_vec(),
                    })
                };
                let mut out = format!(
                    "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in &reply.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = w.write_all(out.as_bytes()).await;
                let _ = w.write_all(&reply.body).await;
                let _ = w.shutdown().await;
            });
        }
    });
    (addr, seen)
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn latest_json(base: &str, archive: &str) -> String {
    serde_json::json!({
        "tag_name": format!("v{NEW}"),
        "html_url": format!("https://github.com/FerroApps/Ferrocord/releases/tag/v{NEW}"),
        "draft": false,
        "prerelease": false,
        "assets": [
            {"name": archive, "browser_download_url": format!("{base}/dl/{archive}"), "size": 1},
            {"name": format!("{archive}.sha256"), "browser_download_url": format!("{base}/dl/{archive}.sha256"), "size": 1},
        ]
    })
    .to_string()
}

/// Serves `archive` (named `name`) with a checksum; `corrupt` serves a
/// checksum of other bytes.
async fn release_server(name: &str, archive: Vec<u8>, corrupt: bool) -> (Endpoint, Release, Seen) {
    let sum_of = if corrupt {
        b"other".to_vec()
    } else {
        archive.clone()
    };
    let sum = format!("{}  {name}\n", hex(&Sha256::digest(&sum_of)));
    let mut routes = HashMap::new();
    routes.insert(format!("/dl/{name}"), Reply::ok(archive));
    routes.insert(format!("/dl/{name}.sha256"), Reply::ok(sum));
    // The base URL is only known once bound: bind first, then add /latest
    // through a second server sharing the routes would be overkill, so the
    // JSON is built from the address after binding.
    let (addr, seen) = server(routes.clone()).await;
    let base = format!("http://{addr}");
    let mut routes2 = routes;
    routes2.insert("/latest".into(), Reply::ok(latest_json(&base, name)));
    // Second server: /latest pointing at the first one's assets.
    let (addr2, _) = server(routes2).await;
    let endpoint = Endpoint::resolve(Some(&format!("http://{addr2}/latest")));
    assert!(endpoint.loopback);
    let release = match update::net::check(&endpoint, None).await.unwrap() {
        CheckOutcome::Release { release, .. } => release,
        other => panic!("unexpected {other:?}"),
    };
    (endpoint, release, seen)
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn no_cache_refresh() {
    // SAFETY: only read by the installer (skips desktop cache tools).
    unsafe { std::env::set_var("FERRO_UPDATE_NO_CACHE_REFRESH", "1") };
}

#[cfg(target_os = "linux")]
fn tarball(version: &str) -> Vec<u8> {
    let root = format!("ferrocord-v{version}-linux-x86_64");
    let mut b = tar::Builder::new(Vec::new());
    let mut add = |path: String, data: &[u8], mode: u32| {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, path, data).unwrap();
    };
    for bin in ["ferrocord", "ferrocord-player", "ferrocord-login"] {
        add(
            format!("{root}/{bin}"),
            format!("#!new {bin} {version}").as_bytes(),
            0o755,
        );
    }
    add(
        format!("{root}/packaging/dev.ferro.ferrocord.desktop"),
        b"[Desktop Entry]\nName=Ferrocord\nExec=ferrocord %U\n",
        0o644,
    );
    add(
        format!("{root}/packaging/dev.ferro.ferrocord.svg"),
        b"<svg/>",
        0o644,
    );
    add(format!("{root}/README.md"), b"readme", 0o644);
    let tar = b.into_inner().unwrap();
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

/// An `install.sh` layout with version 0.1.0 installed.
fn script_layout(root: &Path) -> ScriptLayout {
    let l = ScriptLayout::from_env(
        Some(&root.join("home")),
        Some(&root.join("xdg-bin")),
        Some(&root.join("xdg-data")),
    )
    .unwrap();
    std::fs::create_dir_all(&l.bin_dir).unwrap();
    std::fs::create_dir_all(l.state_dir()).unwrap();
    std::fs::write(l.bin_path("ferrocord"), "#!old ferrocord").unwrap();
    std::fs::write(l.bin_path("ferrocord-player"), "#!old player").unwrap();
    std::fs::write(l.version_file(), "0.1.0\n").unwrap();
    l
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn install_sh_layout_is_replaced_atomically() {
    no_cache_refresh();
    let name = format!("ferrocord-v{NEW}-linux-x86_64.tar.gz");
    let (endpoint, release, seen) = release_server(&name, tarball(NEW), false).await;
    let tmp = tempfile::tempdir().unwrap();
    let layout = script_layout(tmp.path());
    let method = InstallMethod::Script(layout.clone());
    let mut events = Vec::new();
    let v = update::perform_install(&endpoint, &release, &method, Platform::LinuxX64, |e| {
        events.push(e)
    })
    .await
    .unwrap();
    assert_eq!(v, Version::parse(NEW).unwrap());
    assert!(matches!(events.first(), Some(InstallEvent::Progress(0, _))));
    assert!(events.contains(&InstallEvent::Verifying));
    assert_eq!(events.last(), Some(&InstallEvent::Installing));

    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
    assert_eq!(
        read(layout.bin_path("ferrocord")),
        format!("#!new ferrocord {NEW}")
    );
    assert_eq!(
        read(layout.bin_path("ferrocord-player")),
        format!("#!new ferrocord-player {NEW}")
    );
    assert_eq!(
        read(layout.bin_path("ferrocord-login")),
        format!("#!new ferrocord-login {NEW}")
    );
    assert_eq!(read(layout.version_file()), format!("{NEW}\n"));
    let desktop = read(layout.apps_dir().join("dev.ferro.ferrocord.desktop"));
    assert!(desktop.contains(&format!(
        "Exec={} %U",
        layout.bin_path("ferrocord").display()
    )));
    assert!(layout.icon_dir().join("dev.ferro.ferrocord.svg").is_file());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(layout.bin_path("ferrocord"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    // Temp files lived in the bin dir (same filesystem) and are gone.
    assert_eq!(
        files(&layout.bin_dir),
        ["ferrocord", "ferrocord-login", "ferrocord-player"]
    );
    // Plain requests: a User-Agent, no credentials or cookies.
    for (_, h) in seen.lock().unwrap().iter() {
        assert!(
            h.get("user-agent")
                .is_some_and(|ua| ua.starts_with("ferrocord/"))
        );
        assert!(!h.contains_key("authorization") && !h.contains_key("cookie"));
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_corrupt_download_leaves_the_install_untouched() {
    no_cache_refresh();
    let name = format!("ferrocord-v{NEW}-linux-x86_64.tar.gz");
    let (endpoint, release, _) = release_server(&name, tarball(NEW), true).await;
    let tmp = tempfile::tempdir().unwrap();
    let layout = script_layout(tmp.path());
    let before = files(&layout.bin_dir);
    let r = update::perform_install(
        &endpoint,
        &release,
        &InstallMethod::Script(layout.clone()),
        Platform::LinuxX64,
        |_| {},
    )
    .await;
    assert_eq!(r, Err(UpdateError::Checksum));
    assert_eq!(files(&layout.bin_dir), before, "no temp files left");
    assert_eq!(
        std::fs::read_to_string(layout.bin_path("ferrocord")).unwrap(),
        "#!old ferrocord"
    );
    assert_eq!(
        std::fs::read_to_string(layout.version_file()).unwrap(),
        "0.1.0\n"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_hostile_archive_is_rejected_before_anything_is_written() {
    no_cache_refresh();
    let name = format!("ferrocord-v{NEW}-linux-x86_64.tar.gz");
    // A symlink member pointing out of the package.
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Symlink);
    h.set_size(0);
    b.append_link(
        &mut h,
        format!("ferrocord-v{NEW}-linux-x86_64/ferrocord"),
        "/bin/sh",
    )
    .unwrap();
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&b.into_inner().unwrap()).unwrap();
    let (endpoint, release, _) = release_server(&name, gz.finish().unwrap(), false).await;
    let tmp = tempfile::tempdir().unwrap();
    let layout = script_layout(tmp.path());
    let r = update::perform_install(
        &endpoint,
        &release,
        &InstallMethod::Script(layout.clone()),
        Platform::LinuxX64,
        |_| {},
    )
    .await;
    assert!(matches!(r, Err(UpdateError::BadArchive(_))), "{r:?}");
    assert_eq!(
        std::fs::read_to_string(layout.bin_path("ferrocord")).unwrap(),
        "#!old ferrocord"
    );
    assert_eq!(files(&layout.bin_dir), ["ferrocord", "ferrocord-player"]);
}

fn zip_release(version: &str) -> Vec<u8> {
    use std::io::Write;
    let root = format!("ferrocord-v{version}-windows-x86_64");
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    let o = zip::write::SimpleFileOptions::default();
    for exe in [
        "ferrocord.exe",
        "ferrocord-player.exe",
        "ferrocord-login.exe",
    ] {
        // Older PowerShell writes backslash separators.
        w.start_file(format!("{root}\\{exe}"), o).unwrap();
        w.write_all(format!("MZ new {exe} {version}").as_bytes())
            .unwrap();
    }
    w.start_file(format!("{root}/licenses/x.txt"), o).unwrap();
    w.write_all(b"l").unwrap();
    w.finish().unwrap();
    buf.into_inner()
}

#[tokio::test]
async fn windows_zip_folder_is_updated() {
    let name = format!("ferrocord-v{NEW}-windows-x86_64.zip");
    let (endpoint, release, _) = release_server(&name, zip_release(NEW), false).await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Ferrocord");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("ferrocord.exe"), "MZ old").unwrap();
    std::fs::write(dir.join("settings.txt"), "keep").unwrap();
    let method = InstallMethod::WindowsZip { dir: dir.clone() };
    update::perform_install(&endpoint, &release, &method, Platform::WindowsX64, |_| {})
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("ferrocord.exe")).unwrap(),
        format!("MZ new ferrocord.exe {NEW}")
    );
    assert!(dir.join("ferrocord-login.exe").is_file());
    assert_eq!(
        files(&dir),
        [
            "ferrocord-login.exe",
            "ferrocord-player.exe",
            "ferrocord.exe",
            "settings.txt"
        ]
    );
}

#[tokio::test]
async fn checks_use_etags_and_back_off_on_rate_limits() {
    let mut routes = HashMap::new();
    let body = latest_json(
        "https://github.com/x",
        &format!("ferrocord-v{NEW}-linux-x86_64.tar.gz"),
    );
    routes.insert(
        "/latest".to_owned(),
        Reply {
            status: 200,
            headers: vec![("etag".into(), "\"e1\"".into())],
            body: body.into_bytes(),
        },
    );
    routes.insert(
        "/limited".to_owned(),
        Reply {
            status: 403,
            headers: vec![("retry-after".into(), "7200".into())],
            body: b"{}".to_vec(),
        },
    );
    let (addr, seen) = server(routes).await;
    let ep = Endpoint::resolve(Some(&format!("http://{addr}/latest")));
    let CheckOutcome::Release { release, etag } = update::net::check(&ep, None).await.unwrap()
    else {
        panic!("expected a release");
    };
    assert_eq!(release.tag, format!("v{NEW}"));
    assert_eq!(etag.as_deref(), Some("\"e1\""));
    assert_eq!(
        update::net::check(&ep, etag.as_deref()).await.unwrap(),
        CheckOutcome::NotModified
    );
    {
        let seen = seen.lock().unwrap();
        let (_, h) = &seen[1];
        assert_eq!(h.get("if-none-match").map(String::as_str), Some("\"e1\""));
        assert_eq!(
            h.get("accept").map(String::as_str),
            Some("application/vnd.github+json")
        );
    }
    let limited = Endpoint::resolve(Some(&format!("http://{addr}/limited")));
    assert_eq!(
        update::net::check(&limited, None).await.unwrap(),
        CheckOutcome::RateLimited(Some(std::time::Duration::from_secs(7200)))
    );
    // No release yet.
    let none = Endpoint::resolve(Some(&format!("http://{addr}/missing")));
    assert_eq!(
        update::net::check(&none, None).await.unwrap(),
        CheckOutcome::NoRelease
    );
}

#[tokio::test]
async fn downloads_refuse_foreign_hosts_and_redirects() {
    let mut routes = HashMap::new();
    routes.insert(
        "/dl/a.sha256".to_owned(),
        Reply {
            status: 302,
            headers: vec![("location".into(), "https://evil.example/a.sha256".into())],
            body: vec![],
        },
    );
    let (addr, _) = server(routes).await;
    let ep = Endpoint::resolve(Some(&format!("http://{addr}/latest")));
    let r = update::net::fetch_text(&ep, &format!("http://{addr}/dl/a.sha256")).await;
    assert!(matches!(r, Err(UpdateError::Insecure(_))), "{r:?}");
    // Without a loopback override only GitHub hosts over HTTPS are used.
    let gh = Endpoint::resolve(None);
    let r = update::net::fetch_text(&gh, "https://evil.example/a.sha256").await;
    assert!(matches!(r, Err(UpdateError::Insecure(_))), "{r:?}");
    let r = update::net::fetch_text(&gh, "http://github.com/a.sha256").await;
    assert!(matches!(r, Err(UpdateError::Insecure(_))), "{r:?}");
}

/// Windows CI: a running `.exe` cannot be overwritten, but the rename
/// dance moves it aside, puts the new one in place, and the `.old` copy
/// goes on the next start.
#[cfg(windows)]
#[test]
fn windows_rename_dance_replaces_a_running_exe() {
    use update::install::{Step, Strategy, cleanup, commit};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let target = dir.join("ferrocord.exe");
    // A real running executable: a copy of this test binary, started on
    // the sleeper test below.
    std::fs::copy(std::env::current_exe().unwrap(), &target).unwrap();
    let mut child = std::process::Command::new(&target)
        .args(["--exact", "windows_sleeper", "--ignored", "--nocapture"])
        .env("FERRO_SLEEPER", "1")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        std::fs::write(&target, b"overwrite").is_err(),
        "a running exe must be locked (test precondition)"
    );
    let staged = dir.join(".ferrocord.exe.new");
    std::fs::write(&staged, b"MZ new").unwrap();
    commit(
        &[Step {
            staged,
            target: target.clone(),
        }],
        Strategy::MoveAside,
    )
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"MZ new");
    assert!(
        dir.join("ferrocord.exe.old").exists(),
        "still running, kept for later"
    );
    // A second update while the first old copy is still locked.
    let staged = dir.join(".ferrocord.exe.new");
    std::fs::write(&staged, b"MZ newer").unwrap();
    commit(
        &[Step {
            staged,
            target: target.clone(),
        }],
        Strategy::MoveAside,
    )
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"MZ newer");
    let _ = child.kill();
    let _ = child.wait();
    std::thread::sleep(std::time::Duration::from_millis(500));
    // "Next start": the leftovers go.
    cleanup(dir);
    assert_eq!(files(dir), ["ferrocord.exe"]);
}

/// Helper process for the Windows test above (does nothing otherwise).
#[test]
#[ignore]
fn windows_sleeper() {
    if std::env::var_os("FERRO_SLEEPER").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
