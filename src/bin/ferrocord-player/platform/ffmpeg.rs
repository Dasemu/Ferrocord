//! The system FFmpeg libraries, opened with `dlopen` at run time.
//!
//! Nothing is linked at build time (`ldd ferrocord-player` shows no
//! libavcodec): `libavcodec.so.62` (FFmpeg 8) down to `.58` (FFmpeg 4) is
//! tried with its matching `libavutil`. The FFI surface is the decode API
//! that has kept its signature in all of them (`avcodec_send_packet` /
//! `avcodec_receive_frame`, FFmpeg 3.1+), plus AVOptions and name lookups.
//!
//! Struct fields are avoided where an accessor exists. The few read or
//! written directly are in the prefix of `AVPacket` and `AVFrame` that has
//! not moved in any of these versions (checked against the headers of
//! FFmpeg 4.4, 5.1, 6.1, 7.1 and 8.0):
//!
//! | field | offset |
//! |---|---|
//! | `AVPacket.pts` / `.dts` / `.data` / `.size` | 8 / 16 / 24 / 32 |
//! | `AVFrame.data[8]` / `.linesize[8]` / `.extended_data` | 0 / 64 / 96 |
//! | `AVFrame.width` / `.height` / `.nb_samples` / `.format` | 104 / 108 / 112 / 116 |
//!
//! Enum values that changed between versions (pixel formats, codec ids) are
//! never used: decoders are found by name and pixel / sample formats are
//! read back as names. The frame timestamp (`AVFrame.pts` moved) is not
//! read: decoders output in presentation order, so each picture takes the
//! smallest pending packet time, as with OpenH264.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use libloading::Library;

use super::super::audio::FrameDecoder;
use super::super::codec::{Geometry, PendingTimes, Pictures, VideoDecoder};
use super::super::source::{Codec, Packet, VideoInfo};
use super::super::yuv::{self, Raw};
use super::{Missing, ffmpeg_layout, install_hint, key};

/// `(libavcodec, libavutil)` major versions tried, newest first.
const VERSIONS: [(u32, u32); 5] = [(62, 60), (61, 59), (60, 58), (59, 57), (58, 56)];
/// `AVERROR(EAGAIN)` on Linux.
const EAGAIN: c_int = -11;
/// `AVERROR_EOF`.
const AVERROR_EOF: c_int = -0x2046_4F45;
/// `AV_LOG_QUIET`: FFmpeg must not write to the helper's stderr (the app
/// reports its last line when the helper fails to start).
const AV_LOG_QUIET: c_int = -8;
/// `AV_NOPTS_VALUE`.
const NOPTS: i64 = i64::MIN;
/// Decoder threads: enough for 4K H.265 in real time, few enough that the
/// frame buffers each thread holds stay small.
const THREADS: &str = "4";
/// Largest compressed packet handed to FFmpeg.
const MAX_PACKET: usize = 64 * 1024 * 1024;

// Offsets of the stable struct prefixes (see the module docs).
const PKT_PTS: usize = 8;
const PKT_DTS: usize = 16;
const PKT_DATA: usize = 24;
const FRAME_LINESIZE: usize = 64;
const FRAME_EXTENDED: usize = 96;
const FRAME_WIDTH: usize = 104;
const FRAME_HEIGHT: usize = 108;
const FRAME_SAMPLES: usize = 112;
const FRAME_FORMAT: usize = 116;

/// `AVChannelLayout` (FFmpeg 5.1+, public and fixed in size).
#[repr(C)]
struct ChannelLayout {
    order: c_int,
    nb_channels: c_int,
    mask: u64,
    opaque: *mut c_void,
}

type Ptr = *mut c_void;

