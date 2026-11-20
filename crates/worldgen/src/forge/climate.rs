//! The world's climate on the CURRENT heightfield: latitude per row,
//! temperature from latitude and height, and precipitation in mm/yr.
//!
//! - **Latitude** runs along z (rows): 90° N at the north edge (z = 0), the
//!   equator in the middle row, 90° S at the south edge. Southern latitudes
//!   are negative; everything below uses the absolute value.
//! - **Meander**: everything that depends on latitude below (temperature, the
//!   rain table, the wind bands) reads an EFFECTIVE latitude per cell — the
//!   row's latitude plus a slow, seed-derived displacement
//!   (`meander_field`: a few long waves running mostly along x, like
//!   planetary waves on the jet stream), so isotherms and band edges wander
//!   instead of running ruler-straight. Set in degrees of latitude
//!   (`meander_amplitude_deg`, `meander_wavelength_deg`), so it is the same
//!   field at every cell size, coarse-to-fine level and world size; 0 =
//!   straight rows.
//! - **Temperature** at sea level follows the cosine of latitude between
//!   `pole_temp_c` and `equator_temp_c`, minus `lapse_c_per_km` per km of
//!   ground above the sea.
//! - **Winds** blow along rows: from the east below `trade_wind_edge_deg`
//!   (trade winds) and above `polar_edge_deg` (polar easterlies), from the
//!   west between (westerlies); within `BAND_BLEND_DEG` of a band edge a row
//!   blends both directions, so no line marks the edge. Each row's moisture
//!   sweep runs downwind. Over flat land the air's moisture relaxes toward
//!   the continental equilibrium `CONTINENTAL_MOISTURE` over
//!   `land_depletion_deg` (rain-out balanced by evaporation from the land);
//!   forced up a slope it rains out (`orographic_scale_m` of rise wrings out
//!   about all of it) and arrives dry on the lee side; it recharges toward
//!   saturation over any sea (inland seas included) and any lake. The wind
//!   blows east or west, and the Planet wraps east–west, so no air enters
//!   across an edge: each row's sweep goes once round from saturation, then
//!   records the lap after it.
//! - **Precipitation** (mm/yr) is the latitude's base (`LATITUDE_PRECIP_MM`:
//!   wet equator, dry ~25° belts, wet ~50°, dry poles) times the air's
//!   moisture relative to the equilibrium, raised on windward slopes
//!   (`OROGRAPHIC_RUN_M`), clamped to `rain_min`..`rain_max` of the base:
//!   flat land far inland gets exactly the table, coasts downwind of the sea
//!   more, lee sides less. Every smoothing length is a length (metres or
//!   degrees), never cells, so the field does not depend on the cell size.
//! - **Planetary lengths are in degrees of latitude**: every world size is a
//!   whole planet, so the lengths the air travels over seas and continents
//!   (`land_depletion_deg`, `sea_recharge_deg`), the air's sideways and
//!   along-wind mixing (`RAIN_CROSS_BLUR_DEG`, `RAIN_ALONG_BLUR_DEG`) and the
//!   meander's wavelength (`meander_wavelength_deg`) scale with the world
//!   (`metres_per_degree`): a small planet is the same planet as a large one.
//!   The lengths that describe hills (`OROGRAPHIC_RUN_M`, `TERRAIN_BLUR_M`,
//!   `orographic_scale_m`) stay in metres.
//!
//! Precipitation weights discharge (`discharge_weight`), so wet flanks grow
//! bigger rivers and erode faster — Dwarf Fortress computes rain shadows AFTER
//! its rivers, which is why a DF desert river is as wide as a jungle one.

use crate::forge::erosion;
use crate::forge::hydrology::LAKE_MIN_DEPTH_M;
use crate::forge::par;
use crate::forge::periodic::PeriodicX;
use crate::forge::world::{hash01, smoothstep, EdgeKind, ForgeParams, World};

/// Annual precipitation (mm/yr) by absolute latitude (deg), interpolated
/// linearly: the climatological base before rain shadows. The 25° entry is
/// the subtropical desert belt (real interiors there get 25–200 mm), so flat
/// interiors under the subtropical high are desert, not savanna.
pub const LATITUDE_PRECIP_MM: [(f32, f32); 7] =
    [(0.0, 2500.0), (10.0, 1800.0), (25.0, 180.0), (40.0, 900.0), (55.0, 1000.0), (70.0, 400.0), (90.0, 200.0)];

