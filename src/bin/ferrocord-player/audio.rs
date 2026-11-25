//! Sound: AAC from MP4 (symphonia, pure Rust), Opus from WebM / Matroska
//! (libopus, statically linked) and AAC from Matroska (a platform decoder),
//! played through `cpal`.
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
/// Most frames read forward when a Matroska seek has to be done by hand.
const MAX_SKIP_FRAMES: usize = 2_000_000;
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
        Container::Webm => Ok(MkvAudio::open(path)?.map(|a| Box::new(a) as Box<dyn AudioSource>)),
    }
}

// ---------------------------------------------------------------------
// AAC in MP4
// ---------------------------------------------------------------------

/// Channel count from an MPEG-4 AudioSpecificConfig (ISO 14496-3 1.6.2.1):
/// object type (5 bits, or 5 + 6), frequency index (4 bits, or 4 + 24 for
/// an explicit rate), channel configuration (4 bits). `None` for a layout
/// given only by a program config element.
pub fn asc_channels(asc: &[u8]) -> Option<u16> {
    let bits = |from: usize, n: usize| -> Option<u32> {
        (from..from + n).try_fold(0u32, |acc, i| {
            let byte = *asc.get(i / 8)?;
            Some((acc << 1) | u32::from((byte >> (7 - i % 8)) & 1))
        })
    };
    let mut at = 5;
    if bits(0, 5)? == 31 {
        at += 6;
    }
    if bits(at, 4)? == 15 {
        at += 24;
    }
    at += 4;
    match bits(at, 4)? {
        c @ 1..=6 => Some(c as u16),
        7 => Some(8),
        _ => None,
    }
}

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
        // The MP4 reader leaves the channels unset and the decoder's
        // `codec_params` echoes what it was given: the count is in the
        // AudioSpecificConfig.
        let rate = params.sample_rate.ok_or("AAC without a sample rate")?;
        let channels = params
            .channels
            .map(|c| c.count() as u16)
            .or_else(|| params.extra_data.as_deref().and_then(asc_channels))
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

/// Decodes one compressed audio frame at a time (the container is read
/// elsewhere): Opus through libopus, AAC through a platform decoder.
pub trait FrameDecoder {
    /// `(channels, sample rate)` of the decoded samples. Platform decoders
    /// learn it from the first frame.
    fn format(&self) -> (u16, u32);
    /// Decodes `data`, appending interleaved samples to `out`.
    fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<(), String>;
    /// Starts over (after a seek).
    fn reset(&mut self);
}

const OPUS_RATE: u32 = 48_000;
/// 120 ms at 48 kHz: the longest Opus packet.
const OPUS_MAX_FRAMES: usize = 5760;

/// Opus through the statically linked libopus.
struct Opus {
    decoder: opus::Decoder,
    channels: u16,
    pcm: Vec<f32>,
}

impl Opus {
    fn new(channels: u16) -> Result<Self, String> {
        let ch = if channels == 1 {
            opus::Channels::Mono
        } else {
            opus::Channels::Stereo
        };
        Ok(Self {
            decoder: opus::Decoder::new(OPUS_RATE, ch).map_err(|e| format!("Opus decoder: {e}"))?,
            channels,
            pcm: vec![0.0; OPUS_MAX_FRAMES * usize::from(channels)],
        })
    }
}

impl FrameDecoder for Opus {
    fn format(&self) -> (u16, u32) {
        (self.channels, OPUS_RATE)
    }

    fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        let n = self
            .decoder
            .decode_float(data, &mut self.pcm, false)
            .map_err(|e| format!("Opus: {e}"))?
            .min(OPUS_MAX_FRAMES);
        out.extend_from_slice(&self.pcm[..n * usize::from(self.channels)]);
        Ok(())
    }

    fn reset(&mut self) {
        let _ = self.decoder.reset_state();
    }
}

/// What an ADTS header needs from an AudioSpecificConfig: raw AAC frames
/// with this header in front decode without any out-of-band configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adts {
    /// Audio object type of the core (1 Main, 2 LC, 3 SSR, 4 LTP).
    object: u8,
    /// Sampling frequency index of the core.
    freq: u8,
    /// Channel configuration (1-7).
    config: u8,
}

/// Sampling frequencies by index (ISO 14496-3 1.6.3.4).
const AAC_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