/// The functions used, resolved once.
struct Fns {
    find_decoder_by_name: unsafe extern "C" fn(*const c_char) -> *const c_void,
    alloc_context3: unsafe extern "C" fn(*const c_void) -> Ptr,
    open2: unsafe extern "C" fn(Ptr, *const c_void, *mut Ptr) -> c_int,
    send_packet: unsafe extern "C" fn(Ptr, *const c_void) -> c_int,
    receive_frame: unsafe extern "C" fn(Ptr, Ptr) -> c_int,
    flush_buffers: unsafe extern "C" fn(Ptr),
    free_context: unsafe extern "C" fn(*mut Ptr),
    packet_alloc: unsafe extern "C" fn() -> Ptr,
    packet_free: unsafe extern "C" fn(*mut Ptr),
    new_packet: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    packet_unref: unsafe extern "C" fn(Ptr),
    frame_alloc: unsafe extern "C" fn() -> Ptr,
    frame_free: unsafe extern "C" fn(*mut Ptr),
    frame_unref: unsafe extern "C" fn(Ptr),
    pix_fmt_name: unsafe extern "C" fn(c_int) -> *const c_char,
    sample_fmt_name: unsafe extern "C" fn(c_int) -> *const c_char,
    opt_set: unsafe extern "C" fn(Ptr, *const c_char, *const c_char, c_int) -> c_int,
    opt_get_int: unsafe extern "C" fn(Ptr, *const c_char, c_int, *mut i64) -> c_int,
    opt_get: unsafe extern "C" fn(Ptr, *const c_char, c_int, *mut *mut u8) -> c_int,
    free: unsafe extern "C" fn(Ptr),
    /// FFmpeg 5.1+.
    layout_from_string: Option<unsafe extern "C" fn(*mut ChannelLayout, *const c_char) -> c_int>,
    /// FFmpeg 5.1+.
    layout_uninit: Option<unsafe extern "C" fn(*mut ChannelLayout)>,
}

/// The loaded libraries (kept open for the life of the process).
struct Lib {
    fns: Fns,
    /// `libavcodec.so.N`.
    name: String,
    /// `major.minor.micro`.
    version: String,
    _avcodec: Library,
    _avutil: Library,
}

static LIB: OnceLock<Option<Lib>> = OnceLock::new();

/// The system FFmpeg, loaded on first use.
fn lib() -> Option<&'static Lib> {
    LIB.get_or_init(load).as_ref()
}

fn load() -> Option<Lib> {
    VERSIONS.iter().find_map(|&(c, u)| load_version(c, u))
}

fn load_version(codec_major: u32, util_major: u32) -> Option<Lib> {
    // SAFETY: loading a shared library runs its initialisers; libavcodec and
    // libavutil have no side effects on load beyond their own globals.
    let avutil = unsafe { Library::new(format!("libavutil.so.{util_major}")) }.ok()?;
    let avcodec = unsafe { Library::new(format!("libavcodec.so.{codec_major}")) }.ok()?;
    macro_rules! sym {
        ($lib:expr, $name:literal) => {
            // SAFETY: the symbol has this C signature in every supported
            // version (see the module docs); the library outlives the copy.
            unsafe { *$lib.get(concat!($name, "\0").as_bytes()).ok()? }
        };
    }
    macro_rules! opt_sym {
        ($lib:expr, $name:literal) => {
            // SAFETY: as above; absent in older versions.
            unsafe { $lib.get(concat!($name, "\0").as_bytes()).ok().map(|s| *s) }
        };
    }
    let version: unsafe extern "C" fn() -> c_uint = sym!(avcodec, "avcodec_version");
    // SAFETY: no arguments, returns a number.
    let v = unsafe { version() };
    if v >> 16 != codec_major {
        return None;
    }
    let log_level: unsafe extern "C" fn(c_int) = sym!(avutil, "av_log_set_level");
    // SAFETY: sets a global integer.
    unsafe { log_level(AV_LOG_QUIET) };
    let fns = Fns {
        find_decoder_by_name: sym!(avcodec, "avcodec_find_decoder_by_name"),
        alloc_context3: sym!(avcodec, "avcodec_alloc_context3"),
        open2: sym!(avcodec, "avcodec_open2"),
        send_packet: sym!(avcodec, "avcodec_send_packet"),
        receive_frame: sym!(avcodec, "avcodec_receive_frame"),
        flush_buffers: sym!(avcodec, "avcodec_flush_buffers"),
        free_context: sym!(avcodec, "avcodec_free_context"),
        packet_alloc: sym!(avcodec, "av_packet_alloc"),
        packet_free: sym!(avcodec, "av_packet_free"),
        new_packet: sym!(avcodec, "av_new_packet"),
        packet_unref: sym!(avcodec, "av_packet_unref"),
        frame_alloc: sym!(avutil, "av_frame_alloc"),
        frame_free: sym!(avutil, "av_frame_free"),
        frame_unref: sym!(avutil, "av_frame_unref"),
        pix_fmt_name: sym!(avutil, "av_get_pix_fmt_name"),
        sample_fmt_name: sym!(avutil, "av_get_sample_fmt_name"),
        opt_set: sym!(avutil, "av_opt_set"),
        opt_get_int: sym!(avutil, "av_opt_get_int"),
        opt_get: sym!(avutil, "av_opt_get"),
        free: sym!(avutil, "av_free"),
        layout_from_string: opt_sym!(avutil, "av_channel_layout_from_string"),
        layout_uninit: opt_sym!(avutil, "av_channel_layout_uninit"),
    };
    Some(Lib {
        fns,
        name: format!("libavcodec.so.{codec_major}"),
        version: format!("{}.{}.{}", v >> 16, (v >> 8) & 0xFF, v & 0xFF),
        _avcodec: avcodec,
        _avutil: avutil,
    })
}

