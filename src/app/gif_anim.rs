//! GIF picker media: the on-disk cache, the favourites prefetch and the
//! animated tiles.
//!
//! * **Disk cache.** Every file the picker downloads (stills and GIFs) goes
//!   through [`picker_bytes`]: read from the bounded on-disk LRU
//!   (`<cache dir>/gifs`, 100 MB, keyed by URL hash) when present, else
//!   downloaded (at most [`FETCHES`] at a time) and stored.
//! * **Prefetch.** The first time the picker opens in a session, once the
//!   saved GIFs are known, their still and GIF are downloaded into the disk
//!   cache in the background ([`FETCHES`] at a time), so the Favorites page
//!   shows straight from disk, then and in later sessions.
//! * **Animation.** Tiles play like the official picker. The GIF rendition
//!   (Tenor's tile-sized `tinygif`, else the full GIF) is decoded off the UI
//!   thread to frames scaled to the tile ([`Class::Picker`]), at most
//!   [`DECODES`] at a time; the frame handles are made there, once, and the
//!   self-playing `ui::anim_image` widget draws them (no tick: it redraws
//!   only the visible tiles, only when their frame changes). MP4-only
//!   favourites go through the video helper's frames mode instead.
//! * **Settings.** With "Automatically play GIFs" off only the hovered tile
//!   is decoded and plays (as in the official picker); "Reduce motion"
//!   shows stills only. Tiles pause while the window is unfocused.
//! * **Memory.** Only tiles in or near the visible part of the grid are
//!   decoded; frames of tiles that scroll more than a screen away are
//!   dropped, the cache has a byte budget ([`PICKER_BUDGET`]) and closing
//!   the picker frees all of it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use iced::Task;
use tokio::sync::Semaphore;

use super::anim::{self, Anim, AnimCache, Class};
use super::message::Done;
use super::{App, Message, gifs};
use crate::media::disk_cache::DiskCache;
use crate::media::gifv::{self, FrameSpec};
use crate::media::scratch::Scratch;
use crate::media::video;
use crate::net::model::Gif;
use crate::net::rest::RestClient;

/// Decoded picker frames kept at most (about ten tiles' worth).
pub const PICKER_BUDGET: usize = 40 * 1024 * 1024;
/// Downloads at the same time (stills, GIFs, prefetch).
pub const FETCHES: usize = 4;
/// Decodes at the same time.
pub const DECODES: usize = 2;
/// Saved GIFs prefetched at most.
pub const MAX_PREFETCH: usize = 120;
/// Height of the grid's visible area (the picker minus its header).
pub const VIEW_H: f32 = 380.0;
/// Tiles this far beyond the visible area are decoded ahead of a scroll.
pub const LOAD_MARGIN: f32 = VIEW_H / 2.0;
/// Tiles farther than this from the visible area lose their frames.
pub const KEEP_MARGIN: f32 = VIEW_H;
/// Grid geometry (shared with the view).
pub const GAP: f32 = 6.0;
pub const PICKER_W: f32 = 424.0;
pub const TILE_W: f32 = (PICKER_W - 16.0 - GAP) / 2.0;

/// `Id` of the grid's scrollable.
pub const GRID_ID: &str = "gif-grid";

static FETCH_SLOTS: Semaphore = Semaphore::const_new(FETCHES);
static DECODE_SLOTS: Semaphore = Semaphore::const_new(DECODES);

/// A tile's place in the two-column masonry grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub col: usize,
    pub y: f32,
    pub h: f32,
}

/// Height of `g`'s tile at `tile_w`.
pub fn tile_height(g: &Gif, tile_w: f32) -> f32 {
    (tile_w * g.height as f32 / g.width.max(1) as f32).clamp(60.0, 320.0)
}

/// Places each GIF in the shorter column, like the view does.
pub fn masonry(gifs: &[Gif], tile_w: f32, gap: f32) -> Vec<Tile> {
    let mut heights = [0.0f32; 2];
    gifs.iter()
        .map(|g| {
            let h = tile_height(g, tile_w);
            let col = usize::from(heights[1] < heights[0]);
            let t = Tile {
                col,
                y: heights[col],
                h,
            };
            heights[col] += h + gap;
            t
        })
        .collect()
}

/// Indices of the tiles within `margin` of the visible area at `scroll_y`.
pub fn near_view(tiles: &[Tile], scroll_y: f32, margin: f32) -> Vec<usize> {
    let top = scroll_y - margin;
    let bottom = scroll_y + VIEW_H + margin;
    tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| t.y + t.h >= top && t.y <= bottom)
        .map(|(i, _)| i)
        .collect()
}

