//! The monthly climate family placed in the world: an integer frame (origin,
//! extent, wrap, lapse, sea level) and the runtime's own sampler of the
//! normals at a position and a month position (Phase 6).
//!
//! Configuration, never state: not hashed, not in a snapshot, not in the undo
//! log (a region holds it raw through [`ClimateConfig`]). The sampler is
//! INTEGER ONLY — the same bits on native and wasm32 by construction — and is
//! the runtime's definition of the normals; it agrees with `worldgen`'s `f64`
//! readers (`ForgedWorld::monthly_normals`) to within one stored step, not bit
//! for bit (Ruling 3).
//!
//! Units: positions are map metres in Q8 ([`MapPoint`]: x east from the west
//! edge, z south from the north edge); weights and angles are Q16; every
//! division whose dividend can be negative is `div_euclid`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::monthly_climate::{MonthlyClimate, CLIMATE_MONTHS};

/// The widest map the frame accepts (Q8 map metres, 2^38 = 1,073 km): every
/// `x · 360 · 65536` (and `z · 180 · 65536`) then fits `i64`.
pub const CLIMATE_MAX_EXTENT_Q8: i64 = 1 << 38;

/// Everything a pure weather read needs beside the tick: the monthly family and an integer frame.
/// Configuration, never state: not hashed, not in a snapshot (it is sent once, in its own packet).
#[derive(Clone, PartialEq)]
pub struct WorldClimate {
    pub frame: ClimateFrame,
    pub monthly: Arc<MonthlyClimate>,
}

/// What `GameData` holds: `None` = no climate. `Clone` shares the `Arc`;
/// `Debug` prints `monthly_crc`; `StateHash` writes NOTHING; `Serialize`
/// writes `()` and `Deserialize` gives `None` (never reached — the field is
/// `#[serde(skip)]` — but `Undo<T>` demands the bounds).
#[derive(Clone, Default)]
pub struct ClimateConfig(pub Option<Arc<WorldClimate>>);

/// The frame that places the family in the world, all integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClimateFrame {
    pub seed: u64,
    /// Map metres (Q8) of engine metre (0, 0): `round((globals.spawn − ENGINE_SPAWN_M) · 256)` (ROUNDED: the
    /// spawn is a forge cell centre today, so it is whole; a spawn that is not costs ≤ 1/512 m, never the climate).
    pub origin_q8: [i64; 2],
    /// The map's extent east–west and north–south, and the climate cell's edge (Q8 map metres).
    pub width_q8: i64,
    pub height_q8: i64,
    pub cell_q8: i64,
    pub wrap_x: bool,
    /// Cooling per metre of height, micro-°C: `round(ForgedWorld::lapse_per_m() · 1_000_000)` (°C per METRE in).
    pub lapse_uc_per_m: i64,
    /// Block Y of the sea's top water block (`pipeline::SEA_TOP_BLOCK`).
    pub sea_top_block: i32,
    /// `monthly.content_crc()`.
    pub monthly_crc: u32,
}

/// A position in map metres, Q8 (1/256 m): x east from the west edge, z south
/// from the north edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapPoint {
    pub x: i64,
    pub z: i64,
}

/// One place's month-interpolated normals at a date (Ruling 4): temperature
/// (at SEA LEVEL), diurnal range and wind piecewise linear between sharpened
/// mid-month control points; precipitation and wet-day share the month's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NormalsNow {
    pub temp_sea_centi_c: i32,
    pub diurnal_centi_c: i32,
    /// The month's precipitation, µm of water.
    pub precip_um_per_month: i64,
    /// Share of the month's days that are wet, Q16.
    pub wet_q16: i32,
    /// 1/64 m/s east, south: the planes' quarter m/s × 16.
    pub wind_q6: [i32; 2],
}

const Q16: i64 = 65536;
const MONTHS: i64 = CLIMATE_MONTHS as i64;

