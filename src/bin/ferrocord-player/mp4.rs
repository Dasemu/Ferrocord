//! MP4 / QuickTime video track access: codec check, AVCC → Annex B, timing
//! and the keyframe index used for seeking.

use std::fs::File;
use std::io::BufReader;

use super::source::{Codec, Packet, PacketSource, SampleTime, TrackError, VideoInfo};
use super::yuv::Rotation;

/// Largest single compressed sample read (a 4K I-frame is a few MB).
pub const MAX_SAMPLE: usize = 16 * 1024 * 1024;

/// Composition time of every sample in ms, made relative to the earliest
/// one (MP4 files with B-frames start their first frame at a positive
/// offset; edit lists that undo it are not read).
pub fn sample_times(raw: &[(u64, i32, bool)], timescale: u32) -> Vec<SampleTime> {
    let ts = i64::from(timescale.max(1));
    let pts: Vec<i64> = raw
        .iter()
        .map(|&(start, off, _)| (start as i64).saturating_add(i64::from(off)))
        .collect();
    let base = pts.iter().take(64).copied().min().unwrap_or(0);
    raw.iter()
        .zip(pts)
        .map(|(&(_, _, sync), p)| SampleTime {
            pts_ms: (p - base).saturating_mul(1000) / ts,
            sync,
        })
        .collect()
}

/// `(decode time, composition offset, keyframe)` per sample from the MP4
/// sample table (`stts`, `ctts`, `stss`), without reading sample data.
/// Run-length counts are capped by `count`, so a hostile table cannot make
/// it allocate more than one entry per real sample.
pub fn sample_table(
    count: u32,
    stts: impl Iterator<Item = (u32, u32)>,
    ctts: impl Iterator<Item = (u32, i32)>,
    stss: Option<&[u32]>,
) -> Vec<(u64, i32, bool)> {
    let count = count as usize;
    let mut out = Vec::with_capacity(count.min(1 << 20));
    let mut t = 0u64;
    'outer: for (n, delta) in stts {
        for _ in 0..n {
            if out.len() >= count {
                break 'outer;
            }
            out.push((t, 0, stss.is_none()));
            t = t.saturating_add(u64::from(delta));
        }
    }
    let mut i = 0;
    'ctts: for (n, off) in ctts {
        for _ in 0..n {
            let Some(s) = out.get_mut(i) else { break 'ctts };
            s.1 = off;
            i += 1;
        }
    }
    if let Some(sync) = stss {
        for &n in sync {
            if let Some(s) = (n as usize).checked_sub(1).and_then(|k| out.get_mut(k)) {
                s.2 = true;
            }
        }
    }
    out
}

/// Index (0-based) of the keyframe to start decoding from to show `ms`.
pub fn seek_start(times: &[SampleTime], ms: i64) -> usize {
    let mut best = 0;
    for (i, t) in times.iter().enumerate() {
        if t.sync && t.pts_ms <= ms {
            best = i;
        }
        // Samples are in decode order; once well past the target nothing
        // earlier can follow.
        if t.pts_ms > ms + 5_000 {
            break;
        }
    }
    best
}

/// Rewrites length-prefixed NAL units (MP4 `avcC` framing) as an Annex B
/// byte stream. `None` for a malformed sample.
pub fn avcc_to_annexb(sample: &[u8], len_size: usize, out: &mut Vec<u8>) -> Option<()> {
    if !(1..=4).contains(&len_size) {
        return None;
    }
    let mut p = 0;
    while p < sample.len() {
        let hdr = sample.get(p..p + len_size)?;
        let n = hdr
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        p += len_size;
        let nal = sample.get(p..p.checked_add(n)?)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        p += n;
    }
    Some(())
}

/// The H.264 track of an MP4 file.
pub struct Mp4Video {
    reader: mp4::Mp4Reader<BufReader<File>>,
    id: u32,
    len_size: usize,
    /// Per sample, in decode order.
    times: Vec<SampleTime>,
    /// Next sample to hand out.
    next: usize,
    /// What the decoder needs to know.
    pub info: VideoInfo,
}

/// Whole-file duration in ms (longest track).
pub fn duration_ms(path: &std::path::Path) -> Result<u64, TrackError> {
    let f = File::open(path).map_err(|e| TrackError::Broken(format!("cannot open: {e}")))?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let r = mp4::Mp4Reader::read_header(BufReader::new(f), size)
        .map_err(|e| TrackError::Broken(format!("not a playable MP4 file ({e})")))?;
    Ok(r.duration().as_millis() as u64)
}