/// Where a tile's animation comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A GIF (or animated WebP), decoded in-process.
    Image(String),
    /// An MP4, decoded by the video helper's frames mode.
    Mp4(String),
}

/// A Giphy rendition of a Giphy media URL (`/media/<id>/<file>`).
pub fn giphy_variant(src: &str, file: &str) -> Option<String> {
    let mut u = url::Url::parse(src).ok()?;
    if u.scheme() != "https" || !u.host_str()?.ends_with("giphy.com") {
        return None;
    }
    let mut segs: Vec<String> = u.path_segments()?.map(str::to_owned).collect();
    let media = segs.iter().position(|s| s == "media")?;
    if segs.len() < media + 3 {
        return None;
    }
    let last = segs.len() - 1;
    segs[last] = file.to_owned();
    u.set_path(&segs.join("/"));
    u.set_query(None);
    Some(u.into())
}

/// Sources to try for a tile's animation, best first: Tenor's tile-sized
/// GIF, the GIF the API gave, Giphy's 200 px GIF, then the MP4.
pub fn anim_sources(g: &Gif) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    let mut push = |s: Source| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    let urls = [&g.gif_src, &g.src, &g.preview];
    if let Some(t) = urls
        .iter()
        .find_map(|u| gifs::tenor_variant(u, "AAAAM", "gif"))
    {
        push(Source::Image(t));
    }
    if let Some(t) = urls.iter().find_map(|u| giphy_variant(u, "200w.gif")) {
        push(Source::Image(t));
    }
    let path = |u: &str| u.split(['?', '#']).next().unwrap_or(u).to_ascii_lowercase();
    if !g.gif_src.is_empty() && !path(&g.gif_src).ends_with(".png") {
        push(Source::Image(g.gif_src.clone()));
    }
    if path(&g.src).ends_with(".mp4") {
        push(Source::Mp4(g.src.clone()));
    }
    out
}

/// The bytes of a picker file: from the disk cache, else downloaded (at
/// most [`FETCHES`] at a time) and stored there.
pub async fn picker_bytes(
    rest: &RestClient,
    disk: Option<&DiskCache>,
    url: &str,
) -> Result<Vec<u8>, String> {
    if let Some(d) = disk {
        let (d2, u) = (d.clone(), url.to_owned());
        if let Ok(Some(b)) = tokio::task::spawn_blocking(move || d2.get(&u)).await {
            return Ok(b);
        }
    }
    let bytes = {
        let _slot = FETCH_SLOTS.acquire().await.map_err(|e| e.to_string())?;
        rest.fetch_picker_media(url)
            .await
            .map_err(|e| e.to_string())?
    };
    if let Some(d) = disk {
        let (d2, u, b) = (d.clone(), url.to_owned(), bytes.clone());
        let _ = tokio::task::spawn_blocking(move || d2.put(&u, &b)).await;
    }
    Ok(bytes)
}

/// What the helper keeps of an MP4 tile.
pub fn picker_spec() -> FrameSpec {
    let lim = Class::Picker.limits();
    FrameSpec {
        max_w: lim.target_width,
        max_h: 400,
        max_frames: lim.max_frames as u32,
        max_bytes: lim.max_bytes as u64,
    }
}

/// Decodes the first source that works.
pub async fn load_anim(
    rest: RestClient,
    disk: Option<DiskCache>,
    sources: Vec<Source>,
    scratch_dir: PathBuf,
) -> Option<Arc<Anim>> {
    for s in sources {
        let anim = match s {
            Source::Image(url) => {
                let Ok(bytes) = picker_bytes(&rest, disk.as_ref(), &url).await else {
                    continue;
                };
                let _slot = DECODE_SLOTS.acquire().await.ok()?;
                tokio::task::spawn_blocking(move || {
                    anim::decode(&bytes, &Class::Picker.limits()).ok()
                })
                .await
                .ok()
                .flatten()
            }
            Source::Mp4(url) => mp4_anim(&rest, disk.as_ref(), &url, &scratch_dir).await,
        };
        if let Some(a) = anim {
            return Some(Arc::new(a));
        }
    }
    None
}

