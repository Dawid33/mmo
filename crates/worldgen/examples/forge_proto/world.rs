//! The simulated world grid, run parameters, and the initial condition.

use std::path::PathBuf;

use crate::knobs::{Knobs, LAYOUT_ARC, LAYOUT_ARCHIPELAGO, LAYOUT_ONE_ISLAND};
use crate::noise::OctaveNoise;

pub const DEFAULT_SIZE_M: f64 = 16_000.0;
pub const DEFAULT_CELL_M: f64 = 16.0;
pub const DEFAULT_SEED: u64 = 0x00D5_C0DE_0206;
pub const DEFAULT_OUT: &str = "target/world-forge/f1";

/// The ocean margin ramps from fully open to fully drowned over this share of
/// the half-width past `Knobs::ocean_margin_start`.
const OCEAN_MARGIN_RAMP: f64 = 0.13;

/// In a multi-island layout the largest island is `MAIN_ISLAND_SHARE /
/// sqrt(count)` of `island_extent`, clamped: several islands have to share the
/// world one island had to itself, and more islands means smaller ones.
const MAIN_ISLAND_SHARE: f64 = 1.25;
const MAIN_ISLAND_SHARE_BOUNDS: (f64, f64) = (0.30, 0.62);
/// Island arcs are volcanic: the core of each island gets this share of the
/// peak uplift whether or not a mountain belt crosses it.
const ARC_CORE_UPLIFT: f64 = 0.85;
/// The smallest island, as a share of the largest.
const SMALLEST_ISLAND_SHARE: f64 = 0.22;
/// Islands keep this far inside the world (half-width units), margin included.
const ISLAND_REACH_LIMIT: f64 = 0.80;
/// Neighbouring islands blend over this much landness: low necks and
/// isthmuses instead of two circles touching at a point.
const ISLAND_BLEND: f64 = 0.07;
/// Island size the inland/relief rules were tuned on (the default single
/// island). Smaller islands are read as scaled copies of it, so an islet gets
/// hills in proportion instead of being a flat sandbank.
const REFERENCE_EXTENT: f64 = 0.66;

/// One island of the layout: center and radius in half-width units
/// (the world spans -1..1 on both axes).
struct Blob {
    x: f64,
    z: f64,
    r: f64,
}

/// Island placement for the chosen layout. One island = a single blob at the
/// center with radius `island_extent`, which reproduces the original formula
/// `island_extent - radial` bit for bit.
fn layout_blobs(seed: u64, k: &Knobs) -> Vec<Blob> {
    let layout = k.layout();
    if layout == LAYOUT_ONE_ISLAND {
        return vec![Blob { x: 0.0, z: 0.0, r: k.island_extent }];
    }
    let count = k.island_count.round().clamp(2.0, 14.0) as usize;
    let main = k.island_extent
        * (MAIN_ISLAND_SHARE / (count as f64).sqrt()).clamp(MAIN_ISLAND_SHARE_BOUNDS.0, MAIN_ISLAND_SHARE_BOUNDS.1);
    let size = |u: f64| main * (1.0 - k.island_size_variation * u).max(SMALLEST_ISLAND_SHARE);
    let fit = |mut b: Blob| {
        // Keep the whole island inside the world: shrink what sticks out.
        let d = (b.x * b.x + b.z * b.z).sqrt();
        b.r = b.r.min(ISLAND_REACH_LIMIT - d).max(0.06);
        b
    };
    let mut blobs: Vec<Blob> = Vec::new();
    if layout == LAYOUT_ARC {
        // Islands along a circular arc through the world center, largest in
        // the middle — the shape of a volcanic island chain.
        let turn = hash01(seed, 0xA2C, 0) * std::f64::consts::TAU;
        let radius = 1.3;
        let (cx, cz) = (radius * turn.cos(), radius * turn.sin());
        let half_angle = (k.island_spread / radius).min(1.0).asin();
        // Neighbours along the arc are this far apart; an island wider than
        // that (times the separation knob) would fuse the chain into one ridge.
        let spacing = 2.0 * radius * (half_angle / (count - 1).max(1) as f64).sin();
        for i in 0..count {
            let t = if count == 1 { 0.0 } else { 2.0 * i as f64 / (count - 1) as f64 - 1.0 };
            let jitter = (hash01(seed, 0xA2C, 1 + i as u64) - 0.5) * 0.5 / count as f64;
            let angle = turn + std::f64::consts::PI + (t + jitter) * half_angle;
            let off = (hash01(seed, 0xA2C, 100 + i as u64) - 0.5) * 0.10;
            let (x, z) = (cx + (radius + off) * angle.cos(), cz + (radius + off) * angle.sin());
            // The middle of the chain may crowd its neighbours a little.
            let room = 0.5 * spacing / k.island_separation * (1.45 - 0.45 * t.abs());
            blobs.push(fit(Blob { x, z, r: size(t.abs()).min(room) }));
        }
        return blobs;
    }
    debug_assert_eq!(layout, LAYOUT_ARCHIPELAGO);
    // Archipelago: the main island near the center, the rest scattered in a
    // disc, each accepted only if it keeps its distance from the others.
    let a = hash01(seed, 0xA2D, 0) * std::f64::consts::TAU;
    let d = 0.18 * k.island_spread * hash01(seed, 0xA2D, 1);
    blobs.push(fit(Blob { x: d * a.cos(), z: d * a.sin(), r: main }));
    let mut attempt = 0u64;
    while blobs.len() < count && attempt < 400 {
        attempt += 1;
        let angle = hash01(seed, 0xA2E, attempt) * std::f64::consts::TAU;
        let dist = k.island_spread * hash01(seed, 0xA2F, attempt).sqrt(); // uniform over the disc
        let u = hash01(seed, 0xA30, attempt);
        let candidate = fit(Blob { x: dist * angle.cos(), z: dist * angle.sin(), r: size(0.25 + 0.75 * u) });
        let clear = blobs.iter().all(|b| {
            let gap = ((b.x - candidate.x).powi(2) + (b.z - candidate.z).powi(2)).sqrt();
            gap >= (b.r + candidate.r) * k.island_separation
        });
        if clear {
            blobs.push(candidate);
        }
    }
    blobs
}

