//! 12- or 24-hour clock for every timestamp the views format (Appearance
//! "Time format", synced as `timestamp_hour_cycle`). The formatters are free
//! functions called from many views, so the choice is a flag. `update` and
//! `view` run on the same (UI) thread, so the flag is thread-local: tests
//! running in parallel never see each other's clock.

use std::cell::Cell;

thread_local! {
    static H24: Cell<bool> = const { Cell::new(false) };
}

pub fn set_24h(on: bool) {
    H24.with(|h| h.set(on));
}

pub fn is_24h() -> bool {
    H24.with(Cell::get)
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
