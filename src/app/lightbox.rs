//! Full-window image viewer: a click on an image or GIF in the chat shows
//! it as large as the window allows, over a dark margin; a click on the
//! margin (or Escape, or the close button) goes back.

/// Space kept around the picture; clicking it closes the viewer.
pub const MARGIN: f32 = 56.0;

/// The image the viewer shows.
#[derive(Clone, PartialEq, Eq)]
pub struct Lightbox {
    /// Cache key of what the chat already shows (image or animation): the
    /// viewer shows it at once.
    pub key: String,
    /// A larger copy to load and show instead once it arrives (attachment
    /// previews are scaled down for the chat). `None` when `key` is already
    /// the full picture.
    pub full: Option<String>,
    /// Picture size (or just its aspect ratio when the real size is unknown).
    pub width: u32,
    pub height: u32,
    /// Original file, for "Open in browser".
    pub url: Option<String>,
}

impl std::fmt::Debug for Lightbox {
    /// URLs point at user content: never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lightbox({}x{})", self.width, self.height)
    }
}

/// Scales (w, h) to fill (max_w, max_h) keeping the aspect ratio; small
/// pictures are enlarged too. Never zero.
pub fn fill_size(w: u32, h: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    let (max_w, max_h) = (max_w.max(1.0), max_h.max(1.0));
    if w == 0 || h == 0 {
        return (max_w, max_h);
    }
    let scale = (max_w / w as f32).min(max_h / h as f32);
    (
        (w as f32 * scale).floor().max(1.0),
        (h as f32 * scale).floor().max(1.0),
    )
}

/// Attachment preview URL scaled to fit `max_w` x `max_h` on the media
/// proxy (the chat asks for 400 x 300).
pub fn proxy_resized(base: &str, w: u32, h: u32, max_w: u32, max_h: u32) -> String {
    let (w, h) = crate::ui::fit_size(w, h, max_w, max_h);
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}width={w}&height={h}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_pictures_are_enlarged_and_large_ones_shrunk() {
        assert_eq!(fill_size(100, 50, 800.0, 600.0), (800.0, 400.0));
        assert_eq!(fill_size(4000, 4000, 800.0, 600.0), (600.0, 600.0));
    }

    #[test]
    fn unknown_or_degenerate_sizes_never_give_zero() {
        assert_eq!(fill_size(0, 0, 800.0, 600.0), (800.0, 600.0));
        assert_eq!(fill_size(10_000, 1, 800.0, 600.0), (800.0, 1.0));
        assert_eq!(fill_size(10, 10, -5.0, 0.0), (1.0, 1.0));
    }

    #[test]
    fn proxy_url_keeps_existing_query() {
        assert_eq!(
            proxy_resized("https://m/x.png?ex=1", 4000, 2000, 1920, 1080),
            "https://m/x.png?ex=1&width=1920&height=960"
        );
        assert_eq!(
            proxy_resized("https://m/x.png", 100, 50, 1920, 1080),
            "https://m/x.png?width=100&height=50"
        );
    }
}
