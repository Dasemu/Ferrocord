//! Audio devices. The media engine talks to an [`AudioIo`]: 20 ms frames
//! of 48 kHz interleaved stereo in and out. [`CpalIo`] opens the real
//! microphone and speakers with `cpal` on a dedicated thread (cpal streams
//! are not `Send`), only while a call is connected; tests use a fake.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};

use super::mixer::{FRAME_LEN, Resampler, SAMPLE_RATE, from_stereo, to_stereo};

/// Microphone in, speakers out, one 20 ms frame at a time.
pub trait AudioIo: Send {
    /// Fills `out` (`FRAME_LEN` samples) with captured audio; false when a
    /// full frame is not available yet.
    fn capture(&mut self, out: &mut [f32]) -> bool;
    /// Queues one frame for playback.
    fn play(&mut self, frame: &[f32]);
}

/// Opens audio for a connection (injectable for tests).
pub type AudioFactory =
    Arc<dyn Fn(&DeviceChoice) -> Result<Box<dyn AudioIo>, String> + Send + Sync>;

/// Device names picked in settings; `None` = system default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DeviceChoice {
    pub input: Option<String>,
    pub output: Option<String>,
}

/// Lists input and output device names (blocking; call off the UI thread).
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let names = |it: Option<Vec<cpal::Device>>| -> Vec<String> {
        let mut v: Vec<String> = it
            .unwrap_or_default()
            .iter()
            .filter_map(|d| d.name().ok())
            .take(64)
            .collect();
        v.dedup();
        v
    };
    (
        names(host.input_devices().ok().map(Iterator::collect)),
        names(host.output_devices().ok().map(Iterator::collect)),
    )
}

/// Bounded sample ring shared with the device callbacks.
type Ring = Arc<Mutex<VecDeque<f32>>>;
/// Capture ring cap: 200 ms; playback ring cap: 200 ms.
const RING_CAP: usize = FRAME_LEN * 10;

fn push_bounded(ring: &Ring, samples: &[f32]) {
    if let Ok(mut r) = ring.lock() {
        r.extend(samples.iter().copied());
        let over = r.len().saturating_sub(RING_CAP);
        r.drain(..over);
    }
}

/// Real audio through cpal.
pub struct CpalIo {
    input: Ring,
    output: Ring,
    /// Dropping this stops the device thread (and closes the streams).
    _stop: std::sync::mpsc::Sender<()>,
}

