//! Animation engine: GIF / APNG / animated WebP decoding off the UI thread,
//! a byte-bounded LRU of decoded frames, and the clock that decides which
//! frame is on screen.
//!
//! Rules that keep it light:
//! * frames are decoded in `spawn_blocking`, downscaled to the displayed
//!   size, and wrapped in `image::Handle`s once (never in `view`);
//! * every asset is capped in source frames, source size, kept frames and
//!   bytes (extra frames are dropped by merging delays, so the animation
//!   keeps its duration);
//! * the cache is an LRU with a total byte budget, and whole classes
//!   (profile effects, banners) are dropped when the popout closes;
//! * the app ticks only while something animated is hovered or the popout
//!   is open (see [`AnimState::active`]).

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::image::Handle;
use image::{AnimationDecoder, ImageDecoder};

use crate::net::model::{MessageId, UserId};

/// Total decoded-frame budget of the cache.
pub const CACHE_BUDGET: usize = 32 * 1024 * 1024;
/// A failed fetch / decode is retried after this long.
pub const RETRY_AFTER: Duration = Duration::from_secs(300);
/// Frames are never shown for less than this (GIF delay 0 means "default").
pub const MIN_DELAY: Duration = Duration::from_millis(20);
const DEFAULT_DELAY: Duration = Duration::from_millis(100);
/// Tick period while animating (about 15 fps).
pub const TICK: Duration = Duration::from_millis(66);
/// Larger sources are refused before decoding.
pub const MAX_SOURCE_DIM: u32 = 2048;

/// What an animation is for; decides eviction on popout close and budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    Avatar,
    Decoration,
    Banner,
    Effect,
    Emoji,
}

impl Class {
    /// Held only while the popout is open.
    pub fn popout_only(self) -> bool {
        matches!(self, Self::Banner | Self::Effect)
    }

    pub fn limits(self) -> Limits {
        match self {
            Self::Avatar => Limits::new(80, 24, 2 * MB, 160),
            Self::Decoration => Limits::new(80, 40, 3 * MB, 240),
            Self::Banner => Limits::new(80, 48, 8 * MB, 340),
            // Effect canvases are large: decode at a reduced width, the
            // view scales them to the card.
            Self::Effect => Limits::new(120, 40, 10 * MB, 256),
            Self::Emoji => Limits::new(80, 32, MB, 96),
        }
    }
}

const MB: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Source frames read at most.
    pub max_read: usize,
    /// Frames kept at most.
    pub max_frames: usize,
    /// Bytes of kept frames at most.
    pub max_bytes: usize,
    /// Frames wider than this are downscaled to it.
    pub target_width: u32,
}

