//! Media Foundation decoder transforms (Windows).
//!
//! The decoders are the synchronous software MFTs Windows registers: the
//! H.264 and AAC decoders every desktop Windows has, H.265 from "HEVC Video
//! Extensions" (from Device Manufacturer), AV1 from "AV1 Video Extension",
//! VP9 from "VP9 Video Extensions" (Microsoft Store). Asynchronous
//! (hardware-only) transforms are not driven: they need the event model
//! and a Direct3D device.
//!
//! `mfplat.dll` is loaded at run time, never imported: Windows N editions
//! without the Media Feature Pack have no Media Foundation, and the helper
//! must still start there and play what its built-in decoders handle.
//! Everything else goes through COM interfaces (vtable calls, no imports).

use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use libloading::Library;
use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, HRESULT};

use super::super::audio::{Adts, FrameDecoder};
use super::super::codec::{Geometry, PendingTimes, Pictures, VideoDecoder};
use super::super::source::{Codec, Packet, VideoInfo};
use super::super::yuv::{self, Layout, Raw};
use super::{Missing, key, windows_hint};

/// Largest compressed packet handed to a decoder.
const MAX_PACKET: usize = 64 * 1024 * 1024;
/// Largest decoded buffer read back (8K P010 is ~100 MB).
const MAX_OUTPUT: usize = 256 * 1024 * 1024;
/// Output calls per input before giving up (a decoder that loops).
const MAX_PULLS: usize = 64;

type EnumEx = unsafe extern "system" fn(
    GUID,
    u32,
    *const MFT_REGISTER_TYPE_INFO,
    *const MFT_REGISTER_TYPE_INFO,
    *mut *mut Option<IMFActivate>,
    *mut u32,
) -> HRESULT;

/// The `mfplat.dll` entry points used.
struct Mf {
    enum_ex: EnumEx,
    media_type: unsafe extern "system" fn(*mut Option<IMFMediaType>) -> HRESULT,
    sample: unsafe extern "system" fn(*mut Option<IMFSample>) -> HRESULT,
    memory_buffer: unsafe extern "system" fn(u32, *mut Option<IMFMediaBuffer>) -> HRESULT,
    _lib: Library,
}

static MF: OnceLock<Option<Mf>> = OnceLock::new();

/// COM on this thread (decoding runs on the helper's main thread).
fn com() {
    // SAFETY: initialises COM for the calling thread; repeated calls only
    // return S_FALSE, a different apartment only an error code.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

fn mf() -> Option<&'static Mf> {
    MF.get_or_init(|| {
        com();
        // SAFETY: mfplat.dll is a system library without load side effects.
        let lib = unsafe { Library::new("mfplat.dll") }.ok()?;
        macro_rules! sym {
            ($name:literal) => {
                // SAFETY: the documented signature of this export.
                unsafe { *lib.get(concat!($name, "\0").as_bytes()).ok()? }
            };
        }
        let startup: unsafe extern "system" fn(u32, u32) -> HRESULT = sym!("MFStartup");
        // SAFETY: starts Media Foundation once for the process (never shut
        // down: the helper exits with its video).
        if unsafe { startup(MF_VERSION, MFSTARTUP_LITE) }.is_err() {
            return None;
        }
        Some(Mf {
            enum_ex: sym!("MFTEnumEx"),
            media_type: sym!("MFCreateMediaType"),
            sample: sym!("MFCreateSample"),
            memory_buffer: sym!("MFCreateMemoryBuffer"),
            _lib: lib,
        })
    })
    .as_ref()
}

/// Calls an `MFCreate*` function that returns one interface.
fn create<T>(f: impl FnOnce(*mut Option<T>) -> HRESULT) -> windows::core::Result<T> {
    let mut out = None;
    f(&mut out).ok()?;
    out.ok_or_else(|| E_POINTER.into())
}

impl Mf {
    fn media_type(&self) -> windows::core::Result<IMFMediaType> {
        // SAFETY: writes one interface pointer.
        create(|p| unsafe { (self.media_type)(p) })
    }

