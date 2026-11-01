//! Derived per-cell fields of a forged world on DF's 0..100 scales —
//! drainage, volcanism, salinity — plus the volcano sites. They feed the
//! biome table (drainage thresholds 33 / 66, salinity >= 66 for mangrove) and
//! are stored in the world file. Computed once, on the final grid, after the
//! hydrology (`compute`); only the volcano SITES are needed earlier, by
//! `World::new` (each is a cone of uplift), so they come from the seed and
//! knobs alone on a fixed reference grid (`volcano_sites`) and are the same at
//! every level of the coarse-to-fine schedule.
//!
//! - **Drainage** (`drainage`): how fast ground sheds water. High on slopes
//!   and on permeable rock (sandstone, limestone, conglomerate), low on shale
//!   and on flat ground standing within `DRAINAGE_FLOOD_M` of the nearest river
//!   or lake surface — floodplains, and deltas where rivers reach the sea.
//!   Slopes are never swamp-grade (< 33): see `drainage_at`.
//! - **Volcanism** (`volcanism`): a continental-scale noise base that never
//!   reaches the top on its own, plus hot spots on the flanks of convergent
//!   plate boundaries. Volcano sites are cells at 100, `VOLCANO_SPACING_M`
//!   apart. Only the continental layouts have plates: island layouts get the
//!   base noise and no volcanoes, so their terrain is untouched.
//! - **Salinity** (`salinity`): 100 at sea and on closed (endorheic) lakes,
//!   0 on open lakes, falling from 100 to 0 over `SALT_REACH_M` inland of
//!   either. Every forged lake has a spill path (the Priority-Flood fills each
//!   depression to its spill level), so "closed" is decided by water balance
//!   instead: a lake whose rain-weighted inflow is under
//!   `ENDORHEIC_INFLOW_RATIO` times its own surface loses it all to
//!   evaporation and never spills.

use crate::forge::geology::{rock_index, Geology};
use crate::forge::hydrology::{Analysis, LAKE_MIN_DEPTH_M};
use crate::forge::knobs::Knobs;
use crate::forge::par;
use crate::forge::plates::{self, PlateField};
use crate::forge::world::{hash01, smoothstep, ForgeParams, World, REFERENCE_SIZE_M};
use crate::noise::OctaveNoise;

/// Flat ground at most this high above the nearest river or lake surface is
/// floodplain: poorly drained.
pub const DRAINAGE_FLOOD_M: f64 = 4.0;
/// The floodplain effect fades out over this much more height above the water.
pub const DRAINAGE_FLOOD_FADE_M: f64 = 4.0;
/// ... and with horizontal distance from the water, from this far to twice it
/// (a plain 3 m above a river 10 km away is not its floodplain).
pub const DRAINAGE_FLOOD_REACH_M: f64 = 1_500.0;
/// Slope (rise/run) at and above which ground drains fully by run-off.
pub const DRAINAGE_SLOPE_FULL: f64 = 0.2;
/// Drainage of flat, dry, impermeable ground (shale), 0..1.
pub const DRAINAGE_BASE: f64 = 0.35;
/// Added at full permeability, and at full slope.
pub const DRAINAGE_PERMEABILITY_WEIGHT: f64 = 0.4;
pub const DRAINAGE_SLOPE_WEIGHT: f64 = 0.5;
/// Share of the drainage a floodplain takes away on flat ground (fading with
/// the square of the slope share, so a floodplain's valley walls stay dry).
pub const DRAINAGE_FLOOD_DAMP: f64 = 0.7;
/// Rock permeability 0..1 by rock name (`geology::ROCKS`): sandstone,
/// limestone and conglomerate drain, shale does not, crystalline rock drains
/// only through its joints.
pub const PERMEABILITY: [(&str, f64); 6] = [
    ("sandstone", 1.0),
    ("shale", 0.0),
    ("limestone", 1.0),
    ("conglomerate", 0.9),
    ("basalt", 0.5),
    ("granite", 0.35),
];

