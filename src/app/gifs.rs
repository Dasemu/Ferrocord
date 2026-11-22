//! GIF picker state: a home page with the "Favorites" and "Trending GIFs"
//! tiles (like the official picker), a debounced search as the user types,
//! and the saved GIFs. Picking one sends its Tenor URL, which Discord
//! embeds as a GIF.
//!
//! Thumbnails are fetched for the GIFs on screen, decoded off the UI
//! thread to a small still (first frame) and kept in their own small LRU
//! (`App::gif_thumbs`); the handle is made once in `update`.

use iced::Point;

use crate::net::frecency::{FavoriteChange, FavoriteGif, GifFormat};
use crate::net::model::Gif;

/// Byte budget of the decoded picker thumbnails (~60 tiles).
pub const THUMB_BUDGET: usize = 12 * 1024 * 1024;
/// Thumbnails are scaled down to fit this box (tiles are ~200 px wide;
/// the extra room keeps them sharp at 125 % scaling).
pub const THUMB_MAX_W: u32 = 256;
pub const THUMB_MAX_H: u32 = 400;

/// What the picker shows when the search box is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GifPage {
    /// The two tiles: Favorites and Trending GIFs.
    Home,
    Favorites,
    Trending,
}

/// Wait this long after the last keystroke before searching.
pub const SEARCH_DEBOUNCE_MS: u64 = 350;

#[derive(Debug, Clone)]
pub struct GifPicker {
    /// Where the composer button was clicked (the popup opens above it).
    pub at: Point,
    pub page: GifPage,
    pub query: String,
    pub results: Vec<Gif>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped per keystroke / request so stale timers and responses are
    /// dropped.
    pub generation: u64,
}

impl GifPicker {
    /// A picker that is loading the trending GIFs; returns it with the
    /// generation to tag that request with.
    pub fn open(at: Point) -> (Self, u64) {
        let p = Self {
            at,
            page: GifPage::Home,
            query: String::new(),
            results: Vec::new(),
            loading: true,
            error: None,
            generation: 1,
        };
        (p, 1)
    }

    /// The search text changed; returns the generation of the debounce
    /// timer to start.
    pub fn set_query(&mut self, q: String) -> u64 {
        self.query = q;
        self.generation += 1;
        self.generation
    }

    /// A debounce timer fired: returns the query to search for if no
    /// keystroke came after it.
    pub fn due(&mut self, generation: u64) -> Option<String> {
        if generation != self.generation {
            return None;
        }
        self.loading = true;
        self.error = None;
        Some(self.query.clone())
    }

    /// Opens a page of the home screen (or goes back to it).
    pub fn show(&mut self, page: GifPage) {
        self.page = page;
    }

    /// Whether the grid shows search results (the box has text).
    pub fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Applies a response; stale generations are ignored.
    pub fn finish(&mut self, generation: u64, result: Result<Vec<Gif>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(g) => self.results = g,
            Err(e) => self.error = Some(e),
        }
    }
}

/// The saved GIFs, kept in sync with the frecency settings. Changes are
/// optimistic: applied at once, rolled back if the save fails.
#[derive(Debug, Clone, Default)]
pub struct Favorites {
    /// `None` until loaded from the server.
    pub list: Option<Vec<FavoriteGif>>,
    /// Bumped per change, so only the last save's answer is applied.
    pub generation: u64,
}

impl Favorites {
    pub fn contains(&self, url: &str) -> bool {
        self.list
            .as_ref()
            .is_some_and(|l| l.iter().any(|g| g.url == url))
    }

    /// Toggles `gif`; returns the change to save, the generation to tag it
    /// with and the list to restore if it fails.
    pub fn toggle(&mut self, gif: &Gif) -> (FavoriteChange, u64, Vec<FavoriteGif>) {
        let list = self.list.get_or_insert_with(Vec::new);
        let before = list.clone();
        let change = if list.iter().any(|g| g.url == gif.url) {
            list.retain(|g| g.url != gif.url);
            FavoriteChange::Remove(gif.url.clone())
        } else {
            let fav = favorite_from(gif);
            let order = list.iter().map(|g| g.order).max().unwrap_or(0) + 1;
            list.insert(
                0,
                FavoriteGif {
                    order,
                    ..fav.clone()
                },
            );
            FavoriteChange::Add(fav)
        };
        self.generation += 1;
        (change, self.generation, before)
    }

    /// The answer to a load or a save of `generation`: the server's list,
    /// or the rollback on failure. Stale answers are ignored.
    pub fn finish(
        &mut self,
        generation: u64,
        result: Result<Vec<FavoriteGif>, String>,
        rollback: Option<Vec<FavoriteGif>>,
    ) -> Option<String> {
        if generation != self.generation {
            return None;
        }
        match result {
            Ok(l) => {
                self.list = Some(l);
                None
            }
            Err(e) => {
                if let Some(r) = rollback {
                    self.list = Some(r);
                }
                Some(e)
            }
        }
    }

