//! Plate-like structure for the continental layouts: where the mountain belts
//! go. The world is cut into `Knobs::plate_count` plates — a Voronoi diagram
//! of jittered sites, its boundaries bent by a domain warp so they wander —
//! and each plate drifts in its own direction. Where two plates move toward
//! each other the boundary is CONVERGENT and a belt of uplift rises along it;
//! divergent and transform boundaries stay low. With a handful of plates the
//! boundaries are a third to half the world long, so belts are tens of km
//! long in a 100 km world.
//!
//! A few INTERIOR ranges (`interior_ranges`: straight belts of
//! `belt_length_km`, about a third of a boundary belt's strength) stand for old
//! belts inside a plate, so not every range sits on a boundary.
//!
//! Everything is laid out in shares of the world's size (and the warp noise
//! is sampled at `size_m / REFERENCE_SIZE_M`), so a coarse level of the
//! erosion schedule and the final grid see the same plates, and a 100 km
//! world is the 16 km one scaled up — except the interior range length,
//! which is in km.

use crate::forge::knobs::Knobs;
use crate::forge::par;
use crate::forge::world::{edges, hash01, smoothstep, EdgeKind, LAND_EDGE_TAPER_M, REFERENCE_SIZE_M};
use crate::noise::OctaveNoise;

/// Plate sites are scattered over the world grown by this share of its size
/// on every side, so plates along the edge are cut by it like any other.
const PLATE_SITE_REACH: f64 = 0.15;
/// Sites keep at least this share of `size / sqrt(count)` apart: plates of
/// comparable size, not slivers.
const PLATE_SPACING_SHARE: f64 = 0.55;
const PLATE_PLACEMENT_ATTEMPTS: u64 = 4_000;
/// Domain warp of the plate boundaries, as a share of the world's size.
const PLATE_WARP_SHARE: f64 = 0.05;
/// A boundary running along a land edge (within its belt half-width, the
/// edge taper and the warp of the edge) would build a range parallel to the
/// edge whose crest sits where the edge taper ends — a straight ridge. Such a
/// stretch keeps `1 - parallel^LAND_EDGE_PARALLEL_POWER` of its strength,
/// `parallel` = |cos| of its angle to the edge: belts crossing the edge stay,
/// belts along it go.
const LAND_EDGE_PARALLEL_POWER: f64 = 2.0;
/// Convergence (closing speed over the plates' unit drift, -1..1) at which a
/// boundary starts to build a belt, and at which the belt is at full height.
const CONVERGENCE_MIN: f64 = 0.10;
const CONVERGENCE_FULL: f64 = 0.35;
/// Belt strength varies along the boundary by up to this share (a 4 km noise
/// at the reference size): separate massifs, saddles between them.
const ALONG_STRIKE_VARIATION: f64 = 0.3;
/// A belt is at full strength within this share of its half-width of the
/// boundary, then falls off smoothly: a broad range, not a knife ridge.
const BELT_CORE_SHARE: f64 = 0.35;
/// An interior range's uplift relative to a full boundary belt, and its
/// half-width relative to a boundary belt's.
pub const INTERIOR_RANGE_STRENGTH: f64 = 0.35;
const INTERIOR_RANGE_WIDTH_SHARE: f64 = 0.8;
/// Interior range length cap and placement box, as shares of the world size.
const INTERIOR_RANGE_MAX_SHARE: f64 = 0.6;
const INTERIOR_RANGE_BOX: (f64, f64) = (0.2, 0.8);

/// One plate: site (m) and unit drift direction.
#[derive(Clone, Copy, Debug)]
pub struct Plate {
    pub x: f64,
    pub z: f64,
    pub vx: f64,
    pub vz: f64,
}

/// The plates of a world, from the seed and knobs only.
pub fn plates(seed: u64, size_m: f64, k: &Knobs) -> Vec<Plate> {
    let count = k.plate_total();
    let min_gap = PLATE_SPACING_SHARE * size_m / (count as f64).sqrt();
    let span = size_m * (1.0 + 2.0 * PLATE_SITE_REACH);
    let mut sites: Vec<(f64, f64)> = Vec::with_capacity(count);
    let mut attempt = 0u64;
    while sites.len() < count {
        attempt += 1;
        let x = hash01(seed, 0x91A7, attempt) * span - PLATE_SITE_REACH * size_m;
        let z = hash01(seed, 0x91A8, attempt) * span - PLATE_SITE_REACH * size_m;
        // After the attempt budget, accept anything (never loops forever).
        let clear = attempt > PLATE_PLACEMENT_ATTEMPTS
            || sites.iter().all(|&(sx, sz)| (sx - x).hypot(sz - z) >= min_gap);
        if clear {
            sites.push((x, z));
        }
    }
    sites
        .into_iter()
        .enumerate()
        .map(|(i, (x, z))| {
            let a = hash01(seed, 0x91A9, i as u64) * std::f64::consts::TAU;
            Plate { x, z, vx: a.cos(), vz: a.sin() }
        })
        .collect()
}