impl WorldClimate {
    /// Checks the family (`validate`) and that the frame matches it: the cell
    /// is a whole number of Q8 steps, the grid spans the map exactly, the wrap
    /// and the crc agree, and the map is at most [`CLIMATE_MAX_EXTENT_Q8`]
    /// each way.
    pub fn new(frame: ClimateFrame, monthly: Arc<MonthlyClimate>) -> Result<Self, String> {
        monthly.validate()?;
        let cell = monthly.cell_m * 256.0;
        if cell.fract() != 0.0 || cell < 1.0 || cell > CLIMATE_MAX_EXTENT_Q8 as f64 {
            return Err(format!("climate cell {} m is not a whole number of 1/256 m", monthly.cell_m));
        }
        if frame.cell_q8 != cell as i64 {
            return Err(format!("frame cell {} (Q8) but the family's cell is {} m", frame.cell_q8, monthly.cell_m));
        }
        for (what, extent) in [("width", frame.width_q8), ("height", frame.height_q8)] {
            if extent < 1 || extent > CLIMATE_MAX_EXTENT_Q8 {
                return Err(format!("map {what} {extent} (Q8) is outside 1 ..= 2^38"));
            }
        }
        if i64::from(monthly.nx) * frame.cell_q8 != frame.width_q8 {
            return Err(format!(
                "climate grid {} cells of {} (Q8) does not span the map's width {} (Q8)",
                monthly.nx, frame.cell_q8, frame.width_q8
            ));
        }
        if i64::from(monthly.nz) * frame.cell_q8 != frame.height_q8 {
            return Err(format!(
                "climate grid {} cells of {} (Q8) does not span the map's height {} (Q8)",
                monthly.nz, frame.cell_q8, frame.height_q8
            ));
        }
        if frame.wrap_x != monthly.wrap_x {
            return Err(format!("frame wrap_x {} but the family's is {}", frame.wrap_x, monthly.wrap_x));
        }
        let crc = monthly.content_crc();
        if frame.monthly_crc != crc {
            return Err(format!("frame names climate crc {:08x}, the family's is {crc:08x}", frame.monthly_crc));
        }
        Ok(Self { frame, monthly })
    }

    /// The centre of canonical world block `(bx, bz)` in map metres, x folded
    /// when the map wraps (`bx · 256` must fit `i64`).
    pub fn point_of_block(&self, world_block_x: i64, world_block_z: i64) -> MapPoint {
        let o = self.frame.origin_q8;
        self.fold(MapPoint { x: world_block_x * 256 + 128 + o[0], z: world_block_z * 256 + 128 + o[1] })
    }

    /// `p` with x folded into `[0, width)` when the map wraps; else `p`.
    pub fn fold(&self, p: MapPoint) -> MapPoint {
        if self.frame.wrap_x { MapPoint { x: p.x.rem_euclid(self.frame.width_q8), z: p.z } } else { p }
    }

    /// Longitude, Q16 degrees east of the west edge: `x · 360 · 65536 / width`
    /// (floored). An x on the map `[0, width]` is read as given (the east edge
    /// is 360°); any other is folded (wrapping map) or clamped.
    pub fn lon_q16(&self, p: MapPoint) -> i64 {
        let w = self.frame.width_q8;
        let x = if (0..=w).contains(&p.x) { p.x } else if self.frame.wrap_x { p.x.rem_euclid(w) } else { p.x.clamp(0, w) };
        (x * 360 * Q16).div_euclid(w)
    }

    /// Latitude, Q16 degrees north: `90 · 65536 − z · 180 · 65536 / height`
    /// (the quotient floored), z clamped to the map (±90° at the poles).
    pub fn lat_q16(&self, p: MapPoint) -> i64 {
        let h = self.frame.height_q8;
        90 * Q16 - (p.z.clamp(0, h) * 180 * Q16).div_euclid(h)
    }

    /// Metres above the sea of block row `block_y`, floored at 0:
    /// `block_y − sea_top_block`.
    pub fn height_m(&self, block_y: i32) -> i32 {
        block_y.saturating_sub(self.frame.sea_top_block).max(0)
    }

    /// The four cells around `p` and their Q16 weights (cell centres; x by
    /// `index`'s wrap, z clamped): `[(x0, z0), (x1, z0), (x0, z1), (x1, z1)]`,
    /// the indices already wrapped / clamped onto the grid; the weights sum to
    /// exactly 65536.
    pub fn cell_weights(&self, p: MapPoint) -> ([(i64, i64); 4], [i64; 4]) {
        let f = &self.frame;
        let (nx, nz) = (i64::from(self.monthly.nx), i64::from(self.monthly.nz));
        // On the map first: folded (wrap) or clamped x, clamped z — beyond an
        // edge both cells of a pair are the edge cell, so the clamp changes no value.
        let x = if f.wrap_x { p.x.rem_euclid(f.width_q8) } else { p.x.clamp(0, f.width_q8) };
        let z = p.z.clamp(0, f.height_q8);
        let (ix, tx) = axis(x, f.cell_q8);
        let (iz, tz) = axis(z, f.cell_q8);
        let col = |i: i64| if f.wrap_x { i.rem_euclid(nx) } else { i.clamp(0, nx - 1) };
        let row = |i: i64| i.clamp(0, nz - 1);
        let (x0, x1, z0, z1) = (col(ix), col(ix + 1), row(iz), row(iz + 1));
        let w11 = (tx * tz).div_euclid(Q16);
        let w10 = tx - w11;
        let w01 = tz - w11;
        let w00 = Q16 - tx - tz + w11;
        ([(x0, z0), (x1, z0), (x0, z1), (x1, z1)], [w00, w10, w01, w11])
    }