/// An MP4 tile through the helper's frames mode (the cached file is read
/// in place; without a disk cache a scratch copy is used).
async fn mp4_anim(
    rest: &RestClient,
    disk: Option<&DiskCache>,
    url: &str,
    scratch_dir: &std::path::Path,
) -> Option<Anim> {
    let helper = video::helper_path()?;
    let bytes = picker_bytes(rest, disk, url).await.ok()?;
    let cached = disk.map(|d| d.path_of(url)).filter(|p| p.is_file());
    let dir = scratch_dir.to_owned();
    let _slot = DECODE_SLOTS.acquire().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let frames = match cached {
            Some(path) => gifv::decode_with(&helper, &path, picker_spec()),
            None => {
                let (scratch, mut file) = Scratch::create(&dir, "mp4").ok()?;
                std::io::Write::write_all(&mut file, &bytes).ok()?;
                drop(file);
                let r = gifv::decode_with(&helper, scratch.path(), picker_spec());
                drop(scratch);
                r
            }
        };
        frames.ok().map(anim::from_rgba_frames)
    })
    .await
    .ok()
    .flatten()
}

/// Downloads the still and the GIF of each saved GIF into the disk cache
/// (skipping what is there). Returns how many files were fetched.
pub async fn prefetch(rest: RestClient, disk: DiskCache, favorites: Vec<Gif>) -> usize {
    futures::stream::iter(favorites.into_iter().take(MAX_PREFETCH))
        .map(|g| {
            let (rest, disk) = (rest.clone(), disk.clone());
            async move {
                let mut fetched = 0;
                let stills = gifs::thumb_sources(&g);
                let images = anim_sources(&g).into_iter().filter_map(|s| match s {
                    Source::Image(u) => Some(u),
                    Source::Mp4(_) => None,
                });
                // The first source of each kind that downloads is the one
                // the picker will ask for.
                for group in [stills, images.collect()] {
                    for url in group {
                        let (d, u) = (disk.clone(), url.clone());
                        let have = tokio::task::spawn_blocking(move || d.contains(&u))
                            .await
                            .unwrap_or(false);
                        if have {
                            break;
                        }
                        if picker_bytes(&rest, Some(&disk), &url).await.is_ok() {
                            fetched += 1;
                            break;
                        }
                    }
                }
                fetched
            }
        })
        .buffer_unordered(FETCHES)
        .fold(0, |a, b| async move { a + b })
        .await
}

/// The picker's decoded tile animations.
#[derive(Debug)]
pub struct PickerAnims {
    pub cache: AnimCache,
    /// Scroll offset of the grid (logical px from the top).
    pub scroll_y: f32,
    /// The tile under the pointer (with GIF autoplay off only it plays).
    pub hovered: Option<String>,
}

impl Default for PickerAnims {
    fn default() -> Self {
        Self {
            cache: AnimCache::new(PICKER_BUDGET),
            scroll_y: 0.0,
            hovered: None,
        }
    }
}

impl PickerAnims {
    pub fn get(&self, url: &str) -> Option<&Arc<Anim>> {
        self.cache.get(url)
    }

    /// Frees every frame (the picker closed).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Keeps the frames of `keep` only.
    pub fn retain(&mut self, keep: &HashSet<String>) {
        self.cache.retain(|k| keep.contains(k));
    }
}

impl App {
    /// Every tile plays: "Automatically play GIFs" on and motion on.
    /// With autoplay off a tile plays only while hovered, like the
    /// official picker; "Reduce motion" stops all of them.
    pub fn picker_autoplay(&self) -> bool {
        self.prefs.autoplay_gifs && !self.prefs.reduce_motion
    }

    /// Whether the tile of `url` plays now (the window must be focused).
    pub fn picker_playing(&self, url: &str) -> bool {
        !self.prefs.reduce_motion
            && self.window_focused
            && (self.picker_autoplay() || self.gif_anims.hovered.as_deref() == Some(url))
    }

