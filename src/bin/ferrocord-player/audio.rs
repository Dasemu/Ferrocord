//! Sound: AAC from MP4 (symphonia, pure Rust) and Opus from WebM (libopus,
//! statically linked), played through `cpal`.
//!
//! Audio is decoded a little ahead of the clock and queued. With a sound
//! device the number of frames the device has consumed drives the playback
//! clock (the audio is the master, so picture and sound cannot drift
//! apart); without one the wall clock does.

use std::collections::VecDeque;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use matroska_demuxer::{Frame, MatroskaFile, TrackType};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_AAC, Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::units::Time;
use symphonia::default::codecs::AacDecoder;
use symphonia::default::formats::IsoMp4Reader;

use super::webm;

/// How far ahead of the clock audio is queued.
pub const AHEAD_MS: i64 = 700;
/// Most queued audio kept (a stuck consumer must not grow the heap).
const MAX_QUEUE_MS: u64 = 4_000;

/// A decoded audio track.
pub trait AudioSource {
    /// `(channels, sample rate)` of the chunks.
    fn format(&self) -> (u16, u32);
    /// Next chunk of interleaved samples and its start time (ms), or `None`
    /// at the end. Damaged packets are skipped.
    fn next_chunk(&mut self) -> Option<(i64, Vec<f32>)>;
    /// Repositions to `ms`.
    fn seek(&mut self, ms: u64);
    /// End of the audio reached.
    fn done(&self) -> bool;
    /// End of the audio decoded so far, ms.
    fn decoded_until(&self) -> i64;
}

/// Opens the audio track of `path` (container by content). `Ok(None)`:
/// no (supported) audio track.
pub fn open(
    path: &Path,
    container: super::source::Container,
) -> Result<Option<Box<dyn AudioSource>>, String> {
    use super::source::Container;
    match container {
        Container::Mp4 => Ok(AacMp4::open(path)?.map(|a| Box::new(a) as Box<dyn AudioSource>)),
        Container::Webm => Ok(OpusWebm::open(path)?.map(|a| Box::new(a) as Box<dyn AudioSource>)),
    }
}

// ---------------------------------------------------------------------
// AAC in MP4
// ---------------------------------------------------------------------

/// AAC audio of an MP4 file.
pub struct AacMp4 {
    reader: IsoMp4Reader,
    decoder: AacDecoder,
    track: u32,
    rate: u32,
    channels: u16,
    until: i64,
    done: bool,
}

impl AacMp4 {
    fn open(path: &Path) -> Result<Option<Self>, String> {
        let f = File::open(path).map_err(|e| format!("open: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(f), Default::default());
        let reader = IsoMp4Reader::try_new(mss, &FormatOptions::default())
            .map_err(|e| format!("mp4 audio demux: {e}"))?;
        let Some(track) = reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec == CODEC_TYPE_AAC)
        else {
            return Ok(None);
        };
        let params = track.codec_params.clone();
        let track = track.id;
        let decoder = AacDecoder::try_new(&params, &DecoderOptions::default())
            .map_err(|e| format!("AAC decoder: {e}"))?;
        // The container may omit the layout; the decoder reads it from the
        // AudioSpecificConfig.
        let known = decoder.codec_params();
        let rate = known
            .sample_rate
            .or(params.sample_rate)
            .ok_or("AAC without a sample rate")?;
        let channels = known
            .channels
            .or(params.channels)
            .map(|c| c.count() as u16)
            .filter(|&c| (1..=8).contains(&c))
            .ok_or("AAC without a channel layout")?;
        Ok(Some(Self {
            reader,
            decoder,
            track,
            rate,
            channels,
            until: 0,
            done: false,
        }))
    }
}

impl AudioSource for AacMp4 {
    fn format(&self) -> (u16, u32) {
        (self.channels, self.rate)
    }

    fn next_chunk(&mut self) -> Option<(i64, Vec<f32>)> {
        for _ in 0..64 {
            let packet = match self.reader.next_packet() {
                Ok(p) => p,
                Err(_) => {
                    self.done = true;
                    return None;
                }
            };
            if packet.track_id() != self.track {
                continue;
            }
            let start = (packet.ts() as i64).saturating_mul(1000) / i64::from(self.rate.max(1));
            let Ok(decoded) = self.decoder.decode(&packet) else {
                continue;
            };
            let spec = *decoded.spec();
            if spec.channels.count() as u16 != self.channels || spec.rate != self.rate {
                // A mid-stream format change: not something phones produce.
                continue;
            }
            let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            buf.copy_interleaved_ref(decoded);
            let samples = buf.samples().to_vec();
            let frames = samples.len() as i64 / i64::from(self.channels.max(1));
            self.until = start + frames * 1000 / i64::from(self.rate.max(1));
            return Some((start, samples));
        }
        None
    }