    /// The month-interpolated normals at `p` (Ruling 4); `month_q16` is
    /// `season::month_q16` (any value: read modulo the year).
    pub fn normals_now(&self, p: MapPoint, month_q16: i64) -> NormalsNow {
        let at = self.cell_weights(p);
        let mq = month_q16.rem_euclid(MONTHS * Q16);
        // The month whose mid-point is at or before `mq`, and the weight toward the next.
        let shifted = mq - Q16 / 2;
        let m0 = shifted.div_euclid(Q16);
        let wt = shifted.rem_euclid(Q16);
        // Temperature-like planes: the bilinear blend of months m0−1 … m0+2,
        // the two sharpened control points, linear between them.
        let curve = |v: fn(&MonthlyClimate, usize) -> i64| -> i64 {
            let n: [i64; 4] =
                std::array::from_fn(|k| self.blend(&at, (m0 + k as i64 - 1).rem_euclid(MONTHS), v).div_euclid(Q16));
            let (c0, c1) = (sharpened(n[0], n[1], n[2]), sharpened(n[1], n[2], n[3]));
            (c0 * (Q16 - wt) + c1 * wt).div_euclid(Q16)
        };
        let temp = curve(|c, i| i64::from(c.temp_sea_centi_c[i]));
        let diurnal = curve(|c, i| i64::from(c.diurnal_deci_c[i]) * 10).max(0);
        let east = curve(|c, i| i64::from(c.wind_east_q[i]) * 16);
        let south = curve(|c, i| i64::from(c.wind_south_q[i]) * 16);
        // Piecewise constant: the month `mq` lies in.
        let here = mq.div_euclid(Q16);
        let precip = (self.blend(&at, here, |c, i| i64::from(c.precip_deci_mm[i])) * 100).div_euclid(Q16);
        let wet = self.blend(&at, here, |c, i| i64::from(c.wet_day_255[i])).div_euclid(255);
        NormalsNow {
            temp_sea_centi_c: temp as i32,
            diurnal_centi_c: diurnal as i32,
            precip_um_per_month: precip,
            wet_q16: wet as i32,
            wind_q6: [east as i32, south as i32],
        }
    }

    /// The twelve bilinear normals of a place (season names, the summary
    /// tool): sea-level temperature in centi-°C and precipitation in deci-mm,
    /// month `m` = calendar month `m + 1`.
    pub fn normals_year(&self, p: MapPoint) -> ([i32; 12], [i32; 12]) {
        let at = self.cell_weights(p);
        let year = |v: fn(&MonthlyClimate, usize) -> i64| -> [i32; 12] {
            std::array::from_fn(|m| self.blend(&at, m as i64, v).div_euclid(Q16) as i32)
        };
        (year(|c, i| i64::from(c.temp_sea_centi_c[i])), year(|c, i| i64::from(c.precip_deci_mm[i])))
    }

    /// `Σ wᵢ · vᵢ` over the four cells of `at` (`cell_weights`) in month `m`
    /// (`0 ≤ m < 12`): Q16 of the plane's unit; the caller divides.
    fn blend(&self, at: &([(i64, i64); 4], [i64; 4]), m: i64, v: fn(&MonthlyClimate, usize) -> i64) -> i64 {
        let (cells, w) = at;
        let mut s = 0i64;
        for k in 0..4 {
            s += w[k] * v(&self.monthly, self.monthly.index(m as usize, cells[k].0, cells[k].1));
        }
        s
    }

    /// Heap and inline bytes of this value (the family's planes + the struct).
    pub fn resident_bytes(&self) -> usize {
        self.monthly.resident_bytes() + size_of::<Self>() + size_of::<MonthlyClimate>()
    }
}

/// One axis of the bilinear read: `v` (Q8, ≥ 0) → (the cell whose centre is
/// at or west / north of it, the Q16 weight toward the next cell). Centres at
/// `(i + ½) · cell`, computed at twice the scale so half a cell is exact.
fn axis(v: i64, cell_q8: i64) -> (i64, i64) {
    let u2 = 2 * v - cell_q8;
    let span = 2 * cell_q8;
    (u2.div_euclid(span), (u2.rem_euclid(span) * Q16).div_euclid(span))
}

/// Ruling 4's control point: `N[m] + (2·N[m] − N[m−1] − N[m+1]) / 8` (floored).
fn sharpened(prev: i64, n: i64, next: i64) -> i64 {
    n + (2 * n - prev - next).div_euclid(8)
}

impl std::fmt::Debug for WorldClimate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldClimate").field("frame", &self.frame).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ClimateConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(c) => write!(f, "ClimateConfig(crc {:08x})", c.frame.monthly_crc),
            None => f.write_str("ClimateConfig(None)"),
        }
    }
}

/// Configuration: contributes nothing to a state hash.
impl state_hash::StateHash for ClimateConfig {
    fn state_hash<H: std::hash::Hasher>(&self, _h: &mut H) {}
}

impl Serialize for ClimateConfig {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit()
    }
}

impl<'de> Deserialize<'de> for ClimateConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        <()>::deserialize(d)?;
        Ok(Self(None))
    }
}