impl Limits {
    pub const fn new(max_read: usize, max_frames: usize, max_bytes: usize, w: u32) -> Self {
        Self {
            max_read,
            max_frames,
            max_bytes,
            target_width: w,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub handle: Handle,
    pub delay: Duration,
}

/// Decoded animation. Cloning shares the frames.
#[derive(Debug)]
pub struct Anim {
    pub frames: Vec<Frame>,
    pub total: Duration,
    pub bytes: usize,
    pub size: (u32, u32),
}

impl Anim {
    /// Frame showing `elapsed` after the start (the animation loops).
    pub fn frame_at(&self, elapsed: Duration) -> &Handle {
        &self.frames[self.index_at(elapsed)].handle
    }

    pub fn index_at(&self, elapsed: Duration) -> usize {
        if self.frames.len() <= 1 || self.total.is_zero() {
            return 0;
        }
        let mut t = Duration::from_nanos((elapsed.as_nanos() % self.total.as_nanos()) as u64);
        for (i, f) in self.frames.iter().enumerate() {
            if t < f.delay {
                return i;
            }
            t -= f.delay;
        }
        self.frames.len() - 1
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnimError {
    /// Not a GIF / APNG / WebP, or damaged.
    Unsupported,
    /// A single frame: keep using the static image.
    NotAnimated,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gif,
    Png,
    Webp,
}

pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"GIF8") {
        Some(Format::Gif)
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(Format::Png)
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Format::Webp)
    } else {
        None
    }
}

struct Raw {
    w: u32,
    h: u32,
    px: Vec<u8>,
    delay: Duration,
}

fn delay_of(d: image::Delay) -> Duration {
    let (n, den) = d.numer_denom_ms();
    let ms = n.checked_div(den).unwrap_or(0);
    let d = Duration::from_millis(u64::from(ms));
    if d <= Duration::from_millis(10) {
        DEFAULT_DELAY
    } else {
        d.max(MIN_DELAY)
    }
}

/// Merges neighbouring frames (dropping every second one, adding its delay
/// to the kept one): half the memory, same duration.
fn decimate(v: &mut Vec<Raw>) {
    let old = std::mem::take(v);
    let mut it = old.into_iter();
    while let Some(mut a) = it.next() {
        if let Some(b) = it.next() {
            a.delay += b.delay;
        }
        v.push(a);
    }
}

fn collect<'a>(frames: image::Frames<'a>, lim: &Limits) -> Result<Vec<Raw>, AnimError> {
    let mut out: Vec<Raw> = Vec::new();
    let mut bytes = 0usize;
    for (n, f) in frames.enumerate() {
        if n >= lim.max_read {
            break;
        }
        let Ok(f) = f else { break };
        let delay = delay_of(f.delay());
        let mut buf = f.into_buffer();
        if buf.width() > lim.target_width && lim.target_width > 0 {
            let h = ((u64::from(buf.height()) * u64::from(lim.target_width))
                / u64::from(buf.width()))
            .max(1) as u32;
            buf = image::imageops::resize(
                &buf,
                lim.target_width,
                h,
                image::imageops::FilterType::Triangle,
            );
        }
        let (w, h) = buf.dimensions();
        let px = buf.into_raw();
        bytes += px.len();
        out.push(Raw { w, h, px, delay });
        while out.len() > lim.max_frames || bytes > lim.max_bytes {
            if out.len() < 2 {
                return Err(AnimError::TooLarge);
            }
            decimate(&mut out);
            bytes = out.iter().map(|r| r.px.len()).sum();
        }
    }
    Ok(out)
}

/// Decodes an animated image. Single-frame images are
/// [`AnimError::NotAnimated`].
pub fn decode(bytes: &[u8], lim: &Limits) -> Result<Anim, AnimError> {
    let fmt = sniff(bytes).ok_or(AnimError::Unsupported)?;
    let cur = Cursor::new(bytes);
    let too_big = |(w, h): (u32, u32)| w > MAX_SOURCE_DIM || h > MAX_SOURCE_DIM;
    let raws = match fmt {
        Format::Gif => {
            let d = image::codecs::gif::GifDecoder::new(cur).map_err(|_| AnimError::Unsupported)?;
            if too_big(d.dimensions()) {
                return Err(AnimError::TooLarge);
            }
            collect(d.into_frames(), lim)?
        }
        Format::Png => {
            let d = image::codecs::png::PngDecoder::new(cur).map_err(|_| AnimError::Unsupported)?;
            if too_big(d.dimensions()) {
                return Err(AnimError::TooLarge);
            }
            if !d.is_apng().unwrap_or(false) {
                return Err(AnimError::NotAnimated);
            }
            let a = d.apng().map_err(|_| AnimError::Unsupported)?;
            collect(a.into_frames(), lim)?
        }
        Format::Webp => {
            let d =
                image::codecs::webp::WebPDecoder::new(cur).map_err(|_| AnimError::Unsupported)?;
            if too_big(d.dimensions()) {
                return Err(AnimError::TooLarge);
            }
            if !d.has_animation() {
                return Err(AnimError::NotAnimated);
            }
            collect(d.into_frames(), lim)?
        }
    };
    if raws.len() < 2 {
        return Err(AnimError::NotAnimated);
    }
    Ok(build(raws))
}

