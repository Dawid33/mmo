//! A world's monthly climate normals: a coarse grid of twelve months ×
//! (sea-level temperature, diurnal range, precipitation, wet-day share,
//! wind) per climate cell, forged once and stored in the world file, read at
//! runtime with exact arithmetic.
//!
//! Plain data, never hashed, not `#[rollback]`: it is world data, not sim
//! state — weather is a pure function of it (Decision 9 of the climate
//! program). The integer planes are public and are the source of truth;
//! `index(month, cx, cz)` addresses them (the integer read). The `f32`
//! normals (`cell_normals`, `normals_at`) are the convenience read: each
//! stored `raw` decodes as `raw as f64 / DENOMINATOR` with the whole-number
//! denominators below, and `normals_at` interpolates bilinearly between
//! cell centres with add, subtract, multiply, divide and floor only, in a
//! fixed order — identical on native and wasm32.
//!
//! Axes are map metres: x east from the west edge, z south from the north
//! edge; a plane is MONTH-MAJOR, `i = m · nx · nz + cz · nx + cx`. x wraps BY
//! INDEX when `wrap_x` (z is always clamped), so a point and its image one
//! world-width away read bit-identically only when the caller folds x first
//! (`worldgen::forged::ForgedWorld` does). Month interpolation is the
//! caller's: every read returns all twelve months of a place, month `m`
//! being calendar month `m + 1` (`history::calendar`).

use serde::{Deserialize, Serialize};

/// Months per climate year: the calendar's `MONTHS_PER_YEAR`.
pub const CLIMATE_MONTHS: usize = 12;
const _: () = assert!(CLIMATE_MONTHS == crate::history::calendar::MONTHS_PER_YEAR as usize);
/// The northern summer solstice, in months from the start of the year (the
/// year starts at the northern spring equinox): the boundary of months 2 | 3.
/// The northern winter solstice is six months later.
pub const SOLSTICE_NORTH_SUMMER_MONTHS: f64 = 3.0;
/// The month after the northern summer solstice ("July").
pub const NORTH_MIDSUMMER_MONTH: usize = 3;
/// The month after the northern winter solstice ("January").
pub const NORTH_MIDWINTER_MONTH: usize = 9;

/// Stored steps per unit: a raw value decodes as `raw as f64 / DENOMINATOR`.
/// Sea-level temperature, steps per °C (`i16`).
pub const CLIMATE_TEMP_PER_C: f64 = 100.0;
/// Diurnal range (mean daily max − min), steps per °C (`u8`).
pub const CLIMATE_DIURNAL_PER_C: f64 = 10.0;
/// Precipitation per month, steps per mm (`u16`).
pub const CLIMATE_PRECIP_PER_MM: f64 = 10.0;
/// Share of the month's days that are wet, steps per 1 (`u8`).
pub const CLIMATE_WET_DAY_PER_ONE: f64 = 255.0;
/// Wind component, steps per m/s (`i8`; toward +x east, toward +z south).
pub const CLIMATE_WIND_PER_MS: f64 = 4.0;
/// Stored bytes per climate cell.
pub const CLIMATE_BYTES_PER_CELL: usize = 96;
const _: () = assert!(CLIMATE_BYTES_PER_CELL == CLIMATE_MONTHS * (2 + 1 + 2 + 1 + 1 + 1));

/// The monthly normals of a world on its climate grid: `nx × nz` cells of
/// `cell_m` map metres, one value per cell centre and month in each plane
/// (month-major, row-major). Every bit pattern of a plane is a legal value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonthlyClimate {
    pub nx: u32,
    pub nz: u32,
    pub cell_m: f64,
    pub wrap_x: bool,
    pub temp_sea_centi_c: Vec<i16>,
    pub diurnal_deci_c: Vec<u8>,
    pub precip_deci_mm: Vec<u16>,
    pub wet_day_255: Vec<u8>,
    pub wind_east_q: Vec<i8>,
    pub wind_south_q: Vec<i8>,
}

