//! A bounded on-disk cache of downloaded media (GIF picker files).
//!
//! Files are named by the SHA-256 of their URL (no URL, name or other
//! personal data in the file name) and evicted least recently used first
//! once the directory holds more than its byte cap: a read refreshes the
//! file's modification time, and every write trims the oldest files. All
//! calls do blocking file I/O: run them in `spawn_blocking`.
//!
//! Everything is best effort: a missing directory, a full disk or a file
//! removed by another process only means a cache miss.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// Default cap of the GIF picker cache.
pub const GIF_CACHE_CAP: u64 = 100 * 1024 * 1024;
/// Files larger than this are never stored (one GIF must not push out the
/// rest of the cache).
pub const MAX_ENTRY: usize = 10 * 1024 * 1024;
const EXT: &str = "bin";

#[derive(Debug, Clone)]
pub struct DiskCache {
    dir: PathBuf,
    cap: u64,
}

impl DiskCache {
    pub fn new(dir: impl Into<PathBuf>, cap: u64) -> Self {
        Self {
            dir: dir.into(),
            cap,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// File holding `url`'s bytes.
    pub fn path_of(&self, url: &str) -> PathBuf {
        let hash = Sha256::digest(url.as_bytes());
        let mut name = String::with_capacity(64 + 4);
        for b in hash {
            name.push_str(&format!("{b:02x}"));
        }
        name.push('.');
        name.push_str(EXT);
        self.dir.join(name)
    }

    /// The cached bytes of `url`, marking them recently used.
    pub fn get(&self, url: &str) -> Option<Vec<u8>> {
        let path = self.path_of(url);
        let bytes = fs::read(&path).ok()?;
        if bytes.is_empty() || bytes.len() > MAX_ENTRY {
            let _ = fs::remove_file(&path);
            return None;
        }
        if let Ok(f) = fs::File::options().append(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(bytes)
    }

    /// Whether `url` is stored (without touching it).
    pub fn contains(&self, url: &str) -> bool {
        self.path_of(url).is_file()
    }

    /// Stores `bytes` for `url` (atomically: a temporary file renamed into
    /// place), then trims the cache to its cap.
    pub fn put(&self, url: &str, bytes: &[u8]) {
        if bytes.is_empty() || bytes.len() > MAX_ENTRY {
            return;
        }
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let path = self.path_of(url);
        let tmp = path.with_extension(format!("tmp{}", fastrand::u32(..)));
        let written = fs::File::create(&tmp).and_then(|mut f| f.write_all(bytes));
        if written.is_err() || fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
        self.trim();
    }

    /// Bytes stored now (cache files only).
    pub fn size(&self) -> u64 {
        self.entries().iter().map(|e| e.1).sum()
    }

    fn entries(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| {
                let path = e.path();
                let ext = path.extension()?.to_str()?;
                // Leftover temporary files of an interrupted write count too
                // (and go first).
                if ext != EXT && !ext.starts_with("tmp") {
                    return None;
                }
                let md = e.metadata().ok()?;
                if !md.is_file() {
                    return None;
                }
                let at = if ext == EXT {
                    md.modified().unwrap_or(SystemTime::UNIX_EPOCH)
                } else {
                    SystemTime::UNIX_EPOCH
                };
                Some((path, md.len(), at))
            })
            .collect()
    }

    /// Deletes the least recently used files until the total fits the cap.
    pub fn trim(&self) {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        if total <= self.cap {
            return;
        }
        entries.sort_by_key(|e| e.2);
        for (path, len, _) in entries {
            if total <= self.cap {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn stores_and_reads_back_by_url_hash() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path().join("gifs"), 1024);
        assert_eq!(c.get("https://media.tenor.com/a.gif"), None);
        c.put("https://media.tenor.com/a.gif", b"GIF89a-1");
        assert!(c.contains("https://media.tenor.com/a.gif"));
        assert_eq!(
            c.get("https://media.tenor.com/a.gif").as_deref(),
            Some(&b"GIF89a-1"[..])
        );
        // The file name is a hash: no part of the URL is on disk.
        let name = c.path_of("https://media.tenor.com/a.gif");
        let name = name.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 64 + 4);
        assert!(!name.contains("tenor"));
        assert_ne!(c.path_of("a"), c.path_of("b"));
    }

    #[test]
    fn evicts_least_recently_used_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 250);
        let old = SystemTime::now() - Duration::from_secs(3600);
        for (i, url) in ["a", "b", "c"].iter().enumerate() {
            c.put(url, &[i as u8; 100][..]);
            // Distinct ages, oldest first, without sleeping.
            let f = fs::File::options()
                .append(true)
                .open(c.path_of(url))
                .unwrap();
            f.set_modified(old + Duration::from_secs(i as u64 * 60))
                .unwrap();
        }
        // "c" pushed the total to 300 > 250 when it was written: "a" (the
        // oldest) went.
        assert!(!c.contains("a") && c.contains("b") && c.contains("c"));
        // Reading "b" makes it the newest; the next write evicts "c".
        assert!(c.get("b").is_some());
        c.put("d", &[9u8; 100]);
        assert!(c.contains("b") && !c.contains("c") && c.contains("d"));
        assert!(c.size() <= 250);
    }

    #[test]
    fn refuses_empty_and_oversized_entries_and_tolerates_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path().join("x"), 1 << 30);
        c.put("e", b"");
        assert!(!c.contains("e"));
        c.put("big", &vec![0u8; MAX_ENTRY + 1]);
        assert!(!c.contains("big"));
        let gone = DiskCache::new(dir.path().join("never/created"), 10);
        assert_eq!(gone.get("x"), None);
        gone.trim();
        assert_eq!(gone.size(), 0);
    }

    #[test]
    fn leftover_temporary_files_are_trimmed_first() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 150);
        fs::write(dir.path().join("abc.tmp123"), [0u8; 100]).unwrap();
        c.put("a", &[1u8; 100]);
        assert!(c.contains("a"));
        assert!(!dir.path().join("abc.tmp123").exists());
    }
}