impl Mp4Video {
    /// Opens the first video track. `Ok(None)`: the file has no video.
    pub fn open(path: &std::path::Path) -> Result<Option<Self>, TrackError> {
        let f = File::open(path).map_err(|e| TrackError::Broken(format!("cannot open: {e}")))?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let reader = mp4::Mp4Reader::read_header(BufReader::new(f), size)
            .map_err(|e| TrackError::Broken(format!("not a playable MP4 file ({e})")))?;
        let duration_ms = reader.duration().as_millis() as u64;
        let mut video: Vec<_> = reader
            .tracks()
            .values()
            .filter(|t| matches!(t.track_type(), Ok(mp4::TrackType::Video)))
            .collect();
        video.sort_by_key(|t| t.track_id());
        let Some(track) = video.first() else {
            return Ok(None);
        };
        match track.media_type() {
            Ok(mp4::MediaType::H264) => {}
            Ok(mp4::MediaType::H265) => return Err(TrackError::Unsupported("H.265 (HEVC)".into())),
            Ok(mp4::MediaType::VP9) => return Err(TrackError::Unsupported("VP9 in MP4".into())),
            _ => {
                return Err(TrackError::Unsupported(
                    "a video codec other than H.264".into(),
                ));
            }
        }
        let Some(avc1) = track.trak.mdia.minf.stbl.stsd.avc1.as_ref() else {
            return Err(TrackError::Unsupported(
                "H.264 without its configuration".into(),
            ));
        };
        let avcc = &avc1.avcc;
        let mut header = Vec::new();
        for nal in avcc
            .sequence_parameter_sets
            .iter()
            .chain(avcc.picture_parameter_sets.iter())
        {
            header.extend_from_slice(&[0, 0, 0, 1]);
            header.extend_from_slice(&nal.bytes);
        }
        let id = track.track_id();
        let timescale = track.timescale();
        let m = &track.trak.tkhd.matrix;
        let rotation = Rotation::from_matrix(m.a, m.b, m.c, m.d);
        let (width, height) = (usize::from(avc1.width), usize::from(avc1.height));
        let len_size = usize::from(avcc.length_size_minus_one & 3) + 1;
        let stbl = &track.trak.mdia.minf.stbl;
        let raw = sample_table(
            track.sample_count(),
            stbl.stts
                .entries
                .iter()
                .map(|e| (e.sample_count, e.sample_delta)),
            stbl.ctts
                .iter()
                .flat_map(|c| c.entries.iter().map(|e| (e.sample_count, e.sample_offset))),
            stbl.stss.as_ref().map(|s| s.entries.as_slice()),
        );
        Ok(Some(Self {
            reader,
            id,
            len_size,
            times: sample_times(&raw, timescale),
            next: 0,
            info: VideoInfo {
                codec: Codec::H264,
                width,
                height,
                rotation,
                header,
                duration_ms,
            },
        }))
    }

    /// Sample `i` (0-based) as Annex B, appended to `out`.
    fn read(&mut self, i: usize, out: &mut Vec<u8>) -> Result<(), String> {
        let s = self
            .reader
            .read_sample(self.id, i as u32 + 1)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "sample missing".to_string())?;
        if s.bytes.len() > MAX_SAMPLE {
            return Err("sample too large".into());
        }
        avcc_to_annexb(&s.bytes, self.len_size, out).ok_or_else(|| "malformed sample".into())
    }
}

impl PacketSource for Mp4Video {
    fn seek(&mut self, ms: i64) -> Result<(), String> {
        self.next = seek_start(&self.times, ms);
        Ok(())
    }

    fn next(&mut self, p: &mut Packet) -> Result<bool, String> {
        let i = self.next;
        let Some(t) = self.times.get(i).copied() else {
            return Ok(false);
        };
        self.next += 1;
        p.pts_ms = t.pts_ms;
        p.key = t.sync;
        p.data.clear();
        self.read(i, &mut p.data)
            .map_err(|e| format!("reading video sample {i} failed: {e}"))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_rewrites_every_nal_and_rejects_truncation() {
        let mut out = Vec::new();
        avcc_to_annexb(&[0, 0, 0, 2, 0x65, 0xAA, 0, 0, 0, 1, 0x41], 4, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xAA, 0, 0, 0, 1, 0x41]);
        let mut out = Vec::new();
        assert!(avcc_to_annexb(&[0, 0, 0, 9, 0x65], 4, &mut out).is_none());
        assert!(avcc_to_annexb(&[0, 3, 1, 2, 3], 2, &mut Vec::new()).is_some());
        assert!(avcc_to_annexb(&[1], 5, &mut Vec::new()).is_none());
        // Huge declared length must not overflow.
        assert!(avcc_to_annexb(&[0xFF, 0xFF, 0xFF, 0xFF, 1], 4, &mut Vec::new()).is_none());
    }

    #[test]
    fn times_apply_composition_offsets_relative_to_the_first_frame() {
        // Decode order I P B B with a 2-frame offset (timescale 30 fps * 512).
        let ts = 15360;
        let raw = [
            (0, 1024, true),
            (512, 2560, false),
            (1024, 512, false),
            (1536, 512, false),
        ];
        let t = sample_times(&raw, ts);
        let ms: Vec<i64> = t.iter().map(|s| s.pts_ms).collect();
        assert_eq!(ms, vec![0, 66, 16, 33]);
        assert!(t[0].sync && !t[1].sync);
    }

    #[test]
    fn the_sample_table_expands_runs_and_marks_keyframes() {
        let t = sample_table(
            4,
            [(3, 512), (5, 512)].into_iter(),
            [(1, 1024), (3, 0)].into_iter(),
            Some(&[1, 4, 99]),
        );
        assert_eq!(
            t,
            vec![
                (0, 1024, true),
                (512, 0, false),
                (1024, 0, false),
                (1536, 0, true)
            ]
        );
        // No stss: every sample is a keyframe. A huge run is capped.
        let t = sample_table(2, [(u32::MAX, 1)].into_iter(), std::iter::empty(), None);
        assert_eq!(t, vec![(0, 0, true), (1, 0, true)]);
    }

    #[test]
    fn seeking_starts_at_the_last_keyframe_before_the_target() {
        let t: Vec<SampleTime> = (0..100)
            .map(|i| SampleTime {
                pts_ms: i * 33,
                sync: i % 30 == 0,
            })
            .collect();
        assert_eq!(seek_start(&t, 0), 0);
        assert_eq!(seek_start(&t, 1500), 30);
        assert_eq!(seek_start(&t, 2000), 60);
        assert_eq!(seek_start(&t, 99_999), 90);
    }
}