/// Twelve monthly values of one place. `temp_c`'s datum is the producer's
/// (sea level from `normals_at` / `cell_normals`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonthlyNormals {
    pub temp_c: [f32; CLIMATE_MONTHS],
    pub diurnal_c: [f32; CLIMATE_MONTHS],
    pub precip_mm: [f32; CLIMATE_MONTHS],
    pub wet_day: [f32; CLIMATE_MONTHS],
    /// `[east, south]` m/s.
    pub wind_ms: [[f32; 2]; CLIMATE_MONTHS],
}

impl MonthlyClimate {
    /// Climate cells per month: `nx · nz`.
    pub fn cells(&self) -> usize {
        self.nx as usize * self.nz as usize
    }

    /// The plane index of `month` at cell `(cx, cz)`: x by `rem_euclid(nx)`
    /// when `wrap_x`, else clamped to the grid; z always clamped.
    pub fn index(&self, month: usize, cx: i64, cz: i64) -> usize {
        let nx = i64::from(self.nx);
        let x = if self.wrap_x { cx.rem_euclid(nx) } else { cx.clamp(0, nx - 1) };
        let z = cz.clamp(0, i64::from(self.nz) - 1);
        month * self.cells() + z as usize * self.nx as usize + x as usize
    }

    /// Checks the grid and that every plane holds `12 · nx · nz` values; the
    /// error names what is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if self.nx < 1 || self.nz < 1 {
            return Err(format!("monthly climate grid {} × {} is empty", self.nx, self.nz));
        }
        if !self.cell_m.is_finite() || self.cell_m <= 0.0 {
            return Err(format!("monthly climate cell {} m is not a positive length", self.cell_m));
        }
        let len = CLIMATE_MONTHS
            .checked_mul(self.nx as usize)
            .and_then(|n| n.checked_mul(self.nz as usize))
            .ok_or_else(|| format!("monthly climate grid {} × {} × 12 overflows usize", self.nx, self.nz))?;
        let planes = [
            ("temp_sea_centi_c", self.temp_sea_centi_c.len()),
            ("diurnal_deci_c", self.diurnal_deci_c.len()),
            ("precip_deci_mm", self.precip_deci_mm.len()),
            ("wet_day_255", self.wet_day_255.len()),
            ("wind_east_q", self.wind_east_q.len()),
            ("wind_south_q", self.wind_south_q.len()),
        ];
        for (name, got) in planes {
            if got != len {
                return Err(format!("monthly climate plane {name} holds {got} values, the grid needs {len}"));
            }
        }
        Ok(())
    }

    /// The normals of one climate cell (through `index`), no interpolation.
    pub fn cell_normals(&self, cx: i64, cz: i64) -> MonthlyNormals {
        self.read(|m, plane| plane(self.index(m, cx, cz)))
    }

    /// The normals at map metres `(ax_m, az_m)`, bilinear between cell
    /// centres (`ForgedWorld::bilinear_cells`'s arithmetic on the climate
    /// grid), each value cast to `f32` last. At a cell centre it is that
    /// cell's `cell_normals` bit for bit.
    pub fn normals_at(&self, ax_m: f64, az_m: f64) -> MonthlyNormals {
        let (u, v) = (ax_m / self.cell_m - 0.5, az_m / self.cell_m - 0.5);
        let (iu, iv) = (u.floor(), v.floor());
        let (tu, tv) = (u - iu, v - iv);
        let (iu, iv) = (iu as i64, iv as i64);
        self.read(|m, plane| {
            let at = |ix: i64, iz: i64| plane(self.index(m, ix, iz));
            let row = |iz: i64| at(iu, iz) + tu * (at(iu + 1, iz) - at(iu, iz));
            row(iv) + tv * (row(iv + 1) - row(iv))
        })
    }

    /// The planes' heap bytes (their lengths × element sizes).
    pub fn resident_bytes(&self) -> usize {
        self.temp_sea_centi_c.len() * size_of::<i16>()
            + self.diurnal_deci_c.len()
            + self.precip_deci_mm.len() * size_of::<u16>()
            + self.wet_day_255.len()
            + self.wind_east_q.len()
            + self.wind_south_q.len()
    }

    /// Builds the normals from `sample(month, decode)`, where `decode(i)` is
    /// one plane's decoded value at plane index `i`; the plane's `f64`
    /// result is cast to `f32`.
    fn read(&self, sample: impl Fn(usize, &dyn Fn(usize) -> f64) -> f64) -> MonthlyNormals {
        let temp = |i: usize| f64::from(self.temp_sea_centi_c[i]) / CLIMATE_TEMP_PER_C;
        let diurnal = |i: usize| f64::from(self.diurnal_deci_c[i]) / CLIMATE_DIURNAL_PER_C;
        let precip = |i: usize| f64::from(self.precip_deci_mm[i]) / CLIMATE_PRECIP_PER_MM;
        let wet = |i: usize| f64::from(self.wet_day_255[i]) / CLIMATE_WET_DAY_PER_ONE;
        let east = |i: usize| f64::from(self.wind_east_q[i]) / CLIMATE_WIND_PER_MS;
        let south = |i: usize| f64::from(self.wind_south_q[i]) / CLIMATE_WIND_PER_MS;
        let mut n = MonthlyNormals {
            temp_c: [0.0; CLIMATE_MONTHS],
            diurnal_c: [0.0; CLIMATE_MONTHS],
            precip_mm: [0.0; CLIMATE_MONTHS],
            wet_day: [0.0; CLIMATE_MONTHS],
            wind_ms: [[0.0; 2]; CLIMATE_MONTHS],
        };
        for m in 0..CLIMATE_MONTHS {
            n.temp_c[m] = sample(m, &temp) as f32;
            n.diurnal_c[m] = sample(m, &diurnal) as f32;
            n.precip_mm[m] = sample(m, &precip) as f32;
            n.wet_day[m] = sample(m, &wet) as f32;
            n.wind_ms[m] = [sample(m, &east) as f32, sample(m, &south) as f32];
        }
        n
    }
}