/// Per cell (row-major, `n` x `n` over `size_m`): the plate it belongs to and
/// the belt strength there, 0..1 — 1 on a fully convergent boundary, falling
/// to 0 a `Knobs::plate_belt_width` share of the world away from it, and 0
/// along divergent and transform boundaries.
pub struct PlateField {
    pub plate: Vec<u16>,
    pub convergence: Vec<f32>,
}

/// Warp field that bends the plate boundaries (both coordinates, m).
struct Warp {
    x: OctaveNoise,
    z: OctaveNoise,
    along: OctaveNoise,
    scale: f64,
    amplitude: f64,
}

impl Warp {
    fn new(seed: u64, size_m: f64) -> Warp {
        Warp {
            x: OctaveNoise::new(seed ^ 0xF101, -13, vec![1.0, 0.5]), // 8 km at the reference size
            z: OctaveNoise::new(seed ^ 0xF102, -13, vec![1.0, 0.5]),
            along: OctaveNoise::new(seed ^ 0xF103, -12, vec![1.0, 0.5]), // 4 km
            scale: size_m / REFERENCE_SIZE_M,
            amplitude: PLATE_WARP_SHARE * size_m,
        }
    }

    fn at(&self, mx: f64, mz: f64) -> (f64, f64) {
        let (sx, sz) = (mx / self.scale, mz / self.scale);
        (mx + self.amplitude * self.x.sample2(sx, sz), mz + self.amplitude * self.z.sample2(sx, sz))
    }
}

/// A convergent plate boundary: the Voronoi edge between two plates (in the
/// warped frame), clipped to where those two really are the nearest sites,
/// and how strongly the plates close across it (0..1).
#[derive(Clone, Copy, Debug)]
pub struct Boundary {
    pub x0: f64,
    pub z0: f64,
    pub x1: f64,
    pub z1: f64,
    pub strength: f64,
}

/// The convergent boundaries between `plates`. Each pair's bisector is
/// clipped by every other site's half-plane, so a boundary ends at its triple
/// junctions — the belt rounds off there instead of running on as a phantom
/// bisector (which left straight seams across the uplift).
pub fn convergent_boundaries(plates: &[Plate], size_m: f64) -> Vec<Boundary> {
    let reach = 4.0 * size_m; // unbounded hull edges are cut here
    let mut out = Vec::new();
    for a in 0..plates.len() {
        for j in a + 1..plates.len() {
            let (pa, pj) = (&plates[a], &plates[j]);
            let gap = (pj.x - pa.x).hypot(pj.z - pa.z).max(1.0e-9);
            let (ux, uz) = ((pj.x - pa.x) / gap, (pj.z - pa.z) / gap);
            let closing = 0.5 * ((pa.vx - pj.vx) * ux + (pa.vz - pj.vz) * uz);
            let strength = smoothstep(CONVERGENCE_MIN, CONVERGENCE_FULL, closing);
            if strength <= 0.0 {
                continue;
            }
            // Bisector: m + t * d. A point is on the real edge while no other
            // site k is nearer than a: 2 q.(k - a) <= |k|^2 - |a|^2, linear in t.
            let (mx, mz) = (0.5 * (pa.x + pj.x), 0.5 * (pa.z + pj.z));
            let (dx, dz) = (-uz, ux);
            let (mut lo, mut hi) = (-reach, reach);
            for (k, pk) in plates.iter().enumerate() {
                if k == a || k == j {
                    continue;
                }
                let (kx, kz) = (pk.x - pa.x, pk.z - pa.z);
                let rhs = (pk.x * pk.x + pk.z * pk.z) - (pa.x * pa.x + pa.z * pa.z) - 2.0 * (mx * kx + mz * kz);
                let c = 2.0 * (dx * kx + dz * kz);
                if c > 1.0e-12 {
                    hi = hi.min(rhs / c);
                } else if c < -1.0e-12 {
                    lo = lo.max(rhs / c);
                } else if rhs < 0.0 {
                    hi = lo - 1.0; // parallel and outside: no edge
                }
            }
            if lo < hi {
                out.push(Boundary { x0: mx + lo * dx, z0: mz + lo * dz, x1: mx + hi * dx, z1: mz + hi * dz, strength });
            }
        }
    }
    out
}

