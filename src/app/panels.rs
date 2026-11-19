//! State of the channel-header popovers: notification settings (bell),
//! pinned messages and the mentions inbox. Pure reducers, unit-tested.

use crate::net::extras::MENTIONS_PAGE;
use crate::net::model::*;

/// Which header popover is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popover {
    Bell,
    Pins,
    Inbox,
}

#[derive(Debug, Clone, Default)]
pub struct PinsState {
    pub channel: Option<ChannelId>,
    pub loading: bool,
    pub error: Option<String>,
    /// Newest pin first.
    pub items: Vec<Message>,
    generation: u64,
}

impl PinsState {
    /// Starts loading `channel`; returns the generation to tag the
    /// response with. Switching channel drops the old list at once.
    pub fn begin(&mut self, channel: ChannelId) -> u64 {
        self.generation += 1;
        if self.channel != Some(channel) {
            self.items.clear();
        }
        self.channel = Some(channel);
        self.loading = true;
        self.error = None;
        self.generation
    }

    pub fn finish(&mut self, generation: u64, result: Result<Vec<Message>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(items) => self.items = items,
            Err(e) => self.error = Some(e),
        }
    }

    pub fn remove(&mut self, message: MessageId) {
        self.items.retain(|m| m.id != message);
    }
}

#[derive(Debug, Clone, Default)]
pub struct InboxState {
    pub loading: bool,
    pub error: Option<String>,
    /// Newest first.
    pub items: Vec<Message>,
    /// "This Server" filter.
    pub guild_only: bool,
    pub has_more: bool,
    generation: u64,
}

impl InboxState {
    /// Starts a load. `reset` reloads from the newest mention; otherwise the
    /// next page after the oldest one shown. Returns `(generation, before)`.
    pub fn begin(&mut self, reset: bool) -> (u64, Option<MessageId>) {
        self.generation += 1;
        if reset {
            self.items.clear();
            self.has_more = false;
        }
        self.loading = true;
        self.error = None;
        let before = if reset {
            None
        } else {
            self.items.iter().map(|m| m.id).min()
        };
        (self.generation, before)
    }

    pub fn finish(&mut self, generation: u64, result: Result<Vec<Message>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(page) => {
                self.has_more = page.len() as u32 >= MENTIONS_PAGE;
                for m in page {
                    if !self.items.iter().any(|x| x.id == m.id) {
                        self.items.push(m);
                    }
                }
                self.items.sort_by_key(|m| std::cmp::Reverse(m.id));
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn set_guild_only(&mut self, on: bool) -> bool {
        let changed = self.guild_only != on;
        self.guild_only = on;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;

    fn msgs(ids: &[u64]) -> Vec<Message> {
        ids.iter()
            .map(|i| mock::text_message(Snowflake(1), Snowflake(*i), mock::user(2), "x"))
            .collect()
    }

    #[test]
    fn pins_drop_stale_responses_and_the_old_channels_list() {
        let mut p = PinsState::default();
        let g1 = p.begin(Snowflake(1));
        p.finish(g1, Ok(msgs(&[3, 2])));
        assert_eq!(p.items.len(), 2);
        // Reopening the same channel keeps the list while it refreshes.
        let g2 = p.begin(Snowflake(1));
        assert_eq!(p.items.len(), 2);
        assert!(p.loading);
        // A different channel starts empty; the old response is ignored.
        let g3 = p.begin(Snowflake(2));
        assert!(p.items.is_empty());
        p.finish(g2, Ok(msgs(&[9])));
        assert!(p.items.is_empty() && p.loading);
        p.finish(g3, Err("boom".into()));
        assert_eq!(p.error.as_deref(), Some("boom"));
        assert!(!p.loading);
    }

    #[test]
    fn unpinning_removes_locally() {
        let mut p = PinsState::default();
        let g = p.begin(Snowflake(1));
        p.finish(g, Ok(msgs(&[3, 2, 1])));
        p.remove(Snowflake(2));
        assert_eq!(
            p.items.iter().map(|m| m.id.0).collect::<Vec<_>>(),
            vec![3, 1]
        );
    }

    #[test]
    fn inbox_pages_with_before_and_dedupes() {
        let mut i = InboxState::default();
        let (g, before) = i.begin(true);
        assert_eq!(before, None);
        let first: Vec<u64> = (100..125).rev().collect();
        i.finish(g, Ok(msgs(&first)));
        assert_eq!(i.items.len(), 25);
        assert!(i.has_more, "a full page means there may be more");
        let (g, before) = i.begin(false);
        assert_eq!(before, Some(Snowflake(100)));
        // Overlapping page: duplicates are dropped, short page ends paging.
        i.finish(g, Ok(msgs(&[100, 99, 98])));
        assert_eq!(i.items.len(), 27);
        assert!(!i.has_more);
        assert_eq!(i.items.first().unwrap().id.0, 124);
        assert_eq!(i.items.last().unwrap().id.0, 98);
    }

    #[test]
    fn inbox_ignores_stale_generations_and_resets() {
        let mut i = InboxState::default();
        let (old, _) = i.begin(true);
        let (new, _) = i.begin(true);
        i.finish(old, Ok(msgs(&[1])));
        assert!(i.items.is_empty() && i.loading);
        i.finish(new, Err("nope".into()));
        assert_eq!(i.error.as_deref(), Some("nope"));
        assert!(i.set_guild_only(true));
        assert!(!i.set_guild_only(true));
    }
}