/// Moisture (1 = saturated sea air) that air settles at over endless flat
/// land: what it rains out is balanced by what the land gives back. Flat land
/// under air at this moisture gets the latitude table's precipitation.
pub const CONTINENTAL_MOISTURE: f64 = 0.65;
/// Half-width (deg) of the latitude band over which the wind turns at a band
/// edge (`trade_wind_edge_deg`, `polar_edge_deg`): rows inside it blend the
/// two directions' sweeps. The two edge knobs' help text in `knobs.rs`
/// quotes this value: keep them in step.
pub const BAND_BLEND_DEG: f64 = 5.0;
/// The meander's wavelength is at most the world's size over this, so a
/// small world still sees whole waves instead of one big kink.
pub const MEANDER_MIN_WAVES: f64 = 2.0;
/// The meander's component waves: (wavelength as a share of
/// `meander_wavelength_deg`, amplitude as a share of `meander_amplitude_deg`,
/// largest angle (rad) of the wave's travel off the x axis). The main wave
/// runs along x (a meander of the westerly jet) and shifts every band
/// together; the shorter ones turn up to ~80° off it, so neighbouring
/// latitudes are displaced differently and bands widen, narrow and break.
const MEANDER_WAVES: [(f64, f64, f64); 3] = [(1.0, 1.0, 0.3), (0.5, 0.6, 1.0), (0.27, 0.35, 1.4)];
/// Seed salt of the meander's phases and directions.
const MEANDER_SALT: u64 = 0x3EA7_D0E5;

/// Windward slopes rain more: precipitation is raised by
/// `slope * OROGRAPHIC_RUN_M / orographic_scale_m` (a 1:10 upslope with the
/// default 650 m scale nearly doubles it).
pub const OROGRAPHIC_RUN_M: f64 = 6_000.0;

// Sea recharge, land relaxation distance, orographic scale and the rain
// bounds are knobs (knobs.rs, "Climate").
/// Heights are blurred over this radius (m) before taking windward slopes,
/// so rain follows ranges, not individual gullies.
const TERRAIN_BLUR_M: f64 = 400.0;
/// Cross-wind blur radius of the rain field (deg of latitude; the box is
/// twice as wide): air mixes sideways. 1,500 m on a Large world (557.5 m per
/// degree).
const RAIN_CROSS_BLUR_DEG: f64 = 2.69;
/// Along-wind blur radius of the rain field (deg of latitude): 500 m on a
/// Large world.
const RAIN_ALONG_BLUR_DEG: f64 = 0.897;

/// The climate fields of one grid, row-major like `World::h`.
#[derive(Clone, Debug, PartialEq)]
pub struct Climate {
    /// Mean annual temperature at the ground (sea level over water), deg C.
    pub temperature_c: Vec<f32>,
    /// Annual precipitation, mm/yr.
    pub precip_mm: Vec<f32>,
    /// Latitude of each ROW (`nz` values, deg; south negative).
    pub latitude_deg: Vec<f32>,
}

/// Which way a row's wind blows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wind {
    /// From the west (x = 0) toward +x: the westerlies.
    FromWest,
    /// From the east (x = max) toward -x: trade winds and polar easterlies.
    FromEast,
}

/// Latitude (deg) of each of `nz` rows: 90° at row 0's edge falling
/// linearly to −90° at the last row's (each half written from its own pole,
/// as the forge has always computed it).
pub fn latitudes(nz: usize) -> Vec<f32> {
    /// Degrees from a pole to the equator.
    const SPAN: f64 = 90.0;
    (0..nz)
        .map(|z| {
            let t = (z as f64 + 0.5) / nz as f64; // 0 at the north edge, 1 at the south
            let lat = if t < 0.5 { 90.0 - SPAN * 2.0 * t } else { -90.0 + SPAN * 2.0 * (1.0 - t) };
            lat.clamp(-90.0, 90.0) as f32
        })
        .collect()
}

/// Latitude displacement (deg, northward positive) of the meander at artifact
/// metres `(x_m, z_m)`: a function of position, seed and knobs only. Each
/// wave's x-frequency is rounded to a whole number of cycles round the width
/// (at least 1), so the meander meets itself at the seam.
pub fn meander_deg(x_m: f64, z_m: f64, p: &ForgeParams) -> f64 {
    let k = &p.knobs;
    if k.meander_amplitude_deg <= 0.0 {
        return 0.0;
    }
    let wavelength =
        (k.meander_wavelength_deg * metres_per_degree(p.height_m)).min(p.span_m() / MEANDER_MIN_WAVES).max(1.0);
    let wrap = PeriodicX::for_world(p);
    let mut d = 0.0;
    for (wave, &(share, weight, spread)) in MEANDER_WAVES.iter().enumerate() {
        let angle = (hash01(p.seed, MEANDER_SALT, 2 * wave as u64) - 0.5) * 2.0 * spread;
        let phase = hash01(p.seed, MEANDER_SALT, 2 * wave as u64 + 1) * std::f64::consts::TAU;
        // |angle| < π/2, so the wave always travels east (cos > 0).
        let cycles = (wrap.period_m * angle.cos() / (wavelength * share)).round().max(1.0);
        let turns = cycles * x_m / wrap.period_m + z_m * angle.sin() / (wavelength * share);
        d += weight * (std::f64::consts::TAU * turns + phase).sin();
    }
    k.meander_amplitude_deg * d
}

