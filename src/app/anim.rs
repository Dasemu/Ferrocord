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
//! * nothing ticks: every animation is drawn by the self-playing
//!   `ui::anim_image` widget, which asks for a redraw only when its own
//!   next frame is due (on a shared 60 Hz grid) and only while on screen.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::image::Handle;
use image::{AnimationDecoder, ImageDecoder};

use crate::net::model::{MessageId, UserId};

/// Total decoded-frame budget of the cache (cosmetics plus the GIFs,
/// stickers and emoji of the open conversation).
pub const CACHE_BUDGET: usize = 80 * 1024 * 1024;
/// A failed fetch / decode is retried after this long.
pub const RETRY_AFTER: Duration = Duration::from_secs(300);
/// Fallback wait when a frame lookup falls off the end (rounding).
pub const MIN_DELAY: Duration = Duration::from_millis(10);
/// Delays of 10 ms or less mean "as fast as possible" in old GIFs; like
/// Chromium (the official client) they are shown for 100 ms.
const DEFAULT_DELAY: Duration = Duration::from_millis(100);
const FAST_DELAY: Duration = Duration::from_millis(10);
/// Larger sources are refused before decoding.
pub const MAX_SOURCE_DIM: u32 = 2048;

/// What an animation is for; decides eviction on popout close and budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Avatar in the popout.
    Avatar,
    /// Avatar in lists and messages (plays while its row is hovered).
    AvatarList,
    /// Decoration in the popout.
    Decoration,
    /// Decoration in lists and messages.
    DecorationList,
    Banner,
    Effect,
    Emoji,
    /// GIF / animated WebP attachments and embed images in messages.
    Media,
    /// GIF / APNG stickers in messages.
    Sticker,
    /// `gifv` embeds (Tenor / Giphy MP4s), decoded by the video helper's
    /// frames mode (`media::gifv`).
    Gifv,
    /// GIF-picker tiles (kept in the picker's own cache, `gifs::PickerAnims`).
    Picker,
}

impl Class {
    /// Held only while the popout is open.
    pub fn popout_only(self) -> bool {
        matches!(self, Self::Banner | Self::Effect)
    }

