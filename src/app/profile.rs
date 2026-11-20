//! Profile popout state: fetched profiles (bounded, with a refresh age),
//! the open popout, and the profile-effect configs.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::Point;

use crate::net::cosmetics::{Profile, ProfileEffect};
use crate::net::model::{GuildId, Snowflake, UserId};

/// Profiles older than this are fetched again when the popout opens (the
/// cached one keeps showing meanwhile).
pub const TTL: Duration = Duration::from_secs(300);
/// A failed fetch is retried after this long.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Profiles kept.
pub const MAX_PROFILES: usize = 48;

/// Popout card width and the height reserved when clamping its position.
pub const CARD_W: f32 = 340.0;
pub const CARD_H: f32 = 540.0;

pub type Key = (UserId, Option<GuildId>);

#[derive(Debug, Clone)]
pub enum Slot {
    Loading,
    Ready(Box<Profile>),
    Failed,
}

#[derive(Debug, Clone)]
struct Entry {
    slot: Slot,
    /// Keeps showing while a refresh is in flight.
    stale: Option<Box<Profile>>,
    at: Instant,
    used: u64,
}

/// The open popout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Popout {
    pub user: UserId,
    pub guild: Option<GuildId>,
    /// Top-left of the card (already clamped to the window).
    pub at: Point,
}

#[derive(Debug, Clone, Default)]
pub enum Effects {
    #[default]
    Unknown,
    Loading(Instant),
    Ready(HashMap<Snowflake, ProfileEffect>),
    Failed(Instant),
}

#[derive(Debug, Default)]
pub struct ProfileState {
    entries: HashMap<Key, Entry>,
    clock: u64,
    pub popout: Option<Popout>,
    pub effects: Effects,
}

impl ProfileState {
    /// True if `key` should be requested now; marks it loading.
    pub fn want(&mut self, key: Key, now: Instant) -> bool {
        self.clock += 1;
        let clock = self.clock;
        match self.entries.get_mut(&key) {
            Some(e) => {
                e.used = clock;
                match &e.slot {
                    Slot::Loading => false,
                    Slot::Ready(p) if now.saturating_duration_since(e.at) >= TTL => {
                        e.stale = Some(p.clone());
                        e.slot = Slot::Loading;
                        e.at = now;
                        true
                    }
                    Slot::Ready(_) => false,
                    Slot::Failed if now.saturating_duration_since(e.at) >= RETRY_AFTER => {
                        e.slot = Slot::Loading;
                        e.at = now;
                        true
                    }
                    Slot::Failed => false,
                }
            }
            None => {
                if self.entries.len() >= MAX_PROFILES
                    && let Some(old) = self
                        .entries
                        .iter()
                        .min_by_key(|(_, e)| e.used)
                        .map(|(k, _)| *k)
                {
                    self.entries.remove(&old);
                }
                self.entries.insert(
                    key,
                    Entry {
                        slot: Slot::Loading,
                        stale: None,
                        at: now,
                        used: clock,
                    },
                );
                true
            }
        }
    }

    pub fn set(&mut self, key: Key, result: Option<Profile>, now: Instant) {
        self.clock += 1;
        let clock = self.clock;
        let e = self.entries.entry(key).or_insert(Entry {
            slot: Slot::Loading,
            stale: None,
            at: now,
            used: clock,
        });
        e.at = now;
        e.used = clock;
        match result {
            Some(p) => {
                e.slot = Slot::Ready(Box::new(p));
                e.stale = None;
            }
            // Keep an older profile rather than replacing it with an error.
            None if e.stale.is_some() => {
                e.slot = Slot::Ready(e.stale.take().unwrap_or_default());
                // Retry soon, not after the full TTL.
                e.at = now.checked_sub(TTL - RETRY_AFTER).unwrap_or(now);
            }
            None => e.slot = Slot::Failed,
        }
    }

    /// The profile to draw: fresh, or the stale one while refreshing.
    pub fn get(&self, key: Key) -> Option<&Profile> {
        let e = self.entries.get(&key)?;
        match &e.slot {
            Slot::Ready(p) => Some(p),
            _ => e.stale.as_deref(),
        }
    }

