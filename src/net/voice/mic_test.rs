//! "Let's Check" microphone test in Voice & Video settings: opens the
//! chosen devices outside a call, feeds the level meter and plays the
//! microphone back (with the input and output volumes applied), like the
//! official client. Runs on its own thread; dropping [`MicTest`] stops it
//! and closes the devices.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::VoiceControl;
use super::audio::{AudioFactory, DeviceChoice};
use super::mixer::{FLOOR_DB, FRAME_LEN, apply_gain, finish, level_db};

/// A running microphone test.
pub struct MicTest {
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for MicTest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MicTest")
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One step: capture a frame, report its level, play it back. False when
/// no full frame was ready.
pub fn step(io: &mut dyn super::audio::AudioIo, control: &VoiceControl, frame: &mut [f32]) -> bool {
    if !io.capture(frame) {
        return false;
    }
    apply_gain(frame, control.input_gain());
    control.set_input_level(level_db(frame));
    apply_gain(frame, control.output_gain());
    finish(frame);
    io.play(frame);
    true
}

impl MicTest {
    /// Opens the devices (blocking for a moment; call off the UI thread is
    /// not required: opening happens on the test thread) and starts the
    /// loop. The meter reads `control.input_level()`.
    pub fn start(
        factory: AudioFactory,
        devices: DeviceChoice,
        control: VoiceControl,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::Builder::new()
            .name("ferrocord-mic-test".into())
            .spawn(move || {
                let mut io = match factory(&devices) {
                    Ok(io) => io,
                    Err(e) => {
                        tracing::warn!(error = %e, "microphone test could not open audio");
                        return;
                    }
                };
                let mut frame = vec![0.0f32; FRAME_LEN];
                while !flag.load(Ordering::Relaxed) {
                    if !step(io.as_mut(), &control, &mut frame) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                control.set_input_level(FLOOR_DB);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { stop })
    }
}

#[cfg(test)]
mod tests {
    use super::super::audio::{AudioIo, FakeIo};
    use super::*;

    #[test]
    fn step_meters_and_plays_back_with_volumes() {
        let control = VoiceControl::new();
        control.set_volumes(100.0, 50.0);
        let (mut io, played) = FakeIo::new(440.0, 0.5);
        let mut frame = vec![0.0; FRAME_LEN];
        assert!(step(&mut io, &control, &mut frame));
        assert!((control.input_level() + 9.0).abs() < 0.5);
        // Played back at half volume.
        let peak = played
            .lock()
            .unwrap()
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.25).abs() < 0.01, "{peak}");
    }

    #[test]
    fn test_thread_stops_on_drop() {
        let control = VoiceControl::new();
        let opened = Arc::new(AtomicBool::new(false));
        let o = opened.clone();
        let factory: AudioFactory = Arc::new(move |_| {
            o.store(true, Ordering::Relaxed);
            let (io, _) = FakeIo::new(440.0, 0.5);
            Ok(Box::new(io) as Box<dyn AudioIo>)
        });
        let t = MicTest::start(factory, DeviceChoice::default(), control.clone()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while control.input_level() <= FLOOR_DB && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(opened.load(Ordering::Relaxed));
        assert!(control.input_level() > FLOOR_DB);
        drop(t);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while control.input_level() > FLOOR_DB && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(control.input_level(), FLOOR_DB);
    }
}
