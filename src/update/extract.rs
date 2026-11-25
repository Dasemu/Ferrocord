//! Safe archive extraction: regular files and directories only, every path
//! relative and inside the destination, total size and entry count capped.
//! Symlinks, hard links, devices and FIFOs are rejected, never followed.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::UpdateError;

/// Caps on what an archive may unpack to.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_total: u64,
    pub max_entries: usize,
}

/// A release unpacks to ~60 MB (three binaries, licences); anything far
/// beyond that is not ours.
pub const LIMITS: Limits = Limits {
    max_total: 600 * 1024 * 1024,
    max_entries: 2_000,
};

fn bad(why: impl Into<String>) -> UpdateError {
    UpdateError::BadArchive(why.into())
}

/// Validates an archive member name and returns it as a relative path.
/// Backslashes count as separators (zips made by older PowerShell use
/// them).
pub fn safe_rel_path(name: &str) -> Result<PathBuf, UpdateError> {
    if name.is_empty() || name.len() > 1024 || name.contains('\0') {
        return Err(bad("invalid member name"));
    }
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return Err(bad("absolute path in archive"));
    }
    let mut out = PathBuf::new();
    let mut depth = 0;
    for part in name.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(bad("`..` in archive path")),
            p if p.contains(':') => return Err(bad("drive or stream name in archive path")),
            p => {
                depth += 1;
                if depth > 32 {
                    return Err(bad("archive path too deep"));
                }
                out.push(p);
            }
        }
    }
    if depth == 0 {
        return Err(bad("empty member name"));
    }
    Ok(out)
}

/// Running totals while extracting.
struct Budget {
    limits: Limits,
    entries: usize,
    total: u64,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            entries: 0,
            total: 0,
        }
    }

    fn entry(&mut self) -> Result<(), UpdateError> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(bad("too many files in archive"));
        }
        Ok(())
    }

    /// Copies `r` to a new file at `path`, counting bytes against the cap
    /// (the bytes actually read, not the size the header claims).
    fn write_file(
        &mut self,
        mut r: impl Read,
        path: &Path,
        executable: bool,
    ) -> Result<(), UpdateError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(UpdateError::io)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    bad("duplicate file in archive")
                } else {
                    UpdateError::io(e)
                }
            })?;
        let room = self.limits.max_total - self.total;
        let mut limited = (&mut r).take(room + 1);
        let n = std::io::copy(&mut limited, &mut f).map_err(|e| bad(e.to_string()))?;
        if n > room {
            return Err(bad("archive unpacks to more than the size cap"));
        }
        self.total += n;
        f.flush().map_err(UpdateError::io)?;
        set_mode(path, executable);
        Ok(())
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _executable: bool) {}

/// Extracts a `.tar.gz` release into the empty directory `dest`.
#[cfg(target_os = "linux")]
pub fn extract_tar_gz(archive: &Path, dest: &Path, limits: Limits) -> Result<(), UpdateError> {
    let file = std::fs::File::open(archive).map_err(UpdateError::io)?;
    let gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    extract_tar(gz, dest, limits)
}

/// Tarballs are Linux releases; other systems never install one.
#[cfg(not(target_os = "linux"))]
pub fn extract_tar_gz(_archive: &Path, _dest: &Path, _limits: Limits) -> Result<(), UpdateError> {
    Err(UpdateError::NoAsset)
}

/// Extracts an (uncompressed) tar stream into `dest`.
#[cfg(target_os = "linux")]
pub fn extract_tar(r: impl Read, dest: &Path, limits: Limits) -> Result<(), UpdateError> {
    let mut ar = tar::Archive::new(r);
    let mut budget = Budget::new(limits);
    let entries = ar.entries().map_err(|e| bad(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| bad(e.to_string()))?;
        budget.entry()?;
        let kind = entry.header().entry_type();
        // Extension headers carry metadata for the next member; the tar
        // crate applies long names itself.
        if kind.is_pax_global_extensions()
            || kind.is_pax_local_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink()
        {
            continue;
        }
        let raw = entry.path_bytes();
        let name = std::str::from_utf8(&raw).map_err(|_| bad("non-UTF-8 path in archive"))?;
        let rel = safe_rel_path(name)?;
        let path = dest.join(&rel);
        if kind.is_dir() {
            std::fs::create_dir_all(&path).map_err(UpdateError::io)?;
        } else if kind.is_file() || kind.is_contiguous() {
            let mode = entry.header().mode().unwrap_or(0o644);
            budget.write_file(entry, &path, mode & 0o111 != 0)?;
        } else if kind.is_symlink() {
            return Err(bad("symlink in archive"));
        } else if kind.is_hard_link() {
            return Err(bad("hard link in archive"));
        } else {
            return Err(bad("special file in archive"));
        }
    }
    Ok(())
}