/// `boundaries` with the stretches that run along a land edge weakened
/// (`LAND_EDGE_PARALLEL_POWER`): each boundary is split where it enters the
/// band of width `band` m inside a land edge, and the part inside keeps the
/// share of its strength its angle to the edge allows.
fn soften_along_land_edges(boundaries: Vec<Boundary>, land: [bool; 4], size_m: f64, band: f64) -> Vec<Boundary> {
    let mut out = boundaries;
    for e in (0..4).filter(|&e| land[e]) {
        // Distance in from edge e (N, E, S, W) of a point.
        let inward = |x: f64, z: f64| [z, size_m - x, size_m - z, x][e];
        let mut next = Vec::with_capacity(out.len());
        for b in out {
            let (d0, d1) = (inward(b.x0, b.z0), inward(b.x1, b.z1));
            let len = (b.x1 - b.x0).hypot(b.z1 - b.z0).max(1.0e-9);
            let along = if e % 2 == 0 { (b.x1 - b.x0).abs() } else { (b.z1 - b.z0).abs() } / len;
            let kept = b.strength * (1.0 - along.powf(LAND_EDGE_PARALLEL_POWER));
            let at = |t: f64| (b.x0 + t * (b.x1 - b.x0), b.z0 + t * (b.z1 - b.z0));
            // Parameter range [lo, hi] of the segment inside the band.
            let (lo, hi) = if (d1 - d0).abs() < 1.0e-9 {
                if d0 < band { (0.0, 1.0) } else { (1.0, 0.0) }
            } else {
                let t = (band - d0) / (d1 - d0);
                if d1 < d0 { (t.max(0.0), 1.0) } else { (0.0, t.min(1.0)) }
            };
            let mut piece = |t0: f64, t1: f64, strength: f64| {
                if t1 > t0 && strength > 0.0 {
                    let ((x0, z0), (x1, z1)) = (at(t0), at(t1));
                    next.push(Boundary { x0, z0, x1, z1, strength });
                }
            };
            if lo >= hi {
                piece(0.0, 1.0, b.strength);
            } else {
                piece(0.0, lo, b.strength);
                piece(lo, hi, kept);
                piece(hi, 1.0, b.strength);
            }
        }
        out = next;
    }
    out
}

/// Distance (m) from (x, z) to the segment (x0, z0)-(x1, z1).
fn segment_distance(x: f64, z: f64, x0: f64, z0: f64, x1: f64, z1: f64) -> f64 {
    let (ex, ez) = (x1 - x0, z1 - z0);
    let len2 = (ex * ex + ez * ez).max(1.0e-9);
    let t = (((x - x0) * ex + (z - z0) * ez) / len2).clamp(0.0, 1.0);
    (x - (x0 + t * ex)).hypot(z - (z0 + t * ez))
}

/// Belt profile at `d` from the axis: 1 within `BELT_CORE_SHARE` of the
/// half-width, 0 beyond it.
fn belt_profile(d: f64, half_width: f64) -> f64 {
    1.0 - smoothstep(BELT_CORE_SHARE * half_width, half_width, d)
}

/// The plate structure on an `n` x `n` grid over a `size_m` world.
pub fn plate_field(seed: u64, size_m: f64, n: usize, k: &Knobs) -> PlateField {
    let plates = plates(seed, size_m, k);
    let warp = Warp::new(seed, size_m);
    let cell = size_m / n as f64;
    let half_width = (k.plate_belt_width * size_m).max(cell);
    let land = edges(seed, k).map(|e| e == EdgeKind::Land);
    let band = half_width + LAND_EDGE_TAPER_M.min(0.25 * size_m) + warp.amplitude;
    let boundaries = soften_along_land_edges(convergent_boundaries(&plates, size_m), land, size_m, band);
    let per_cell = par::map_range(n * n, |i| {
        let (mx, mz) = (((i % n) as f64 + 0.5) * cell, ((i / n) as f64 + 0.5) * cell);
        let (px, pz) = warp.at(mx, mz);
        // Nearest site (ties to the lower index): the cell's plate.
        let d2 = |s: &Plate| (px - s.x).powi(2) + (pz - s.z).powi(2);
        let a = (0..plates.len()).fold(0, |best, j| if d2(&plates[j]) < d2(&plates[best]) { j } else { best });
        // The strongest belt of any convergent boundary within reach: a max
        // of continuous fields, so continuous itself.
        let belt = boundaries
            .iter()
            .map(|b| b.strength * belt_profile(segment_distance(px, pz, b.x0, b.z0, b.x1, b.z1), half_width))
            .fold(0.0, f64::max);
        let along = 1.0
            - ALONG_STRIKE_VARIATION * (0.5 + 0.5 * warp.along.sample2(px / warp.scale, pz / warp.scale)).clamp(0.0, 1.0);
        (a as u16, (belt * along) as f32)
    });
    let (plate, convergence) = per_cell.into_iter().unzip();
    PlateField { plate, convergence }
}