fn build(raws: Vec<Raw>) -> Anim {
    let size = raws.first().map_or((0, 0), |r| (r.w, r.h));
    let mut total = Duration::ZERO;
    let mut bytes = 0;
    let frames = raws
        .into_iter()
        .map(|r| {
            total += r.delay;
            bytes += r.px.len();
            Frame {
                handle: Handle::from_rgba(r.w, r.h, r.px),
                delay: r.delay,
            }
        })
        .collect();
    Anim {
        frames,
        total,
        bytes,
        size,
    }
}

/// Builds an animation from ready RGBA frames (mock data and tests).
pub fn from_rgba_frames(frames: Vec<(u32, u32, Vec<u8>, Duration)>) -> Anim {
    build(
        frames
            .into_iter()
            .map(|(w, h, px, delay)| Raw { w, h, px, delay })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Entry {
    anim: Arc<Anim>,
    class: Class,
    used: u64,
}

/// LRU of decoded animations with a total byte budget.
#[derive(Debug)]
pub struct AnimCache {
    entries: HashMap<String, Entry>,
    pending: HashSet<String>,
    failed: HashMap<String, Instant>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl Default for AnimCache {
    fn default() -> Self {
        Self::new(CACHE_BUDGET)
    }
}

impl AnimCache {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
            bytes: 0,
            budget,
            clock: 0,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Arc<Anim>> {
        self.entries.get(key).map(|e| &e.anim)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// True if `key` should be fetched now; marks it pending.
    pub fn want(&mut self, key: &str, now: Instant) -> bool {
        if self.entries.contains_key(key) || self.pending.contains(key) {
            return false;
        }
        if let Some(at) = self.failed.get(key) {
            if now.saturating_duration_since(*at) < RETRY_AFTER {
                return false;
            }
            self.failed.remove(key);
        }
        self.pending.insert(key.to_owned());
        true
    }

    pub fn mark_failed(&mut self, key: &str, now: Instant) {
        self.pending.remove(key);
        if self.failed.len() >= 512 {
            self.failed.clear();
        }
        self.failed.insert(key.to_owned(), now);
    }

    /// Marks `key` most recently used.
    pub fn touch(&mut self, key: &str) {
        self.clock += 1;
        if let Some(e) = self.entries.get_mut(key) {
            e.used = self.clock;
        }
    }

    pub fn insert(&mut self, key: String, anim: Arc<Anim>, class: Class) {
        self.pending.remove(&key);
        self.remove(&key);
        self.clock += 1;
        self.bytes += anim.bytes;
        self.entries.insert(
            key.clone(),
            Entry {
                anim,
                class,
                used: self.clock,
            },
        );
        // Evict least recently used first, never the entry just inserted.
        while self.bytes > self.budget && self.entries.len() > 1 {
            let victim = self
                .entries
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => self.remove(&k),
                None => break,
            }
        }
    }

    fn remove(&mut self, key: &str) {
        if let Some(e) = self.entries.remove(key) {
            self.bytes -= e.anim.bytes;
        }
    }

    /// Drops every entry of the classes held only while the popout is open
    /// (and forgets their in-flight requests).
    pub fn drop_popout_only(&mut self) {
        let keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.class.popout_only())
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            self.remove(&k);
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Clock and hover
// ---------------------------------------------------------------------------

/// What the pointer is over: a user's avatar area (list row, message row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hover {
    pub user: UserId,
    pub message: Option<MessageId>,
}

#[derive(Debug)]
pub struct AnimState {
    pub cache: AnimCache,
    pub hover: Option<Hover>,
    hover_since: Instant,
    /// Hovered thing has something animated (cached or loading).
    pub hover_hot: bool,
    popout_since: Option<Instant>,
    /// The open popout has something animated.
    pub popout_hot: bool,
    now: Instant,
}

impl Default for AnimState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            cache: AnimCache::default(),
            hover: None,
            hover_since: now,
            hover_hot: false,
            popout_since: None,
            popout_hot: false,
            now,
        }
    }
}

impl AnimState {
    /// Pointer entered `h`. `hot`: it has animated assets.
    pub fn enter(&mut self, h: Hover, hot: bool, now: Instant) {
        if self.hover != Some(h) {
            self.hover_since = now;
        }
        self.hover = Some(h);
        self.hover_hot = hot;
        self.now = now;
    }