/// The layout at one point, before coast noise.
struct Landmass {
    /// Landness: > 0 inside an island (smooth union of the blobs).
    mass: f64,
    /// Radius of the island that shapes this point, blended smoothly between
    /// neighbours so nothing derived from it shows a seam in the sea.
    radius: f64,
    /// 1 at the center of the dominant island, 0 on its edge.
    core: f64,
}

fn landmass(blobs: &[Blob], rx: f64, rz: f64) -> Landmass {
    let (mut best, mut best_r, mut blended) = (f64::MIN, 1.0, f64::MIN);
    for b in blobs {
        let v = b.r - ((rx - b.x) * (rx - b.x) + (rz - b.z) * (rz - b.z)).sqrt();
        if v > best {
            best = v;
            best_r = b.r;
        }
        // Polynomial smooth maximum: equals max() once the two differ by ISLAND_BLEND.
        blended = if blended == f64::MIN {
            v
        } else {
            let h = (ISLAND_BLEND - (blended - v).abs()).max(0.0) / ISLAND_BLEND;
            blended.max(v) + h * h * ISLAND_BLEND * 0.25
        };
    }
    // Radius blended by closeness to each island (inverse-square of how far
    // behind the leader it is): continuous where the leader changes.
    let (mut sum_w, mut sum_r) = (0.0, 0.0);
    for b in blobs {
        let v = b.r - ((rx - b.x) * (rx - b.x) + (rz - b.z) * (rz - b.z)).sqrt();
        let w = 1.0 / ((0.03 + (best - v)) * (0.03 + (best - v)));
        sum_w += w;
        sum_r += w * b.r;
    }
    Landmass { mass: blended, radius: sum_r / sum_w, core: (best / best_r).clamp(0.0, 1.0) }
}