/// `meander_deg` at every cell centre of an `nx` x `nz` grid of `dx` cells.
pub fn meander_field(nx: usize, nz: usize, dx: f64, p: &ForgeParams) -> Vec<f32> {
    (0..nx * nz)
        .map(|i| meander_deg(((i % nx) as f64 + 0.5) * dx, ((i / nx) as f64 + 0.5) * dx, p) as f32)
        .collect()
}

/// Effective latitude of every cell: its row's latitude plus the meander.
pub fn effective_latitudes(nx: usize, nz: usize, dx: f64, p: &ForgeParams) -> Vec<f32> {
    let rows = latitudes(nz);
    let meander = meander_field(nx, nz, dx, p);
    (0..nx * nz).map(|i| (rows[i / nx] + meander[i]).clamp(-90.0, 90.0)).collect()
}

/// Metres of the world per degree of latitude: the pole-to-pole height
/// `height_m` spans 180°.
pub fn metres_per_degree(height_m: f64) -> f64 {
    height_m / 180.0
}

/// Sea-level temperature (deg C) at latitude `lat_deg`.
pub fn sea_level_temperature_c(lat_deg: f64, p: &ForgeParams) -> f64 {
    let k = &p.knobs;
    k.pole_temp_c + (k.equator_temp_c - k.pole_temp_c) * lat_deg.abs().min(90.0).to_radians().cos()
}

/// Share of a row's rain swept by the westerlies at latitude `lat_deg`: 1
/// inside the westerly band, 0 in the trades and polar easterlies, a
/// smoothstep over `BAND_BLEND_DEG` either side of each band edge.
pub fn westerly_weight(lat_deg: f64, p: &ForgeParams) -> f64 {
    let (k, lat) = (&p.knobs, lat_deg.abs());
    let b = BAND_BLEND_DEG;
    smoothstep(k.trade_wind_edge_deg - b, k.trade_wind_edge_deg + b, lat)
        * (1.0 - smoothstep(k.polar_edge_deg - b, k.polar_edge_deg + b, lat))
}

/// The prevailing wind at latitude `lat_deg` (the larger share).
pub fn wind(lat_deg: f64, p: &ForgeParams) -> Wind {
    if westerly_weight(lat_deg, p) >= 0.5 { Wind::FromWest } else { Wind::FromEast }
}

/// `LATITUDE_PRECIP_MM` interpolated at `lat_deg` (absolute value taken).
pub fn base_precip_mm(lat_deg: f32) -> f32 {
    let lat = lat_deg.abs().min(90.0);
    for pair in LATITUDE_PRECIP_MM.windows(2) {
        let ((a, pa), (b, pb)) = (pair[0], pair[1]);
        if lat <= b {
            return pa + (pb - pa) * (lat - a) / (b - a);
        }
    }
    LATITUDE_PRECIP_MM[LATITUDE_PRECIP_MM.len() - 1].1
}

/// Box blur of an `nx` x `nz` raster along x (each row) or along z (each
/// column). `wrap_x`: a row is a circle (column `nx - 1` beside 0), so the
/// blur along x runs round the seam with the full `2 * radius + 1` box.
fn box_blur_rows(src: &[f64], nx: usize, nz: usize, radius: usize, along_x: bool, wrap_x: bool) -> Vec<f64> {
    let mut out = vec![0.0; nx * nz];
    if along_x && wrap_x {
        // Columns b - radius ..= b + radius, mod nx (a box wider than the row
        // counts some columns twice, every one the same number of times).
        let back = nx * (radius / nx + 1); // >= radius + 1: keeps indices unsigned
        let count = (2 * radius + 1) as f64;
        for a in 0..nz {
            let row = &src[a * nx..(a + 1) * nx];
            let at = |b: usize| row[b % nx];
            let mut sum: f64 = (0..=2 * radius).map(|o| at(back + o - radius)).sum();
            for b in 0..nx {
                out[a * nx + b] = sum / count;
                sum += at(b + radius + 1);
                sum -= at(back + b - radius);
            }
        }
        return out;
    }
    // `lines` rows (or columns) of `len` cells each.
    let (lines, len) = if along_x { (nz, nx) } else { (nx, nz) };
    for a in 0..lines {
        let at = |b: usize| if along_x { src[a * nx + b] } else { src[b * nx + a] };
        let mut sum: f64 = (0..=radius.min(len - 1)).map(at).sum();
        let mut count = radius.min(len - 1) + 1;
        for b in 0..len {
            let v = sum / count as f64;
            if along_x { out[a * nx + b] = v } else { out[b * nx + a] = v }
            if b + radius + 1 < len {
                sum += at(b + radius + 1);
                count += 1;
            }
            if b >= radius {
                sum -= at(b - radius);
                count -= 1;
            }
        }
    }
    out
}

