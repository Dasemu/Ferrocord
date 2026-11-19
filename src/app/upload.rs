//! Attachments being composed and uploads in flight.
//!
//! Pure state and parsing live here (unit-tested). The blocking helpers at
//! the bottom talk to the desktop (file dialog, clipboard) through the
//! standard tools of the platform and are only called from `Task`s.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::net::model::*;
use crate::net::rest::{
    MAX_FILES_PER_MESSAGE, UploadError, UploadFile, check_upload, sanitize_filename,
};

/// A file waiting in the upload bar above the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftAttachment {
    pub file: UploadFile,
    pub is_image: bool,
    /// Image cache key of the preview thumbnail, once decoded.
    pub preview_key: Option<String>,
    /// The file is a temporary copy (pasted image) to delete afterwards.
    pub temporary: bool,
}

impl DraftAttachment {
    pub fn from_file(file: UploadFile, temporary: bool) -> Self {
        let is_image = is_image_name(&file.name);
        Self {
            file,
            is_image,
            preview_key: None,
            temporary,
        }
    }

    pub fn key(&self) -> String {
        format!("local://{}", self.file.path.display())
    }
}

pub fn is_image_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp"]
        .iter()
        .any(|e| n.ends_with(e))
}

/// Stats a path into an [`UploadFile`]. Directories and unreadable paths
/// are errors.
pub fn file_from_path(path: &Path) -> Result<UploadFile, String> {
    let meta = std::fs::metadata(path).map_err(|_| "Could not read that file.".to_owned())?;
    if !meta.is_file() {
        return Err("Folders cannot be uploaded.".into());
    }
    let name = path
        .file_name()
        .map(|n| sanitize_filename(&n.to_string_lossy()))
        .unwrap_or_else(|| "file".into());
    Ok(UploadFile {
        path: path.to_path_buf(),
        name,
        size: meta.len(),
    })
}

/// Result of adding files to the draft.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddOutcome {
    pub added: usize,
    /// Messages for the toast (one per rejected file).
    pub rejected: Vec<String>,
}

/// Adds `files` to `draft`, keeping the ones that respect the limits.
pub fn add_to_draft(
    draft: &mut Vec<DraftAttachment>,
    files: Vec<(UploadFile, bool)>,
    limit: u64,
) -> AddOutcome {
    let mut out = AddOutcome::default();
    for (f, temporary) in files {
        if draft.len() >= MAX_FILES_PER_MESSAGE {
            out.rejected.push(format!(
                "You can attach at most {MAX_FILES_PER_MESSAGE} files."
            ));
            break;
        }
        if draft.iter().any(|d| d.file.path == f.path) {
            continue;
        }
        match check_upload(std::slice::from_ref(&f), limit) {
            Ok(()) => {
                draft.push(DraftAttachment::from_file(f, temporary));
                out.added += 1;
            }
            Err(e) => out.rejected.push(upload_error_text(&e)),
        }
    }
    out
}

pub fn upload_error_text(e: &UploadError) -> String {
    match e {
        UploadError::TooMany => {
            format!("You can attach at most {MAX_FILES_PER_MESSAGE} files.")
        }
        UploadError::TooLarge { name, limit } => format!(
            "{name} is too large. The limit is {}.",
            crate::ui::human_size(*limit)
        ),
        UploadError::Empty { name } => format!("{name} is empty."),
    }
}

/// Progress events of one upload.
#[derive(Debug, Clone)]
pub enum UploadEvent {
    Progress { sent: u64, total: u64 },
    Done(Result<Box<Message>, String>),
}

/// An upload in flight, shown as a progress strip.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveUpload {
    pub id: u64,
    pub channel: ChannelId,
    pub label: String,
    pub sent: u64,
    pub total: u64,
}

impl ActiveUpload {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.sent as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// "photo.png" or "photo.png and 2 more".
    pub fn label_for(files: &[UploadFile]) -> String {
        match files {
            [] => String::new(),
            [one] => one.name.clone(),
            [first, rest @ ..] => format!("{} and {} more", first.name, rest.len()),
        }
    }
}

// -- parsing of dialog / clipboard output ---------------------------------------