    pub fn slot(&self, key: Key) -> Option<&Slot> {
        self.entries.get(&key).map(|e| &e.slot)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the effect configs should be requested now.
    pub fn want_effects(&mut self, now: Instant) -> bool {
        let go = match &self.effects {
            Effects::Unknown => true,
            Effects::Failed(at) => now.saturating_duration_since(*at) >= RETRY_AFTER,
            Effects::Loading(_) | Effects::Ready(_) => false,
        };
        if go {
            self.effects = Effects::Loading(now);
        }
        go
    }

    pub fn set_effects(&mut self, list: Option<Vec<ProfileEffect>>, now: Instant) {
        self.effects = match list {
            Some(l) => Effects::Ready(l.into_iter().map(|e| (e.id, e)).collect()),
            None => Effects::Failed(now),
        };
    }

    pub fn effect(&self, id: Snowflake) -> Option<&ProfileEffect> {
        match &self.effects {
            Effects::Ready(m) => m.get(&id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u64) -> Key {
        (Snowflake(n), None)
    }

    fn profile(n: u64) -> Profile {
        Profile {
            bio: format!("bio{n}"),
            ..Profile::default()
        }
    }

    #[test]
    fn want_dedupes_while_loading_and_after_success() {
        let mut s = ProfileState::default();
        let t0 = Instant::now();
        assert!(s.want(key(1), t0));
        assert!(!s.want(key(1), t0));
        s.set(key(1), Some(profile(1)), t0);
        assert!(!s.want(key(1), t0 + Duration::from_secs(10)));
        assert_eq!(s.get(key(1)).unwrap().bio, "bio1");
    }

    #[test]
    fn stale_profiles_refresh_but_keep_showing() {
        let mut s = ProfileState::default();
        let t0 = Instant::now();
        s.want(key(1), t0);
        s.set(key(1), Some(profile(1)), t0);
        let later = t0 + TTL + Duration::from_secs(1);
        assert!(s.want(key(1), later));
        // The old card keeps showing during the refresh.
        assert_eq!(s.get(key(1)).unwrap().bio, "bio1");
        assert!(matches!(s.slot(key(1)), Some(Slot::Loading)));
        // A failed refresh keeps the old profile.
        s.set(key(1), None, later);
        assert_eq!(s.get(key(1)).unwrap().bio, "bio1");
        // And a good one replaces it.
        assert!(s.want(key(1), later + TTL));
        s.set(key(1), Some(profile(9)), later + TTL);
        assert_eq!(s.get(key(1)).unwrap().bio, "bio9");
    }

    #[test]
    fn failures_retry_later() {
        let mut s = ProfileState::default();
        let t0 = Instant::now();
        s.want(key(1), t0);
        s.set(key(1), None, t0);
        assert!(s.get(key(1)).is_none());
        assert!(matches!(s.slot(key(1)), Some(Slot::Failed)));
        assert!(!s.want(key(1), t0 + Duration::from_secs(5)));
        assert!(s.want(key(1), t0 + RETRY_AFTER));
    }

    #[test]
    fn profiles_are_bounded_and_evict_least_recently_used() {
        let mut s = ProfileState::default();
        let t0 = Instant::now();
        for n in 0..MAX_PROFILES as u64 {
            s.want(key(n), t0);
            s.set(key(n), Some(profile(n)), t0);
        }
        // Touch 0 so 1 is the oldest.
        s.want(key(0), t0);
        s.want(key(1000), t0);
        assert_eq!(s.len(), MAX_PROFILES);
        assert!(s.get(key(0)).is_some());
        assert!(s.get(key(1)).is_none());
    }

    #[test]
    fn guild_and_global_profiles_are_separate() {
        let mut s = ProfileState::default();
        let t0 = Instant::now();
        assert!(s.want((Snowflake(1), None), t0));
        assert!(s.want((Snowflake(1), Some(Snowflake(2))), t0));
    }

    #[test]
    fn effects_load_once_and_retry_after_failure() {
        let mut s = ProfileState::default();
        let t0 = Instant::now();
        assert!(s.want_effects(t0));
        assert!(!s.want_effects(t0));
        s.set_effects(None, t0);
        assert!(!s.want_effects(t0 + Duration::from_secs(1)));
        assert!(s.want_effects(t0 + RETRY_AFTER));
        s.set_effects(
            Some(vec![ProfileEffect {
                id: Snowflake(5),
                ..Default::default()
            }]),
            t0,
        );
        assert!(!s.want_effects(t0 + TTL));
        assert!(s.effect(Snowflake(5)).is_some() && s.effect(Snowflake(6)).is_none());
    }
}
