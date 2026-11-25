//! YUV 4:2:0 → RGBA with scaling and rotation in one pass (nearest
//! sample). Frames are at most 960x540 on the way out, so the cost is small
//! and no intermediate full-size RGBA buffer is needed.

/// Clockwise rotation from the track header's display matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// As stored.
    #[default]
    None,
    /// 90° clockwise (portrait phone videos).
    Cw90,
    /// Upside down.
    Cw180,
    /// 90° counter-clockwise.
    Cw270,
}

impl Rotation {
    /// From the `tkhd` matrix entries `a, b, c, d` (16.16 fixed point).
    pub fn from_matrix(a: i32, b: i32, c: i32, d: i32) -> Self {
        match (a.signum(), b.signum(), c.signum(), d.signum()) {
            (0, 1, -1, 0) => Rotation::Cw90,
            (-1, 0, 0, -1) => Rotation::Cw180,
            (0, -1, 1, 0) => Rotation::Cw270,
            _ => Rotation::None,
        }
    }

    /// Display size of a `w`x`h` picture.
    pub fn display(self, w: usize, h: usize) -> (usize, usize) {
        match self {
            Rotation::Cw90 | Rotation::Cw270 => (h, w),
            _ => (w, h),
        }
    }
}

/// Largest output size keeping the aspect ratio (never upscales).
pub fn fit(w: usize, h: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    if w == 0 || h == 0 {
        return (0, 0);
    }
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    (
        ((w as f64 * s).round() as usize).max(1),
        ((h as f64 * s).round() as usize).max(1),
    )
}

/// Borrowed planes of one decoded picture.
pub struct Planes<'a> {
    /// Luma.
    pub y: &'a [u8],
    /// Cb.
    pub u: &'a [u8],
    /// Cr.
    pub v: &'a [u8],
    /// Strides of y, u, v.
    pub strides: (usize, usize, usize),
    /// Picture size (luma).
    pub width: usize,
    /// Picture height.
    pub height: usize,
    /// Horizontal chroma subsampling shift (1 for 4:2:0 / 4:2:2, 0 for 4:4:4).
    pub sub_x: u32,
    /// Vertical chroma subsampling shift (1 for 4:2:0, 0 for 4:2:2 / 4:4:4).
    pub sub_y: u32,
}

/// Colour matrix (limited range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    /// SD.
    Bt601,
    /// HD.
    Bt709,
}

impl Matrix {
    /// The usual choice when the stream does not say: HD sizes are BT.709.
    pub fn for_height(h: usize) -> Self {
        if h >= 720 {
            Matrix::Bt709
        } else {
            Matrix::Bt601
        }
    }

    /// (r_v, g_u, g_v, b_u) in 1/1024 units.
    fn coeffs(self) -> (i32, i32, i32, i32) {
        match self {
            Matrix::Bt601 => (1634, 401, 832, 2066),
            Matrix::Bt709 => (1836, 218, 546, 2163),
        }
    }
}

/// Converts `p` to an `out_w`x`out_h` RGBA picture shown with `rot`.
/// Returns `None` when the planes are smaller than the declared size.
pub fn to_rgba(
    p: &Planes<'_>,
    rot: Rotation,
    out_w: usize,
    out_h: usize,
    m: Matrix,
) -> Option<Vec<u8>> {
    let (w, h) = (p.width, p.height);
    if w == 0 || h == 0 || out_w == 0 || out_h == 0 {
        return None;
    }
    let (ys, us, vs) = p.strides;
    if p.sub_x > 1 || p.sub_y > 1 {
        return None;
    }
    let (cw, ch) = (
        (w + (1 << p.sub_x) - 1) >> p.sub_x,
        (h + (1 << p.sub_y) - 1) >> p.sub_y,
    );
    if p.y.len() < ys * (h - 1) + w
        || p.u.len() < us * (ch - 1) + cw
        || p.v.len() < vs * (ch - 1) + cw
    {
        return None;
    }
    let (dw, dh) = rot.display(w, h);
    let (rv, gu, gv, bu) = m.coeffs();
    let mut out = vec![0u8; out_w * out_h * 4];
    for oy in 0..out_h {
        // Position in the displayed (rotated) picture.
        let dy = (oy * dh + dh / (2 * out_h)) / out_h;
        for ox in 0..out_w {
            let dx = (ox * dw + dw / (2 * out_w)) / out_w;
            // Back to the stored picture.
            let (sx, sy) = match rot {
                Rotation::None => (dx, dy),
                Rotation::Cw90 => (dy, h - 1 - dx),
                Rotation::Cw180 => (w - 1 - dx, h - 1 - dy),
                Rotation::Cw270 => (w - 1 - dy, dx),
            };
            let (sx, sy) = (sx.min(w - 1), sy.min(h - 1));
            let yv = (i32::from(p.y[sy * ys + sx]) - 16) * 1192;
            let (cx, cy) = (sx >> p.sub_x, sy >> p.sub_y);
            let u = i32::from(p.u[cy * us + cx]) - 128;
            let v = i32::from(p.v[cy * vs + cx]) - 128;
            let r = (yv + rv * v) >> 10;
            let g = (yv - gu * u - gv * v) >> 10;
            let b = (yv + bu * u) >> 10;
            let o = (oy * out_w + ox) * 4;
            out[o] = r.clamp(0, 255) as u8;
            out[o + 1] = g.clamp(0, 255) as u8;
            out[o + 2] = b.clamp(0, 255) as u8;
            out[o + 3] = 255;
        }
    }
    Some(out)
}

