//! Renders the bundled SVG icon to the ARGB32 pixmaps the
//! StatusNotifierItem protocol carries.

use resvg::{tiny_skia, usvg};

/// Pixmap sizes offered to the panel; it picks the closest one.
pub const SIZES: [u32; 3] = [22, 32, 48];

const ICON_SVG: &str = include_str!("../../assets/ferrocord.svg");
const BADGE_SVG: &str = include_str!("../../assets/ferrocord-badge.svg");

/// The normal icon and the "unread" variant with a red badge.
pub struct Icons {
    pub normal: Vec<ksni::Icon>,
    pub attention: Vec<ksni::Icon>,
}

impl Icons {
    /// Renders every size; `None` if the bundled SVG fails to parse.
    pub fn render() -> Option<Self> {
        let mut normal = Vec::new();
        let mut attention = Vec::new();
        for size in SIZES {
            let base = render_svg(ICON_SVG, size)?;
            let mut badged = base.clone();
            // Red dot with a white ring in the bottom-right corner, 45 % of
            // the icon, like the unread badge of the official client.
            let badge_size = (size as f32 * 0.45).round().max(1.0) as u32;
            let badge = render_svg(BADGE_SVG, badge_size)?;
            let at = (size - badge_size) as i32;
            badged.draw_pixmap(
                at,
                at,
                badge.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                tiny_skia::Transform::identity(),
                None,
            );
            normal.push(to_argb(&base));
            attention.push(to_argb(&badged));
        }
        Some(Self { normal, attention })
    }
}

fn render_svg(svg: &str, size: u32) -> Option<tiny_skia::Pixmap> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / tree.size().width().max(1.0);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(pixmap)
}

/// tiny-skia stores premultiplied RGBA; SNI wants straight ARGB32 in
/// network byte order (A, R, G, B).
fn to_argb(p: &tiny_skia::Pixmap) -> ksni::Icon {
    let mut data = Vec::with_capacity(p.pixels().len() * 4);
    for px in p.pixels() {
        let c = px.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    ksni::Icon {
        width: p.width() as i32,
        height: p.height() as i32,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_every_size_as_argb() {
        let icons = Icons::render().unwrap();
        assert_eq!(icons.normal.len(), SIZES.len());
        for (icon, size) in icons.normal.iter().zip(SIZES) {
            assert_eq!(icon.width, size as i32);
            assert_eq!(icon.data.len(), (size * size * 4) as usize);
        }
        // Rounded square: the corner is transparent, the centre is opaque
        // blurple (#5865f2).
        let i = &icons.normal[2];
        let px = |x: usize, y: usize| {
            let o = (y * i.width as usize + x) * 4;
            &i.data[o..o + 4]
        };
        assert_eq!(px(0, 0)[0], 0);
        let c = px(10, 40);
        assert_eq!(c, &[255, 0x58, 0x65, 0xf2]);
    }

    #[test]
    fn attention_variant_has_a_red_badge() {
        let icons = Icons::render().unwrap();
        let (n, a) = (&icons.normal[2], &icons.attention[2]);
        assert_ne!(n.data, a.data);
        // Centre of the badge in the bottom-right corner is red.
        let o = (38 * a.width as usize + 38) * 4;
        assert_eq!(&a.data[o..o + 4], &[255, 0xf2, 0x3f, 0x43]);
    }
}