impl CpalIo {
    pub fn open(choice: &DeviceChoice) -> Result<Box<dyn AudioIo>, String> {
        let input: Ring = Arc::default();
        let output: Ring = Arc::default();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (inp, out, choice) = (input.clone(), output.clone(), choice.clone());
        std::thread::Builder::new()
            .name("ferrocord-audio".into())
            .spawn(move || {
                let streams = open_streams(&choice, inp, out);
                match streams {
                    Ok(s) => {
                        let _ = ready_tx.send(Ok(()));
                        // Park until the engine drops us.
                        let _ = stop_rx.recv();
                        drop(s);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx
            .recv()
            .map_err(|_| "audio thread failed".to_owned())??;
        Ok(Box::new(Self {
            input,
            output,
            _stop: stop_tx,
        }))
    }
}

impl AudioIo for CpalIo {
    fn capture(&mut self, out: &mut [f32]) -> bool {
        let Ok(mut r) = self.input.lock() else {
            return false;
        };
        if r.len() < out.len() {
            return false;
        }
        let n = out.len();
        for (o, s) in out.iter_mut().zip(r.drain(..n)) {
            *o = s;
        }
        true
    }

    fn play(&mut self, frame: &[f32]) {
        push_bounded(&self.output, frame);
    }
}

fn find(devices: Option<impl Iterator<Item = cpal::Device>>, name: &str) -> Option<cpal::Device> {
    devices?.find(|d| d.name().ok().as_deref() == Some(name))
}

/// Prefers 48 kHz (no resampling); otherwise the device default.
fn pick_config(
    supported: Vec<cpal::SupportedStreamConfigRange>,
    default: cpal::SupportedStreamConfig,
) -> cpal::SupportedStreamConfig {
    let rate = cpal::SampleRate(SAMPLE_RATE);
    supported
        .into_iter()
        .filter(|c| c.min_sample_rate() <= rate && rate <= c.max_sample_rate())
        .filter(|c| c.channels() <= 2 && c.sample_format() == default.sample_format())
        .max_by_key(|c| c.channels())
        .map(|c| c.with_sample_rate(rate))
        .unwrap_or(default)
}

fn open_streams(
    choice: &DeviceChoice,
    input: Ring,
    output: Ring,
) -> Result<Vec<cpal::Stream>, String> {
    let host = cpal::default_host();
    let in_dev = match &choice.input {
        Some(n) => find(host.input_devices().ok(), n).or_else(|| host.default_input_device()),
        None => host.default_input_device(),
    };
    let out_dev = match &choice.output {
        Some(n) => find(host.output_devices().ok(), n).or_else(|| host.default_output_device()),
        None => host.default_output_device(),
    };
    let mut streams = Vec::new();
    // A missing microphone still lets the user listen (like the official
    // client); a missing output device is an error.
    if let Some(dev) = in_dev {
        match build_input(&dev, input) {
            Ok(s) => streams.push(s),
            Err(e) => tracing::warn!(error = %e, "microphone unavailable"),
        }
    }
    let dev = out_dev.ok_or_else(|| "No audio output device".to_owned())?;
    streams.push(build_output(&dev, output)?);
    for s in &streams {
        s.play().map_err(|e| e.to_string())?;
    }
    Ok(streams)
}

fn build_input(dev: &cpal::Device, ring: Ring) -> Result<cpal::Stream, String> {
    let default = dev.default_input_config().map_err(|e| e.to_string())?;
    let supported = dev
        .supported_input_configs()
        .map(Iterator::collect)
        .unwrap_or_default();
    let cfg = pick_config(supported, default);
    let format = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    match format {
        cpal::SampleFormat::F32 => input_stream::<f32>(dev, &config, ring),
        cpal::SampleFormat::I16 => input_stream::<i16>(dev, &config, ring),
        cpal::SampleFormat::U16 => input_stream::<u16>(dev, &config, ring),
        cpal::SampleFormat::I32 => input_stream::<i32>(dev, &config, ring),
        f => Err(format!("unsupported input sample format {f:?}")),
    }
}

fn input_stream<T>(
    dev: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Ring,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let mut rs = Resampler::new(config.sample_rate.0, SAMPLE_RATE);
    dev.build_input_stream(
        config,
        move |data: &[T], _| {
            let f: Vec<f32> = data.iter().map(|s| f32::from_sample(*s)).collect();
            let stereo = to_stereo(&f, channels);
            push_bounded(&ring, &rs.process(&stereo));
        },
        |e| tracing::warn!(error = %e, "microphone stream error"),
        None,
    )
    .map_err(|e| e.to_string())
}

fn build_output(dev: &cpal::Device, ring: Ring) -> Result<cpal::Stream, String> {
    let default = dev.default_output_config().map_err(|e| e.to_string())?;
    let supported = dev
        .supported_output_configs()
        .map(Iterator::collect)
        .unwrap_or_default();
    let cfg = pick_config(supported, default);
    let format = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    match format {
        cpal::SampleFormat::F32 => output_stream::<f32>(dev, &config, ring),
        cpal::SampleFormat::I16 => output_stream::<i16>(dev, &config, ring),
        cpal::SampleFormat::U16 => output_stream::<u16>(dev, &config, ring),
        cpal::SampleFormat::I32 => output_stream::<i32>(dev, &config, ring),
        f => Err(format!("unsupported output sample format {f:?}")),
    }
}

fn output_stream<T>(
    dev: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Ring,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let mut rs = Resampler::new(SAMPLE_RATE, config.sample_rate.0);
    let mut pending: VecDeque<f32> = VecDeque::new();
    dev.build_output_stream(
        config,
        move |data: &mut [T], _| {
            while pending.len() < data.len() {
                let mut frame = vec![0.0f32; FRAME_LEN];
                let got = match ring.lock() {
                    Ok(mut r) if r.len() >= FRAME_LEN => {
                        for (o, s) in frame.iter_mut().zip(r.drain(..FRAME_LEN)) {
                            *o = s;
                        }
                        true
                    }
                    _ => false,
                };
                if !got && !pending.is_empty() {
                    break;
                }
                pending.extend(from_stereo(&rs.process(&frame), channels));
                if !got {
                    break;
                }
            }
            for d in data.iter_mut() {
                *d = T::from_sample(pending.pop_front().unwrap_or(0.0));
            }
        },
        |e| tracing::warn!(error = %e, "speaker stream error"),
        None,
    )
    .map_err(|e| e.to_string())
}

/// No devices: captures nothing, discards playback.
pub struct NullIo;

impl AudioIo for NullIo {
    fn capture(&mut self, _out: &mut [f32]) -> bool {
        false
    }
    fn play(&mut self, _frame: &[f32]) {}
}

/// The real factory used by the app.
pub fn cpal_factory() -> AudioFactory {
    Arc::new(CpalIo::open)
}

/// Fake audio for tests and the in-process loopback: a sine wave as the
/// microphone, playback recorded for inspection.
pub struct FakeIo {
    pub freq: f32,
    pub amplitude: f32,
    phase: u64,
    pub played: Arc<Mutex<Vec<f32>>>,
}

impl FakeIo {
    pub fn new(freq: f32, amplitude: f32) -> (Self, Arc<Mutex<Vec<f32>>>) {
        let played: Arc<Mutex<Vec<f32>>> = Arc::default();
        (
            Self {
                freq,
                amplitude,
                phase: 0,
                played: played.clone(),
            },
            played,
        )
    }
}

impl AudioIo for FakeIo {
    fn capture(&mut self, out: &mut [f32]) -> bool {
        for pair in out.chunks_exact_mut(2) {
            let t = self.phase as f32 / SAMPLE_RATE as f32;
            let v = self.amplitude * (2.0 * std::f32::consts::PI * self.freq * t).sin();
            pair[0] = v;
            pair[1] = v;
            self.phase += 1;
        }
        true
    }

    fn play(&mut self, frame: &[f32]) {
        if let Ok(mut p) = self.played.lock() {
            // Keep the last few seconds only.
            if p.len() > SAMPLE_RATE as usize * 2 * 5 {
                p.drain(..FRAME_LEN);
            }
            p.extend_from_slice(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_io_produces_a_sine() {
        let (mut io, played) = FakeIo::new(440.0, 0.5);
        let mut f = vec![0.0; FRAME_LEN];
        assert!(io.capture(&mut f));
        let lvl = super::super::mixer::level_db(&f);
        assert!((lvl + 9.0).abs() < 0.5, "{lvl}");
        io.play(&f);
        assert_eq!(played.lock().unwrap().len(), FRAME_LEN);
    }

    #[test]
    fn rings_are_bounded() {
        let ring: Ring = Arc::default();
        for _ in 0..100 {
            push_bounded(&ring, &[0.0; FRAME_LEN]);
        }
        assert_eq!(ring.lock().unwrap().len(), RING_CAP);
    }
}
