//! Smooth mouse-wheel scrolling for any `scrollable`.
//!
//! iced moves a scrollable 60 px at once per wheel notch, so the view jumps.
//! [`smooth`] wraps the scrollable: wheel notches (line deltas) are taken
//! out of the event stream and handed back to the content as small pixel
//! deltas over the next frames, easing out (exponential approach, about
//! [`TAU`] per 63 % of the distance). Touchpads already send pixel deltas
//! and pass through untouched, as does Shift + wheel (horizontal).
//!
//! No `Message` is produced: the wrapper asks the runtime for redraws while
//! it has distance left and replays the deltas inside its own `update`, so
//! the app's `view` does not run per frame.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, keyboard, mouse, window};

/// Pixels per wheel line (iced's own factor).
pub const LINE: f32 = 60.0;
/// Time constant of the ease-out.
pub const TAU: Duration = Duration::from_millis(55);
/// Frame time assumed for the first step and the cap for a late frame.
const FIRST_FRAME: Duration = Duration::from_millis(16);
const MAX_FRAME: Duration = Duration::from_millis(50);

pub struct SmoothScroll<'a, M, T, R> {
    content: Element<'a, M, T, R>,
}

/// Wraps `content` (a scrollable) so wheel notches scroll smoothly.
pub fn smooth<'a, M, T, R>(content: impl Into<Element<'a, M, T, R>>) -> SmoothScroll<'a, M, T, R> {
    SmoothScroll {
        content: content.into(),
    }
}

#[derive(Debug, Default)]
struct State {
    /// Distance still to scroll, in the wheel's pixel convention.
    pending: Vector,
    /// Where the pointer was at the last notch (so the same scrollable
    /// keeps receiving the steps even if the pointer moves away).
    at: Option<Point>,
    last_frame: Option<Instant>,
    shift: bool,
}

/// One animation step: how much of `pending` to scroll after `dt`.
pub fn step(pending: Vector, dt: Duration) -> Vector {
    if pending.x.abs() < 0.5 && pending.y.abs() < 0.5 {
        return pending;
    }
    let k = 1.0 - (-dt.as_secs_f32() / TAU.as_secs_f32()).exp();
    pending * k
}

impl<M, T, R> Widget<M, T, R> for SmoothScroll<'_, M, T, R>
where
    R: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &R, limits: &layout::Limits) -> layout::Node {
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(child.size(), vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &R,
        operation: &mut dyn Operation,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &R,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let Some(child) = layout.children().next() else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => state.shift = m.shift(),
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x, y },
            }) if !state.shift => {
                if let Some(p) = cursor.position_over(layout.bounds()) {
                    // A new gesture after the last one finished starts its
                    // clock fresh.
                    if state.pending == Vector::ZERO {
                        state.last_frame = None;
                    }
                    state.pending += Vector::new(*x, *y) * LINE;
                    state.at = Some(p);
                    shell.request_redraw();
                    shell.capture_event();
                    return;
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) if state.pending != Vector::ZERO => {
                let dt = state
                    .last_frame
                    .map_or(FIRST_FRAME, |t| now.saturating_duration_since(t))
                    .min(MAX_FRAME);
                state.last_frame = Some(*now);
                let s = step(state.pending, dt);
                state.pending -= s;
                if state.pending.x.abs() < 0.01 && state.pending.y.abs() < 0.01 {
                    state.pending = Vector::ZERO;
                } else {
                    shell.request_redraw();
                }
                let at = state.at.map_or(cursor, mouse::Cursor::Available);
                // The redraw itself reaches the content first (animated
                // images inside need it)...
                self.content.as_widget_mut().update(
                    &mut tree.children[0],
                    event,
                    child,
                    cursor,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                );
                // ...then the step, through a shell of its own so the
                // scrollable capturing it never hides the redraw from the
                // widgets after this one.
                let wheel = Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: s.x, y: s.y },
                });
                let mut messages = Vec::new();
                let mut inner = Shell::new(&mut messages);
                self.content.as_widget_mut().update(
                    &mut tree.children[0],
                    &wheel,
                    child,
                    at,
                    renderer,
                    clipboard,
                    &mut inner,
                    viewport,
                );
                let layout_invalid = inner.is_layout_invalid();
                let widgets_invalid = inner.are_widgets_invalid();
                let redraw = inner.redraw_request();
                drop(inner);
                for m in messages {
                    shell.publish(m);
                }
                if layout_invalid {
                    shell.invalidate_layout();
                }
                if widgets_invalid {
                    shell.invalidate_widgets();
                }
                match redraw {
                    window::RedrawRequest::NextFrame => shell.request_redraw(),
                    window::RedrawRequest::At(t) => shell.request_redraw_at(t),
                    window::RedrawRequest::Wait => {}
                }
                return;
            }
            _ => {}
        }
        if shell.is_event_captured() {
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &R,
    ) -> mouse::Interaction {
        layout
            .children()
            .next()
            .map_or(mouse::Interaction::None, |child| {
                self.content.as_widget().mouse_interaction(
                    &tree.children[0],
                    child,
                    cursor,
                    viewport,
                    renderer,
                )
            })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if let Some(child) = layout.children().next() {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                viewport,
            );
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &R,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, T, R>> {
        let child = layout.children().next()?;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a, T: 'a, R: renderer::Renderer + 'a> From<SmoothScroll<'a, M, T, R>>
    for Element<'a, M, T, R>
{
    fn from(s: SmoothScroll<'a, M, T, R>) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_ease_out_and_reach_the_target() {
        let mut pending = Vector::new(0.0, -LINE);
        let mut moved = 0.0f32;
        let mut first = None;
        let mut frames = 0;
        while pending != Vector::ZERO && frames < 200 {
            let s = step(pending, Duration::from_millis(16));
            first.get_or_insert(s.y);
            moved += s.y;
            pending -= s;
            if pending.y.abs() < 0.01 {
                pending = Vector::ZERO;
            }
            frames += 1;
        }
        // The whole notch is scrolled, in several frames, biggest step first.
        assert!((moved + LINE).abs() < 0.02, "{moved}");
        assert!(frames > 5 && frames < 60, "{frames}");
        assert!(first.is_some_and(|f| f.abs() < LINE / 2.0));
    }

    #[test]
    fn a_tiny_remainder_is_finished_at_once() {
        let v = Vector::new(0.0, 0.3);
        assert_eq!(step(v, Duration::from_millis(16)), v);
    }
}
