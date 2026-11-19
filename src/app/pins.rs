//! Pinned messages panel state (replaces the member list while open).

use crate::net::model::*;

#[derive(Debug, Clone, Default)]
pub struct PinsState {
    /// Channel whose pins are shown; `None` when the panel is closed.
    pub channel: Option<ChannelId>,
    /// Most recently pinned first.
    pub messages: Vec<Message>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped per request so late responses (or ones for a channel the
    /// panel no longer shows) are dropped.
    pub generation: u64,
}

impl PinsState {
    /// Opens the panel for `channel` and starts loading; returns the
    /// generation to tag the request with.
    pub fn open(&mut self, channel: ChannelId) -> u64 {
        self.generation += 1;
        self.channel = Some(channel);
        self.messages.clear();
        self.loading = true;
        self.error = None;
        self.generation
    }

    /// Applies a response; stale generations are ignored.
    pub fn finish(&mut self, generation: u64, result: Result<Vec<Message>, String>) {
        if generation != self.generation || self.channel.is_none() {
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

    /// Whether the panel is showing the pins of `channel`.
    pub fn is_open_for(&self, channel: Option<ChannelId>) -> bool {
        self.channel.is_some() && self.channel == channel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;

    fn msg(id: u64) -> Message {
        mock::text_message(Snowflake(10), Snowflake(id), mock::user(2), "pinned")
    }

    #[test]
    fn open_finish_close() {
        let mut p = PinsState::default();
        assert!(!p.is_open_for(Some(Snowflake(10))));
        let g = p.open(Snowflake(10));
        assert!(p.loading && p.is_open_for(Some(Snowflake(10))));
        assert!(!p.is_open_for(Some(Snowflake(11))));
        p.finish(g, Ok(vec![msg(2), msg(1)]));
        assert!(!p.loading);
        assert_eq!(p.messages.len(), 2);
        p.close();
        assert!(p.channel.is_none() && p.messages.is_empty());
        assert!(!p.is_open_for(Some(Snowflake(10))));
    }

    #[test]
    fn stale_responses_are_dropped() {
        let mut p = PinsState::default();
        let old = p.open(Snowflake(10));
        let new = p.open(Snowflake(11));
        p.finish(old, Ok(vec![msg(1)]));
        assert!(p.loading && p.messages.is_empty());
        p.finish(new, Err("nope".into()));
        assert_eq!(p.error.as_deref(), Some("nope"));
        // A response arriving after the panel was closed is ignored too.
        let g = p.open(Snowflake(10));
        p.close();
        p.finish(g, Ok(vec![msg(1)]));
        assert!(p.messages.is_empty() && !p.loading);
    }
}
