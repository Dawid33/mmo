//! The world's climate on the CURRENT heightfield: latitude per row,
//! temperature from latitude and height, and precipitation in mm/yr.
//!
//! - **Latitude** runs along z (rows): `pole` puts a pole on the north
//!   (z = 0) edge, the south edge, both, or none (the whole map at
//!   `latitude_center_deg`); `latitude_span_deg` is how far the map reaches
//!   from its pole(s). A planet has both poles and the full span: 90° N at
//!   the north edge, the equator in the middle row, 90° S at the south edge. Southern latitudes are negative; everything below uses
//!   the absolute value.
//! - **Meander**: everything that depends on latitude below (temperature, the
//!   rain table, the wind bands) reads an EFFECTIVE latitude per cell — the
//!   row's latitude plus a slow, seed-derived displacement
//!   (`meander_field`: a few long waves running mostly along x, like
//!   planetary waves on the jet stream), so isotherms and band edges wander
//!   instead of running ruler-straight. Set in metres and degrees
//!   (`meander_amplitude_deg`, `meander_wavelength_km`), so it is the same
//!   field at every cell size and coarse-to-fine level; 0 = straight rows.
//! - **Temperature** at sea level follows the cosine of latitude between
//!   `pole_temp_c` and `equator_temp_c`, minus `lapse_c_per_km` per km of
//!   ground above the sea.
//! - **Winds** blow along rows: from the east below `trade_wind_edge_deg`
//!   (trade winds) and above `polar_edge_deg` (polar easterlies), from the
//!   west between (westerlies); within `BAND_BLEND_DEG` of a band edge a row
//!   blends both directions, so no line marks the edge. Each row's moisture
//!   sweep runs downwind. Over flat land the air's moisture relaxes toward
//!   the continental equilibrium `CONTINENTAL_MOISTURE` over
//!   `land_depletion_m` (rain-out balanced by evaporation from the land);
//!   forced up a slope it rains out (`orographic_scale_m` of rise wrings out
//!   about all of it) and arrives dry on the lee side; it recharges toward
//!   saturation over any sea (inland seas included) and any lake. Air that
//!   enters across a SEA edge is saturated; air that enters across a LAND
//!   edge has crossed endless land and is at the equilibrium. On a world that
//!   wraps east-west (the planet) no air enters: each row's sweep goes once
//!   round from saturation, then records the lap after it.
//! - **Precipitation** (mm/yr) is the latitude's base (`LATITUDE_PRECIP_MM`:
//!   wet equator, dry ~25° belts, wet ~50°, dry poles) times the air's
//!   moisture relative to the equilibrium, raised on windward slopes
//!   (`OROGRAPHIC_RUN_M`), clamped to `rain_min`..`rain_max` of the base:
//!   flat land far inland gets exactly the table, coasts downwind of the sea
//!   more, lee sides less. Every smoothing length is in metres, so the field
//!   does not depend on the cell size.
//!
//! Precipitation weights discharge (`discharge_weight`), so wet flanks grow
//! bigger rivers and erode faster — Dwarf Fortress computes rain shadows AFTER
//! its rivers, which is why a DF desert river is as wide as a jungle one.

use crate::forge::erosion;
use crate::forge::hydrology::LAKE_MIN_DEPTH_M;
use crate::forge::par;
use crate::forge::world::{hash01, smoothstep, EdgeKind, ForgeParams, World};

/// `Knobs::pole` values.
pub const POLE_NONE: u32 = 0;
pub const POLE_NORTH: u32 = 1;
pub const POLE_SOUTH: u32 = 2;
pub const POLE_BOTH: u32 = 3;

/// Annual precipitation (mm/yr) by absolute latitude (deg), interpolated
/// linearly: the climatological base before rain shadows. The 25° entry is
/// the subtropical desert belt (real interiors there get 25–200 mm), so flat
/// interiors under the subtropical high are desert, not savanna.
pub const LATITUDE_PRECIP_MM: [(f32, f32); 7] =
    [(0.0, 2500.0), (10.0, 1800.0), (25.0, 180.0), (40.0, 900.0), (55.0, 1000.0), (70.0, 400.0), (90.0, 200.0)];

/// Moisture (1 = saturated sea air) that air settles at over endless flat
/// land: what it rains out is balanced by what the land gives back. Flat land
/// under air at this moisture gets the latitude table's precipitation; air
/// blowing in across a LAND edge arrives at it.
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
/// `meander_wavelength_km`, amplitude as a share of `meander_amplitude_deg`,
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
/// Cross-wind blur radius of the rain field (m; the box is twice as wide):
/// air mixes sideways.
const RAIN_CROSS_BLUR_M: f64 = 1_500.0;
/// Along-wind blur radius of the rain field (m).
const RAIN_ALONG_BLUR_M: f64 = 500.0;

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

/// Latitude (deg) of each of `nz` rows.
pub fn latitudes(nz: usize, p: &ForgeParams) -> Vec<f32> {
    let k = &p.knobs;
    let span = k.latitude_span();
    (0..nz)
        .map(|z| {
            let t = (z as f64 + 0.5) / nz as f64; // 0 at the north edge, 1 at the south
            let lat = match k.pole_choice() {
                POLE_NONE => k.latitude_center_deg,
                POLE_NORTH => 90.0 - span * t,
                POLE_SOUTH => -90.0 + span * (1.0 - t),
                _ if t < 0.5 => 90.0 - span * 2.0 * t,
                _ => -90.0 + span * 2.0 * (1.0 - t),
            };
            lat.clamp(-90.0, 90.0) as f32
        })
        .collect()
}

/// Latitude displacement (deg, northward positive) of the meander at artifact
/// metres `(x_m, z_m)`: a function of position, seed and knobs only.
pub fn meander_deg(x_m: f64, z_m: f64, p: &ForgeParams) -> f64 {
    let k = &p.knobs;
    if k.meander_amplitude_deg <= 0.0 {
        return 0.0;
    }
    let wavelength = (k.meander_wavelength_km * 1000.0).min(p.span_m() / MEANDER_MIN_WAVES).max(1.0);
    let mut d = 0.0;
    for (wave, &(share, weight, spread)) in MEANDER_WAVES.iter().enumerate() {
        let angle = (hash01(p.seed, MEANDER_SALT, 2 * wave as u64) - 0.5) * 2.0 * spread;
        let phase = hash01(p.seed, MEANDER_SALT, 2 * wave as u64 + 1) * std::f64::consts::TAU;
        let along = x_m * angle.cos() + z_m * angle.sin();
        d += weight * (std::f64::consts::TAU * along / (wavelength * share) + phase).sin();
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
    let rows = latitudes(nz, p);
    let meander = meander_field(nx, nz, dx, p);
    (0..nx * nz).map(|i| (rows[i / nx] + meander[i]).clamp(-90.0, 90.0)).collect()
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
    let latitude_deg = latitudes(w.nz, p);
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

/// Blur radius (cells) of `metres` on a `dx` grid.
fn radius_cells(metres: f64, dx: f64) -> usize {
    (metres / dx).round() as usize
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
    let recharge = (dx / k.sea_recharge_m).min(1.0);
    let relax = (dx / k.land_depletion_m).min(1.0);
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
    let (cross, along) = (radius_cells(RAIN_CROSS_BLUR_M, dx), radius_cells(RAIN_ALONG_BLUR_M, dx));
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