/// Decoder names tried for a codec, best first. AV1 needs libdav1d or
/// libaom: FFmpeg's own `av1` decoder only drives hardware.
fn decoder_names(codec: Codec) -> &'static [&'static str] {
    match codec {
        Codec::H264 => &["h264"],
        Codec::Hevc => &["hevc"],
        Codec::Av1 => &["libdav1d", "libaom-av1"],
        Codec::Vp9 => &["vp9", "libvpx-vp9"],
        Codec::Vp8 => &["vp8", "libvpx"],
    }
}

const AAC_NAMES: &[&str] = &["aac", "libfdk_aac"];

/// The first decoder of `names` the library has.
fn find(lib: &Lib, names: &[&'static str]) -> Option<(*const c_void, &'static str)> {
    names.iter().find_map(|&n| {
        let c = CString::new(n).ok()?;
        // SAFETY: a NUL-terminated name; returns a static descriptor or null.
        let d = unsafe { (lib.fns.find_decoder_by_name)(c.as_ptr()) };
        (!d.is_null()).then_some((d, n))
    })
}

/// Why `what` has no decoder here.
fn missing(what: &str, lib: Option<&Lib>) -> Missing {
    Missing {
        hint: install_hint(what, lib.map(|l| l.name.as_str())),
    }
}

/// `--backends` lines.
pub fn report() -> Vec<String> {
    let Some(lib) = lib() else {
        return vec!["platform none (no libavcodec.so.58-62 found)".into()];
    };
    let mut out = vec![format!("platform ffmpeg {} {}", lib.name, lib.version)];
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1, Codec::Vp9] {
        let name = find(lib, decoder_names(codec)).map_or("-", |(_, n)| n);
        out.push(format!("{} {name}", key(codec)));
    }
    out.push(format!(
        "aac {}",
        find(lib, AAC_NAMES).map_or("-", |(_, n)| n)
    ));
    out
}

/// The hint for `what` on this system.
pub fn hint(what: &str, lib: Option<&str>) -> Option<String> {
    Some(super::linux_hint(what, lib, &super::os_release_ids()))
}

/// One open decoder with its packet and frame.
struct Ctx {
    lib: &'static Lib,
    ctx: Ptr,
    pkt: Ptr,
    frame: Ptr,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let f = &self.lib.fns;
        // SAFETY: each pointer came from its allocator and is freed once
        // (the free functions take the address and null it).
        unsafe {
            (f.frame_free)(&mut self.frame);
            (f.packet_free)(&mut self.pkt);
            (f.free_context)(&mut self.ctx);
        }
    }
}

