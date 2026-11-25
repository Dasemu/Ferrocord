//! When automatic checks run.

use std::time::Duration;

/// First check: 10–20 s after startup, off the startup critical path.
pub const FIRST_MIN: Duration = Duration::from_secs(10);
pub const FIRST_MAX: Duration = Duration::from_secs(20);
/// Then every 6 h (plus up to 30 min of jitter), also while hidden in the
/// tray.
pub const INTERVAL: Duration = Duration::from_secs(6 * 3600);
pub const INTERVAL_JITTER: Duration = Duration::from_secs(30 * 60);
/// Longest back-off after GitHub's rate limit.
pub const MAX_BACKOFF: Duration = Duration::from_secs(24 * 3600);
/// Shortest back-off after a rate limit or a failed timed check.
pub const MIN_BACKOFF: Duration = Duration::from_secs(3600);

/// `lo + (hi - lo) * unit`, `unit` in `[0, 1]` (a random draw).
fn between(lo: Duration, hi: Duration, unit: f64) -> Duration {
    let unit = if unit.is_finite() {
        unit.clamp(0.0, 1.0)
    } else {
        0.0
    };
    lo + (hi - lo).mul_f64(unit)
}

/// Delay of the first automatic check.
pub fn first_delay(unit: f64) -> Duration {
    between(FIRST_MIN, FIRST_MAX, unit)
}

/// Delay between automatic checks.
pub fn next_delay(unit: f64) -> Duration {
    between(INTERVAL, INTERVAL + INTERVAL_JITTER, unit)
}

/// Delay after a rate limit (403/429): GitHub's `retry-after` or
/// `x-ratelimit-reset` when given, at least an hour, at most a day.
pub fn backoff(retry_after: Option<Duration>) -> Duration {
    retry_after
        .unwrap_or(MIN_BACKOFF)
        .clamp(MIN_BACKOFF, MAX_BACKOFF)
}

/// Whether the updater may touch the network at all: never in mock or
/// screenshot mode (they promise no network), and not in debug builds
/// unless `FERRO_UPDATE_FORCE=1`. Timed checks also need the user's
/// "Check for updates automatically" setting.
pub fn auto_allowed(mock: bool, screenshot: bool, debug_build: bool, force: bool) -> bool {
    !mock && !screenshot && (!debug_build || force)
}

/// `FERRO_UPDATE_FORCE=1`.
pub fn force_from_env() -> bool {
    std::env::var("FERRO_UPDATE_FORCE").is_ok_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_check_is_jittered_between_10_and_20_seconds() {
        assert_eq!(first_delay(0.0), Duration::from_secs(10));
        assert_eq!(first_delay(1.0), Duration::from_secs(20));
        assert_eq!(first_delay(0.5), Duration::from_secs(15));
        for _ in 0..1000 {
            let d = first_delay(fastrand::f64());
            assert!((FIRST_MIN..=FIRST_MAX).contains(&d));
        }
        // Out-of-range draws are clamped.
        assert_eq!(first_delay(-3.0), FIRST_MIN);
        assert_eq!(first_delay(7.0), FIRST_MAX);
        assert_eq!(first_delay(f64::NAN), FIRST_MIN);
    }

    #[test]
    fn later_checks_run_every_six_hours_with_jitter() {
        for _ in 0..1000 {
            let d = next_delay(fastrand::f64());
            assert!(d >= Duration::from_secs(6 * 3600));
            assert!(d <= Duration::from_secs(6 * 3600 + 1800));
        }
    }

    #[test]
    fn rate_limit_backoff_is_bounded() {
        assert_eq!(backoff(None), MIN_BACKOFF);
        assert_eq!(backoff(Some(Duration::from_secs(5))), MIN_BACKOFF);
        assert_eq!(
            backoff(Some(Duration::from_secs(7200))),
            Duration::from_secs(7200)
        );
        assert_eq!(backoff(Some(Duration::from_secs(10 * 86400))), MAX_BACKOFF);
    }

    #[test]
    fn no_automatic_checks_in_mock_screenshot_or_debug_builds() {
        assert!(auto_allowed(false, false, false, false));
        assert!(!auto_allowed(true, false, false, false));
        assert!(!auto_allowed(false, true, false, false));
        assert!(!auto_allowed(false, false, true, false));
        // FERRO_UPDATE_FORCE=1 lifts the debug-build rule only: mock and
        // screenshot mode never touch the network.
        assert!(auto_allowed(false, false, true, true));
        assert!(!auto_allowed(true, false, true, true));
        assert!(!auto_allowed(false, true, false, true));
    }
}