impl Adts {
    /// From an AudioSpecificConfig. HE-AAC (object type 5 / 29, explicit
    /// SBR / PS signalling) maps to its AAC-LC core: the decoder finds the
    /// extension in the frames (implicit signalling). `None` for layouts
    /// ADTS cannot carry (explicit sample rates, program config elements).
    pub fn from_asc(asc: &[u8]) -> Option<Adts> {
        let bits = |from: usize, n: usize| -> Option<u32> {
            (from..from + n).try_fold(0u32, |acc, i| {
                let byte = *asc.get(i / 8)?;
                Some((acc << 1) | u32::from((byte >> (7 - i % 8)) & 1))
            })
        };
        let mut at = 0;
        let object_type = |at: &mut usize| -> Option<u32> {
            let t = bits(*at, 5)?;
            *at += 5;
            if t == 31 {
                let e = bits(*at, 6)?;
                *at += 6;
                return Some(32 + e);
            }
            Some(t)
        };
        let mut object = object_type(&mut at)?;
        let freq = bits(at, 4)?;
        at += 4;
        let config = bits(at, 4)?;
        at += 4;
        let mut core_freq = freq;
        if object == 5 || object == 29 {
            // Extension sampling frequency, then the core's object type.
            let ext = bits(at, 4)?;
            at += 4;
            if ext == 15 {
                return None;
            }
            object = object_type(&mut at)?;
        }
        if core_freq == 15 {
            return None;
        }
        core_freq = core_freq.min(12);
        if !(1..=4).contains(&object) || !(1..=7).contains(&config) {
            return None;
        }
        Some(Adts {
            object: object as u8,
            freq: core_freq as u8,
            config: config as u8,
        })
    }

    /// Channels of the configuration.
    pub fn channels(&self) -> u16 {
        match self.config {
            7 => 8,
            c => u16::from(c),
        }
    }

    /// Core sample rate (SBR doubles it; the decoder reports the real one).
    pub fn rate(&self) -> u32 {
        AAC_RATES
            .get(usize::from(self.freq))
            .copied()
            .unwrap_or(48_000)
    }

    /// The 7-byte header (no CRC) for a raw frame of `payload` bytes.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn header(&self, payload: usize) -> Option<[u8; 7]> {
        let len = payload.checked_add(7).filter(|&l| l < 1 << 13)?;
        let profile = self.object - 1;
        Some([
            0xFF,
            0xF1, // MPEG-4, layer 0, no CRC
            (profile << 6) | (self.freq << 2) | (self.config >> 2),
            ((self.config & 3) << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            (((len & 7) as u8) << 5) | 0x1F,
            0xFC, // buffer fullness 0x7FF (VBR), one raw block
        ])
    }
}

/// The audio track of a WebM / Matroska file: Opus (libopus) or AAC (a
/// platform decoder: the system FFmpeg on Linux, Media Foundation on
/// Windows).
pub struct MkvAudio {
    path: std::path::PathBuf,
    mkv: MatroskaFile<std::io::BufReader<File>>,
    decoder: Box<dyn FrameDecoder>,
    track: u64,
    scale_ns: u64,
    /// Samples (per channel) to drop at the start (Opus pre-skip).
    pre_skip: usize,
    skip: usize,
    frame: Frame,
    pcm: Vec<f32>,
    /// A chunk decoded at open to learn the format, not yet handed out.
    primed: Option<(i64, Vec<f32>)>,
    /// `frame` holds a frame read but not yet decoded (after a seek).
    peeked: bool,
    until: i64,
    done: bool,
}