/// Lakes as the climate sees them: land cells at least `LAKE_MIN_DEPTH_M`
/// under the water surface `filled` (a flooded routing surface).
pub fn lakes_from_filled(w: &World, filled: &[f64]) -> Vec<bool> {
    (0..w.cells()).map(|i| !w.ocean[i] && filled[i] - w.h[i] >= LAKE_MIN_DEPTH_M).collect()
}

/// The climate of `w`, its lakes found with a Priority-Flood of the surface.
pub fn compute(w: &World, p: &ForgeParams) -> Climate {
    let filled = erosion::route(w, 0.0).filled;
    compute_with_lakes(w, p, &lakes_from_filled(w, &filled))
}

/// The climate of `w` with the given lake cells (`lakes_from_filled`). Rows
/// are swept in parallel and each row's sweep is serial, so the result does
/// not depend on the thread count.
pub fn compute_with_lakes(w: &World, p: &ForgeParams, lakes: &[bool]) -> Climate {
    let (cells, k) = (w.cells(), &p.knobs);
    let latitude_deg = latitudes(w.nz);
    let lat = effective_latitudes(w.nx, w.nz, w.dx, p);
    let lapse_c_per_m = k.lapse_c_per_km / 1000.0;
    let temperature_c = (0..cells)
        .map(|i| {
            let ground = if w.ocean[i] { 0.0 } else { w.h[i].max(0.0) };
            (sea_level_temperature_c(lat[i] as f64, p) - lapse_c_per_m * ground) as f32
        })
        .collect();

    let relative = if k.climate_strength <= 0.0 { vec![1.0; cells] } else { orographic(w, p, &lat, lakes) };
    let precip_mm = (0..cells).map(|i| base_precip_mm(lat[i]) * relative[i] as f32).collect();
    Climate { temperature_c, precip_mm, latitude_deg }
}

/// Blur radius (cells) of `metres` on a `dx` grid: the nearest whole number
/// of cells, but none (no blur) under one cell — a radius of a fraction of a
/// cell must not become a whole cell's box.
fn radius_cells(metres: f64, dx: f64) -> usize {
    let cells = metres / dx;
    if cells < 1.0 { 0 } else { cells.round() as usize }
}

