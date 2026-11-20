//! Shaping passes applied once, after the landscape evolution: valley floors
//! and lake basins — features the erosion law does not make on its own.

use crate::forge::erosion::{discharge, route, FLOOD_EPSILON};
use crate::forge::knobs::Knobs;
use crate::forge::world::{hash01, smoothstep, World};

// Valley floors: half-width `floodplain_m_per_sqrt_km2` per sqrt(km^2) of
// discharge (a river draining 16 km^2 gets a ~110 m wide flat floor), capped
// at `floodplain_max_m`. Stream-power incision alone cuts V-shaped valleys one
// cell wide; rivers really plane their floors sideways.
/// Floors are only made for rivers at least this big (m^2 of average rain).
const FLOODPLAIN_MIN_DISCHARGE_M2: f64 = 1.0e6;
/// Gentle rise of the floor away from the channel (rise/run).
const FLOODPLAIN_CROSS_SLOPE: f64 = 0.012;

/// Overdeepened basins excavated along valley floors (the way glaciers and
/// subsidence make lake basins; a fluvial steady state has no closed
/// depressions of its own). Priority-Flood then floods each one to its true
/// spill level, so every lake is flat, has an outlet, and the river carries on.
/// Count, spacing, largest radius and depth are knobs ("Shaping").
/// Candidate sites: valley floors with a discharge in this range (m^2)...
const LAKE_SITE_DISCHARGE_M2: (f64, f64) = (0.7e6, 2.5e7);
/// ...and this far above the sea.
const LAKE_SITE_MIN_HEIGHT_M: f64 = 6.0;
const LAKE_BASIN_MIN_RADIUS_M: f64 = 140.0;
const LAKE_BASIN_MIN_DEPTH_M: f64 = 5.0;

// `Knobs::max_peak_m`: the tallest peak allowed (default 2,800 m, at most
// 3,500 m: forged terrain is generated up to block 4096, and the map colour
// ramps and tool height scales end at 3,500 / 3,600 m).

/// Scale land heights down if the simulated relief overshoots `max_peak_m`.
/// Simulated relief is only predictable to ~30% (it depends on which rock the
/// ranges happen to exhume, and on grid resolution), so the forge FITS it
/// rather than hoping. A uniform vertical scale keeps every drainage path,
/// lake and divide exactly where it is; slopes shrink by the same factor.
/// Returns the factor applied (1.0 = untouched).
pub fn fit_height_band(w: &mut World, k: &Knobs) -> f64 {
    let max = w.h.iter().copied().fold(f64::MIN, f64::max);
    if max <= k.max_peak_m {
        return 1.0;
    }
    let factor = k.max_peak_m / max;
    for i in 0..w.cells() {
        if !w.ocean[i] {
            // `uplifted` scales with it, so the rock column stays attached to
            // the surface it was exhumed at.
            w.h[i] *= factor;
            w.uplifted[i] *= factor;
        }
    }
    factor
}

/// Plane a flat floor beside every sizeable river, easing into the valley wall.
pub fn carve_floodplains(w: &mut World, rain: &[f64], k: &Knobs) {
    if k.floodplain_m_per_sqrt_km2 <= 0.0 || k.floodplain_max_m <= 0.0 {
        return;
    }
    let routing = route(w, FLOOD_EPSILON);
    let q = discharge(w, &routing, rain);
    let (nx, nz) = (w.nx, w.nz);
    let mut floor = w.h.clone();
    for &c in &routing.order {
        let c = c as usize;
        if q[c] < FLOODPLAIN_MIN_DISCHARGE_M2 {
            continue;
        }
        let reach = (k.floodplain_m_per_sqrt_km2 * (q[c] / 1.0e6).sqrt()).min(k.floodplain_max_m);
        let r_cells = (reach / w.dx).ceil() as i64;
        let (cx, cz) = ((c % nx) as i64, (c / nx) as i64);
        for oz in -r_cells..=r_cells {
            for ox in -r_cells..=r_cells {
                let (x, z) = (cx + ox, cz + oz);
                // x wraps round on a wrapping world.
                let x = if w.wrap_x { x.rem_euclid(nx as i64) } else { x };
                if x < 0 || z < 0 || x >= nx as i64 || z >= nz as i64 {
                    continue;
                }
                let j = z as usize * nx + x as usize;
                let d = ((ox * ox + oz * oz) as f64).sqrt() * w.dx;
                if d > reach || w.ocean[j] {
                    continue;
                }
                let level = w.h[c] + FLOODPLAIN_CROSS_SLOPE * d;
                if w.h[j] > level {
                    // Full planation near the channel, fading out by the edge.
                    let keep = smoothstep(0.55, 1.0, d / reach);
                    floor[j] = floor[j].min(level + keep * (w.h[j] - level));
                }
            }
        }
    }
    w.h = floor;
}

