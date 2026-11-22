//! Bounded image cache (avatars, guild icons, attachment previews).
//!
//! Encoded bytes are handed to iced, which decodes lazily on first draw.
//! The cache is an LRU keyed by URL with a byte budget.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use iced::widget::image::Handle;

/// Default byte budget (architecture §3).
pub const DEFAULT_BUDGET: usize = 48 * 1024 * 1024;
/// A failed download is retried after this long.
pub const RETRY_AFTER: Duration = Duration::from_secs(300);
/// At most this many failures are remembered.
pub const MAX_FAILED: usize = 512;

#[derive(Debug)]
pub struct ImageCache {
    entries: HashMap<String, (Handle, usize)>,
    order: VecDeque<String>,
    pending: HashSet<String>,
    failed: HashMap<String, Instant>,
    bytes: usize,
    budget: usize,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new(DEFAULT_BUDGET)
    }
}

impl ImageCache {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
            bytes: 0,
            budget,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Handle> {
        self.entries.get(key).map(|(h, _)| h)
    }

    /// Returns true if `key` should be fetched now (not cached, not in
    /// flight, not failed). Marks it pending.
    pub fn want(&mut self, key: &str) -> bool {
        self.want_at(key, Instant::now())
    }

    pub fn want_at(&mut self, key: &str, now: Instant) -> bool {
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

    pub fn insert(&mut self, key: String, handle: Handle, size: usize) {
        self.pending.remove(&key);
        if let Some((_, old)) = self.entries.remove(&key) {
            self.bytes -= old;
            self.order.retain(|k| k != &key);
        }
        self.bytes += size;
        self.order.push_back(key.clone());
        self.entries.insert(key, (handle, size));
        while self.bytes > self.budget && self.order.len() > 1 {
            if let Some(k) = self.order.pop_front()
                && let Some((_, s)) = self.entries.remove(&k)
            {
                self.bytes -= s;
            }
        }
    }

    pub fn insert_bytes(&mut self, key: String, bytes: Vec<u8>) {
        if is_avatar_url(&key)
            && let Some((w, h, px)) = round_avatar(&bytes)
        {
            let size = px.len();
            self.insert(key, Handle::from_rgba(w, h, px), size);
            return;
        }
        let size = bytes.len();
        self.insert(key, Handle::from_bytes(bytes), size);
    }

    pub fn mark_failed(&mut self, key: &str) {
        self.mark_failed_at(key, Instant::now());
    }

    pub fn mark_failed_at(&mut self, key: &str, now: Instant) {
        self.pending.remove(key);
        if self.failed.len() >= MAX_FAILED {
            self.failed
                .retain(|_, at| now.saturating_duration_since(*at) < RETRY_AFTER);
            if self.failed.len() >= MAX_FAILED {
                self.failed.clear();
            }
        }
        self.failed.insert(key.to_owned(), now);
    }

    /// Marks `key` most recently used.
    pub fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key)
            && let Some(k) = self.order.remove(pos)
        {
            self.order.push_back(k);
        }
    }

    /// Downloads in flight.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
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

/// User avatars (`/avatars/…`, also guild-member and default ones).
pub fn is_avatar_url(url: &str) -> bool {
    url.contains("/avatars/")
}

/// Decodes an avatar and cuts it to a circle. tiny-skia ignores the image
/// border radius, so the round shape has to be in the pixels.
fn round_avatar(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    if w > 1024 || h > 1024 {
        return None;
    }
    let mut px = img.into_raw();
    super::anim::round_mask(w, h, &mut px);
    Some((w, h, px))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_set_is_bounded() {
        let mut c = ImageCache::new(100);
        let t0 = Instant::now();
        for i in 0..(MAX_FAILED * 3) {
            c.mark_failed_at(&format!("k{i}"), t0);
        }
        assert!(c.failed.len() <= MAX_FAILED);
    }

    #[test]
    fn evicts_least_recently_used_over_budget() {
        let mut c = ImageCache::new(100);
        c.insert_bytes("a".into(), vec![0; 40]);
        c.insert_bytes("b".into(), vec![0; 40]);
        c.touch("a");
        c.insert_bytes("c".into(), vec![0; 40]);
        assert!(c.get("a").is_some());
        assert!(c.get("b").is_none());
        assert!(c.get("c").is_some());
        assert!(c.bytes() <= 100);
    }

    #[test]
    fn want_dedupes_requests() {
        let mut c = ImageCache::new(100);
        assert!(c.want("x"));
        assert!(!c.want("x"));
        let t0 = Instant::now();
        c.mark_failed_at("x", t0);
        assert!(!c.want_at("x", t0));
        // Retried later.
        assert!(c.want_at("x", t0 + RETRY_AFTER + Duration::from_secs(1)));
        assert!(c.want("y"));
        c.insert_bytes("y".into(), vec![1]);
        assert!(!c.want("y"));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn avatars_are_cut_round() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(16, 16, image::Rgba([200, 10, 10, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (w, h, px) = round_avatar(&png).unwrap();
        assert_eq!((w, h), (16, 16));
        // Corner transparent, centre opaque.
        assert_eq!(px[3], 0);
        assert_eq!(px[((8 * 16 + 8) * 4 + 3) as usize], 255);
        assert!(is_avatar_url(
            "https://cdn.discordapp.com/avatars/1/abc.png?size=80"
        ));
        assert!(!is_avatar_url("https://cdn.discordapp.com/icons/1/abc.png"));
    }
}
