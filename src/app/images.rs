//! Bounded image cache (avatars, guild icons, attachment previews).
//!
//! Encoded bytes are handed to iced, which decodes lazily on first draw.
//! The cache is an LRU keyed by URL with a byte budget.

use std::collections::{HashMap, HashSet, VecDeque};

use iced::widget::image::Handle;

/// Default byte budget (architecture §3).
pub const DEFAULT_BUDGET: usize = 48 * 1024 * 1024;

#[derive(Debug)]
pub struct ImageCache {
    entries: HashMap<String, (Handle, usize)>,
    order: VecDeque<String>,
    pending: HashSet<String>,
    failed: HashSet<String>,
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
            failed: HashSet::new(),
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
        if self.entries.contains_key(key) || self.pending.contains(key) || self.failed.contains(key)
        {
            return false;
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
        let size = bytes.len();
        self.insert(key, Handle::from_bytes(bytes), size);
    }

    pub fn mark_failed(&mut self, key: &str) {
        self.pending.remove(key);
        self.failed.insert(key.to_owned());
    }

    /// Marks `key` most recently used.
    pub fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key)
            && let Some(k) = self.order.remove(pos)
        {
            self.order.push_back(k);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        c.mark_failed("x");
        assert!(!c.want("x"));
        assert!(c.want("y"));
        c.insert_bytes("y".into(), vec![1]);
        assert!(!c.want("y"));
        assert_eq!(c.len(), 1);
    }
}
