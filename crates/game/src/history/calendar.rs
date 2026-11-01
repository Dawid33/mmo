//! The world calendar: one game day is `DAY_MS` of play, 28 days a month, 12 months a year.

use serde::{Deserialize, Serialize};

/// Real milliseconds of play per game day.
pub const DAY_MS: u64 = 10_000;
/// Sim ticks per game day.
pub const TICKS_PER_DAY: u64 = DAY_MS / crate::TICK_RATE;
pub const DAYS_PER_MONTH: u64 = 28;
pub const MONTHS_PER_YEAR: u64 = 12;
pub const DAYS_PER_YEAR: u64 = DAYS_PER_MONTH * MONTHS_PER_YEAR;

/// Dwarf Fortress's month names.
pub const MONTH_NAMES: [&str; MONTHS_PER_YEAR as usize] = [
    "Granite", "Slate", "Felsite", "Hematite", "Malachite", "Galena", "Limestone", "Sandstone",
    "Timber", "Moonstone", "Opal", "Obsidian",
];

/// A point on the world calendar, in sim ticks since year 0.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
pub struct CalendarTick(pub u64);

/// A calendar date.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Date {
    pub year: u32,
    /// 1..=12
    pub month: u8,
    /// 1..=28
    pub day: u8,
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}", self.day, MONTH_NAMES[(self.month - 1) as usize], self.year)
    }
}

impl CalendarTick {
    pub const fn from_days(d: u64) -> Self {
        CalendarTick(d * TICKS_PER_DAY)
    }

    pub fn plus_days(self, d: u64) -> Self {
        CalendarTick(self.0 + d * TICKS_PER_DAY)
    }

    pub fn plus_ticks(self, t: u64) -> Self {
        CalendarTick(self.0 + t)
    }

    pub fn date(self) -> Date {
        let days = self.0 / TICKS_PER_DAY;
        let in_year = days % DAYS_PER_YEAR;
        Date {
            year: (days / DAYS_PER_YEAR) as u32,
            month: (in_year / DAYS_PER_MONTH) as u8 + 1,
            day: (in_year % DAYS_PER_MONTH) as u8 + 1,
        }
    }

    /// Whole years elapsed since `earlier` (0 if `earlier` is later).
    pub fn years_since(self, earlier: CalendarTick) -> u32 {
        (self.0.saturating_sub(earlier.0) / (TICKS_PER_DAY * DAYS_PER_YEAR)) as u32
    }
}