    /// Pointer left `h`. Leaves are ignored when the pointer has already
    /// entered something else (the events of two rows arrive in either
    /// order).
    pub fn leave(&mut self, h: Hover) {
        if self.hover == Some(h) {
            self.hover = None;
            self.hover_hot = false;
        }
    }

    pub fn open_popout(&mut self, hot: bool, now: Instant) {
        self.popout_since = Some(now);
        self.popout_hot = hot;
        self.now = now;
    }

    pub fn close_popout(&mut self) {
        self.popout_since = None;
        self.popout_hot = false;
        self.cache.drop_popout_only();
    }

    pub fn tick(&mut self, now: Instant) {
        self.now = now;
    }

    /// Whether a tick subscription is needed. `reduce_motion` turns every
    /// animation off.
    pub fn active(&self, reduce_motion: bool) -> bool {
        !reduce_motion
            && ((self.hover.is_some() && self.hover_hot)
                || (self.popout_since.is_some() && self.popout_hot))
    }

    pub fn hover_elapsed(&self) -> Duration {
        self.now.saturating_duration_since(self.hover_since)
    }

    pub fn popout_elapsed(&self) -> Duration {
        self.popout_since
            .map_or(Duration::ZERO, |s| self.now.saturating_duration_since(s))
    }

    pub fn is_hovering_user(&self, user: UserId) -> bool {
        self.hover.is_some_and(|h| h.user == user)
    }

    pub fn is_hovering_message(&self, m: MessageId) -> bool {
        self.hover.is_some_and(|h| h.message == Some(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::Snowflake;
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame as IFrame, RgbaImage};

    fn gif(frames: usize, w: u32, h: u32, delay_ms: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            enc.set_repeat(Repeat::Infinite).unwrap();
            for i in 0..frames {
                let img =
                    RgbaImage::from_pixel(w, h, image::Rgba([(i * 20 % 256) as u8, 40, 200, 255]));
                enc.encode_frame(IFrame::from_parts(
                    img,
                    0,
                    0,
                    Delay::from_numer_denom_ms(delay_ms, 1),
                ))
                .unwrap();
            }
        }
        out
    }

    fn apng(frames: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, 4, 4);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_animated(frames, 0).unwrap();
        enc.set_frame_delay(1, 10).unwrap();
        let mut w = enc.write_header().unwrap();
        for i in 0..frames {
            w.write_image_data(&[(i * 40) as u8; 4 * 4 * 4]).unwrap();
        }
        w.finish().unwrap();
        out
    }

    #[test]
    fn decodes_apng_like_decorations() {
        let lim = Limits::new(100, 50, MB, 1000);
        let a = decode(&apng(6), &lim).unwrap();
        assert_eq!(a.frames.len(), 6);
        assert_eq!(a.total, Duration::from_millis(600));
        assert_eq!(sniff(&apng(2)), Some(Format::Png));
        // A plain PNG is not animated; truncation never panics.
        let b = apng(6);
        let _ = decode(&b[..b.len() / 2], &lim);
    }

    #[test]
    fn sniffs_formats() {
        assert_eq!(sniff(b"GIF89a.."), Some(Format::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::Webp));
        assert_eq!(sniff(b"<html>"), None);
        assert_eq!(sniff(b""), None);
    }

    fn anim_of(n: usize, ms: u64) -> Anim {
        from_rgba_frames(
            (0..n)
                .map(|i| (1, 1, vec![i as u8, 0, 0, 255], Duration::from_millis(ms)))
                .collect(),
        )
    }

    #[test]
    fn decodes_gif_frames_and_delays() {
        let lim = Limits::new(100, 50, MB, 1000);
        let a = decode(&gif(5, 8, 8, 100), &lim).unwrap();
        assert_eq!(a.frames.len(), 5);
        assert_eq!(a.size, (8, 8));
        assert_eq!(a.total, Duration::from_millis(500));
        assert_eq!(a.bytes, 5 * 8 * 8 * 4);
    }

