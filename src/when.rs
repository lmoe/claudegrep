use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, Months, NaiveDate, NaiveTime, Utc};
use strum::{EnumString, IntoStaticStr};

use crate::timestamp::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

const FORMATS: &str = "a date (2026-10-14) or an age: 30m, 6h, 2d, 1w, 3mo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// A local calendar day: `--since` starts at its first millisecond, `--until` ends at its last.
    Date(NaiveDate),
    Ago(u32, Unit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
pub enum Unit {
    #[strum(serialize = "m")]
    Minutes,
    #[strum(serialize = "h")]
    Hours,
    #[strum(serialize = "d")]
    Days,
    #[strum(serialize = "w")]
    Weeks,
    #[strum(serialize = "mo")]
    Months,
}

impl Unit {
    /// `amount` of this unit as a fixed span, or `None` for months, whose length varies, and on overflow.
    fn span(self, amount: u32) -> Option<Duration> {
        let amount = i64::from(amount);
        match self {
            Self::Minutes => Duration::try_minutes(amount),
            Self::Hours => Duration::try_hours(amount),
            Self::Days => Duration::try_days(amount),
            Self::Weeks => Duration::try_weeks(amount),
            Self::Months => None,
        }
    }
}

impl FromStr for When {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            return Ok(Self::Date(date));
        }
        ago(value).ok_or_else(|| format!("expected {FORMATS}"))
    }
}

fn ago(value: &str) -> Option<When> {
    let split = value.find(|c: char| !c.is_ascii_digit())?;
    let (amount, unit) = value.split_at(split);
    Some(When::Ago(amount.parse().ok()?, unit.parse().ok()?))
}

impl When {
    pub fn resolve(self, edge: Edge, now: DateTime<Local>) -> Result<Timestamp> {
        let instant = match self {
            Self::Date(date) => {
                let time = match edge {
                    Edge::Start => NaiveTime::MIN,
                    Edge::End => NaiveTime::from_hms_milli_opt(23, 59, 59, 999).expect("valid time"),
                };
                date.and_time(time)
                    .and_local_timezone(Local)
                    .earliest()
                    .with_context(|| format!("{date} does not exist in the local time zone"))?
                    .with_timezone(&Utc)
            }
            Self::Ago(amount, unit) => {
                let then = match unit {
                    Unit::Months => now.checked_sub_months(Months::new(amount)),
                    _ => unit.span(amount).and_then(|span| now.checked_sub_signed(span)),
                };
                then.with_context(|| format!("{amount}{} is too long ago", <&str>::from(unit)))?.with_timezone(&Utc)
            }
        };
        Ok(instant.into())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap()
    }

    fn resolve(value: &str, edge: Edge) -> DateTime<Utc> {
        value.parse::<When>().unwrap().resolve(edge, now()).unwrap().utc()
    }

    #[test]
    fn parses_dates_and_ages() {
        assert_eq!("2026-10-14".parse(), Ok(When::Date(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap())));
        assert_eq!("30m".parse(), Ok(When::Ago(30, Unit::Minutes)));
        assert_eq!("6h".parse(), Ok(When::Ago(6, Unit::Hours)));
        assert_eq!("2d".parse(), Ok(When::Ago(2, Unit::Days)));
        assert_eq!("1w".parse(), Ok(When::Ago(1, Unit::Weeks)));
        assert_eq!("3mo".parse(), Ok(When::Ago(3, Unit::Months)));
        assert_eq!("0d".parse(), Ok(When::Ago(0, Unit::Days)));
        assert_eq!(" 2d ".parse(), Ok(When::Ago(2, Unit::Days)));
    }

    #[test]
    fn rejects_other_input() {
        for bad in ["yesterday", "5y", "m", "2026-13-01", "-2d", "2D", "2 d", "", "99999999999d", "2Days", "2days", "3MO", "3mos"] {
            assert!(bad.parse::<When>().is_err(), "{bad}");
        }
        assert_eq!("5y".parse::<When>(), Err(format!("expected {FORMATS}")));
    }

    #[test]
    fn units_round_trip_through_their_suffixes() {
        let units = [(Unit::Minutes, "m"), (Unit::Hours, "h"), (Unit::Days, "d"), (Unit::Weeks, "w"), (Unit::Months, "mo")];
        for (unit, suffix) in units {
            assert_eq!(<&str>::from(unit), suffix);
            assert_eq!(suffix.parse::<Unit>(), Ok(unit));
        }
    }

    #[test]
    fn ages_count_back_from_now() {
        let now = now().with_timezone(&Utc);
        assert_eq!(now - resolve("30m", Edge::Start), Duration::minutes(30));
        assert_eq!(now - resolve("6h", Edge::Start), Duration::hours(6));
        assert_eq!(now - resolve("2d", Edge::Start), Duration::days(2));
        assert_eq!(now - resolve("1w", Edge::Start), Duration::weeks(1));
        assert_eq!(resolve("0d", Edge::Start), now);
        assert_eq!(resolve("1mo", Edge::Start).with_timezone(&Local).date_naive(), NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
    }

    #[test]
    fn ages_ignore_the_edge() {
        assert_eq!(resolve("2d", Edge::Start), resolve("2d", Edge::End));
    }

    #[test]
    fn dates_cover_the_whole_day() {
        let start = resolve("2026-10-14", Edge::Start).with_timezone(&Local);
        let end = resolve("2026-10-14", Edge::End).with_timezone(&Local);
        assert_eq!(start.date_naive(), end.date_naive());
        assert_eq!(end - start, Duration::milliseconds(86_399_999));
    }

    #[test]
    fn resolves_to_utc_with_milliseconds() {
        let resolved = When::Ago(0, Unit::Days).resolve(Edge::Start, now()).unwrap().to_string();
        assert!(resolved.ends_with('Z') && resolved.contains('.'), "{resolved}");
    }

    #[test]
    fn ages_too_far_back_are_an_error_not_a_panic() {
        assert!(When::Ago(u32::MAX, Unit::Minutes).resolve(Edge::Start, now()).is_ok(), "about 8000 years back is representable");
        for unit in [Unit::Hours, Unit::Days, Unit::Weeks, Unit::Months] {
            let error = When::Ago(u32::MAX, unit).resolve(Edge::Start, now()).unwrap_err();
            assert_eq!(error.to_string(), format!("{}{} is too long ago", u32::MAX, <&str>::from(unit)));
        }
    }
}