    /// Tiles that may hold frames now, from the near-view `candidates`.
    fn picker_wanted<'a>(&self, candidates: impl Iterator<Item = &'a str>) -> HashSet<String> {
        let auto = self.picker_autoplay();
        candidates
            .filter(|u| auto || self.gif_anims.hovered.as_deref() == Some(*u))
            .map(str::to_owned)
            .collect()
    }

    /// The decoded animation of a picker tile, if ready.
    pub fn gif_anim(&self, url: &str) -> Option<&Arc<Anim>> {
        if self.prefs.reduce_motion {
            return None;
        }
        self.gif_anims.get(url)
    }

    /// The GIFs the picker grid shows now (empty on the home tiles).
    pub fn picker_grid(&self) -> Vec<Gif> {
        let Some(p) = &self.gif_picker else {
            return Vec::new();
        };
        match (p.searching(), p.page) {
            (false, gifs::GifPage::Home) => Vec::new(),
            (false, gifs::GifPage::Favorites) => self.gif_favorites.items(),
            _ => p.results.clone(),
        }
    }

    /// Drops the frames of tiles far from view and decodes the ones near
    /// it that are missing.
    pub(super) fn request_gif_anims(&mut self) -> Task<Message> {
        if self.gif_picker.is_none() || self.prefs.reduce_motion {
            self.gif_anims.clear();
            return Task::none();
        }
        let grid = self.picker_grid();
        let tiles = masonry(&grid, TILE_W, GAP);
        let scroll = self.gif_anims.scroll_y;
        let keep = self.picker_wanted(
            near_view(&tiles, scroll, KEEP_MARGIN)
                .into_iter()
                .map(|i| grid[i].url.as_str()),
        );
        self.gif_anims.retain(&keep);
        let load = self.picker_wanted(
            near_view(&tiles, scroll, LOAD_MARGIN)
                .into_iter()
                .map(|i| grid[i].url.as_str()),
        );
        let now = Instant::now();
        let mut tasks = Vec::new();
        for i in near_view(&tiles, scroll, LOAD_MARGIN) {
            let g = &grid[i];
            if !load.contains(&g.url) || !self.gif_anims.cache.want(&g.url, now) {
                continue;
            }
            let key = g.url.clone();
            if self.cfg.mock {
                let n = crate::mock::gif_number(&g.url);
                tasks.push(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            n.and_then(crate::mock::gif_anim).map(Arc::new)
                        })
                        .await
                        .ok()
                        .flatten()
                    },
                    move |anim| Message::Done(Done::GifAnim { key, anim }),
                ));
                continue;
            }
            let rest = self.rest.clone();
            let disk = self.gif_disk.clone();
            let sources = anim_sources(g);
            let dir = self.video_dir();
            tasks.push(Task::perform(
                load_anim(rest, disk, sources, dir),
                move |anim| Message::Done(Done::GifAnim { key, anim }),
            ));
        }
        Task::batch(tasks)
    }

    /// A tile animation finished decoding. Kept only if the picker still
    /// shows that tile near the visible area.
    pub(super) fn on_gif_anim(&mut self, key: String, anim: Option<Arc<Anim>>) -> Task<Message> {
        let wanted = self.gif_picker.is_some() && !self.prefs.reduce_motion && {
            let grid = self.picker_grid();
            let tiles = masonry(&grid, TILE_W, GAP);
            self.picker_wanted(
                near_view(&tiles, self.gif_anims.scroll_y, KEEP_MARGIN)
                    .into_iter()
                    .map(|i| grid[i].url.as_str()),
            )
            .contains(&key)
        };
        match anim {
            Some(a) if wanted => self.gif_anims.cache.insert(key, a, Class::Picker),
            Some(_) => self.gif_anims.cache.forget(&key),
            None => self.gif_anims.cache.mark_failed(&key, Instant::now()),
        }
        Task::none()
    }

    /// The pointer entered (`Some`) or left a tile.
    pub(super) fn on_gif_hover(
        &mut self,
        enter: Option<String>,
        left: Option<String>,
    ) -> Task<Message> {
        match (enter, left) {
            (Some(u), _) => self.gif_anims.hovered = Some(u),
            // A late leave of another tile does not cancel the hover.
            (None, Some(u)) if self.gif_anims.hovered.as_deref() == Some(u.as_str()) => {
                self.gif_anims.hovered = None;
            }
            _ => return Task::none(),
        }
        if self.picker_autoplay() {
            Task::none()
        } else {
            self.request_gif_anims()
        }
    }

    /// The grid scrolled to `y`.
    pub(super) fn on_gif_scrolled(&mut self, y: f32) -> Task<Message> {
        let old = self.gif_anims.scroll_y;
        self.gif_anims.scroll_y = y.max(0.0);
        // Re-plan only when the view moved by a good part of a tile.
        if (old - y).abs() < 24.0 {
            return Task::none();
        }
        self.request_gif_anims()
    }

    /// Starts the one-per-session prefetch of the saved GIFs.
    pub(super) fn prefetch_gif_favorites(&mut self) -> Task<Message> {
        if self.gif_prefetched || self.gif_picker.is_none() {
            return Task::none();
        }
        let Some(disk) = self.gif_disk.clone() else {
            return Task::none();
        };
        let favorites = self.gif_favorites.items();
        if self.gif_favorites.list.is_none() {
            return Task::none();
        }
        self.gif_prefetched = true;
        let rest = self.rest.clone();
        Task::perform(prefetch(rest, disk, favorites), |n| {
            Message::Done(Done::GifPrefetched(n))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(n: u32, w: u32, h: u32) -> Gif {
        Gif {
            url: format!("https://tenor.com/view/{n}"),
            preview: format!("https://media.tenor.com/RrKz-Y2kmFUAAAAe/c{n}.png"),
            gif_src: format!("https://media.tenor.com/RrKz-Y2kmFUAAAAC/c{n}.gif"),
            src: format!("https://media.tenor.com/RrKz-Y2kmFUAAAPo/c{n}.mp4"),
            width: w,
            height: h,
        }
    }

    #[test]
    fn masonry_fills_the_shorter_column() {
        let gifs = [gif(1, 200, 200), gif(2, 200, 100), gif(3, 200, 100)];
        let t = masonry(&gifs, 200.0, 6.0);
        assert_eq!(
            t[0],
            Tile {
                col: 0,
                y: 0.0,
                h: 200.0
            }
        );
        assert_eq!(
            t[1],
            Tile {
                col: 1,
                y: 0.0,
                h: 100.0
            }
        );
        assert_eq!(
            t[2],
            Tile {
                col: 1,
                y: 106.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn only_tiles_near_the_view_are_kept() {
        let gifs: Vec<Gif> = (0..40).map(|n| gif(n, 200, 200)).collect();
        let t = masonry(&gifs, 200.0, 6.0);
        // At the top: the first rows (two columns of 206 px rows).
        let top = near_view(&t, 0.0, 0.0);
        assert!(top.contains(&0) && top.contains(&1) && !top.contains(&10));
        // Far down: the first tiles are gone.
        let down = near_view(&t, 2000.0, KEEP_MARGIN);
        assert!(!down.contains(&0) && down.contains(&20));
        // The load band is inside the keep band: nothing is decoded only
        // to be dropped at once.
        let load: HashSet<_> = near_view(&t, 900.0, LOAD_MARGIN).into_iter().collect();
        let keep: HashSet<_> = near_view(&t, 900.0, KEEP_MARGIN).into_iter().collect();
        assert!(load.is_subset(&keep));
    }

    #[test]
    fn sources_prefer_the_tile_sized_gif_then_fall_back() {
        let s = anim_sources(&gif(1, 200, 200));
        assert_eq!(
            s,
            vec![
                Source::Image("https://media.tenor.com/RrKz-Y2kmFUAAAAM/c1.gif".into()),
                Source::Image("https://media.tenor.com/RrKz-Y2kmFUAAAAC/c1.gif".into()),
                Source::Mp4("https://media.tenor.com/RrKz-Y2kmFUAAAPo/c1.mp4".into()),
            ]
        );
        // A saved Giphy MP4: its 200 px GIF, then the MP4.
        let g = Gif {
            url: "https://giphy.com/gifs/x".into(),
            preview: String::new(),
            gif_src: String::new(),
            src: "https://media2.giphy.com/media/abc123/giphy.mp4?cid=1".into(),
            width: 1,
            height: 1,
        };
        assert_eq!(
            anim_sources(&g),
            vec![
                Source::Image("https://media2.giphy.com/media/abc123/200w.gif".into()),
                Source::Mp4("https://media2.giphy.com/media/abc123/giphy.mp4?cid=1".into()),
            ]
        );
        assert_eq!(
            giphy_variant("https://example.com/media/a/b.mp4", "x"),
            None
        );
        assert_eq!(giphy_variant("https://giphy.com/x.mp4", "x"), None);
    }

    #[test]
    fn picker_cache_is_bounded_and_cleared() {
        let mut p = PickerAnims::default();
        let frame = || {
            anim::from_rgba_frames(vec![
                (
                    512,
                    512,
                    vec![0u8; 512 * 512 * 4],
                    std::time::Duration::from_millis(50),
                ),
                (
                    512,
                    512,
                    vec![0u8; 512 * 512 * 4],
                    std::time::Duration::from_millis(50),
                ),
            ])
        };
        for n in 0..40 {
            p.cache
                .insert(format!("k{n}"), Arc::new(frame()), Class::Picker);
        }
        assert!(p.cache.bytes() <= PICKER_BUDGET);
        let keep: HashSet<String> = ["k39".to_owned()].into();
        p.retain(&keep);
        assert_eq!(p.cache.len(), 1);
        p.scroll_y = 50.0;
        p.clear();
        assert!(p.cache.is_empty() && p.scroll_y == 0.0);
    }
}
