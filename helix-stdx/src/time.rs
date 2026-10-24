use std::sync::LazyLock;
use std::time::{Instant, SystemTime};

const SECOND: i64 = 1;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const MONTH: i64 = 30 * DAY;
const YEAR: i64 = 365 * DAY;

/// Like `std::time::SystemTime::now()` but does not cause a syscall on every invocation.
///
/// There is just one syscall at the start of the program, subsequent invocations are
/// much cheaper and use the monotonic clock instead of trigerring a syscall.
#[inline]
fn now() -> SystemTime {
    static START_INSTANT: LazyLock<Instant> = LazyLock::new(Instant::now);
    static START_SYSTEM_TIME: LazyLock<SystemTime> = LazyLock::new(SystemTime::now);

    *START_SYSTEM_TIME + START_INSTANT.elapsed()
}

/// Formats a timestamp into a human-readable relative time string.
///
/// # Arguments
///
/// * `timestamp` - A point in history. Seconds since UNIX epoch (UTC)
/// * `timezone_offset` - Timezone offset in seconds
///
/// # Returns
///
/// A String representing the relative time (e.g., "4 years ago", "11 months from now")
#[inline]
pub fn format_relative_time(timestamp: i64, timezone_offset: i32) -> String {
    let timestamp = timestamp + timezone_offset as i64;
    let now = now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        + timezone_offset as i64;

    let time_passed = now - timestamp;

    let time_difference = time_passed.abs();

    let (value, unit) = if time_difference >= YEAR {
        let years = time_difference / YEAR;
        (years, if years == 1 { "year" } else { "years" })
    } else if time_difference >= MONTH {
        let months = time_difference / MONTH;
        (months, if months == 1 { "month" } else { "months" })
    } else if time_difference >= DAY {
        let days = time_difference / DAY;
        (days, if days == 1 { "day" } else { "days" })
    } else if time_difference >= HOUR {
        let hours = time_difference / HOUR;
        (hours, if hours == 1 { "hour" } else { "hours" })
    } else if time_difference >= MINUTE {
        let minutes = time_difference / MINUTE;
        (minutes, if minutes == 1 { "minute" } else { "minutes" })
    } else {
        let seconds = time_difference / SECOND;
        (seconds, if seconds == 1 { "second" } else { "seconds" })
    };
    let value = value.to_string();

    let label = if time_passed >= 0 { "ago" } else { "from now" };

    format!("{value} {unit} {label}")
}

#[cfg(test)]
mod test {
    use super::*;

    fn now_secs() -> i64 {
        now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    // The clock keeps ticking while the test runs, so only use margins well inside a unit.
    #[test]
    fn past() {
        let now = now_secs();
        assert_eq!(format_relative_time(now - 30 * SECOND, 0), "30 seconds ago");
        assert_eq!(format_relative_time(now - MINUTE - 5, 0), "1 minute ago");
        assert_eq!(
            format_relative_time(now - 5 * MINUTE - 5, 0),
            "5 minutes ago"
        );
        assert_eq!(format_relative_time(now - HOUR - 5, 0), "1 hour ago");
        assert_eq!(format_relative_time(now - 3 * HOUR - 5, 0), "3 hours ago");
        assert_eq!(format_relative_time(now - DAY - 5, 0), "1 day ago");
        assert_eq!(format_relative_time(now - 10 * DAY - 5, 0), "10 days ago");
        assert_eq!(format_relative_time(now - MONTH - 5, 0), "1 month ago");
        assert_eq!(
            format_relative_time(now - 11 * MONTH - 5, 0),
            "11 months ago"
        );
        assert_eq!(format_relative_time(now - YEAR - 5, 0), "1 year ago");
        assert_eq!(format_relative_time(now - 4 * YEAR - 5, 0), "4 years ago");
    }

    #[test]
    fn future() {
        let now = now_secs();
        assert_eq!(
            format_relative_time(now + 5 * MINUTE + 30, 0),
            "5 minutes from now"
        );
        assert_eq!(
            format_relative_time(now + 11 * MONTH + DAY, 0),
            "11 months from now"
        );
    }

    #[test]
    fn just_now_is_not_in_the_future() {
        // the clock may tick over to the next second between the two readings
        let formatted = format_relative_time(now_secs(), 0);
        assert!(
            formatted == "0 seconds ago" || formatted == "1 second ago",
            "{formatted}"
        );
    }

    #[test]
    fn timezone_offset_does_not_change_the_difference() {
        let now = now_secs();
        for offset in [-12 * HOUR, 0, 5 * HOUR + 30 * MINUTE, 14 * HOUR] {
            assert_eq!(
                format_relative_time(now - 3 * DAY - 5, offset as i32),
                "3 days ago"
            );
        }
    }
}
