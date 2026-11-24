//! A widget that plays an [`Anim`] by itself.
//!
//! The app never ticks for it: on every redraw the widget records the
//! frame time and, while it is playing and inside the visible viewport,
//! asks the runtime for the next redraw exactly when its frame changes
//! (`Shell::request_redraw_at`). No `Message` is produced, so `view()` and
//! layout do not run per frame, and tiny-skia's damage tracking repaints
//! only the image's rectangle. Off-screen or paused images request nothing.
//!
//! Redraw times are rounded up to a 60 Hz grid shared by every image
//! ([`FRAME_SLOT`] from the app's epoch), so any number of animations with
//! unrelated frame delays cost at most 60 window redraws per second (a
//! display cannot show more), each frame still landing on its own delay.
//! An optional [`Timeline`] shows the animation only during part of a
//! cycle (profile-effect layers).

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::advanced::image as adv_image;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::widget::image::Handle;
use iced::{ContentFit, Element, Event, Length, Rectangle, Size, mouse, window};

use crate::app::anim::Anim;

/// Redraws are aligned to this grid (one 60 Hz display frame).
pub const FRAME_SLOT: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// When a layer is on screen, relative to the epoch: from `start`, for
/// `duration`, then (when `looped`) again after `loop_delay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeline {
    pub start: Duration,
    pub duration: Duration,
    pub looped: bool,
    pub loop_delay: Duration,
}

impl Timeline {
    /// Time into the animation `t` after the epoch, or `None` while the
    /// layer is hidden (not started, between loops, or over).
    pub fn local(&self, t: Duration) -> Option<Duration> {
        let t = t.checked_sub(self.start)?;
        let dur = self.duration.max(Duration::from_millis(1));
        if !self.looped {
            return (t < dur).then_some(t);
        }
        let period = dur + self.loop_delay;
        let r = Duration::from_nanos((t.as_nanos() % period.as_nanos()) as u64);
        (r < dur).then_some(r)
    }

    /// Time from `t` until the layer appears or disappears next.
    pub fn until_change(&self, t: Duration) -> Option<Duration> {
        let Some(t) = t.checked_sub(self.start) else {
            return Some(self.start - t);
        };
        let dur = self.duration.max(Duration::from_millis(1));
        if !self.looped {
            return dur.checked_sub(t).filter(|d| !d.is_zero());
        }
        let period = dur + self.loop_delay;
        let r = Duration::from_nanos((t.as_nanos() % period.as_nanos()) as u64);
        Some(if r < dur { dur - r } else { period - r })
    }
}

/// An animated image of a fixed size.
pub struct AnimImage {
    anim: Arc<Anim>,
    epoch: Instant,
    playing: bool,
    width: f32,
    height: f32,
    fit: ContentFit,
    radius: f32,
    timeline: Option<Timeline>,
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
        timeline: None,
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

