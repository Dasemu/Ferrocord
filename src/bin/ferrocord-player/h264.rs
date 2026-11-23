//! The few H.264 sequence parameter set (SPS) fields that decide whether
//! OpenH264 can decode a stream at all.
//!
//! OpenH264 decodes 8-bit 4:2:0 progressive video (Constrained Baseline,
//! Main and High profiles). It refuses an SPS with more than 8 bits per
//! sample, 4:2:2 / 4:4:4 chroma or interlaced coding
//! (`frame_mbs_only_flag = 0`) and then returns no picture and no useful
//! error: without this check such a file only says "no picture could be
//! decoded". The fields are read here so the viewer can name the real
//! reason ("10-bit H.264") instead.

/// The SPS fields the decoder support depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sps {
    /// `profile_idc` (66 Baseline, 77 Main, 100 High, 110 High 10, ...).
    pub profile: u8,
    /// 0 monochrome, 1 4:2:0, 2 4:2:2, 3 4:4:4.
    pub chroma_format: u32,
    /// Bits per luma sample.
    pub bit_depth: u32,
    /// Every picture is a progressive frame.
    pub frame_mbs_only: bool,
}

impl Sps {
    /// What OpenH264 cannot decode in this stream, named for the user
    /// ("This video uses {it}, which the built-in player cannot decode").
    pub fn unsupported(&self) -> Option<String> {
        let profile = match self.profile {
            110 => " (High 10 profile)",
            122 => " (High 4:2:2 profile)",
            244 | 44 => " (High 4:4:4 profile)",
            _ => "",
        };
        if self.bit_depth > 8 {
            return Some(format!("{}-bit H.264{profile}", self.bit_depth));
        }
        match self.chroma_format {
            2 => return Some(format!("H.264 with 4:2:2 colour{profile}")),
            3 => return Some(format!("H.264 with 4:4:4 colour{profile}")),
            _ => {}
        }
        if !self.frame_mbs_only {
            return Some("interlaced H.264".into());
        }
        None
    }

    /// Parses an SPS NAL unit (with its one-byte NAL header).
    pub fn parse(nal: &[u8]) -> Option<Sps> {
        if nal.first()? & 0x1F != 7 {
            return None;
        }
        let rbsp = unescape(nal.get(1..)?);
        let mut b = Bits::new(&rbsp);
        let profile = b.bits(8)? as u8;
        b.bits(16)?; // constraint flags, level
        b.ue()?; // seq_parameter_set_id
        let (mut chroma_format, mut bit_depth) = (1, 8);
        if matches!(
            profile,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            chroma_format = b.ue()?;
            if chroma_format == 3 {
                b.bits(1)?; // separate_colour_plane_flag
            }
            bit_depth = b.ue()?.checked_add(8)?;
            let chroma_depth = b.ue()?.checked_add(8)?;
            bit_depth = bit_depth.max(chroma_depth);
            b.bits(1)?; // qpprime_y_zero_transform_bypass_flag
            if b.bits(1)? == 1 {
                // seq_scaling_matrix_present_flag
                let lists = if chroma_format == 3 { 12 } else { 8 };
                for i in 0..lists {
                    if b.bits(1)? == 1 {
                        b.scaling_list(if i < 6 { 16 } else { 64 })?;
                    }
                }
            }
        }
        b.ue()?; // log2_max_frame_num_minus4
        match b.ue()? {
            0 => {
                b.ue()?; // log2_max_pic_order_cnt_lsb_minus4
            }
            1 => {
                b.bits(1)?;
                b.se()?;
                b.se()?;
                let n = b.ue()?;
                if n > 255 {
                    return None;
                }
                for _ in 0..n {
                    b.se()?;
                }
            }
            _ => {}
        }
        b.ue()?; // max_num_ref_frames
        b.bits(1)?; // gaps_in_frame_num_value_allowed_flag
        b.ue()?; // pic_width_in_mbs_minus1
        b.ue()?; // pic_height_in_map_units_minus1
        let frame_mbs_only = b.bits(1)? == 1;
        Some(Sps {
            profile,
            chroma_format,
            bit_depth,
            frame_mbs_only,
        })
    }
}

