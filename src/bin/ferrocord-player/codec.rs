//! Video decoders behind one interface: H.264 (OpenH264, built from source
//! and linked statically), VP8 and VP9 (pure Rust), and the platform
//! decoders ([`super::platform`]: system FFmpeg on Linux, Media Foundation
//! on Windows) for H.265, AV1 and the H.264 streams OpenH264 refuses. Every
//! decoder returns pictures already converted to RGBA at the output size,
//! tagged with their presentation time.
//!
//! **Choice.** The built-in decoder stays the default for what it handles
//! well (8-bit 4:2:0 progressive H.264, VP8, VP9), so playback does not
//! change where it already worked. The platform decoder takes H.265, AV1
//! and H.264 whose SPS OpenH264 refuses (High 10, 4:2:2, 4:4:4,
//! interlaced), and is the fallback when the built-in decoder gives up at
//! run time ([`fallback`]: an SPS refused in band, no IDR picture, a stream
//! of undecodable packets).
//!
//! This module has no dependency on the rest of the helper beyond the
//! packet and YUV types, so another binary that needs to decode video (for
//! instance a call's video stream) can reuse it as is.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

use super::h264::{self, Sps};
use super::platform;
use super::source::{Codec, Packet, VideoInfo};
use super::yuv::{self, Matrix, Planes, Rotation};

/// Decoded pictures: presentation time in ms and RGBA pixels.
pub type Pictures = VecDeque<(i64, Vec<u8>)>;

/// Output geometry of the converted pictures.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    /// Rotation to apply.
    pub rotation: Rotation,
    /// Output width.
    pub out_w: usize,
    /// Output height.
    pub out_h: usize,
    /// Colour matrix.
    pub matrix: Matrix,
}

/// A video decoder.
pub trait VideoDecoder {
    /// Feeds one packet; zero or more pictures are appended to `out`.
    /// `Err` for a packet that could not be decoded (the stream goes on).
    fn decode(&mut self, p: &Packet, out: &mut Pictures) -> Result<(), String>;
    /// End of stream: appends the pictures still held back.
    fn flush(&mut self, out: &mut Pictures);
    /// Starts over, as for a fresh stream (after a seek).
    fn reset(&mut self) -> Result<(), String>;
    /// The stream is in a format this decoder cannot handle at all (named
    /// for the user), as far as the headers seen so far tell.
    fn unsupported(&self) -> Option<String> {
        None
    }
    /// Why a stream that went through the decoder gave no picture, when
    /// the decoder can tell.
    fn no_picture_reason(&self) -> Option<String> {
        None
    }
    /// A platform decoder (it has no fallback behind it).
    fn is_platform(&self) -> bool {
        false
    }
}

/// What the built-in decoders say about `info` before decoding: the
/// codec or H.264 variant they cannot handle, if any.
pub fn native_refusal(info: &VideoInfo) -> Option<String> {
    match info.codec {
        Codec::H264 => refused_sps(&info.header),
        Codec::Hevc | Codec::Av1 => Some(info.codec.name().to_string()),
        Codec::Vp8 | Codec::Vp9 => None,
    }
}

/// Why a stream cannot be played here, for the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// A codec the player cannot decode (as opposed to a broken stream).
    pub missing: bool,
    /// The codec, then (after a newline) what to install, if anything.
    pub text: String,
}

impl Refusal {
    /// `what` cannot be decoded; `hint` says how to change that.
    pub fn missing(what: &str, hint: Option<String>) -> Self {
        Refusal {
            missing: true,
            text: missing_text(what, hint),
        }
    }
}

/// The text of a "cannot decode" failure: the codec, then on a second
/// line what the user can install (the app shows both).
pub fn missing_text(what: &str, hint: Option<String>) -> String {
    match hint {
        Some(h) => format!("{what}\n{h}"),
        None => what.to_string(),
    }
}

/// The decoder for `info.codec`: built-in where it handles the stream,
/// the platform decoder for the rest.
pub fn new_decoder(info: &VideoInfo, geo: Geometry) -> Result<Box<dyn VideoDecoder>, Refusal> {
    let broken = |e: String| Refusal {
        missing: false,
        text: e,
    };
    match info.codec {
        Codec::H264 => {
            let refused = refused_sps(&info.header);
            if refused.is_some() || platform::preferred() {
                match platform::video(info, geo) {
                    Ok(d) => return Ok(d),
                    Err(m) => {
                        if let Some(why) = refused {
                            return Err(Refusal::missing(&why, m.hint));
                        }
                    }
                }
            }
            Ok(Box::new(
                H264::new(info.header.clone(), geo).map_err(broken)?,
            ))
        }
        Codec::Hevc | Codec::Av1 => {
            platform::video(info, geo).map_err(|m| Refusal::missing(info.codec.name(), m.hint))
        }
        Codec::Vp8 | Codec::Vp9 if platform::preferred() => match platform::video(info, geo) {
            Ok(d) => Ok(d),
            Err(_) => Ok(native_vp(info.codec, geo)),
        },
        Codec::Vp8 | Codec::Vp9 => Ok(native_vp(info.codec, geo)),
    }
}

