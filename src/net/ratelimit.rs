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
    /// `None` a slot has been reserved (remaining is decremented).
    pub fn acquire(&mut self, route: &str, now: Instant) -> Option<Duration> {
        if let Some(until) = self.global_until {
            if until > now {
                return Some(until - now);
            }
            self.global_until = None;
        }
        let key = self.bucket_key(route);
        let bucket = match key {
            Some(k) => self.buckets.get_mut(&k),
            None => self.unbucketed.get_mut(route),
        };
        let Some(b) = bucket else { return None };
        if b.reset_at <= now {
            // Bucket has reset; the next response will tell us the new state.
            return None;
        }
        if b.remaining == 0 {
            return Some(b.reset_at - now);
        }
        b.remaining -= 1;
        None
    }

    /// Records the headers of a response.
    pub fn update(&mut self, route: &str, h: &RateLimitHeaders, now: Instant) {
        let Some(remaining) = h.remaining else { return };
        let reset_at = now + Duration::from_secs_f64(h.reset_after.unwrap_or(0.0).max(0.0));
        let state = Bucket {
            remaining,
            reset_at,
        };
        if let Some(hash) = &h.bucket {
            self.routes.insert(route.to_owned(), hash.clone());
            self.unbucketed.remove(route);
            let key = format!("{hash}:{}", major_of(route));
            self.buckets.insert(key, state);
        } else {
            self.unbucketed.insert(route.to_owned(), state);
        }
    }

    /// Records a 429. `retry_after` comes from the JSON body (preferred) or
    /// the `Retry-After` header.
    pub fn on_429(&mut self, route: &str, retry_after: Duration, global: bool, now: Instant) {
        let until = now + retry_after;
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