impl Ctx {
    /// Opens the first of `names` the library has.
    fn open(what: &str, names: &[&'static str]) -> Result<Self, Missing> {
        let lib = lib().ok_or_else(|| missing(what, None))?;
        let (codec, _) = find(lib, names).ok_or_else(|| missing(what, Some(lib)))?;
        let f = &lib.fns;
        // SAFETY: plain allocations; each is checked and owned by `Ctx`,
        // whose `Drop` frees what was allocated (null pointers are fine).
        let c = unsafe {
            Ctx {
                lib,
                ctx: (f.alloc_context3)(codec),
                pkt: (f.packet_alloc)(),
                frame: (f.frame_alloc)(),
            }
        };
        let failed = || Missing {
            hint: Some(format!(
                "The system FFmpeg ({}) could not start its {what} decoder.",
                lib.name
            )),
        };
        if c.ctx.is_null() || c.pkt.is_null() || c.frame.is_null() {
            return Err(failed());
        }
        let (k, v) = (c"threads", CString::new(THREADS).map_err(|_| failed())?);
        // SAFETY: a valid context and NUL-terminated strings; a missing
        // option is only an error code.
        unsafe { (f.opt_set)(c.ctx, k.as_ptr(), v.as_ptr(), 0) };
        // SAFETY: the context was allocated for this codec; no options.
        if unsafe { (f.open2)(c.ctx, codec, std::ptr::null_mut()) } < 0 {
            return Err(failed());
        }
        Ok(c)
    }

    /// Sends one packet. `Ok(false)`: the decoder is full, receive first.
    fn send(&mut self, data: &[u8], pts: i64) -> Result<bool, String> {
        if data.len() > MAX_PACKET {
            return Err("packet too large".into());
        }
        let f = &self.lib.fns;
        let len = c_int::try_from(data.len()).map_err(|_| "packet too large".to_string())?;
        // SAFETY: `av_new_packet` allocates `len` bytes (plus padding) and
        // sets `data` / `size`; the copy stays within them. The packet is
        // unreferenced after the send, which takes its own reference.
        unsafe {
            if (f.new_packet)(self.pkt, len) < 0 {
                return Err("out of memory".into());
            }
            let base = self.pkt.cast::<u8>();
            let dst = base.add(PKT_DATA).cast::<*mut u8>().read();
            if dst.is_null() {
                (f.packet_unref)(self.pkt);
                return Err("out of memory".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
            base.add(PKT_PTS).cast::<i64>().write_unaligned(pts);
            base.add(PKT_DTS).cast::<i64>().write_unaligned(NOPTS);
            let r = (f.send_packet)(self.ctx, self.pkt);
            (f.packet_unref)(self.pkt);
            match r {
                0 => Ok(true),
                EAGAIN => Ok(false),
                e => Err(format!("error {e}")),
            }
        }
    }

    /// End of stream: the decoder releases what it holds back.
    fn send_eof(&mut self) {
        // SAFETY: a null packet is the documented drain signal.
        unsafe { (self.lib.fns.send_packet)(self.ctx, std::ptr::null()) };
    }

    /// The next decoded frame into `self.frame`; `Ok(false)` when there is
    /// none yet (or no more).
    fn receive(&mut self) -> Result<bool, String> {
        // SAFETY: a valid context and frame (unreferenced first).
        let r = unsafe {
            (self.lib.fns.frame_unref)(self.frame);
            (self.lib.fns.receive_frame)(self.ctx, self.frame)
        };
        match r {
            0 => Ok(true),
            EAGAIN | AVERROR_EOF => Ok(false),
            e => Err(format!("error {e}")),
        }
    }

    /// Drops everything in flight (seek) and leaves the end-of-stream state.
    fn flush(&mut self) {
        // SAFETY: a valid, open context.
        unsafe { (self.lib.fns.flush_buffers)(self.ctx) };
    }

    /// An `i32` field of the frame at `offset`.
    fn frame_int(&self, offset: usize) -> i32 {
        // SAFETY: one of the stable prefix offsets of a live AVFrame.
        unsafe { self.frame.cast::<u8>().add(offset).cast::<i32>().read() }
    }

    /// `data[i]` / `linesize[i]` of the frame.
    fn plane(&self, i: usize) -> (*const u8, i32) {
        // SAFETY: `i < 8`; both arrays are in the stable prefix.
        unsafe {
            let base = self.frame.cast::<u8>();
            (
                base.cast::<*const u8>().add(i).read(),
                base.add(FRAME_LINESIZE).cast::<i32>().add(i).read(),
            )
        }
    }

    /// A string AVOption of the codec context.
    fn opt_string(&self, name: &CStr) -> Option<String> {
        let mut out: *mut u8 = std::ptr::null_mut();
        // SAFETY: on success `out` is an av_malloc'ed C string we free.
        unsafe {
            if (self.lib.fns.opt_get)(self.ctx, name.as_ptr(), 0, &mut out) < 0 || out.is_null() {
                return None;
            }
            let s = CStr::from_ptr(out.cast()).to_string_lossy().into_owned();
            (self.lib.fns.free)(out.cast());
            Some(s)
        }
    }

    /// An integer AVOption of the codec context.
    fn opt_int(&self, name: &CStr) -> Option<i64> {
        let mut v = 0i64;
        // SAFETY: writes one integer on success.
        let r = unsafe { (self.lib.fns.opt_get_int)(self.ctx, name.as_ptr(), 0, &mut v) };
        (r >= 0).then_some(v)
    }

    /// Channel count of the decoded audio: `ch_layout` (FFmpeg 5.1+), else
    /// the older `ac` option.
    fn channels(&self) -> Option<u16> {
        let from_layout = || -> Option<i32> {
            let (parse, uninit) = (
                self.lib.fns.layout_from_string?,
                self.lib.fns.layout_uninit?,
            );
            let desc = CString::new(self.opt_string(c"ch_layout")?).ok()?;
            let mut l = ChannelLayout {
                order: 0,
                nb_channels: 0,
                mask: 0,
                opaque: std::ptr::null_mut(),
            };
            // SAFETY: fills a caller-owned AVChannelLayout, released after.
            unsafe {
                let ok = parse(&mut l, desc.as_ptr()) >= 0;
                let n = l.nb_channels;
                uninit(&mut l);
                ok.then_some(n)
            }
        };
        let n =
            from_layout().or_else(|| self.opt_int(c"ac").and_then(|v| i32::try_from(v).ok()))?;
        u16::try_from(n).ok().filter(|n| (1..=8).contains(n))
    }
}

/// Name of a format through one of the `av_get_*_fmt_name` functions.
fn fmt_name(f: unsafe extern "C" fn(c_int) -> *const c_char, v: c_int) -> Option<String> {
    // SAFETY: returns a static string or null for unknown values.
    let p = unsafe { f(v) };
    // SAFETY: non-null: a static NUL-terminated name.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------

/// A video stream through the system FFmpeg.
pub struct Video {
    ctx: Ctx,
    codec: Codec,
    header: Vec<u8>,
    /// The next packet goes in after a (re)start: send the header first.
    fresh: bool,
    pending: PendingTimes,
    geo: Geometry,
    buf: Vec<u8>,
    /// A picture format the helper does not convert, named.
    odd_format: Option<String>,
}

/// An FFmpeg video decoder for `info`.
pub fn video(info: &VideoInfo, geo: Geometry) -> Result<Box<dyn VideoDecoder>, Missing> {
    let ctx = Ctx::open(info.codec.name(), decoder_names(info.codec))?;
    Ok(Box::new(Video {
        ctx,
        codec: info.codec,
        header: info.header.clone(),
        fresh: true,
        pending: PendingTimes::default(),
        geo,
        buf: Vec::new(),
        odd_format: None,
    }))
}

impl Video {
    /// The frame in `ctx.frame` as RGBA at the output size.
    fn convert(&mut self) -> Option<Vec<u8>> {
        let c = &self.ctx;
        let (w, h) = (c.frame_int(FRAME_WIDTH), c.frame_int(FRAME_HEIGHT));
        let (w, h) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
        let name = fmt_name(c.lib.fns.pix_fmt_name, c.frame_int(FRAME_FORMAT))?;
        let Some(layout) = ffmpeg_layout(&name) else {
            self.odd_format = Some(name);
            return None;
        };
        let bytes = if layout.depth == yuv::Depth::Eight {
            1
        } else {
            2
        };
        let ch = (h + (1 << layout.sub_y) - 1) >> layout.sub_y;
        let mut planes: [&[u8]; 3] = [&[]; 3];
        let mut strides = [0usize; 3];
        let count = if layout.interleaved { 2 } else { 3 };
        for i in 0..count {
            let (ptr, stride) = c.plane(i);
            let stride = usize::try_from(stride).ok().filter(|&s| s > 0)?;
            if ptr.is_null() {
                return None;
            }
            let rows = if i == 0 { h } else { ch };
            let len = stride.checked_mul(rows)?;
            if len > 512 * 1024 * 1024 || stride < bytes {
                return None;
            }
            // SAFETY: FFmpeg allocates each plane as `linesize * rows`
            // bytes (plus padding), alive until the frame is unreferenced.
            planes[i] = unsafe { std::slice::from_raw_parts(ptr, len) };
            strides[i] = stride;
        }
        let g = self.geo;
        yuv::raw_to_rgba(
            &Raw {
                planes,
                strides,
                width: w,
                height: h,
                layout,
            },
            g.rotation,
            g.out_w,
            g.out_h,
            g.matrix,
        )
    }

    fn drain(&mut self, out: &mut Pictures, last: i64) -> Result<(), String> {
        while self.ctx.receive()? {
            let t = self.pending.pop().unwrap_or(last);
            if let Some(px) = self.convert() {
                out.push_back((t, px));
            }
        }
        Ok(())
    }
}

impl VideoDecoder for Video {
    fn decode(&mut self, p: &Packet, out: &mut Pictures) -> Result<(), String> {
        let name = self.codec.name();
        // Parameter sets / sequence header in band, before the first
        // picture after a (re)start.
        let with_header = self.fresh && !self.header.is_empty();
        if with_header {
            self.buf.clear();
            self.buf.extend_from_slice(&self.header);
            self.buf.extend_from_slice(&p.data);
        }
        self.fresh = false;
        self.pending.push(p.pts_ms);
        for _ in 0..8 {
            let data: &[u8] = if with_header { &self.buf } else { &p.data };
            if self
                .ctx
                .send(data, p.pts_ms)
                .map_err(|e| format!("{name}: {e}"))?
            {
                return self
                    .drain(out, p.pts_ms)
                    .map_err(|e| format!("{name}: {e}"));
            }
            // Full: take pictures out, then send again.
            self.drain(out, p.pts_ms)
                .map_err(|e| format!("{name}: {e}"))?;
        }
        Err(format!("{name}: the decoder accepts no more data"))
    }

    fn flush(&mut self, out: &mut Pictures) {
        self.ctx.send_eof();
        for _ in 0..64 {
            match self.ctx.receive() {
                Ok(true) => {
                    if let (Some(t), Some(px)) = (self.pending.pop(), self.convert()) {
                        out.push_back((t, px));
                    }
                }
                _ => break,
            }
        }
    }

    fn reset(&mut self) -> Result<(), String> {
        self.ctx.flush();
        self.pending.clear();
        self.fresh = true;
        Ok(())
    }

    fn is_platform(&self) -> bool {
        true
    }

    fn no_picture_reason(&self) -> Option<String> {
        self.odd_format.as_ref().map(|f| {
            format!(
                "the {} pictures are in a format the player does not show ({f})",
                self.codec.name()
            )
        })
    }
}

// ---------------------------------------------------------------------
// AAC
// ---------------------------------------------------------------------

/// AAC through the system FFmpeg. Raw frames go in with an ADTS header
/// built from the AudioSpecificConfig, so the decoder needs no extradata
/// (whose struct field moved between versions).
pub struct Aac {
    ctx: Ctx,
    adts: super::super::audio::Adts,
    channels: u16,
    rate: u32,
    buf: Vec<u8>,
}

/// An FFmpeg AAC decoder for raw frames described by `asc`.
pub fn aac(asc: &[u8]) -> Result<Box<dyn FrameDecoder>, Missing> {
    let adts = super::super::audio::Adts::from_asc(asc).ok_or(Missing { hint: None })?;
    let ctx = Ctx::open("AAC audio", AAC_NAMES)?;
    Ok(Box::new(Aac {
        ctx,
        channels: adts.channels(),
        rate: adts.rate(),
        adts,
        buf: Vec::new(),
    }))
}

impl Aac {
    /// Appends the frame in `ctx.frame` as interleaved `f32`.
    fn take(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        let c = &self.ctx;
        if let Some(ch) = c.channels() {
            self.channels = ch;
        }
        if let Some(r) = c
            .opt_int(c"ar")
            .and_then(|r| u32::try_from(r).ok())
            .filter(|r| (8_000..=192_000).contains(r))
        {
            self.rate = r;
        }
        let n = usize::try_from(c.frame_int(FRAME_SAMPLES))
            .unwrap_or(0)
            .min(16_384);
        let ch = usize::from(self.channels);
        let fmt =
            fmt_name(c.lib.fns.sample_fmt_name, c.frame_int(FRAME_FORMAT)).unwrap_or_default();
        let (planar, kind) = match fmt.as_str() {
            "fltp" => (true, 'f'),
            "flt" => (false, 'f'),
            "s16p" => (true, 's'),
            "s16" => (false, 's'),
            "s32p" => (true, 'i'),
            "s32" => (false, 'i'),
            other => return Err(format!("AAC: unexpected sample format {other}")),
        };
        let size = if kind == 's' { 2 } else { 4 };
        // SAFETY: `extended_data` points to one plane per channel (planar)
        // or one interleaved plane, each `nb_samples` samples long.
        unsafe {
            let ext = c
                .frame
                .cast::<u8>()
                .add(FRAME_EXTENDED)
                .cast::<*const *const u8>()
                .read();
            if ext.is_null() {
                return Err("AAC: empty frame".into());
            }
            let planes = if planar { ch } else { 1 };
            let per_plane = if planar { n } else { n * ch };
            let mut ptrs = Vec::with_capacity(planes);
            for i in 0..planes {
                let p = ext.add(i).read();
                if p.is_null() {
                    return Err("AAC: missing channel".into());
                }
                ptrs.push(std::slice::from_raw_parts(p, per_plane * size));
            }
            let read = |b: &[u8], i: usize| -> f32 {
                let s = &b[i * size..(i + 1) * size];
                match kind {
                    'f' => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    's' => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
                    _ => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                }
            };
            out.reserve(n * ch);
            for i in 0..n {
                for c in 0..ch {
                    out.push(if planar {
                        read(ptrs[c], i)
                    } else {
                        read(ptrs[0], i * ch + c)
                    });
                }
            }
        }
        Ok(())
    }
}

impl FrameDecoder for Aac {
    fn format(&self) -> (u16, u32) {
        (self.channels, self.rate)
    }

    fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        self.buf.clear();
        self.buf
            .extend_from_slice(&self.adts.header(data.len()).ok_or("AAC frame too large")?);
        self.buf.extend_from_slice(data);
        let buf = std::mem::take(&mut self.buf);
        let sent = self.ctx.send(&buf, NOPTS);
        self.buf = buf;
        if !sent.map_err(|e| format!("AAC: {e}"))? {
            return Err("AAC: the decoder accepts no more data".into());
        }
        while self.ctx.receive().map_err(|e| format!("AAC: {e}"))? {
            self.take(out)?;
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.ctx.flush();
    }
}
