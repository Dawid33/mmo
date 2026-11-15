//! The world's climate on the CURRENT heightfield: latitude per row,
//! temperature from latitude and height, and precipitation in mm/yr.
//!
//! - **Latitude** runs along z (rows): `pole` puts a pole on the north
//!   (z = 0) edge, the south edge, both, or none (the whole map at
//!   `latitude_center_deg`); `latitude_span_deg` is how far the map reaches
//!   from its pole(s). Southern latitudes are negative; everything below uses
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
//!   edge has crossed endless land and is at the equilibrium.
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
    let span = k.latitude_span_deg.clamp(0.0, 180.0);
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
/// column).
fn box_blur_rows(src: &[f64], nx: usize, nz: usize, radius: usize, along_x: bool) -> Vec<f64> {
    let mut out = vec![0.0; nx * nz];
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
    let ground = box_blur_rows(&box_blur_rows(&ground, nx, nz, terrain, true), nx, nz, terrain, false);
    // `World::edges` order: N, E, S, W.
    let inflow = |edge: usize| if w.edges[edge] == EdgeKind::Sea { 1.0 } else { CONTINENTAL_MOISTURE };
    let recharge = (dx / k.sea_recharge_m).min(1.0);
    let relax = (dx / k.land_depletion_m).min(1.0);
    let lift = OROGRAPHIC_RUN_M / k.orographic_scale_m / dx;

    // One row's precipitation relative to the base, wind from the west or east.
    let sweep = |z: usize, from_west: bool, row: &mut [f64]| {
        let mut moisture: f64 = if from_west { inflow(3) } else { inflow(1) };
        let mut prev: Option<usize> = None;
        for step in 0..nx {
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
    let blur = |v: &[f64]| box_blur_rows(&box_blur_rows(v, nx, nz, cross, false), nx, nz, along, true);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::world::EdgeKind;
    use crate::forge::Knobs;

    const N: usize = 64;
    const CELL_M: f64 = 128.0;

    /// Params for an `N`-cell world at `CELL_M`, one latitude everywhere
    /// (`pole` none, no meander: straight rows) unless the caller changes it.
    fn params(latitude: f64) -> ForgeParams {
        let mut p = ForgeParams::square(N as f64 * CELL_M, CELL_M);
        p.knobs.pole = POLE_NONE as f64;
        p.knobs.latitude_center_deg = latitude;
        p.knobs.meander_amplitude_deg = 0.0;
        p
    }

    /// With the default meander, the -5 C sea-level isotherm wanders north
    /// and south across a pole-to-equator sea; without it, it is one row.
    #[test]
    fn isotherms_meander() {
        let (n, dx) = (256, 128.0);
        let isotherm_spread_m = |amplitude: f64| {
            let mut p = ForgeParams::square(n as f64 * dx, dx);
            p.knobs.pole = POLE_NORTH as f64;
            p.knobs.meander_amplitude_deg = amplitude;
            let w = world_sized(n, dx, |_, _| -10.0);
            let c = compute_with_lakes(&w, &p, &vec![false; n * n]);
            // Per column: the first row (from the pole) at or above -5 C.
            let rows: Vec<usize> =
                (0..n).map(|x| (0..n).find(|&z| c.temperature_c[z * n + x] >= -5.0).unwrap_or(n)).collect();
            (rows.iter().max().unwrap() - rows.iter().min().unwrap()) as f64 * dx
        };
        assert_eq!(isotherm_spread_m(0.0), 0.0);
        let spread = isotherm_spread_m(Knobs::default().meander_amplitude_deg);
        assert!(spread >= 1_500.0, "the -5 C isotherm only wanders {spread} m across a 32 km map");
    }

    /// The meander is a function of metres: the 64 m grid's value is the
    /// mean of its four 32 m cells (to curvature), and it is the same at
    /// every call.
    #[test]
    fn meander_field_is_resolution_independent() {
        let size = 16_000.0;
        let p = ForgeParams::square(size, crate::forge::world::DEFAULT_CELL_M);
        let (fine, coarse) = (meander_field(500, 500, 32.0, &p), meander_field(250, 250, 64.0, &p));
        assert_eq!(fine, meander_field(500, 500, 32.0, &p));
        let mut worst = 0.0f32;
        for z in 0..250 {
            for x in 0..250 {
                let f = |dx: usize, dz: usize| fine[(2 * z + dz) * 500 + 2 * x + dx];
                let mean = (f(0, 0) + f(1, 0) + f(0, 1) + f(1, 1)) / 4.0;
                worst = worst.max((mean - coarse[z * 250 + x]).abs());
            }
        }
        assert!(worst < 0.02, "64 m vs 32 m meander differs by {worst} deg");
        let (lo, hi) = coarse.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        assert!(hi - lo > 2.0 * p.knobs.meander_amplitude_deg as f32, "meander range {lo}..{hi}");
    }

    /// A hand-made world: `height(x, z)`, ocean where it is <= 0, sea on
    /// every edge.
    fn world(height: impl Fn(usize, usize) -> f64) -> World {
        world_sized(N, CELL_M, height)
    }

    fn world_sized(n: usize, dx: f64, height: impl Fn(usize, usize) -> f64) -> World {
        let h: Vec<f64> = (0..n * n).map(|i| height(i % n, i / n)).collect();
        let ocean: Vec<bool> = h.iter().map(|&h| h <= 0.0).collect();
        World {
            nx: n,
            nz: n,
            dx,
            outlet: ocean.clone(),
            ocean,
            uplift: vec![0.0; n * n],
            uplifted: vec![0.0; n * n],
            margin_drowned: 0,
            edges: [EdgeKind::Sea; 4],
            h,
        }
    }

    /// Land between 4-cell ocean strips on the west and east, `ridge` m high
    /// along the N-S line x = N/2 (a 6-cell-wide tent), flat 20 m elsewhere.
    fn ridge_world(ridge: f64) -> World {
        world(|x, _| {
            if x < 4 || x >= N - 4 {
                -50.0
            } else {
                let d = (x as f64 - N as f64 / 2.0).abs();
                20.0 + ridge * (1.0 - d / 6.0).max(0.0)
            }
        })
    }

    /// Mean precipitation over land cells with `lo <= x < hi`.
    fn band_precip(c: &Climate, w: &World, lo: usize, hi: usize) -> f64 {
        let cells: Vec<f64> = (0..N * N)
            .filter(|&i| (lo..hi).contains(&(i % N)) && !w.ocean[i])
            .map(|i| c.precip_mm[i] as f64)
            .collect();
        cells.iter().sum::<f64>() / cells.len() as f64
    }

    #[test]
    fn temperature_falls_toward_the_pole_and_with_height() {
        let mut p = params(45.0);
        p.knobs.pole = POLE_NORTH as f64;
        // Sea level everywhere but one 1,000 m column in the middle row.
        let w = world(|x, z| if (x, z) == (20, N / 2) { 1_000.0 } else if x < 4 { -10.0 } else { 0.0 });
        let c = compute(&w, &p);
        let row_mean = |z: usize| (0..N).map(|x| c.temperature_c[z * N + x] as f64).sum::<f64>() / N as f64;
        let (north, south) = (row_mean(0), row_mean(N - 1));
        let want = p.knobs.equator_temp_c - p.knobs.pole_temp_c;
        assert!((south - north - want).abs() <= 2.0, "south {south:.1} - north {north:.1} vs {want}");
        assert!(c.latitude_deg[0] > 89.0 && c.latitude_deg[N - 1].abs() < 1.0, "{:?}", c.latitude_deg);
        let row = N / 2;
        let drop = c.temperature_c[row * N + 21] - c.temperature_c[row * N + 20];
        assert!((drop - 6.5).abs() < 1e-3, "1,000 m up is {drop} C colder");
    }

    #[test]
    fn westerlies_cast_a_rain_shadow_east_of_a_ridge() {
        let p = params(45.0);
        assert_eq!(wind(45.0, &p), Wind::FromWest);
        let w = ridge_world(1_200.0);
        let c = compute(&w, &p);
        let (west, east) = (band_precip(&c, &w, 20, 30), band_precip(&c, &w, 34, 44));
        assert!(west > 1.5 * east, "west flank {west:.0} mm vs east {east:.0} mm");
    }

    #[test]
    fn trade_winds_cast_the_shadow_west() {
        let p = params(15.0);
        assert_eq!(wind(15.0, &p), Wind::FromEast);
        let w = ridge_world(1_200.0);
        let c = compute(&w, &p);
        let (west, east) = (band_precip(&c, &w, 20, 30), band_precip(&c, &w, 34, 44));
        assert!(east > 1.5 * west, "east flank {east:.0} mm vs west {west:.0} mm");
        // Polar easterlies too.
        assert_eq!(wind(75.0, &p), Wind::FromEast);
    }

    /// At the default knobs: flat land under air at the continental
    /// equilibrium gets the latitude table's value, and so does flat land
    /// far inland from a sea coast.
    #[test]
    fn precip_follows_the_latitude_table_on_flat_land() {
        let p = params(25.0);
        // All land, land edges all round: air arrives at the equilibrium.
        let mut w = world(|_, _| 2.0);
        w.edges = [EdgeKind::Land; 4];
        let c = compute(&w, &p);
        for i in 0..N * N {
            assert!((c.precip_mm[i] - 180.0).abs() <= 60.0, "all land, cell {i}: {} mm", c.precip_mm[i]);
        }
        // 32 km of plain with the sea upwind (the trades blow from the east).
        let wide = 512.0;
        let w = world_sized(N, wide, |x, _| if x >= N - 4 { -50.0 } else { 2.0 });
        let p = ForgeParams { width_m: N as f64 * wide, height_m: N as f64 * wide, cell_m: wide, ..p };
        let c = compute(&w, &p);
        let inland = p.knobs.land_depletion_m * 3.0;
        for i in (0..N * N).filter(|&i| i % N < N - 4 && ((N - 4 - i % N) as f64) * wide > inland) {
            assert!((c.precip_mm[i] - 180.0).abs() <= 60.0, "far inland, cell {i}: {} mm", c.precip_mm[i]);
        }
        // The coast itself is wetter than the interior.
        // (The same ratio as before the desert belt: 400 mm over a 350 mm base.)
        assert!(c.precip_mm[N / 2 * N + N - 5] > 180.0 * 400.0 / 350.0, "coast {}", c.precip_mm[N / 2 * N + N - 5]);
        assert_eq!(base_precip_mm(17.5), (1800.0 + 180.0) / 2.0);
        assert_eq!(base_precip_mm(-90.0), 200.0);
    }

    /// Where the westerlies meet the trades, the rain does not jump between
    /// nearby rows: the bands blend over a few degrees. Measured as the
    /// change of precipitation (relative to the latitude base) over 1 km of z
    /// on a 16 km pole-to-equator map (64 m cells).
    #[test]
    fn band_edges_have_no_step() {
        let (n, dx) = (250, 64.0);
        let mut p = ForgeParams { width_m: n as f64 * dx, height_m: n as f64 * dx, cell_m: dx, ..params(45.0) };
        p.knobs.pole = POLE_NORTH as f64;
        // Sea strips on both sides, so each wind direction has its own wet coast.
        let w = world_sized(n, dx, |x, _| if x < 8 || x >= n - 8 { -50.0 } else { 2.0 });
        let c = compute(&w, &p);
        let rel = |x: usize, z: usize| c.precip_mm[z * n + x] / base_precip_mm(c.latitude_deg[z]);
        let window = (1_000.0 / dx) as usize;
        let mut worst = (0.0f32, 0, 0);
        for z in 0..n - window {
            for x in 8..n - 8 {
                let d = (rel(x, z + window) - rel(x, z)).abs();
                if d > worst.0 {
                    worst = (d, x, z);
                }
            }
        }
        assert!(
            worst.0 < 0.3,
            "rows {}..{} (lat {:.1}) at x {}: relative precipitation changes {:.3} within 1 km",
            worst.2, worst.2 + window, c.latitude_deg[worst.2], worst.1, worst.0
        );
    }

    /// The rain field is set in metres, not cells: a lake's downwind wet
    /// stripe is about as wide at 32 m cells as at 64 m.
    #[test]
    fn rain_field_is_resolution_independent() {
        let size = 16_000.0;
        // Across z, at 3 km downwind (east) of a 2 km square lake in the
        // westerlies: the width (m) of the stripe wetter than halfway to its
        // peak, and of its edges (10%..90% of the way up).
        let stripe = |dx: f64| {
            let n = (size / dx) as usize;
            let centre = size / 2.0;
            let p = ForgeParams { width_m: size, height_m: size, cell_m: dx, quiet: true, ..params(45.0) };
            // All land under continental air, so the lake's moisture shows.
            let mut w = world_sized(n, dx, |_, _| 20.0);
            w.edges = [EdgeKind::Land; 4];
            let lakes: Vec<bool> = (0..n * n)
                .map(|i| {
                    let (x, z) = ((i % n) as f64 * dx + dx / 2.0, (i / n) as f64 * dx + dx / 2.0);
                    (x - centre).abs() < 1_000.0 && (z - centre).abs() < 1_000.0
                })
                .collect();
            let c = compute_with_lakes(&w, &p, &lakes);
            let col = ((centre + 3_000.0) / dx) as usize;
            let profile: Vec<f32> = (0..n).map(|z| c.precip_mm[z * n + col]).collect();
            let (lo, hi) = profile.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            assert!(hi > lo * 1.03, "{dx} m: no stripe ({lo}..{hi})");
            let edge = |v: f32| v > lo + 0.1 * (hi - lo) && v < lo + 0.9 * (hi - lo);
            let (wide, soft) = (
                profile.iter().filter(|&&v| v > (lo + hi) / 2.0).count() as f64 * dx,
                profile.iter().filter(|&&v| edge(v)).count() as f64 * dx,
            );
            (wide, soft)
        };
        let ((fine, fine_edge), (coarse, coarse_edge)) = (stripe(32.0), stripe(64.0));
        assert!(fine > 0.67 * coarse && fine < 1.5 * coarse, "stripe {fine} m at 32 m cells, {coarse} m at 64 m");
        // Its edges blur over the same metres, not the same cells.
        assert!(
            fine_edge > 0.67 * coarse_edge && fine_edge < 1.5 * coarse_edge,
            "stripe edges {fine_edge} m at 32 m cells, {coarse_edge} m at 64 m"
        );
    }

    #[test]
    fn discharge_weight_averages_one_over_land() {
        let p = params(45.0);
        let w = ridge_world(800.0);
        let weight = discharge_weight(&compute(&w, &p), &w);
        let land: Vec<f64> = (0..N * N).filter(|&i| !w.ocean[i]).map(|i| weight[i]).collect();
        let mean = land.iter().sum::<f64>() / land.len() as f64;
        assert!((mean - 1.0).abs() < 1e-6, "{mean}");
    }

    /// Air blowing in across a land edge is drier than air off the sea: with
    /// a sea strip in the middle of the land, the land upwind of it gets less
    /// of the rain when the upwind (west) edge is land.
    #[test]
    fn a_land_edge_upwind_brings_drier_air() {
        let mut p = params(45.0);
        p.knobs.sea_recharge_m = 500.0;
        let strip = |x: usize| (N / 2 - 4..N / 2 + 4).contains(&x) || x >= N - 4;
        let upwind_share = |edge: EdgeKind| {
            let mut w = world(|x, _| if strip(x) { -50.0 } else { 20.0 });
            w.edges[3] = edge;
            let c = compute(&w, &p);
            band_precip(&c, &w, 0, N / 2 - 4) / band_precip(&c, &w, N / 2 + 4, N - 4)
        };
        let (land, sea) = (upwind_share(EdgeKind::Land), upwind_share(EdgeKind::Sea));
        assert!(land < 0.8 * sea, "upwind/downwind rain: land edge {land:.2}, sea edge {sea:.2}");
    }

    /// A 2:1 world (twice as wide as tall): one latitude per ROW (`nz`),
    /// pole to pole down z — not per column.
    #[test]
    fn latitudes_run_down_the_rows_of_a_wide_world() {
        let (nx, nz) = (2 * N, N);
        let mut p = params(0.0);
        p.width_m = nx as f64 * CELL_M;
        p.knobs.pole = POLE_BOTH as f64;
        let mut w = world_sized(N, CELL_M, |_, _| -50.0);
        w.nx = nx;
        for v in [&mut w.h, &mut w.uplift, &mut w.uplifted] {
            *v = vec![-50.0; nx * nz];
        }
        w.ocean = vec![true; nx * nz];
        w.outlet = w.ocean.clone();
        let c = compute(&w, &p);
        assert_eq!(c.latitude_deg.len(), nz);
        assert!(c.latitude_deg[0] > 85.0 && c.latitude_deg[nz - 1] < -85.0, "{:?}", c.latitude_deg);
        assert_eq!(c.temperature_c.len(), nx * nz);
    }

    /// A blur along z (`along_x = false`) of a single-column impulse on a
    /// non-square grid stays in that column: a swapped stride would smear it
    /// across the row instead (and still stay in range).
    #[test]
    fn box_blur_along_z_stays_in_its_column() {
        let (nx, nz) = (12, 5);
        let column = 7;
        let src: Vec<f64> = (0..nx * nz).map(|i| if i % nx == column { 1.0 } else { 0.0 }).collect();
        let out = box_blur_rows(&src, nx, nz, 2, false);
        for i in 0..nx * nz {
            let want = if i % nx == column { 1.0 } else { 0.0 };
            assert_eq!(out[i], want, "cell ({}, {})", i % nx, i / nx);
        }
        // Along x the same impulse spreads over its row neighbours.
        let across = box_blur_rows(&src, nx, nz, 2, true);
        assert!(across[column - 1] > 0.0 && across[column + 3] == 0.0);
    }

    #[test]
    fn latitudes_follow_the_pole() {
        let mut p = params(30.0);
        assert!(latitudes(8, &p).iter().all(|&l| l == 30.0));
        p.knobs.pole = POLE_SOUTH as f64;
        let l = latitudes(8, &p);
        assert!(l[7] < -84.0 && l[0] > -10.0, "{l:?}");
        p.knobs.pole = POLE_BOTH as f64;
        let l = latitudes(64, &p);
        assert!(l[0] > 85.0 && l[63] < -85.0 && l[31] > 0.0 && l[32] < 0.0 && l[31] < 5.0, "{l:?}");
    }
}