/// Precipitation per cell relative to the latitude base, clamped and scaled
/// by the climate strength (1.0 at sea). `lat` is the effective latitude per
/// cell; it sets each cell's mix of the westerly and easterly sweeps.
fn orographic(w: &World, p: &ForgeParams, lat: &[f32], lakes: &[bool]) -> Vec<f64> {
    let (nx, nz, k, dx) = (w.nx, w.nz, &p.knobs, w.dx);
    let ground: Vec<f64> = w.h.iter().map(|h| h.max(0.0)).collect();
    let terrain = radius_cells(TERRAIN_BLUR_M, dx);
    let wrap_x = w.wrap_x;
    let ground =
        box_blur_rows(&box_blur_rows(&ground, nx, nz, terrain, true, wrap_x), nx, nz, terrain, false, wrap_x);
    // `World::edges` order: N, E, S, W.
    let inflow = |edge: usize| if w.edges[edge] == EdgeKind::Sea { 1.0 } else { CONTINENTAL_MOISTURE };
    let per_degree = metres_per_degree(p.height_m);
    let recharge = (dx / (k.sea_recharge_deg * per_degree)).min(1.0);
    let relax = (dx / (k.land_depletion_deg * per_degree)).min(1.0);
    let lift = OROGRAPHIC_RUN_M / k.orographic_scale_m / dx;

    // One row's precipitation relative to the base, wind from the west or
    // east. A wrapping world has no edge to blow in across: the air goes
    // once round the row from saturation (a spin-up lap, not recorded), then
    // the recorded lap carries on from where it ended.
    let sweep = |z: usize, from_west: bool, row: &mut [f64]| {
        let mut moisture: f64 = if wrap_x {
            1.0
        } else if from_west {
            inflow(3)
        } else {
            inflow(1)
        };
        let mut prev: Option<usize> = None;
        let laps = if wrap_x { 2 } else { 1 };
        for step in 0..laps * nx {
            let step = step % nx;
            let x = if from_west { step } else { nx - 1 - step };
            let i = z * nx + x;
            if w.ocean[i] {
                moisture += (1.0 - moisture) * recharge;
            } else {
                // Upslope from the upwind cell; a lake is flat water.
                let rise = match prev {
                    Some(j) if !lakes[i] => (ground[i] - ground[j]).max(0.0),
                    _ => 0.0,
                };
                row[x] = moisture * (1.0 + rise * lift) / CONTINENTAL_MOISTURE;
                moisture -= moisture * (rise / k.orographic_scale_m).min(1.0);
                moisture += (CONTINENTAL_MOISTURE - moisture) * relax;
                if lakes[i] {
                    moisture += (1.0 - moisture) * recharge;
                }
            }
            prev = Some(i);
        }
    };
    let rows: Vec<Vec<f64>> = par::map_range(nz, |z| {
        let west: Vec<f64> = (0..nx).map(|x| westerly_weight(lat[z * nx + x] as f64, p)).collect();
        let (mut from_west, mut from_east) = (vec![0.0; nx], vec![0.0; nx]);
        if west.iter().any(|&v| v > 0.0) {
            sweep(z, true, &mut from_west);
        }
        if west.iter().any(|&v| v < 1.0) {
            sweep(z, false, &mut from_east);
        }
        (0..nx).map(|x| west[x] * from_west[x] + (1.0 - west[x]) * from_east[x]).collect()
    });
    // Blur over the land only (the sea's zero would dry the coasts).
    let rain: Vec<f64> = rows.concat();
    let land: Vec<f64> = w.ocean.iter().map(|&o| if o { 0.0 } else { 1.0 }).collect();
    let (cross, along) =
        (radius_cells(RAIN_CROSS_BLUR_DEG * per_degree, dx), radius_cells(RAIN_ALONG_BLUR_DEG * per_degree, dx));
    let blur =
        |v: &[f64]| box_blur_rows(&box_blur_rows(v, nx, nz, cross, false, wrap_x), nx, nz, along, true, wrap_x);
    let (rain_sum, land_share) = (blur(&rain), blur(&land));
    (0..w.cells())
        .map(|i| {
            if w.ocean[i] || land_share[i] <= 0.0 {
                1.0
            } else {
                let relative = (rain_sum[i] / land_share[i]).clamp(k.rain_min, k.rain_max.max(k.rain_min));
                1.0 + k.climate_strength.min(1.0) * (relative - 1.0)
            }
        })
        .collect()
}

/// Discharge weight per cell: precipitation relative to the mean over the
/// land (`precip_mm / mean(land precip_mm)`), what `erosion::step` and the
/// hydrology weigh drained area by.
pub fn discharge_weight(c: &Climate, w: &World) -> Vec<f64> {
    let (mut sum, mut land) = (0.0, 0usize);
    for (i, &mm) in c.precip_mm.iter().enumerate() {
        if !w.ocean[i] {
            sum += mm as f64;
            land += 1;
        }
    }
    let mean = (sum / land.max(1) as f64).max(f64::MIN_POSITIVE);
    c.precip_mm.iter().map(|&mm| mm as f64 / mean).collect()
}

// ---- The monthly climate (Phase 1b): the climate grid and a placeholder ----

/// The climate cell aimed at (m): the climate grid's cell is this, halved
/// until the grid has `CLIMATE_MIN_ROWS` rows, never finer than the forge cell.
pub const CLIMATE_CELL_M: f64 = 256.0;
/// Fewest climate rows pole to pole (at most 1.9° of latitude per climate cell).
pub const CLIMATE_MIN_ROWS: usize = 96;

