//! MP4 / QuickTime video track access: a small, defensive ISO-BMFF reader
//! (progressive and fragmented files, `moov` before or after the media,
//! QuickTime sample descriptions), the codec check, AVCC → Annex B, timing
//! and the keyframe index used for seeking.
//!
//! Only the boxes the video track needs are read: `moov` and every `moof`
//! are loaded (both small and capped), `mdat` is never read whole. Every
//! count and offset comes from the file, so all arithmetic is checked and
//! every table is capped by the real sample count.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use super::source::{Codec, Packet, PacketSource, SampleTime, TrackError, VideoInfo};
use super::yuv::Rotation;

/// Largest single compressed sample read (a 4K I-frame is a few MB).
pub const MAX_SAMPLE: usize = 16 * 1024 * 1024;
/// Largest `moov` / `moof` box loaded into memory.
const MAX_HEADER_BOX: u64 = 64 * 1024 * 1024;
/// Bytes of `moof` boxes kept in total (a long recording has thousands).
const MAX_FRAGMENT_BYTES: usize = 128 * 1024 * 1024;
/// Most top-level boxes walked.
const MAX_TOP_BOXES: usize = 1 << 20;
/// Most samples indexed for one track (4.8 h at 60 fps).
const MAX_SAMPLES: usize = 1 << 20;

/// Composition time of every sample in ms, made relative to the earliest
/// one (MP4 files with B-frames start their first frame at a positive
/// offset; edit lists that undo it are not read).
pub fn sample_times(raw: &[(u64, i32, bool)], timescale: u32) -> Vec<SampleTime> {
    let ts = i64::from(timescale.max(1));
    let pts: Vec<i64> = raw
        .iter()
        .map(|&(start, off, _)| {
            i64::try_from(start)
                .unwrap_or(i64::MAX)
                .saturating_add(i64::from(off))
        })
        .collect();
    let base = pts.iter().take(64).copied().min().unwrap_or(0);
    raw.iter()
        .zip(pts)
        .map(|(&(_, _, sync), p)| SampleTime {
            pts_ms: p.saturating_sub(base).saturating_mul(1000) / ts,
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
    let mut out = Vec::with_capacity(count.min(MAX_SAMPLES));
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
        let hdr = sample.get(p..p.checked_add(len_size)?)?;
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

// ---------------------------------------------------------------------
// Box parsing
// ---------------------------------------------------------------------

/// Big-endian field reader over a box body; every read is bounds-checked.
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }
    fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.bytes(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }
    fn u64(&mut self) -> Option<u64> {
        let hi = u64::from(self.u32()?);
        let lo = u64::from(self.u32()?);
        Some((hi << 32) | lo)
    }
    /// Full-box header: `(version, flags)`.
    fn full(&mut self) -> Option<(u8, u32)> {
        let v = self.u32()?;
        Some(((v >> 24) as u8, v & 0x00FF_FFFF))
    }
    fn rest(&self) -> &'a [u8] {
        self.b.get(self.p..).unwrap_or_default()
    }
}

/// The child boxes of a box body, as `(type, body)`. Stops at the first
/// malformed header (a truncated file keeps what came before it).
struct Boxes<'a> {
    data: &'a [u8],
}

impl<'a> Iterator for Boxes<'a> {
    type Item = ([u8; 4], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let mut r = Rd::new(self.data);
        let size32 = r.u32()?;
        let kind: [u8; 4] = r.bytes(4)?.try_into().ok()?;
        let (hdr, size) = match size32 {
            0 => (8, self.data.len() as u64),
            1 => (16, r.u64()?),
            n => (8, u64::from(n)),
        };
        let size = usize::try_from(size).ok();
        match size {
            Some(size) if size >= hdr && size <= self.data.len() => {
                let body = &self.data[hdr..size];
                self.data = &self.data[size..];
                Some((kind, body))
            }
            _ => {
                self.data = &[];
                None
            }
        }
    }
}

fn boxes(data: &[u8]) -> Boxes<'_> {
    Boxes { data }
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, b)| b)
}

/// `child` along a path of box types.
fn path<'a>(data: &'a [u8], kinds: &[&[u8; 4]]) -> Option<&'a [u8]> {
    kinds.iter().try_fold(data, |d, k| child(d, k))
}

