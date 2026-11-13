//! Small time helpers.
//!
//! `eggpass` deliberately avoids a date/time crate: the only things it needs
//! are "now" in unix seconds, converting an RFC 3339 timestamp (the format the
//! official Grok CLI writes) into unix seconds, and printing a log timestamp.

use std::str::FromStr;

use toml::value::{Datetime, Offset};

/// The current time as seconds since the unix epoch.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Days between the unix epoch and a civil date.
///
/// Howard Hinnant's `days_from_civil`, valid for any Gregorian date.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// The civil date that is `days` after the unix epoch.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let day_of_era = days - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Parse an RFC 3339 timestamp into seconds since the unix epoch.
///
/// A timestamp without an offset is treated as UTC.
pub fn parse_rfc3339_to_unix(text: &str) -> Option<i64> {
    let parsed = Datetime::from_str(text).ok()?;
    let date = parsed.date?;
    let time = parsed.time?;
    let days = days_from_civil(date.year as i64, date.month as i64, date.day as i64);
    let seconds = days * 86_400
        + time.hour as i64 * 3_600
        + time.minute as i64 * 60
        + time.second.unwrap_or(0) as i64;
    let offset_seconds = match parsed.offset {
        Some(Offset::Z) | None => 0,
        Some(Offset::Custom { minutes }) => minutes as i64 * 60,
    };
    Some(seconds - offset_seconds)
}

/// Format seconds since the unix epoch as an RFC 3339 UTC timestamp.
pub fn format_rfc3339(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let rest = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// Render a duration the way `eggpass status` shows it.
pub fn humanize_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(parse_rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn known_timestamps_parse() {
        assert_eq!(
            parse_rfc3339_to_unix("2026-12-13T05:12:18Z"),
            Some(1_797_138_738)
        );
        assert_eq!(
            parse_rfc3339_to_unix("2026-11-13T05:12:18Z"),
            Some(1_794_546_738)
        );
    }

    #[test]
    fn offsets_are_honoured() {
        let utc = parse_rfc3339_to_unix("2026-01-01T12:00:00Z").unwrap();
        let plus_one = parse_rfc3339_to_unix("2026-01-01T13:00:00+01:00").unwrap();
        assert_eq!(utc, plus_one);
    }

    #[test]
    fn formatting_round_trips() {
        let unix = 1_797_138_738;
        let text = format_rfc3339(unix);
        assert_eq!(text, "2026-12-13T05:12:18Z");
        assert_eq!(parse_rfc3339_to_unix(&text), Some(unix));
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(humanize_duration(45), "45s");
        assert_eq!(humanize_duration(90), "1m 30s");
        assert_eq!(humanize_duration(5 * 3_600 + 12 * 60), "5h 12m");
        assert_eq!(humanize_duration(-5), "0s");
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(parse_rfc3339_to_unix("not a timestamp"), None);
        assert_eq!(parse_rfc3339_to_unix(""), None);
    }
}