    /// A sample holding `data`, at `pts_ms`.
    fn sample(&self, data: &[u8], pts_ms: i64, key: bool) -> windows::core::Result<IMFSample> {
        let len = u32::try_from(data.len()).map_err(|_| windows::core::Error::from(E_POINTER))?;
        // SAFETY: a buffer of `len` bytes, filled within its bounds while
        // locked; the sample takes a reference to it.
        unsafe {
            let buf = create(|p| (self.memory_buffer)(len.max(1), p))?;
            let mut ptr = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None)?;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            }
            buf.Unlock()?;
            buf.SetCurrentLength(len)?;
            let s: IMFSample = create(|p| (self.sample)(p))?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(pts_ms.saturating_mul(10_000))?;
            if key {
                s.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
            }
            Ok(s)
        }
    }

    /// An empty sample of `size` bytes for a decoder that does not
    /// allocate its own output.
    fn empty_sample(&self, size: u32) -> windows::core::Result<IMFSample> {
        // SAFETY: plain allocations.
        unsafe {
            let buf = create(|p| (self.memory_buffer)(size.max(1), p))?;
            let s: IMFSample = create(|p| (self.sample)(p))?;
            s.AddBuffer(&buf)?;
            Ok(s)
        }
    }

    /// The registered synchronous decoders for `subtype`, best first.
    fn decoders(&self, category: GUID, major: GUID, subtype: GUID) -> Vec<IMFActivate> {
        com();
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: major,
            guidSubtype: subtype,
        };
        let flags = MFT_ENUM_FLAG_SYNCMFT.0
            | MFT_ENUM_FLAG_LOCALMFT.0
            | MFT_ENUM_FLAG_SORTANDFILTER.0
            | MFT_ENUM_FLAG_UNTRUSTED_STOREMFT.0;
        let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        // SAFETY: on success `arr` holds `n` activation objects allocated
        // with CoTaskMemAlloc: each is moved out (released on drop) and the
        // array freed.
        unsafe {
            if (self.enum_ex)(
                category,
                flags as u32,
                &input,
                std::ptr::null(),
                &mut arr,
                &mut n,
            )
            .is_err()
                || arr.is_null()
            {
                return Vec::new();
            }
            let out = (0..n as usize).filter_map(|i| arr.add(i).read()).collect();
            CoTaskMemFree(Some(arr as *const _));
            out
        }
    }

    /// The first decoder for `subtype` that starts.
    fn transform(&self, category: GUID, major: GUID, subtype: GUID) -> Option<IMFTransform> {
        self.decoders(category, major, subtype)
            .into_iter()
            // SAFETY: creates the registered COM object.
            .find_map(|a| unsafe { a.ActivateObject::<IMFTransform>() }.ok())
    }
}

/// The input subtype of a video codec.
fn subtype(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MFVideoFormat_AV1,
        Codec::Vp9 => MFVideoFormat_VP90,
        Codec::Vp8 => MFVideoFormat_VP80,
    }
}

/// `--backends` lines.
pub fn report() -> Vec<String> {
    let Some(mf) = mf() else {
        return vec!["platform none (Media Foundation is not installed)".into()];
    };
    let mut out = vec!["platform mediafoundation".to_string()];
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1, Codec::Vp9] {
        let n = mf
            .decoders(
                MFT_CATEGORY_VIDEO_DECODER,
                MFMediaType_Video,
                subtype(codec),
            )
            .len();
        out.push(format!(
            "{} {}",
            key(codec),
            if n > 0 { "mft" } else { "-" }
        ));
    }
    let n = mf
        .decoders(
            MFT_CATEGORY_AUDIO_DECODER,
            MFMediaType_Audio,
            MFAudioFormat_AAC,
        )
        .len();
    out.push(format!("aac {}", if n > 0 { "mft" } else { "-" }));
    out
}

/// The hint for `what` on this system.
pub fn hint(what: &str, _lib: Option<&str>) -> Option<String> {
    windows_hint(what, mf().is_some())
}

fn missing(what: &str) -> Missing {
    Missing {
        hint: windows_hint(what, mf().is_some()),
    }
}

/// What one `ProcessOutput` call gave.
enum Pull {
    Sample(IMFSample),
    NeedInput,
    FormatChanged,
}

/// One `ProcessOutput` call, with the sample allocated here when the
/// decoder does not provide its own.
fn pull(mf: &Mf, t: &IMFTransform) -> windows::core::Result<Pull> {
    // SAFETY: the output buffer struct owns at most one sample and one
    // event collection; both are taken back and released after the call.
    unsafe {
        let info = t.GetOutputStreamInfo(0)?;
        let own =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        let sample = if info.dwFlags & own == 0 {
            Some(mf.empty_sample(info.cbSize)?)
        } else {
            None
        };
        let mut buf = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let r = t.ProcessOutput(0, &mut buf, &mut status);
        let sample = ManuallyDrop::take(&mut buf[0].pSample);
        drop(ManuallyDrop::take(&mut buf[0].pEvents));
        match r {
            Ok(()) => Ok(sample.map_or(Pull::NeedInput, Pull::Sample)),
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Pull::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Pull::FormatChanged),
            Err(e) => Err(e),
        }
    }
}

