//! The hours of the US stock market, by the New York clock: when it is pre-market, open, after hours
//! or closed. CNBC's own status field says `REG_MKT` at three in the morning, so the header goes by
//! the clock instead. Holidays are not known: on one the clock says open while the exchange is shut.

use crate::quote::{Session, days_from_civil};

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;

/// The year of a day, counted in days from 1970-01-01.
fn year_of(days: i64) -> i64 {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    year_of_era + era * 400 + i64::from(month_index >= 10)
}

/// The weekday of a day counted from 1970-01-01, 0 for Sunday.
fn weekday(days: i64) -> i64 {
    (days + 4).rem_euclid(7)
}

/// The first Sunday on or after `days`.
fn sunday_on_or_after(days: i64) -> i64 {
    days + (7 - weekday(days)) % 7
}

/// Whether New York keeps summer time at this Unix time: from the second Sunday of March, 02:00 local
/// (07:00 UTC), to the first Sunday of November, 02:00 local (06:00 UTC).
fn summer_time(unix: i64) -> bool {
    let year = year_of(unix.div_euclid(DAY));
    let starts = (sunday_on_or_after(days_from_civil(year, 3, 1)) + 7) * DAY + 7 * HOUR;
    let ends = sunday_on_or_after(days_from_civil(year, 11, 1)) * DAY + 6 * HOUR;
    (starts..ends).contains(&unix)
}

/// New York's offset from UTC at a Unix time, in seconds: minus five hours, minus four in summer.
#[must_use]
pub fn new_york_offset(unix: i64) -> i64 {
    if summer_time(unix) { -4 * HOUR } else { -5 * HOUR }
}

/// The session of the US stock market at a Unix time: pre-market from 4:00, open from 9:30, after
/// hours from 16:00 to 20:00, on weekdays by the New York clock.
#[must_use]
pub fn us_session(unix: i64) -> Session {
    let local = unix + new_york_offset(unix);
    let days = local.div_euclid(DAY);
    if matches!(weekday(days), 0 | 6) {
        return Session::Closed;
    }
    let secs = local.rem_euclid(DAY);
    if secs < 4 * HOUR {
        Session::Closed
    } else if secs < 9 * HOUR + 30 * 60 {
        Session::Pre
    } else if secs < 16 * HOUR {
        Session::Regular
    } else if secs < 20 * HOUR {
        Session::Post
    } else {
        Session::Closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Unix time from a UTC date and time.
    fn utc(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> i64 {
        days_from_civil(year, month, day) * DAY + hour * HOUR + minute * 60
    }

    #[test]
    fn years_and_weekdays_come_out_of_the_day_count() {
        assert_eq!(year_of(0), 1970);
        assert_eq!(year_of(days_from_civil(2026, 12, 31)), 2026);
        assert_eq!(year_of(days_from_civil(2027, 1, 1)), 2027);
        assert_eq!(year_of(days_from_civil(2000, 2, 29)), 2000);
        assert_eq!(weekday(0), 4, "1970-01-01 was a Thursday");
        assert_eq!(weekday(days_from_civil(2026, 11, 22)), 0, "a Sunday");
        assert_eq!(weekday(days_from_civil(2026, 11, 24)), 2, "a Tuesday");
    }

    #[test]
    fn summer_time_runs_from_the_second_sunday_of_march_to_the_first_of_november() {
        // 2026: March 8 and November 1.
        assert!(!summer_time(utc(2026, 3, 8, 6, 59)));
        assert!(summer_time(utc(2026, 3, 8, 7, 0)));
        assert!(summer_time(utc(2026, 10, 31, 12, 0)));
        assert!(summer_time(utc(2026, 11, 1, 5, 59)));
        assert!(!summer_time(utc(2026, 11, 1, 6, 0)));
        assert!(!summer_time(utc(2026, 1, 15, 12, 0)));
        // 2027: March 14 and November 7.
        assert!(!summer_time(utc(2027, 3, 14, 6, 59)));
        assert!(summer_time(utc(2027, 3, 14, 7, 0)));
        assert!(!summer_time(utc(2027, 11, 7, 6, 0)));
    }

    #[test]
    fn a_winter_weekday_goes_through_its_sessions_by_the_new_york_clock() {
        // Tuesday 2026-11-24, New York on standard time: UTC is five hours ahead.
        assert_eq!(us_session(utc(2026, 11, 24, 7, 47)), Session::Closed, "02:47");
        assert_eq!(us_session(utc(2026, 11, 24, 9, 0)), Session::Pre, "04:00");
        assert_eq!(us_session(utc(2026, 11, 24, 14, 29)), Session::Pre, "09:29");
        assert_eq!(us_session(utc(2026, 11, 24, 14, 30)), Session::Regular, "09:30");
        assert_eq!(us_session(utc(2026, 11, 24, 20, 59)), Session::Regular, "15:59");
        assert_eq!(us_session(utc(2026, 11, 24, 21, 0)), Session::Post, "16:00");
        assert_eq!(us_session(utc(2026, 11, 25, 0, 59)), Session::Post, "19:59");
        assert_eq!(us_session(utc(2026, 11, 25, 1, 0)), Session::Closed, "20:00");
    }

    #[test]
    fn new_york_is_four_or_five_hours_behind() {
        assert_eq!(new_york_offset(utc(2026, 7, 15, 12, 0)), -4 * HOUR);
        assert_eq!(new_york_offset(utc(2026, 11, 24, 12, 0)), -5 * HOUR);
    }

    #[test]
    fn a_summer_weekday_is_an_hour_earlier_in_utc() {
        assert_eq!(us_session(utc(2026, 7, 15, 13, 29)), Session::Pre);
        assert_eq!(us_session(utc(2026, 7, 15, 13, 30)), Session::Regular);
        assert_eq!(us_session(utc(2026, 7, 15, 20, 0)), Session::Post);
    }

    #[test]
    fn the_weekend_is_closed_and_the_new_york_date_decides_the_day() {
        assert_eq!(us_session(utc(2026, 11, 20, 15, 0)), Session::Regular, "Friday 10:00");
        assert_eq!(us_session(utc(2026, 11, 21, 15, 0)), Session::Closed, "Saturday 10:00");
        assert_eq!(us_session(utc(2026, 11, 22, 15, 0)), Session::Closed, "Sunday 10:00");
        // Friday 20:30 in New York is already Saturday in UTC, and closed all the same.
        assert_eq!(us_session(utc(2026, 11, 21, 1, 30)), Session::Closed);
        // Sunday 22:00 in New York is Monday 03:00 UTC: closed. Monday 04:30 there is 09:30 UTC: pre-market.
        assert_eq!(us_session(utc(2026, 11, 23, 3, 0)), Session::Closed);
        assert_eq!(us_session(utc(2026, 11, 23, 9, 30)), Session::Pre);
    }
}