    /// The saved GIFs as picker items, newest first.
    pub fn items(&self) -> Vec<Gif> {
        self.list.iter().flatten().map(gif_from_favorite).collect()
    }
}

/// What the official client saves for a picked GIF: the MP4 it asked for
/// (`media_format=mp4`) as a `VIDEO`, else the GIF as an `IMAGE`.
pub fn favorite_from(g: &Gif) -> FavoriteGif {
    let (src, format) = if !g.src.is_empty() {
        let video = g.src.to_ascii_lowercase().contains(".mp4");
        let f = if video {
            GifFormat::Video
        } else {
            GifFormat::Image
        };
        (g.src.clone(), f)
    } else {
        (g.gif_src.clone(), GifFormat::Image)
    };
    FavoriteGif {
        url: g.url.clone(),
        src,
        format,
        width: g.width,
        height: g.height,
        order: 0,
    }
}

/// A saved GIF as a picker item. The still comes from the saved file: an
/// image as is, a Tenor MP4 through Tenor's matching still/GIF URLs.
pub fn gif_from_favorite(f: &FavoriteGif) -> Gif {
    let (preview, gif_src) = match f.format {
        GifFormat::Video => (
            tenor_variant(&f.src, "AAAAe", "png").unwrap_or_default(),
            tenor_variant(&f.src, "AAAAC", "gif").unwrap_or_default(),
        ),
        _ => (f.src.clone(), f.src.clone()),
    };
    Gif {
        url: f.url.clone(),
        preview,
        gif_src,
        src: f.src.clone(),
        width: f.width.max(1),
        height: f.height.max(1),
    }
}

/// Another rendition of a Tenor media URL. Tenor names files
/// `/<11-char id><5-char format code>/<slug>.<ext>` (`AAAPo` MP4,
/// `AAAAC` GIF, `AAAAe` PNG still). `None` for other URLs.
pub fn tenor_variant(src: &str, code: &str, ext: &str) -> Option<String> {
    let mut u = url::Url::parse(src).ok()?;
    if u.scheme() != "https" || !u.host_str()?.ends_with("tenor.com") {
        return None;
    }
    let segs: Vec<String> = u.path_segments()?.map(str::to_owned).collect();
    let n = segs.len();
    if n < 2 {
        return None;
    }
    let (id, file) = (&segs[n - 2], &segs[n - 1]);
    if id.len() != 16 || !id.is_ascii() {
        return None;
    }
    let stem = file.rsplit_once('.').map_or(file.as_str(), |(s, _)| s);
    let mut out = segs[..n - 2].to_vec();
    out.push(format!("{}{code}", &id[..11]));
    out.push(format!("{stem}.{ext}"));
    u.set_path(&out.join("/"));
    u.set_query(None);
    Some(u.into())
}

/// URLs to try, in order, for a GIF's picker still.
pub fn thumb_sources(g: &Gif) -> Vec<String> {
    let mut v = vec![g.preview.clone()];
    if g.gif_src != g.preview {
        v.push(g.gif_src.clone());
    }
    v.retain(|u| !u.is_empty());
    v
}

