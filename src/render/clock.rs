use chrono::{DateTime, FixedOffset, Local, NaiveDate};

use crate::timestamp::Timestamp;

const DATE_TIME: &str = "%Y-%m-%d %H:%M";
const TIME: &str = "%H:%M";

#[derive(Debug, Clone, Copy)]
pub struct Clock {
    zone: Zone,
    day: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy)]
enum Zone {
    Local,
    #[cfg(test)]
    Fixed(FixedOffset),
}

impl Clock {
    pub const LOCAL: Self = Self { zone: Zone::Local, day: None };

    #[cfg(test)]
    pub const fn fixed(offset: FixedOffset) -> Self {
        Self { zone: Zone::Fixed(offset), day: None }
    }

    #[must_use]
    pub fn starting(self, first: Timestamp) -> Self {
        Self { day: Some(self.at(first).date_naive()), ..self }
    }

    fn at(self, timestamp: Timestamp) -> DateTime<FixedOffset> {
        let t = timestamp.utc();
        match self.zone {
            Zone::Local => t.with_timezone(&Local).fixed_offset(),
            #[cfg(test)]
            Zone::Fixed(offset) => t.with_timezone(&offset),
        }
    }

    pub fn date_time(self, timestamp: Timestamp) -> String {
        self.at(timestamp).format(DATE_TIME).to_string()
    }

    pub fn time(self, timestamp: Timestamp) -> String {
        let t = self.at(timestamp);
        let format = if Some(t.date_naive()) == self.day { TIME } else { DATE_TIME };
        t.format(format).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::timestamp;

    fn clock(hours: i32, first: &str) -> Clock {
        Clock::fixed(FixedOffset::east_opt(hours * 3600).unwrap()).starting(timestamp(first))
    }

    #[test]
    fn times_on_the_first_day_drop_the_date() {
        let clock = clock(0, "2026-03-01T09:00:00.000Z");
        assert_eq!(clock.time(timestamp("2026-03-01T17:45:00.000Z")), "17:45");
        assert_eq!(clock.time(timestamp("2026-03-02T08:00:00.000Z")), "2026-03-02 08:00");
        assert_eq!(clock.time(timestamp("2026-02-28T08:00:00.000Z")), "2026-02-28 08:00");
    }

    #[test]
    fn date_time_always_shows_the_date() {
        assert_eq!(clock(0, "2026-03-01T09:00:00.000Z").date_time(timestamp("2026-03-01T09:00:00.000Z")), "2026-03-01 09:00");
    }

    #[test]
    fn offsets_shift_times_and_days() {
        assert_eq!(clock(2, "2026-03-01T09:00:00.000Z").date_time(timestamp("2026-03-01T09:00:00.000Z")), "2026-03-01 11:00");
        assert_eq!(clock(-5, "2026-03-01T09:00:00.000Z").date_time(timestamp("2026-03-01T03:00:00+01:00")), "2026-02-28 21:00");
    }

    #[test]
    fn the_day_ends_at_midnight_in_the_clocks_zone() {
        let (first, after_midnight) = ("2026-03-01T23:30:00.000Z", timestamp("2026-03-02T00:10:00.000Z"));
        assert_eq!(clock(0, first).time(after_midnight), "2026-03-02 00:10");
        assert_eq!(clock(-2, first).time(after_midnight), "22:10");
        assert_eq!(clock(2, first).time(after_midnight), "02:10");
        assert_eq!(clock(2, "2026-03-01T21:30:00.000Z").time(timestamp(first)), "2026-03-02 01:30");
    }

    #[test]
    fn without_a_first_day_every_time_shows_its_date() {
        assert_eq!(Clock::fixed(FixedOffset::east_opt(0).unwrap()).time(timestamp("2026-03-01T09:00:00.000Z")), "2026-03-01 09:00");
    }
}
