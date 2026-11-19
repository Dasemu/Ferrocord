//! REST rate limiting.
//!
//! Discord rate limits per *bucket*. A bucket is identified by the
//! `X-RateLimit-Bucket` hash returned for a route, combined with the route's
//! major parameter (channel / guild / webhook id). We learn the
//! route → bucket mapping from response headers, track `remaining` and the
//! reset time, honour the global limit, and respect `retry_after` on 429.
//!
//! The limiter is a pure, clock-injected state machine so it can be unit
//! tested deterministically.

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

/// Rate-limit-relevant response headers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimitHeaders {
    pub bucket: Option<String>,
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
    /// Seconds until the bucket resets (`X-RateLimit-Reset-After`).
    pub reset_after: Option<f64>,
    pub global: bool,
}

impl RateLimitHeaders {
    pub fn from_headers(h: &reqwest::header::HeaderMap) -> Self {
        let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
        Self {
            bucket: get("x-ratelimit-bucket").map(String::from),
            limit: get("x-ratelimit-limit").and_then(|v| v.parse().ok()),
            remaining: get("x-ratelimit-remaining").and_then(|v| v.parse().ok()),
            reset_after: get("x-ratelimit-reset-after").and_then(|v| v.parse().ok()),
            global: get("x-ratelimit-global").is_some_and(|v| v.eq_ignore_ascii_case("true")),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
}

#[derive(Debug, Default)]
pub struct RateLimiter {
    /// route key -> bucket hash
    routes: HashMap<String, String>,
    /// "hash:major" -> state
    buckets: HashMap<String, Bucket>,
    /// route key -> state, used until the bucket hash is known (and for 429s
    /// on routes without a bucket header).
    unbucketed: HashMap<String, Bucket>,
    global_until: Option<Instant>,
    /// Routes whose first request is in flight (value: when it started).
    probing: HashMap<String, Instant>,
}

/// Computes the rate-limit route key for `METHOD /path`.
///
/// Major parameters (the id right after `channels/`, `guilds/`, `webhooks/`)
/// are kept; every other snowflake is replaced by `:id`. Message deletion has
/// its own bucket on Discord, so the method is part of the key.
pub fn route_key(method: &str, path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    let mut out = String::with_capacity(path.len() + 8);
    out.push_str(method);
    out.push(' ');
    let mut prev = "";
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        let is_id = seg.bytes().all(|b| b.is_ascii_digit());
        if is_id && !matches!(prev, "channels" | "guilds" | "webhooks") {
            out.push_str(":id");
        } else if prev == "reactions" {
            // Emoji in reaction routes share a bucket.
            out.push_str(":emoji");
        } else {
            out.push_str(seg);
        }
        prev = seg;
    }
    out
}

/// The major parameter of a route key, e.g. `channels/123`.
fn major_of(route: &str) -> &str {
    let path = route.split_once(' ').map_or(route, |(_, p)| p);
    let mut parts = path.trim_start_matches('/').splitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some(kind @ ("channels" | "guilds" | "webhooks")), Some(id)) => {
            let len = kind.len() + 1 + id.len();
            &path.trim_start_matches('/')[..len]
        }
        _ => "",
    }
}

/// Longest wait we ever accept from a header (protects against garbage).
pub const MAX_WAIT: Duration = Duration::from_secs(3600);
/// While the first request on an unknown (or just reset) bucket is in
/// flight, others wait; a lost probe is forgotten after this long.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for a probe.
pub const PROBE_POLL: Duration = Duration::from_millis(50);

/// Converts header seconds to a duration: rejects NaN/negative/huge values.
pub fn secs(v: f64) -> Duration {
    if !v.is_finite() || v <= 0.0 {
        return Duration::ZERO;
    }
    if v >= MAX_WAIT.as_secs_f64() {
        return MAX_WAIT;
    }
    Duration::try_from_secs_f64(v).unwrap_or(Duration::ZERO)
}