/// Feeds `sample`, pulling output whenever the decoder is full.
fn feed(
    t: &IMFTransform,
    sample: &IMFSample,
    mut drain: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    for _ in 0..8 {
        // SAFETY: a valid transform and sample.
        match unsafe { t.ProcessInput(0, sample, 0) } {
            Ok(()) => return drain(),
            Err(e) if e.code() == MF_E_NOTACCEPTING => drain()?,
            Err(e) => return Err(format!("{e}")),
        }
    }
    Err("the decoder accepts no more data".into())
}

fn start(t: &IMFTransform) {
    // SAFETY: plain notifications.
    unsafe {
        let _ = t.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
        let _ = t.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
    }
}

// ---------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------

/// The negotiated output: layout and buffer geometry.
#[derive(Clone, Copy)]
struct Out {
    layout: Layout,
    /// YV12-style order (Cr plane before Cb).
    swap_uv: bool,
    /// Buffer width / height (aligned: 1088 for 1080p H.264).
    width: usize,
    height: usize,
    /// Default stride, when the type gives one.
    stride: Option<usize>,
}

/// A video stream through a Media Foundation decoder.
pub struct Video {
    mf: &'static Mf,
    t: IMFTransform,
    codec: Codec,
    header: Vec<u8>,
    fresh: bool,
    pending: PendingTimes,
    geo: Geometry,
    out: Out,
    /// Visible size from the container.
    width: usize,
    height: usize,
}

/// A Media Foundation video decoder for `info`.
pub fn video(info: &VideoInfo, geo: Geometry) -> Result<Box<dyn VideoDecoder>, Missing> {
    let what = info.codec.name();
    let mf = mf().ok_or_else(|| missing(what))?;
    let t = mf
        .transform(
            MFT_CATEGORY_VIDEO_DECODER,
            MFMediaType_Video,
            subtype(info.codec),
        )
        .ok_or_else(|| missing(what))?;
    let set_input = || -> windows::core::Result<()> {
        let it = mf.media_type()?;
        // SAFETY: attribute setters on a fresh media type.
        unsafe {
            it.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            it.SetGUID(&MF_MT_SUBTYPE, &subtype(info.codec))?;
            if info.width > 0 && info.height > 0 {
                it.SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    ((info.width as u64) << 32) | info.height as u64,
                )?;
            }
            it.SetUINT32(
                &MF_MT_INTERLACE_MODE,
                MFVideoInterlace_MixedInterlaceOrProgressive.0 as u32,
            )?;
            t.SetInputType(0, &it, 0)
        }
    };
    let failed = |_| Missing {
        hint: Some(format!("The Windows {what} decoder could not start.")),
    };
    set_input().map_err(failed)?;
    let out = negotiate(&t).map_err(|e| Missing {
        hint: Some(format!("The Windows {what} decoder could not start ({e}).")),
    })?;
    start(&t);
    Ok(Box::new(Video {
        mf,
        t,
        codec: info.codec,
        header: info.header.clone(),
        fresh: true,
        pending: PendingTimes::default(),
        geo,
        out,
        width: info.width,
        height: info.height,
    }))
}