// The placeholder seasonal climate's constants: each is Phase 1b's
// placeholder, and Phase 3's climate model replaces it (with
// `monthly_placeholder` itself). Named constants, not knobs (Ruling 11).
/// Phase 1b placeholder (Phase 3 replaces it): half the annual temperature
/// range (°C) over the open sea at a pole (`sin(lat)` scales it).
pub const SEASON_AMP_MARITIME_C: f64 = 7.0;
/// Phase 1b placeholder (Phase 3 replaces it): half the annual temperature
/// range (°C) deep inland at a pole.
pub const SEASON_AMP_CONTINENTAL_C: f64 = 25.0;
/// Phase 1b placeholder (Phase 3 replaces it): distance from the sea (deg of
/// latitude) at which continentality is one half.
pub const CONTINENTALITY_HALF_DEG: f64 = 8.0;
/// Phase 1b placeholder (Phase 3 replaces it): months the temperature lags
/// the solstice deep inland.
pub const SEASON_LAG_LAND_MONTHS: f64 = 1.0;
/// Phase 1b placeholder (Phase 3 replaces it): months the temperature lags
/// the solstice over the open sea.
pub const SEASON_LAG_SEA_MONTHS: f64 = 2.0;
/// Phase 1b placeholder (Phase 3 replaces it): diurnal range (°C) over the
/// open sea.
pub const DIURNAL_SEA_C: f64 = 4.0;
/// Phase 1b placeholder (Phase 3 replaces it): diurnal range (°C) added deep
/// inland.
pub const DIURNAL_CONTINENT_C: f64 = 6.0;
/// Phase 1b placeholder (Phase 3 replaces it): diurnal range (°C) added over
/// land with no precipitation.
pub const DIURNAL_DRY_C: f64 = 6.0;
/// Phase 1b placeholder (Phase 3 replaces it): annual precipitation (mm) at
/// and above which land adds no dry diurnal range.
pub const DIURNAL_DRY_REFERENCE_MM: f64 = 1500.0;
/// Phase 1b placeholder (Phase 3 replaces it): the share of the monthly mean
/// precipitation that swings with the season (wetter in the local summer).
pub const PRECIP_SEASON_SHARE: f64 = 0.5;
/// Phase 1b placeholder (Phase 3 replaces it): latitude (deg) from which the
/// precipitation season is full (none at the equator, linear between).
pub const PRECIP_SEASON_FULL_DEG: f64 = 15.0;
/// Phase 1b placeholder (Phase 3 replaces it): mean rain (mm) of a wet day in
/// a cool month (at or below `WET_DAY_COOL_C`).
pub const WET_DAY_MM_COOL: f64 = 5.0;
/// Phase 1b placeholder (Phase 3 replaces it): mean rain (mm) of a wet day in
/// a hot month (at or above `WET_DAY_HOT_C`).
pub const WET_DAY_MM_HOT: f64 = 14.0;
/// Phase 1b placeholder (Phase 3 replaces it): sea-level temperature (°C) of
/// a cool month for the wet-day amount.
pub const WET_DAY_COOL_C: f64 = 0.0;
/// Phase 1b placeholder (Phase 3 replaces it): sea-level temperature (°C) of
/// a hot month for the wet-day amount.
pub const WET_DAY_HOT_C: f64 = 25.0;
/// Phase 1b placeholder (Phase 3 replaces it): the largest share of a
/// month's days that are wet.
pub const WET_DAY_MAX: f64 = 0.9;
/// Phase 1b placeholder (Phase 3 replaces it): wind speed (m/s) of the zonal
/// bands, east–west only.
pub const WIND_PLACEHOLDER_MS: f64 = 6.0;

/// Fine cells per climate cell per axis (the plan's rule): max(1, round(CLIMATE_CELL_M / cell_m)),
/// halved while above 1 and nz / s < CLIMATE_MIN_ROWS.
pub fn climate_stride(cell_m: f64, nz: usize) -> usize {
    let mut s = ((CLIMATE_CELL_M / cell_m).round() as usize).max(1);
    while s > 1 && nz / s < CLIMATE_MIN_ROWS {
        s /= 2;
    }
    s
}

/// Err when nx or nz is not a whole number of strides:
/// "this world's sides are not whole numbers of its {c} m climate cells ({w} x {h} m)".
pub fn check_climate_grid(p: &ForgeParams) -> Result<(), String> {
    let (nx, nz) = (p.nx(), p.nz());
    let s = climate_stride(p.cell_m, nz);
    if nx % s != 0 || nz % s != 0 {
        return Err(format!(
            "this world's sides are not whole numbers of its {} m climate cells ({} x {} m)",
            p.cell_m * s as f64,
            p.width_m,
            p.height_m
        ));
    }
    Ok(())
}

/// 90 at the north edge, 0 in the middle, -90 at the south edge (`latitudes`' line, at any z).
pub fn latitude_at(z_m: f64, height_m: f64) -> f64 {
    /// Degrees from a pole to the equator.
    const SPAN: f64 = 90.0;
    let t = z_m / height_m; // 0 at the north edge, 1 at the south
    let lat = if t < 0.5 { 90.0 - SPAN * 2.0 * t } else { -90.0 + SPAN * 2.0 * (1.0 - t) };
    lat.clamp(-90.0, 90.0)
}

/// The placeholder's seasonal shape of month `m` (0-based) for a curve that
/// peaks `lag` months after the northern summer solstice: the cosine AT
/// mid-month, which is 1.1 % above the month's true mean of a cosine (a
/// placeholder nicety; Phase 3 stores real means). Its twelve values sum to 0.
fn season_phase(m: usize, lag: f64) -> f64 {
    let months = game::CLIMATE_MONTHS as f64;
    (std::f64::consts::TAU * (m as f64 + 0.5 - (game::SOLSTICE_NORTH_SUMMER_MONTHS + lag)) / months).cos()
}

/// Climate rows `monthly_placeholder` computes in parallel before it
/// scatters them into the planes (bounds its transient per-row arrays: 64
/// rows of 784 cells at Large are 12 MB, against 74 MB for every row).
const MONTHLY_ROWS_PER_BATCH: usize = 64;