/// Excavate LAKE_BASINS bowls on valley floors; returns how many were placed.
pub fn carve_lake_basins(w: &mut World, rain: &[f64], seed: u64, k: &Knobs) -> usize {
    let wanted = k.lake_basins.round().max(0.0) as usize;
    if wanted == 0 {
        return 0;
    }
    let routing = route(w, FLOOD_EPSILON);
    let q = discharge(w, &routing, rain);
    // Candidates in a hashed order, so sites are spread over all rivers
    // instead of clustering on the biggest one.
    let mut candidates: Vec<(u64, usize)> = routing
        .order
        .iter()
        .map(|&i| i as usize)
        .filter(|&i| {
            (LAKE_SITE_DISCHARGE_M2.0..=LAKE_SITE_DISCHARGE_M2.1).contains(&q[i])
                && w.h[i] >= LAKE_SITE_MIN_HEIGHT_M
        })
        .map(|i| {
            let (x, z) = w.center(i);
            ((hash01(seed, x as u64, z as u64) * 1.0e15) as u64, i)
        })
        .collect();
    candidates.sort_unstable();

    let (nx, nz) = (w.nx, w.nz);
    let mut sites: Vec<usize> = Vec::new();
    for (_, i) in candidates {
        let (x, z) = w.center(i);
        let width = w.nx as f64 * w.dx;
        let far_enough = sites.iter().all(|&s| {
            let (sx, sz) = w.center(s);
            // The shorter way round on a wrapping world.
            let ex = if w.wrap_x { (sx - x).abs().min(width - (sx - x).abs()) } else { sx - x };
            (ex.powi(2) + (sz - z).powi(2)).sqrt() >= k.lake_spacing_m
        });
        if far_enough {
            sites.push(i);
            if sites.len() == wanted {
                break;
            }
        }
    }
    for (n_site, &site) in sites.iter().enumerate() {
        let t = hash01(seed, n_site as u64, 0x1A4E);
        let radius = LAKE_BASIN_MIN_RADIUS_M + t * (k.lake_radius_max_m - LAKE_BASIN_MIN_RADIUS_M).max(0.0);
        let depth = LAKE_BASIN_MIN_DEPTH_M
            + hash01(seed, n_site as u64, 0x1A4F) * (k.lake_depth_max_m - LAKE_BASIN_MIN_DEPTH_M).max(0.0);
        let (cx, cz) = ((site % nx) as i64, (site / nx) as i64);
        let r_cells = (radius / w.dx).ceil() as i64;
        for oz in -r_cells..=r_cells {
            for ox in -r_cells..=r_cells {
                let (x, z) = (cx + ox, cz + oz);
                // Never on the border; x wraps round on a wrapping world.
                let x = if w.wrap_x { x.rem_euclid(nx as i64) } else { x };
                let off_x = !w.wrap_x && (x < 1 || x >= nx as i64 - 1);
                if off_x || z < 1 || z >= nz as i64 - 1 {
                    continue;
                }
                let j = z as usize * nx + x as usize;
                let d = ((ox * ox + oz * oz) as f64).sqrt() * w.dx / radius;
                if d >= 1.0 || w.ocean[j] {
                    continue;
                }
                // Smooth bowl; never dug below the sea, so lakes stay fresh.
                let bowl = (1.0 - d * d) * (1.0 - d * d);
                w.h[j] = (w.h[j] - depth * bowl).max(0.5);
            }
        }
    }
    sites.len()
}
