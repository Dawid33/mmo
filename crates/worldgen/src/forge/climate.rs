//! The world's climate on the CURRENT heightfield: latitude per row,
//! temperature from latitude and height, and precipitation in mm/yr.
//!
//! - **Latitude** runs along z (rows): `pole` puts a pole on the north
//!   (z = 0) edge, the south edge, both, or none (the whole map at
//!   `latitude_center_deg`); `latitude_span_deg` is how far the map reaches
//!   from its pole(s). Southern latitudes are negative; everything below uses
//!   the absolute value.
//! - **Temperature** at sea level follows the cosine of latitude between
//!   `pole_temp_c` and `equator_temp_c`, minus `lapse_c_per_km` per km of
//!   ground above the sea.
//! - **Winds** blow along rows: from the east below `trade_wind_edge_deg`
//!   (trade winds) and above `polar_edge_deg` (polar easterlies), from the
//!   west between (westerlies). Each row's moisture sweep runs downwind:
//!   moist air rains out as it crosses land and is forced up slopes, and
//!   arrives dry on the lee side. It recharges over any sea (inland seas
//!   included) and any lake. Air that enters across a SEA edge is moist; air
//!   that enters across a LAND edge has crossed land the map does not show
//!   and arrives with `LAND_EDGE_INFLOW_MOISTURE`.
//! - **Precipitation** (mm/yr) is the latitude's base (`LATITUDE_PRECIP_MM`:
//!   wet equator, dry ~25° belts, wet ~50°, dry poles) times the orographic
//!   factor (rain-out relative to the land mean), clamped to
//!   `rain_min`..`rain_max` of the base.
//!
//! Precipitation weights discharge (`discharge_weight`), so wet flanks grow
//! bigger rivers and erode faster — Dwarf Fortress computes rain shadows AFTER
//! its rivers, which is why a DF desert river is as wide as a jungle one.

use crate::forge::erosion;
use crate::forge::hydrology::LAKE_MIN_DEPTH_M;
use crate::forge::par;
use crate::forge::world::{EdgeKind, ForgeParams, World};

/// `Knobs::pole` values.
pub const POLE_NONE: u32 = 0;
pub const POLE_NORTH: u32 = 1;
pub const POLE_SOUTH: u32 = 2;
pub const POLE_BOTH: u32 = 3;

/// Annual precipitation (mm/yr) by absolute latitude (deg), interpolated
/// linearly: the climatological base before rain shadows.
pub const LATITUDE_PRECIP_MM: [(f32, f32); 7] =
    [(0.0, 2500.0), (10.0, 1800.0), (25.0, 350.0), (40.0, 900.0), (55.0, 1000.0), (70.0, 400.0), (90.0, 200.0)];

/// Moisture (1 = saturated sea air) of air blowing in across a LAND edge: it
/// has crossed land beyond the map and rained part of its water out.
pub const LAND_EDGE_INFLOW_MOISTURE: f64 = 0.5;

// Sea recharge, land rain-out distance, orographic scale and the rain bounds
// are knobs (knobs.rs, "Climate").
/// Heights are blurred over this radius (cells) before taking windward slopes,
/// so rain follows ranges, not individual gullies.
const TERRAIN_BLUR_CELLS: usize = 6;
/// Cross-wind blur of the rain field (cells): air mixes sideways.
const RAIN_BLUR_CELLS: usize = 5;
/// Along-wind blur of the rain field (cells).
const RAIN_ALONG_BLUR_CELLS: usize = 2;

