//! The message box state: a multi-line `text_editor` buffer plus the key
//! bindings that make it behave like the official client (Enter sends,
//! Shift+Enter inserts a newline, Up in an empty box edits the last
//! message), and the per-channel drafts.

use std::collections::HashMap;
use std::sync::Arc;

use iced::keyboard::{Key, key::Named};
use iced::widget::text_editor::{Action, Binding, Content, Edit, KeyPress, Status};

use super::message::Ui;
use crate::net::model::ChannelId;

/// Drafts kept for channels that are not open (bounded).
const MAX_DRAFTS: usize = 64;

/// Editable text with a cursor (the composer and the inline edit box).
#[derive(Debug, Clone, Default)]
pub struct Composer {
    content: Content,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer holding `text`, with the cursor at the end. The text goes
    /// in as a paste so the editor lays it out like typed input.
    pub fn with_text(text: &str) -> Self {
        let mut c = Self::new();
        if !text.is_empty() {
            c.content
                .perform(Action::Edit(Edit::Paste(Arc::new(text.to_owned()))));
        }
        c
    }

    pub fn content(&self) -> &Content {
        &self.content
    }

    pub fn text(&self) -> String {
        self.content.text()
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    pub fn clear(&mut self) {
        self.content = Content::new();
    }

    pub fn set_text(&mut self, text: &str) {
        *self = Self::with_text(text);
    }

    /// Applies an editor action; true when it changed the text.
    pub fn perform(&mut self, action: Action) -> bool {
        let edit = action.is_edit();
        self.content.perform(action);
        edit
    }

    /// Inserts `text` at the end of the buffer (emoji picker).
    pub fn push_str(&mut self, text: &str) {
        self.content.move_to(end_cursor(&self.content));
        self.content
            .perform(Action::Edit(Edit::Paste(Arc::new(text.to_owned()))));
    }
}

fn end_cursor(content: &Content) -> iced::widget::text_editor::Cursor {
    let last = content.line_count().saturating_sub(1);
    // Columns are byte offsets into the line.
    let column = content.line(last).map_or(0, |l| l.text.len());
    iced::widget::text_editor::Cursor {
        position: iced::widget::text_editor::Position { line: last, column },
        selection: None,
    }
}

/// Key bindings shared by the composer and the edit box.
/// `submit` is produced by a plain Enter; Shift+Enter breaks the line.
/// With `up` set, Arrow Up in an empty box produces it.
pub fn key_binding(kp: KeyPress, empty: bool, submit: Ui, up: Option<Ui>) -> Option<Binding<Ui>> {
    if !matches!(kp.status, Status::Focused { .. }) {
        return None;
    }
    match kp.modified_key.as_ref() {
        Key::Named(Named::Enter) => {
            return Some(if kp.modifiers.shift() {
                Binding::Enter
            } else {
                Binding::Custom(submit)
            });
        }
        Key::Named(Named::ArrowUp) if empty && kp.modifiers.is_empty() && up.is_some() => {
            return up.map(Binding::Custom);
        }
        _ => {}
    }
    Binding::from_key_press(kp)
}

/// Pane share the box may grow to before it scrolls.
pub const MAX_HEIGHT_FRACTION: f32 = 0.5;

/// Height of one line of composer text (the official client's 22 px).
pub const LINE_HEIGHT: f32 = 22.0;
/// Space above and below the text: one line plus this padding gives the
/// official 44 px box.
pub const PADDING_Y: f32 = 11.0;

/// Limits for the text area *inside* the padding. iced's `text_editor`
/// applies `min_height`/`max_height` to the text and then adds the padding,
/// so a total height passed here would grow the box by the padding again
/// (the old `min_height(46)` with 11 px padding made a 68 px box with the
/// placeholder stuck at the top).
pub fn text_height_limits(window_height: f32) -> (f32, f32) {
    let max_total = (window_height * MAX_HEIGHT_FRACTION).max(120.0);
    (LINE_HEIGHT, (max_total - 2.0 * PADDING_Y).max(LINE_HEIGHT))
}

/// Drafts of the channels that are not open.
#[derive(Debug, Default)]
pub struct Drafts(HashMap<ChannelId, String>);

impl Drafts {
    pub fn save(&mut self, c: ChannelId, text: String) {
        if text.trim().is_empty() {
            self.0.remove(&c);
        } else {
            if self.0.len() >= MAX_DRAFTS
                && !self.0.contains_key(&c)
                && let Some(k) = self.0.keys().next().copied()
            {
                self.0.remove(&k);
            }
            self.0.insert(c, text);
        }
    }

    pub fn take(&mut self, c: ChannelId) -> Option<String> {
        self.0.remove(&c)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::{Modifiers, key::Physical};
    use iced::widget::text_editor::Motion;

    fn press(key: Named, modifiers: Modifiers) -> KeyPress {
        KeyPress {
            key: Key::Named(key),
            modified_key: Key::Named(key),
            physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
            modifiers,
            text: None,
            status: Status::Focused { is_hovered: false },
        }
    }

    #[test]
    fn enter_sends_shift_enter_breaks_the_line() {
        let b = key_binding(
            press(Named::Enter, Modifiers::empty()),
            false,
            Ui::Send,
            None,
        );
        assert!(matches!(b, Some(Binding::Custom(Ui::Send))));
        let b = key_binding(press(Named::Enter, Modifiers::SHIFT), false, Ui::Send, None);
        assert!(matches!(b, Some(Binding::Enter)));
    }

    #[test]
    fn up_edits_last_only_when_empty() {
        let k = || press(Named::ArrowUp, Modifiers::empty());
        let b = key_binding(k(), true, Ui::Send, Some(Ui::EditLast));
        assert!(matches!(b, Some(Binding::Custom(Ui::EditLast))));
        let b = key_binding(k(), false, Ui::Send, Some(Ui::EditLast));
        assert!(matches!(b, Some(Binding::Move(Motion::Up))));
    }

    #[test]
    fn unfocused_editor_ignores_keys() {
        let mut k = press(Named::Enter, Modifiers::empty());
        k.status = Status::Active;
        assert!(key_binding(k, false, Ui::Send, None).is_none());
    }

    #[test]
    fn buffer_keeps_newlines_and_appends_at_the_end() {
        let mut c = Composer::with_text("a\nb");
        assert_eq!(c.text(), "a\nb");
        c.push_str("🍕 ");
        assert_eq!(c.text(), "a\nb🍕 ");
        // Non-ASCII lines: cursor columns are byte offsets.
        let mut u = Composer::with_text("é\nü🍕");
        u.push_str("é");
        assert_eq!(u.text(), "é\nü🍕é");
        c.clear();
        assert!(c.is_empty());
        c.perform(Action::Edit(Edit::Insert('x')));
        c.perform(Action::Edit(Edit::Enter));
        c.perform(Action::Edit(Edit::Insert('y')));
        assert_eq!(c.text(), "x\ny");
    }

    #[test]
    fn drafts_are_per_channel_and_bounded() {
        let mut d = Drafts::default();
        for i in 0..100u64 {
            d.save(crate::net::model::Snowflake(i + 1), format!("t{i}"));
        }
        assert!(d.len() <= MAX_DRAFTS);
        d.save(crate::net::model::Snowflake(500), "keep".into());
        assert_eq!(
            d.take(crate::net::model::Snowflake(500)).as_deref(),
            Some("keep")
        );
        d.save(crate::net::model::Snowflake(501), "  ".into());
        assert!(d.take(crate::net::model::Snowflake(501)).is_none());
    }
}