/// Volcano sites keep at least this far apart (m).
pub const VOLCANO_SPACING_M: f64 = 8_000.0;
/// Radius of a volcano's cone of uplift (m): uplift falls linearly from the
/// full belt rate at the site to nothing here.
pub const VOLCANO_CONE_RADIUS_M: f64 = 3_000.0;
/// Volcano sites are picked on a grid of this cell size (m), whatever the
/// forge's cell: the sites, and so the cones, are the same at every level of
/// the erosion schedule.
pub const VOLCANO_GRID_M: f64 = 250.0;
/// A site must be at least this far inland (landness, `World::new`'s units)…
pub const VOLCANO_MIN_LANDNESS: f64 = 0.15;
/// Volcanism's continental noise base reaches at most this (0..1), so only
/// the hot spots make 100.
pub const VOLCANISM_BASE_MAX: f64 = 0.45;
/// Hot-spot gain: a hot spot at full strength adds this (0..1 units).
pub const VOLCANISM_HOTSPOT_GAIN: f64 = 1.3;
/// Hot spots sit on the flanks of convergent belts: they ramp in between
/// these convergences …
pub const VOLCANISM_FLANK: (f64, f64) = (0.1, 0.35);
/// … and fall by `VOLCANISM_CORE_DAMP` toward the belt's core between these,
/// so a volcano's cone stands out instead of hiding in the range's crest.
pub const VOLCANISM_CORE: (f64, f64) = (0.6, 0.95);
pub const VOLCANISM_CORE_DAMP: f64 = 0.7;
/// Hot-spot noise (0..1) between these maps onto 0..1 strength.
pub const VOLCANISM_SPOT: (f64, f64) = (0.35, 0.6);

/// A lake whose rain-weighted inflow (m^2, `Analysis::q`) is under this many
/// times its own surface is closed: evaporation takes all of it.
pub const ENDORHEIC_INFLOW_RATIO: f64 = 3.0;
/// Salt spray / salt flats reach this far inland of salt water (m).
pub use crate::habitat::SALT_REACH_M;

/// Every derived field of a forged world, as stored in the world file.
pub struct Fields {
    pub drainage: Vec<u8>,
    pub volcanism: Vec<u8>,
    pub salinity: Vec<u8>,
    /// Volcano sites, world metres (cell centres of the reference grid).
    pub volcanoes: Vec<[f32; 2]>,
}

/// All fields for the final world.
pub fn compute(p: &ForgeParams, w: &World, a: &Analysis, geo: &Geology) -> Fields {
    let k = &p.knobs;
    let plates = if k.is_continental() {
        plates::plate_field(p.seed, p.size_m, w.n, k)
    } else {
        PlateField { plate: vec![0; w.n * w.n], convergence: vec![0.0; w.n * w.n] }
    };
    Fields {
        drainage: drainage(w, a, geo),
        volcanism: volcanism(p.seed, w, &plates, k),
        salinity: salinity(w, a),
        volcanoes: crate::forge::world::volcano_sites(p),
    }
}

fn to_u8(v: f64) -> u8 {
    (v * 100.0).round().clamp(0.0, 100.0) as u8
}

// ---- drainage ----

/// Permeability of rock `id` (index into `geology::ROCKS`).
pub fn permeability(id: u8) -> f64 {
    PERMEABILITY
        .iter()
        .find(|(name, _)| rock_index(name) == Some(id as usize))
        .map_or(0.5, |&(_, p)| p)
}

/// Drainage 0..1 of one cell: `slope` rise/run, `permeability` 0..1, and
/// `above_water` (m) above the nearest river or lake surface `water_dist` m
/// away (infinite: none). Flat floodplain ground is < 0.33 on any rock; from
/// a slope of 0.3 * `DRAINAGE_SLOPE_FULL` up nothing is, whatever the rock
/// or water: dry = base + perm + slope shares, times 1 - damp * flood *
/// (1 - slope share)^2.
pub fn drainage_at(slope: f64, permeability: f64, above_water: f64, water_dist: f64) -> f64 {
    let sl = (slope / DRAINAGE_SLOPE_FULL).clamp(0.0, 1.0);
    let dry = (DRAINAGE_BASE + DRAINAGE_PERMEABILITY_WEIGHT * permeability + DRAINAGE_SLOPE_WEIGHT * sl).min(1.0);
    let flood = (1.0 - smoothstep(DRAINAGE_FLOOD_M, DRAINAGE_FLOOD_M + DRAINAGE_FLOOD_FADE_M, above_water))
        * (1.0 - smoothstep(DRAINAGE_FLOOD_REACH_M, 2.0 * DRAINAGE_FLOOD_REACH_M, water_dist));
    dry * (1.0 - DRAINAGE_FLOOD_DAMP * flood * (1.0 - sl) * (1.0 - sl))
}

