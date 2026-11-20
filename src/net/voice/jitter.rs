//! Per-speaker jitter buffer: reorders RTP packets by sequence number and
//! releases one 20 ms frame per playout tick after a short pre-roll.
//! Bounded: at most `CAPACITY` frames are held per speaker.

use std::collections::BTreeMap;

/// Maximum frames held (1 s of audio).
pub const CAPACITY: usize = 50;
/// Frames buffered before playout starts (60 ms).
pub const PREROLL: usize = 3;
/// Consecutive missing frames before the stream counts as stopped.
const MAX_GAP: u16 = 10;

/// What the playout tick gets for one speaker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pop {
    Frame(Vec<u8>),
    /// A packet is missing in the middle of a stream: conceal it.
    Lost,
    /// Nothing to play (not started or stream ended).
    Empty,
}

#[derive(Debug, Default)]
pub struct JitterBuffer {
    /// Frames keyed by an unwrapped (extended) sequence number.
    frames: BTreeMap<u64, Vec<u8>>,
    /// Next extended sequence to play, once playing.
    next: Option<u64>,
    /// Highest extended sequence seen (for unwrapping).
    highest: Option<u64>,
    missing_run: u16,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    fn extend(&self, seq: u16) -> u64 {
        match self.highest {
            None => u64::from(seq) + (1 << 16),
            Some(h) => {
                let base = h & !0xffff;
                let cand = base | u64::from(seq);
                // Pick the candidate closest to `h` (handles wraparound).
                [cand.wrapping_sub(1 << 16), cand, cand + (1 << 16)]
                    .into_iter()
                    .min_by_key(|c| c.abs_diff(h))
                    .unwrap_or(cand)
            }
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn push(&mut self, seq: u16, frame: Vec<u8>) {
        let ext = self.extend(seq);
        if self.next.is_some_and(|n| ext < n) {
            return; // too late, already played or concealed
        }
        self.highest = Some(self.highest.map_or(ext, |h| h.max(ext)));
        self.frames.insert(ext, frame);
        while self.frames.len() > CAPACITY {
            // Drop the oldest; playout jumps ahead.
            if let Some((&k, _)) = self.frames.iter().next() {
                self.frames.remove(&k);
                if self.next.is_some_and(|n| n <= k) {
                    self.next = Some(k + 1);
                }
            }
        }
    }

    pub fn pop(&mut self) -> Pop {
        let Some(next) = self.next else {
            if self.frames.len() < PREROLL {
                return Pop::Empty;
            }
            let Some((&first, _)) = self.frames.iter().next() else {
                return Pop::Empty;
            };
            self.next = Some(first);
            return self.pop();
        };
        if let Some(f) = self.frames.remove(&next) {
            self.next = Some(next + 1);
            self.missing_run = 0;
            return Pop::Frame(f);
        }
        if self.frames.is_empty() {
            // Talk spurt ended: re-arm the pre-roll for the next one.
            self.next = None;
            self.missing_run = 0;
            return Pop::Empty;
        }
        self.missing_run += 1;
        if self.missing_run > MAX_GAP {
            // Long hole: skip to the oldest frame we have.
            self.next = self.frames.keys().next().copied();
            self.missing_run = 0;
            return self.pop();
        }
        self.next = Some(next + 1);
        Pop::Lost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(n: u8) -> Vec<u8> {
        vec![n]
    }

    #[test]
    fn reorders_and_waits_for_preroll() {
        let mut j = JitterBuffer::new();
        j.push(2, f(2));
        assert_eq!(j.pop(), Pop::Empty);
        j.push(1, f(1));
        assert_eq!(j.pop(), Pop::Empty);
        j.push(3, f(3));
        assert_eq!(j.pop(), Pop::Frame(f(1)));
        assert_eq!(j.pop(), Pop::Frame(f(2)));
        assert_eq!(j.pop(), Pop::Frame(f(3)));
        assert_eq!(j.pop(), Pop::Empty);
        // A late duplicate of a played frame is ignored... after re-arm
        // it is just a new spurt start, which is fine.
    }

    #[test]
    fn missing_frames_are_concealed() {
        let mut j = JitterBuffer::new();
        for s in [10u16, 11, 13, 14] {
            j.push(s, f(s as u8));
        }
        assert_eq!(j.pop(), Pop::Frame(f(10)));
        assert_eq!(j.pop(), Pop::Frame(f(11)));
        assert_eq!(j.pop(), Pop::Lost);
        assert_eq!(j.pop(), Pop::Frame(f(13)));
        // A late 12 is dropped.
        j.push(12, f(12));
        assert_eq!(j.pop(), Pop::Frame(f(14)));
    }

    #[test]
    fn sequence_wraparound() {
        let mut j = JitterBuffer::new();
        for s in [65534u16, 65535, 0, 1] {
            j.push(s, f(s as u8));
        }
        let got: Vec<Pop> = (0..4).map(|_| j.pop()).collect();
        assert_eq!(
            got,
            vec![
                Pop::Frame(f(254)),
                Pop::Frame(f(255)),
                Pop::Frame(f(0)),
                Pop::Frame(f(1))
            ]
        );
    }

    #[test]
    fn bounded_under_flood() {
        let mut j = JitterBuffer::new();
        for s in 0..10_000u32 {
            j.push(s as u16, f(0));
            assert!(j.len() <= CAPACITY);
        }
        // Random sequence numbers never panic and stay bounded.
        let mut seed = 7u32;
        for _ in 0..10_000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            j.push(seed as u16, f(1));
            let _ = j.pop();
            assert!(j.len() <= CAPACITY);
        }
    }

    #[test]
    fn long_gap_skips_ahead() {
        let mut j = JitterBuffer::new();
        for s in 0..3u16 {
            j.push(s, f(s as u8));
        }
        for _ in 0..3 {
            assert!(matches!(j.pop(), Pop::Frame(_)));
        }
        for s in 40..43u16 {
            j.push(s, f(s as u8));
        }
        let mut lost = 0;
        loop {
            match j.pop() {
                Pop::Lost => lost += 1,
                Pop::Frame(x) => {
                    assert_eq!(x, f(40));
                    break;
                }
                Pop::Empty => panic!("unexpected empty"),
            }
        }
        assert!(lost <= MAX_GAP as usize);
    }
}
