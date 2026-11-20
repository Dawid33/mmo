//! The sim date: an exact integer function of the world tick (Phase 6, Ruling 2).
//!
//! Weather and seasons follow the WORLD tick (`WorldClock`) and nothing else —
//! the one clock every region, host and predicting client agree on bit for
//! bit. The coordinator's calendar (`history::CalendarTick`) is a different
//! count and is not consulted here. Tick 0 is day 0 of year 0: the northern
//! spring equinox, 1st Granite.
//!
//! Integers only. Every length derives from `history::calendar`'s
//! `TICKS_PER_DAY` (= `DAY_MS / TICK_RATE`, the owner's to change), never
//! from a fixed tick count. Fixed point: `_q16` = 1/65536 of the unit named.

use crate::Tick;
use crate::history::calendar::{
    DAYS_PER_MONTH, DAYS_PER_YEAR, MONTH_NAMES, MONTHS_PER_YEAR, TICKS_PER_DAY,
};

const _: () = assert!(TICKS_PER_DAY >= 1);

/// Sim ticks per calendar year.
pub const TICKS_PER_YEAR: u64 = TICKS_PER_DAY * DAYS_PER_YEAR;

/// Ruling 5: local solar time follows the longitude (the sun rises in the
/// east); `false` = one solar time everywhere. Read by `solar_frac_q16`, the
/// one place both the weather's diurnal term and the client's sun ask.
pub const SOLAR_TIME_BY_LONGITUDE: bool = true;

/// Ruling 15: a place whose annual temperature range (warmest − coldest month
/// normal) is under this has no four seasons. *Untested* (the research's inference).
pub const SEASONLESS_RANGE_CENTI_C: i32 = 500;

/// Ruling 15: a seasonless place has wet and dry seasons when its wettest month
/// has more than this many times the driest month's rain. *Untested*.
pub const WET_DRY_RATIO: i64 = 3;

/// The world tick read as a date. Every field is 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimDate {
    pub year: u64,
    /// 0..12; index `m` is calendar month `m + 1`, `MONTH_NAMES[m]`.
    pub month: u8,
    /// 0..28
    pub day: u8,
    /// 0..336
    pub day_of_year: u16,
    /// Q16 of the day since the global midnight.
    pub day_frac_q16: u16,
    /// Q16 of the year since its start (the northern spring equinox).
    pub year_frac_q16: u16,
}

/// The date of world tick `tick`.
pub fn sim_date(tick: Tick) -> SimDate {
    let days = tick / TICKS_PER_DAY;
    let day_of_year = days % DAYS_PER_YEAR;
    SimDate {
        year: days / DAYS_PER_YEAR,
        month: (day_of_year / DAYS_PER_MONTH) as u8,
        day: (day_of_year % DAYS_PER_MONTH) as u8,
        day_of_year: day_of_year as u16,
        // `(t · 65536) / TICKS_PER_DAY & 0xFFFF` without the wide product: the
        // remainder's share of a day (< 65536).
        day_frac_q16: q16_share(tick % TICKS_PER_DAY, TICKS_PER_DAY),
        year_frac_q16: q16_share(tick % TICKS_PER_YEAR, TICKS_PER_YEAR),
    }
}

/// `floor(part · 65536 / whole)` for `part < whole` — always below 65536.
fn q16_share(part: u64, whole: u64) -> u16 {
    ((part as u128 * 65536) / whole as u128) as u16
}

/// Time in days, Q16 (`tq` of the vocabulary): `(t · 65536) / TICKS_PER_DAY`.
/// Exact for every tick below 2^47 (asserted in debug); above it the value
/// saturates at `i64::MAX` instead of wrapping (the product is taken in `u128`).
pub fn days_q16(tick: Tick) -> i64 {
    debug_assert!(tick < 1 << 47, "world tick {tick} past the date's range");
    let tq = (tick as u128 * 65536) / TICKS_PER_DAY as u128;
    tq.min(i64::MAX as u128) as i64
}

/// Months since the year's start, Q16 (`mq`): exact at every month start (the
/// first tick of month `m` reads `m · 65536`). The ONE implementation —
/// `normals_now`, the snow replay's months and a storm's birth month call it.
pub fn month_q16(tick: Tick) -> i64 {
    month_q16_of(days_q16(tick))
}

/// [`month_q16`] from time in days, Q16 (`tq`; may be negative).
pub fn month_q16_of(tq: i64) -> i64 {
    tq.rem_euclid(DAYS_PER_YEAR as i64 * 65536) / DAYS_PER_MONTH as i64
}

/// Local solar time, Q16 of a day (`sf`; 0 = local midnight, 0x8000 = local
/// noon): `(day_frac + lon / 360) mod 1`, `lon_q16` in Q16 degrees east (any
/// value: a full turn is one day). The day fraction itself when
/// [`SOLAR_TIME_BY_LONGITUDE`] is off.
pub fn solar_frac_q16(day_frac_q16: u16, lon_q16: i64) -> u16 {
    if !SOLAR_TIME_BY_LONGITUDE {
        return day_frac_q16;
    }
    (day_frac_q16 as i64 + lon_q16.div_euclid(360)).rem_euclid(65536) as u16
}

/// A place's season (Ruling 15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
    Wet,
    Dry,
}

impl Season {
    pub fn name(self) -> &'static str {
        match self {
            Season::Spring => "Spring",
            Season::Summer => "Summer",
            Season::Autumn => "Autumn",
            Season::Winter => "Winter",
            Season::Wet => "Wet season",
            Season::Dry => "Dry season",
        }
    }
}

/// Ruling 15. `temp_centi_c` / `precip_deci_mm`: the place's twelve normals (bilinear, sea level is fine).
///
/// A place whose temperature range is under [`SEASONLESS_RANGE_CENTI_C`] has
/// `Wet` / `Dry` seasons when its wettest month has over [`WET_DRY_RATIO`]
/// times the driest month's rain (wet while this month's normal is above the
/// year's mean), else none. Otherwise the four seasons by quarter of the year
/// from the spring equinox, shifted by two quarters south of the equator.
pub fn season_at(
    lat_q16: i64,
    temp_centi_c: &[i32; 12],
    precip_deci_mm: &[i32; 12],
    date: SimDate,
) -> Option<Season> {
    let month = date.month as usize;
    let warmest = temp_centi_c.iter().copied().max().unwrap_or(0) as i64;
    let coldest = temp_centi_c.iter().copied().min().unwrap_or(0) as i64;
    if warmest - coldest < SEASONLESS_RANGE_CENTI_C as i64 {
        let wettest = precip_deci_mm.iter().copied().max().unwrap_or(0) as i64;
        let driest = precip_deci_mm.iter().copied().min().unwrap_or(0) as i64;
        if wettest <= WET_DRY_RATIO * driest {
            return None;
        }
        let total: i64 = precip_deci_mm.iter().map(|&p| p as i64).sum();
        let wet = precip_deci_mm[month] as i64 * MONTHS_PER_YEAR as i64 > total;
        return Some(if wet { Season::Wet } else { Season::Dry });
    }
    const MONTHS_PER_QUARTER: u64 = MONTHS_PER_YEAR / 4;
    let shift = if lat_q16 < 0 { 2 } else { 0 };
    let quarter = (month as u64 / MONTHS_PER_QUARTER + shift) % 4;
    Some(
        [
            Season::Spring,
            Season::Summer,
            Season::Autumn,
            Season::Winter,
        ][quarter as usize],
    )
}

/// "12 Granite": day + 1 and the month's name, no year (the HUD's date).
impl std::fmt::Display for SimDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.day + 1, MONTH_NAMES[self.month as usize])
    }
}
