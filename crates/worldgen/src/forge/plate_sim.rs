//! The plate simulation on the planet grid (Phase 2, Ruling 1): PlaTec's
//! bookkeeping with Cortial's rules. One record per planet cell (struct of
//! arrays); each plate is a translation of whole cells — every step a plate's
//! fractional position accumulates its velocity and it moves by a whole
//! `(dx, dz)`, each in {−1, 0, 1}, so crust is never resampled or lost to
//! rounding. Where several cells land on one, ONE total order picks the
//! survivor (continental over oceanic, the bigger continent, the plate whose
//! ocean floor is younger on average) and the rest are consumed; where none
//! lands, new oceanic crust is made, owned by the plate that held the cell;
//! rows moved past a pole are destroyed.
//!
//! Pinned so two builders make the same planet: every loop over cells is
//! row-major, every loop over plates ascending id; random draws are
//! `hash01(key.seed, SALT, index)` and noises `PlanetNoise::new(key.seed,
//! SALT, wavelength)` with the salts below. Positions are `planet_deg(c)` —
//! degrees east and degrees SOUTH of the north edge — and every displacement,
//! direction, velocity and `fold` is in that (east, south) frame; the noises
//! are sampled at `planet_deg(c)` itself. A longitude difference is always
//! the shorter way round.
//!
//! Nothing reads this yet: the forge switches over in Task 7. Plan:
//! docs/superpowers/plans/2026-11-20-plates-and-aged-relief-2.md (Task 1).

use crate::forge::periodic::PeriodicX;
use crate::forge::planet::{
    planet_deg, planet_lat_deg, planet_step, CrustKind, OrogenyKind, PlanetKey, PlanetNoise, PLANET_CELLS,
    PLANET_DEG_PER_CELL, PLANET_NX, PLANET_NZ,
};
use crate::forge::world::{hash01, smoothstep};

/// Model time per step, My (Ruling 1).
pub const PLATE_DT_MY: f64 = 1.5;
/// Plates at the start (untested design).
pub const PLATES_INITIAL: usize = 9;
/// Plate ids are below this (rifts add plates up to it, Task 3).
pub const PLATES_MAX: usize = 24;
/// A cell with no plate (only inside a step, before the gap fill).
pub const NO_PLATE: u8 = 255;

/// The supercontinent's share of the planet's cells (untested design).
const SUPERCONTINENT_SHARE: f64 = 0.26;
/// The supercontinent outline's scale bracket and its bisection's halvings.
const SUPERCONTINENT_SIGMA: [f64; 2] = [0.2, 3.0];
const SUPERCONTINENT_HALVINGS: usize = 24;
/// The supercontinent's half-axes at σ = 1, degrees east–west and north–south.
const SUPERCONTINENT_AXES_DEG: [f64; 2] = [100.0, 58.0];
/// Weight of `N_SHAPE` in the continent outlines.
const OUTLINE_NOISE: f64 = 0.35;
/// The supercontinent's centre latitude is within this of the equator (Ruling 10).
const SUPERCONTINENT_LAT_DEG: f64 = 30.0;
/// Microcontinent centres are within this of the equator (Ruling 10).
const MICRO_LAT_DEG: f64 = 60.0;
/// Placement tries per microcontinent before it is skipped.
const MICRO_ATTEMPTS: usize = 32;
/// A microcontinent's area share: `MICRO_SHARE[0] + MICRO_SHARE[1] · h`.
const MICRO_SHARE: [f64; 2] = [0.008, 0.012];
/// East–west over north–south half-axis of a microcontinent.
const MICRO_AXES_RATIO: f64 = 1.5;
/// Square degrees of the whole planet (360 × 180).
const PLANET_DEG2: f64 = 64_800.0;
/// A microcontinent keeps more than this many cells (chamfer) from other land.
const MICRO_GAP_CELLS: f64 = 6.0;
/// Continental base elevation, m: `CONT_BASE_M + CONT_BASE_NOISE_M · N_BASE`.
const CONT_BASE_M: f64 = 400.0;
const CONT_BASE_NOISE_M: f64 = 350.0;
/// Initial continental crust is born `CONT_BORN_MY[0] − CONT_BORN_MY[1] · t` My (−600 … −2,500).
const CONT_BORN_MY: [f64; 2] = [-600.0, 1_900.0];
/// Initial ocean floor is born up to this long before the start, My.
const OCEAN_BORN_SPREAD_MY: f64 = 150.0;
/// Old belts across the supercontinent at the start: count, half-width (degrees),
/// strength at the axis, the outline warp (degrees), and the segment's offset
/// from the centroid, angle and length draws.
const ANCIENT_SUTURES: usize = 2;
const SUTURE_HALF_WIDTH_DEG: f64 = 6.0;
const SUTURE_STRENGTH: f64 = 0.9;
const SUTURE_WARP_DEG: f64 = 3.0;
const SUTURE_OFFSET_DEG: [f64; 2] = [-25.0, 50.0];
const SUTURE_LENGTH_DEG: [f64; 2] = [60.0, 50.0];
/// Plate sites are kept at least this far apart (today's `0.55 · span / sqrt(count)`, in degrees).
const PLATE_SITE_GAP_DEG: f64 = 33.0;
/// After this many site attempts any site is kept.
const PLATE_SITE_ATTEMPTS: u64 = 4_000;
/// Warp of the plate boundaries, degrees.
const PLATE_SITE_WARP_DEG: f64 = 8.0;
/// A plate more than this share continental moves slowly.
const SLOW_PLATE_CONT_SHARE: f64 = 0.30;
/// Speeds, cells per step: `[base, spread]` for continental and oceanic plates.
const SLOW_SPEED: [f64; 2] = [0.15, 0.30];
const FAST_SPEED: [f64; 2] = [0.40, 0.60];

/// Oceanic elevation, m: `OCEAN_ABYSS_M + (OCEAN_RIDGE_M − OCEAN_ABYSS_M) ·
/// exp(−crust_age / OCEAN_SUBSIDE_MY)` — Cortial's ridge `zr` and abyss `za`;
/// the e-folding is a shape only (untested design).
pub const OCEAN_RIDGE_M: f64 = -1_000.0;
pub const OCEAN_ABYSS_M: f64 = -6_000.0;
pub const OCEAN_SUBSIDE_MY: f64 = 40.0;
/// A full-strength orogen's height above its crust, m, by `OrogenyKind`
/// (None, Arc, Andean, Collision, Plateau, RiftShoulder, Hotspot; the plan's
/// vocabulary, untested design).
pub const OROGEN_HEIGHT_M: [f64; 7] = [0.0, 3_000.0, 6_000.0, 6_000.0, 6_000.0, 800.0, 7_000.0];
/// A hotspot mark's sinking e-folding, My (untested design; Task 3 makes hotspots).
pub const HOTSPOT_SUBSIDE_MY: f64 = 25.0;
/// `root_keep(age) = ROOT_RESIDUAL + (1 − ROOT_RESIDUAL) · exp(−age / ROOT_DECAY_MY)`:
/// how much of an orogen's crustal root is left. 333 My = `zc / εc` = 10 km ÷
/// 0.03 mm/yr, Cortial's continental-erosion e-folding; the residual is untested design.
pub const ROOT_RESIDUAL: f64 = 0.15;
pub const ROOT_DECAY_MY: f64 = 333.0;

