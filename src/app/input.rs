//! Input state that is read on demand and must not cost a view rebuild per
//! event: the last cursor position, the window size and the Shift state.
//!
//! The event subscription records them in atomics and returns no message,
//! so moving the mouse or pressing Shift never triggers `update`/`view`.
//! `update` reads them when a click needs them (context menu position,
//! "Shift+click skips the confirmation").

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use iced::Point;

static CURSOR_X: AtomicU32 = AtomicU32::new(0);
static CURSOR_Y: AtomicU32 = AtomicU32::new(0);
static WIN_W: AtomicU32 = AtomicU32::new(1280f32.to_bits());
static WIN_H: AtomicU32 = AtomicU32::new(800f32.to_bits());
static SHIFT: AtomicBool = AtomicBool::new(false);

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// Records the parts of `event` we care about.
pub fn record(event: &iced::Event) {
    match event {
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            CURSOR_X.store(position.x.to_bits(), Ordering::Relaxed);
            CURSOR_Y.store(position.y.to_bits(), Ordering::Relaxed);
        }
        iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(m)) => {
            SHIFT.store(m.shift(), Ordering::Relaxed);
        }
        iced::Event::Window(iced::window::Event::Resized(size)) => {
            WIN_W.store(size.width.to_bits(), Ordering::Relaxed);
            WIN_H.store(size.height.to_bits(), Ordering::Relaxed);
        }
        _ => {}
    }
}

pub fn cursor() -> Point {
    Point::new(load(&CURSOR_X), load(&CURSOR_Y))
}

pub fn window_size() -> iced::Size {
    iced::Size::new(load(&WIN_W), load(&WIN_H))
}

pub fn shift_down() -> bool {
    SHIFT.load(Ordering::Relaxed)
}

/// Test hooks.
#[doc(hidden)]
pub fn set_shift_for_tests(v: bool) {
    SHIFT.store(v, Ordering::Relaxed);
}

/// Top-left corner for a popup of `size` opened at `at`, kept inside the
/// window with a margin.
pub fn clamp_popup(at: Point, size: iced::Size, window: iced::Size) -> Point {
    const MARGIN: f32 = 8.0;
    let x = at.x.min(window.width - size.width - MARGIN).max(MARGIN);
    let y = at.y.min(window.height - size.height - MARGIN).max(MARGIN);
    Point::new(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_stay_inside_the_window() {
        let w = iced::Size::new(1000.0, 700.0);
        let s = iced::Size::new(200.0, 300.0);
        assert_eq!(
            clamp_popup(Point::new(100.0, 100.0), s, w),
            Point::new(100.0, 100.0)
        );
        assert_eq!(
            clamp_popup(Point::new(990.0, 690.0), s, w),
            Point::new(792.0, 392.0)
        );
        assert_eq!(
            clamp_popup(Point::new(-5.0, -5.0), s, w),
            Point::new(8.0, 8.0)
        );
    }

    #[test]
    fn events_are_recorded() {
        record(&iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: Point::new(12.0, 34.0),
        }));
        assert_eq!(cursor(), Point::new(12.0, 34.0));
        record(&iced::Event::Keyboard(
            iced::keyboard::Event::ModifiersChanged(iced::keyboard::Modifiers::SHIFT),
        ));
        assert!(shift_down());
        record(&iced::Event::Keyboard(
            iced::keyboard::Event::ModifiersChanged(iced::keyboard::Modifiers::empty()),
        ));
        assert!(!shift_down());
    }
}
