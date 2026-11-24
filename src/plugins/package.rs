//! Finding and reading plugins: a folder or a `.ferroplug` zip in the
//! plugins directory, each holding `plugin.toml` and `plugin.wasm`.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::manifest::{MAX_MANIFEST_BYTES, Manifest};

/// Largest `plugin.wasm` we load.
pub const MAX_WASM_BYTES: u64 = 8 * 1024 * 1024;
/// Largest `.ferroplug` file we open.
const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Most entries a plugins directory scan looks at.
const MAX_ENTRIES: usize = 256;
/// Extension of zipped plugins.
pub const PACKAGE_EXT: &str = "ferroplug";

/// One plugin found on disk.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub manifest: Manifest,
}

/// A directory entry that looked like a plugin but could not be read.
#[derive(Debug, Clone)]
pub struct Broken {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub found: Vec<Found>,
    pub broken: Vec<Broken>,
}

impl Scan {
    pub fn get(&self, id: &str) -> Option<&Found> {
        self.found.iter().find(|f| f.manifest.id == id)
    }
}

fn is_package(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == PACKAGE_EXT)
}

/// Every plugin in `dir` (sorted by name). A missing directory is empty.
/// Two plugins with the same id: the first by path wins, the other is
/// reported as broken.
pub fn scan(dir: &Path) -> Scan {
    let mut out = Scan::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() || is_package(p))
        .take(MAX_ENTRIES)
        .collect();
    paths.sort();
    for path in paths {
        match read_manifest(&path) {
            Ok(manifest) => {
                if out.get(&manifest.id).is_some() {
                    out.broken.push(Broken {
                        path,
                        error: format!("another plugin already uses the id {:?}", manifest.id),
                    });
                } else {
                    out.found.push(Found { path, manifest });
                }
            }
            Err(error) => out.broken.push(Broken { path, error }),
        }
    }
    out.found.sort_by(|a, b| {
        a.manifest
            .name
            .to_lowercase()
            .cmp(&b.manifest.name.to_lowercase())
    });
    out
}

/// Reads at most `max` bytes; more is an error (no zip bombs, no huge files).
fn read_capped(r: impl Read, max: u64, what: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    r.take(max + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("{what}: {e}"))?;
    if buf.len() as u64 > max {
        return Err(format!("{what} is larger than {} KiB", max / 1024));
    }
    Ok(buf)
}

fn manifest_from(bytes: Vec<u8>) -> Result<Manifest, String> {
    let text = String::from_utf8(bytes).map_err(|_| "plugin.toml is not UTF-8".to_owned())?;
    Manifest::parse(&text)
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if len > MAX_PACKAGE_BYTES {
        return Err("package is too large".into());
    }
    zip::ZipArchive::new(f).map_err(|e| format!("not a valid .{PACKAGE_EXT}: {e}"))
}

fn zip_entry(path: &Path, name: &str, max: u64) -> Result<Vec<u8>, String> {
    let mut z = open_zip(path)?;
    let e = z
        .by_name(name)
        .map_err(|_| format!("the package has no {name}"))?;
    if e.size() > max {
        return Err(format!("{name} is too large"));
    }
    read_capped(e, max, name)
}

fn file(path: &Path, max: u64, what: &str) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path).map_err(|_| format!("missing {what}"))?;
    read_capped(f, max, what)
}

/// The manifest of the plugin at `path` (folder or package).
pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let bytes = if is_package(path) {
        zip_entry(path, "plugin.toml", MAX_MANIFEST_BYTES as u64)?
    } else {
        file(
            &path.join("plugin.toml"),
            MAX_MANIFEST_BYTES as u64,
            "plugin.toml",
        )?
    };
    manifest_from(bytes)
}

/// The module of the plugin at `path`.
pub fn read_wasm(path: &Path) -> Result<Vec<u8>, String> {
    let wasm = if is_package(path) {
        zip_entry(path, "plugin.wasm", MAX_WASM_BYTES)?
    } else {
        file(&path.join("plugin.wasm"), MAX_WASM_BYTES, "plugin.wasm")?
    };
    if !wasm.starts_with(b"\0asm") {
        return Err("plugin.wasm is not a WebAssembly module".into());
    }
    Ok(wasm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const TOML: &str = "id = \"z\"\nname = \"Zed\"\nversion = \"1\"\napi_version = 1\n";

    #[test]
    fn scans_folders_and_packages() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(a.join("plugin.toml"), TOML.replace("\"z\"", "\"a\"")).unwrap();
        std::fs::write(a.join("plugin.wasm"), b"\0asm\x01\0\0\0").unwrap();

        let zp = dir.path().join("z.ferroplug");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("plugin.toml", o).unwrap();
        z.write_all(TOML.as_bytes()).unwrap();
        z.start_file("plugin.wasm", o).unwrap();
        z.write_all(b"\0asm\x01\0\0\0").unwrap();
        z.finish().unwrap();

        // Broken: no manifest; duplicate id.
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        let dup = dir.path().join("dup");
        std::fs::create_dir(&dup).unwrap();
        std::fs::write(dup.join("plugin.toml"), TOML).unwrap();
        // Ignored: plain files.
        std::fs::write(dir.path().join("state.json"), "{}").unwrap();

        let s = scan(dir.path());
        let ids: Vec<_> = s.found.iter().map(|f| f.manifest.id.as_str()).collect();
        assert_eq!(ids, ["a", "z"]);
        assert_eq!(s.broken.len(), 2, "{:?}", s.broken);
        assert!(read_wasm(&zp).is_ok());
        assert!(read_wasm(&a).is_ok());
        assert!(read_wasm(&dup).is_err());
    }

    #[test]
    fn missing_dir_is_empty() {
        let s = scan(Path::new("/nonexistent/ferrocord/plugins"));
        assert!(s.found.is_empty() && s.broken.is_empty());
    }

    #[test]
    fn caps_sizes() {
        let big = [0u8; 100];
        assert!(read_capped(&big[..], 99, "x").is_err());
        assert_eq!(read_capped(&big[..], 100, "x").unwrap().len(), 100);
    }
}