/// The monthly family before quantisation: six planes of 12 * nx * nz, month-major (the plan's vocabulary).
#[derive(Clone, Debug, PartialEq)]
pub struct MonthlyFields {
    pub nx: usize,
    pub nz: usize,
    pub cell_m: f64,
    pub temp_sea_c: Vec<f32>,
    pub diurnal_c: Vec<f32>,
    pub precip_mm: Vec<f32>,
    pub wet_day: Vec<f32>,
    pub wind_east_ms: Vec<f32>,
    pub wind_south_ms: Vec<f32>,
}

/// The placeholder seasonal climate of `w` on its climate grid
/// (`climate_stride`), from today's annual climate `c`: Phase 1b's
/// placeholder, which Phase 3's climate model replaces. Per climate cell, at
/// its centre's effective latitude: the sea-level temperature swings round
/// its annual mean by an amplitude growing with latitude and continentality
/// (distance from the sea on the climate grid), lagging the solstice by two
/// months at sea and one deep inland; the cell's mean annual precipitation is
/// shared out wetter in the local summer; the diurnal range grows inland and
/// with dryness; the wet-day share follows from the month's rain and a wet
/// day's amount by temperature; the wind is the zonal band's, east–west only.
/// Reads its arguments only; cells are computed in parallel by row, each from
/// its own inputs, so the result does not depend on the thread count.
pub fn monthly_placeholder(w: &World, c: &Climate, p: &ForgeParams) -> MonthlyFields {
    let months = game::CLIMATE_MONTHS;
    let s = climate_stride(w.dx, w.nz);
    let (cnx, cnz) = (w.nx / s, w.nz / s);
    let cell_m = w.dx * s as f64;
    let height_m = w.nz as f64 * w.dx;
    // Per climate cell: (share of land, mean annual precipitation).
    let shares: Vec<(f64, f64)> = par::map_range(cnz, |cz| {
        (0..cnx)
            .map(|cx| {
                let (mut land, mut precip) = (0usize, 0.0f64);
                for z in cz * s..(cz + 1) * s {
                    for x in cx * s..(cx + 1) * s {
                        let i = z * w.nx + x;
                        land += usize::from(!w.ocean[i]);
                        precip += c.precip_mm[i] as f64;
                    }
                }
                let n = (s * s) as f64;
                (land as f64 / n, precip / n)
            })
            .collect::<Vec<_>>()
    })
    .concat();
    let sea: Vec<bool> = shares.iter().map(|&(land, _)| land < 0.5).collect();
    let coast = crate::habitat::coast_distance_by(cnx, cnz, cell_m, w.wrap_x, |i| sea[i]);
    let half_m = CONTINENTALITY_HALF_DEG * metres_per_degree(height_m);
    let days = game::history::calendar::DAYS_PER_MONTH as f64;

    let plane = months * cnx * cnz;
    let mut f = MonthlyFields {
        nx: cnx,
        nz: cnz,
        cell_m,
        temp_sea_c: vec![0.0; plane],
        diurnal_c: vec![0.0; plane],
        precip_mm: vec![0.0; plane],
        wet_day: vec![0.0; plane],
        wind_east_ms: vec![0.0; plane],
        wind_south_ms: vec![0.0; plane],
    };
    // Each row's cells, each cell its twelve (temperature, diurnal, precip,
    // wet day, wind east) values, computed in batches of rows scattered into
    // the month-major planes before the next batch: the per-row arrays never
    // hold more than `MONTHLY_ROWS_PER_BATCH` rows beside the planes.
    let row_values = |cz: usize| -> Vec<[[f32; 5]; game::CLIMATE_MONTHS]> {
        let zc = (cz as f64 + 0.5) * cell_m;
        (0..cnx)
            .map(|cx| {
                let ci = cz * cnx + cx;
                let xc = (cx as f64 + 0.5) * cell_m;
                let lat = (latitude_at(zc, height_m) + meander_deg(xc, zc, p)).clamp(-90.0, 90.0);
                let (land, annual_precip) = shares[ci];
                let mean_c = sea_level_temperature_c(lat, p);
                let d = coast[ci] as f64;
                let k = d / (d + half_m);
                let lag = SEASON_LAG_SEA_MONTHS + (SEASON_LAG_LAND_MONTHS - SEASON_LAG_SEA_MONTHS) * k;
                let amp = lat.to_radians().sin()
                    * (SEASON_AMP_MARITIME_C + (SEASON_AMP_CONTINENTAL_C - SEASON_AMP_MARITIME_C) * k);
                let wet_season = PRECIP_SEASON_SHARE * (lat / PRECIP_SEASON_FULL_DEG).clamp(-1.0, 1.0);
                let diurnal = DIURNAL_SEA_C
                    + DIURNAL_CONTINENT_C * k
                    + DIURNAL_DRY_C * land * (1.0 - annual_precip / DIURNAL_DRY_REFERENCE_MM).clamp(0.0, 1.0);
                let east = (2.0 * westerly_weight(lat, p) - 1.0) * WIND_PLACEHOLDER_MS;
                std::array::from_fn(|m| {
                    let temp = mean_c + amp * season_phase(m, lag);
                    let precip = annual_precip / months as f64
                        * (1.0 + wet_season * season_phase(m, SEASON_LAG_LAND_MONTHS));
                    let warmth = ((temp - WET_DAY_COOL_C) / (WET_DAY_HOT_C - WET_DAY_COOL_C)).clamp(0.0, 1.0);
                    let amount = WET_DAY_MM_COOL + (WET_DAY_MM_HOT - WET_DAY_MM_COOL) * warmth;
                    let wet = (precip / (days * amount)).clamp(0.0, WET_DAY_MAX);
                    [temp as f32, diurnal as f32, precip as f32, wet as f32, east as f32]
                })
            })
            .collect()
    };
    for z0 in (0..cnz).step_by(MONTHLY_ROWS_PER_BATCH) {
        let rows = par::map_range(MONTHLY_ROWS_PER_BATCH.min(cnz - z0), |k| row_values(z0 + k));
        for (k, row) in rows.iter().enumerate() {
            let cz = z0 + k;
            for (cx, cell) in row.iter().enumerate() {
                for (m, v) in cell.iter().enumerate() {
                    let i = m * cnx * cnz + cz * cnx + cx;
                    f.temp_sea_c[i] = v[0];
                    f.diurnal_c[i] = v[1];
                    f.precip_mm[i] = v[2];
                    f.wet_day[i] = v[3];
                    f.wind_east_ms[i] = v[4];
                }
            }
        }
    }
    f
}