    #[test]
    fn downscales_to_the_target_width() {
        let lim = Limits::new(100, 50, MB, 4);
        let a = decode(&gif(3, 16, 8, 100), &lim).unwrap();
        assert_eq!(a.size, (4, 2));
        assert_eq!(a.bytes, 3 * 4 * 2 * 4);
    }

    #[test]
    fn frame_count_and_bytes_are_capped_keeping_duration() {
        // 40 frames of 100 ms with room for 10: decimated, same duration.
        let lim = Limits::new(100, 10, MB, 100);
        let a = decode(&gif(40, 4, 4, 100), &lim).unwrap();
        assert!(a.frames.len() <= 10 && a.frames.len() >= 5);
        assert_eq!(a.total, Duration::from_millis(4000));
        // Byte cap: 20 frames of 64 bytes, 300 bytes allowed.
        let lim = Limits::new(100, 100, 300, 100);
        let a = decode(&gif(20, 4, 4, 100), &lim).unwrap();
        assert!(a.bytes <= 300, "{}", a.bytes);
        assert_eq!(a.total, Duration::from_millis(2000));
        // Source frame cap.
        let lim = Limits::new(3, 100, MB, 100);
        assert_eq!(decode(&gif(10, 4, 4, 100), &lim).unwrap().frames.len(), 3);
    }

