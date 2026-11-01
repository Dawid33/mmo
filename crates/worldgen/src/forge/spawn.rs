//! Where players start: the forge picks the site, because a finite world has
//! one spawn worth choosing (the procedural world had to scan SEEDS for one).
//! Flat dry ground near a river and the sea, with mountains inside the ~2 km
//! LOD horizon, so the first view shows what the world is.

use std::collections::VecDeque;

use crate::forge::hydrology::{Analysis, LAKE_MIN_DEPTH_M};
use crate::forge::world::{hash01, smoothstep, World};

/// Height band for a spawn site (m above sea): the coastal lowlands below
/// kilometre-plus ranges reach a hundred metres and more.
const SPAWN_HEIGHT_M: (f64, f64) = (3.0, 120.0);
/// The ground within this many cells must be this flat (max - min, m).
const FLAT_RADIUS_CELLS: i64 = 3;
const FLAT_MAX_RELIEF_M: f64 = 4.0;
/// A river worth standing next to drains at least this much (m^2 of rain).
const RIVER_DISCHARGE_M2: f64 = 1.5e6;
/// "Mountain" for the view criterion: this share of the world's highest peak.
const MOUNTAIN_SHARE_OF_PEAK: f64 = 0.55;
/// A scored site keeps this far from the map edge (where a land edge ends the
/// playable world), or `EDGE_CLEARANCE_MAX_SHARE` of a small world's size.
const EDGE_CLEARANCE_M: f64 = 1_000.0;
const EDGE_CLEARANCE_MAX_SHARE: f64 = 0.125;

/// Grid distance (m, 8-connected steps approximated as 1 cell) to the nearest
/// cell where `source` holds.
fn distance_field(w: &World, source: impl Fn(usize) -> bool) -> Vec<f64> {
    let n = w.n;
    let mut dist = vec![f64::INFINITY; n * n];
    let mut queue = VecDeque::new();
    for i in 0..n * n {
        if source(i) {
            dist[i] = 0.0;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        for (nb, _) in w.neighbors(i) {
            if dist[nb].is_infinite() {
                dist[nb] = dist[i] + w.dx;
                queue.push_back(nb);
            }
        }
    }
    dist
}

/// Best spawn site in world meters. If no site meets every criterion (a
/// rugged world: no patch flat enough in the height band), the flattest dry
/// patch of land at least `SPAWN_HEIGHT_M.0` above the sea instead.
pub fn pick(w: &World, a: &Analysis, seed: u64) -> (f64, f64) {
    let n = w.n as i64;
    let peak = w.h.iter().copied().fold(f64::MIN, f64::max);
    let to_river = distance_field(w, |i| !w.ocean[i] && a.q[i] >= RIVER_DISCHARGE_M2);
    let to_sea = distance_field(w, |i| w.ocean[i]);
    let to_mountain = distance_field(w, |i| w.h[i] >= MOUNTAIN_SHARE_OF_PEAK * peak);

    let clearance = EDGE_CLEARANCE_M.min(EDGE_CLEARANCE_MAX_SHARE * w.n as f64 * w.dx);
    let mut best = (f64::MIN, 0usize);
    // Flattest dry patch (its relief, cell) for when no site qualifies.
    // ... preferring one clear of the map edge (the plain along a land edge
    // is flat because it is not uplifted, and it is where the world ends).
    let mut flattest = (f64::MAX, None);
    let mut flattest_clear = (f64::MAX, None);
    let size = w.n as f64 * w.dx;
    for z in FLAT_RADIUS_CELLS..n - FLAT_RADIUS_CELLS {
        for x in FLAT_RADIUS_CELLS..n - FLAT_RADIUS_CELLS {
            let i = (z * n + x) as usize;
            if w.ocean[i] || w.h[i] < SPAWN_HEIGHT_M.0 {
                continue;
            }
            let (mut lo, mut hi, mut dry) = (f64::MAX, f64::MIN, true);
            for oz in -FLAT_RADIUS_CELLS..=FLAT_RADIUS_CELLS {
                for ox in -FLAT_RADIUS_CELLS..=FLAT_RADIUS_CELLS {
                    let j = ((z + oz) * n + x + ox) as usize;
                    lo = lo.min(w.h[j]);
                    hi = hi.max(w.h[j]);
                    dry &= !w.ocean[j] && a.lake_depth[j] < LAKE_MIN_DEPTH_M && a.q[j] < RIVER_DISCHARGE_M2;
                }
            }
            if !dry {
                continue;
            }
            let (cx, cz) = w.center(i);
            let clear = cx.min(cz).min(size - cx).min(size - cz) >= clearance;
            if hi - lo < flattest.0 {
                flattest = (hi - lo, Some(i));
            }
            if clear && hi - lo < flattest_clear.0 {
                flattest_clear = (hi - lo, Some(i));
            }
            if hi - lo > FLAT_MAX_RELIEF_M || w.h[i] > SPAWN_HEIGHT_M.1 {
                continue;
            }
            if !clear {
                continue;
            }
            // Near, not in: a river 60-300 m away, the sea within ~1.5 km,
            // a mountain within ~2.5 km.
            let score = smoothstep(40.0, 80.0, to_river[i]) * (1.0 - smoothstep(250.0, 600.0, to_river[i]))
                + 0.7 * (1.0 - smoothstep(800.0, 2_500.0, to_sea[i]))
                + (1.0 - smoothstep(1_500.0, 4_000.0, to_mountain[i]))
                + 0.05 * hash01(seed, i as u64, 0x5A) ;
            if score > best.0 {
                best = (score, i);
            }
        }
    }
    if best.0 > f64::MIN {
        return w.center(best.1);
    }
    w.center(flattest_clear.1.or(flattest.1).expect("no dry land for a spawn site"))
}
