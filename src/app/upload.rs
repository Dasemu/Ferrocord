//! Attachments being composed and uploads in flight.
//!
//! Pure state and parsing live here (unit-tested). The blocking helpers at
//! the bottom talk to the desktop (file dialog, clipboard) through the
//! standard tools of the platform and are only called from `Task`s.

use std::path::{Path, PathBuf};

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

/// Content found on the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clipboard {
    Files(Vec<PathBuf>),
    /// PNG bytes of a copied image.
    Image(Vec<u8>),
    Nothing,
}

/// Encodes RGBA pixels as PNG, refusing oversized images (decompression
/// bomb style clipboard contents) and results above `max_bytes`.
pub fn encode_png(width: usize, height: usize, rgba: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    let (w, h) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > crate::net::rest::MAX_MEDIA_PIXELS {
        return None;
    }
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    let bytes = out.into_inner();
    (bytes.len() <= max_bytes).then_some(bytes)
}

/// Reads copied files or an image from the system clipboard (`arboard`,
/// X11 and Wayland). Blocking: call from a `Task`.
pub fn read_clipboard_blocking(max_image: usize) -> Clipboard {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return Clipboard::Nothing;
    };
    if let Ok(mut files) = cb.get().file_list()
        && !files.is_empty()
    {
        files.truncate(MAX_FILES_PER_MESSAGE);
        return Clipboard::Files(files);
    }
    if let Ok(img) = cb.get().image()
        && let Some(png) = encode_png(img.width, img.height, &img.bytes, max_image)
    {
        return Clipboard::Image(png);
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
    fn png_encoding_is_bounded() {
        let px = vec![255u8; 4 * 4 * 4];
        let png = encode_png(4, 4, &px, 10_000).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert!(encode_png(4, 4, &px, 10).is_none(), "size cap");
        assert!(encode_png(0, 4, &px, 10_000).is_none());
        assert!(
            encode_png(4, 4, &px[..10], 10_000).is_none(),
            "short buffer"
        );
        assert!(
            encode_png(100_000, 100_000, &px, usize::MAX).is_none(),
            "pixel cap"
        );
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