/// A subduction belt's reach from its front, degrees: Cortial's `rs` 1,800 km.
pub const SUBDUCTION_REACH_DEG: f64 = 16.2;
/// Where the belt's profile peaks, as a share of the reach (untested: the
/// paper gives no control distance).
pub const SUBDUCTION_PEAK_SHARE: f64 = 0.25;
/// Cortial's subduction uplift `u0`, m per My (0.6 mm/yr); a gain is this
/// over the Andean `OROGEN_HEIGHT_M` (6,000) per My.
pub const SUBDUCTION_U0_M_PER_MY: f64 = 600.0;
/// The ends of Cortial's elevation scale `z̃`, m.
pub const TRENCH_M: f64 = -10_000.0;
pub const CONT_MAX_M: f64 = 10_000.0;
/// A mark's life, steps (untested design: two field refreshes).
pub const FRONT_TTL_STEPS: u8 = 8;
/// The subduction field (front chamfer and gains) runs every this many steps
/// (Ruling 1's budget; the first dial if the stage is too slow).
pub const FIELD_EVERY_STEPS: u32 = 4;
/// An oceanic cell whose strength reaches this becomes continental: an
/// island arc (untested design).
pub const ARC_EMERGE_STRENGTH: f32 = 0.25;
/// Continental cells of one pair consumed before the pair sutures (untested design).
pub const SUTURE_CONSUMED_CELLS: u32 = 24;
/// A collision belt's radius, degrees: at most Cortial's `rc` 4,200 km, at
/// least `COLLISION_RADIUS_MIN_DEG` (untested), `r ∝ sqrt(speed · A)`.
pub const COLLISION_RADIUS_MAX_DEG: f64 = 37.8;
pub const COLLISION_RADIUS_MIN_DEG: f64 = 3.0;
/// A collision's peak gain per square degree of terrane: Cortial's `Δc`
/// 1.3·10⁻⁵ km⁻¹ × 12,321 km² per square degree ÷ `OROGEN_HEIGHT_M` 6,000.
/// `COLLISION_GAIN_PER_DEG2 · A` reaches 1 at 37.5 square degrees (about 76
/// cells), so with `SUTURE_CONSUMED_CELLS = 24` nearly every suture has a
/// full-strength core — in this model a terrane's AREA sets the belt's
/// radius (`r ∝ sqrt(A)`), not its peak.
pub const COLLISION_GAIN_PER_DEG2: f64 = 0.0267;
/// A terrane at least this large, square degrees, raises a `Plateau` (untested design).
pub const PLATEAU_MIN_DEG2: f64 = 300.0;
/// The merged plate's velocity is multiplied by this after a suture (untested design).
pub const COLLISION_BRAKE: f64 = 0.5;

/// Noise salts and wavelengths (degrees).
const N_SHAPE: u64 = 0x5C01;
const N_SHAPE2: u64 = 0x5C02;
const N_BASE: u64 = 0x5C03;
const N_AGE: u64 = 0x5C04;
const N_SHAPE_DEG: f64 = 40.0;
const N_BASE_DEG: f64 = 25.0;
const N_AGE_DEG: f64 = 60.0;
/// Hash salts.
const H_SUPER: u64 = 0x5D01;
const H_MICRO: u64 = 0x5D02;
const H_SUTURE: u64 = 0x5D03;
const H_SITE: u64 = 0x5D04;
const H_VEL: u64 = 0x5D05;
const H_OCEAN_AGE: u64 = 0x5D06;

/// The plate state: one record per planet cell (`c = cz · PLANET_NX + cx`)
/// and one entry per plate id below `PLATES_MAX`.
#[derive(Clone)]
pub struct PlateState {
    /// Plate id per cell.
    pub plate: Vec<u8>,
    /// `CrustKind` per cell.
    pub crust: Vec<u8>,
    /// When the crust formed, My.
    pub born_my: Vec<f32>,
    /// Continental base elevation, m (0 on oceanic crust).
    pub base_m: Vec<f32>,
    /// How strong the last orogeny was, 0 … 1.
    pub strength: Vec<f32>,
    /// When the last orogeny acted, My (`NEG_INFINITY` = never).
    pub orogeny_my: Vec<f32>,
    /// `OrogenyKind` per cell.
    pub kind: Vec<u8>,
    /// Unit direction of convergence (east, south); `[0, 0]` = none.
    pub fold: Vec<[f32; 2]>,
    /// The cell's mark (a subduction front or a collision contact): steps
    /// left, 0 = no mark. The other `mark_*` fields of a cell are read only
    /// while its `mark_ttl > 0` (they are not cleared when it falls to 0).
    /// A mark moves with its cell's record and is dropped with it.
    pub mark_ttl: Vec<u8>,
    /// A collision contact (`true`) or a subduction front.
    pub mark_contact: Vec<bool>,
    /// The overriding plate (the winner's) and the plate going under.
    pub mark_over: Vec<u8>,
    pub mark_under: Vec<u8>,
    /// `min(1, |vel_under − vel_over| / 2)`: a RELATIVE speed as a share of
    /// the largest possible (two plates each capped at 1 cell per step), not
    /// Ruling 1's per-plate cap.
    pub mark_speed: Vec<f32>,
    /// Cortial's `h(z̃) = z̃²` of the cell that went under (Ruling 12).
    pub mark_relief: Vec<f32>,
    /// `unit(vel_under − vel_over)` (east, south); `[0, 0]` for equal velocities.
    pub mark_dir: Vec<[f32; 2]>,
    /// Per plate: cells per step (east, south), each component in −1 … 1.
    pub vel: Vec<[f64; 2]>,
    /// Per plate: the fractional remainder of its motion.
    pub acc: Vec<[f64; 2]>,
    /// Per plate: the id is in use.
    pub alive: Vec<bool>,
    /// Per pair of plates `(O, U)` at `O · PLATES_MAX + U`: continental cells
    /// of `U` consumed under `O` since their last suture.
    pub pending: Vec<u32>,
    pub step: u32,
    pub counters: Counters,
    /// The destination arrays of a step (swapped in at its end).
    next: Next,
    /// Buffers of the field and the sutures (no state between steps).
    scratch: Scratch,
}

/// Work buffers reused across steps.
#[derive(Clone, Default)]
struct Scratch {
    dist: Vec<f64>,
    src: Vec<u32>,
    terrane: Vec<bool>,
    stack: Vec<u32>,
}

/// What a step writes before the swap, and pass A's winners.
#[derive(Clone, Default)]
struct Next {
    plate: Vec<u8>,
    crust: Vec<u8>,
    born_my: Vec<f32>,
    base_m: Vec<f32>,
    strength: Vec<f32>,
    orogeny_my: Vec<f32>,
    kind: Vec<u8>,
    fold: Vec<[f32; 2]>,
    mark_ttl: Vec<u8>,
    mark_contact: Vec<bool>,
    mark_over: Vec<u8>,
    mark_under: Vec<u8>,
    mark_speed: Vec<f32>,
    mark_relief: Vec<f32>,
    mark_dir: Vec<[f32; 2]>,
    /// Per destination: the best source cell seen (`NO_SOURCE` = none).
    winner: Vec<u32>,
    /// Per SOURCE cell: its destination this step (`NO_DEST` = past a pole).
    dest: Vec<u32>,
    /// This step's consumptions `(destination, consumed source)`, in pass B's
    /// (row-major source) order.
    consumed: Vec<(u32, u32)>,
}

const NO_SOURCE: u32 = u32::MAX;
const NO_DEST: u32 = u32::MAX;

#[derive(Default, Clone, Debug)]
pub struct Counters {
    /// Oceanic cells consumed where two met.
    pub subducted: u64,
    /// Continental cells consumed where two met.
    pub collided: u64,
    /// New oceanic cells made in gaps.
    pub created: u64,
    /// Cells moved past a pole, by crust.
    pub polar_lost_oceanic: u64,
    pub polar_lost_continental: u64,
    pub sutures: u32,
    pub rifts: u32,
}

