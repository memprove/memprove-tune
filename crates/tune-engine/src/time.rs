//! UTC timestamps without a date-time dependency.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `now` as `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn rfc3339(now: SystemTime) -> String {
    let (date, seconds) = split(now);
    format!(
        "{date}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// `now` as `YYYYMMDDTHHMMSSZ`, for directory names.
#[must_use]
pub fn compact(now: SystemTime) -> String {
    rfc3339(now).replace(['-', ':'], "")
}

/// Milliseconds in `duration`, saturating.
#[must_use]
pub fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn split(now: SystemTime) -> (String, u64) {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = i64::try_from(since / 86_400).unwrap_or(i64::MAX);
    let (year, month, day) = civil_from_days(days);
    (format!("{year:04}-{month:02}-{day:02}"), since % 86_400)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a
/// proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let instant = UNIX_EPOCH + Duration::from_secs(1_794_694_403);
        assert_eq!(rfc3339(instant), "2026-11-14T22:13:23Z");
        assert_eq!(compact(instant), "20261114T221323Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(rfc3339(leap), "2000-02-29T00:00:00Z");
    }
}
