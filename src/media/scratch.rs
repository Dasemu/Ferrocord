//! Downloaded videos live in a private directory for as long as they play
//! and are removed afterwards: when the video closes ([`Scratch`] is
//! dropped) and, for files a crash left behind, at the next start
//! ([`sweep`]).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Files older than this are leftovers of an earlier run.
pub const STALE_AFTER: Duration = Duration::from_secs(6 * 3600);
/// Prefix of every file this module creates (and the only kind it sweeps).
const PREFIX: &str = "ferrocord-video-";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary file that is deleted when dropped.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Creates a new, empty file in `dir` (created if missing, owner-only
    /// on Unix) and opens it for writing. `ext` is a short alphanumeric
    /// extension (anything else becomes `bin`).
    pub fn create(dir: &Path, ext: &str) -> io::Result<(Scratch, File)> {
        std::fs::create_dir_all(dir)?;
        let _ = crate::config::restrict_permissions(dir);
        let ext: String = ext
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(5)
            .collect();
        let ext = if ext.is_empty() { "bin".into() } else { ext };
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{PREFIX}{}-{n}.{ext}", std::process::id()));
        let mut opts = OpenOptions::new();
        // `create_new` refuses a pre-planted file or symlink.
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&path)?;
        Ok((Scratch { path }, file))
    }

    /// Where the file is.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Deletes video files in `dir` that are older than `older_than` (other
/// files are never touched). Returns how many were removed.
pub fn sweep(dir: &Path, older_than: Duration) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(PREFIX) {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        if age >= older_than && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ferrocord-scratch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_scratch_file_exists_while_held_and_vanishes_when_dropped() {
        let d = dir("drop");
        let (s, mut f) = Scratch::create(&d, "mp4").unwrap();
        io::Write::write_all(&mut f, b"x").unwrap();
        let p = s.path().to_path_buf();
        assert!(p.is_file());
        assert!(p.extension().is_some_and(|e| e == "mp4"));
        drop(f);
        drop(s);
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn names_are_unique_and_the_extension_is_sanitised() {
        let d = dir("names");
        let (a, _) = Scratch::create(&d, "../../x").unwrap();
        let (b, _) = Scratch::create(&d, "").unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(a.path().parent(), Some(d.as_path()));
        assert!(b.path().extension().is_some_and(|e| e == "bin"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("perm");
        let (s, _) = Scratch::create(&d, "mp4").unwrap();
        let mode = std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_sweep_removes_only_stale_video_files() {
        let d = dir("sweep");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ferrocord-video-1-0.mp4"), b"x").unwrap();
        std::fs::write(d.join("notes.txt"), b"keep").unwrap();
        // Nothing is old enough yet.
        assert_eq!(sweep(&d, Duration::from_secs(3600)), 0);
        // With a zero age limit the video file goes, the other stays.
        assert_eq!(sweep(&d, Duration::ZERO), 1);
        assert!(d.join("notes.txt").exists());
        assert!(!d.join("ferrocord-video-1-0.mp4").exists());
        assert_eq!(sweep(&d.join("missing"), Duration::ZERO), 0);
        let _ = std::fs::remove_dir_all(d);
    }
}
