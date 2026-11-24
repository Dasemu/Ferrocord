//! Opt-in animation timing statistics (`FERROCORD_FRAME_STATS=1`).
//!
//! The self-playing images ([`super::anim_image`]) report every redraw they
//! see and how late it came compared with the time they asked for. Every
//! five seconds one `info` line sums it up in the log: window redraws per
//! second, `view()` rebuilds per second, animation frame changes per second and the lateness of the
//! redraws (mean / max). Off by default: then every call is one relaxed
//! atomic load.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static ENABLED: OnceLock<bool> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();
/// Nanoseconds (since `START`) of the last redraw seen, to count each
/// window redraw once however many images see it.
static LAST_REDRAW: AtomicU64 = AtomicU64::new(0);
static REDRAWS: AtomicU64 = AtomicU64::new(0);
static CHANGES: AtomicU64 = AtomicU64::new(0);
static VIEWS: AtomicU64 = AtomicU64::new(0);
static LATE_SUM_US: AtomicU64 = AtomicU64::new(0);
static LATE_MAX_US: AtomicU64 = AtomicU64::new(0);
static LATE_N: AtomicU64 = AtomicU64::new(0);
static WINDOW_START: AtomicU64 = AtomicU64::new(0);
static REPORTING: AtomicBool = AtomicBool::new(false);

const REPORT_EVERY: Duration = Duration::from_secs(5);

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("FERROCORD_FRAME_STATS").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

fn nanos(at: Instant) -> u64 {
    let start = *START.get_or_init(Instant::now);
    at.saturating_duration_since(start).as_nanos() as u64
}

/// `view()` ran (each run rebuilds and re-lays out the widget tree).
pub fn view_built() {
    if enabled() {
        VIEWS.fetch_add(1, Ordering::Relaxed);
    }
}

/// An image saw the redraw at `now`; `asked` is the time it had asked for
/// (if this redraw is the one it asked for) and `changed` whether its frame
/// changed.
pub fn record(now: Instant, asked: Option<Instant>, changed: bool) {
    if !enabled() {
        return;
    }
    let t = nanos(now).max(1);
    if LAST_REDRAW.swap(t, Ordering::Relaxed) != t {
        REDRAWS.fetch_add(1, Ordering::Relaxed);
    }
    if changed {
        CHANGES.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(at) = asked.filter(|at| *at <= now) {
        let late = now.duration_since(at).as_micros() as u64;
        LATE_SUM_US.fetch_add(late, Ordering::Relaxed);
        LATE_MAX_US.fetch_max(late, Ordering::Relaxed);
        LATE_N.fetch_add(1, Ordering::Relaxed);
    }
    let w = WINDOW_START.load(Ordering::Relaxed);
    if w == 0 {
        WINDOW_START.store(t, Ordering::Relaxed);
        return;
    }
    let span = t.saturating_sub(w);
    if span >= REPORT_EVERY.as_nanos() as u64 && !REPORTING.swap(true, Ordering::Relaxed) {
        let secs = span as f64 / 1e9;
        let redraws = REDRAWS.swap(0, Ordering::Relaxed);
        let changes = CHANGES.swap(0, Ordering::Relaxed);
        let views = VIEWS.swap(0, Ordering::Relaxed);
        let n = LATE_N.swap(0, Ordering::Relaxed);
        let sum = LATE_SUM_US.swap(0, Ordering::Relaxed);
        let max = LATE_MAX_US.swap(0, Ordering::Relaxed);
        tracing::info!(
            target: "ferrocord::frame_stats",
            redraws_per_s = format!("{:.1}", redraws as f64 / secs),
            views_per_s = format!("{:.1}", views as f64 / secs),
            frame_changes_per_s = format!("{:.1}", changes as f64 / secs),
            late_mean_ms = format!("{:.2}", if n > 0 { sum as f64 / n as f64 / 1000.0 } else { 0.0 }),
            late_max_ms = format!("{:.2}", max as f64 / 1000.0),
            "animation frame stats"
        );
        WINDOW_START.store(t, Ordering::Relaxed);
        REPORTING.store(false, Ordering::Relaxed);
    }
}