    fn seek(&mut self, ms: u64) {
        let _ = self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from(ms as f64 / 1000.0),
                track_id: Some(self.track),
            },
        );
        self.decoder.reset();
        self.until = ms as i64;
        self.done = false;
    }

    fn done(&self) -> bool {
        self.done
    }

    fn decoded_until(&self) -> i64 {
        self.until
    }
}

// ---------------------------------------------------------------------
// Opus in WebM
// ---------------------------------------------------------------------

/// What `OpusHead` (the codec private data) says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusHead {
    /// 1 or 2 (channel mapping family 0 only).
    pub channels: u16,
    /// Samples (48 kHz) to drop at the start.
    pub pre_skip: u32,
}

/// Parses `OpusHead`; `None` for anything this player does not decode
/// (surround layouts, bad magic, truncated data).
pub fn parse_opus_head(b: &[u8]) -> Option<OpusHead> {
    if b.len() < 19 || &b[..8] != b"OpusHead" || b[8] != 1 {
        return None;
    }
    let channels = u16::from(b[9]);
    let pre_skip = u32::from(u16::from_le_bytes([b[10], b[11]]));
    let family = b[18];
    (family == 0 && (1..=2).contains(&channels)).then_some(OpusHead { channels, pre_skip })
}

/// Opus audio of a WebM file.
pub struct OpusWebm {
    path: std::path::PathBuf,
    mkv: MatroskaFile<std::io::BufReader<File>>,
    decoder: opus::Decoder,
    track: u64,
    scale_ns: u64,
    channels: u16,
    pre_skip: usize,
    skip: usize,
    frame: Frame,
    pcm: Vec<f32>,
    until: i64,
    done: bool,
}

const OPUS_RATE: u32 = 48_000;
/// 120 ms at 48 kHz: the longest Opus packet.
const OPUS_MAX_FRAMES: usize = 5760;

impl OpusWebm {
    fn open(path: &Path) -> Result<Option<Self>, String> {
        let mkv = webm::open_mkv(path).map_err(|_| "webm audio demux failed".to_string())?;
        let scale_ns = mkv.info().timestamp_scale().get();
        let Some(t) = mkv
            .tracks()
            .iter()
            .find(|t| t.track_type() == TrackType::Audio)
        else {
            return Ok(None);
        };
        if t.codec_id() != "A_OPUS" {
            return Err(format!(
                "{} is not supported",
                webm::describe_codec(t.codec_id())
            ));
        }
        let head = t
            .codec_private()
            .and_then(parse_opus_head)
            .ok_or("unsupported Opus layout")?;
        let track = t.track_number().get();
        let ch = if head.channels == 1 {
            opus::Channels::Mono
        } else {
            opus::Channels::Stereo
        };
        let decoder =
            opus::Decoder::new(OPUS_RATE, ch).map_err(|e| format!("Opus decoder: {e}"))?;
        Ok(Some(Self {
            path: path.to_path_buf(),
            mkv,
            decoder,
            track,
            scale_ns,
            channels: head.channels,
            pre_skip: head.pre_skip as usize,
            skip: head.pre_skip as usize,
            frame: Frame::default(),
            pcm: vec![0.0; OPUS_MAX_FRAMES * usize::from(head.channels)],
            until: 0,
            done: false,
        }))
    }
}

impl AudioSource for OpusWebm {
    fn format(&self) -> (u16, u32) {
        (self.channels, OPUS_RATE)
    }

    fn next_chunk(&mut self) -> Option<(i64, Vec<f32>)> {
        let ch = usize::from(self.channels);
        for _ in 0..64 {
            match self.mkv.next_frame(&mut self.frame) {
                Ok(true) => {}
                _ => {
                    self.done = true;
                    return None;
                }
            }
            if self.frame.track != self.track || self.frame.data.len() > webm::MAX_FRAME {
                continue;
            }
            let start = webm::to_ms(self.frame.timestamp, self.scale_ns);
            let Ok(n) = self
                .decoder
                .decode_float(&self.frame.data, &mut self.pcm, false)
            else {
                continue;
            };
            let n = n.min(OPUS_MAX_FRAMES);
            let drop = self.skip.min(n);
            self.skip -= drop;
            let samples = self.pcm[drop * ch..n * ch].to_vec();
            if samples.is_empty() {
                continue;
            }
            self.until = start + (n - drop) as i64 * 1000 / i64::from(OPUS_RATE);
            return Some((start, samples));
        }
        None
    }