/// The first index of the largest (`greater`) or smallest value.
fn extreme_month(v: &[f32; CLIMATE_MONTHS], greater: bool) -> usize {
    let mut best = 0;
    for m in 1..CLIMATE_MONTHS {
        if if greater { v[m] > v[best] } else { v[m] < v[best] } {
            best = m;
        }
    }
    best
}

impl MonthlyNormals {
    /// The first month of the highest temperature.
    pub fn warmest_month(&self) -> usize {
        extreme_month(&self.temp_c, true)
    }

    /// The first month of the lowest temperature.
    pub fn coldest_month(&self) -> usize {
        extreme_month(&self.temp_c, false)
    }

    /// The first month of the most precipitation.
    pub fn wettest_month(&self) -> usize {
        extreme_month(&self.precip_mm, true)
    }

    /// The first month of the least precipitation.
    pub fn driest_month(&self) -> usize {
        extreme_month(&self.precip_mm, false)
    }

    /// The mean of the twelve temperatures: months 0..12 added in order,
    /// then divided by 12.
    pub fn annual_mean_temp_c(&self) -> f32 {
        let mut sum = 0.0f32;
        for t in self.temp_c {
            sum += t;
        }
        sum / CLIMATE_MONTHS as f32
    }

    /// The year's precipitation: months 0..12 added in order.
    pub fn annual_precip_mm(&self) -> f32 {
        let mut sum = 0.0f32;
        for p in self.precip_mm {
            sum += p;
        }
        sum
    }

    /// Every temperature lowered by `delta_c` (a lapse); nothing else changes.
    pub fn lowered_by(mut self, delta_c: f32) -> MonthlyNormals {
        for t in &mut self.temp_c {
            *t -= delta_c;
        }
        self
    }
}