/// Decodes `bytes` (PNG/JPEG/GIF/WebP; first frame of an animation) to a
/// still that fits [`THUMB_MAX_W`] x [`THUMB_MAX_H`]. Runs off the UI
/// thread. Refuses decompression bombs.
pub fn thumbnail(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    crate::net::rest::check_image_dimensions(bytes).ok()?;
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > THUMB_MAX_W || img.height() > THUMB_MAX_H {
        img.resize(
            THUMB_MAX_W,
            THUMB_MAX_H,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(n: u32) -> Gif {
        Gif {
            url: format!("https://tenor.com/view/{n}"),
            preview: format!("https://media.tenor.com/{n}.png"),
            gif_src: format!("https://media.tenor.com/{n}.gif"),
            src: format!("https://media.tenor.com/{n}.mp4"),
            width: 200,
            height: 100,
        }
    }

    #[test]
    fn opens_on_the_home_tiles() {
        let (mut p, _) = GifPicker::open(Point::ORIGIN);
        assert_eq!(p.page, GifPage::Home);
        assert!(!p.searching());
        p.show(GifPage::Favorites);
        assert_eq!(p.page, GifPage::Favorites);
        p.set_query("  ".into());
        assert!(!p.searching());
        p.set_query("cat".into());
        assert!(p.searching());
    }

    #[test]
    fn favourite_toggle_is_optimistic_and_rolls_back() {
        let mut f = Favorites::default();
        let (change, g1, before) = f.toggle(&gif(1));
        assert!(
            matches!(change, FavoriteChange::Add(ref a) if a.url == gif(1).url
            && a.format == GifFormat::Video && a.src.ends_with(".mp4"))
        );
        assert!(f.contains(&gif(1).url));
        assert!(before.is_empty());
        // The save fails: back to the old list, with an error to show.
        assert_eq!(
            f.finish(g1, Err("offline".into()), Some(before)).as_deref(),
            Some("offline")
        );
        assert!(!f.contains(&gif(1).url));
        // Saved, then unsaved.
        let (_, g2, _) = f.toggle(&gif(1));
        let saved = f.list.clone().unwrap();
        assert_eq!(f.finish(g2, Ok(saved), None), None);
        let (change, _, _) = f.toggle(&gif(1));
        assert_eq!(change, FavoriteChange::Remove(gif(1).url));
        assert!(!f.contains(&gif(1).url));
    }

    #[test]
    fn stale_favourite_answers_are_ignored() {
        let mut f = Favorites::default();
        let (_, g1, _) = f.toggle(&gif(1));
        let (_, _g2, _) = f.toggle(&gif(2));
        assert_eq!(f.finish(g1, Ok(vec![]), None), None);
        assert!(f.contains(&gif(1).url) && f.contains(&gif(2).url));
    }

    #[test]
    fn newest_favourite_comes_first() {
        let mut f = Favorites::default();
        f.toggle(&gif(1));
        f.toggle(&gif(2));
        let items = f.items();
        assert_eq!(items[0].url, gif(2).url);
        assert_eq!(items[1].url, gif(1).url);
    }

    #[test]
    fn tenor_mp4_maps_to_its_still_and_gif() {
        let mp4 = "https://media.tenor.com/RrKz-Y2kmFUAAAPo/cat-cats.mp4";
        assert_eq!(
            tenor_variant(mp4, "AAAAe", "png").as_deref(),
            Some("https://media.tenor.com/RrKz-Y2kmFUAAAAe/cat-cats.png")
        );
        let m1 = "https://media1.tenor.com/m/RrKz-Y2kmFUAAAPo/cat-cats.mp4";
        assert_eq!(
            tenor_variant(m1, "AAAAC", "gif").as_deref(),
            Some("https://media1.tenor.com/m/RrKz-Y2kmFUAAAAC/cat-cats.gif")
        );
        assert_eq!(
            tenor_variant("https://example.com/a/b.mp4", "AAAAe", "png"),
            None
        );
        assert_eq!(
            tenor_variant("http://media.tenor.com/RrKz-Y2kmFUAAAPo/c.mp4", "x", "png"),
            None
        );
        assert_eq!(tenor_variant("not a url", "AAAAe", "png"), None);
        // A saved MP4 shows Tenor's still, then its GIF.
        let fav = FavoriteGif {
            url: "https://tenor.com/view/cat".into(),
            src: mp4.into(),
            format: GifFormat::Video,
            width: 0,
            height: 50,
            order: 1,
        };
        let g = gif_from_favorite(&fav);
        assert_eq!(thumb_sources(&g).len(), 2);
        assert!(g.preview.ends_with("AAAAe/cat-cats.png"));
        assert_eq!(g.width, 1);
    }

    #[test]
    fn thumbnails_are_downscaled_stills() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(1000, 500, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (w, h, px) = thumbnail(&png).unwrap();
        assert!(w <= THUMB_MAX_W && h <= THUMB_MAX_H, "{w}x{h}");
        assert_eq!(px.len(), (w * h * 4) as usize);
        assert_eq!(&px[..4], &[1, 2, 3, 255]);
        // Small images stay as they are; garbage is refused.
        let mut small = Vec::new();
        image::RgbaImage::new(10, 10)
            .write_to(
                &mut std::io::Cursor::new(&mut small),
                image::ImageFormat::Png,
            )
            .unwrap();
        assert_eq!(thumbnail(&small).unwrap().0, 10);
        assert!(thumbnail(b"not an image").is_none());
    }

    #[test]
    fn thumbnail_sources_fall_back_to_the_gif() {
        assert_eq!(thumb_sources(&gif(1)), vec![gif(1).preview, gif(1).gif_src]);
        let mut g = gif(2);
        g.preview = g.gif_src.clone();
        assert_eq!(thumb_sources(&g).len(), 1);
    }

    #[test]
    fn open_loads_trending() {
        let (mut p, g) = GifPicker::open(Point::ORIGIN);
        assert!(p.loading && p.query.is_empty());
        p.finish(g, Ok(vec![gif(1)]));
        assert!(!p.loading);
        assert_eq!(p.results.len(), 1);
    }

    #[test]
    fn only_the_last_keystroke_searches() {
        let (mut p, _) = GifPicker::open(Point::ORIGIN);
        let first = p.set_query("ca".into());
        let last = p.set_query("cat".into());
        assert_eq!(p.due(first), None);
        assert_eq!(p.due(last).as_deref(), Some("cat"));
        assert!(p.loading);
    }

    #[test]
    fn stale_responses_are_dropped() {
        let (mut p, trending) = GifPicker::open(Point::ORIGIN);
        let g = p.set_query("cat".into());
        // Trending arrives after the user started typing: ignored.
        p.finish(trending, Ok(vec![gif(1)]));
        assert!(p.results.is_empty());
        assert!(p.due(g).is_some());
        p.finish(g, Err("offline".into()));
        assert_eq!(p.error.as_deref(), Some("offline"));
    }
}