/// A straight interior range: its axis, end to end (m).
#[derive(Clone, Copy, Debug)]
pub struct InteriorRange {
    pub x0: f64,
    pub z0: f64,
    pub x1: f64,
    pub z1: f64,
}

/// The interior ranges of a world (`Knobs::interior_ranges` of them).
pub fn interior_ranges(seed: u64, size_m: f64, k: &Knobs) -> Vec<InteriorRange> {
    let count = k.interior_ranges.round().max(0.0) as u64;
    let length = (k.belt_length_km * 1_000.0).min(INTERIOR_RANGE_MAX_SHARE * size_m);
    let (lo, hi) = INTERIOR_RANGE_BOX;
    (0..count)
        .map(|r| {
            let cx = (lo + (hi - lo) * hash01(seed, 0x1A7E, r)) * size_m;
            let cz = (lo + (hi - lo) * hash01(seed, 0x1A7F, r)) * size_m;
            let a = hash01(seed, 0x1A80, r) * std::f64::consts::PI;
            let (dx, dz) = (0.5 * length * a.cos(), 0.5 * length * a.sin());
            InteriorRange { x0: cx - dx, z0: cz - dz, x1: cx + dx, z1: cz + dz }
        })
        .collect()
}

/// Interior-range belt strength at (x, z) in m: `INTERIOR_RANGE_STRENGTH` on
/// a range's axis, 0 a (narrowed) belt half-width away.
pub fn interior_strength(ranges: &[InteriorRange], x: f64, z: f64, size_m: f64, k: &Knobs) -> f64 {
    let half_width = INTERIOR_RANGE_WIDTH_SHARE * k.plate_belt_width * size_m;
    ranges
        .iter()
        .map(|r| INTERIOR_RANGE_STRENGTH * belt_profile(segment_distance(x, z, r.x0, r.z0, r.x1, r.z1), half_width))
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plate_field_is_deterministic_and_uses_every_plate() {
        let k = Knobs::default();
        let a = plate_field(11, 16_000.0, 125, &k);
        let b = plate_field(11, 16_000.0, 125, &k);
        assert_eq!(a.plate, b.plate);
        assert_eq!(a.convergence, b.convergence);
        assert!(a.convergence.iter().all(|&c| (0.0..=1.0).contains(&c)));
        // Sites reach a little past the world, so most (not all) plates show.
        let mut seen = vec![false; k.plate_total()];
        for &p in &a.plate {
            seen[p as usize] = true;
        }
        assert!(seen.iter().filter(|&&s| s).count() >= k.plate_total() / 2);
        assert!(a.convergence.iter().any(|&c| c > 0.5), "some boundary converges");
    }

    /// The same plates on a coarse and a fine grid (the erosion schedule's
    /// levels): a fine cell's plate is its coarse parent's almost everywhere.
    #[test]
    fn plates_agree_across_resolutions() {
        let k = Knobs::default();
        let (coarse, fine) = (plate_field(5, 16_000.0, 100, &k), plate_field(5, 16_000.0, 200, &k));
        let mut same = 0;
        for z in 0..200 {
            for x in 0..200 {
                same += (fine.plate[z * 200 + x] == coarse.plate[(z / 2) * 100 + x / 2]) as usize;
            }
        }
        assert!(same as f64 / 40_000.0 > 0.95, "{same} of 40000 agree");
    }

    /// Belts round off at triple junctions instead of stopping at a seam:
    /// the field has no jumps between neighbouring cells.
    #[test]
    fn belt_field_is_continuous() {
        let k = Knobs::default();
        for seed in [3, 0x00D5_C0DE_0206] {
            let n = 250;
            let f = plate_field(seed, 16_000.0, n, &k);
            let mut worst: f32 = 0.0;
            for z in 0..n - 1 {
                for x in 0..n - 1 {
                    let i = z * n + x;
                    worst = worst.max((f.convergence[i] - f.convergence[i + 1]).abs());
                    worst = worst.max((f.convergence[i] - f.convergence[i + n]).abs());
                }
            }
            assert!(worst < 0.15, "seed {seed}: neighbouring cells differ by {worst}");
        }
    }
}