    fn seek(&mut self, ms: u64) {
        let ok = ms > 0
            && self
                .mkv
                .seek(webm::from_ms(
                    i64::try_from(ms).unwrap_or(i64::MAX),
                    self.scale_ns,
                ))
                .is_ok();
        if !ok && let Ok(m) = webm::open_mkv(&self.path) {
            self.mkv = m;
        }
        let _ = self.decoder.reset_state();
        self.skip = if ms == 0 { self.pre_skip } else { 0 };
        self.until = ms as i64;
        self.done = false;
    }

    fn done(&self) -> bool {
        self.done
    }

    fn decoded_until(&self) -> i64 {
        self.until
    }
}

// ---------------------------------------------------------------------
// Output through cpal
// ---------------------------------------------------------------------

/// Queued source-format audio and how much of it the device has taken.
#[derive(Debug, Default)]
pub struct Queue {
    samples: VecDeque<f32>,
    channels: usize,
    rate: u32,
    /// Position between the first two queued frames, `0.0..1.0`.
    frac: f64,
    /// Source frames consumed since the last clear.
    consumed: u64,
}

impl Queue {
    /// Appends interleaved samples of the given format (a format change
    /// drops what was queued).
    pub fn push(&mut self, channels: u16, rate: u32, samples: &[f32]) {
        let channels = usize::from(channels.max(1));
        if channels != self.channels || rate != self.rate {
            self.samples.clear();
            self.frac = 0.0;
            self.channels = channels;
            self.rate = rate;
        }
        let cap = (u64::from(rate) * MAX_QUEUE_MS / 1000) as usize * channels;
        if self.samples.len() + samples.len() > cap {
            return;
        }
        self.samples.extend(samples.iter().copied());
    }

    /// Empties the queue and the consumed counter (seek).
    pub fn clear(&mut self) {
        self.samples.clear();
        self.frac = 0.0;
        self.consumed = 0;
    }

    /// Whole frames queued.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    /// Audio consumed since the last clear, ms.
    pub fn consumed_ms(&self) -> i64 {
        if self.rate == 0 {
            return 0;
        }
        (self.consumed.saturating_mul(1000) / u64::from(self.rate)) as i64
    }

    /// Fills `out` (interleaved, `out_ch` channels at `out_rate`) with
    /// linearly resampled audio scaled by `gain`; silence once the queue
    /// runs dry. Returns the frames of the source consumed.
    pub fn fill(&mut self, out: &mut [f32], out_ch: usize, out_rate: u32, gain: f32) {
        let out_ch = out_ch.max(1);
        let src_ch = self.channels.max(1);
        let step = if out_rate == 0 || self.rate == 0 {
            1.0
        } else {
            f64::from(self.rate) / f64::from(out_rate)
        };
        for frame in out.chunks_mut(out_ch) {
            if self.samples.len() < src_ch * 2 {
                frame.fill(0.0);
                continue;
            }
            let f = self.frac as f32;
            let sample = |q: &Queue, i: usize, c: usize| -> f32 {
                // Map output channel `c` to the source layout.
                let get = |k: usize| q.samples.get(i * src_ch + k).copied().unwrap_or(0.0);
                if src_ch == 1 {
                    get(0)
                } else if out_ch == 1 {
                    (get(0) + get(1)) * 0.5
                } else if c < src_ch {
                    get(c)
                } else {
                    0.0
                }
            };
            for (c, o) in frame.iter_mut().enumerate() {
                let a = sample(self, 0, c);
                let b = sample(self, 1, c);
                *o = (a + (b - a) * f) * gain;
            }
            self.frac += step;
            while self.frac >= 1.0 && self.samples.len() >= src_ch * 2 {
                self.frac -= 1.0;
                self.samples.drain(..src_ch);
                self.consumed += 1;
            }
        }
    }
}

struct Shared {
    queue: Mutex<Queue>,
    playing: AtomicBool,
    /// `f32` bits.
    gain: AtomicU32,
}