/// How the samples of a platform decoder's picture are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// One byte per sample.
    Eight,
    /// Little-endian 16-bit words; `word >> shift` is the 8-bit value
    /// (2 for 10-bit planar `yuv420p10le`, 8 for MSB-aligned `P010`).
    Wide {
        /// Right shift down to 8 bits.
        shift: u32,
    },
}

/// Layout of a decoded picture as platform decoders hand it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Horizontal chroma subsampling shift.
    pub sub_x: u32,
    /// Vertical chroma subsampling shift.
    pub sub_y: u32,
    /// Cb and Cr interleaved in one plane (`NV12`, `P010`).
    pub interleaved: bool,
    /// Sample size.
    pub depth: Depth,
}

impl Layout {
    /// 8-bit planar 4:2:0, the common case.
    pub const I420: Layout = Layout {
        sub_x: 1,
        sub_y: 1,
        interleaved: false,
        depth: Depth::Eight,
    };
    /// 8-bit 4:2:0 with interleaved chroma.
    pub const NV12: Layout = Layout {
        sub_x: 1,
        sub_y: 1,
        interleaved: true,
        depth: Depth::Eight,
    };
    /// 10-bit (MSB-aligned) 4:2:0 with interleaved chroma.
    pub const P010: Layout = Layout {
        sub_x: 1,
        sub_y: 1,
        interleaved: true,
        depth: Depth::Wide { shift: 8 },
    };
}

/// A picture as raw planes: `planes[1]` holds both chroma components when
/// the layout is interleaved (`planes[2]` is then unused).
pub struct Raw<'a> {
    /// Luma, Cb (or CbCr), Cr.
    pub planes: [&'a [u8]; 3],
    /// Bytes per row of each plane.
    pub strides: [usize; 3],
    /// Visible picture width.
    pub width: usize,
    /// Visible picture height.
    pub height: usize,
    /// How the planes are stored.
    pub layout: Layout,
}