/// H.264 decoder configuration (`avcC`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Avcc {
    /// Size in bytes of each NAL length prefix (1, 2 or 4).
    pub len_size: usize,
    /// Sequence and picture parameter sets, in that order.
    pub params: Vec<Vec<u8>>,
}

/// Parses an `avcC` body. `None` when it is malformed.
pub fn parse_avcc(body: &[u8]) -> Option<Avcc> {
    let mut r = Rd::new(body);
    let _version = r.u8()?;
    r.skip(3)?; // profile, compatibility, level
    let len_size = usize::from(r.u8()? & 3) + 1;
    let mut params = Vec::new();
    let sps = r.u8()? & 0x1F;
    for _ in 0..sps {
        let n = usize::from(r.u16()?);
        params.push(r.bytes(n)?.to_vec());
    }
    // A configuration cut after the SPS still decodes if the PPS is in band.
    let pps = r.u8().unwrap_or(0);
    for _ in 0..pps {
        let Some(n) = r.u16() else { break };
        let Some(b) = r.bytes(usize::from(n)) else {
            break;
        };
        params.push(b.to_vec());
    }
    Some(Avcc { len_size, params })
}

/// The first sample description of a track.
#[derive(Debug, Clone, Default)]
struct Entry {
    fourcc: [u8; 4],
    width: u16,
    height: u16,
    avcc: Option<Avcc>,
}

fn parse_stsd(body: &[u8]) -> Option<Entry> {
    let mut r = Rd::new(body);
    r.full()?;
    let _count = r.u32()?;
    let (fourcc, e) = boxes(r.rest()).next()?;
    let mut r = Rd::new(e);
    // SampleEntry: reserved(6) + data_reference_index(2); VisualSampleEntry
    // (also QuickTime's video description): 16 bytes of version / vendor /
    // quality, width, height, resolution, frame count, compressor name,
    // depth, colour table id = 70 bytes before the child boxes.
    r.skip(8 + 16)?;
    let width = r.u16()?;
    let height = r.u16()?;
    r.skip(70 - 20)?;
    let children = r.rest();
    let avcc = child(children, b"avcC").and_then(parse_avcc);
    Some(Entry {
        fourcc,
        width,
        height,
        avcc,
    })
}

/// One sample: where it is and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    offset: u64,
    size: u32,
    dts: u64,
    cto: i32,
    sync: bool,
}

/// Fragment defaults from `trex`.
#[derive(Debug, Clone, Copy, Default)]
struct Trex {
    duration: u32,
    size: u32,
    flags: u32,
}

/// A track of the file, with its samples in decode order.
#[derive(Debug, Default)]
struct Track {
    id: u32,
    handler: [u8; 4],
    timescale: u32,
    /// `tkhd` matrix entries a, b, c, d.
    matrix: [i32; 4],
    tkhd_size: (u32, u32),
    entry: Option<Entry>,
    samples: Vec<Sample>,
    trex: Trex,
    /// Decode time after the last sample (where the next fragment starts).
    end_dts: u64,
}

