//! Small time helpers.
//!
//! `eggpass` deliberately avoids a date/time crate: the only things it needs
//! are "now" in unix seconds, converting an RFC 3339 timestamp (the format the
//! official Grok CLI writes) into unix seconds, printing a log timestamp, and
//! reading the local UTC offset so a quota reset time can be shown in the
//! user's own timezone.
//!
//! The local offset is resolved with `libc::localtime` at the requested
//! instant, so daylight-saving changes use the offset in force then.

use std::str::FromStr;

use toml::value::{Datetime, Offset};

/// The month abbreviations, in order.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Serialises the calls that read or move `TZ`.
///
/// `TZ` is process-global, so two lookups in different zones must not
/// overlap.
#[cfg(unix)]
static TZ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// The offset from UTC, in seconds, that the local zone was at `unix`.
///
/// `None` when the platform cannot tell us — the caller falls back to UTC.
///
/// The offset is looked up at the instant asked for rather than at the
/// current one, so a time on the far side of a daylight-saving change
/// carries the offset that was in force then.
#[cfg(unix)]
pub fn local_offset_seconds(unix: i64) -> Option<i64> {
    offset_at(unix)
}

/// The offset from UTC, in seconds, that the local zone was at `unix`.
///
/// A platform without a usable local zone falls back to UTC.
#[cfg(not(unix))]
pub fn local_offset_seconds(_unix: i64) -> Option<i64> {
    None
}

/// The offset from UTC in a named zone, at `unix`.
///
/// This is the tested primitive: the zone is applied for the lookup only, and
/// the process's own `TZ` is restored before the function returns.
///
/// # Safety of the environment
///
/// `TZ` is process-global. This function sets it, reads it back and restores
/// it, but another thread reading `TZ` at the same time would see the
/// temporary value. It is meant for tests and for one-off lookups, not for
/// concurrent use.
#[cfg(all(test, unix))]
pub fn local_offset_seconds_in(zone: &str, unix: i64) -> Option<i64> {
    let _guard = tz_lock();
    let previous = std::env::var_os("TZ");

    // SAFETY: the environment is mutated and restored inside this function.
    // The window is small and the value is a plain string.
    unsafe { std::env::set_var("TZ", zone) };
    let offset = offset_at_locked(unix);

    // Put the zone back, and let the C library read it again: `localtime`
    // re-reads `TZ` every time, so this leaves the process's zone as it was.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TZ", value),
            None => std::env::remove_var("TZ"),
        }
    }
    let _ = offset_at_locked(unix);

    offset
}

/// The lock that serialises the reads of, and the moves of, `TZ`.
#[cfg(unix)]
fn tz_lock() -> std::sync::MutexGuard<'static, ()> {
    TZ_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

/// The offset from UTC, in seconds, at `unix`, in the process's own zone.
#[cfg(unix)]
fn offset_at(unix: i64) -> Option<i64> {
    let _guard = tz_lock();
    offset_at_locked(unix)
}

/// The offset from UTC, in seconds, at `unix`, in the process's own zone.
///
/// The caller holds the lock.
///
/// `localtime` is used rather than `localtime_r` because the zone has to be
/// read *now*: on glibc `localtime_r` parses the zone the first time it is
/// called in the process and reuses that parse afterwards, while `localtime`
/// re-reads `TZ` every time. `localtime` answers into a static buffer, which
/// is why the lock is held and the answer is copied out.
#[cfg(unix)]
fn offset_at_locked(unix: i64) -> Option<i64> {
    let time = unix as libc::time_t;

    // SAFETY: `localtime` returns a pointer to a static buffer it fills, or
    // null on failure. The lock is held, so the buffer is ours to read, and
    // the answer is copied out before the lock is released.
    let broken = unsafe { libc::localtime(&time) };
    if broken.is_null() {
        return None;
    }
    let broken = unsafe { *broken };

    // `tm_gmtoff` is a `c_long`, which is 32 bits on some targets and 64 on
    // others. The cast is a no-op where they agree, and clippy notices.
    #[allow(clippy::unnecessary_cast)]
    let offset = broken.tm_gmtoff as i64;
    Some(offset)
}

/// Format a local date and time, as `Nov 18, 09:00`.
///
/// The offset is a parameter rather than something looked up here, so that
/// the rendering can be checked against a fixed zone instead of against
/// whatever the machine happens to be in.
///
/// The year is printed only when it differs from the year of `now`: a reset
/// that lands in January would otherwise read as a date in the past.
pub fn format_local_datetime(unix: i64, now: i64, offset: i64) -> String {
    let (year, month, day, hour, minute) = civil_parts(unix + offset);
    let (now_year, _, _, _, _) = civil_parts(now + offset);

    if year == now_year {
        format!("{} {day:02}, {hour:02}:{minute:02}", month_name(month))
    } else {
        format!(
            "{} {day:02}, {year} {hour:02}:{minute:02}",
            month_name(month)
        )
    }
}

/// Format a local date and time in a named zone, as `Nov 18, 09:00`.
#[cfg(all(test, unix))]
pub fn format_local_datetime_in(zone: &str, unix: i64, now: i64) -> String {
    let offset = local_offset_seconds_in(zone, unix).unwrap_or(0);
    format_local_datetime(unix, now, offset)
}