/// The sound device. Missing (no device, CI): playback is silent and the
/// wall clock drives the picture.
pub struct Output {
    shared: Arc<Shared>,
    _stream: cpal::Stream,
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let ch = usize::from(config.channels);
    let rate = config.sample_rate.0;
    let mut tmp: Vec<f32> = Vec::new();
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            tmp.resize(data.len(), 0.0);
            if shared.playing.load(Ordering::Relaxed) {
                let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed));
                match shared.queue.lock() {
                    Ok(mut q) => q.fill(&mut tmp, ch, rate, gain),
                    Err(_) => tmp.fill(0.0),
                }
            } else {
                tmp.fill(0.0);
            }
            for (d, s) in data.iter_mut().zip(tmp.iter()) {
                *d = T::from_sample(*s);
            }
        },
        |_| {},
        None,
    )
}

impl Output {
    /// Opens the default output device.
    pub fn open() -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let config: cpal::StreamConfig = supported.config();
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
        });
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, shared.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, shared.clone()),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, shared.clone()),
            _ => return None,
        }
        .ok()?;
        stream.play().ok()?;
        Some(Self {
            shared,
            _stream: stream,
        })
    }

    /// Queues samples.
    pub fn push(&self, channels: u16, rate: u32, samples: &[f32]) {
        if let Ok(mut q) = self.shared.queue.lock() {
            q.push(channels, rate, samples);
        }
    }

    /// Drops everything queued (seek).
    pub fn clear(&self) {
        if let Ok(mut q) = self.shared.queue.lock() {
            q.clear();
        }
    }

    /// Pause / resume.
    pub fn set_playing(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
    }

    /// Volume 0..=1 (0 when muted).
    pub fn set_volume(&self, v: f32) {
        self.shared
            .gain
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Audio the device consumed since the last [`Output::clear`], ms.
    pub fn consumed_ms(&self) -> i64 {
        self.shared.queue.lock().map_or(0, |q| q.consumed_ms())
    }

    /// Whole frames waiting to be played.
    pub fn queued_frames(&self) -> usize {
        self.shared.queue.lock().map_or(0, |q| q.frames())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_head_accepts_mono_and_stereo_only() {
        let mut h = b"OpusHead".to_vec();
        h.extend([1, 2, 0x38, 0x01]); // v1, 2 ch, pre-skip 312
        h.extend(48_000u32.to_le_bytes());
        h.extend([0, 0, 0]); // gain, family 0
        assert_eq!(
            parse_opus_head(&h),
            Some(OpusHead {
                channels: 2,
                pre_skip: 312
            })
        );
        let mut six = h.clone();
        six[9] = 6;
        assert_eq!(parse_opus_head(&six), None);
        let mut fam1 = h.clone();
        fam1[18] = 1;
        assert_eq!(parse_opus_head(&fam1), None);
        assert_eq!(parse_opus_head(b"OpusTags........"), None);
        assert_eq!(parse_opus_head(&h[..10]), None);
    }

    #[test]
    fn the_queue_resamples_and_counts_consumed_frames() {
        let mut q = Queue::default();
        // 1 s of mono ramp at 4 Hz is silly but exact: 4 frames.
        q.push(1, 4, &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        let mut out = [0.0f32; 4];
        // Same rate, mono → mono.
        q.fill(&mut out, 1, 4, 1.0);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(q.consumed_ms(), 1000);
        // Double rate output interpolates.
        let mut out = [0.0f32; 4];
        q.fill(&mut out, 1, 8, 1.0);
        assert_eq!(out, [4.0, 4.5, 5.0, 5.5]);
        // Gain applies; a dry queue gives silence and no consumption.
        let mut out = [9.0f32; 6];
        q.fill(&mut out, 1, 4, 0.5);
        assert_eq!(&out[..1], &[3.0]);
        assert!(out[2..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn channel_layouts_are_mapped() {
        let mut q = Queue::default();
        q.push(2, 10, &[1.0, -1.0, 1.0, -1.0, 1.0, -1.0]);
        let mut mono = [0.0f32; 1];
        q.fill(&mut mono, 1, 10, 1.0);
        assert_eq!(mono, [0.0]);
        let mut q = Queue::default();
        q.push(1, 10, &[0.5, 0.5, 0.5]);
        let mut stereo = [0.0f32; 2];
        q.fill(&mut stereo, 2, 10, 1.0);
        assert_eq!(stereo, [0.5, 0.5]);
    }

    #[test]
    fn the_queue_is_bounded_and_clears() {
        let mut q = Queue::default();
        let big = vec![0.0f32; 48_000 * 10];
        q.push(1, 48_000, &big);
        assert_eq!(q.frames(), 0, "an oversized push is refused");
        q.push(1, 48_000, &vec![0.0; 1000]);
        assert_eq!(q.frames(), 1000);
        q.clear();
        assert_eq!((q.frames(), q.consumed_ms()), (0, 0));
    }
}