impl MkvAudio {
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
        let (decoder, pre_skip): (Box<dyn FrameDecoder>, usize) = match t.codec_id() {
            "A_OPUS" => {
                let head = t
                    .codec_private()
                    .and_then(parse_opus_head)
                    .ok_or("unsupported Opus layout")?;
                (Box::new(Opus::new(head.channels)?), head.pre_skip as usize)
            }
            // OBS and phones record AAC into Matroska: `A_AAC` with the
            // AudioSpecificConfig as private data.
            "A_AAC" => {
                let asc = t.codec_private().ok_or("AAC without its configuration")?;
                let d = super::platform::aac(asc).map_err(|m| {
                    let mut why = "AAC in Matroska needs a system AAC decoder".to_string();
                    if let Some(h) = m.hint {
                        why = format!("{why}. {h}");
                    }
                    why
                })?;
                (d, 0)
            }
            other => {
                return Err(format!("{} is not supported", webm::describe_codec(other)));
            }
        };
        let track = t.track_number().get();
        let mut a = Self {
            path: path.to_path_buf(),
            mkv,
            decoder,
            track,
            scale_ns,
            pre_skip,
            skip: pre_skip,
            frame: Frame::default(),
            pcm: Vec::new(),
            primed: None,
            peeked: false,
            until: 0,
            done: false,
        };
        // Platform decoders know the real format (HE-AAC doubles the rate)
        // only after a frame: decode the first chunk now.
        a.primed = a.decode_next();
        if a.primed.is_none() {
            return Err("no audio frame could be decoded".into());
        }
        Ok(Some(a))
    }

    fn decode_next(&mut self) -> Option<(i64, Vec<f32>)> {
        let (ch, _) = self.decoder.format();
        let ch = usize::from(ch.max(1));
        for _ in 0..64 {
            if !std::mem::take(&mut self.peeked) {
                match self.mkv.next_frame(&mut self.frame) {
                    Ok(true) => {}
                    _ => {
                        self.done = true;
                        return None;
                    }
                }
            }
            if self.frame.track != self.track || self.frame.data.len() > webm::MAX_FRAME {
                continue;
            }
            let start = webm::to_ms(self.frame.timestamp, self.scale_ns);
            self.pcm.clear();
            if self
                .decoder
                .decode(&self.frame.data, &mut self.pcm)
                .is_err()
            {
                continue;
            }
            let (ch_now, rate) = self.decoder.format();
            let ch = if ch_now == 0 { ch } else { usize::from(ch_now) };
            let n = self.pcm.len() / ch;
            let drop = self.skip.min(n);
            self.skip -= drop;
            let samples = self.pcm[drop * ch..n * ch].to_vec();
            if samples.is_empty() {
                continue;
            }
            self.until = start + (n - drop) as i64 * 1000 / i64::from(rate.max(1));
            return Some((start, samples));
        }
        None
    }
}

impl AudioSource for MkvAudio {
    fn format(&self) -> (u16, u32) {
        self.decoder.format()
    }

    fn next_chunk(&mut self) -> Option<(i64, Vec<f32>)> {
        self.primed.take().or_else(|| self.decode_next())
    }