/// Picks an output type the helper converts (the decoder lists its
/// preferred one first) and sets it.
fn negotiate(t: &IMFTransform) -> Result<Out, String> {
    for i in 0..64 {
        // SAFETY: enumeration of the decoder's output types.
        let mt = match unsafe { t.GetOutputAvailableType(0, i) } {
            Ok(mt) => mt,
            Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
            Err(e) => return Err(format!("{e}")),
        };
        // SAFETY: attribute getters.
        let Ok(sub) = (unsafe { mt.GetGUID(&MF_MT_SUBTYPE) }) else {
            continue;
        };
        let (layout, swap_uv) = if sub == MFVideoFormat_NV12 {
            (Layout::NV12, false)
        } else if sub == MFVideoFormat_P010 {
            (Layout::P010, false)
        } else if sub == MFVideoFormat_I420 || sub == MFVideoFormat_IYUV {
            (Layout::I420, false)
        } else if sub == MFVideoFormat_YV12 {
            (Layout::I420, true)
        } else {
            continue;
        };
        // SAFETY: setting a type the decoder offered.
        unsafe { t.SetOutputType(0, &mt, 0) }.map_err(|e| format!("{e}"))?;
        // SAFETY: attribute getters.
        let size = unsafe { mt.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
        let stride = unsafe { mt.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
            .ok()
            .and_then(|s| usize::try_from(s as i32).ok())
            .filter(|&s| s > 0);
        return Ok(Out {
            layout,
            swap_uv,
            width: (size >> 32) as usize,
            height: (size & 0xFFFF_FFFF) as usize,
            stride,
        });
    }
    Err("no output format the player can show".into())
}

impl Video {
    /// A decoded sample as RGBA at the output size.
    fn convert(&self, s: &IMFSample) -> Option<Vec<u8>> {
        let o = self.out;
        let bytes = if o.layout.depth == yuv::Depth::Eight {
            1
        } else {
            2
        };
        // SAFETY: the buffer is locked while its memory is read, within
        // the length it reports, and unlocked after.
        unsafe {
            let buf = s.ConvertToContiguousBuffer().ok()?;
            let len = buf.GetCurrentLength().ok()? as usize;
            let mut ptr = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None).ok()?;
            let result = (|| {
                if ptr.is_null() || len > MAX_OUTPUT {
                    return None;
                }
                let data = std::slice::from_raw_parts(ptr.cast_const(), len);
                // The buffer is laid out at the type's (aligned) frame size.
                let fw = if o.width > 0 { o.width } else { self.width };
                let fh = if o.height > 0 { o.height } else { self.height };
                let stride = o.stride.unwrap_or(fw * bytes).max(fw * bytes);
                let luma = stride.checked_mul(fh)?;
                let ch = fh.div_ceil(2);
                let (planes, strides): ([&[u8]; 3], [usize; 3]) = if o.layout.interleaved {
                    (
                        [data.get(..luma)?, data.get(luma..luma + stride * ch)?, &[]],
                        [stride, stride, 0],
                    )
                } else {
                    let cs = stride / 2;
                    let (a, b) = (
                        data.get(luma..luma + cs * ch)?,
                        data.get(luma + cs * ch..luma + 2 * cs * ch)?,
                    );
                    let (u, v) = if o.swap_uv { (b, a) } else { (a, b) };
                    ([data.get(..luma)?, u, v], [stride, cs, cs])
                };
                let g = self.geo;
                yuv::raw_to_rgba(
                    &Raw {
                        planes,
                        strides,
                        width: self.width.min(fw).max(1),
                        height: self.height.min(fh).max(1),
                        layout: o.layout,
                    },
                    g.rotation,
                    g.out_w,
                    g.out_h,
                    g.matrix,
                )
            })();
            let _ = buf.Unlock();
            result
        }
    }

    fn drain(&mut self, out: &mut Pictures, last: i64) -> Result<(), String> {
        for _ in 0..MAX_PULLS {
            match pull(self.mf, &self.t).map_err(|e| format!("{e}"))? {
                Pull::NeedInput => return Ok(()),
                Pull::FormatChanged => self.out = negotiate(&self.t)?,
                Pull::Sample(s) => {
                    let pending = self.pending.pop();
                    // SAFETY: attribute getter.
                    let t = unsafe { s.GetSampleTime() }
                        .ok()
                        .map(|t| t / 10_000)
                        .or(pending)
                        .unwrap_or(last);
                    if let Some(px) = self.convert(&s) {
                        out.push_back((t, px));
                    }
                }
            }
        }
        Ok(())
    }
}

impl VideoDecoder for Video {
    fn decode(&mut self, p: &Packet, out: &mut Pictures) -> Result<(), String> {
        let name = self.codec.name();
        if p.data.len() > MAX_PACKET {
            return Err(format!("{name}: packet too large"));
        }
        let sample = if self.fresh && !self.header.is_empty() {
            let mut data = self.header.clone();
            data.extend_from_slice(&p.data);
            self.mf.sample(&data, p.pts_ms, p.key)
        } else {
            self.mf.sample(&p.data, p.pts_ms, p.key)
        }
        .map_err(|e| format!("{name}: {e}"))?;
        self.fresh = false;
        self.pending.push(p.pts_ms);
        let t = self.t.clone();
        feed(&t, &sample, || self.drain(out, p.pts_ms)).map_err(|e| format!("{name}: {e}"))
    }

    fn flush(&mut self, out: &mut Pictures) {
        // SAFETY: plain notifications.
        unsafe {
            let _ = self.t.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.t.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        let _ = self.drain(out, 0);
    }

    fn reset(&mut self) -> Result<(), String> {
        // SAFETY: plain notifications.
        unsafe {
            let _ = self.t.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        start(&self.t);
        self.pending.clear();
        self.fresh = true;
        Ok(())
    }

    fn is_platform(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------
// AAC
// ---------------------------------------------------------------------

/// AAC through the Media Foundation AAC decoder (raw frames, the
/// AudioSpecificConfig in `MF_MT_USER_DATA`).
pub struct Aac {
    mf: &'static Mf,
    t: IMFTransform,
    channels: u16,
    rate: u32,
    float: bool,
}

/// A Media Foundation AAC decoder for raw frames described by `asc`.
pub fn aac(asc: &[u8]) -> Result<Box<dyn FrameDecoder>, Missing> {
    let adts = Adts::from_asc(asc).ok_or(Missing { hint: None })?;
    let mf = mf().ok_or_else(|| missing("AAC audio"))?;
    let t = mf
        .transform(
            MFT_CATEGORY_AUDIO_DECODER,
            MFMediaType_Audio,
            MFAudioFormat_AAC,
        )
        .ok_or_else(|| missing("AAC audio"))?;
    let failed = || Missing {
        hint: Some("The Windows AAC decoder could not start.".into()),
    };
    let setup = || -> windows::core::Result<(u16, u32, bool)> {
        let it = mf.media_type()?;
        // HEAACWAVEINFO after WAVEFORMATEX: payload type 0 (raw), profile
        // 0xFE (unspecified), struct type, reserved; then the config.
        let mut user = vec![0u8, 0, 0xFE, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        user.extend_from_slice(asc);
        // SAFETY: attribute setters, then the decoder's own types.
        unsafe {
            it.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            it.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            it.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, adts.rate())?;
            it.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(adts.channels()))?;
            it.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            it.SetBlob(&MF_MT_USER_DATA, &user)?;
            t.SetInputType(0, &it, 0)?;
            let mut chosen: Option<(IMFMediaType, bool)> = None;
            for i in 0..32 {
                let Ok(mt) = t.GetOutputAvailableType(0, i) else {
                    break;
                };
                let sub = mt.GetGUID(&MF_MT_SUBTYPE)?;
                let bits = mt.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).unwrap_or(0);
                let float = sub == MFAudioFormat_Float && bits == 32;
                if float || (sub == MFAudioFormat_PCM && bits == 16) {
                    let better = chosen.as_ref().is_none_or(|c| float && !c.1);
                    if better {
                        chosen = Some((mt, float));
                    }
                }
            }
            let Some((mt, float)) = chosen else {
                return Err(E_POINTER.into());
            };
            t.SetOutputType(0, &mt, 0)?;
            let ch = mt.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?;
            let rate = mt.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
            Ok((u16::try_from(ch).unwrap_or(0), rate, float))
        }
    };
    let (channels, rate, float) = setup().map_err(|_| failed())?;
    if !(1..=8).contains(&channels) || !(8_000..=192_000).contains(&rate) {
        return Err(failed());
    }
    start(&t);
    Ok(Box::new(Aac {
        mf,
        t,
        channels,
        rate,
        float,
    }))
}

impl Aac {
    fn take(&self, s: &IMFSample, out: &mut Vec<f32>) {
        // SAFETY: the buffer is read while locked, within its length.
        unsafe {
            let Ok(buf) = s.ConvertToContiguousBuffer() else {
                return;
            };
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            if buf.Lock(&mut ptr, None, Some(&mut len)).is_err() {
                return;
            }
            if !ptr.is_null() && (len as usize) <= MAX_OUTPUT {
                let data = std::slice::from_raw_parts(ptr.cast_const(), len as usize);
                if self.float {
                    out.extend(
                        data.chunks_exact(4)
                            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
                    );
                } else {
                    out.extend(
                        data.chunks_exact(2)
                            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0),
                    );
                }
            }
            let _ = buf.Unlock();
        }
    }
}

impl FrameDecoder for Aac {
    fn format(&self) -> (u16, u32) {
        (self.channels, self.rate)
    }

    fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        let sample = self
            .mf
            .sample(data, 0, true)
            .map_err(|e| format!("AAC: {e}"))?;
        let mut drain = || -> Result<(), String> {
            for _ in 0..MAX_PULLS {
                match pull(self.mf, &self.t).map_err(|e| format!("{e}"))? {
                    Pull::NeedInput => return Ok(()),
                    Pull::FormatChanged => return Err("the format changed".into()),
                    Pull::Sample(s) => self.take(&s, out),
                }
            }
            Ok(())
        };
        feed(&self.t, &sample, &mut drain).map_err(|e| format!("AAC: {e}"))
    }

    fn reset(&mut self) {
        // SAFETY: plain notifications.
        unsafe {
            let _ = self.t.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        start(&self.t);
    }
}
