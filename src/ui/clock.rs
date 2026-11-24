//! 12- or 24-hour clock for every timestamp the views format (Appearance
//! "24-Hour Time", synced as `timestamp_hour_cycle`). A process-wide flag:
//! the formatters are free functions called from many views.

use std::sync::atomic::{AtomicBool, Ordering};

static H24: AtomicBool = AtomicBool::new(false);

pub fn set_24h(on: bool) {
    H24.store(on, Ordering::Relaxed);
}

pub fn is_24h() -> bool {
    H24.load(Ordering::Relaxed)
}

/// `chrono` format of a time of day.
pub fn time_format(h24: bool, seconds: bool) -> &'static str {
    match (h24, seconds) {
        (false, false) => "%-I:%M %p",
        (false, true) => "%-I:%M:%S %p",
        (true, false) => "%H:%M",
        (true, true) => "%H:%M:%S",
    }
}

/// The time format for the current setting.
pub fn hm() -> &'static str {
    time_format(is_24h(), false)
}

pub fn hms() -> &'static str {
    time_format(is_24h(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    #[test]
    fn formats() {
        let t = NaiveDate::from_ymd_opt(2026, 11, 24)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(14, 3, 9).unwrap());
        assert_eq!(t.format(time_format(false, false)).to_string(), "2:03 PM");
        assert_eq!(t.format(time_format(true, false)).to_string(), "14:03");
        assert_eq!(t.format(time_format(true, true)).to_string(), "14:03:09");
    }
}