fn after(now: Instant, d: Duration) -> Instant {
    now.checked_add(d.min(MAX_WAIT)).unwrap_or(now)
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn bucket_key(&self, route: &str) -> Option<String> {
        self.routes
            .get(route)
            .map(|hash| format!("{hash}:{}", major_of(route)))
    }

    /// How long to wait before sending a request on `route`. When it returns
    /// `None` a slot has been reserved: `remaining` is decremented, or, for
    /// an unknown / just-reset bucket, this request becomes the single
    /// "probe" whose response teaches us the limits (others wait for it).
    pub fn acquire(&mut self, route: &str, now: Instant) -> Option<Duration> {
        if let Some(until) = self.global_until {
            if until > now {
                return Some(until.saturating_duration_since(now));
            }
            self.global_until = None;
        }
        let key = self.bucket_key(route);
        let bucket = match &key {
            Some(k) => self.buckets.get_mut(k),
            None => self.unbucketed.get_mut(route),
        };
        if let Some(b) = bucket
            && b.reset_at > now
        {
            if b.remaining == 0 {
                return Some(b.reset_at.saturating_duration_since(now));
            }
            b.remaining -= 1;
            return None;
        }
        // Unknown bucket, or its window has passed (state is stale):
        // serialise until a response refreshes it.
        match key {
            Some(k) => {
                self.buckets.remove(&k);
            }
            None => {
                self.unbucketed.remove(route);
            }
        }
        if let Some(started) = self.probing.get(route)
            && now.saturating_duration_since(*started) < PROBE_TIMEOUT
        {
            return Some(PROBE_POLL);
        }
        self.probing.insert(route.to_owned(), now);
        None
    }

    /// Call when a request failed before any response arrived, so waiting
    /// requests on the same route are not blocked by a dead probe.
    pub fn release(&mut self, route: &str) {
        self.probing.remove(route);
    }

    /// Records the headers of a response.
    pub fn update(&mut self, route: &str, h: &RateLimitHeaders, now: Instant) {
        self.probing.remove(route);
        let Some(remaining) = h.remaining else { return };
        let reset_at = after(now, secs(h.reset_after.unwrap_or(0.0)));
        let fresh = Bucket {
            remaining,
            reset_at,
        };
        // Responses can arrive out of order. Within the same window keep
        // the smaller `remaining`; never let an older window overwrite a
        // newer one.
        let merge = |old: Option<&Bucket>| -> Bucket {
            match old {
                Some(o) if o.reset_at > now => {
                    let gap = if o.reset_at > reset_at {
                        o.reset_at - reset_at
                    } else {
                        reset_at - o.reset_at
                    };
                    if gap < Duration::from_secs(1) {
                        Bucket {
                            remaining: o.remaining.min(remaining),
                            reset_at: o.reset_at.max(reset_at),
                        }
                    } else if reset_at < o.reset_at {
                        *o
                    } else {
                        fresh
                    }
                }
                _ => fresh,
            }
        };
        if let Some(hash) = &h.bucket {
            self.routes.insert(route.to_owned(), hash.clone());
            let old = self.unbucketed.remove(route);
            let key = format!("{hash}:{}", major_of(route));
            let state = merge(self.buckets.get(&key).or(old.as_ref()));
            self.buckets.insert(key, state);
        } else {
            let state = merge(self.unbucketed.get(route));
            self.unbucketed.insert(route.to_owned(), state);
        }
    }

    /// Records a 429. `retry_after` comes from the JSON body (preferred) or
    /// the `Retry-After` header.
    pub fn on_429(&mut self, route: &str, retry_after: Duration, global: bool, now: Instant) {
        self.probing.remove(route);
        let until = after(now, retry_after);
        if global {
            self.global_until = Some(self.global_until.map_or(until, |g| g.max(until)));
            return;
        }
        let state = Bucket {
            remaining: 0,
            reset_at: until,
        };
        match self.bucket_key(route) {
            Some(k) => {
                self.buckets.insert(k, state);
            }
            None => {
                self.unbucketed.insert(route.to_owned(), state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_keys_keep_major_params() {
        assert_eq!(
            route_key("GET", "/channels/123/messages?limit=50"),
            "GET /channels/123/messages"
        );
        assert_eq!(
            route_key("DELETE", "/channels/123/messages/456"),
            "DELETE /channels/123/messages/:id"
        );
        assert_eq!(
            route_key("PUT", "/channels/1/messages/2/reactions/%F0%9F%91%8D/@me"),
            "PUT /channels/1/messages/:id/reactions/:emoji/@me"
        );
        assert_eq!(route_key("GET", "/users/@me"), "GET /users/@me");
        assert_eq!(major_of("GET /channels/123/messages"), "channels/123");
        assert_eq!(major_of("GET /users/@me"), "");
    }

    #[test]
    fn unknown_route_is_free() {
        let mut rl = RateLimiter::new();
        assert_eq!(rl.acquire("GET /x", Instant::now()), None);
    }

    #[test]
    fn exhausted_bucket_waits_until_reset() {
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        let route = route_key("POST", "/channels/1/messages");
        let h = RateLimitHeaders {
            bucket: Some("abc".into()),
            limit: Some(5),
            remaining: Some(1),
            reset_after: Some(2.0),
            global: false,
        };
        rl.update(&route, &h, now);
        assert_eq!(rl.acquire(&route, now), None); // uses the last slot
        assert_eq!(rl.acquire(&route, now), Some(Duration::from_secs(2)));
        let later = now + Duration::from_millis(2500);
        assert_eq!(rl.acquire(&route, later), None);
    }

    #[test]
    fn buckets_are_shared_per_major_param() {
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        let a = route_key("POST", "/channels/1/messages");
        let b = route_key("POST", "/channels/2/messages");
        let h = RateLimitHeaders {
            bucket: Some("same".into()),
            remaining: Some(0),
            reset_after: Some(1.0),
            ..Default::default()
        };
        rl.update(&a, &h, now);
        assert!(rl.acquire(&a, now).is_some());
        // Different channel: different bucket instance, not limited.
        assert_eq!(rl.acquire(&b, now), None);
    }

    #[test]
    fn global_429_blocks_everything() {
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        rl.on_429("POST /x", Duration::from_millis(500), true, now);
        assert_eq!(rl.acquire("GET /y", now), Some(Duration::from_millis(500)));
        assert_eq!(rl.acquire("GET /y", now + Duration::from_millis(600)), None);
    }

    #[test]
    fn route_429_blocks_route() {
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        rl.on_429(
            "POST /channels/1/typing",
            Duration::from_secs(3),
            false,
            now,
        );
        assert_eq!(
            rl.acquire("POST /channels/1/typing", now),
            Some(Duration::from_secs(3))
        );
        assert_eq!(rl.acquire("POST /channels/2/typing", now), None);
    }

    #[test]
    fn unknown_route_is_probed_by_one_request_at_a_time() {
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        let r = "POST /channels/1/messages";
        assert_eq!(rl.acquire(r, now), None);
        assert_eq!(rl.acquire(r, now), Some(PROBE_POLL));
        rl.update(
            r,
            &RateLimitHeaders {
                bucket: Some("b".into()),
                remaining: Some(3),
                reset_after: Some(1.0),
                ..Default::default()
            },
            now,
        );
        assert_eq!(rl.acquire(r, now), None);
        // A failed probe is released.
        let r2 = "GET /x";
        assert_eq!(rl.acquire(r2, now), None);
        rl.release(r2);
        assert_eq!(rl.acquire(r2, now), None);
        // ...and a lost one times out.
        assert_eq!(rl.acquire(r2, now + PROBE_TIMEOUT), None);
    }

    #[test]
    fn stale_bucket_is_reprobed_after_reset() {
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        let r = "POST /channels/1/messages";
        rl.acquire(r, now);
        rl.update(
            r,
            &RateLimitHeaders {
                bucket: Some("b".into()),
                remaining: Some(0),
                reset_after: Some(1.0),
                ..Default::default()
            },
            now,
        );
        let later = now + Duration::from_secs(2);
        assert_eq!(rl.acquire(r, later), None);
        assert_eq!(rl.acquire(r, later), Some(PROBE_POLL));
    }

    #[test]
    fn out_of_order_updates_keep_lowest_remaining() {
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        let r = "POST /channels/1/messages";
        let h = |rem, reset| RateLimitHeaders {
            bucket: Some("b".into()),
            remaining: Some(rem),
            reset_after: Some(reset),
            ..Default::default()
        };
        rl.update(r, &h(1, 2.0), now);
        // An older response (higher remaining, same window) arrives late.
        rl.update(r, &h(4, 2.05), now);
        assert_eq!(rl.acquire(r, now), None);
        assert!(rl.acquire(r, now).is_some(), "remaining must stay at 1");
    }

    #[test]
    fn garbage_header_values_are_clamped() {
        assert_eq!(secs(f64::NAN), Duration::ZERO);
        assert_eq!(secs(-5.0), Duration::ZERO);
        assert_eq!(secs(f64::INFINITY), Duration::ZERO);
        assert_eq!(secs(1e300), MAX_WAIT);
        let mut rl = RateLimiter::new();
        let now = Instant::now();
        rl.update(
            "GET /a",
            &RateLimitHeaders {
                remaining: Some(0),
                reset_after: Some(f64::MAX),
                ..Default::default()
            },
            now,
        );
        assert!(rl.acquire("GET /a", now).unwrap() <= MAX_WAIT);
    }

    #[test]
    fn parses_headers() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-ratelimit-bucket", "b1".parse().unwrap());
        h.insert("x-ratelimit-remaining", "3".parse().unwrap());
        h.insert("x-ratelimit-reset-after", "1.250".parse().unwrap());
        h.insert("x-ratelimit-global", "true".parse().unwrap());
        let p = RateLimitHeaders::from_headers(&h);
        assert_eq!(p.bucket.as_deref(), Some("b1"));
        assert_eq!(p.remaining, Some(3));
        assert_eq!(p.reset_after, Some(1.25));
        assert!(p.global);
    }
}