/// `round(v · per)` in f64 and whether it lies outside `lo ..= hi` (a value
/// that is not a number counts as outside and stores 0).
fn quantum(v: f32, per: f64, lo: f64, hi: f64) -> (f64, bool) {
    let r = (f64::from(v) * per).round();
    if r.is_nan() {
        return (0.0, true);
    }
    (r.clamp(lo, hi), !(lo..=hi).contains(&r))
}

impl MonthlyFields {
    /// The six planes with each one's denominator and integer bounds.
    fn planes(&self) -> [(&[f32], f64, f64, f64); 6] {
        [
            (&self.temp_sea_c, game::CLIMATE_TEMP_PER_C, f64::from(i16::MIN), f64::from(i16::MAX)),
            (&self.diurnal_c, game::CLIMATE_DIURNAL_PER_C, 0.0, f64::from(u8::MAX)),
            (&self.precip_mm, game::CLIMATE_PRECIP_PER_MM, 0.0, f64::from(u16::MAX)),
            (&self.wet_day, game::CLIMATE_WET_DAY_PER_ONE, 0.0, f64::from(u8::MAX)),
            (&self.wind_east_ms, game::CLIMATE_WIND_PER_MS, f64::from(i8::MIN), f64::from(i8::MAX)),
            (&self.wind_south_ms, game::CLIMATE_WIND_PER_MS, f64::from(i8::MIN), f64::from(i8::MAX)),
        ]
    }

    /// round(value * DENOMINATOR) in f64, saturating at each type's bounds (game's CLIMATE_*_PER_* constants).
    pub fn quantise(&self, wrap_x: bool) -> game::MonthlyClimate {
        fn q<T>((v, per, lo, hi): (&[f32], f64, f64, f64), cast: fn(f64) -> T) -> Vec<T> {
            v.iter().map(|&x| cast(quantum(x, per, lo, hi).0)).collect()
        }
        let [t, d, pr, wd, we, ws] = self.planes();
        game::MonthlyClimate {
            nx: self.nx as u32,
            nz: self.nz as u32,
            cell_m: self.cell_m,
            wrap_x,
            temp_sea_centi_c: q(t, |r| r as i16),
            diurnal_deci_c: q(d, |r| r as u8),
            precip_deci_mm: q(pr, |r| r as u16),
            wet_day_255: q(wd, |r| r as u8),
            wind_east_q: q(we, |r| r as i8),
            wind_south_q: q(ws, |r| r as i8),
        }
    }

    /// How many values `quantise` saturates.
    pub fn clamped_values(&self) -> usize {
        self.planes()
            .into_iter()
            .map(|(v, per, lo, hi)| v.iter().filter(|&&x| quantum(x, per, lo, hi).1).count())
            .sum()
    }
}
