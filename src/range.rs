use jiff::civil::{Date, ISOWeekDate, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};

use crate::error::AppError;

/// Parses a RANGE argument into inclusive local dates: `today`, `yesterday`, `week`,
/// `last-week`, `YYYY-MM-DD`, `YYYY-Www`, or `A..B` of any two of those.
pub fn parse(arg: &str, today: Date) -> Result<(Date, Date), AppError> {
    let bad = || AppError::Range(arg.to_owned());
    if let Some((a, b)) = arg.split_once("..") {
        let (from, _) = parse_one(a, today).ok_or_else(bad)?;
        let (_, to) = parse_one(b, today).ok_or_else(bad)?;
        return if from <= to {
            Ok((from, to))
        } else {
            Err(bad())
        };
    }
    parse_one(arg, today).ok_or_else(bad)
}

fn parse_one(arg: &str, today: Date) -> Option<(Date, Date)> {
    match arg {
        "today" => Some((today, today)),
        "yesterday" => today.yesterday().ok().map(|d| (d, d)),
        "week" => week_of(today),
        "last-week" => week_of(today.checked_sub(1.week()).ok()?),
        _ => {
            if let Some((year, week)) = arg.split_once("-W") {
                let monday =
                    ISOWeekDate::new(year.parse().ok()?, week.parse().ok()?, Weekday::Monday);
                week_of(monday.ok()?.date())
            } else {
                arg.parse().ok().map(|d| (d, d))
            }
        }
    }
}

/// Monday and Sunday of the ISO week holding `date`.
fn week_of(date: Date) -> Option<(Date, Date)> {
    let w = date.iso_week_date();
    let day = |wd| {
        ISOWeekDate::new(w.year(), w.week(), wd)
            .ok()
            .map(ISOWeekDate::date)
    };
    Some((day(Weekday::Monday)?, day(Weekday::Sunday)?))
}

/// The instants from the start of `from` to the end of `to` in `tz`, end exclusive.
pub fn span(from: Date, to: Date, tz: &TimeZone) -> Result<(Timestamp, Timestamp), AppError> {
    let start = from.to_zoned(tz.clone())?.timestamp();
    let end = to.tomorrow()?.to_zoned(tz.clone())?.timestamp();
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    const TODAY: Date = date(2026, 9, 25); // a Friday

    fn days(arg: &str) -> (Date, Date) {
        parse(arg, TODAY).unwrap()
    }

    #[test]
    fn named_ranges() {
        assert_eq!(days("today"), (TODAY, TODAY));
        assert_eq!(days("yesterday"), (date(2026, 9, 24), date(2026, 9, 24)));
        assert_eq!(days("week"), (date(2026, 9, 21), date(2026, 9, 27)));
        assert_eq!(days("last-week"), (date(2026, 9, 14), date(2026, 9, 20)));
    }

    #[test]
    fn dates_and_iso_weeks() {
        assert_eq!(days("2026-09-01"), (date(2026, 9, 1), date(2026, 9, 1)));
        assert_eq!(days("2026-W39"), (date(2026, 9, 21), date(2026, 9, 27)));
        // ISO week 1 of 2026 starts in 2025.
        assert_eq!(days("2026-W01"), (date(2025, 12, 29), date(2026, 1, 4)));
    }

    #[test]
    fn spans_join_both_ends() {
        assert_eq!(days("2026-09-01..2026-09-25"), (date(2026, 9, 1), TODAY));
        assert_eq!(days("2026-W38..today"), (date(2026, 9, 14), TODAY));
    }

    #[test]
    fn bad_ranges_are_errors() {
        for arg in [
            "",
            "tomorrow",
            "2026-13-01",
            "2026-W54",
            "2026-09-25..2026-09-01",
            "a..b",
        ] {
            assert!(parse(arg, TODAY).is_err(), "{arg}");
        }
    }

    #[test]
    fn span_covers_whole_local_days() {
        let tz = TimeZone::fixed(jiff::tz::offset(2));
        let (from, to) = span(TODAY, TODAY, &tz).unwrap();
        assert_eq!(from.to_string(), "2026-09-24T22:00:00Z");
        assert_eq!(to.to_string(), "2026-09-25T22:00:00Z");
    }
}
