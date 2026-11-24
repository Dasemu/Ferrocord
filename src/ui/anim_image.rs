//! A widget that plays an [`Anim`] by itself.
//!
//! The app never ticks for it: on every redraw the widget records the
//! frame time and, while it is playing and inside the visible viewport,
//! asks the runtime for the next redraw exactly when its frame changes
//! (`Shell::request_redraw_at`). No `Message` is produced, so `view()` and
//! layout do not run per frame, and tiny-skia's damage tracking repaints
//! only the image's rectangle. Off-screen or paused images request nothing.

use std::sync::Arc;
use std::time::Instant;

use iced::advanced::image as adv_image;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::widget::image::Handle;
use iced::{ContentFit, Element, Event, Length, Rectangle, Size, mouse, window};

use crate::app::anim::Anim;

/// An animated image of a fixed size.
pub struct AnimImage {
    anim: Arc<Anim>,
    epoch: Instant,
    playing: bool,
    width: f32,
    height: f32,
    fit: ContentFit,
    radius: f32,
}

/// `anim` drawn in a `width` x `height` box. All animations share the
/// app's `epoch` so they stay in step across view rebuilds.
pub fn anim_image(
    anim: Arc<Anim>,
    epoch: Instant,
    playing: bool,
    width: f32,
    height: f32,
) -> AnimImage {
    AnimImage {
        anim,
        epoch,
        playing,
        width,
        height,
        fit: ContentFit::Fill,
        radius: 0.0,
    }
}

/// A message animation from the app's cache, playing while the window is
/// focused.
pub fn message_anim(app: &crate::app::App, anim: &Arc<Anim>, width: f32, height: f32) -> AnimImage {
    anim_image(
        anim.clone(),
        app.anim.epoch,
        app.media_playing(),
        width,
        height,
    )
}

impl AnimImage {
    pub fn content_fit(mut self, fit: ContentFit) -> Self {
        self.fit = fit;
        self
    }

    pub fn border_radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }
}

#[derive(Debug, Default)]
struct State {
    /// Time of the last redraw: the frame shown is picked from it.
    now: Option<Instant>,
    /// The redraw time this image asked for last (frame statistics).
    asked: Option<Instant>,
}

/// Frame index to draw: the first frame while paused.
pub fn shown_index(anim: &Anim, epoch: Instant, now: Option<Instant>, playing: bool) -> usize {
    match now {
        Some(now) if playing => anim.index_at(now.saturating_duration_since(epoch)),
        _ => 0,
    }
}

/// When the next redraw is due, or `None` when nothing will change.
pub fn next_redraw(anim: &Anim, epoch: Instant, now: Instant, playing: bool) -> Option<Instant> {
    if !playing {
        return None;
    }
    let wait = anim.until_next(now.saturating_duration_since(epoch))?;
    Some(now + wait)
}

impl<M, T, R> Widget<M, T, R> for AnimImage
where
    R: adv_image::Renderer<Handle = Handle> + renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Fixed(self.height))
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &R, limits: &layout::Limits) -> layout::Node {
        let size = Size::new(self.width, self.height);
        layout::Node::new(limits.resolve(
            Length::Fixed(size.width),
            Length::Fixed(size.height),
            size,
        ))
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &R,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = tree.state.downcast_mut::<State>();
            let visible = layout.bounds().intersects(viewport);
            if super::frame_stats::enabled() && visible && self.playing {
                let before = shown_index(&self.anim, self.epoch, state.now, self.playing);
                let after = shown_index(&self.anim, self.epoch, Some(*now), self.playing);
                super::frame_stats::record(*now, state.asked.take(), before != after);
            }
            state.now = Some(*now);
            // Off-screen images stop asking; scrolling back redraws them.
            if visible && let Some(at) = next_redraw(&self.anim, self.epoch, *now, self.playing) {
                state.asked = Some(at);
                shell.request_redraw_at(at);
            }
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut R,
        _theme: &T,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let Some(frame) =
            self.anim
                .frames
                .get(shown_index(&self.anim, self.epoch, state.now, self.playing))
        else {
            return;
        };
        let bounds = layout.bounds();
        let (w, h) = self.anim.size;
        let fitted = self.fit.fit(Size::new(w as f32, h as f32), bounds.size());
        let drawn = Rectangle {
            x: bounds.x + (bounds.width - fitted.width) / 2.0,
            y: bounds.y + (bounds.height - fitted.height) / 2.0,
            width: fitted.width,
            height: fitted.height,
        };
        let mut image = adv_image::Image::new(frame.handle.clone());
        image.border_radius = self.radius.into();
        renderer.draw_image(image, drawn, bounds);
    }
}

impl<'a, M, T, R> From<AnimImage> for Element<'a, M, T, R>
where
    R: adv_image::Renderer<Handle = Handle> + renderer::Renderer + 'a,
{
    fn from(image: AnimImage) -> Self {
        Element::new(image)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::app::anim::from_rgba_frames;

    fn three_frames() -> Anim {
        let px = || vec![0u8; 4];
        from_rgba_frames(vec![
            (1, 1, px(), Duration::from_millis(100)),
            (1, 1, px(), Duration::from_millis(50)),
            (1, 1, px(), Duration::from_millis(200)),
        ])
    }

    #[test]
    fn paused_shows_the_first_frame_and_asks_nothing() {
        let a = three_frames();
        let t0 = Instant::now();
        let later = t0 + Duration::from_millis(120);
        assert_eq!(shown_index(&a, t0, Some(later), false), 0);
        assert_eq!(next_redraw(&a, t0, later, false), None);
    }

    #[test]
    fn playing_follows_the_frame_delays() {
        let a = three_frames();
        let t0 = Instant::now();
        assert_eq!(shown_index(&a, t0, None, true), 0);
        assert_eq!(
            shown_index(&a, t0, Some(t0 + Duration::from_millis(120)), true),
            1
        );
        assert_eq!(
            shown_index(&a, t0, Some(t0 + Duration::from_millis(200)), true),
            2
        );
        // Loops after the 350 ms total.
        assert_eq!(
            shown_index(&a, t0, Some(t0 + Duration::from_millis(360)), true),
            0
        );
    }

    #[test]
    fn next_redraw_lands_on_the_frame_change() {
        let a = three_frames();
        let t0 = Instant::now();
        let now = t0 + Duration::from_millis(120);
        assert_eq!(
            next_redraw(&a, t0, now, true),
            Some(now + Duration::from_millis(30))
        );
    }

    #[test]
    fn a_still_image_never_redraws() {
        let a = from_rgba_frames(vec![(1, 1, vec![0u8; 4], Duration::from_millis(100))]);
        let t0 = Instant::now();
        assert_eq!(next_redraw(&a, t0, t0, true), None);
    }
}
