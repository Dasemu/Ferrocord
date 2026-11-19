//! Side panel listing messages: a channel's pins, or the inbox of recent
//! mentions. Replaces the member list while open (like search).

use crate::net::model::*;

/// What the message panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    /// Pinned messages of a channel.
    Pins(ChannelId),
    /// Recent mentions of the current user, across all channels.
    Inbox,
}

#[derive(Debug, Clone, Default)]
pub struct MessagePanel {
    /// `None` when the panel is closed.
    pub kind: Option<PanelKind>,
    /// Newest first (most recently pinned / most recent mention).
    pub messages: Vec<Message>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped per request so late responses (or ones for a panel no longer
    /// shown) are dropped.
    pub generation: u64,
}

impl MessagePanel {
    /// Opens the panel on `kind` and starts loading; returns the generation
    /// to tag the request with.
    pub fn open(&mut self, kind: PanelKind) -> u64 {
        self.generation += 1;
        self.kind = Some(kind);
        self.messages.clear();
        self.loading = true;
        self.error = None;
        self.generation
    }

    /// Applies a response; stale generations are ignored.
    pub fn finish(&mut self, generation: u64, result: Result<Vec<Message>, String>) {
        if generation != self.generation || self.kind.is_none() {
            return;
        }
        self.loading = false;
        match result {
            Ok(m) => self.messages = m,
            Err(e) => self.error = Some(e),
        }
    }

    pub fn close(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Default::default()
        };
    }

    pub fn is_open(&self, kind: PanelKind) -> bool {
        self.kind == Some(kind)
    }

    /// What to draw with `channel` open: pins only belong to their channel,
    /// the inbox is shown everywhere.
    pub fn visible(&self, channel: Option<ChannelId>) -> Option<PanelKind> {
        match self.kind? {
            PanelKind::Pins(c) if Some(c) != channel => None,
            k => Some(k),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;

    fn msg(id: u64) -> Message {
        mock::text_message(Snowflake(10), Snowflake(id), mock::user(2), "hi")
    }

    #[test]
    fn open_finish_close() {
        let mut p = MessagePanel::default();
        let pins = PanelKind::Pins(Snowflake(10));
        assert!(!p.is_open(pins));
        let g = p.open(pins);
        assert!(p.loading && p.is_open(pins));
        assert!(!p.is_open(PanelKind::Inbox));
        p.finish(g, Ok(vec![msg(2), msg(1)]));
        assert!(!p.loading);
        assert_eq!(p.messages.len(), 2);
        p.close();
        assert!(p.kind.is_none() && p.messages.is_empty());
    }

    #[test]
    fn pins_show_only_in_their_channel_inbox_everywhere() {
        let mut p = MessagePanel::default();
        assert_eq!(p.visible(Some(Snowflake(10))), None);
        p.open(PanelKind::Pins(Snowflake(10)));
        assert_eq!(
            p.visible(Some(Snowflake(10))),
            Some(PanelKind::Pins(Snowflake(10)))
        );
        assert_eq!(p.visible(Some(Snowflake(11))), None);
        assert_eq!(p.visible(None), None);
        p.open(PanelKind::Inbox);
        assert_eq!(p.visible(Some(Snowflake(11))), Some(PanelKind::Inbox));
        assert_eq!(p.visible(None), Some(PanelKind::Inbox));
    }

    #[test]
    fn stale_responses_are_dropped() {
        let mut p = MessagePanel::default();
        let old = p.open(PanelKind::Pins(Snowflake(10)));
        let new = p.open(PanelKind::Inbox);
        p.finish(old, Ok(vec![msg(1)]));
        assert!(p.loading && p.messages.is_empty());
        p.finish(new, Err("nope".into()));
        assert_eq!(p.error.as_deref(), Some("nope"));
        // A response arriving after the panel was closed is ignored too.
        let g = p.open(PanelKind::Inbox);
        p.close();
        p.finish(g, Ok(vec![msg(1)]));
        assert!(p.messages.is_empty() && !p.loading);
    }
}