/// Extracts a `.zip` release into the empty directory `dest`.
pub fn extract_zip(archive: &Path, dest: &Path, limits: Limits) -> Result<(), UpdateError> {
    let file = std::fs::File::open(archive).map_err(UpdateError::io)?;
    let mut zip =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| bad(e.to_string()))?;
    let mut budget = Budget::new(limits);
    if zip.len() > limits.max_entries {
        return Err(bad("too many files in archive"));
    }
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| bad(e.to_string()))?;
        budget.entry()?;
        if entry.is_symlink() {
            return Err(bad("symlink in archive"));
        }
        let rel = safe_rel_path(entry.name())?;
        let path = dest.join(&rel);
        let is_dir = entry.is_dir() || entry.name().ends_with('\\');
        if is_dir {
            std::fs::create_dir_all(&path).map_err(UpdateError::io)?;
        } else {
            let exec = entry.unix_mode().is_some_and(|m| m & 0o111 != 0)
                || path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
            budget.write_file(entry, &path, exec)?;
        }
    }
    Ok(())
}

/// The package folder inside an extracted release: the archives hold one
/// top-level directory (`ferrocord-v<ver>-<platform>/`), as `install.sh`
/// expects.
pub fn package_root(dest: &Path) -> Result<PathBuf, UpdateError> {
    let mut dirs = Vec::new();
    for e in std::fs::read_dir(dest).map_err(UpdateError::io)? {
        let e = e.map_err(UpdateError::io)?;
        if e.file_type().map_err(UpdateError::io)?.is_dir() {
            dirs.push(e.path());
        }
    }
    match dirs.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(bad("unexpected archive layout")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names_must_stay_inside() {
        assert_eq!(
            safe_rel_path("ferrocord-v1/ferrocord").unwrap(),
            Path::new("ferrocord-v1").join("ferrocord")
        );
        assert_eq!(
            safe_rel_path("./a//b/./c").unwrap(),
            Path::new("a").join("b").join("c")
        );
        assert_eq!(
            safe_rel_path(r"dir\ferrocord.exe").unwrap(),
            Path::new("dir").join("ferrocord.exe")
        );
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            r"..\x",
            r"\abs",
            "C:/Windows/x",
            r"C:\x",
            "a/file.txt:stream",
            ".",
            "./",
            "a\0b",
        ] {
            assert!(safe_rel_path(bad).is_err(), "{bad:?} must be rejected");
        }
        let deep = vec!["d"; 40].join("/");
        assert!(safe_rel_path(&deep).is_err());
    }

    fn zip_with(entries: &[(&str, &[u8])], symlink: Option<(&str, &str)>) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                if name.ends_with('/') {
                    w.add_directory(*name, opts).unwrap();
                } else {
                    w.start_file(*name, opts).unwrap();
                    w.write_all(data).unwrap();
                }
            }
            if let Some((name, target)) = symlink {
                w.add_symlink(name, target, opts).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn unzip(bytes: &[u8], limits: Limits) -> (tempfile::TempDir, Result<(), UpdateError>) {
        let tmp = tempfile::tempdir().unwrap();
        let ar = tmp.path().join("a.zip");
        std::fs::write(&ar, bytes).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let r = extract_zip(&ar, &out, limits);
        (tmp, r)
    }

    #[test]
    fn zip_extracts_files_and_dirs() {
        let z = zip_with(
            &[
                ("pkg/", b""),
                ("pkg/ferrocord.exe", b"MZ app"),
                ("pkg/licenses/a.txt", b"text"),
            ],
            None,
        );
        let (tmp, r) = unzip(&z, LIMITS);
        r.unwrap();
        let out = tmp.path().join("out");
        let root = package_root(&out).unwrap();
        assert_eq!(
            std::fs::read(root.join("ferrocord.exe")).unwrap(),
            b"MZ app"
        );
        assert_eq!(std::fs::read(root.join("licenses/a.txt")).unwrap(), b"text");
    }

    #[test]
    fn zip_rejects_traversal_symlinks_and_oversize() {
        let (_t, r) = unzip(&zip_with(&[("../evil.exe", b"x")], None), LIMITS);
        assert!(matches!(r, Err(UpdateError::BadArchive(_))), "{r:?}");
        let (_t, r) = unzip(
            &zip_with(&[("pkg/a", b"x")], Some(("pkg/l", "/etc"))),
            LIMITS,
        );
        assert!(matches!(r, Err(UpdateError::BadArchive(m)) if m.contains("symlink")));
        let small = Limits {
            max_total: 10,
            max_entries: 100,
        };
        let (_t, r) = unzip(&zip_with(&[("pkg/big", &[7u8; 64])], None), small);
        assert!(matches!(r, Err(UpdateError::BadArchive(m)) if m.contains("size cap")));
        let few = Limits {
            max_total: 1 << 20,
            max_entries: 2,
        };
        let (_t, r) = unzip(
            &zip_with(&[("p/a", b"1"), ("p/b", b"2"), ("p/c", b"3")], None),
            few,
        );
        assert!(matches!(r, Err(UpdateError::BadArchive(m)) if m.contains("too many")));
    }

    #[test]
    fn package_root_needs_exactly_one_folder() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(package_root(tmp.path()).is_err());
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        assert_eq!(package_root(tmp.path()).unwrap(), tmp.path().join("a"));
        std::fs::create_dir(tmp.path().join("b")).unwrap();
        assert!(package_root(tmp.path()).is_err());
    }

    #[cfg(target_os = "linux")]
    mod tar_tests {
        use super::super::*;

        /// Builds a tar by hand-written headers so hostile members (links,
        /// absolute names, devices) can be expressed.
        fn tar_of(members: &[(&str, u8, &[u8], &str)]) -> Vec<u8> {
            let mut b = tar::Builder::new(Vec::new());
            for (name, kind, data, link) in members {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o755);
                h.set_entry_type(tar::EntryType::new(*kind));
                if !link.is_empty() {
                    h.set_link_name(link).unwrap();
                }
                // `set_path` refuses `..` and absolute names: write the raw
                // name field like a hostile archive would.
                let raw = &mut h.as_old_mut().name;
                raw.fill(0);
                raw[..name.len()].copy_from_slice(name.as_bytes());
                h.set_cksum();
                b.append(&h, *data).unwrap();
            }
            b.into_inner().unwrap()
        }

        fn untar(bytes: &[u8], limits: Limits) -> (tempfile::TempDir, Result<(), UpdateError>) {
            let tmp = tempfile::tempdir().unwrap();
            let out = tmp.path().join("out");
            std::fs::create_dir(&out).unwrap();
            let r = extract_tar(bytes, &out, limits);
            (tmp, r)
        }

        #[test]
        fn extracts_regular_files_with_modes() {
            let t = tar_of(&[
                ("pkg/", b'5', b"", ""),
                ("pkg/ferrocord", b'0', b"\x7fELF", ""),
            ]);
            let (tmp, r) = untar(&t, LIMITS);
            r.unwrap();
            let f = tmp.path().join("out/pkg/ferrocord");
            assert_eq!(std::fs::read(&f).unwrap(), b"\x7fELF");
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }

        #[test]
        fn rejects_hostile_members() {
            for (members, why) in [
                (vec![("/etc/x", b'0', &b"x"[..], "")], "absolute"),
                (vec![("pkg/../../x", b'0', &b"x"[..], "")], ".."),
                (vec![("pkg/l", b'2', &b""[..], "/etc/passwd")], "symlink"),
                (vec![("pkg/h", b'1', &b""[..], "pkg/a")], "hard link"),
                (vec![("pkg/dev", b'3', &b""[..], "")], "special"),
                (vec![("pkg/fifo", b'6', &b""[..], "")], "special"),
            ] {
                let (tmp, r) = untar(&tar_of(&members), LIMITS);
                assert!(
                    matches!(&r, Err(UpdateError::BadArchive(m)) if m.contains(why)),
                    "{why}: {r:?}"
                );
                // Nothing escaped the destination.
                assert!(!tmp.path().join("x").exists());
            }
        }

        #[test]
        fn caps_total_size_by_bytes_read() {
            let t = tar_of(&[
                ("pkg/a", b'0', &[1u8; 600], ""),
                ("pkg/b", b'0', &[2u8; 600], ""),
            ]);
            let limits = Limits {
                max_total: 1000,
                max_entries: 10,
            };
            let (_tmp, r) = untar(&t, limits);
            assert!(matches!(r, Err(UpdateError::BadArchive(m)) if m.contains("size cap")));
        }

        #[test]
        fn gzip_round_trip() {
            let t = tar_of(&[("pkg/ferrocord", b'0', b"bin", "")]);
            let tmp = tempfile::tempdir().unwrap();
            let ar = tmp.path().join("a.tar.gz");
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(&t).unwrap();
            std::fs::write(&ar, gz.finish().unwrap()).unwrap();
            let out = tmp.path().join("out");
            std::fs::create_dir(&out).unwrap();
            extract_tar_gz(&ar, &out, LIMITS).unwrap();
            assert_eq!(std::fs::read(out.join("pkg/ferrocord")).unwrap(), b"bin");
            // A corrupt gzip is an archive error, not a panic.
            std::fs::write(&ar, b"\x1f\x8bnot really").unwrap();
            let out2 = tmp.path().join("out2");
            std::fs::create_dir(&out2).unwrap();
            assert!(extract_tar_gz(&ar, &out2, LIMITS).is_err());
        }
    }
}