    #[test]
    fn single_frames_and_garbage_are_rejected() {
        let lim = Limits::new(100, 50, MB, 100);
        assert_eq!(
            decode(&gif(1, 4, 4, 100), &lim).unwrap_err(),
            AnimError::NotAnimated
        );
        assert_eq!(
            decode(b"not an image", &lim).unwrap_err(),
            AnimError::Unsupported
        );
        assert_eq!(decode(b"GIF89a", &lim).unwrap_err(), AnimError::Unsupported);
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(4, 4)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decode(&png, &lim).unwrap_err(), AnimError::NotAnimated);
        // Truncated animated data never panics.
        let g = gif(5, 8, 8, 100);
        let _ = decode(&g[..g.len() / 2], &lim);
        // Absurd dimensions are refused before decoding any frame.
        let mut huge = gif(2, 4, 4, 100);
        huge[6..10].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        assert!(decode(&huge, &lim).is_err());
    }

    #[test]
    fn zero_delays_use_the_browser_default() {
        let lim = Limits::new(100, 50, MB, 100);
        let a = decode(&gif(3, 4, 4, 0), &lim).unwrap();
        assert!(a.frames.iter().all(|f| f.delay == DEFAULT_DELAY));
    }

    #[test]
    fn frame_selection_loops() {
        let a = anim_of(4, 100);
        assert_eq!(a.index_at(Duration::ZERO), 0);
        assert_eq!(a.index_at(Duration::from_millis(99)), 0);
        assert_eq!(a.index_at(Duration::from_millis(100)), 1);
        assert_eq!(a.index_at(Duration::from_millis(350)), 3);
        // Loops after the total (400 ms).
        assert_eq!(a.index_at(Duration::from_millis(400)), 0);
        assert_eq!(a.index_at(Duration::from_millis(1250)), 0);
        assert_eq!(a.index_at(Duration::from_millis(1550)), 3);
        assert_eq!(a.index_at(Duration::from_millis(1650)), 0);
        // Uneven delays.
        let b = from_rgba_frames(vec![
            (1, 1, vec![0; 4], Duration::from_millis(50)),
            (1, 1, vec![0; 4], Duration::from_millis(300)),
        ]);
        assert_eq!(b.index_at(Duration::from_millis(49)), 0);
        assert_eq!(b.index_at(Duration::from_millis(50)), 1);
        assert_eq!(b.index_at(Duration::from_millis(349)), 1);
        assert_eq!(b.index_at(Duration::from_millis(350)), 0);
        // frame_at hands out the matching handle.
        assert_eq!(
            b.frame_at(Duration::from_millis(60)).id(),
            b.frames[1].handle.id()
        );
    }

    #[test]
    fn cache_evicts_least_recently_used_over_budget() {
        let mut c = AnimCache::new(100);
        let mk = |n| {
            Arc::new(Anim {
                frames: vec![],
                total: Duration::ZERO,
                bytes: n,
                size: (0, 0),
            })
        };
        c.insert("a".into(), mk(40), Class::Avatar);
        c.insert("b".into(), mk(40), Class::Avatar);
        c.touch("a");
        c.insert("c".into(), mk(40), Class::Avatar);
        assert!(c.contains("a") && !c.contains("b") && c.contains("c"));
        assert!(c.bytes() <= 100);
        // An oversized entry is kept alone rather than looping forever.
        c.insert("big".into(), mk(500), Class::Effect);
        assert_eq!(c.len(), 1);
        assert!(c.contains("big"));
        // Replacing a key does not double count.
        c.insert("big".into(), mk(50), Class::Effect);
        assert_eq!(c.bytes(), 50);
    }

    #[test]
    fn closing_the_popout_drops_effects_and_banners_only() {
        let mut s = AnimState::default();
        let mk = |n| {
            Arc::new(Anim {
                frames: vec![],
                total: Duration::ZERO,
                bytes: n,
                size: (0, 0),
            })
        };
        s.cache.insert("av".into(), mk(10), Class::Avatar);
        s.cache.insert("ef".into(), mk(10), Class::Effect);
        s.cache.insert("bn".into(), mk(10), Class::Banner);
        s.cache.insert("dc".into(), mk(10), Class::Decoration);
        s.open_popout(true, Instant::now());
        s.close_popout();
        assert!(s.cache.contains("av") && s.cache.contains("dc"));
        assert!(!s.cache.contains("ef") && !s.cache.contains("bn"));
        assert_eq!(s.cache.bytes(), 20);
    }

    #[test]
    fn want_dedupes_and_retries_failures() {
        let mut c = AnimCache::new(100);
        let t0 = Instant::now();
        assert!(c.want("x", t0));
        assert!(!c.want("x", t0));
        c.mark_failed("x", t0);
        assert!(!c.want("x", t0));
        assert!(c.want("x", t0 + RETRY_AFTER + Duration::from_secs(1)));
    }

    #[test]
    fn ticking_starts_and_stops_with_hover_and_popout() {
        let mut s = AnimState::default();
        let t0 = Instant::now();
        let h = Hover {
            user: Snowflake(1),
            message: None,
        };
        let other = Hover {
            user: Snowflake(2),
            message: None,
        };
        assert!(!s.active(false));
        // Hovering something static never ticks.
        s.enter(h, false, t0);
        assert!(!s.active(false));
        s.enter(h, true, t0);
        assert!(s.active(false));
        // Reduce motion overrides everything.
        assert!(!s.active(true));
        // A late leave of another row does not cancel the hover...
        s.leave(other);
        assert!(s.active(false));
        // ...but leaving the hovered row does.
        s.leave(h);
        assert!(!s.active(false));
        // Popout.
        s.open_popout(false, t0);
        assert!(!s.active(false));
        s.close_popout();
        s.open_popout(true, t0);
        assert!(s.active(false));
        s.close_popout();
        assert!(!s.active(false));
    }

    #[test]
    fn hover_restarts_the_clock_only_on_a_new_target() {
        let mut s = AnimState::default();
        let t0 = Instant::now();
        let h = Hover {
            user: Snowflake(1),
            message: Some(Snowflake(9)),
        };
        s.enter(h, true, t0);
        s.tick(t0 + Duration::from_millis(500));
        assert_eq!(s.hover_elapsed(), Duration::from_millis(500));
        // Re-entering the same target keeps the phase.
        s.enter(h, true, t0 + Duration::from_millis(500));
        assert_eq!(s.hover_elapsed(), Duration::from_millis(500));
        let h2 = Hover {
            user: Snowflake(2),
            message: None,
        };
        s.enter(h2, true, t0 + Duration::from_millis(600));
        assert_eq!(s.hover_elapsed(), Duration::ZERO);
        assert!(s.is_hovering_user(Snowflake(2)) && !s.is_hovering_user(Snowflake(1)));
        assert!(!s.is_hovering_message(Snowflake(9)));
    }
}