const NEIGHBORS: [(i32, i32); 8] =
    [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

#[derive(Clone)]
pub struct Params {
    pub size_m: f64,
    pub cell_m: f64,
    pub seed: u64,
    pub out: PathBuf,
    /// Every tunable of the simulation (knobs.rs).
    pub knobs: Knobs,
    /// Try this many seeds at 32 m cells and rank them instead of forging one.
    pub search: usize,
    /// Serve the tuning page on this port instead of forging once.
    pub serve: Option<u16>,
    pub quiet: bool,
}

impl Params {
    pub fn parse() -> Params {
        let mut p = Params {
            size_m: DEFAULT_SIZE_M,
            cell_m: DEFAULT_CELL_M,
            seed: DEFAULT_SEED,
            out: PathBuf::from(DEFAULT_OUT),
            knobs: Knobs::default(),
            search: 0,
            serve: None,
            quiet: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
            match flag.as_str() {
                "--size" => p.size_m = value().parse().expect("--size <meters>"),
                "--cell" => p.cell_m = value().parse().expect("--cell <meters>"),
                "--iters" => p.knobs.iters = value().parse().expect("--iters <count>"),
                "--seed" => {
                    let v = value();
                    let hex = v.trim_start_matches("0x").replace('_', "");
                    p.seed = u64::from_str_radix(&hex, 16).expect("--seed <hex>");
                }
                "--out" => p.out = PathBuf::from(value()),
                "--dt" => p.knobs.dt_years = value().parse().expect("--dt <years>"),
                "--uplift" => p.knobs.uplift = value().parse().expect("--uplift <m/yr>"),
                "--k" => p.knobs.erodibility = value().parse().expect("--k <erodibility>"),
                "--diffusivity" => p.knobs.diffusivity = value().parse().expect("--diffusivity <m^2/yr>"),
                "--no-geology" => p.knobs.geology_strength = 0.0,
                "--no-climate" => p.knobs.climate_strength = 0.0,
                "--search" => p.search = value().parse().expect("--search <seed count>"),
                "--serve" => p.serve = Some(value().parse().expect("--serve <port>")),
                // Any knob by its key (knobs.rs; the tuning page's "Copy as flags").
                "--set" => {
                    let v = value();
                    let (key, number) = v.split_once('=').expect("--set <knob>=<value>");
                    let number: f64 = number.parse().unwrap_or_else(|_| panic!("--set {key}: not a number"));
                    assert!(p.knobs.set(key, number), "--set: no knob named {key} (see knobs.rs)");
                }
                other => panic!(
                    "unknown argument {other}; flags: --size --cell --iters --seed --out --dt \
                     --uplift --k --diffusivity --no-geology --no-climate --set <knob>=<value> \
                     --search <n> --serve <port>"
                ),
            }
        }
        p
    }
}

pub fn smoothstep(lo: f64, hi: f64, x: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Uniform hash in [0, 1) of (seed, a, b).
pub fn hash01(seed: u64, a: u64, b: u64) -> f64 {
    let mut z = seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

/// The simulated world: square grid, row-major (z rows, x columns). Heights
/// are meters relative to sea level (engine y = SEA_LEVEL + h).
pub struct World {
    pub n: usize,
    pub dx: f64,
    /// Ground height.
    pub h: Vec<f64>,
    /// Fixed sea cells: the base level everything drains to.
    pub ocean: Vec<bool>,
    /// Rock uplift rate (m/yr).
    pub uplift: Vec<f64>,
    /// Total rock uplift applied so far (m): the rock now at height y started
    /// at y - uplifted, which is what the geology column is indexed by.
    pub uplifted: Vec<f64>,
    /// Would-be land the ocean margin drowned (a big number = the island is
    /// pressing against the world edge; the seed search penalises it).
    pub margin_drowned: usize,
}

impl World {
    /// Island + uplift map from noise. Wavelength of a noise's first octave is
    /// 2^-first_octave meters (all sampled at scale 1).
    pub fn new(p: &Params) -> World {
        let n = (p.size_m / p.cell_m).round() as usize;
        let half = p.size_m * 0.5;
        let shape_lo = OctaveNoise::new(p.seed ^ 0xF001, -13, vec![1.0, 0.6, 0.3]); // 8 km
        let shape_hi = OctaveNoise::new(p.seed ^ 0xF002, -10, vec![1.0, 0.5]); // 1 km
        let belts = OctaveNoise::new(p.seed ^ 0xF003, -12, vec![1.0, 0.5]); // 4 km
        let k = &p.knobs;
        let blobs = layout_blobs(p.seed, k);
        let one_island = k.layout() == LAYOUT_ONE_ISLAND;
        let arc = k.layout() == LAYOUT_ARC;
        let rough = OctaveNoise::new(p.seed ^ 0xF004, -8, vec![1.0, 1.0, 1.0]); // 256 m

        let mut w = World {
            n,
            dx: p.cell_m,
            h: vec![0.0; n * n],
            ocean: vec![false; n * n],
            uplift: vec![0.0; n * n],
            uplifted: vec![0.0; n * n],
            margin_drowned: 0,
        };
        for z in 0..n {
            for x in 0..n {
                let (mx, mz) = ((x as f64 + 0.5) * p.cell_m, (z as f64 + 0.5) * p.cell_m);
                let (rx, rz) = ((mx - half) / half, (mz - half) / half);
                // Superellipse (p = 4) distance to the rim: 1 on the world edge,
                // rounder than max(|x|, |z|) so the margin is not a visible frame.
                let edge = (rx.powi(4) + rz.powi(4)).sqrt().sqrt();
                // The coast-shaping noise fades toward the rim, so the island
                // wanders freely in the middle but stays clear of the margin.
                let reach = 1.0 - 0.75 * smoothstep(0.50, 0.95, edge);
                let lm = landmass(&blobs, rx, rz);
                // Everything tuned on the reference island is rescaled to the
                // island at hand (all three factors are exactly 1 for one island).
                let size = if one_island { 1.0 } else { lm.radius / REFERENCE_EXTENT };
                let relief_scale = 1.0 / size;
                // Coast noise at full strength fuses or drowns small islands.
                // The 8 km wander shrinks with the island (size^0.75); the
                // 1 km detail barely does (size^0.25) — it is what gives an
                // islet a coastline instead of a circle.
                let (wander, detail) = (size.sqrt() * size.sqrt().sqrt(), size.sqrt().sqrt());
                let shape = lm.mass
                    + reach
                        * (wander * k.coast_noise_lo * shape_lo.sample2(mx, mz)
                            + detail * k.coast_noise_hi * shape_hi.sample2(mx, mz));
                // "Landness": > 0 is land. Beyond the margin it is forced under
                // water, which is what guarantees "ocean all round".
                let margin_end = (k.ocean_margin_start + OCEAN_MARGIN_RAMP).min(0.99);
                let land = shape - 2.0 * smoothstep(k.ocean_margin_start, margin_end, edge);

                let i = z * n + x;
                if land <= 0.0 {
                    w.ocean[i] = true;
                    w.h[i] = if shape < 0.0 {
                        (shape * 160.0 - 1.0).max(-90.0) // shelf, then abyss
                    } else {
                        w.margin_drowned += 1;
                        (land * 40.0 - 1.0).max(-20.0) // drowned by the margin: shoals
                    };
                    continue;
                }
                // Low initial relief: erosion, not noise, is to make the shape.
                // Small islands are read as scaled copies of the reference one.
                let inland_ness = land * relief_scale;
                w.h[i] = (1.0 + 45.0 * smoothstep(0.0, 0.5, inland_ness) + 3.0 * rough.sample2(mx, mz))
                    .max(0.5);
                // Mountain belts along the zero contour of a low-frequency
                // noise (long, linear), only inland; none on the coast.
                let belt_noise = belts.sample2(mx * k.belt_scale, mz * k.belt_scale);
                let belt = 1.0 - smoothstep(0.0, k.belt_width, belt_noise.abs());
                let inland = smoothstep(0.06, k.inland_gate.max(0.07), inland_ness);
                let volcanic = if arc { ARC_CORE_UPLIFT * smoothstep(0.35, 0.9, lm.core) } else { 0.0 };
                let coast_taper = smoothstep(0.0, 0.05, land);
                w.uplift[i] = k.uplift
                    * coast_taper
                    // Grouped as ((1 - share) * belt) * inland, exactly as before
                    // the layouts existed: regrouping shifts the last bit of the
                    // uplift, which the f64 discharge sums then carry into the
                    // world file.
                    * (k.lowland_uplift_share
                        + ((1.0 - k.lowland_uplift_share) * belt * inland)
                            .max((1.0 - k.lowland_uplift_share) * volcanic));
            }
        }
        for k in 0..n {
            for i in [k, (n - 1) * n + k, k * n, k * n + n - 1] {
                assert!(w.ocean[i], "ocean margin must reach the world edge");
            }
        }
        w
    }

    /// 8-neighbors of cell `i` with their distance in meters.
    pub fn neighbors(&self, i: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        let (x, z) = ((i % self.n) as i32, (i / self.n) as i32);
        let n = self.n as i32;
        NEIGHBORS.iter().filter_map(move |&(ox, oz)| {
            let (nx, nz) = (x + ox, z + oz);
            (nx >= 0 && nz >= 0 && nx < n && nz < n).then(|| {
                let dist = if ox != 0 && oz != 0 { std::f64::consts::SQRT_2 } else { 1.0 };
                ((nz * n + nx) as usize, dist * self.dx)
            })
        })
    }

    /// Cell center in world meters.
    pub fn center(&self, i: usize) -> (f64, f64) {
        (((i % self.n) as f64 + 0.5) * self.dx, ((i / self.n) as f64 + 0.5) * self.dx)
    }
}
