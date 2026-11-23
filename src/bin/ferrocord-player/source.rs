//! What the demuxers hand to the decoders: container detection, track
//! description and compressed packets.

use std::io::Read;
use std::path::Path;

use super::yuv::Rotation;

/// Why the file cannot be played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackError {
    /// Not a readable file of a known container.
    Broken(String),
    /// A codec (or container) the built-in decoders do not handle.
    Unsupported(String),
}

/// Video codecs the helper decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// H.264 / AVC through OpenH264.
    H264,
    /// VP8 (pure Rust).
    Vp8,
    /// VP9 (pure Rust).
    Vp9,
    /// H.265 / HEVC: platform decoders only (system FFmpeg, Media Foundation).
    Hevc,
    /// AV1: platform decoders only.
    Av1,
}

impl Codec {
    /// Name for messages.
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Vp8 => "VP8",
            Codec::Vp9 => "VP9",
            Codec::Hevc => "H.265 (HEVC)",
            Codec::Av1 => "AV1",
        }
    }
}

/// What the decoder needs to know about the video track.
#[derive(Debug, Clone)]
pub struct VideoInfo {
    /// Codec.
    pub codec: Codec,
    /// Coded width.
    pub width: usize,
    /// Coded height.
    pub height: usize,
    /// Display rotation from the container.
    pub rotation: Rotation,
    /// Decoder configuration fed again after every decoder reset: H.264
    /// SPS + PPS / H.265 VPS + SPS + PPS as Annex B, AV1 configuration OBUs.
    pub header: Vec<u8>,
    /// Container duration in ms (0 = unknown).
    pub duration_ms: u64,
}

/// One video sample's place in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleTime {
    /// Presentation time in ms (composition offsets applied, first frame 0).
    pub pts_ms: i64,
    /// Keyframe (decoding can start here).
    pub sync: bool,
}

/// One compressed picture.
#[derive(Debug, Default)]
pub struct Packet {
    /// Presentation time in ms.
    pub pts_ms: i64,
    /// Decoding can start here.
    pub key: bool,
    /// Bitstream (Annex B for H.264 / H.265, raw frame for VP8 / VP9 / AV1).
    pub data: Vec<u8>,
}

/// Reads video packets in decoding order.
pub trait PacketSource {
    /// Positions at the keyframe at or before `ms` (the next packet is a
    /// keyframe the decoder can start from).
    fn seek(&mut self, ms: i64) -> Result<(), String>;
    /// The next packet into `p`; `Ok(false)` at the end of the stream.
    fn next(&mut self, p: &mut Packet) -> Result<bool, String>;
}

/// File formats the helper opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// MP4 / QuickTime (`ftyp`, `moov`, ...).
    Mp4,
    /// WebM / Matroska (EBML header).
    Webm,
}

/// Decides the container from the first bytes, never from the file name
/// (attachment names are chosen by the sender).
pub fn sniff(head: &[u8]) -> Option<Container> {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Container::Webm);
    }
    let tag = head.get(4..8)?;
    matches!(
        tag,
        b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"styp" | b"pnot"
    )
    .then_some(Container::Mp4)
}

/// Reads the head of `path` and detects its container.
pub fn detect(path: &Path) -> Result<Container, TrackError> {
    let mut f =
        std::fs::File::open(path).map_err(|e| TrackError::Broken(format!("cannot open: {e}")))?;
    let mut head = [0u8; 16];
    let n = f
        .read(&mut head)
        .map_err(|e| TrackError::Broken(format!("cannot read: {e}")))?;
    sniff(&head[..n])
        .ok_or_else(|| TrackError::Unsupported("a file format other than MP4 or WebM".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_are_recognised_by_content() {
        assert_eq!(
            sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F]),
            Some(Container::Webm)
        );
        assert_eq!(sniff(b"\0\0\0\x20ftypisom"), Some(Container::Mp4));
        assert_eq!(sniff(b"\0\0\0\x08free\0\0\0\x08"), Some(Container::Mp4));
        assert_eq!(sniff(b"GIF89a......"), None);
        assert_eq!(sniff(b"RIFF....AVI "), None);
        assert_eq!(sniff(&[]), None);
        assert_eq!(sniff(b"abc"), None);
    }
}