/// Water surface per cell: lakes at their level, rivers (discharge at least
/// the analysis' river threshold) at their routed water surface, NaN
/// elsewhere (the sea included).
pub fn water_levels(w: &World, a: &Analysis) -> Vec<f64> {
    let river = a.stats.river_min_discharge_m2;
    (0..w.n * w.n)
        .map(|i| {
            if w.ocean[i] {
                f64::NAN
            } else if a.lake_depth[i] >= LAKE_MIN_DEPTH_M {
                w.h[i] + a.lake_depth[i]
            } else if a.q[i] >= river {
                a.routing.filled[i].max(w.h[i])
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// Drainage 0..100 per cell (sea and lakes: 0).
pub fn drainage(w: &World, a: &Analysis, geo: &Geology) -> Vec<u8> {
    let rock: Vec<u8> = (0..w.n * w.n).map(|i| if w.ocean[i] { 0 } else { geo.surface_rock(w, i) }).collect();
    drainage_from(w, &water_levels(w, a), &rock)
}

/// For each cell, the distance (m) to the nearest cell with a water level
/// (not NaN) and that level: a two-pass chamfer carrying the source's level.
/// (INFINITY, NaN) when there is no water at all.
fn nearest_water(n: usize, dx: f64, level: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let mut dist: Vec<f64> = level.iter().map(|l| if l.is_nan() { f64::INFINITY } else { 0.0 }).collect();
    let mut at = level.to_vec();
    let diag = dx * std::f64::consts::SQRT_2;
    let relax = |dist: &mut Vec<f64>, at: &mut Vec<f64>, x: usize, z: usize, ox: isize, oz: isize, step: f64| {
        let (nx, nz) = (x as isize + ox, z as isize + oz);
        if nx >= 0 && nz >= 0 && (nx as usize) < n && (nz as usize) < n {
            let j = nz as usize * n + nx as usize;
            let i = z * n + x;
            if dist[j] + step < dist[i] {
                dist[i] = dist[j] + step;
                at[i] = at[j];
            }
        }
    };
    for z in 0..n {
        for x in 0..n {
            relax(&mut dist, &mut at, x, z, -1, 0, dx);
            relax(&mut dist, &mut at, x, z, 0, -1, dx);
            relax(&mut dist, &mut at, x, z, -1, -1, diag);
            relax(&mut dist, &mut at, x, z, 1, -1, diag);
        }
    }
    for z in (0..n).rev() {
        for x in (0..n).rev() {
            relax(&mut dist, &mut at, x, z, 1, 0, dx);
            relax(&mut dist, &mut at, x, z, 0, 1, dx);
            relax(&mut dist, &mut at, x, z, 1, 1, diag);
            relax(&mut dist, &mut at, x, z, -1, 1, diag);
        }
    }
    (dist, at)
}

/// Core of `drainage`: `water_level` is the water surface of a river or lake
/// cell, NaN elsewhere; `rock` the surface rock per cell. Water cells (and
/// the sea) are 0.
pub fn drainage_from(w: &World, water_level: &[f64], rock: &[u8]) -> Vec<u8> {
    let (n, dx) = (w.n, w.dx);
    let (dist, level) = nearest_water(n, dx, water_level);
    // Land height for slopes: the sea reads as sea level.
    let ground = |x: usize, z: usize| w.h[z * n + x].max(0.0);
    par::map_range(n * n, |i| {
        if w.ocean[i] || !water_level[i].is_nan() {
            return 0;
        }
        let (x, z) = (i % n, i / n);
        let (x0, x1, z0, z1) = (x.saturating_sub(1), (x + 1).min(n - 1), z.saturating_sub(1), (z + 1).min(n - 1));
        let gx = (ground(x1, z) - ground(x0, z)) / (((x1 - x0).max(1)) as f64 * dx);
        let gz = (ground(x, z1) - ground(x, z0)) / (((z1 - z0).max(1)) as f64 * dx);
        let above = if level[i].is_nan() { f64::INFINITY } else { w.h[i] - level[i] };
        to_u8(drainage_at(gx.hypot(gz), permeability(rock[i]), above, dist[i]))
    })
}

// ---- volcanism ----

/// Volcanism's noises: the continental base and the hot spots, sampled at
/// the world's scale (`REFERENCE_SIZE_M`), so a bigger world is the same
/// pattern scaled up.
struct VolcanismNoise {
    base: OctaveNoise,
    spots: OctaveNoise,
    scale: f64,
}

impl VolcanismNoise {
    fn new(seed: u64, size_m: f64) -> VolcanismNoise {
        VolcanismNoise {
            base: OctaveNoise::new(seed ^ 0xF201, -14, vec![1.0, 0.5]), // 16 km at the reference size
            spots: OctaveNoise::new(seed ^ 0xF202, -11, vec![1.0, 0.5]), // 2 km
            scale: size_m / REFERENCE_SIZE_M,
        }
    }

    /// Volcanism 0..1 (unclamped above) at (mx, mz) m with plate-belt
    /// `convergence` there.
    fn at(&self, mx: f64, mz: f64, convergence: f64, k: &Knobs) -> f64 {
        let (sx, sz) = (mx / self.scale, mz / self.scale);
        let base = VOLCANISM_BASE_MAX * (0.5 + 0.5 * self.base.sample2(sx, sz)).clamp(0.0, 1.0);
        let flank = smoothstep(VOLCANISM_FLANK.0, VOLCANISM_FLANK.1, convergence)
            * (1.0 - VOLCANISM_CORE_DAMP * smoothstep(VOLCANISM_CORE.0, VOLCANISM_CORE.1, convergence));
        let spot = smoothstep(VOLCANISM_SPOT.0, VOLCANISM_SPOT.1, 0.5 + 0.5 * self.spots.sample2(sx, sz));
        k.volcanism_strength.max(0.0) * (base + VOLCANISM_HOTSPOT_GAIN * flank * spot)
    }
}

/// Volcanism 0..100 on an `n` x `n` grid over a `size_m` world.
pub fn volcanism_grid(seed: u64, size_m: f64, n: usize, plates: &PlateField, k: &Knobs) -> Vec<u8> {
    let noise = VolcanismNoise::new(seed, size_m);
    let cell = size_m / n as f64;
    par::map_range(n * n, |i| {
        let (mx, mz) = (((i % n) as f64 + 0.5) * cell, ((i / n) as f64 + 0.5) * cell);
        to_u8(noise.at(mx, mz, plates.convergence[i] as f64, k))
    })
}

/// Volcanism 0..100 per cell of `w`: continental-scale noise plus hot spots
/// along the convergent boundaries of `plates` (all-zero convergence for the
/// island layouts: base only, never 100).
pub fn volcanism(seed: u64, w: &World, plates: &PlateField, k: &Knobs) -> Vec<u8> {
    volcanism_grid(seed, w.n as f64 * w.dx, w.n, plates, k)
}

/// Volcano sites: cells of `volcanism` (a square grid of `cell_m` cells) at
/// 100 that are `eligible`, taken in a seeded random order, each kept only if
/// `VOLCANO_SPACING_M` from every earlier one, at most
/// `Knobs::volcano_count_max`. Cell centres, world metres.
pub fn volcanoes(seed: u64, volcanism: &[u8], eligible: &[bool], cell_m: f64, k: &Knobs) -> Vec<[f32; 2]> {
    let n = (volcanism.len() as f64).sqrt().round() as usize;
    let max = k.volcano_count_max.round().max(0.0) as usize;
    let mut candidates: Vec<usize> = (0..volcanism.len()).filter(|&i| volcanism[i] >= 100 && eligible[i]).collect();
    candidates.sort_by(|&a, &b| hash01(seed, 0x701C, a as u64).total_cmp(&hash01(seed, 0x701C, b as u64)).then(a.cmp(&b)));
    let mut sites: Vec<[f64; 2]> = Vec::new();
    for i in candidates {
        if sites.len() >= max {
            break;
        }
        let p = [((i % n) as f64 + 0.5) * cell_m, ((i / n) as f64 + 0.5) * cell_m];
        if sites.iter().all(|s| (s[0] - p[0]).hypot(s[1] - p[1]) >= VOLCANO_SPACING_M) {
            sites.push(p);
        }
    }
    sites.into_iter().map(|[x, z]| [x as f32, z as f32]).collect()
}

/// The volcano sites of a continental world from its seed and knobs only, on
/// the `VOLCANO_GRID_M` reference grid; `eligible(mx, mz)` says whether a
/// site may stand at a point (inland, clear of land edges: `World::new`
/// decides). Empty when `volcano_count_max` is 0.
pub fn volcano_sites_on(p: &ForgeParams, eligible: &dyn Fn(f64, f64) -> bool) -> Vec<[f32; 2]> {
    let k = &p.knobs;
    if k.volcano_count_max.round() < 1.0 {
        return Vec::new();
    }
    let n = ((p.size_m / VOLCANO_GRID_M).round() as usize).max(8);
    let cell = p.size_m / n as f64;
    let field = plates::plate_field(p.seed, p.size_m, n, k);
    let v = volcanism_grid(p.seed, p.size_m, n, &field, k);
    let ok: Vec<bool> =
        (0..n * n).map(|i| eligible(((i % n) as f64 + 0.5) * cell, ((i / n) as f64 + 0.5) * cell)).collect();
    volcanoes(p.seed, &v, &ok, cell, k)
}

/// Cone of uplift at (mx, mz): 1 on a site, falling linearly to 0 at
/// `VOLCANO_CONE_RADIUS_M`; the largest of all sites.
pub fn cone(sites: &[[f32; 2]], mx: f64, mz: f64) -> f64 {
    sites
        .iter()
        .map(|s| 1.0 - ((s[0] as f64 - mx).hypot(s[1] as f64 - mz) / VOLCANO_CONE_RADIUS_M).min(1.0))
        .fold(0.0, f64::max)
}

// ---- salinity ----

/// Closed (endorheic) lake cells: 4-connected bodies of lake cells (at least
/// `LAKE_MIN_DEPTH_M` deep, not sea) whose inflow — the largest discharge
/// `q` (m^2, rain-weighted area) in the body, i.e. what leaves through its
/// outlet — is under `ENDORHEIC_INFLOW_RATIO` times the body's surface.
pub fn closed_lakes(w: &World, lake_depth: &[f64], q: &[f64]) -> Vec<bool> {
    let n = w.n;
    let is_lake = |i: usize| !w.ocean[i] && lake_depth[i] >= LAKE_MIN_DEPTH_M;
    let mut seen = vec![false; n * n];
    let mut closed = vec![false; n * n];
    for start in 0..n * n {
        if seen[start] || !is_lake(start) {
            continue;
        }
        let (mut stack, mut cells) = (vec![start], Vec::new());
        seen[start] = true;
        while let Some(i) = stack.pop() {
            cells.push(i);
            let (x, z) = (i % n, i / n);
            let mut visit = |j: usize| {
                if !seen[j] && is_lake(j) {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 { visit(i - 1); }
            if x + 1 < n { visit(i + 1); }
            if z > 0 { visit(i - n); }
            if z + 1 < n { visit(i + n); }
        }
        let inflow = cells.iter().map(|&i| q[i]).fold(0.0, f64::max);
        let area = cells.len() as f64 * w.dx * w.dx;
        if inflow < ENDORHEIC_INFLOW_RATIO * area {
            for &i in &cells {
                closed[i] = true;
            }
        }
    }
    closed
}

/// Core of `salinity`: 100 at sea and on `closed` cells, falling linearly to
/// 0 at `SALT_REACH_M` from their shore; 0 elsewhere (open lakes included).
/// The shore lies half a cell from a shore cell's centre, so the taper reads
/// the same at any cell size.
pub fn salinity_from(w: &World, closed: &[bool]) -> Vec<u8> {
    let salt: Vec<f32> = (0..w.n * w.n).map(|i| if w.ocean[i] || closed[i] { -1.0 } else { 1.0 }).collect();
    let dist = crate::habitat::coast_distance_m(&salt, w.n, w.dx);
    dist.iter()
        .map(|&d| if d <= 0.0 { 100 } else { to_u8(1.0 - (d as f64 - 0.5 * w.dx) / SALT_REACH_M as f64) })
        .collect()
}

/// Salinity 0..100 per cell: the sea and closed lakes are salt, open lakes
/// fresh, the shores of either taper over `SALT_REACH_M`.
pub fn salinity(w: &World, a: &Analysis) -> Vec<u8> {
    salinity_from(w, &closed_lakes(w, &a.lake_depth, &a.q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::world::EdgeKind;

    /// A flat world of `n` x `n` cells at `dx` m, `h` per cell, no sea.
    fn world(n: usize, dx: f64, h: impl Fn(usize, usize) -> f64) -> World {
        let h: Vec<f64> = (0..n * n).map(|i| h(i % n, i / n)).collect();
        World {
            n,
            dx,
            h,
            ocean: vec![false; n * n],
            uplift: vec![0.0; n * n],
            uplifted: vec![0.0; n * n],
            margin_drowned: 0,
            edges: [EdgeKind::Land; 4],
            outlet: vec![false; n * n],
        }
    }

    fn rock(name: &str) -> u8 {
        rock_index(name).unwrap() as u8
    }

    /// A river runs down column 10 of a flat plain 1 m above it: the plain
    /// beside it is poorly drained, whatever the rock.
    #[test]
    fn floodplain_is_poorly_drained() {
        let n = 21;
        let w = world(n, 32.0, |x, _| if x == 10 { 0.0 } else { 1.0 });
        let water: Vec<f64> = (0..n * n).map(|i| if i % n == 10 { 0.5 } else { f64::NAN }).collect();
        for r in ["sandstone", "limestone", "shale", "granite"] {
            let d = drainage_from(&w, &water, &vec![rock(r); n * n]);
            let beside = 10 * n + 12;
            assert!(d[beside] < 33, "{r}: floodplain drainage {}", d[beside]);
        }
    }

    /// Ground at slope 0.5 is well drained, even on shale and beside a river.
    #[test]
    fn steep_slope_is_well_drained() {
        let n = 21;
        let dx = 32.0;
        let w = world(n, dx, |x, _| x as f64 * dx * 0.5);
        let water: Vec<f64> = (0..n * n).map(|i| if i % n == 0 { 0.0 } else { f64::NAN }).collect();
        for r in ["shale", "sandstone"] {
            let d = drainage_from(&w, &water, &vec![rock(r); n * n]);
            for x in [2, 10] {
                assert!(d[10 * n + x] > 66, "{r} x {x}: slope drainage {}", d[10 * n + x]);
            }
        }
    }

    /// A lake fed by little more than its own surface is closed (evaporation
    /// takes the inflow) and salt; one fed by a big catchment spills, fresh.
    #[test]
    fn closed_lake_is_salt() {
        let n = 40;
        let dx = 32.0;
        let w = world(n, dx, |_, _| 10.0);
        let cell = dx * dx;
        let mut depth = vec![0.0; n * n];
        let mut q = vec![cell; n * n];
        // Closed: 3 x 3 cells, inflow = 1.5 x its surface.
        for z in 5..8 {
            for x in 5..8 {
                depth[z * n + x] = 2.0;
                q[z * n + x] = 1.5 * 9.0 * cell;
            }
        }
        // Open: 3 x 3 cells, inflow = 50 x its surface.
        for z in 25..28 {
            for x in 25..28 {
                depth[z * n + x] = 2.0;
                q[z * n + x] = 50.0 * 9.0 * cell;
            }
        }
        let closed = closed_lakes(&w, &depth, &q);
        assert!(closed[6 * n + 6], "the small-catchment lake is closed");
        assert!(!closed[26 * n + 26], "the big-catchment lake spills");
        let s = salinity_from(&w, &closed);
        assert_eq!(s[6 * n + 6], 100, "closed lake");
        assert_eq!(s[26 * n + 26], 0, "open lake");
        assert_eq!(s[20 * n + 20], 0, "dry land far from salt water");
        // The shore of the salt lake tapers.
        let shore = s[6 * n + 8];
        assert!(shore > 0 && shore < 100, "shore {shore}");
    }

    #[test]
    fn volcanoes_keep_their_spacing() {
        // A 48 km world at 250 m, volcanism saturated everywhere.
        let (n, cell) = (192, 250.0);
        let k = Knobs::default();
        let v = vec![100u8; n * n];
        let sites = volcanoes(7, &v, &vec![true; n * n], cell, &k);
        assert_eq!(sites.len(), k.volcano_count_max.round() as usize);
        for (a, p) in sites.iter().enumerate() {
            for q in &sites[a + 1..] {
                let d = ((p[0] - q[0]) as f64).hypot((p[1] - q[1]) as f64);
                assert!(d >= VOLCANO_SPACING_M, "{p:?} and {q:?} only {d:.0} m apart");
            }
        }
        // Only cells at 100, only eligible ones.
        let mut v = vec![99u8; n * n];
        v[50 * n + 50] = 100;
        v[150 * n + 150] = 100;
        let mut eligible = vec![true; n * n];
        eligible[150 * n + 150] = false;
        let sites = volcanoes(7, &v, &eligible, cell, &k);
        assert_eq!(sites, vec![[50.5 * 250.0, 50.5 * 250.0]]);
    }

    /// Swamp-grade drainage (< 33) only on flat ground: from a slope of
    /// 0.3 * DRAINAGE_SLOPE_FULL up, no rock, water height or distance gets
    /// there.
    #[test]
    fn slopes_are_never_swamp_grade() {
        let slope = 0.3 * DRAINAGE_SLOPE_FULL;
        for s in [slope, 0.1, 0.2, 0.5, 2.0] {
            for perm in [0.0, 0.35, 1.0] {
                for above in [-5.0, 0.0, 2.0, 4.0, 6.0, 50.0] {
                    for dist in [0.0, 100.0, 2_000.0, 1.0e9] {
                        let d = to_u8(drainage_at(s, perm, above, dist));
                        assert!(d >= 33, "slope {s} perm {perm} above {above} dist {dist}: {d}");
                    }
                }
            }
        }
        // Flat dry permeable ground is well drained, flat dry shale is not.
        assert!(to_u8(drainage_at(0.0, permeability(rock("sandstone")), 50.0, 1.0e9)) > 66);
        assert!(to_u8(drainage_at(0.0, permeability(rock("shale")), 50.0, 1.0e9)) < 50);
    }

    #[test]
    fn every_rock_has_a_permeability() {
        for (name, _) in PERMEABILITY {
            assert!(rock_index(name).is_some(), "{name}");
        }
        assert_eq!(PERMEABILITY.len(), crate::forge::geology::ROCKS.len());
    }

    /// The sites come from the seed and knobs, not the cell: every level of
    /// the erosion schedule raises the same cones. The default continent has
    /// some; the island layouts none.
    #[test]
    fn volcano_sites_ignore_the_cell_size() {
        let p = ForgeParams { size_m: 32_000.0, cell_m: 64.0, ..Default::default() };
        let sites = crate::forge::world::volcano_sites(&p);
        assert!(!sites.is_empty(), "the default continent has volcanoes");
        for cell_m in [32.0, 128.0, 256.0] {
            assert_eq!(crate::forge::world::volcano_sites(&ForgeParams { cell_m, ..p.clone() }), sites);
        }
        let mut island = p.clone();
        island.knobs.layout = crate::forge::knobs::LAYOUT_ONE_ISLAND as f64;
        assert!(crate::forge::world::volcano_sites(&island).is_empty());
        let mut none = p.clone();
        none.knobs.volcano_count_max = 0.0;
        assert!(crate::forge::world::volcano_sites(&none).is_empty());
    }

    /// A cone of full belt uplift stands at each site, inside the land.
    #[test]
    fn world_new_raises_a_cone_at_each_volcano() {
        let p = ForgeParams { size_m: 32_000.0, cell_m: 128.0, ..Default::default() };
        let w = World::new(&p);
        let k = &p.knobs;
        for s in crate::forge::world::volcano_sites(&p) {
            let i = (s[1] as f64 / w.dx) as usize * w.n + (s[0] as f64 / w.dx) as usize;
            assert!(!w.ocean[i], "{s:?} at sea");
            let full = k.uplift * k.belt_uplift;
            assert!(w.uplift[i] > 0.9 * full, "{s:?}: uplift {} vs {full}", w.uplift[i]);
        }
    }
}
