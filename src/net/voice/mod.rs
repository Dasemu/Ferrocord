//! Discord voice: voice gateway v8, UDP/RTP media with transport
//! encryption, DAVE end-to-end encryption, Opus, jitter buffering and
//! mixing. Nothing here runs or allocates until a call connects: the app
//! starts [`conn::spawn`] as a subscription only while it is in a voice
//! channel or call.

pub mod audio;
pub mod conn;
pub mod dave;
pub mod jitter;
pub mod media;
pub mod mic_test;
pub mod mixer;
pub mod rtp;
pub mod session;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Events from a voice connection to the app.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceEvent {
    /// Media is flowing (`e2ee`: DAVE active).
    Connected { e2ee: bool },
    /// Voice gateway round trip in ms.
    Ping(u32),
    /// A user (possibly ourselves) started or stopped speaking.
    Speaking { user: u64, speaking: bool },
    /// DAVE state changed (privacy code available when E2EE).
    Encryption { e2ee: bool },
    /// Reconnecting to the voice server (resume).
    Reconnecting,
    /// The connection ended. `rejoin`: the session is gone and the app
    /// should ask the main gateway for a new voice server.
    Ended { reason: String, rejoin: bool },
}

/// Settings shared between the app and the running engine (lock-free
/// atomics on the audio path; the per-user map changes rarely).
#[derive(Clone, Default)]
pub struct VoiceControl {
    inner: Arc<ControlInner>,
}

#[derive(Default)]
struct ControlInner {
    self_mute: AtomicBool,
    self_deaf: AtomicBool,
    /// Voice activity threshold in dB * 100.
    threshold: AtomicI32,
    /// Last microphone level in dB * 100 (settings meter).
    input_level: AtomicI32,
    /// "Input Volume" / "Output Volume" in percent (0..=200; 0 = unset,
    /// read as 100).
    input_volume: AtomicI32,
    output_volume: AtomicI32,
    /// user -> (gain, locally muted).
    users: Mutex<HashMap<u64, (f32, bool)>>,
    /// Bumped when the device choice changes: the engine reopens audio.
    device_generation: AtomicU64,
    devices: Mutex<audio::DeviceChoice>,
}

impl std::fmt::Debug for VoiceControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VoiceControl")
    }
}

impl VoiceControl {
    pub fn new() -> Self {
        let c = Self::default();
        c.set_threshold_db(-60.0);
        c.set_input_level(mixer::FLOOR_DB);
        c
    }

    pub fn set_self_mute(&self, v: bool) {
        self.inner.self_mute.store(v, Ordering::Relaxed);
    }
    pub fn self_mute(&self) -> bool {
        self.inner.self_mute.load(Ordering::Relaxed)
    }
    pub fn set_self_deaf(&self, v: bool) {
        self.inner.self_deaf.store(v, Ordering::Relaxed);
    }
    pub fn self_deaf(&self) -> bool {
        self.inner.self_deaf.load(Ordering::Relaxed)
    }
    pub fn set_threshold_db(&self, db: f32) {
        let v = (db.clamp(mixer::FLOOR_DB, 0.0) * 100.0) as i32;
        self.inner.threshold.store(v, Ordering::Relaxed);
    }
    pub fn threshold_db(&self) -> f32 {
        self.inner.threshold.load(Ordering::Relaxed) as f32 / 100.0
    }
    pub fn set_input_level(&self, db: f32) {
        self.inner
            .input_level
            .store((db * 100.0) as i32, Ordering::Relaxed);
    }
    pub fn input_level(&self) -> f32 {
        self.inner.input_level.load(Ordering::Relaxed) as f32 / 100.0
    }
    /// Microphone and speaker volume in percent (0..=200).
    pub fn set_volumes(&self, input: f32, output: f32) {
        let pct = |v: f32| (v.clamp(0.0, 200.0).round() as i32).max(0) + 1;
        self.inner.input_volume.store(pct(input), Ordering::Relaxed);
        self.inner
            .output_volume
            .store(pct(output), Ordering::Relaxed);
    }
    fn volume(v: &AtomicI32) -> f32 {
        match v.load(Ordering::Relaxed) {
            0 => 1.0,
            n => (n - 1) as f32 / 100.0,
        }
    }
    /// Gain applied to the microphone before voice activity and encoding.
    pub fn input_gain(&self) -> f32 {
        Self::volume(&self.inner.input_volume)
    }
    /// Gain applied to everything played.
    pub fn output_gain(&self) -> f32 {
        Self::volume(&self.inner.output_volume)
    }
    /// Per-user volume (0.0..=2.0) and local mute.
    pub fn set_user(&self, user: u64, volume: f32, muted: bool) {
        if let Ok(mut m) = self.inner.users.lock() {
            if (volume - 1.0).abs() < f32::EPSILON && !muted {
                m.remove(&user);
            } else if m.len() < 10_000 || m.contains_key(&user) {
                m.insert(user, (volume.clamp(0.0, mixer::MAX_USER_VOLUME), muted));
            }
        }
    }
    /// Effective gain for a user's audio.
    pub fn gain(&self, user: u64) -> f32 {
        match self
            .inner
            .users
            .lock()
            .ok()
            .and_then(|m| m.get(&user).copied())
        {
            Some((_, true)) => 0.0,
            Some((v, false)) => v,
            None => 1.0,
        }
    }
    pub fn set_devices(&self, d: audio::DeviceChoice) {
        if let Ok(mut cur) = self.inner.devices.lock()
            && *cur != d
        {
            *cur = d;
            self.inner.device_generation.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn devices(&self) -> audio::DeviceChoice {
        self.inner
            .devices
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }
    pub fn device_generation(&self) -> u64 {
        self.inner.device_generation.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_gains_and_threshold() {
        let c = VoiceControl::new();
        assert_eq!(c.threshold_db(), -60.0);
        c.set_threshold_db(-500.0);
        assert_eq!(c.threshold_db(), mixer::FLOOR_DB);
        assert_eq!(c.gain(5), 1.0);
        c.set_user(5, 0.5, false);
        assert_eq!(c.gain(5), 0.5);
        c.set_user(5, 0.5, true);
        assert_eq!(c.gain(5), 0.0);
        c.set_user(5, 9.0, false);
        assert_eq!(c.gain(5), 2.0);
        c.set_user(5, 1.0, false);
        assert_eq!(c.gain(5), 1.0);
        let g = c.device_generation();
        c.set_devices(audio::DeviceChoice {
            input: Some("USB".into()),
            output: None,
        });
        assert_eq!(c.device_generation(), g + 1);
        c.set_devices(c.devices());
        assert_eq!(c.device_generation(), g + 1);
    }
}