/// Sample offsets from `stsz` / `stz2`, `stsc` and `stco` / `co64`.
fn chunk_layout(stbl: &[u8]) -> Option<Vec<(u64, u32)>> {
    let sizes: Vec<u32> = if let Some(b) = child(stbl, b"stsz") {
        let mut r = Rd::new(b);
        r.full()?;
        let fixed = r.u32()?;
        let count = (r.u32()? as usize).min(MAX_SAMPLES);
        if fixed != 0 {
            vec![fixed; count]
        } else {
            let count = count.min(r.rest().len() / 4);
            (0..count).map_while(|_| r.u32()).collect()
        }
    } else if let Some(b) = child(stbl, b"stz2") {
        let mut r = Rd::new(b);
        r.full()?;
        r.skip(3)?;
        let field = r.u8()?;
        let count = (r.u32()? as usize).min(MAX_SAMPLES);
        let data = r.rest();
        match field {
            4 => (0..count)
                .map_while(|i| {
                    let b = *data.get(i / 2)?;
                    Some(u32::from(if i % 2 == 0 { b >> 4 } else { b & 15 }))
                })
                .collect(),
            8 => data.iter().take(count).map(|&b| u32::from(b)).collect(),
            16 => data
                .chunks_exact(2)
                .take(count)
                .map(|c| u32::from(u16::from_be_bytes([c[0], c[1]])))
                .collect(),
            _ => return None,
        }
    } else {
        return Some(Vec::new());
    };
    let chunks: Vec<u64> = if let Some(b) = child(stbl, b"stco") {
        let mut r = Rd::new(b);
        r.full()?;
        let n = (r.u32()? as usize).min(r.rest().len() / 4);
        (0..n).map_while(|_| r.u32().map(u64::from)).collect()
    } else if let Some(b) = child(stbl, b"co64") {
        let mut r = Rd::new(b);
        r.full()?;
        let n = (r.u32()? as usize).min(r.rest().len() / 8);
        (0..n).map_while(|_| r.u64()).collect()
    } else {
        Vec::new()
    };
    let stsc: Vec<(u32, u32)> = child(stbl, b"stsc")
        .and_then(|b| {
            let mut r = Rd::new(b);
            r.full()?;
            let n = (r.u32()? as usize).min(r.rest().len() / 12);
            Some(
                (0..n)
                    .map_while(|_| {
                        let first = r.u32()?;
                        let per = r.u32()?;
                        r.u32()?;
                        Some((first, per))
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    Some(place_samples(&sizes, &chunks, &stsc))
}

/// `(offset, size)` of each sample from the chunk offsets and the
/// sample-to-chunk runs (`(first chunk, samples per chunk)`, 1-based).
fn place_samples(sizes: &[u32], chunks: &[u64], stsc: &[(u32, u32)]) -> Vec<(u64, u32)> {
    let mut out = Vec::with_capacity(sizes.len());
    let mut run = 0;
    for (ci, &base) in chunks.iter().enumerate() {
        let chunk_no = ci as u64 + 1;
        while stsc
            .get(run + 1)
            .is_some_and(|&(first, _)| u64::from(first) <= chunk_no)
        {
            run += 1;
        }
        let Some(&(_, per)) = stsc.get(run) else {
            break;
        };
        let mut off = base;
        for _ in 0..per {
            let Some(&size) = sizes.get(out.len()) else {
                return out;
            };
            out.push((off, size));
            off = off.saturating_add(u64::from(size));
        }
    }
    out
}

fn parse_trak(trak: &[u8]) -> Option<Track> {
    let mut t = Track::default();
    let tkhd = child(trak, b"tkhd")?;
    let mut r = Rd::new(tkhd);
    let (v, _) = r.full()?;
    if v == 1 {
        r.skip(16)?;
        t.id = r.u32()?;
        r.skip(4 + 8)?;
    } else {
        r.skip(8)?;
        t.id = r.u32()?;
        r.skip(4 + 4)?;
    }
    r.skip(8 + 2 + 2 + 2 + 2)?;
    let a = r.i32()?;
    let b = r.i32()?;
    r.skip(4)?;
    let c = r.i32()?;
    let d = r.i32()?;
    r.skip(4 * 4)?;
    t.matrix = [a, b, c, d];
    t.tkhd_size = (r.u32().unwrap_or(0) >> 16, r.u32().unwrap_or(0) >> 16);
    let mdia = child(trak, b"mdia")?;
    if let Some(mdhd) = child(mdia, b"mdhd") {
        let mut r = Rd::new(mdhd);
        let (v, _) = r.full()?;
        r.skip(if v == 1 { 16 } else { 8 })?;
        t.timescale = r.u32()?;
    }
    if let Some(hdlr) = child(mdia, b"hdlr") {
        let mut r = Rd::new(hdlr);
        r.full()?;
        r.skip(4)?;
        t.handler = r.bytes(4)?.try_into().ok()?;
    }
    let Some(stbl) = path(mdia, &[b"minf", b"stbl"]) else {
        return Some(t);
    };
    t.entry = child(stbl, b"stsd").and_then(parse_stsd);
    let placed = chunk_layout(stbl).unwrap_or_default();
    let stts: Vec<(u32, u32)> = child(stbl, b"stts")
        .and_then(|b| pairs(b, |r| Some((r.u32()?, r.u32()?))))
        .unwrap_or_default();
    let ctts: Vec<(u32, i32)> = child(stbl, b"ctts")
        .and_then(|b| pairs(b, |r| Some((r.u32()?, r.i32()?))))
        .unwrap_or_default();
    let stss: Option<Vec<u32>> =
        child(stbl, b"stss").map(|b| pairs(b, Rd::u32).unwrap_or_default());
    let times = sample_table(
        placed.len() as u32,
        stts.into_iter(),
        ctts.into_iter(),
        stss.as_deref(),
    );
    t.samples = placed
        .iter()
        .zip(&times)
        .map(|(&(offset, size), &(dts, cto, sync))| Sample {
            offset,
            size,
            dts,
            cto,
            sync,
        })
        .collect();
    // The decode time where fragments continue (stts end).
    t.end_dts = t.samples.last().map_or(0, |s| {
        let step = times
            .len()
            .checked_sub(2)
            .and_then(|i| times.get(i))
            .map_or(0, |p| s.dts.saturating_sub(p.0));
        s.dts.saturating_add(step)
    });
    Some(t)
}

/// A full box with an entry count followed by entries read by `f`.
fn pairs<'a, T>(body: &'a [u8], f: impl Fn(&mut Rd<'a>) -> Option<T>) -> Option<Vec<T>> {
    let mut r = Rd::new(body);
    r.full()?;
    let n = (r.u32()? as usize).min(r.rest().len()).min(MAX_SAMPLES);
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        match f(&mut r) {
            Some(v) => out.push(v),
            None => break,
        }
    }
    Some(out)
}

/// Sample flag: not a sync sample.
const NON_SYNC: u32 = 0x0001_0000;

/// Appends the samples of `track` from one `moof` that starts at `moof_pos`.
fn parse_moof(moof: &[u8], moof_pos: u64, track: &mut Track) {
    for (kind, traf) in boxes(moof) {
        if &kind != b"traf" {
            continue;
        }
        let _ = parse_traf(traf, moof_pos, track);
    }
}

fn parse_traf(traf: &[u8], moof_pos: u64, track: &mut Track) -> Option<()> {
    let mut r = Rd::new(child(traf, b"tfhd")?);
    let (_, flags) = r.full()?;
    if r.u32()? != track.id {
        return Some(());
    }
    let base = if flags & 0x1 != 0 {
        r.u64()?
    } else {
        // default-base-is-moof, and what every writer means without it.
        moof_pos
    };
    if flags & 0x2 != 0 {
        r.u32()?;
    }
    let def_dur = if flags & 0x8 != 0 {
        r.u32()?
    } else {
        track.trex.duration
    };
    let def_size = if flags & 0x10 != 0 {
        r.u32()?
    } else {
        track.trex.size
    };
    let def_flags = if flags & 0x20 != 0 {
        r.u32()?
    } else {
        track.trex.flags
    };
    let mut dts = match child(traf, b"tfdt") {
        Some(b) => {
            let mut r = Rd::new(b);
            let (v, _) = r.full()?;
            if v == 1 {
                r.u64()?
            } else {
                u64::from(r.u32()?)
            }
        }
        None => track.end_dts,
    };
    let mut data_pos = base;
    for (kind, trun) in boxes(traf) {
        if &kind != b"trun" {
            continue;
        }
        let mut r = Rd::new(trun);
        let (v, tf) = r.full()?;
        let count = r.u32()? as usize;
        if tf & 0x1 != 0 {
            data_pos = base.checked_add_signed(i64::from(r.i32()?))?;
        }
        let first_flags = if tf & 0x4 != 0 { Some(r.u32()?) } else { None };
        for i in 0..count {
            if track.samples.len() >= MAX_SAMPLES {
                return None;
            }
            let dur = if tf & 0x100 != 0 { r.u32()? } else { def_dur };
            let size = if tf & 0x200 != 0 { r.u32()? } else { def_size };
            let sflags = if tf & 0x400 != 0 {
                r.u32()?
            } else if i == 0 {
                first_flags.unwrap_or(def_flags)
            } else {
                def_flags
            };
            let cto = if tf & 0x800 != 0 {
                let raw = r.u32()?;
                // Version 0 offsets are unsigned; real ones are small.
                if v == 0 {
                    i32::try_from(raw).unwrap_or(i32::MAX)
                } else {
                    raw as i32
                }
            } else {
                0
            };
            track.samples.push(Sample {
                offset: data_pos,
                size,
                dts,
                cto,
                sync: sflags & NON_SYNC == 0,
            });
            data_pos = data_pos.saturating_add(u64::from(size));
            dts = dts.saturating_add(u64::from(dur));
        }
    }
    track.end_dts = dts;
    Some(())
}

/// What a file holds: its tracks and the movie duration in ms.
struct Movie {
    tracks: Vec<Track>,
    duration_ms: u64,
}

/// Reads a box body of `len` bytes at the reader's position.
fn read_body(f: &mut impl Read, len: u64) -> Result<Vec<u8>, TrackError> {
    if len > MAX_HEADER_BOX {
        return Err(TrackError::Broken("the MP4 header is too large".into()));
    }
    let mut v = Vec::with_capacity(len as usize);
    f.take(len)
        .read_to_end(&mut v)
        .map_err(|e| TrackError::Broken(format!("cannot read: {e}")))?;
    Ok(v)
}

fn read_movie(f: &mut (impl Read + Seek), file_len: u64) -> Result<Movie, TrackError> {
    let mut moov = None;
    let mut moofs: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut kept = 0usize;
    let mut pos = 0u64;
    for _ in 0..MAX_TOP_BOXES {
        if pos.saturating_add(8) > file_len {
            break;
        }
        f.seek(SeekFrom::Start(pos))
            .map_err(|e| TrackError::Broken(format!("cannot read: {e}")))?;
        let mut h = [0u8; 16];
        if f.read_exact(&mut h[..8]).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
        let kind = [h[4], h[5], h[6], h[7]];
        let (hdr, size) = match size32 {
            0 => (8u64, file_len - pos),
            1 => {
                if f.read_exact(&mut h[8..16]).is_err() {
                    break;
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&h[8..16]);
                (16, u64::from_be_bytes(b))
            }
            n => (8, u64::from(n)),
        };
        if size < hdr {
            break;
        }
        // A truncated last box keeps what is in the file.
        let body_len = size.min(file_len - pos) - hdr;
        match &kind {
            b"moov" if moov.is_none() => moov = Some(read_body(f, body_len)?),
            b"moof" => {
                kept = kept.saturating_add(body_len as usize);
                if kept > MAX_FRAGMENT_BYTES {
                    break;
                }
                moofs.push((pos, read_body(f, body_len)?));
            }
            _ => {}
        }
        pos = pos.saturating_add(size);
    }
    let Some(moov) = moov else {
        return Err(TrackError::Broken(
            "not a playable MP4 file (no movie header)".into(),
        ));
    };
    let mut duration_ms = 0;
    if let Some(mvhd) = child(&moov, b"mvhd") {
        let mut r = Rd::new(mvhd);
        if let Some((v, _)) = r.full() {
            let (scale, dur) = if v == 1 {
                (r.skip(16).and_then(|()| r.u32()), r.u64())
            } else {
                (r.skip(8).and_then(|()| r.u32()), r.u32().map(u64::from))
            };
            if let (Some(s), Some(d)) = (scale, dur)
                && s > 0
                && d != u64::MAX
                && d != u64::from(u32::MAX)
            {
                duration_ms = u64::try_from(u128::from(d) * 1000 / u128::from(s)).unwrap_or(0);
            }
        }
    }
    let mut tracks: Vec<Track> = boxes(&moov)
        .filter(|(k, _)| k == b"trak")
        .filter_map(|(_, b)| parse_trak(b))
        .collect();
    if let Some(mvex) = child(&moov, b"mvex") {
        for (kind, trex) in boxes(mvex) {
            if &kind != b"trex" {
                continue;
            }
            let mut r = Rd::new(trex);
            let parsed = (|| {
                r.full()?;
                let id = r.u32()?;
                r.u32()?;
                Some((id, r.u32()?, r.u32()?, r.u32()?))
            })();
            if let Some((id, duration, size, flags)) = parsed
                && let Some(t) = tracks.iter_mut().find(|t| t.id == id)
            {
                t.trex = Trex {
                    duration,
                    size,
                    flags,
                };
            }
        }
    }
    // Only a movie with `mvex` is fragmented. A "hybrid" recording (OBS)
    // ends with a complete `moov` without it, and its `moof`s describe the
    // same samples again: reading both would play every frame twice.
    if child(&moov, b"mvex").is_some() {
        for (moof_pos, moof) in &moofs {
            for t in tracks.iter_mut() {
                parse_moof(moof, *moof_pos, t);
            }
        }
    }
    if duration_ms == 0 {
        // Fragmented files leave the movie duration at zero: the last
        // sample of the longest track ends the movie.
        duration_ms = tracks
            .iter()
            .filter(|t| t.timescale > 0)
            .map(|t| {
                let end = t
                    .samples
                    .iter()
                    .map(|s| s.dts.saturating_add_signed(i64::from(s.cto.max(0))))
                    .max()
                    .unwrap_or(0)
                    .max(t.end_dts);
                u64::try_from(u128::from(end) * 1000 / u128::from(t.timescale)).unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
    }
    Ok(Movie {
        tracks,
        duration_ms,
    })
}

fn open_movie(p: &Path) -> Result<(BufReader<File>, Movie), TrackError> {
    let f = File::open(p).map_err(|e| TrackError::Broken(format!("cannot open: {e}")))?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut r = BufReader::new(f);
    let movie = read_movie(&mut r, size)?;
    Ok((r, movie))
}

/// What the helper does with a sample description's codec.
fn codec_of(fourcc: &[u8; 4]) -> Result<Codec, TrackError> {
    let unsupported = |s: &str| Err(TrackError::Unsupported(s.into()));
    match fourcc {
        b"avc1" | b"avc2" | b"avc3" | b"avc4" => Ok(Codec::H264),
        b"vp09" => Ok(Codec::Vp9),
        b"vp08" => Ok(Codec::Vp8),
        b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => unsupported("H.265 (HEVC)"),
        b"av01" => unsupported("AV1"),
        b"mp4v" => unsupported("MPEG-4 Part 2 video"),
        b"encv" => unsupported("encrypted (DRM) video"),
        b"apcn" | b"apch" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => unsupported("Apple ProRes"),
        other => {
            let name: String = other
                .iter()
                .filter(|b| b.is_ascii_alphanumeric())
                .map(|&b| char::from(b))
                .collect();
            Err(TrackError::Unsupported(format!(
                "a video codec other than H.264 ({name})"
            )))
        }
    }
}

/// The video track of an MP4 file.
pub struct Mp4Video {
    reader: BufReader<File>,
    /// `(offset, size)` per sample, in decode order.
    samples: Vec<(u64, u32)>,
    /// NAL length size for H.264 (0 for codecs that are not length-framed).
    len_size: usize,
    /// Per sample, in decode order.
    times: Vec<SampleTime>,
    /// Next sample to hand out.
    next: usize,
    /// What the decoder needs to know.
    pub info: VideoInfo,
}

/// Whole-file duration in ms (the movie header, or the longest track).
pub fn duration_ms(path: &Path) -> Result<u64, TrackError> {
    Ok(open_movie(path)?.1.duration_ms)
}

impl Mp4Video {
    /// Opens the first video track. `Ok(None)`: the file has no video.
    pub fn open(path: &Path) -> Result<Option<Self>, TrackError> {
        let (reader, movie) = open_movie(path)?;
        let duration_ms = movie.duration_ms;
        let mut video: Vec<Track> = movie
            .tracks
            .into_iter()
            .filter(|t| &t.handler == b"vide" && t.entry.is_some())
            .collect();
        // Several picture tracks: a playable one with samples first.
        video.sort_by_key(|t| {
            let ok = t
                .entry
                .as_ref()
                .is_some_and(|e| codec_of(&e.fourcc).is_ok());
            (!ok, t.samples.is_empty(), t.id)
        });
        let Some(track) = video.into_iter().next() else {
            return Ok(None);
        };
        let Some(entry) = track.entry.as_ref() else {
            return Ok(None);
        };
        let codec = codec_of(&entry.fourcc)?;
        let (header, len_size) = match codec {
            Codec::H264 => {
                let Some(avcc) = entry.avcc.as_ref() else {
                    return Err(TrackError::Unsupported(
                        "H.264 without its configuration".into(),
                    ));
                };
                let mut header = Vec::new();
                for nal in &avcc.params {
                    header.extend_from_slice(&[0, 0, 0, 1]);
                    header.extend_from_slice(nal);
                }
                (header, avcc.len_size)
            }
            Codec::Vp8 | Codec::Vp9 => (Vec::new(), 0),
        };
        let [a, b, c, d] = track.matrix;
        let rotation = Rotation::from_matrix(a, b, c, d);
        let (mut width, mut height) = (usize::from(entry.width), usize::from(entry.height));
        if width == 0 || height == 0 {
            (width, height) = (track.tkhd_size.0 as usize, track.tkhd_size.1 as usize);
        }
        let raw: Vec<(u64, i32, bool)> = track
            .samples
            .iter()
            .map(|s| (s.dts, s.cto, s.sync))
            .collect();
        Ok(Some(Self {
            reader,
            samples: track.samples.iter().map(|s| (s.offset, s.size)).collect(),
            len_size,
            times: sample_times(&raw, track.timescale),
            next: 0,
            info: VideoInfo {
                codec,
                width,
                height,
                rotation,
                header,
                duration_ms,
            },
        }))
    }

    /// Sample `i` (0-based) appended to `out` (Annex B for H.264).
    fn read(&mut self, i: usize, out: &mut Vec<u8>) -> Result<(), String> {
        let &(offset, size) = self.samples.get(i).ok_or("sample missing")?;
        let size = size as usize;
        if size > MAX_SAMPLE {
            return Err("sample too large".into());
        }
        let mut buf = vec![0u8; size];
        self.reader
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.reader.read_exact(&mut buf))
            .map_err(|e| e.to_string())?;
        if self.len_size == 0 {
            out.extend_from_slice(&buf);
            return Ok(());
        }
        avcc_to_annexb(&buf, self.len_size, out).ok_or_else(|| "malformed sample".into())
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
        match self.read(i, &mut p.data) {
            Ok(()) => Ok(true),
            // A recording cut off mid-file (a crashed recorder, a partial
            // download) plays up to where its data ends.
            Err(_) if i > 0 => {
                self.next = self.times.len();
                Ok(false)
            }
            Err(e) => Err(format!("reading video sample {i} failed: {e}")),
        }
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
        assert_eq!(ms, vec![0, 133, 33, 66]);
        assert!(t[0].sync && !t[1].sync);
        // Hostile times never overflow.
        let t = sample_times(&[(u64::MAX, i32::MAX, true), (0, i32::MIN, false)], 1);
        assert_eq!(t.len(), 2);
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

    fn mk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn full(v: u8, flags: u32, rest: &[u8]) -> Vec<u8> {
        let mut b = ((u32::from(v) << 24) | flags).to_be_bytes().to_vec();
        b.extend_from_slice(rest);
        b
    }

    fn be(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    #[test]
    fn chunks_place_samples_back_to_back() {
        // 5 samples, chunks of 2, 2, 1 (the last run starts at chunk 3).
        let placed = place_samples(&[10, 20, 30, 40, 50], &[100, 500, 900], &[(1, 2), (3, 1)]);
        assert_eq!(
            placed,
            vec![(100, 10), (110, 20), (500, 30), (530, 40), (900, 50)]
        );
        // Tables that disagree never index past the sizes.
        assert_eq!(place_samples(&[1], &[0, 9, 9], &[(1, 50)]), vec![(0, 1)]);
        assert!(place_samples(&[1, 2], &[0], &[]).is_empty());
    }

    #[test]
    fn avcc_keeps_every_parameter_set_and_the_length_size() {
        let body = [
            1, 0x64, 0, 0x1F, 0xFD, 0xE2, 0, 2, 0x67, 1, 0, 1, 0x67, 2, 0x68,
        ];
        // lengthSizeMinusOne = 1 (2-byte lengths), two SPS, one PPS (no PPS
        // count byte: cut short), so only the two SPS come back.
        let a = parse_avcc(&body).unwrap();
        assert_eq!(a.len_size, 2);
        assert_eq!(a.params, vec![vec![0x67, 1], vec![0x67]]);
        assert!(parse_avcc(&[1, 2]).is_none());
    }

    #[test]
    fn codecs_are_named_from_the_sample_description() {
        assert_eq!(codec_of(b"avc1"), Ok(Codec::H264));
        assert_eq!(codec_of(b"avc3"), Ok(Codec::H264));
        assert_eq!(codec_of(b"vp09"), Ok(Codec::Vp9));
        for hevc in [b"hvc1", b"hev1", b"dvh1"] {
            assert_eq!(
                codec_of(hevc),
                Err(TrackError::Unsupported("H.265 (HEVC)".into()))
            );
        }
        let Err(TrackError::Unsupported(s)) = codec_of(b"x/\n\0") else {
            panic!()
        };
        assert!(!s.contains('/') && !s.contains('\n'), "{s}");
    }

    /// A fragmented movie: `moov` with an empty sample table and a `trex`,
    /// then one `moof` (tfhd default-base-is-moof, tfdt, a trun with sizes
    /// and flags) and its `mdat`.
    fn fragmented() -> Vec<u8> {
        let tkhd = mk(b"tkhd", &full(0, 3, &[0u8; 80]));
        let mdhd = mk(b"mdhd", &full(0, 0, &be(&[0, 0, 1000, 0, 0])));
        let hdlr = mk(b"hdlr", &full(0, 0, b"\0\0\0\0vide"));
        let stsd = mk(b"stsd", &full(0, 0, &be(&[0])));
        let stbl = mk(b"stbl", &stsd);
        let minf = mk(b"minf", &stbl);
        let mdia = mk(b"mdia", &[mdhd, hdlr, minf].concat());
        let mut tkhd = tkhd;
        // track_id = 1 (after version/flags, ctime, mtime).
        tkhd[8 + 12..8 + 16].copy_from_slice(&1u32.to_be_bytes());
        let trak = mk(b"trak", &[tkhd, mdia].concat());
        let trex = mk(b"trex", &full(0, 0, &be(&[1, 1, 40, 0, NON_SYNC])));
        let mvex = mk(b"mvex", &trex);
        let moov = mk(b"moov", &[trak, mvex].concat());
        let tfhd = mk(b"tfhd", &full(0, 0x2_0000, &be(&[1])));
        let tfdt = mk(b"tfdt", &full(1, 0, &[0, 0, 0, 0, 0, 0, 0x03, 0xE8]));
        // data_offset (patched below) + first-sample flags + per-sample size.
        let trun = mk(b"trun", &full(0, 0x205, &be(&[3, 0, 0, 5, 6, 7])));
        let traf = mk(b"traf", &[tfhd, tfdt, trun].concat());
        let mut moof = mk(b"moof", &traf);
        let data_offset = (moof.len() + 8) as u32;
        let at = moof.len() - 5 * 4;
        moof[at..at + 4].copy_from_slice(&data_offset.to_be_bytes());
        let mdat = mk(b"mdat", &[0xAB; 18]);
        let mut file = moov;
        file.extend_from_slice(&moof);
        file.extend_from_slice(&mdat);
        file
    }

    #[test]
    fn fragmented_files_index_every_sample_in_the_fragments() {
        let file = fragmented();
        let len = file.len() as u64;
        let m = read_movie(&mut std::io::Cursor::new(&file), len).unwrap();
        let t = &m.tracks[0];
        let data = file.len() as u64 - 18;
        let got: Vec<(u64, u32, u64, bool)> = t
            .samples
            .iter()
            .map(|s| (s.offset, s.size, s.dts, s.sync))
            .collect();
        assert_eq!(
            got,
            vec![
                (data, 5, 1000, true),
                (data + 5, 6, 1040, false),
                (data + 11, 7, 1080, false),
            ]
        );
        // The movie header says 0: the duration comes from the samples.
        assert_eq!(m.duration_ms, 1120);
    }

    /// OBS "hybrid MP4": a complete `moov` (no `mvex`) at the end and the
    /// `moof`s of the recording still in the file. The fragments describe
    /// the same samples again and must be ignored.
    #[test]
    fn fragments_without_mvex_are_ignored() {
        let mut file = fragmented();
        let at = file.windows(4).position(|w| w == b"mvex").unwrap();
        file[at..at + 4].copy_from_slice(b"free");
        let len = file.len() as u64;
        let m = read_movie(&mut std::io::Cursor::new(&file), len).unwrap();
        assert!(m.tracks[0].samples.is_empty());
    }

    #[test]
    fn truncated_and_hostile_headers_never_panic() {
        let file = fragmented();
        for cut in 0..file.len() {
            let f = &file[..cut];
            let _ = read_movie(&mut std::io::Cursor::new(f), cut as u64);
        }
        let mut flipped = file.clone();
        for i in 0..flipped.len() {
            flipped[i] ^= 0xFF;
            let _ = read_movie(&mut std::io::Cursor::new(&flipped), flipped.len() as u64);
            flipped[i] ^= 0xFF;
        }
        // A 64-bit size pointing past the end.
        let mut big = 1u32.to_be_bytes().to_vec();
        big.extend_from_slice(b"moov");
        big.extend_from_slice(&u64::MAX.to_be_bytes());
        let m = read_movie(&mut std::io::Cursor::new(&big), big.len() as u64);
        assert!(m.is_ok_and(|m| m.tracks.is_empty()));
    }
}