/// Format an offset from UTC, as `UTC+01:00`.
///
/// A zero offset reads as plain `UTC`.
pub fn format_utc_offset(offset: i64) -> String {
    if offset == 0 {
        return "UTC".to_string();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let total = offset.abs();
    format!("UTC{sign}{:02}:{:02}", total / 3_600, (total % 3_600) / 60)
}

/// Format the local offset from UTC in a named zone, as `UTC+01:00`.
#[cfg(all(test, unix))]
pub fn format_local_offset_in(zone: &str, unix: i64) -> String {
    format_utc_offset(local_offset_seconds_in(zone, unix).unwrap_or(0))
}

/// The year, month, day, hour and minute of a unix timestamp, as UTC fields.
fn civil_parts(unix: i64) -> (i64, i64, i64, i64, i64) {
    let days = unix.div_euclid(86_400);
    let rest = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (year, month, day, rest / 3_600, (rest % 3_600) / 60)
}

/// The abbreviation of a month number.
fn month_name(month: i64) -> &'static str {
    match month {
        1..=12 => MONTHS[(month - 1) as usize],
        _ => "???",
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

    /// A platform without a local zone cannot answer this.
    #[cfg(unix)]
    #[test]
    fn the_offset_is_zero_at_utc() {
        assert_eq!(local_offset_seconds_in("UTC", 0), Some(0));
        assert_eq!(local_offset_seconds_in("UTC0", 1_794_546_738), Some(0));
    }

    /// A platform without a local zone cannot answer this.
    #[cfg(unix)]
    #[test]
    fn a_known_offset_is_applied() {
        // A POSIX `TZ` string spells the offset the other way round: the
        // value is the time to add to the local time to reach UTC, so a zone
        // two hours ahead of UTC is `UTC-02:00`.
        let summer = parse_rfc3339_to_unix("2026-07-01T12:00:00Z").unwrap();
        assert_eq!(local_offset_seconds_in("UTC-02:00", summer), Some(7_200));
    }

    /// A platform without a local zone cannot answer this.
    #[cfg(unix)]
    #[test]
    fn a_negative_offset_is_applied() {
        let summer = parse_rfc3339_to_unix("2026-07-01T12:00:00Z").unwrap();
        assert_eq!(
            local_offset_seconds_in("UTC+05:00", summer),
            Some(-5 * 3_600)
        );
    }

    /// A platform without a local zone cannot answer this.
    #[cfg(unix)]
    #[test]
    fn dst_is_resolved_at_the_instant() {
        // Eastern time, with the daylight-saving rules spelled out so the
        // test does not depend on a zone database being installed.
        let zone = "EST5EDT,M3.2.0,M11.1.0";
        let winter = parse_rfc3339_to_unix("2026-01-15T12:00:00Z").unwrap();
        let summer = parse_rfc3339_to_unix("2026-07-15T12:00:00Z").unwrap();

        assert_eq!(local_offset_seconds_in(zone, winter), Some(-5 * 3_600));
        assert_eq!(local_offset_seconds_in(zone, summer), Some(-4 * 3_600));
        assert_ne!(
            local_offset_seconds_in(zone, winter),
            local_offset_seconds_in(zone, summer)
        );
    }

    /// A platform without a local zone cannot answer this.
    #[cfg(unix)]
    #[test]
    fn the_local_time_is_the_utc_time_plus_the_offset() {
        let unix = parse_rfc3339_to_unix("2026-11-18T08:00:00Z").unwrap();
        assert_eq!(
            format_local_datetime_in("UTC-01:00", unix, unix),
            "Nov 18, 09:00"
        );
        assert_eq!(format_local_offset_in("UTC-01:00", unix), "UTC+01:00");
    }

    /// A platform without a local zone cannot answer this.
    #[cfg(unix)]
    #[test]
    fn the_year_appears_only_when_it_differs() {
        let now = parse_rfc3339_to_unix("2026-11-14T00:00:00Z").unwrap();
        let same_year = parse_rfc3339_to_unix("2026-12-30T09:00:00Z").unwrap();
        let next_year = parse_rfc3339_to_unix("2027-01-06T09:00:00Z").unwrap();

        assert_eq!(
            format_local_datetime_in("UTC", same_year, now),
            "Dec 30, 09:00"
        );
        assert_eq!(
            format_local_datetime_in("UTC", next_year, now),
            "Jan 06, 2027 09:00"
        );
    }

    /// A platform without a local zone cannot answer this.
    #[cfg(unix)]
    #[test]
    fn a_zero_offset_reads_as_utc() {
        let unix = parse_rfc3339_to_unix("2026-11-18T08:00:00Z").unwrap();
        assert_eq!(format_local_offset_in("UTC", unix), "UTC");
        assert_eq!(format_local_offset_in("UTC+00:00", unix), "UTC");
    }

    #[test]
    fn an_impossible_local_time_falls_back_to_utc() {
        // A platform that cannot tell us the offset renders in UTC, and says
        // so rather than guessing at one.
        assert_eq!(format_utc_offset(0), "UTC");

        let unix = parse_rfc3339_to_unix("2026-11-18T08:00:00Z").unwrap();
        assert_eq!(format_local_datetime(unix, unix, 0), "Nov 18, 08:00");
    }
}