    pub fn limits(self) -> Limits {
        match self {
            // Frames are decoded at about 1.5-2x the size they are drawn
            // at (enough for 125-200 % scaling) and every source frame is
            // read: a typical asset keeps all of its frames, so it plays at
            // its own frame rate. Only unusually long or large ones are
            // decimated (evenly, keeping their duration).
            // Drawn at 80 px in the popout.
            Self::Avatar => Limits::new(600, 150, 6 * MB, 120).round(),
            // Drawn at 32-40 px.
            Self::AvatarList => Limits::new(600, 150, 3 * MB, 80).round(),
            // Drawn at 96 px (80 px avatar x 1.2).
            Self::Decoration => Limits::new(600, 240, 12 * MB, 128),
            // Drawn at 38-48 px.
            Self::DecorationList => Limits::new(600, 240, 4 * MB, 72),
            // Drawn 340 px wide; popout only.
            Self::Banner => Limits::new(600, 150, 12 * MB, 340),
            // Effect canvases are large: decode at a reduced width, the
            // view scales them to the card. Popout only.
            Self::Effect => Limits::new(600, 150, 10 * MB, 200),
            // Drawn at 22 px inline, 48 px jumbo.
            Self::Emoji => Limits::new(600, 150, 2 * MB, 64),
            // Shown at most 400 px wide (the chat's media box).
            Self::Media => Limits::new(600, 150, 16 * MB, 400),
            Self::Sticker => Limits::new(600, 150, 8 * MB, 160),
            // A GIF-picker tile, decoded at the tile width so it is
            // drawn 1:1 (tiny-skia's fast path for opaque images, no
            // per-redraw scaling). Picker only.
            Self::Picker => Limits::new(600, 150, 6 * MB, super::gif_anim::TILE_W as u32),
            // Fitted by the helper to 90 % of the 400 x 300 media box (a
            // 498 x 280 Tenor clip keeps about 34 frames instead of 17):
            // a clip has many frames to spread over its budget.
            Self::Gifv => Limits::new(0, 150, 10 * MB, 360),
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
    /// Cut the frames to a circle (avatars: tiny-skia ignores the image
    /// border radius).
    pub round: bool,
}

impl Limits {
    pub const fn new(max_read: usize, max_frames: usize, max_bytes: usize, w: u32) -> Self {
        Self {
            max_read,
            max_frames,
            max_bytes,
            target_width: w,
            round: false,
        }
    }

    pub const fn round(mut self) -> Self {
        self.round = true;
        self
    }
}

/// Makes everything outside the inscribed circle of a `w` x `h` RGBA
/// buffer transparent, with a one-pixel anti-aliased edge.
pub fn round_mask(w: u32, h: u32, px: &mut [u8]) {
    if w == 0 || h == 0 || px.len() < (w as usize) * (h as usize) * 4 {
        return;
    }
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let r = cx.min(cy);
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let cover = (r - d + 0.5).clamp(0.0, 1.0);
            if cover < 1.0 {
                let i = ((y * w + x) * 4) as usize;
                // Premultiplied or not, scaling every channel keeps the
                // pixel consistent.
                for c in &mut px[i..i + 4] {
                    *c = (f32::from(*c) * cover).round() as u8;
                }
            }
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

    /// Time from `elapsed` until the shown frame changes; `None` for a
    /// still image (nothing to redraw).
    pub fn until_next(&self, elapsed: Duration) -> Option<Duration> {
        if self.frames.len() <= 1 || self.total.is_zero() {
            return None;
        }
        let mut t = Duration::from_nanos((elapsed.as_nanos() % self.total.as_nanos()) as u64);
        for f in &self.frames {
            if t < f.delay {
                return Some(f.delay - t);
            }
            t -= f.delay;
        }
        Some(MIN_DELAY)
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

/// A frame's delay, exactly (APNG and WebP delays are fractions: 1/60 s is
/// 16.67 ms, not 16 or 20). Only "as fast as possible" delays (10 ms or
/// less) are changed, to the browser default.
pub fn delay_of(d: image::Delay) -> Duration {
    let (n, den) = d.numer_denom_ms();
    let d = if den == 0 {
        Duration::ZERO
    } else {
        Duration::from_nanos(u64::from(n) * 1_000_000 / u64::from(den))
    };
    if d <= FAST_DELAY { DEFAULT_DELAY } else { d }
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

/// Reads the frames, keeping one in every `stride` source frames (the
/// dropped ones add their delay to the kept one). The stride doubles each
/// time a limit is hit, so every kept frame stands for the same number of
/// source frames and the animation keeps an even pace.
fn collect<'a>(frames: image::Frames<'a>, lim: &Limits) -> Result<Vec<Raw>, AnimError> {
    let mut out: Vec<Raw> = Vec::new();
    let mut bytes = 0usize;
    let mut stride = 1usize;
    // Source frames merged into `out.last()`.
    let mut merged = 0usize;
    for (n, f) in frames.enumerate() {
        if n >= lim.max_read {
            break;
        }
        let Ok(f) = f else { break };
        let delay = delay_of(f.delay());
        if merged > 0
            && merged < stride
            && let Some(last) = out.last_mut()
        {
            last.delay += delay;
            merged += 1;
            continue;
        }
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
        let mut px = buf.into_raw();
        if lim.round {
            round_mask(w, h, &mut px);
        }
        bytes += px.len();
        out.push(Raw { w, h, px, delay });
        merged = 1;
        while out.len() > lim.max_frames || bytes > lim.max_bytes {
            if out.len() < 2 {
                return Err(AnimError::TooLarge);
            }
            // An even count pairs the last frame with the one before it.
            if out.len().is_multiple_of(2) {
                merged += stride;
            }
            decimate(&mut out);
            stride *= 2;
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

/// Runs ready RGBA frames through the same limits as a decoded file
/// (frame / byte caps, downscaling, rounding): mock mode uses it so its
/// cosmetics behave like real ones, and tests use it to measure pacing.
pub fn from_frames_limited(
    frames: Vec<(u32, u32, Vec<u8>, Duration)>,
    lim: &Limits,
) -> Result<Anim, AnimError> {
    let it = frames.into_iter().map(|(w, h, px, d)| {
        let buf = image::RgbaImage::from_raw(w, h, px).ok_or(image::ImageError::Limits(
            image::error::LimitError::from_kind(image::error::LimitErrorKind::DimensionError),
        ))?;
        Ok(image::Frame::from_parts(
            buf,
            0,
            0,
            image::Delay::from_saturating_duration(d),
        ))
    });
    let raws = collect(image::Frames::new(Box::new(it)), lim)?;
    if raws.len() < 2 {
        return Err(AnimError::NotAnimated);
    }
    Ok(build(raws))
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

    /// Keeps only the entries whose key passes `keep`.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let gone: Vec<String> = self.entries.keys().filter(|k| !keep(k)).cloned().collect();
        for k in gone {
            self.remove(&k);
        }
    }

    /// Forgets an in-flight request whose result is not wanted any more
    /// (it may be asked for again).
    pub fn forget(&mut self, key: &str) {
        self.pending.remove(key);
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
/// Animated avatars and decorations in lists play only while their own row
/// is hovered, like the official client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hover {
    pub user: UserId,
    pub message: Option<MessageId>,
    pub zone: Zone,
}

/// The list a hovered row belongs to: the same user can be in several at
/// once (member list and DM list), and only the hovered row animates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// A message row (`Hover::message` names it).
    Message,
    /// The member list (guild or group DM).
    Members,
    /// The DM list.
    DmList,
    /// The user panel at the bottom of the sidebar.
    Panel,
}

impl Hover {
    /// The row of message `message` by `user`.
    pub fn message(user: UserId, message: MessageId) -> Self {
        Self {
            user,
            message: Some(message),
            zone: Zone::Message,
        }
    }

    /// `user`'s row in a list.
    pub fn row(user: UserId, zone: Zone) -> Self {
        Self {
            user,
            message: None,
            zone,
        }
    }
}

#[derive(Debug)]
pub struct AnimState {
    pub cache: AnimCache,
    pub hover: Option<Hover>,
    hover_since: Instant,
    popout_since: Option<Instant>,
    /// Shared start of the self-playing animations (`ui::anim_image`), so
    /// they stay in step across view rebuilds.
    pub epoch: Instant,
}

impl Default for AnimState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            cache: AnimCache::default(),
            hover: None,
            hover_since: now,
            popout_since: None,
            epoch: now,
        }
    }
}

impl AnimState {
    /// Pointer entered `h`; the hover clock restarts on a new target.
    pub fn enter(&mut self, h: Hover, now: Instant) {
        if self.hover != Some(h) {
            self.hover_since = now;
        }
        self.hover = Some(h);
    }

    /// Pointer left `h`. Leaves are ignored when the pointer has already
    /// entered something else (the events of two rows arrive in either
    /// order).
    pub fn leave(&mut self, h: Hover) {
        if self.hover == Some(h) {
            self.hover = None;
        }
    }

    pub fn open_popout(&mut self, now: Instant) {
        self.popout_since = Some(now);
    }

    pub fn close_popout(&mut self) {
        self.popout_since = None;
        self.cache.drop_popout_only();
    }

    /// Start of the hovered target's animations.
    pub fn hover_epoch(&self) -> Instant {
        self.hover_since
    }

    /// Start of the open popout's animations.
    pub fn popout_epoch(&self) -> Option<Instant> {
        self.popout_since
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
    fn decimation_keeps_an_even_pace() {
        // 45 frames of 33 ms (a decoration) with room for 12: every kept
        // frame stands for the same number of source frames (the last may
        // be shorter), never a long first frame then a fast tail.
        let lim = Limits::new(100, 12, MB, 100);
        let a = decode(&gif(45, 4, 4, 33), &lim).unwrap();
        assert!(a.frames.len() <= 12);
        assert_eq!(a.total, Duration::from_millis(45 * 30));
        let first = a.frames[0].delay;
        for f in &a.frames[..a.frames.len() - 1] {
            assert_eq!(f.delay, first);
        }
        assert!(a.frames.last().unwrap().delay <= first);
    }

    #[test]
    fn round_mask_clears_the_corners() {
        let mut px = vec![255u8; 8 * 8 * 4];
        round_mask(8, 8, &mut px);
        assert_eq!(px[3], 0);
        assert_eq!(px[(4 * 8 + 4) * 4 + 3], 255);
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
        s.open_popout(Instant::now());
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
    fn hover_restarts_the_clock_only_on_a_new_target() {
        let mut s = AnimState::default();
        let t0 = Instant::now();
        let h = Hover::message(Snowflake(1), Snowflake(9));
        s.enter(h, t0);
        assert_eq!(s.hover_epoch(), t0);
        // Re-entering the same target keeps the phase.
        s.enter(h, t0 + Duration::from_millis(500));
        assert_eq!(s.hover_epoch(), t0);
        let h2 = Hover::row(Snowflake(2), Zone::Members);
        let t1 = t0 + Duration::from_millis(600);
        s.enter(h2, t1);
        assert_eq!(s.hover_epoch(), t1);
        assert!(s.is_hovering_user(Snowflake(2)) && !s.is_hovering_user(Snowflake(1)));
        assert!(!s.is_hovering_message(Snowflake(9)));
        // A late leave of another row does not cancel the hover.
        s.leave(h);
        assert!(s.is_hovering_user(Snowflake(2)));
        s.leave(h2);
        assert!(s.hover.is_none());
        // Popout clock.
        assert_eq!(s.popout_epoch(), None);
        s.open_popout(t1);
        assert_eq!(s.popout_epoch(), Some(t1));
        s.close_popout();
        assert_eq!(s.popout_epoch(), None);
    }

    #[test]
    fn delays_are_exact_and_only_instant_ones_are_slowed() {
        let ms = |n, d| delay_of(image::Delay::from_numer_denom_ms(n, d));
        // APNG 1/60 s: 16.67 ms, not truncated to 16 or clamped to 20.
        let d = ms(1000, 60);
        assert!(d > Duration::from_micros(16_660) && d < Duration::from_micros(16_670));
        assert_eq!(ms(15, 1), Duration::from_millis(15));
        assert_eq!(ms(33, 1), Duration::from_millis(33));
        // "As fast as possible" (<= 10 ms) plays at the browser default.
        assert_eq!(ms(10, 1), DEFAULT_DELAY);
        assert_eq!(ms(0, 1), DEFAULT_DELAY);
    }

    /// Frames of a `side` px square animation: `n` frames of `delay`.
    fn frames(n: usize, side: u32, delay: Duration) -> Vec<(u32, u32, Vec<u8>, Duration)> {
        (0..n)
            .map(|i| {
                let px = vec![(i % 256) as u8; (side * side * 4) as usize];
                (side, side, px, delay)
            })
            .collect()
    }

    #[test]
    fn typical_cosmetics_keep_every_frame_at_their_own_rate() {
        let sixty = Duration::from_nanos(1_000_000_000 / 60);
        // (class, frames, CDN size, delay): what Discord serves.
        let cases = [
            // A 3 s decoration APNG at 60 fps, list and popout sizes.
            (Class::DecorationList, 180, 96, sixty),
            (Class::Decoration, 180, 240, sixty),
            // A 2 s GIF avatar at 30 ms.
            (Class::AvatarList, 66, 80, Duration::from_millis(30)),
            (Class::Avatar, 66, 160, Duration::from_millis(30)),
            // A 2 s animated emoji at 20 ms.
            (Class::Emoji, 100, 96, Duration::from_millis(20)),
            // A 2 s APNG sticker at 30 fps.
            (
                Class::Sticker,
                60,
                320,
                Duration::from_nanos(1_000_000_000 / 30),
            ),
        ];
        for (class, n, side, delay) in cases {
            let a = from_frames_limited(frames(n, side, delay), &class.limits()).unwrap();
            assert_eq!(a.frames.len(), n, "{class:?} dropped frames");
            let fps = a.frames.len() as f64 / a.total.as_secs_f64();
            let want = 1.0 / delay.as_secs_f64();
            assert!(
                (fps - want).abs() < 0.5,
                "{class:?}: {fps:.1} fps, want {want:.1}"
            );
        }
    }

    #[test]
    fn oversized_assets_are_decimated_evenly_and_keep_their_duration() {
        // A 10 s, 60 fps decoration (600 frames) exceeds the list budget.
        let sixty = Duration::from_nanos(1_000_000_000 / 60);
        let lim = Class::DecorationList.limits();
        let a = from_frames_limited(frames(600, 96, sixty), &lim).unwrap();
        assert!(a.frames.len() <= lim.max_frames && a.bytes <= lim.max_bytes);
        assert!((a.total.as_secs_f64() - 10.0).abs() < 0.01);
    }
}
