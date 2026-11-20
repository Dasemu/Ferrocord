//! WebM / Matroska: the VP8 / VP9 video track and the Opus audio track.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use matroska_demuxer::{Frame, MatroskaFile, TrackType};

use super::source::{Codec, Packet, PacketSource, TrackError, VideoInfo};
use super::yuv::Rotation;

/// Largest single compressed frame accepted.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Most frames looked at when scanning for keyframes / the duration.
const MAX_SCAN: usize = 5_000_000;

/// A Matroska file opened for reading.
pub fn open_mkv(path: &Path) -> Result<MatroskaFile<BufReader<File>>, TrackError> {
    let f = File::open(path).map_err(|e| TrackError::Broken(format!("cannot open: {e}")))?;
    MatroskaFile::open(BufReader::new(f))
        .map_err(|e| TrackError::Broken(format!("not a playable WebM file ({e})")))
}

/// `ts` timestamp units to ms.
pub fn to_ms(ts: u64, scale_ns: u64) -> i64 {
    i64::try_from(u128::from(ts) * u128::from(scale_ns) / 1_000_000).unwrap_or(i64::MAX)
}

/// ms to timestamp units (rounding down).
pub fn from_ms(ms: i64, scale_ns: u64) -> u64 {
    let ms = u128::try_from(ms.max(0)).unwrap_or(0);
    u64::try_from(ms * 1_000_000 / u128::from(scale_ns.max(1))).unwrap_or(u64::MAX)
}

/// Names a codec id for the "cannot play" message.
pub fn describe_codec(id: &str) -> String {
    match id {
        "V_AV1" => "AV1".into(),
        "V_MPEGH/ISO/HEVC" => "H.265 (HEVC)".into(),
        "V_MPEG4/ISO/AVC" => "H.264 in Matroska".into(),
        "V_VP8" => "VP8".into(),
        "V_VP9" => "VP9".into(),
        "A_VORBIS" => "Vorbis audio".into(),
        "A_AAC" => "AAC in WebM".into(),
        other => {
            let clean: String = other
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                .take(24)
                .collect();
            format!("the codec {clean}")
        }
    }
}

/// Keyframe timestamps (raw units) of `track` and the last timestamp of
/// any track. Bounded by [`MAX_SCAN`].
fn scan(path: &Path, track: u64) -> (Vec<u64>, u64) {
    let mut keys = Vec::new();
    let mut last = 0u64;
    let Ok(mut mkv) = open_mkv(path) else {
        return (keys, last);
    };
    let mut f = Frame::default();
    for _ in 0..MAX_SCAN {
        match mkv.next_frame(&mut f) {
            Ok(true) => {
                last = last.max(f.timestamp);
                if f.track == track && f.is_keyframe == Some(true) {
                    keys.push(f.timestamp);
                }
            }
            _ => break,
        }
    }
    (keys, last)
}

/// The video track of a WebM file.
pub struct WebmVideo {
    path: PathBuf,
    mkv: MatroskaFile<BufReader<File>>,
    track: u64,
    scale_ns: u64,
    /// Raw keyframe timestamps, scanned on the first seek.
    keys: Option<Vec<u64>>,
    frame: Frame,
    /// What the decoder needs to know.
    pub info: VideoInfo,
}

impl WebmVideo {
    /// Opens the first video track. `Ok(None)`: the file has no video.
    pub fn open(path: &Path) -> Result<Option<Self>, TrackError> {
        let mkv = open_mkv(path)?;
        let scale_ns = mkv.info().timestamp_scale().get();
        let Some(t) = mkv
            .tracks()
            .iter()
            .find(|t| t.track_type() == TrackType::Video)
        else {
            return Ok(None);
        };
        let codec = match t.codec_id() {
            "V_VP8" => Codec::Vp8,
            "V_VP9" => Codec::Vp9,
            other => return Err(TrackError::Unsupported(describe_codec(other))),
        };
        let Some(v) = t.video() else {
            return Err(TrackError::Broken(
                "the video track has no picture size".into(),
            ));
        };
        let (width, height) = (
            usize::try_from(v.pixel_width().get()).unwrap_or(0),
            usize::try_from(v.pixel_height().get()).unwrap_or(0),
        );
        let track = t.track_number().get();
        let mut keys = None;
        let mut duration_ms = mkv
            .info()
            .duration()
            .filter(|d| d.is_finite() && *d > 0.0)
            .map_or(0, |d| to_ms(d as u64, scale_ns).max(0) as u64);
        if duration_ms == 0 {
            // Recorded in a browser: no duration in the header. One pass
            // over the clusters finds the last timestamp.
            let (k, last) = scan(path, track);
            duration_ms = to_ms(last, scale_ns).max(0) as u64;
            keys = Some(k);
        }
        Ok(Some(Self {
            path: path.to_path_buf(),
            mkv,
            track,
            scale_ns,
            keys,
            frame: Frame::default(),
            info: VideoInfo {
                codec,
                width,
                height,
                rotation: Rotation::None,
                header: Vec::new(),
                duration_ms,
            },
        }))
    }

    fn restart(&mut self) -> Result<(), String> {
        self.mkv = open_mkv(&self.path).map_err(|_| "cannot reopen the file".to_string())?;
        Ok(())
    }
}

impl PacketSource for WebmVideo {
    fn seek(&mut self, ms: i64) -> Result<(), String> {
        if self.keys.is_none() {
            self.keys = Some(scan(&self.path, self.track).0);
        }
        let target = from_ms(ms, self.scale_ns);
        let key = self
            .keys
            .as_deref()
            .unwrap_or_default()
            .iter()
            .rev()
            .find(|&&k| k <= target)
            .copied();
        match key {
            // `seek` lands on the first frame AFTER the timestamp.
            Some(k) if k > 0 => {
                if self.mkv.seek(k - 1).is_err() {
                    self.restart()?;
                }
                Ok(())
            }
            // The first keyframe, or none known: decode from the start.
            _ => self.restart(),
        }
    }

    fn next(&mut self, p: &mut Packet) -> Result<bool, String> {
        loop {
            match self.mkv.next_frame(&mut self.frame) {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                // A truncated file ends the stream instead of failing it.
                Err(_) => return Ok(false),
            }
            if self.frame.track != self.track {
                continue;
            }
            if self.frame.data.len() > MAX_FRAME {
                return Err("video frame too large".into());
            }
            p.pts_ms = to_ms(self.frame.timestamp, self.scale_ns);
            p.key = self.frame.is_keyframe.unwrap_or(false);
            p.data.clear();
            p.data.extend_from_slice(&self.frame.data);
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_convert_with_the_segment_scale() {
        assert_eq!(to_ms(1500, 1_000_000), 1500);
        assert_eq!(to_ms(2000, 500_000), 1000);
        assert_eq!(from_ms(1500, 1_000_000), 1500);
        assert_eq!(from_ms(-5, 1_000_000), 0);
        // No overflow on hostile values.
        assert_eq!(to_ms(u64::MAX, u64::MAX), i64::MAX);
        assert_eq!(from_ms(1, 0), 1_000_000);
    }

    #[test]
    fn codec_names_never_echo_arbitrary_bytes() {
        assert_eq!(describe_codec("V_AV1"), "AV1");
        assert_eq!(describe_codec("V_MPEGH/ISO/HEVC"), "H.265 (HEVC)");
        let d = describe_codec("V_X/../../etc/passwd\u{0}\n");
        assert!(!d.contains('/') && !d.contains('\n'), "{d}");
    }
}
