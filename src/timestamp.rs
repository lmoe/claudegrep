use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, ParseError, SecondsFormat, SubsecRound, Utc};
use rusqlite::ToSql;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};

/// A point in time, to the millisecond. It is written as RFC 3339 in UTC with milliseconds, `2026-01-02T10:00:00.000Z`,
/// the format of Claude Code's transcripts and of the index, where sorting the text sorts by time.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    pub const fn utc(self) -> DateTime<Utc> {
        self.0
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(instant: DateTime<Utc>) -> Self {
        Self(instant.trunc_subsecs(3))
    }
}

impl FromStr for Timestamp {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(value).map(|instant| instant.to_utc().into())
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::Millis, true))
    }
}

/// The text a transcript holds, quoted, so debug output reads like the string it replaced.
impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.to_string(), f)
    }
}

impl ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

impl FromSql for Timestamp {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value.as_str()?.parse().map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rusqlite::Connection;

    use super::*;

    fn at(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    #[test]
    fn writes_utc_with_milliseconds() {
        assert_eq!(at("2026-01-01T10:00:00.000Z").to_string(), "2026-01-01T10:00:00.000Z");
        assert_eq!(at("2026-01-01T10:00:00Z").to_string(), "2026-01-01T10:00:00.000Z");
        assert_eq!(at("2026-01-01T11:00:00.5+01:00").to_string(), "2026-01-01T10:00:00.500Z");
    }

    #[test]
    fn debug_output_is_the_quoted_text() {
        assert_eq!(format!("{:?}", at("2026-01-01T10:00:00Z")), "\"2026-01-01T10:00:00.000Z\"");
    }

    #[test]
    fn the_same_instant_is_equal_at_any_offset() {
        assert_eq!(at("2026-01-01T10:00:00Z"), at("2026-01-01T12:00:00+02:00"));
        assert_eq!(at("2026-01-01T10:00:00Z"), at("2026-01-01T05:00:00-05:00"));
    }

    #[test]
    fn orders_by_time_not_by_text() {
        let (earlier, later) = ("2026-01-01T10:00:00+02:00", "2026-01-01T09:00:00Z");
        assert!(earlier > later, "as text, the earlier time sorts last");
        assert!(at(earlier) < at(later));
    }

    #[test]
    fn precision_beyond_milliseconds_is_dropped() {
        assert_eq!(at("2026-01-01T10:00:00.123999Z"), at("2026-01-01T10:00:00.123Z"));
        assert_eq!(at("2026-01-01T10:00:00.123999Z").to_string(), "2026-01-01T10:00:00.123Z");
    }

    #[test]
    fn rejects_anything_but_rfc_3339() {
        for bad in ["", "t", "yesterday", "2026-01-01", "2026-13-01T00:00:00Z", "2026-01-01 10:00"] {
            assert!(bad.parse::<Timestamp>().is_err(), "{bad}");
        }
    }

    #[test]
    fn is_stored_as_its_text_and_reads_back() {
        let conn = Connection::open_in_memory().unwrap();
        let timestamp = at("2026-01-01T10:00:00.250Z");
        let (kind, text, read): (String, String, Timestamp) =
            conn.query_row("SELECT typeof(?1), ?1, ?1", [timestamp], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!((kind.as_str(), text.as_str(), read), ("text", "2026-01-01T10:00:00.250Z", timestamp));
    }

    #[test]
    fn a_stored_value_that_is_no_timestamp_is_an_error() {
        let conn = Connection::open_in_memory().unwrap();
        for bad in ["SELECT 'yesterday'", "SELECT 5", "SELECT NULL"] {
            assert!(conn.query_row(bad, [], |row| row.get::<_, Timestamp>(0)).is_err(), "{bad}");
        }
        assert_eq!(conn.query_row("SELECT NULL", [], |row| row.get::<_, Option<Timestamp>>(0)).unwrap(), None);
    }

    /// Any millisecond in the four-digit years, the range RFC 3339 can write.
    fn any_timestamp() -> impl Strategy<Value = Timestamp> {
        (-62_135_596_800_000..=253_402_300_799_999_i64).prop_map(|millis| DateTime::from_timestamp_millis(millis).unwrap().into())
    }

    proptest! {
        #[test]
        fn text_round_trips(timestamp in any_timestamp()) {
            prop_assert_eq!(at(&timestamp.to_string()), timestamp);
        }

        /// `ORDER BY` on the stored text relies on this.
        #[test]
        fn text_sorts_like_time(a in any_timestamp(), b in any_timestamp()) {
            prop_assert_eq!(a.to_string().cmp(&b.to_string()), a.cmp(&b));
        }
    }
}