/// Decodes `%XX` escapes (invalid sequences are kept literally).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Parses a `text/uri-list` (or newline separated paths): `file://` URIs
/// and absolute paths are kept, everything else (http links, comments) is
/// ignored. At most [`MAX_FILES_PER_MESSAGE`] entries.
pub fn parse_path_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            if let Some(rest) = l.strip_prefix("file://") {
                // `file://localhost/path` and `file:///path`.
                let path = rest.strip_prefix("localhost").unwrap_or(rest);
                path.starts_with('/')
                    .then(|| PathBuf::from(percent_decode(path)))
            } else if l.starts_with('/') {
                Some(PathBuf::from(l))
            } else {
                None
            }
        })
        .take(MAX_FILES_PER_MESSAGE)
        .collect()
}

/// Content found on the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clipboard {
    Files(Vec<PathBuf>),
    /// PNG bytes of a copied image.
    Image(Vec<u8>),
    Nothing,
}

/// Picks what to read from a list of clipboard MIME types: files first,
/// then PNG, then other images we can upload as they are.
pub fn choose_clipboard_type(types: &str) -> Option<&'static str> {
    let has = |t: &str| types.lines().any(|l| l.trim() == t);
    if has("text/uri-list") {
        Some("text/uri-list")
    } else if has("image/png") {
        Some("image/png")
    } else {
        None
    }
}

// -- blocking desktop helpers (call from `Task`s only) --------------------------

/// Runs a command with a deadline and returns its stdout (capped at `cap`
/// bytes). `None` on failure, timeout or when the tool is missing.
fn run_capped(cmd: &str, args: &[&str], timeout: Duration, cap: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(cap as u64 + 1).read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    };
    let buf = reader.join().ok()?;
    (status.success() && buf.len() <= cap).then_some(buf)
}

/// Reads files or an image from the clipboard using `wl-paste` (Wayland)
/// or `xclip` (X11). Returns `Nothing` when neither tool exists.
pub fn read_clipboard_blocking(max_image: usize) -> Clipboard {
    let t = Duration::from_secs(3);
    let backends: [(&str, &[&str], &str); 2] = [
        ("wl-paste", &["--list-types"], "wl-paste"),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "TARGETS", "-o"],
            "xclip",
        ),
    ];
    for (cmd, list_args, kind) in backends {
        let Some(types) = run_capped(cmd, list_args, t, 16 * 1024) else {
            continue;
        };
        let types = String::from_utf8_lossy(&types).into_owned();
        let Some(mime) = choose_clipboard_type(&types) else {
            return Clipboard::Nothing;
        };
        let args: Vec<&str> = if kind == "wl-paste" {
            vec!["--no-newline", "--type", mime]
        } else {
            vec!["-selection", "clipboard", "-t", mime, "-o"]
        };
        let cap = if mime == "image/png" {
            max_image
        } else {
            64 * 1024
        };
        let Some(data) = run_capped(cmd, &args, Duration::from_secs(10), cap) else {
            return Clipboard::Nothing;
        };
        return if mime == "text/uri-list" {
            let files = parse_path_list(&String::from_utf8_lossy(&data));
            if files.is_empty() {
                Clipboard::Nothing
            } else {
                Clipboard::Files(files)
            }
        } else {
            Clipboard::Image(data)
        };
    }
    Clipboard::Nothing
}

/// Writes pasted image bytes to a private temporary file.
pub fn save_pasted_image(bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let dir = std::env::temp_dir();
    let name = format!("ferrocord-paste-{:08x}.png", fastrand::u32(..));
    let path = dir.join(name);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    f.write_all(bytes)?;
    Ok(path)
}