/// The SPS NAL units of an Annex B byte stream.
pub fn sps_units(annexb: &[u8]) -> impl Iterator<Item = Sps> + '_ {
    nal_units(annexb).filter_map(Sps::parse)
}

/// The Annex B access unit holds an IDR slice (NAL type 5).
pub fn has_idr(annexb: &[u8]) -> bool {
    nal_units(annexb).any(|n| n.first().is_some_and(|b| b & 0x1F == 5))
}

/// Splits an Annex B byte stream at its start codes.
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    std::iter::from_fn(move || {
        loop {
            let start = find_start(rest)?;
            let body = &rest[start..];
            let end = find_start(body).map_or(body.len(), |e| {
                // Back to the beginning of the next start code (3 or 4 bytes).
                let mut e = e - 3;
                while e > 0 && body[e - 1] == 0 {
                    e -= 1;
                }
                e
            });
            let nal = &body[..end];
            rest = &body[end..];
            if !nal.is_empty() {
                return Some(nal);
            }
            if rest.is_empty() {
                return None;
            }
        }
    })
}

/// Index just after the first `00 00 01` in `d`.
fn find_start(d: &[u8]) -> Option<usize> {
    d.windows(3).position(|w| w == [0, 0, 1]).map(|p| p + 3)
}

/// Removes emulation prevention bytes (`00 00 03` → `00 00`).
fn unescape(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len().min(4096));
    let mut zeros = 0;
    // An SPS is tiny; the fields needed sit in its first bytes.
    for &b in d.iter().take(4096) {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// MSB-first bit reader with Exp-Golomb codes.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.d.get(self.pos / 8)?;
        let v = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(v))
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0u32, |acc, _| Some((acc << 1) | self.bit()?))
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        ((1u64 << zeros) - 1 + u64::from(rest)).try_into().ok()
    }

    fn se(&mut self) -> Option<i64> {
        let k = i64::from(self.ue()?);
        Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
    }

    fn scaling_list(&mut self, size: usize) -> Option<()> {
        let (mut last, mut next) = (8i64, 8i64);
        for _ in 0..size {
            if next != 0 {
                next = (last + self.se()? + 256) % 256;
            }
            if next != 0 {
                last = next;
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SPS NAL units written by x264 (128x72), one per format.
    const HIGH: &[u8] = &[
        0x67, 0x64, 0x00, 0x0a, 0xac, 0xd9, 0x42, 0x0b, 0xf9, 0x70, 0x11, 0x00, 0x00, 0x03, 0x00,
        0x01, 0x00, 0x00, 0x03, 0x00, 0x3c, 0x0f, 0x12, 0x25, 0x96,
    ];
    const BASELINE: &[u8] = &[
        0x67, 0x42, 0xc0, 0x0a, 0xd9, 0x02, 0x0b, 0xf9, 0x70, 0x11, 0x00, 0x00, 0x03, 0x00, 0x01,
        0x00, 0x00, 0x03, 0x00, 0x3c, 0x0f, 0x12, 0x26, 0x48,
    ];
    const HIGH10: &[u8] = &[
        0x67, 0x6e, 0x00, 0x0a, 0xa6, 0xcd, 0x94, 0x20, 0xbf, 0x97, 0x01, 0x10, 0x00, 0x00, 0x03,
        0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xc0, 0xf1, 0x22, 0x59, 0x60,
    ];
    const YUV422: &[u8] = &[
        0x67, 0x7a, 0x00, 0x0a, 0xbc, 0xd9, 0x42, 0x0b, 0xf8, 0x9c, 0x04, 0x40, 0x00, 0x00, 0x03,
        0x00, 0x40, 0x00, 0x00, 0x0f, 0x03, 0xc4, 0x89, 0x65, 0x80,
    ];
    const YUV444: &[u8] = &[
        0x67, 0xf4, 0x00, 0x0a, 0x91, 0x9b, 0x28, 0x41, 0x7f, 0x13, 0x80, 0x88, 0x00, 0x00, 0x03,
        0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0x91, 0x2c, 0xb0,
    ];
    const INTERLACED: &[u8] = &[
        0x67, 0x64, 0x00, 0x15, 0xac, 0xd9, 0x42, 0x1b, 0xf3, 0xe0, 0x22, 0x00, 0x00, 0x03, 0x00,
        0x02, 0x00, 0x00, 0x03, 0x00, 0x78, 0x3e, 0x28, 0x53, 0x2c,
    ];

    fn sps(profile: u8, chroma_format: u32, bit_depth: u32, frame_mbs_only: bool) -> Sps {
        Sps {
            profile,
            chroma_format,
            bit_depth,
            frame_mbs_only,
        }
    }

    #[test]
    fn real_parameter_sets_are_read() {
        assert_eq!(Sps::parse(HIGH), Some(sps(100, 1, 8, true)));
        assert_eq!(Sps::parse(BASELINE), Some(sps(66, 1, 8, true)));
        assert_eq!(Sps::parse(HIGH10), Some(sps(110, 1, 10, true)));
        assert_eq!(Sps::parse(YUV422), Some(sps(122, 2, 8, true)));
        assert_eq!(Sps::parse(YUV444), Some(sps(244, 3, 8, true)));
        assert_eq!(Sps::parse(INTERLACED), Some(sps(100, 1, 8, false)));
        for ok in [HIGH, BASELINE] {
            assert_eq!(Sps::parse(ok).and_then(|s| s.unsupported()), None);
        }
        for bad in [HIGH10, YUV422, YUV444, INTERLACED] {
            assert!(Sps::parse(bad).and_then(|s| s.unsupported()).is_some());
        }
    }

    #[test]
    fn unsupported_formats_are_named() {
        let s = sps;
        assert_eq!(
            s(110, 1, 10, true).unsupported().as_deref(),
            Some("10-bit H.264 (High 10 profile)")
        );
        assert_eq!(
            s(244, 3, 8, true).unsupported().as_deref(),
            Some("H.264 with 4:4:4 colour (High 4:4:4 profile)")
        );
        assert_eq!(
            s(122, 2, 8, true).unsupported().as_deref(),
            Some("H.264 with 4:2:2 colour (High 4:2:2 profile)")
        );
        assert_eq!(
            s(100, 1, 8, false).unsupported().as_deref(),
            Some("interlaced H.264")
        );
        assert_eq!(s(66, 1, 8, true).unsupported(), None);
    }

    #[test]
    fn nal_units_split_at_three_and_four_byte_start_codes() {
        let d = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65];
        let v: Vec<&[u8]> = nal_units(&d).collect();
        assert_eq!(v, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65]]);
        assert_eq!(nal_units(&[]).count(), 0);
        assert_eq!(nal_units(&[0, 0, 1]).count(), 0);
        assert!(has_idr(&[0, 0, 1, 0x67, 0, 0, 1, 0x65, 1]));
        assert!(!has_idr(&[0, 0, 1, 0x67, 0, 0, 1, 0x41, 1]));
        let s: Vec<Sps> = sps_units(&[[0, 0, 0, 1].as_slice(), HIGH].concat()).collect();
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn garbage_never_panics() {
        assert!(Sps::parse(&[]).is_none());
        assert!(Sps::parse(&[0x67]).is_none());
        assert!(Sps::parse(&[0x65, 0x64]).is_none());
        for n in 0..HIGH.len() {
            let _ = Sps::parse(&HIGH[..n]);
        }
        let mut x = 0x1234_5678u32;
        for _ in 0..2000 {
            let v: Vec<u8> = (0..40)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            let _ = Sps::parse(&[[0x67].as_slice(), &v].concat());
            let _ = nal_units(&v).count();
        }
    }
}