impl PlateState {
    pub fn initial(key: &PlanetKey) -> PlateState {
        let seed = key.seed;
        let wrap = PeriodicX::for_width(360.0);
        let h = |salt: u64, index: u64| hash01(seed, salt, index);
        let deg: Vec<(f64, f64)> = (0..PLANET_CELLS).map(planet_deg).collect();
        let field = |salt: u64, wavelength: f64| {
            let n = PlanetNoise::new(seed, salt, wavelength);
            deg.iter().map(|&(lon, lat_s)| n.at(lon, lat_s)).collect::<Vec<f64>>()
        };
        let shape = field(N_SHAPE, N_SHAPE_DEG);
        let shape2 = field(N_SHAPE2, N_SHAPE_DEG);

        // The supercontinent: an ellipse warped by N_SHAPE, scaled by σ to
        // `SUPERCONTINENT_SHARE` of the cells (the final LOWER end's mask).
        let lon0 = 360.0 * h(H_SUPER, 0);
        let lat0 = -SUPERCONTINENT_LAT_DEG + 2.0 * SUPERCONTINENT_LAT_DEG * h(H_SUPER, 1);
        let supercontinent = |sigma: f64| -> Vec<bool> {
            let [ax, az] = SUPERCONTINENT_AXES_DEG;
            (0..PLANET_CELLS)
                .map(|c| {
                    let (lon, lat_s) = deg[c];
                    let dlon = wrap.dx(lon0, lon) / (ax * sigma);
                    let dlat = (90.0 - lat_s - lat0) / (az * sigma);
                    1.0 - dlon * dlon - dlat * dlat + OUTLINE_NOISE * shape[c] > 0.0
                })
                .collect()
        };
        let [mut lo, mut hi] = SUPERCONTINENT_SIGMA;
        for _ in 0..SUPERCONTINENT_HALVINGS {
            let mid = 0.5 * (lo + hi);
            let share = supercontinent(mid).iter().filter(|&&m| m).count() as f64 / PLANET_CELLS as f64;
            if share > SUPERCONTINENT_SHARE {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let main = supercontinent(lo);
        let mut continental = main.clone();

        // Microcontinents: ellipses away from all land so far.
        let micros = key.microcontinents.round().max(0.0) as u64;
        for m in 0..micros {
            let gap = chamfer(&continental, 1.0);
            for t in 0..MICRO_ATTEMPTS as u64 {
                // `96·m + 3·t + j` (m < 10, t < 32, j < 3): no two draws share an index.
                let hm = |j: u64| h(H_MICRO, 96 * m + 3 * t + j);
                let lon_c = 360.0 * hm(0);
                let lat_c = -MICRO_LAT_DEG + 2.0 * MICRO_LAT_DEG * hm(1);
                let share = MICRO_SHARE[0] + MICRO_SHARE[1] * hm(2);
                let b = (share * PLANET_DEG2 / (std::f64::consts::PI * MICRO_AXES_RATIO)).sqrt();
                let a = MICRO_AXES_RATIO * b;
                let cells: Vec<usize> = (0..PLANET_CELLS)
                    .filter(|&c| {
                        let (lon, lat_s) = deg[c];
                        let dlon = wrap.dx(lon_c, lon) / a;
                        let dlat = (90.0 - lat_s - lat_c) / b;
                        1.0 - dlon * dlon - dlat * dlat + OUTLINE_NOISE * shape[c] > 0.0
                    })
                    .collect();
                if cells.iter().all(|&c| gap[c] > MICRO_GAP_CELLS) {
                    for c in cells {
                        continental[c] = true;
                    }
                    break;
                }
            }
        }

        let base = field(N_BASE, N_BASE_DEG);
        let age = field(N_AGE, N_AGE_DEG);
        let mut crust = vec![CrustKind::Oceanic as u8; PLANET_CELLS];
        let mut base_m = vec![0.0f32; PLANET_CELLS];
        let mut born_my = vec![0.0f32; PLANET_CELLS];
        for c in 0..PLANET_CELLS {
            if continental[c] {
                crust[c] = CrustKind::Continental as u8;
                base_m[c] = (CONT_BASE_M + CONT_BASE_NOISE_M * base[c]) as f32;
                born_my[c] = (CONT_BORN_MY[0] - CONT_BORN_MY[1] * (0.5 + 0.5 * age[c]).clamp(0.0, 1.0)) as f32;
            } else {
                born_my[c] = (-OCEAN_BORN_SPREAD_MY * h(H_OCEAN_AGE, c as u64)) as f32;
            }
        }

        // Ancient sutures across the supercontinent: OLD collision belts.
        let mut strength = vec![0.0f32; PLANET_CELLS];
        let mut orogeny_my = vec![f32::NEG_INFINITY; PLANET_CELLS];
        let mut kind = vec![OrogenyKind::None as u8; PLANET_CELLS];
        let mut fold = vec![[0.0f32; 2]; PLANET_CELLS];
        let (mut sin_sum, mut cos_sum, mut lat_s_sum, mut n) = (0.0, 0.0, 0.0, 0usize);
        for c in 0..PLANET_CELLS {
            if main[c] {
                let (lon, lat_s) = deg[c];
                sin_sum += lon.to_radians().sin();
                cos_sum += lon.to_radians().cos();
                lat_s_sum += lat_s;
                n += 1;
            }
        }
        let centre_lon = wrap.fold(sin_sum.atan2(cos_sum).to_degrees());
        let centre_lat_s = lat_s_sum / n.max(1) as f64;
        for k in 0..ANCIENT_SUTURES as u64 {
            let hs = |j: u64| h(H_SUTURE, 4 * k + j);
            let mid_lon = centre_lon + SUTURE_OFFSET_DEG[0] + SUTURE_OFFSET_DEG[1] * hs(0);
            let angle = std::f64::consts::PI * hs(1);
            let half = 0.5 * (SUTURE_LENGTH_DEG[0] + SUTURE_LENGTH_DEG[1] * hs(2));
            let (ux, uz) = (angle.cos(), angle.sin());
            let normal = [-uz as f32, ux as f32];
            for c in 0..PLANET_CELLS {
                if !continental[c] {
                    continue;
                }
                let (lon, lat_s) = deg[c];
                let dx = wrap.dx(mid_lon, lon + SUTURE_WARP_DEG * shape[c]);
                let dz = lat_s + SUTURE_WARP_DEG * shape2[c] - centre_lat_s;
                let t = (dx * ux + dz * uz).clamp(-half, half);
                let d = (dx - t * ux).hypot(dz - t * uz);
                if d < SUTURE_HALF_WIDTH_DEG {
                    let r = d / SUTURE_HALF_WIDTH_DEG;
                    let s = SUTURE_STRENGTH * (1.0 - r * r) * (1.0 - r * r);
                    kind[c] = OrogenyKind::Collision as u8;
                    strength[c] = strength[c].max(s as f32);
                    orogeny_my[c] = 0.0;
                    fold[c] = normal;
                }
            }
        }

        // Plates: well-spaced sites, cells to the nearest from a warped position.
        let mut sites: Vec<(f64, f64)> = Vec::with_capacity(PLATES_INITIAL);
        let mut t = 1u64;
        while sites.len() < PLATES_INITIAL {
            let lon = 360.0 * h(H_SITE, 2 * t);
            let lat_s = 180.0 - 180.0 * h(H_SITE, 2 * t + 1); // lat = −90 + 180 · h1
            let far = sites.iter().all(|&(sx, sz)| wrap.dx(sx, lon).hypot(lat_s - sz) >= PLATE_SITE_GAP_DEG);
            if far || t > PLATE_SITE_ATTEMPTS {
                sites.push((lon, lat_s));
            }
            t += 1;
        }
        let mut plate = vec![0u8; PLANET_CELLS];
        for c in 0..PLANET_CELLS {
            let (lon, lat_s) = deg[c];
            let (px, pz) = (lon + PLATE_SITE_WARP_DEG * shape[c], lat_s + PLATE_SITE_WARP_DEG * shape2[c]);
            let mut best = (f64::INFINITY, 0usize);
            for (i, &(sx, sz)) in sites.iter().enumerate() {
                let dx = wrap.dx(sx, px);
                let d2 = dx * dx + (pz - sz) * (pz - sz);
                if d2 < best.0 {
                    best = (d2, i);
                }
            }
            plate[c] = best.1 as u8;
        }
        let mut cells = [0usize; PLATES_MAX];
        let mut cont = [0usize; PLATES_MAX];
        for c in 0..PLANET_CELLS {
            cells[plate[c] as usize] += 1;
            cont[plate[c] as usize] += continental[c] as usize;
        }
        let mut vel = vec![[0.0f64; 2]; PLATES_MAX];
        let mut alive = vec![false; PLATES_MAX];
        for i in 0..PLATES_INITIAL {
            let dir = 2.0 * std::f64::consts::PI * h(H_VEL, 2 * i as u64);
            let share = cont[i] as f64 / cells[i].max(1) as f64;
            let [s0, s1] = if share > SLOW_PLATE_CONT_SHARE { SLOW_SPEED } else { FAST_SPEED };
            let speed = s0 + s1 * h(H_VEL, 2 * i as u64 + 1);
            vel[i] = [speed * dir.cos(), speed * dir.sin()];
            alive[i] = true;
        }

        PlateState {
            plate,
            crust,
            born_my,
            base_m,
            strength,
            orogeny_my,
            kind,
            fold,
            mark_ttl: vec![0; PLANET_CELLS],
            mark_contact: vec![false; PLANET_CELLS],
            mark_over: vec![0; PLANET_CELLS],
            mark_under: vec![0; PLANET_CELLS],
            mark_speed: vec![0.0; PLANET_CELLS],
            mark_relief: vec![0.0; PLANET_CELLS],
            mark_dir: vec![[0.0; 2]; PLANET_CELLS],
            vel,
            acc: vec![[0.0; 2]; PLATES_MAX],
            alive,
            pending: vec![0; PLATES_MAX * PLATES_MAX],
            step: 0,
            counters: Counters::default(),
            next: Next::default(),
            scratch: Scratch::default(),
        }
    }

    /// One step of `PLATE_DT_MY` (the plan's step list; Task 3 adds items
    /// 6, 9–11): move, resolve overlaps and leave marks, fill gaps; every
    /// `FIELD_EVERY_STEPS` the subduction field; then the sutures.
    pub fn advance(&mut self, _key: &PlanetKey) {
        self.advance_in(0..PLANET_CELLS);
        if self.step % FIELD_EVERY_STEPS == 0 {
            self.subduction_field();
        }
        self.sutures();
    }

    /// `advance` with pass A visiting the source cells in `order` (a
    /// permutation of the cells): the result is the same for every order.
    fn advance_in(&mut self, order: impl Iterator<Item = usize>) {
        // 1. Whole-cell moves.
        let mut moves = [(0i32, 0i32); PLATES_MAX];
        for p in 0..PLATES_MAX {
            if !self.alive[p] {
                continue;
            }
            let mut d = [0i32; 2];
            for j in 0..2 {
                self.acc[p][j] += self.vel[p][j];
                let whole = self.acc[p][j].trunc();
                self.acc[p][j] -= whole;
                d[j] = whole as i32;
            }
            moves[p] = (d[0], d[1]);
        }
        // 2. Clear what is read before it is written: `next.plate` (the gap
        // fill looks for `NO_PLATE`) and `next.winner`. Pass B or the gap fill
        // writes every other field of every destination.
        let next = &mut self.next;
        next.plate.clear();
        next.plate.resize(PLANET_CELLS, NO_PLATE);
        next.winner.clear();
        next.winner.resize(PLANET_CELLS, NO_SOURCE);
        next.dest.resize(PLANET_CELLS, NO_DEST);
        for v in [&mut next.crust, &mut next.kind] {
            v.resize(PLANET_CELLS, 0);
        }
        for v in [&mut next.born_my, &mut next.base_m, &mut next.strength, &mut next.orogeny_my] {
            v.resize(PLANET_CELLS, 0.0);
        }
        next.fold.resize(PLANET_CELLS, [0.0; 2]);
        for v in [&mut next.mark_ttl, &mut next.mark_over, &mut next.mark_under] {
            v.resize(PLANET_CELLS, 0);
        }
        next.mark_contact.resize(PLANET_CELLS, false);
        next.mark_speed.resize(PLANET_CELLS, 0.0);
        next.mark_relief.resize(PLANET_CELLS, 0.0);
        next.mark_dir.resize(PLANET_CELLS, [0.0; 2]);
        // Pass A, first loop (row-major): each source's destination, once
        // (`NO_DEST` past a pole; the row wrap by a compare), and the per-plate
        // snapshot the contest reads. Every pass below works on local slices
        // (the borrows split by field), so nothing is re-loaded per cell.
        let Next {
            plate: n_plate,
            crust: n_crust,
            born_my: n_born,
            base_m: n_base,
            strength: n_strength,
            orogeny_my: n_orogeny,
            kind: n_kind,
            fold: n_fold,
            mark_ttl: n_ttl,
            mark_contact: n_contact,
            mark_over: n_over,
            mark_under: n_under,
            mark_speed: n_speed,
            mark_relief: n_relief,
            mark_dir: n_dir,
            winner,
            dest,
            consumed,
        } = &mut self.next;
        consumed.clear();
        let (plate, crust, born_my) =
            (&self.plate[..PLANET_CELLS], &self.crust[..PLANET_CELLS], &self.born_my[..PLANET_CELLS]);
        let (n_plate, winner, dest) =
            (&mut n_plate[..PLANET_CELLS], &mut winner[..PLANET_CELLS], &mut dest[..PLANET_CELLS]);
        let mut cont = [0u32; PLATES_MAX];
        // Per plate: `born_my` summed over its oceanic cells, and their count.
        let mut ocean_sum = [0.0f64; PLATES_MAX];
        let mut ocean_cells = [0u32; PLATES_MAX];
        // Sources past a pole, by crust (`[oceanic, continental]`).
        let mut lost = [0u64; 2];
        let nx = PLANET_NX as i32;
        for cz in 0..PLANET_NZ as i32 {
            let row = cz as usize * PLANET_NX;
            // A row is counted by runs of one plate (the counts stay in
            // registers; a plate's move is looked up once per run).
            let mut cx = 0i32;
            while cx < nx {
                let p = plate[row + cx as usize] as usize;
                let (dx, dz) = moves[p];
                let z = cz + dz;
                let polar = z < 0 || z >= PLANET_NZ as i32;
                let start = cx;
                let mut run_cont = 0u32;
                while cx < nx && plate[row + cx as usize] as usize == p {
                    let c = row + cx as usize;
                    if crust[c] == CrustKind::Continental as u8 {
                        run_cont += 1;
                    } else {
                        ocean_sum[p] += f64::from(born_my[c]);
                    }
                    dest[c] = if polar {
                        NO_DEST
                    } else {
                        let mut x = cx + dx;
                        if x < 0 {
                            x += nx;
                        } else if x >= nx {
                            x -= nx;
                        }
                        (z * nx + x) as u32
                    };
                    cx += 1;
                }
                let run = (cx - start) as u32;
                cont[p] += run_cont;
                ocean_cells[p] += run - run_cont;
                if polar {
                    lost[1] += u64::from(run_cont);
                    lost[0] += u64::from(run - run_cont);
                }
            }
        }
        let ocean: [f64; PLATES_MAX] = std::array::from_fn(|p| ocean_mean_born(ocean_sum[p], ocean_cells[p]));
        // Pass A, the contest: the winner of each destination under one
        // total order (the result does not depend on `order`).
        for c in order {
            let d = dest[c] as usize;
            if d < PLANET_CELLS {
                let w = winner[d];
                if w == NO_SOURCE || beats(plate, crust, &cont, &ocean, c, w as usize) {
                    winner[d] = c as u32;
                }
            }
        }
        // Pass B: winners move, the rest are consumed (lost at a pole: counted above).
        let (base_m, strength, orogeny_my, kind, fold) = (
            &self.base_m[..PLANET_CELLS],
            &self.strength[..PLANET_CELLS],
            &self.orogeny_my[..PLANET_CELLS],
            &self.kind[..PLANET_CELLS],
            &self.fold[..PLANET_CELLS],
        );
        let (n_crust, n_born, n_base, n_strength, n_orogeny, n_kind, n_fold) = (
            &mut n_crust[..PLANET_CELLS],
            &mut n_born[..PLANET_CELLS],
            &mut n_base[..PLANET_CELLS],
            &mut n_strength[..PLANET_CELLS],
            &mut n_orogeny[..PLANET_CELLS],
            &mut n_kind[..PLANET_CELLS],
            &mut n_fold[..PLANET_CELLS],
        );
        let (ttl, contact, over, under, speed, relief, dir) = (
            &self.mark_ttl[..PLANET_CELLS],
            &self.mark_contact[..PLANET_CELLS],
            &self.mark_over[..PLANET_CELLS],
            &self.mark_under[..PLANET_CELLS],
            &self.mark_speed[..PLANET_CELLS],
            &self.mark_relief[..PLANET_CELLS],
            &self.mark_dir[..PLANET_CELLS],
        );
        let (n_ttl, n_contact, n_over, n_under, n_speed, n_relief, n_dir) = (
            &mut n_ttl[..PLANET_CELLS],
            &mut n_contact[..PLANET_CELLS],
            &mut n_over[..PLANET_CELLS],
            &mut n_under[..PLANET_CELLS],
            &mut n_speed[..PLANET_CELLS],
            &mut n_relief[..PLANET_CELLS],
            &mut n_dir[..PLANET_CELLS],
        );
        let mut counters = self.counters.clone();
        counters.polar_lost_oceanic += lost[0];
        counters.polar_lost_continental += lost[1];
        for c in 0..PLANET_CELLS {
            // `NO_DEST` is above every cell: one compare skips it and lets
            // the compiler drop the bounds checks below.
            let d = dest[c] as usize;
            if d >= PLANET_CELLS {
                continue;
            }
            if winner[d] == c as u32 {
                n_plate[d] = plate[c];
                n_crust[d] = crust[c];
                n_born[d] = born_my[c];
                n_base[d] = base_m[c];
                n_strength[d] = strength[c];
                n_orogeny[d] = orogeny_my[c];
                n_kind[d] = kind[c];
                n_fold[d] = fold[c];
                // The step's ttl fall (step list item 5) is taken here: every
                // mark that moves loses one step; one written by this step's
                // consumptions (below) is written after, at the full ttl.
                let t = ttl[c];
                n_ttl[d] = t.saturating_sub(1);
                if t > 1 {
                    n_contact[d] = contact[c];
                    n_over[d] = over[c];
                    n_under[d] = under[c];
                    n_speed[d] = speed[c];
                    n_relief[d] = relief[c];
                    n_dir[d] = dir[c];
                }
            } else {
                consumed.push((d as u32, c as u32));
                if crust[c] == CrustKind::Continental as u8 {
                    counters.collided += 1;
                } else {
                    counters.subducted += 1;
                }
            }
        }
        // The consumptions' marks, after every winner moved (a mark is written
        // on the WINNER's record at the destination; with several losers on
        // one destination the last in row-major source order stays). An
        // oceanic loser leaves a subduction front, a continental one a
        // collision contact and counts toward its pair's suture.
        let now = self.step as f64 * PLATE_DT_MY;
        for &(d, c) in consumed.iter() {
            let (d, c) = (d as usize, c as usize);
            let (o, u) = (n_plate[d] as usize, plate[c] as usize);
            let dv = [self.vel[u][0] - self.vel[o][0], self.vel[u][1] - self.vel[o][1]];
            let elevation = cell_elevation_m(crust[c], born_my[c], base_m[c], strength[c], orogeny_my[c], kind[c], now);
            let z = (elevation - TRENCH_M) / (CONT_MAX_M - TRENCH_M);
            let is_contact = crust[c] == CrustKind::Continental as u8;
            if is_contact {
                let p = &mut self.pending[o * PLATES_MAX + u];
                *p = p.saturating_add(1);
            }
            n_ttl[d] = FRONT_TTL_STEPS;
            n_contact[d] = is_contact;
            n_over[d] = o as u8;
            n_under[d] = u as u8;
            n_speed[d] = (0.5 * dv[0].hypot(dv[1])).min(1.0) as f32;
            n_relief[d] = (z * z) as f32;
            n_dir[d] = unit(dv);
        }
        // 3. Gaps: new oceanic crust, born at the step's end, owned by the
        // plate that held the cell before the step (two plates moving apart
        // each fill their own wake: the ridge is where their boundary stood).
        let time = (self.step + 1) as f64 * PLATE_DT_MY;
        for d in 0..PLANET_CELLS {
            if n_plate[d] != NO_PLATE {
                continue;
            }
            n_plate[d] = plate[d];
            n_crust[d] = CrustKind::Oceanic as u8;
            n_born[d] = time as f32;
            n_base[d] = 0.0;
            n_strength[d] = 0.0;
            n_orogeny[d] = f32::NEG_INFINITY;
            n_kind[d] = OrogenyKind::None as u8;
            n_fold[d] = [0.0; 2];
            n_ttl[d] = 0;
            counters.created += 1;
        }
        self.counters = counters;
        // 4. Swap.
        let next = &mut self.next;
        std::mem::swap(&mut self.plate, &mut next.plate);
        std::mem::swap(&mut self.crust, &mut next.crust);
        std::mem::swap(&mut self.born_my, &mut next.born_my);
        std::mem::swap(&mut self.base_m, &mut next.base_m);
        std::mem::swap(&mut self.strength, &mut next.strength);
        std::mem::swap(&mut self.orogeny_my, &mut next.orogeny_my);
        std::mem::swap(&mut self.kind, &mut next.kind);
        std::mem::swap(&mut self.fold, &mut next.fold);
        std::mem::swap(&mut self.mark_ttl, &mut next.mark_ttl);
        std::mem::swap(&mut self.mark_contact, &mut next.mark_contact);
        std::mem::swap(&mut self.mark_over, &mut next.mark_over);
        std::mem::swap(&mut self.mark_under, &mut next.mark_under);
        std::mem::swap(&mut self.mark_speed, &mut next.mark_speed);
        std::mem::swap(&mut self.mark_relief, &mut next.mark_relief);
        std::mem::swap(&mut self.mark_dir, &mut next.mark_dir);
        self.step += 1;
    }

    /// Model time, My.
    pub fn time_my(&self) -> f64 {
        self.step as f64 * PLATE_DT_MY
    }

    /// The subduction belts (step list item 7): a chamfer from every live
    /// front mark carries each cell its nearest mark; a cell of that mark's
    /// overriding plate within `SUBDUCTION_REACH_DEG` gains
    /// `(u0 / 6,000) · f(d) · speed · relief · polar_fade · (FIELD_EVERY_STEPS · PLATE_DT_MY)`,
    /// `f` rising over the first `SUBDUCTION_PEAK_SHARE` of the reach and
    /// falling to 0 at it. Continental cells become `Andean`, oceanic `Arc`;
    /// an arc reaching `ARC_EMERGE_STRENGTH` becomes continental crust.
    fn subduction_field(&mut self) {
        let time = self.time_my();
        let reach = SUBDUCTION_REACH_DEG;
        let peak = SUBDUCTION_PEAK_SHARE * reach;
        let rate = SUBDUCTION_U0_M_PER_MY / OROGEN_HEIGHT_M[OrogenyKind::Andean as usize]
            * (f64::from(FIELD_EVERY_STEPS) * PLATE_DT_MY);
        let Scratch { dist, src, .. } = &mut self.scratch;
        let (ttl, contact) = (&self.mark_ttl, &self.mark_contact);
        chamfer_into(|c| ttl[c] > 0 && !contact[c], PLANET_DEG_PER_CELL, reach, dist, src);
        for c in 0..PLANET_CELLS {
            let d = dist[c];
            if d >= reach {
                continue;
            }
            let m = src[c] as usize;
            if self.plate[c] != self.mark_over[m] {
                continue;
            }
            let f = if d < peak { smoothstep(0.0, peak, d) } else { 1.0 - smoothstep(peak, reach, d) };
            let lat = planet_lat_deg(c / PLANET_NX);
            let gain = rate
                * f
                * f64::from(self.mark_speed[m])
                * f64::from(self.mark_relief[m])
                * polar_fade(lat);
            if gain <= 1e-5 {
                continue;
            }
            self.strength[c] = orogeny_strength(self.strength[c], self.orogeny_my[c], time, gain);
            self.orogeny_my[c] = time as f32;
            self.fold[c] = self.mark_dir[m];
            if self.crust[c] == CrustKind::Continental as u8 {
                self.kind[c] = OrogenyKind::Andean as u8;
            } else {
                self.kind[c] = OrogenyKind::Arc as u8;
                if self.strength[c] >= ARC_EMERGE_STRENGTH {
                    self.crust[c] = CrustKind::Continental as u8;
                    self.base_m[c] = 0.0;
                }
            }
        }
    }

    /// Step list item 8: every pair `(O, U)` with `pending ≥
    /// SUTURE_CONSUMED_CELLS`, ascending, sutures.
    fn sutures(&mut self) {
        for o in 0..PLATES_MAX {
            for u in 0..PLATES_MAX {
                if self.pending[o * PLATES_MAX + u] >= SUTURE_CONSUMED_CELLS {
                    self.suture(o, u);
                }
            }
        }
    }

    /// The suture of `U`'s terrane onto `O` (the plan's Collision rule). The
    /// terrane `R`: the continental cells of `U` 8-connected (x wraps) to a
    /// continental cell of `U` that is an 8-neighbour of a live contact mark
    /// of the pair. An empty `R` (the colliding crust is all consumed) makes
    /// no suture and clears the pair's count.
    fn suture(&mut self, o: usize, u: usize) {
        let time = self.time_my();
        self.pending[o * PLATES_MAX + u] = 0;
        let continental = CrustKind::Continental as u8;
        let Scratch { dist, src, terrane, stack } = &mut self.scratch;
        terrane.clear();
        terrane.resize(PLANET_CELLS, false);
        stack.clear();
        let in_u = |c: usize| self.plate[c] as usize == u && self.crust[c] == continental;
        for c in 0..PLANET_CELLS {
            let is_mark = self.mark_ttl[c] > 0
                && self.mark_contact[c]
                && self.mark_over[c] as usize == o
                && self.mark_under[c] as usize == u;
            if !is_mark {
                continue;
            }
            for (dx, dz) in NEIGHBOURS_8 {
                if let Some(n) = planet_step(c, dx, dz) {
                    if in_u(n) && !terrane[n] {
                        terrane[n] = true;
                        stack.push(n as u32);
                    }
                }
            }
        }
        while let Some(c) = stack.pop() {
            for (dx, dz) in NEIGHBOURS_8 {
                if let Some(n) = planet_step(c as usize, dx, dz) {
                    if in_u(n) && !terrane[n] {
                        terrane[n] = true;
                        stack.push(n as u32);
                    }
                }
            }
        }
        // Size and centroid (longitude by circular mean), row-major.
        let (mut n_r, mut sin_sum, mut cos_sum, mut lat_s_sum) = (0usize, 0.0f64, 0.0f64, 0.0f64);
        for c in 0..PLANET_CELLS {
            if terrane[c] {
                let (lon, lat_s) = planet_deg(c);
                sin_sum += lon.to_radians().sin();
                cos_sum += lon.to_radians().cos();
                lat_s_sum += lat_s;
                n_r += 1;
            }
        }
        if n_r == 0 {
            return;
        }
        let wrap = PeriodicX::for_width(360.0);
        let q = (wrap.fold(sin_sum.atan2(cos_sum).to_degrees()), lat_s_sum / n_r as f64);
        let q_cell = {
            let qx = ((q.0 / PLANET_DEG_PER_CELL) as usize).min(PLANET_NX - 1);
            let qz = ((q.1 / PLANET_DEG_PER_CELL) as usize).min(PLANET_NZ - 1);
            qz * PLANET_NX + qx
        };
        let area = n_r as f64 * PLANET_DEG_PER_CELL * PLANET_DEG_PER_CELL;
        let dv = [self.vel[u][0] - self.vel[o][0], self.vel[u][1] - self.vel[o][1]];
        let speed = (0.5 * dv[0].hypot(dv[1])).min(1.0);
        let a0 = PLANET_DEG2 / PLATES_INITIAL as f64;
        let r = (COLLISION_RADIUS_MAX_DEG * (speed * area / a0).sqrt())
            .clamp(COLLISION_RADIUS_MIN_DEG, COLLISION_RADIUS_MAX_DEG);
        let peak = (COLLISION_GAIN_PER_DEG2 * area).min(1.0);
        let kind =
            if area >= PLATEAU_MIN_DEG2 { OrogenyKind::Plateau as u8 } else { OrogenyKind::Collision as u8 };
        chamfer_into(|c| terrane[c], PLANET_DEG_PER_CELL, r, dist, src);
        let mut n_o = 0usize;
        for c in 0..PLANET_CELLS {
            if self.crust[c] != continental {
                continue;
            }
            let of_o = self.plate[c] as usize == o;
            n_o += of_o as usize;
            let d = dist[c];
            if d >= r || !(of_o || terrane[c]) {
                continue;
            }
            let t = 1.0 - (d / r) * (d / r);
            let gain = peak * t * t * polar_fade(planet_lat_deg(c / PLANET_NX));
            self.strength[c] = orogeny_strength(self.strength[c], self.orogeny_my[c], time, gain);
            self.orogeny_my[c] = time as f32;
            self.kind[c] = kind;
            self.fold[c] = if c == q_cell {
                [0.0; 2]
            } else {
                let (lon, lat_s) = planet_deg(c);
                unit([wrap.dx(q.0, lon), lat_s - q.1])
            };
        }
        let mut u_cells = 0usize;
        for c in 0..PLANET_CELLS {
            if terrane[c] {
                self.plate[c] = o as u8;
            } else if self.plate[c] as usize == u {
                u_cells += 1;
            }
        }
        if u_cells == 0 {
            self.alive[u] = false;
        }
        let (wo, wr) = (n_o as f64, n_r as f64);
        for j in 0..2 {
            self.vel[o][j] = (self.vel[o][j] * wo + self.vel[u][j] * wr) / (wo + wr) * COLLISION_BRAKE;
        }
        self.counters.sutures += 1;
    }
}

/// The 8 neighbours as `(dx, dz)`, row-major.
const NEIGHBOURS_8: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

/// How much of an orogen's crustal root is left after `age_my` (the
/// vocabulary's `root_keep`; 1 at age 0, `ROOT_RESIDUAL` for ever).
pub fn root_keep(age_my: f64) -> f64 {
    ROOT_RESIDUAL + (1.0 - ROOT_RESIDUAL) * (-age_my / ROOT_DECAY_MY).exp()
}

/// The factor on every orogeny gain near the poles (Ruling 10): 1 away from
/// them. Task 3 gives it its ramp over the last `POLAR_FADE_DEG`; until then 1.
pub fn polar_fade(_lat_deg: f64) -> f64 {
    1.0
}

/// The vocabulary's model elevation of cell `c`, m, at the state's current
/// time (`time_my`): oceanic crust sinks from the ridge to the abyss with its
/// age, continental crust stands at its `base_m`; either carries its orogen's
/// `OROGEN_HEIGHT_M[kind] · strength · root_keep(orogeny_age)` — a `Hotspot`
/// `7,000 · strength · exp(−orogeny_age / HOTSPOT_SUBSIDE_MY)` instead.
pub fn elevation_m(state: &PlateState, c: usize) -> f64 {
    cell_elevation_m(
        state.crust[c],
        state.born_my[c],
        state.base_m[c],
        state.strength[c],
        state.orogeny_my[c],
        state.kind[c],
        state.time_my(),
    )
}

/// `elevation_m` of one cell's record at `time`.
fn cell_elevation_m(crust: u8, born_my: f32, base_m: f32, strength: f32, orogeny_my: f32, kind: u8, time: f64) -> f64 {
    let ground = if crust == CrustKind::Continental as u8 {
        f64::from(base_m)
    } else {
        let age = time - f64::from(born_my);
        OCEAN_ABYSS_M + (OCEAN_RIDGE_M - OCEAN_ABYSS_M) * (-age / OCEAN_SUBSIDE_MY).exp()
    };
    let orogeny_age = time - f64::from(orogeny_my);
    let height = OROGEN_HEIGHT_M[kind as usize] * f64::from(strength);
    let orogen = if kind == OrogenyKind::Hotspot as u8 {
        height * (-orogeny_age / HOTSPOT_SUBSIDE_MY).exp()
    } else {
        height * root_keep(orogeny_age)
    };
    ground + orogen
}

/// One strength, two decays: an orogeny acting at `time` with `gain` on a
/// cell whose last orogeny was `strength` at `orogeny_my` leaves
/// `min(1, strength · root_keep(time − orogeny_my) + gain)` (a never-active
/// cell's strength is 0, so its root does not matter).
fn orogeny_strength(strength: f32, orogeny_my: f32, time: f64, gain: f64) -> f32 {
    (f64::from(strength) * root_keep(time - f64::from(orogeny_my)) + gain).min(1.0) as f32
}

/// `v` over its length, `[0, 0]` for a zero vector (never NaN).
fn unit(v: [f64; 2]) -> [f32; 2] {
    let len = v[0].hypot(v[1]);
    if len > 0.0 { [(v[0] / len) as f32, (v[1] / len) as f32] } else { [0.0; 2] }
}

/// A plate's mean `born_my` over its oceanic cells (`sum` over `count`);
/// `NEG_INFINITY` for a plate with none (never NaN). The `f64` sum is NOT
/// exact (rounding depends on the order of the additions); it is the same on
/// every run because pass A adds the cells in a pinned order, row-major.
fn ocean_mean_born(sum: f64, count: u32) -> f64 {
    if count == 0 { f64::NEG_INFINITY } else { sum / f64::from(count) }
}

/// Source `a` beats source `b` for one destination: continental beats
/// oceanic; between continental cells the plate with more continental cells
/// in the snapshot `cont`; between oceanic cells the plate whose sea floor is
/// younger on average (larger `ocean`, its `ocean_mean_born` — one answer per
/// pair of plates per step, so a boundary is consumed as a line); ties the
/// lower plate id. Two sources of one destination are always of different
/// plates, so this is a total order on them.
fn beats(plate: &[u8], crust: &[u8], cont: &[u32; PLATES_MAX], ocean: &[f64; PLATES_MAX], a: usize, b: usize) -> bool {
    let continental = CrustKind::Continental as u8;
    let (ca, cb) = (crust[a] == continental, crust[b] == continental);
    if ca != cb {
        return ca;
    }
    let (pa, pb) = (plate[a], plate[b]);
    if ca {
        let (na, nb) = (cont[pa as usize], cont[pb as usize]);
        if na != nb {
            return na > nb;
        }
    } else {
        let (ma, mb) = (ocean[pa as usize], ocean[pb as usize]);
        if ma != mb {
            return ma > mb;
        }
    }
    pa < pb
}

/// Chamfer distance (steps of `unit` to the 4 edge neighbours, `√2 · unit` to
/// the diagonal ones) from every cell to the nearest `seed` cell: a forward
/// row-major pass (west, north-west, north, north-east) then a backward one
/// (east, south-east, south, south-west), x wrapping, the pair repeated until
/// nothing changes (at most 3 pairs); a cell takes a neighbour's distance only
/// when strictly smaller. `INFINITY` with no seed.
fn chamfer(seed: &[bool], unit: f64) -> Vec<f64> {
    let (mut d, mut src) = (Vec::new(), Vec::new());
    chamfer_into(|c| seed[c], unit, f64::INFINITY, &mut d, &mut src);
    d
}

/// `chamfer` into `d`, with `src[c]` the seed cell whose distance `c` took (a
/// cell takes a neighbour's distance AND source only when strictly smaller,
/// in the order of the passes' neighbour lists, so ties keep what was written
/// first). No value at or above `cap` is written (such a cell stays
/// `INFINITY`): every distance below `cap`, and its source, is exactly the
/// uncapped one — along a chamfer path the sums only grow, so a value below
/// the cap is built only from values below it. For the same reason the passes
/// skip every `CHAMFER_TILE`² tile farther from all seed tiles (Chebyshev,
/// x wrapping) than a cap's worth of cells: no cell there can come below the
/// cap (every step costs at least `unit`), so it would never change.
fn chamfer_into(seed: impl Fn(usize) -> bool, unit: f64, cap: f64, d: &mut Vec<f64>, src: &mut Vec<u32>) {
    const PAIRS_MAX: usize = 3;
    const NX: usize = PLANET_NX;
    const T: usize = CHAMFER_TILE;
    const TX: usize = PLANET_NX / CHAMFER_TILE;
    const TZ: usize = PLANET_NZ / CHAMFER_TILE;
    let diag = std::f64::consts::SQRT_2 * unit;
    d.clear();
    d.resize(PLANET_CELLS, f64::INFINITY);
    src.clear();
    src.extend(0..PLANET_CELLS as u32);
    // Tiles that can hold a value below the cap: within `out` tiles of a
    // tile holding a seed, where `out · T + 1` cells cost at least the cap.
    let mut seeded = [false; TX * TZ];
    for c in 0..PLANET_CELLS {
        if seed(c) {
            d[c] = 0.0;
            seeded[(c / NX / T) * TX + (c % NX) / T] = true;
        }
    }
    let cells = cap / unit;
    let out = if cells.is_finite() { ((cells - 1.0) / T as f64).ceil().max(0.0) as usize } else { TX };
    let mut active = [false; TX * TZ];
    for tz in 0..TZ {
        for tx in 0..TX {
            if !seeded[tz * TX + tx] {
                continue;
            }
            for az in tz.saturating_sub(out)..(tz + out + 1).min(TZ) {
                if out * 2 + 1 >= TX {
                    active[az * TX..(az + 1) * TX].fill(true);
                    continue;
                }
                for k in 0..=2 * out {
                    active[az * TX + (tx + TX + k - out) % TX] = true;
                }
            }
        }
    }
    let none_d = [f64::INFINITY; NX];
    let none_s = [0u32; NX];
    // Each pass visits the active tiles' cells in its order, one row span
    // of `T` cells at a time; a cell's neighbour along the row (the one the
    // scan comes from) rides in registers. A span whose inputs did not change
    // since the same pass last visited it is skipped — none of its cells
    // could change: its inputs are its own cells, the span the scan enters it
    // from, and the three spans of the row it reads. `stamp` = the visit (a
    // clock) that last changed a span, `seen` = each pass's last visit.
    const SPANS: usize = PLANET_NZ * TX;
    let mut clock = 1u32;
    let mut stamp = vec![1u32; SPANS];
    let mut seen = [vec![0u32; SPANS], vec![0u32; SPANS]];
    #[inline(always)]
    fn take(best: &mut f64, from: &mut u32, v: f64, s: u32, cap: f64) {
        let ok = (v < *best) & (v < cap);
        *best = if ok { v } else { *best };
        *from = if ok { s } else { *from };
    }
    for _ in 0..PAIRS_MAX {
        let mut changed = false;
        // Forward: west, north-west, north, north-east.
        for z in 0..PLANET_NZ {
            let row = z * NX;
            let (d_head, d_tail) = d.split_at_mut(row);
            let (s_head, s_tail) = src.split_at_mut(row);
            let cur: &mut [f64; NX] = (&mut d_tail[..NX]).try_into().unwrap();
            let s_cur: &mut [u32; NX] = (&mut s_tail[..NX]).try_into().unwrap();
            let (up, s_up): (&[f64; NX], &[u32; NX]) = if z > 0 {
                ((&d_head[row - NX..]).try_into().unwrap(), (&s_head[row - NX..]).try_into().unwrap())
            } else {
                (&none_d, &none_s)
            };
            for tx in 0..TX {
                if !active[(z / T) * TX + tx] {
                    continue;
                }
                let k = z * TX + tx;
                let (txw, txe) = ((tx + TX - 1) % TX, (tx + 1) % TX);
                let last = seen[0][k];
                let mut fresh = stamp[k] > last || stamp[z * TX + txw] > last;
                if z > 0 {
                    let u = k - TX;
                    fresh |= stamp[u] > last || stamp[z * TX - TX + txw] > last || stamp[z * TX - TX + txe] > last;
                }
                if !fresh {
                    continue;
                }
                clock += 1;
                seen[0][k] = clock;
                let x0 = tx * T;
                let xw0 = if x0 == 0 { NX - 1 } else { x0 - 1 };
                let (mut along, mut s_along) = (cur[xw0], s_cur[xw0]);
                let mut span_changed = false;
                for x in x0..x0 + T {
                    let xw = if x == 0 { NX - 1 } else { x - 1 };
                    let xe = if x == NX - 1 { 0 } else { x + 1 };
                    let (mut best, mut from) = (cur[x], s_cur[x]);
                    take(&mut best, &mut from, along + unit, s_along, cap);
                    take(&mut best, &mut from, up[xw] + diag, s_up[xw], cap);
                    take(&mut best, &mut from, up[x] + unit, s_up[x], cap);
                    take(&mut best, &mut from, up[xe] + diag, s_up[xe], cap);
                    span_changed |= best < cur[x];
                    cur[x] = best;
                    s_cur[x] = from;
                    (along, s_along) = (best, from);
                }
                if span_changed {
                    stamp[k] = clock;
                    changed = true;
                }
            }
        }
        // Backward: east, south-east, south, south-west.
        for z in (0..PLANET_NZ).rev() {
            let row = z * NX;
            let (d_head, d_tail) = d.split_at_mut(row + NX);
            let (s_head, s_tail) = src.split_at_mut(row + NX);
            let cur: &mut [f64; NX] = (&mut d_head[row..]).try_into().unwrap();
            let s_cur: &mut [u32; NX] = (&mut s_head[row..]).try_into().unwrap();
            let (down, s_down): (&[f64; NX], &[u32; NX]) = if z + 1 < PLANET_NZ {
                ((&d_tail[..NX]).try_into().unwrap(), (&s_tail[..NX]).try_into().unwrap())
            } else {
                (&none_d, &none_s)
            };
            for tx in (0..TX).rev() {
                if !active[(z / T) * TX + tx] {
                    continue;
                }
                let k = z * TX + tx;
                let (txw, txe) = ((tx + TX - 1) % TX, (tx + 1) % TX);
                let last = seen[1][k];
                let mut fresh = stamp[k] > last || stamp[z * TX + txe] > last;
                if z + 1 < PLANET_NZ {
                    let b = k + TX;
                    fresh |= stamp[b] > last || stamp[z * TX + TX + txw] > last || stamp[z * TX + TX + txe] > last;
                }
                if !fresh {
                    continue;
                }
                clock += 1;
                seen[1][k] = clock;
                let x1 = (tx + 1) * T - 1;
                let xe1 = if x1 == NX - 1 { 0 } else { x1 + 1 };
                let (mut along, mut s_along) = (cur[xe1], s_cur[xe1]);
                let mut span_changed = false;
                for x in (tx * T..=x1).rev() {
                    let xw = if x == 0 { NX - 1 } else { x - 1 };
                    let xe = if x == NX - 1 { 0 } else { x + 1 };
                    let (mut best, mut from) = (cur[x], s_cur[x]);
                    take(&mut best, &mut from, along + unit, s_along, cap);
                    take(&mut best, &mut from, down[xe] + diag, s_down[xe], cap);
                    take(&mut best, &mut from, down[x] + unit, s_down[x], cap);
                    take(&mut best, &mut from, down[xw] + diag, s_down[xw], cap);
                    span_changed |= best < cur[x];
                    cur[x] = best;
                    s_cur[x] = from;
                    (along, s_along) = (best, from);
                }
                if span_changed {
                    stamp[k] = clock;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// The chamfer's skipping tile, cells (divides both grid sides).
const CHAMFER_TILE: usize = 16;

/// `initial`, then `advance` until `key.plate_duration_my`; `on_frame` sees the state every `every` steps (0 = never).
pub fn simulate(key: &PlanetKey, every: u32, on_frame: &mut dyn FnMut(&PlateState)) -> PlateState {
    let steps = (key.plate_duration_my / PLATE_DT_MY).round() as u32;
    let mut state = PlateState::initial(key);
    while state.step < steps {
        state.advance(key);
        if every > 0 && state.step % every == 0 {
            on_frame(&state);
        }
    }
    state
}