    /// Shows the animation only while `timeline` says so (time counted
    /// from the epoch; the animation restarts with each appearance).
    pub fn timeline(mut self, timeline: Timeline) -> Self {
        self.timeline = Some(timeline);
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

/// Time into the animation at `now` (`None`: hidden by the timeline).
fn local_time(
    epoch: Instant,
    now: Option<Instant>,
    playing: bool,
    timeline: Option<&Timeline>,
) -> Option<Duration> {
    let t = match now {
        Some(now) if playing => now.saturating_duration_since(epoch),
        _ => Duration::ZERO,
    };
    match timeline {
        // A paused layer shows its first frame.
        Some(tl) if playing => tl.local(t),
        _ => Some(t),
    }
}

/// Frame index to draw: the first frame while paused.
pub fn shown_index(anim: &Anim, epoch: Instant, now: Option<Instant>, playing: bool) -> usize {
    shown_frame(anim, epoch, now, playing, None).unwrap_or(0)
}

/// Frame index to draw, `None` while a timeline hides the image.
pub fn shown_frame(
    anim: &Anim,
    epoch: Instant,
    now: Option<Instant>,
    playing: bool,
    timeline: Option<&Timeline>,
) -> Option<usize> {
    local_time(epoch, now, playing, timeline).map(|t| anim.index_at(t))
}

/// `at` rounded up to the shared 60 Hz grid counted from `epoch`.
pub fn snap_to_slot(epoch: Instant, at: Instant) -> Instant {
    let since = at.saturating_duration_since(epoch).as_nanos();
    let slot = FRAME_SLOT.as_nanos();
    let k = since.div_ceil(slot);
    epoch + Duration::from_nanos((k * slot) as u64)
}

/// When the next redraw is due, or `None` when nothing will change.
pub fn next_redraw(anim: &Anim, epoch: Instant, now: Instant, playing: bool) -> Option<Instant> {
    next_redraw_in(anim, epoch, now, playing, None)
}

/// [`next_redraw`] with an optional timeline: the earlier of the next
/// frame change and the layer appearing / disappearing, on the 60 Hz grid.
pub fn next_redraw_in(
    anim: &Anim,
    epoch: Instant,
    now: Instant,
    playing: bool,
    timeline: Option<&Timeline>,
) -> Option<Instant> {
    if !playing {
        return None;
    }
    let t = now.saturating_duration_since(epoch);
    let wait = match timeline {
        None => anim.until_next(t)?,
        Some(tl) => {
            let edge = tl.until_change(t);
            let frame = tl.local(t).and_then(|l| anim.until_next(l));
            match (edge, frame) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => a.or(b)?,
            }
        }
    };
    let at = snap_to_slot(epoch, now + wait);
    // Never ask for the current instant again (a redraw loop).
    Some(if at <= now { now + FRAME_SLOT } else { at })
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
            let tl = self.timeline.as_ref();
            let asked = state.asked.take();
            if super::frame_stats::enabled() && visible && self.playing {
                let before = shown_frame(&self.anim, self.epoch, state.now, true, tl);
                let after = shown_frame(&self.anim, self.epoch, Some(*now), true, tl);
                super::frame_stats::record(*now, asked, before != after);
            }
            state.now = Some(*now);
            // Off-screen images stop asking; scrolling back redraws them.
            if visible
                && let Some(at) = next_redraw_in(&self.anim, self.epoch, *now, self.playing, tl)
            {
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
        let Some(frame) = shown_frame(
            &self.anim,
            self.epoch,
            state.now,
            self.playing,
            self.timeline.as_ref(),
        )
        .and_then(|i| self.anim.frames.get(i)) else {
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
        // A covering image overflows its box: clip it like iced's `image`.
        if fitted.width > bounds.width + 0.5 || fitted.height > bounds.height + 0.5 {
            renderer.with_layer(bounds, |r| r.draw_image(image, drawn, bounds));
        } else {
            renderer.draw_image(image, drawn, bounds);
        }
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
        // The change is at 150 ms; the redraw lands on the first 60 Hz
        // slot at or after it, never before.
        let at = next_redraw(&a, t0, now, true).unwrap();
        assert!(at >= t0 + Duration::from_millis(150));
        assert!(at <= t0 + Duration::from_millis(150) + FRAME_SLOT);
        assert_eq!(shown_index(&a, t0, Some(at), true), 2);
    }

    #[test]
    fn a_still_image_never_redraws() {
        let a = from_rgba_frames(vec![(1, 1, vec![0u8; 4], Duration::from_millis(100))]);
        let t0 = Instant::now();
        assert_eq!(next_redraw(&a, t0, t0, true), None);
    }

    fn anim_ms(delays: &[u64]) -> Anim {
        from_rgba_frames(
            delays
                .iter()
                .map(|d| (1, 1, vec![0u8; 4], Duration::from_millis(*d)))
                .collect(),
        )
    }

    /// Follows the redraws an image asks for over `span` and returns the
    /// times its frame changed on screen.
    fn simulate(a: &Anim, t0: Instant, span: Duration) -> Vec<(Duration, usize)> {
        let mut now = t0;
        let mut shown = shown_index(a, t0, Some(now), true);
        let mut out = vec![(Duration::ZERO, shown)];
        while let Some(at) = next_redraw(a, t0, now, true) {
            assert!(at > now, "redraw loop at {:?}", now - t0);
            now = at;
            if now - t0 > span {
                break;
            }
            let i = shown_index(a, t0, Some(now), true);
            if i != shown {
                out.push((now - t0, i));
                shown = i;
            }
        }
        out
    }

    #[test]
    fn every_frame_is_shown_on_its_own_delay_within_one_display_frame() {
        // An APNG decoration at 60 fps and a GIF with uneven delays.
        for delays in [vec![17u64; 60], vec![20, 40, 70, 30, 100, 20]] {
            let a = anim_ms(&delays);
            let t0 = Instant::now();
            let changes = simulate(&a, t0, Duration::from_secs(3));
            // Ideal change times: the running sum of the delays.
            let mut ideal = Duration::ZERO;
            let mut i = 0usize;
            for (at, idx) in changes.iter().skip(1) {
                ideal += Duration::from_millis(delays[i % delays.len()]);
                i += 1;
                // Never early, at most one 60 Hz slot late, and no frame
                // is skipped when it lasts at least one slot.
                assert!(*at >= ideal, "frame {idx} early: {at:?} < {ideal:?}");
                assert!(
                    *at <= ideal + FRAME_SLOT,
                    "frame {idx} late: {at:?} vs {ideal:?}"
                );
                assert_eq!(*idx, i % delays.len());
            }
            assert!(changes.len() > 20);
        }
    }

    #[test]
    fn redraws_of_unrelated_animations_share_the_60_hz_grid() {
        let a = anim_ms(&[23, 23, 23]);
        let b = anim_ms(&[37, 41]);
        let t0 = Instant::now();
        let now = t0 + Duration::from_millis(5);
        for x in [&a, &b] {
            let at = next_redraw(x, t0, now, true).unwrap();
            let since = (at - t0).as_nanos();
            assert_eq!(since % FRAME_SLOT.as_nanos(), 0, "{since}");
        }
        // Many animations together never cause more than 60 redraws a
        // second: every asked time is a grid slot.
        let mut slots = std::collections::BTreeSet::new();
        for d in 11..60u64 {
            let x = anim_ms(&[d, d + 3]);
            let mut now = t0;
            while let Some(at) = next_redraw(&x, t0, now, true) {
                if at - t0 > Duration::from_secs(1) {
                    break;
                }
                slots.insert(at);
                now = at;
            }
        }
        assert!(slots.len() <= 61, "{}", slots.len());
    }

    #[test]
    fn timeline_hides_and_restarts_the_layer() {
        let tl = Timeline {
            start: Duration::from_millis(100),
            duration: Duration::from_millis(300),
            looped: true,
            loop_delay: Duration::from_millis(200),
        };
        assert_eq!(tl.local(Duration::from_millis(50)), None);
        assert_eq!(
            tl.local(Duration::from_millis(150)),
            Some(Duration::from_millis(50))
        );
        assert_eq!(tl.local(Duration::from_millis(450)), None);
        assert_eq!(
            tl.local(Duration::from_millis(650)),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            tl.until_change(Duration::from_millis(50)),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            tl.until_change(Duration::from_millis(150)),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            tl.until_change(Duration::from_millis(450)),
            Some(Duration::from_millis(150))
        );
        let once = Timeline {
            looped: false,
            ..tl
        };
        assert_eq!(once.local(Duration::from_millis(450)), None);
        assert_eq!(once.until_change(Duration::from_millis(450)), None);
        // Hidden: nothing drawn; the redraw comes when it appears.
        let a = anim_ms(&[100, 100]);
        let t0 = Instant::now();
        let now = t0 + Duration::from_millis(20);
        assert_eq!(shown_frame(&a, t0, Some(now), true, Some(&tl)), None);
        let at = next_redraw_in(&a, t0, now, true, Some(&tl)).unwrap();
        assert!(at >= t0 + Duration::from_millis(100));
        assert_eq!(shown_frame(&a, t0, Some(at), true, Some(&tl)), Some(0));
        // Paused: first frame, no redraws.
        assert_eq!(shown_frame(&a, t0, Some(now), false, Some(&tl)), Some(0));
        assert_eq!(next_redraw_in(&a, t0, now, false, Some(&tl)), None);
    }
}
