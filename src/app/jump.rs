//! Scrolling the message list to a given message.
//!
//! A custom widget operation finds the message's container (by `Id`) and
//! the scrollable's geometry; [`target_offset`] converts them to the offset
//! `scrollable::scroll_to` needs (distance from the bottom, because the
//! list is `anchor_bottom`).

use iced::advanced::widget::{Id, Operation, operation};
use iced::widget::scrollable::AbsoluteOffset;
use iced::{Rectangle, Task, Vector};

use crate::net::model::MessageId;

/// Id of the message scrollable.
pub fn list_id() -> Id {
    Id::new("message-list")
}

/// Id of a message row container (only set on the jump target).
pub fn row_id(m: MessageId) -> Id {
    Id::from(format!("msg-{m}"))
}

/// Geometry gathered by the operation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Geometry {
    pub viewport: Option<Rectangle>,
    pub content: Option<Rectangle>,
    pub target: Option<Rectangle>,
}

/// Offset from the bottom that places the target a bit below the top of
/// the viewport. `None` until all three rectangles are known.
pub fn target_offset(g: &Geometry) -> Option<f32> {
    let (vp, content, target) = (g.viewport?, g.content?, g.target?);
    let from_top = (target.y - content.y - 80.0).max(0.0);
    let max = (content.height - vp.height).max(0.0);
    Some((max - from_top).clamp(0.0, max))
}

struct Find {
    list: Id,
    target: Id,
    geo: Geometry,
}

impl Operation<Geometry> for Find {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Geometry>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.target) {
            self.geo.target = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        _translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.list) {
            self.geo.viewport = Some(bounds);
            self.geo.content = Some(content_bounds);
        }
    }

    fn finish(&self) -> operation::Outcome<Geometry> {
        operation::Outcome::Some(self.geo)
    }
}

/// Scrolls the list so `message` is visible. Does nothing if the message is
/// not in the current view tree.
pub fn scroll_to_message<T: Send + 'static>(message: MessageId) -> Task<T> {
    let op = Find {
        list: list_id(),
        target: row_id(message),
        geo: Geometry::default(),
    };
    iced::advanced::widget::operate(op).then(|g| match target_offset(&g) {
        Some(y) => iced::widget::operation::scroll_to(
            list_id(),
            AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        ),
        None => Task::none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(y: f32, h: f32) -> Rectangle {
        Rectangle {
            x: 0.0,
            y,
            width: 100.0,
            height: h,
        }
    }

    #[test]
    fn offset_is_measured_from_the_bottom() {
        // Content 3000 tall starting at y=48, viewport 600, target 1000 px
        // below the content top.
        let g = Geometry {
            viewport: Some(r(48.0, 600.0)),
            content: Some(r(48.0, 3000.0)),
            target: Some(r(1048.0, 60.0)),
        };
        // Wanted top = 1000 - 80 = 920; max = 2400; offset = 2400 - 920.
        assert_eq!(target_offset(&g), Some(1480.0));
    }

    #[test]
    fn offsets_are_clamped() {
        let near_top = Geometry {
            viewport: Some(r(0.0, 600.0)),
            content: Some(r(0.0, 3000.0)),
            target: Some(r(10.0, 60.0)),
        };
        assert_eq!(target_offset(&near_top), Some(2400.0));
        let short = Geometry {
            viewport: Some(r(0.0, 600.0)),
            content: Some(r(0.0, 300.0)),
            target: Some(r(100.0, 60.0)),
        };
        assert_eq!(target_offset(&short), Some(0.0));
        assert_eq!(target_offset(&Geometry::default()), None);
    }
}
