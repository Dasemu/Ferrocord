//! Audio helpers: mixing with per-user gain, levels, voice activity
//! detection and format conversion between the device and Opus
//! (48 kHz, stereo, 20 ms frames).

/// Opus sample rate.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 960;
/// Interleaved stereo samples in one frame.
pub const FRAME_LEN: usize = FRAME_SAMPLES * 2;
/// Lowest level reported (digital silence).
pub const FLOOR_DB: f32 = -100.0;
/// Highest per-user volume (the official client goes to 200 %).
pub const MAX_USER_VOLUME: f32 = 2.0;

/// Adds `src * gain` into `acc` (same length; extra samples ignored).
pub fn mix_into(acc: &mut [f32], src: &[f32], gain: f32) {
    for (a, s) in acc.iter_mut().zip(src) {
        *a += s * gain;
    }
}

/// Soft-clips the mix into [-1, 1].
pub fn finish(acc: &mut [f32]) {
    for s in acc.iter_mut() {
        *s = if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
}

/// RMS level in dBFS.
pub fn level_db(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return FLOOR_DB;
    }
    let sum: f64 = frame
        .iter()
        .filter(|s| s.is_finite())
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum();
    let rms = (sum / frame.len() as f64).sqrt();
    if rms <= 1e-5 {
        FLOOR_DB
    } else {
        ((20.0 * rms.log10()) as f32).max(FLOOR_DB)
    }
}

/// Voice activity detection by level with a hangover, like the official
/// "Input Sensitivity" slider (threshold in dB, -100..0).
#[derive(Debug, Clone)]
pub struct Vad {
    hang: u32,
    active: bool,
}

/// Frames the detector stays open after the level drops (300 ms).
const HANGOVER_FRAMES: u32 = 15;

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

impl Vad {
    pub fn new() -> Self {
        Self {
            hang: 0,
            active: false,
        }
    }

    pub fn update(&mut self, level_db: f32, threshold_db: f32) -> bool {
        if level_db >= threshold_db {
            self.hang = HANGOVER_FRAMES;
            self.active = true;
        } else if self.hang > 0 {
            self.hang -= 1;
        } else {
            self.active = false;
        }
        self.active
    }

    pub fn reset(&mut self) {
        self.hang = 0;
        self.active = false;
    }
}

/// Converts interleaved audio with `channels` channels to stereo.
pub fn to_stereo(input: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => input.iter().flat_map(|s| [*s, *s]).collect(),
        2 => input.to_vec(),
        n => input.chunks_exact(n).flat_map(|c| [c[0], c[1]]).collect(),
    }
}

/// Converts interleaved stereo to `channels` channels.
pub fn from_stereo(input: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => input.chunks_exact(2).map(|c| (c[0] + c[1]) * 0.5).collect(),
        2 => input.to_vec(),
        n => input
            .chunks_exact(2)
            .flat_map(|c| {
                let mut v = vec![0.0; n];
                v[0] = c[0];
                v[1] = c[1];
                v
            })
            .collect(),
    }
}

/// Streaming linear resampler for interleaved stereo.
#[derive(Debug, Clone)]
pub struct Resampler {
    from: u32,
    to: u32,
    /// Fractional read position into the pending input.
    pos: f64,
    last: [f32; 2],
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            from: from.max(1),
            to: to.max(1),
            pos: 0.0,
            last: [0.0; 2],
        }
    }

    pub fn is_identity(&self) -> bool {
        self.from == self.to
    }

    /// Resamples a block of interleaved stereo.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.is_identity() {
            return input.to_vec();
        }
        let frames = input.len() / 2;
        if frames == 0 {
            return Vec::new();
        }
        let step = f64::from(self.from) / f64::from(self.to);
        let mut out = Vec::with_capacity((frames as f64 / step) as usize * 2 + 4);
        let at = |i: isize, ch: usize, last: &[f32; 2]| -> f32 {
            if i < 0 {
                last[ch]
            } else {
                input.get(i as usize * 2 + ch).copied().unwrap_or(0.0)
            }
        };
        // Position is relative to `last` at index -1.
        while self.pos < frames as f64 {
            let i = self.pos.floor() as isize - 1;
            let t = (self.pos - self.pos.floor()) as f32;
            for ch in 0..2 {
                let a = at(i, ch, &self.last);
                let b = at(i + 1, ch, &self.last);
                out.push(a + (b - a) * t);
            }
            self.pos += step;
        }
        self.pos -= frames as f64;
        self.last = [input[(frames - 1) * 2], input[(frames - 1) * 2 + 1]];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let v = amp * (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn mixing_with_gain_and_clipping() {
        let mut acc = vec![0.0; 4];
        mix_into(&mut acc, &[0.5, 0.5, -0.5, 1.0], 1.0);
        mix_into(&mut acc, &[0.5, 0.25, -0.75, f32::NAN], 2.0);
        finish(&mut acc);
        assert_eq!(acc, vec![1.0, 1.0, -1.0, 0.0]);
        // Muted user (gain 0) contributes nothing.
        let mut acc = vec![0.1; 2];
        mix_into(&mut acc, &[1.0, 1.0], 0.0);
        assert_eq!(acc, vec![0.1, 0.1]);
    }

    #[test]
    fn levels() {
        assert_eq!(level_db(&[0.0; 100]), FLOOR_DB);
        assert_eq!(level_db(&[]), FLOOR_DB);
        let full = level_db(&[1.0; 100]);
        assert!(full.abs() < 0.01);
        let s = level_db(&sine(440.0, 0.5, 960));
        assert!((s - (-9.03)).abs() < 0.2, "{s}");
    }

    #[test]
    fn vad_opens_and_hangs_over() {
        let mut v = Vad::new();
        assert!(!v.update(-60.0, -40.0));
        assert!(v.update(-20.0, -40.0));
        for _ in 0..HANGOVER_FRAMES {
            assert!(v.update(-80.0, -40.0));
        }
        assert!(!v.update(-80.0, -40.0));
    }

    #[test]
    fn channel_conversion() {
        assert_eq!(to_stereo(&[0.1, 0.2], 1), vec![0.1, 0.1, 0.2, 0.2]);
        assert_eq!(
            to_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3),
            vec![1.0, 2.0, 4.0, 5.0]
        );
        assert_eq!(from_stereo(&[0.2, 0.4], 1), vec![0.3 as f32]);
        assert_eq!(from_stereo(&[1.0, 2.0], 4), vec![1.0, 2.0, 0.0, 0.0]);
        assert!(to_stereo(&[1.0], 0).is_empty());
    }

    #[test]
    fn resampler_rates_and_continuity() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut total = 0;
        for _ in 0..100 {
            total += r.process(&vec![0.25; 441 * 2]).len() / 2;
        }
        // 1 s of 44.1 kHz becomes ~1 s of 48 kHz.
        assert!((total as i64 - 48_000).abs() <= 2, "{total}");
        let mut r = Resampler::new(48_000, 48_000);
        assert_eq!(r.process(&[1.0, 2.0]), vec![1.0, 2.0]);
        // A constant signal stays constant through the interpolation.
        let mut r = Resampler::new(48_000, 16_000);
        let out = r.process(&vec![0.5; 960 * 2]);
        assert!(out.iter().skip(2).all(|s| (s - 0.5).abs() < 1e-6));
        assert!((out.len() / 2) as i64 - 320 <= 1);
    }
}