/// The climate fields of one grid, row-major like `World::h`.
#[derive(Clone, Debug, PartialEq)]
pub struct Climate {
    /// Mean annual temperature at the ground (sea level over water), deg C.
    pub temperature_c: Vec<f32>,
    /// Annual precipitation, mm/yr.
    pub precip_mm: Vec<f32>,
    /// Latitude of each ROW (`n` values, deg; south negative).
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

/// Latitude (deg) of each of `n` rows.
pub fn latitudes(n: usize, p: &ForgeParams) -> Vec<f32> {
    let k = &p.knobs;
    let span = k.latitude_span_deg.clamp(0.0, 180.0);
    (0..n)
        .map(|z| {
            let t = (z as f64 + 0.5) / n as f64; // 0 at the north edge, 1 at the south
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

/// Sea-level temperature (deg C) at latitude `lat_deg`.
pub fn sea_level_temperature_c(lat_deg: f64, p: &ForgeParams) -> f64 {
    let k = &p.knobs;
    k.pole_temp_c + (k.equator_temp_c - k.pole_temp_c) * lat_deg.abs().min(90.0).to_radians().cos()
}

/// The prevailing wind at latitude `lat_deg`.
pub fn wind(lat_deg: f64, p: &ForgeParams) -> Wind {
    let (k, lat) = (&p.knobs, lat_deg.abs());
    if lat < k.trade_wind_edge_deg || lat >= k.polar_edge_deg {
        Wind::FromEast
    } else {
        Wind::FromWest
    }
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

fn box_blur_rows(src: &[f64], n: usize, radius: usize, along_x: bool) -> Vec<f64> {
    let mut out = vec![0.0; n * n];
    for a in 0..n {
        let at = |b: usize| if along_x { src[a * n + b] } else { src[b * n + a] };
        let mut sum: f64 = (0..=radius.min(n - 1)).map(at).sum();
        let mut count = radius.min(n - 1) + 1;
        for b in 0..n {
            let v = sum / count as f64;
            if along_x { out[a * n + b] = v } else { out[b * n + a] = v }
            if b + radius + 1 < n {
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
    (0..w.n * w.n).map(|i| !w.ocean[i] && filled[i] - w.h[i] >= LAKE_MIN_DEPTH_M).collect()
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
    let (n, k) = (w.n, &p.knobs);
    let latitude_deg = latitudes(n, p);
    let lapse_c_per_m = k.lapse_c_per_km / 1000.0;
    let temperature_c = (0..n * n)
        .map(|i| {
            let ground = if w.ocean[i] { 0.0 } else { w.h[i].max(0.0) };
            (sea_level_temperature_c(latitude_deg[i / n] as f64, p) - lapse_c_per_m * ground) as f32
        })
        .collect();

    let relative = if k.climate_strength <= 0.0 { vec![1.0; n * n] } else { orographic(w, p, &latitude_deg, lakes) };
    let precip_mm = (0..n * n).map(|i| base_precip_mm(latitude_deg[i / n]) * relative[i] as f32).collect();
    Climate { temperature_c, precip_mm, latitude_deg }
}

/// Rain-out per cell relative to the land mean, clamped and scaled by the
/// climate strength (1.0 at sea).
fn orographic(w: &World, p: &ForgeParams, latitude_deg: &[f32], lakes: &[bool]) -> Vec<f64> {
    let (n, k) = (w.n, &p.knobs);
    let ground: Vec<f64> = w.h.iter().map(|h| h.max(0.0)).collect();
    let ground = box_blur_rows(&ground, n, TERRAIN_BLUR_CELLS, true);
    let ground = box_blur_rows(&ground, n, TERRAIN_BLUR_CELLS, false);
    // `World::edges` order: N, E, S, W.
    let inflow = |edge: usize| if w.edges[edge] == EdgeKind::Sea { 1.0 } else { LAND_EDGE_INFLOW_MOISTURE };
    let recharge = (w.dx / k.sea_recharge_m).min(1.0);

    let rows: Vec<Vec<f64>> = par::map_range(n, |z| {
        let mut row = vec![0.0; n];
        let from_west = wind(latitude_deg[z] as f64, p) == Wind::FromWest;
        let mut moisture: f64 = if from_west { inflow(3) } else { inflow(1) };
        let mut prev: Option<usize> = None;
        for step in 0..n {
            let x = if from_west { step } else { n - 1 - step };
            let i = z * n + x;
            if w.ocean[i] {
                moisture += (1.0 - moisture) * recharge;
            } else {
                // A lake rains like flat land, then recharges the air.
                let rise = match prev {
                    Some(j) if !lakes[i] => (ground[i] - ground[j]).max(0.0),
                    _ => 0.0,
                };
                let fallout = moisture * (w.dx / k.land_depletion_m + rise / k.orographic_scale_m).min(1.0);
                moisture -= fallout;
                row[x] = fallout / w.dx;
                if lakes[i] {
                    moisture += (1.0 - moisture) * recharge;
                }
            }
            prev = Some(i);
        }
        row
    });
    // Blur over the land only (the sea's zero rain would dry the coasts).
    let rain: Vec<f64> = rows.concat();
    let land: Vec<f64> = w.ocean.iter().map(|&o| if o { 0.0 } else { 1.0 }).collect();
    let blur = |v: &[f64]| box_blur_rows(&box_blur_rows(v, n, RAIN_BLUR_CELLS, false), n, RAIN_ALONG_BLUR_CELLS, true);
    let (rain_sum, land_share) = (blur(&rain), blur(&land));
    let mut rain: Vec<f64> =
        rain_sum.iter().zip(&land_share).map(|(&r, &l)| if l > 0.0 { r / l } else { 0.0 }).collect();

    let (mut sum, mut land) = (0.0, 0usize);
    for i in 0..n * n {
        if !w.ocean[i] {
            sum += rain[i];
            land += 1;
        }
    }
    let mean = (sum / land.max(1) as f64).max(f64::MIN_POSITIVE);
    for (i, r) in rain.iter_mut().enumerate() {
        *r = if w.ocean[i] {
            1.0
        } else {
            let relative = (*r / mean).clamp(k.rain_min, k.rain_max.max(k.rain_min));
            1.0 + k.climate_strength.min(1.0) * (relative - 1.0)
        };
    }
    rain
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

    const N: usize = 64;
    const CELL_M: f64 = 128.0;

    /// Params for an `N`-cell world at `CELL_M`, one latitude everywhere
    /// (`pole` none) unless the caller changes it.
    fn params(latitude: f64) -> ForgeParams {
        let mut p = ForgeParams { size_m: N as f64 * CELL_M, cell_m: CELL_M, quiet: true, ..Default::default() };
        p.knobs.pole = POLE_NONE as f64;
        p.knobs.latitude_center_deg = latitude;
        p
    }

    /// A hand-made world: `height(x, z)`, ocean where it is <= 0, sea on
    /// every edge.
    fn world(height: impl Fn(usize, usize) -> f64) -> World {
        let h: Vec<f64> = (0..N * N).map(|i| height(i % N, i / N)).collect();
        let ocean: Vec<bool> = h.iter().map(|&h| h <= 0.0).collect();
        World {
            n: N,
            dx: CELL_M,
            outlet: ocean.clone(),
            ocean,
            uplift: vec![0.0; N * N],
            uplifted: vec![0.0; N * N],
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

    #[test]
    fn precip_follows_the_latitude_table_on_flat_land() {
        let mut p = params(25.0);
        // Air hardly rains out over flat land, so every cell sees the base.
        p.knobs.land_depletion_m = 60_000.0;
        // A low plain (a higher one rains on its coast: the air rises onto it).
        let w = world(|x, _| if x < 4 || x >= N - 4 { -50.0 } else { 2.0 });
        let c = compute(&w, &p);
        for i in (0..N * N).filter(|&i| !w.ocean[i]) {
            assert!((c.precip_mm[i] - 350.0).abs() <= 60.0, "cell {i}: {} mm", c.precip_mm[i]);
        }
        assert_eq!(base_precip_mm(17.5), (1800.0 + 350.0) / 2.0);
        assert_eq!(base_precip_mm(-90.0), 200.0);
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