    fn seek(&mut self, ms: u64) {
        let target = webm::from_ms(i64::try_from(ms).unwrap_or(i64::MAX), self.scale_ns);
        // matroska-demuxer's seek can report success and then the end of
        // the stream (files without audio cues): check that a frame
        // follows, else reopen and read forward to the target.
        self.peeked = ms > 0
            && self.mkv.seek(target).is_ok()
            && matches!(self.mkv.next_frame(&mut self.frame), Ok(true));
        if !self.peeked
            && let Ok(m) = webm::open_mkv(&self.path)
        {
            self.mkv = m;
            if ms > 0 {
                for _ in 0..MAX_SKIP_FRAMES {
                    match self.mkv.next_frame(&mut self.frame) {
                        Ok(true) if self.frame.timestamp < target => {}
                        Ok(true) => {
                            self.peeked = true;
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        self.decoder.reset();
        self.primed = None;
        self.skip = if ms == 0 { self.pre_skip } else { 0 };
        self.until = ms as i64;
        self.done = false;
    }

    fn done(&self) -> bool {
        self.done && self.primed.is_none()
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

    /// MP4 / QuickTime AAC used to be dropped as "without a channel
    /// layout" (the reader leaves the count unset): every clip played mute.
    #[test]
    fn aac_in_mp4_and_quicktime_decodes_to_sound() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video");
        for name in ["high_aac.mp4", "h264_frag_aac.mp4", "h264_aac.mov"] {
            let mut a = AacMp4::open(&dir.join(name))
                .unwrap_or_else(|e| panic!("{name}: {e}"))
                .unwrap_or_else(|| panic!("{name}: no audio track"));
            let (channels, rate) = a.format();
            assert_eq!(channels, 1, "{name}");
            assert!([44_100, 48_000].contains(&rate), "{name}: {rate} Hz");
            let mut loud = 0usize;
            let mut total = 0usize;
            while let Some((_, samples)) = a.next_chunk() {
                total += samples.len();
                loud += samples.iter().filter(|s| s.abs() > 0.05).count();
            }
            // 2 s of a sine tone.
            assert!(total > 80_000, "{name}: {total} samples");
            assert!(loud > total / 4, "{name}: silence");
        }
    }

    /// OBS writes AAC into Matroska: it decodes through the platform AAC
    /// decoder (system FFmpeg / Media Foundation). Skipped without one
    /// unless CI says one must be there.
    #[test]
    fn aac_in_matroska_decodes_to_sound_with_a_platform_decoder() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video/h264_aac.mkv");
        let mut a = match MkvAudio::open(&path) {
            Ok(Some(a)) => a,
            Ok(None) => panic!("no audio track"),
            Err(e) => {
                assert!(
                    std::env::var("FERROCORD_EXPECT_PLATFORM").as_deref() != Ok("1"),
                    "no platform AAC decoder: {e}"
                );
                assert!(e.contains("AAC"), "{e}");
                return;
            }
        };
        assert_eq!(a.format(), (1, 48_000));
        let (mut loud, mut total) = (0usize, 0usize);
        let mut last = -1;
        while let Some((t, samples)) = a.next_chunk() {
            assert!(t >= last, "time went back: {t} < {last}");
            last = t;
            total += samples.len();
            loud += samples.iter().filter(|s| s.abs() > 0.05).count();
        }
        assert!(total > 90_000, "{total} samples");
        assert!(loud > total / 4, "silence");
        // Seeking lands near the target, also after the end (where
        // matroska-demuxer's own seek can land past the last frame).
        a.seek(1000);
        let (t, _) = a.next_chunk().expect("audio after the seek");
        assert!((800..=1100).contains(&t), "{t}");
    }

    #[test]
    fn aac_channels_come_from_the_audio_specific_config() {
        // AAC-LC 44.1 kHz mono / stereo (what ffmpeg writes in `esds`).
        assert_eq!(asc_channels(&[0x12, 0x08]), Some(1));
        assert_eq!(asc_channels(&[0x12, 0x10]), Some(2));
        // 48 kHz 5.1, and channel configuration 7 = 7.1.
        assert_eq!(asc_channels(&[0x11, 0xB0]), Some(6));
        assert_eq!(asc_channels(&[0x11, 0xB8]), Some(8));
        // Explicit 24-bit sample rate (index 15) shifts the field.
        assert_eq!(asc_channels(&[0x17, 0x80, 0x00, 0x00, 0x10]), Some(2));
        // Layout in a program config element, and truncated input.
        assert_eq!(asc_channels(&[0x12, 0x00]), None);
        assert_eq!(asc_channels(&[0x12]), None);
        assert_eq!(asc_channels(&[]), None);
    }

    #[test]
    fn adts_headers_come_from_the_audio_specific_config() {
        // AAC-LC 44.1 kHz mono.
        let a = Adts::from_asc(&[0x12, 0x08]).unwrap();
        assert_eq!((a.channels(), a.rate()), (1, 44_100));
        // What ffmpeg writes for a 100-byte frame of that stream.
        assert_eq!(
            a.header(100).unwrap(),
            [0xFF, 0xF1, 0x50, 0x40, 0x0D, 0x7F, 0xFC]
        );
        assert_eq!(a.header(8192), None);
        // 48 kHz stereo.
        let a = Adts::from_asc(&[0x11, 0x90]).unwrap();
        assert_eq!((a.channels(), a.rate()), (2, 48_000));
        // HE-AAC v1 with explicit SBR (type 5): 24 kHz LC core, 48 kHz out.
        // 00101 0110 0010 0011 00010 -> type 5, freq 6, 2 ch, ext 3, core 2.
        let he = Adts::from_asc(&[0x2B, 0x11, 0x88]).unwrap();
        assert_eq!(he.header(10).unwrap()[2] >> 6, 1, "LC profile");
        assert_eq!((he.channels(), he.rate()), (2, 24_000));
        // Layout in a program config element, explicit rate, garbage.
        assert_eq!(Adts::from_asc(&[0x12, 0x00]), None);
        assert_eq!(Adts::from_asc(&[0x17, 0x80, 0x00, 0x00, 0x10]), None);
        assert_eq!(Adts::from_asc(&[]), None);
        assert_eq!(Adts::from_asc(&[0xFF, 0xFF]), None);
    }

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