/// Converts any [`Raw`] picture to RGBA like [`to_rgba`]: 8-bit planar
/// pictures are read in place, the others are first narrowed to 8-bit
/// planes. `None` when the planes are smaller than the layout says.
pub fn raw_to_rgba(
    p: &Raw<'_>,
    rot: Rotation,
    out_w: usize,
    out_h: usize,
    m: Matrix,
) -> Option<Vec<u8>> {
    let l = p.layout;
    if l.sub_x > 1 || l.sub_y > 1 || p.width == 0 || p.height == 0 {
        return None;
    }
    if l.depth == Depth::Eight && !l.interleaved {
        return to_rgba(
            &Planes {
                y: p.planes[0],
                u: p.planes[1],
                v: p.planes[2],
                strides: (p.strides[0], p.strides[1], p.strides[2]),
                width: p.width,
                height: p.height,
                sub_x: l.sub_x,
                sub_y: l.sub_y,
            },
            rot,
            out_w,
            out_h,
            m,
        );
    }
    let (w, h) = (p.width, p.height);
    let (cw, ch) = (
        (w + (1 << l.sub_x) - 1) >> l.sub_x,
        (h + (1 << l.sub_y) - 1) >> l.sub_y,
    );
    let bytes = if l.depth == Depth::Eight { 1 } else { 2 };
    let sample = |plane: &[u8], stride: usize, x: usize, y: usize| -> Option<u8> {
        let at = y.checked_mul(stride)?.checked_add(x * bytes)?;
        match l.depth {
            Depth::Eight => plane.get(at).copied(),
            Depth::Wide { shift } => {
                let word = u16::from_le_bytes([*plane.get(at)?, *plane.get(at + 1)?]);
                Some((word >> shift.min(15)).min(255) as u8)
            }
        }
    };
    let mut y = Vec::with_capacity(w * h);
    for row in 0..h {
        for x in 0..w {
            y.push(sample(p.planes[0], p.strides[0], x, row)?);
        }
    }
    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for row in 0..ch {
        for x in 0..cw {
            if l.interleaved {
                u.push(sample(p.planes[1], p.strides[1], 2 * x, row)?);
                v.push(sample(p.planes[1], p.strides[1], 2 * x + 1, row)?);
            } else {
                u.push(sample(p.planes[1], p.strides[1], x, row)?);
                v.push(sample(p.planes[2], p.strides[2], x, row)?);
            }
        }
    }
    to_rgba(
        &Planes {
            y: &y,
            u: &u,
            v: &v,
            strides: (w, cw, cw),
            width: w,
            height: h,
            sub_x: l.sub_x,
            sub_y: l.sub_y,
        },
        rot,
        out_w,
        out_h,
        m,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x2 picture: luma ramp, two chroma pairs.
    fn reference() -> Vec<u8> {
        let p = planes(
            &[16, 80, 160, 235, 50, 90, 130, 170],
            &[90, 200],
            &[240, 60],
            4,
            2,
        );
        to_rgba(&p, Rotation::None, 4, 2, Matrix::Bt601).unwrap()
    }

    #[test]
    fn nv12_and_p010_and_10_bit_planar_match_8_bit_planar() {
        let want = reference();
        let y = [16u8, 80, 160, 235, 50, 90, 130, 170];
        // NV12 with padded rows (stride 6).
        let mut luma = Vec::new();
        for r in y.chunks(4) {
            luma.extend_from_slice(r);
            luma.extend_from_slice(&[0, 0]);
        }
        let uv = [90u8, 240, 200, 60, 0, 0];
        let nv12 = Raw {
            planes: [&luma, &uv, &[]],
            strides: [6, 6, 0],
            width: 4,
            height: 2,
            layout: Layout::NV12,
        };
        assert_eq!(
            raw_to_rgba(&nv12, Rotation::None, 4, 2, Matrix::Bt601).unwrap(),
            want
        );
        // P010: the same values in the top 8 bits of 16-bit words.
        let wide = |b: &[u8]| -> Vec<u8> {
            b.iter()
                .flat_map(|&v| ((u16::from(v) << 8) | 0x40).to_le_bytes())
                .collect()
        };
        let (y16, uv16) = (wide(&y), wide(&uv[..4]));
        let p010 = Raw {
            planes: [&y16, &uv16, &[]],
            strides: [8, 8, 0],
            width: 4,
            height: 2,
            layout: Layout::P010,
        };
        assert_eq!(
            raw_to_rgba(&p010, Rotation::None, 4, 2, Matrix::Bt601).unwrap(),
            want
        );
        // yuv420p10le: low-aligned 10-bit words.
        let ten = |b: &[u8]| -> Vec<u8> {
            b.iter()
                .flat_map(|&v| ((u16::from(v) << 2) | 1).to_le_bytes())
                .collect()
        };
        let (y10, u10, v10) = (ten(&y), ten(&[90, 200]), ten(&[240, 60]));
        let planar10 = Raw {
            planes: [&y10, &u10, &v10],
            strides: [8, 4, 4],
            width: 4,
            height: 2,
            layout: Layout {
                depth: Depth::Wide { shift: 2 },
                ..Layout::I420
            },
        };
        assert_eq!(
            raw_to_rgba(&planar10, Rotation::None, 4, 2, Matrix::Bt601).unwrap(),
            want
        );
    }

    #[test]
    fn short_raw_planes_are_rejected() {
        let nv12 = Raw {
            planes: [&[0; 8], &[0; 3], &[]],
            strides: [4, 4, 0],
            width: 4,
            height: 2,
            layout: Layout::NV12,
        };
        assert!(raw_to_rgba(&nv12, Rotation::None, 4, 2, Matrix::Bt601).is_none());
        let p010 = Raw {
            planes: [&[0; 15], &[0; 8], &[]],
            strides: [8, 8, 0],
            width: 4,
            height: 2,
            layout: Layout::P010,
        };
        assert!(raw_to_rgba(&p010, Rotation::None, 4, 2, Matrix::Bt601).is_none());
        let huge = Raw {
            planes: [&[0; 8], &[0; 8], &[]],
            strides: [usize::MAX, 4, 0],
            width: 4,
            height: 2,
            layout: Layout::NV12,
        };
        assert!(raw_to_rgba(&huge, Rotation::None, 4, 2, Matrix::Bt601).is_none());
    }

    fn planes<'a>(y: &'a [u8], u: &'a [u8], v: &'a [u8], w: usize, h: usize) -> Planes<'a> {
        Planes {
            y,
            u,
            v,
            strides: (w, w / 2, w / 2),
            width: w,
            height: h,
            sub_x: 1,
            sub_y: 1,
        }
    }

    #[test]
    fn rotation_from_the_track_matrix() {
        assert_eq!(
            Rotation::from_matrix(0x10000, 0, 0, 0x10000),
            Rotation::None
        );
        assert_eq!(
            Rotation::from_matrix(0, 0x10000, -0x10000, 0),
            Rotation::Cw90
        );
        assert_eq!(
            Rotation::from_matrix(-0x10000, 0, 0, -0x10000),
            Rotation::Cw180
        );
        assert_eq!(
            Rotation::from_matrix(0, -0x10000, 0x10000, 0),
            Rotation::Cw270
        );
        assert_eq!(Rotation::Cw90.display(640, 360), (360, 640));
    }

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fit(1920, 1080, 960, 540), (960, 540));
        assert_eq!(fit(1080, 1920, 960, 540), (304, 540));
        assert_eq!(fit(320, 180, 960, 540), (320, 180));
        assert_eq!(fit(0, 10, 960, 540), (0, 0));
    }

    #[test]
    fn grey_and_primaries_convert() {
        // 2x2, all mid grey.
        let p = planes(&[126; 4], &[128], &[128], 2, 2);
        let px = to_rgba(&p, Rotation::None, 2, 2, Matrix::Bt601).unwrap();
        assert!(px.chunks(4).all(|c| (c[0] as i32 - 128).abs() <= 2
            && c[0] == c[1]
            && c[1] == c[2]
            && c[3] == 255));
        // Pure red in BT.601 limited range: Y=81 U=90 V=240.
        let p = planes(&[81; 4], &[90], &[240], 2, 2);
        let px = to_rgba(&p, Rotation::None, 1, 1, Matrix::Bt601).unwrap();
        assert!(px[0] > 240 && px[1] < 15 && px[2] < 15, "{px:?}");
    }

    #[test]
    fn rotation_moves_pixels_where_the_phone_showed_them() {
        // 4x2 luma: left half dark, right half bright (stored landscape).
        let y = [16, 16, 235, 235, 16, 16, 235, 235];
        let p = planes(&y, &[128, 128], &[128, 128], 4, 2);
        // Rotated 90° clockwise the picture is 2x4 and the bright part is at the bottom.
        let px = to_rgba(&p, Rotation::Cw90, 2, 4, Matrix::Bt601).unwrap();
        let top = px[0];
        let bottom = px[(3 * 2) * 4];
        assert!(top < 20 && bottom > 230, "top {top} bottom {bottom}");
    }

    #[test]
    fn full_resolution_chroma_is_read_per_pixel() {
        // 2x1 4:4:4: left pixel red-ish chroma, right pixel blue-ish chroma.
        let p = Planes {
            y: &[81, 41],
            u: &[90, 240],
            v: &[240, 110],
            strides: (2, 2, 2),
            width: 2,
            height: 1,
            sub_x: 0,
            sub_y: 0,
        };
        let px = to_rgba(&p, Rotation::None, 2, 1, Matrix::Bt601).unwrap();
        assert!(px[0] > 200 && px[2] < 40, "left {px:?}");
        assert!(px[6] > 200 && px[4] < 80, "right {px:?}");
        // Invalid shifts are refused.
        let bad = Planes { sub_x: 5, ..p };
        assert!(to_rgba(&bad, Rotation::None, 2, 1, Matrix::Bt601).is_none());
    }

    #[test]
    fn short_planes_are_rejected_not_read_out_of_bounds() {
        let p = planes(&[0; 3], &[0], &[0], 2, 2);
        assert!(to_rgba(&p, Rotation::None, 2, 2, Matrix::Bt601).is_none());
    }
}
