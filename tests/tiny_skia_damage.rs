//! Partial redraws with the tiny-skia renderer (ferroiced) must give the
//! same pixels as full redraws. An animated avatar under a large profile
//! effect only damages the avatar: the renderer may (and with ferroiced's
//! damage-clipped image fills, does) re-rasterise only that region of the
//! effect, and the result must not show it.

use iced::advanced::Renderer as _;
use iced::advanced::image::Renderer as _;
use iced::advanced::image::{FilterMethod, Handle, Image};
use iced::advanced::renderer::Quad;
use iced::{Color, Font, Pixels, Point, Rectangle, Size};
use iced_tiny_skia::Renderer;
use iced_tiny_skia::graphics::Viewport;

/// Physical size of the canvas and its scale factor (a 125 % display).
const WIDTH: u32 = 100;
const HEIGHT: u32 = 80;
const SCALE: f32 = 1.25;
const BACKGROUND: Color = Color::from_rgb(0.1, 0.2, 0.3);

/// Deterministic pseudo-random numbers in `[0, 1)`.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
}

/// Translucent RGBA noise.
fn rgba(width: u32, height: u32, seed: u64) -> Handle {
    let mut rng = Lcg(seed);
    let pixels: Vec<u8> = (0..width * height * 4)
        .map(|_| (rng.next() * 256.0) as u8)
        .collect();
    Handle::from_rgba(width, height, pixels)
}

struct Scene {
    effect: Handle,
    frames: [Handle; 2],
}

/// The avatar on whole source pixels: `draw_pixmap` truncates an image's
/// origin to them, so elsewhere it may be drawn up to a pixel outside its
/// bounds (and its damage), in stock iced too.
const AVATAR: Rectangle = Rectangle {
    x: 12.0,
    y: 20.0,
    width: 16.0,
    height: 16.0,
};

/// A card (opaque quad on physical pixels, whole 8-bit colour: stock
/// tiny-skia rounds half-way values and anti-aliased edges differently with
/// and without a clip mask), an avatar showing `frame`, and a scaled,
/// translucent effect that overflows the card and is clipped to it.
fn render(
    scene: &Scene,
    frame: usize,
    damage: Rectangle,
    mut canvas: tiny_skia::Pixmap,
) -> tiny_skia::Pixmap {
    let mut renderer = Renderer::new(Font::DEFAULT, Pixels(16.0));
    let viewport = Viewport::with_physical_size(Size::new(WIDTH, HEIGHT), SCALE);
    renderer.reset(Rectangle::with_size(viewport.logical_size()));
    let card = Rectangle::new(Point::new(4.0, 3.2), Size::new(60.0, 49.6));
    renderer.fill_quad(
        Quad {
            bounds: card,
            ..Quad::default()
        },
        Color::from_rgb8(77, 26, 102),
    );
    renderer.draw_image(Image::new(scene.frames[frame].clone()), AVATAR, AVATAR);
    renderer.with_layer(card, |renderer| {
        let bounds = Rectangle::new(Point::new(-1.7, 0.4), Size::new(70.3, 116.1));
        renderer.draw_image(
            Image::new(scene.effect.clone())
                .filter_method(FilterMethod::Linear)
                .opacity(0.8_f32),
            bounds,
            card,
        );
    });
    let mut mask = tiny_skia::Mask::new(WIDTH, HEIGHT).expect("mask");
    renderer.draw(
        &mut canvas.as_mut(),
        &mut mask,
        &viewport,
        &[damage],
        BACKGROUND,
    );
    canvas
}

fn first_difference(a: &tiny_skia::Pixmap, b: &tiny_skia::Pixmap) -> Option<(u32, u32)> {
    let i = a
        .pixels()
        .iter()
        .zip(b.pixels())
        .position(|(x, y)| x != y)?;
    Some((i as u32 % WIDTH, i as u32 / WIDTH))
}

#[test]
fn a_partial_redraw_of_an_animation_matches_a_full_redraw() {
    let scene = Scene {
        effect: rgba(37, 61, 1),
        frames: [rgba(8, 8, 2), rgba(8, 8, 3)],
    };
    let full = Rectangle::with_size(Size::new(WIDTH as f32 / SCALE, HEIGHT as f32 / SCALE));
    let blank = tiny_skia::Pixmap::new(WIDTH, HEIGHT).expect("pixmap");
    let expected = render(&scene, 1, full, blank.clone());
    let previous = render(&scene, 0, full, blank);
    assert_ne!(
        first_difference(&expected, &previous),
        None,
        "the frames must differ"
    );

    let mut rng = Lcg(23);
    for case in 0..150 {
        // The compositor rounds damage out to physical pixels (0.8 logical
        // here); it always holds the avatar, plus whatever else changed.
        let px = |value: f32| (value / 0.8).floor() * 0.8;
        let extra = Rectangle::new(
            Point::new(px(rng.next() * 70.0), px(rng.next() * 55.0)),
            Size::new(px(rng.next() * 30.0) + 0.8, px(rng.next() * 25.0) + 0.8),
        );
        let damage = if case == 0 {
            AVATAR
        } else {
            AVATAR.union(&extra)
        };
        let actual = render(&scene, 1, damage, previous.clone());
        assert_eq!(
            first_difference(&expected, &actual),
            None,
            "damage {damage:?}: a partial redraw differs from a full one"
        );
    }
}