fn native_vp(codec: Codec, geo: Geometry) -> Box<dyn VideoDecoder> {
    if codec == Codec::Vp8 {
        Box::new(Vp8::new(geo))
    } else {
        Box::new(Vp9::new(geo))
    }
}

/// The platform decoder for a stream the built-in decoder gave up on.
/// `Err` carries what to install (if anything would help).
pub fn fallback(info: &VideoInfo, geo: Geometry) -> Result<Box<dyn VideoDecoder>, Option<String>> {
    platform::video(info, geo).map_err(|m| m.hint)
}

/// Presentation times of packets handed to a decoder whose pictures have
/// not come out yet. H.264 decoders output in presentation order, so each
/// picture takes the smallest pending time.
#[derive(Debug, Default)]
pub struct PendingTimes(BinaryHeap<Reverse<i64>>);

impl PendingTimes {
    /// A packet went into the decoder.
    pub fn push(&mut self, pts: i64) {
        self.0.push(Reverse(pts));
    }
    /// A picture came out.
    pub fn pop(&mut self) -> Option<i64> {
        self.0.pop().map(|Reverse(t)| t)
    }
    /// Forget everything (after a seek).
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

// ---------------------------------------------------------------------
// H.264
// ---------------------------------------------------------------------

use openh264::decoder::{DecodedYUV, Decoder as H264Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;

/// H.264 through OpenH264.
pub struct H264 {
    decoder: H264Decoder,
    header: Vec<u8>,
    pending: PendingTimes,
    geo: Geometry,
    /// The first parameter set OpenH264 cannot decode, from the container
    /// header or in band.
    refused: Option<String>,
    /// An IDR picture went into the decoder since the last reset.
    saw_idr: bool,
}

/// The first SPS in `annexb` that OpenH264 refuses, named.
fn refused_sps(annexb: &[u8]) -> Option<String> {
    h264::sps_units(annexb).find_map(|s: Sps| s.unsupported())
}

fn new_h264() -> Result<H264Decoder, String> {
    // B-frames (Main / High profile) need the decoder's reorder buffer:
    // flushing after each sample breaks them.
    H264Decoder::with_api_config(
        openh264::OpenH264API::from_source(),
        DecoderConfig::new().flush_after_decode(Flush::NoFlush),
    )
    .map_err(|e| format!("the H.264 decoder could not start: {e}"))
}

fn convert_h264(g: Geometry, pic: &DecodedYUV<'_>) -> Option<Vec<u8>> {
    let (w, h) = pic.dimensions();
    let (ys, us, vs) = pic.strides();
    yuv::to_rgba(
        &Planes {
            y: pic.y(),
            u: pic.u(),
            v: pic.v(),
            strides: (ys, us, vs),
            width: w,
            height: h,
            sub_x: 1,
            sub_y: 1,
        },
        g.rotation,
        g.out_w,
        g.out_h,
        g.matrix,
    )
}

impl H264 {
    fn new(header: Vec<u8>, geo: Geometry) -> Result<Self, String> {
        let mut decoder = new_h264()?;
        let _ = decoder.decode(&header);
        Ok(Self {
            decoder,
            refused: refused_sps(&header),
            header,
            pending: PendingTimes::default(),
            geo,
            saw_idr: false,
        })
    }
}

impl VideoDecoder for H264 {
    fn decode(&mut self, p: &Packet, out: &mut Pictures) -> Result<(), String> {
        if p.key {
            if self.refused.is_none() {
                // Parameter sets sent in band (`avc3`, raw streams).
                self.refused = refused_sps(&p.data);
            }
            self.saw_idr = self.saw_idr || h264::has_idr(&p.data);
        }
        self.pending.push(p.pts_ms);
        match self.decoder.decode(&p.data) {
            Ok(Some(pic)) => {
                if let Some(px) = convert_h264(self.geo, &pic) {
                    let t = self.pending.pop().unwrap_or(p.pts_ms);
                    out.push_back((t, px));
                }
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => Err(format!("H.264: {e}")),
        }
    }

    fn flush(&mut self, out: &mut Pictures) {
        // The reorder buffer still holds the last pictures (a handful at most).
        let pics = self.decoder.flush_remaining().unwrap_or_default();
        for pic in pics.iter().take(16) {
            if let (Some(t), Some(px)) = (self.pending.pop(), convert_h264(self.geo, pic)) {
                out.push_back((t, px));
            }
        }
    }

    fn reset(&mut self) -> Result<(), String> {
        self.decoder = new_h264()?;
        let _ = self.decoder.decode(&self.header);
        self.pending.clear();
        self.saw_idr = false;
        Ok(())
    }

    fn unsupported(&self) -> Option<String> {
        self.refused.clone()
    }

    fn no_picture_reason(&self) -> Option<String> {
        // OpenH264 (with concealment off, as the crate configures it, and
        // also with its concealment on) shows nothing until an IDR picture:
        // a stream whose keyframes are all recovery-point I-frames (an
        // open-GOP clip cut without re-encoding) never shows a picture.
        (!self.saw_idr).then(|| {
            "the H.264 stream has no IDR keyframe to start from (it was probably cut \
             without re-encoding)"
                .into()
        })
    }
}

// ---------------------------------------------------------------------
// VP8
// ---------------------------------------------------------------------

/// VP8 (pure Rust).
pub struct Vp8 {
    decoder: vp8_decoder::Decoder,
    geo: Geometry,
}

impl Vp8 {
    fn new(geo: Geometry) -> Self {
        Self {
            decoder: vp8_decoder::Decoder::new(),
            geo,
        }
    }
}

impl VideoDecoder for Vp8 {
    fn decode(&mut self, p: &Packet, out: &mut Pictures) -> Result<(), String> {
        let g = self.geo;
        match self.decoder.decode(&p.data) {
            Ok(Some(pic)) => {
                let px = yuv::to_rgba(
                    &Planes {
                        y: pic.y,
                        u: pic.u,
                        v: pic.v,
                        strides: (pic.y_stride, pic.uv_stride, pic.uv_stride),
                        width: pic.width as usize,
                        height: pic.height as usize,
                        sub_x: 1,
                        sub_y: 1,
                    },
                    g.rotation,
                    g.out_w,
                    g.out_h,
                    g.matrix,
                )
                .ok_or_else(|| "VP8: the picture is smaller than its header says".to_string())?;
                out.push_back((p.pts_ms, px));
                Ok(())
            }
            // A hidden frame: references updated, nothing to show.
            Ok(None) => Ok(()),
            Err(e) => Err(format!("VP8: {e:?}")),
        }
    }

    fn flush(&mut self, _out: &mut Pictures) {}

    fn reset(&mut self) -> Result<(), String> {
        self.decoder = vp8_decoder::Decoder::new();
        Ok(())
    }
}

// ---------------------------------------------------------------------
// VP9
// ---------------------------------------------------------------------

/// VP9 (pure Rust).
pub struct Vp9 {
    decoder: rusty_vp9::Vp9Decoder,
    geo: Geometry,
}

impl Vp9 {
    fn new(geo: Geometry) -> Self {
        Self {
            decoder: rusty_vp9::Vp9Decoder::new(),
            geo,
        }
    }

    fn drain(&mut self, out: &mut Pictures) -> Result<(), String> {
        loop {
            match self.decoder.next_frame() {
                Ok(f) => {
                    if f.bit_depth != 8 {
                        return Err(format!("VP9: {}-bit video is not supported", f.bit_depth));
                    }
                    let (Some(y), Some(u), Some(v)) =
                        (f.planes.first(), f.planes.get(1), f.planes.get(2))
                    else {
                        return Err("VP9: the picture has no colour planes".into());
                    };
                    let (ys, us, vs) = (
                        f.strides.first().copied().unwrap_or(0),
                        f.strides.get(1).copied().unwrap_or(0),
                        f.strides.get(2).copied().unwrap_or(0),
                    );
                    let g = self.geo;
                    let px = yuv::to_rgba(
                        &Planes {
                            y,
                            u,
                            v,
                            strides: (ys, us, vs),
                            width: f.width as usize,
                            height: f.height as usize,
                            sub_x: f.subsampling_x,
                            sub_y: f.subsampling_y,
                        },
                        g.rotation,
                        g.out_w,
                        g.out_h,
                        g.matrix,
                    )
                    .ok_or_else(|| "VP9: unsupported picture layout".to_string())?;
                    out.push_back((f.pts.unwrap_or(0), px));
                }
                Err(rusty_vp9::Error::Again | rusty_vp9::Error::Eof) => return Ok(()),
                Err(e) => return Err(format!("VP9: {e}")),
            }
        }
    }
}

impl VideoDecoder for Vp9 {
    fn decode(&mut self, p: &Packet, out: &mut Pictures) -> Result<(), String> {
        self.decoder
            .push(&p.data, Some(p.pts_ms))
            .map_err(|e| format!("VP9: {e}"))?;
        self.drain(out)
    }

    fn flush(&mut self, out: &mut Pictures) {
        self.decoder.flush();
        let _ = self.drain(out);
    }

    fn reset(&mut self) -> Result<(), String> {
        self.decoder = rusty_vp9::Vp9Decoder::new();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_times_come_out_in_presentation_order() {
        let mut p = PendingTimes::default();
        for t in [0, 66, 16, 33] {
            p.push(t);
        }
        assert_eq!(
            (p.pop(), p.pop(), p.pop(), p.pop(), p.pop()),
            (Some(0), Some(16), Some(33), Some(66), None)
        );
    }
}