/// Opens the native file chooser through `kdialog` or `zenity`. `Ok(vec![])`
/// means the user cancelled; `Err` that no dialog tool is available.
pub fn pick_files_blocking() -> Result<Vec<PathBuf>, String> {
    let t = Duration::from_secs(600);
    let attempts: [(&str, Vec<&str>); 2] = [
        (
            "kdialog",
            vec!["--getopenfilename", ".", "--multiple", "--separate-output"],
        ),
        (
            "zenity",
            vec!["--file-selection", "--multiple", "--separator=\n"],
        ),
    ];
    for (cmd, args) in attempts {
        // A cancelled dialog exits non-zero; distinguish "missing tool"
        // from "cancelled" by whether the command could be spawned.
        if std::process::Command::new(cmd)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            continue;
        }
        let out = run_capped(cmd, &args, t, 1024 * 1024).unwrap_or_default();
        return Ok(parse_path_list(&String::from_utf8_lossy(&out)));
    }
    Err("No file dialog found (install kdialog or zenity). You can also drag files onto the window or paste them.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, size: u64) -> (UploadFile, bool) {
        (
            UploadFile {
                path: PathBuf::from(format!("/tmp/{name}")),
                name: name.into(),
                size,
            },
            false,
        )
    }

    #[test]
    fn limits_are_enforced_per_file() {
        let mut d = vec![];
        let out = add_to_draft(
            &mut d,
            vec![f("a.png", 5), f("big.bin", 100), f("z.txt", 0)],
            50,
        );
        assert_eq!(out.added, 1);
        assert_eq!(out.rejected.len(), 2);
        assert!(out.rejected[0].contains("big.bin"));
        assert!(out.rejected[1].contains("empty"));
        assert!(d[0].is_image);
    }

    #[test]
    fn duplicates_and_count_cap() {
        let mut d = vec![];
        add_to_draft(&mut d, vec![f("a.txt", 1), f("a.txt", 1)], 50);
        assert_eq!(d.len(), 1);
        let many: Vec<_> = (0..15).map(|i| f(&format!("f{i}.txt"), 1)).collect();
        let out = add_to_draft(&mut d, many, 50);
        assert_eq!(d.len(), MAX_FILES_PER_MESSAGE);
        assert_eq!(out.rejected.len(), 1);
    }

    #[test]
    fn uri_lists_are_parsed_safely() {
        let text = "# comment\nfile:///home/u/My%20Pic.png\nfile://localhost/tmp/a.txt\nhttps://evil.example/x\nrelative/path\n/abs/path.bin\n\n";
        let v = parse_path_list(text);
        assert_eq!(
            v,
            vec![
                PathBuf::from("/home/u/My Pic.png"),
                PathBuf::from("/tmp/a.txt"),
                PathBuf::from("/abs/path.bin"),
            ]
        );
        let many = (0..50).map(|i| format!("/f{i}\n")).collect::<String>();
        assert_eq!(parse_path_list(&many).len(), MAX_FILES_PER_MESSAGE);
    }

    #[test]
    fn percent_decoding_edge_cases() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%E2%82%AC"), "€");
    }

    #[test]
    fn clipboard_type_preference() {
        assert_eq!(
            choose_clipboard_type("text/plain\ntext/uri-list\nimage/png\n"),
            Some("text/uri-list")
        );
        assert_eq!(
            choose_clipboard_type("image/png\ntext/html"),
            Some("image/png")
        );
        assert_eq!(choose_clipboard_type("text/plain"), None);
    }

    #[test]
    fn progress_and_labels() {
        let u = ActiveUpload {
            id: 1,
            channel: Snowflake(1),
            label: String::new(),
            sent: 50,
            total: 200,
        };
        assert!((u.fraction() - 0.25).abs() < 1e-6);
        let z = ActiveUpload {
            total: 0,
            ..u.clone()
        };
        assert_eq!(z.fraction(), 0.0);
        let files = [f("a.png", 1).0, f("b.png", 1).0, f("c.png", 1).0];
        assert_eq!(ActiveUpload::label_for(&files[..1]), "a.png");
        assert_eq!(ActiveUpload::label_for(&files), "a.png and 2 more");
    }

    #[test]
    fn file_from_path_rejects_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(file_from_path(dir.path()).is_err());
        let p = dir.path().join("hello.txt");
        std::fs::write(&p, b"hi").unwrap();
        let f = file_from_path(&p).unwrap();
        assert_eq!((f.name.as_str(), f.size), ("hello.txt", 2));
        assert!(file_from_path(&dir.path().join("missing")).is_err());
    }
}
