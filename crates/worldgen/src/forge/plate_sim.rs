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
    planet_deg, planet_step, CrustKind, OrogenyKind, PlanetKey, PlanetNoise, PLANET_CELLS, PLANET_NX, PLANET_NZ,
};
use crate::forge::world::hash01;

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
    /// Per plate: cells per step (east, south), each component in −1 … 1.
    pub vel: Vec<[f64; 2]>,
    /// Per plate: the fractional remainder of its motion.
    pub acc: Vec<[f64; 2]>,
    /// Per plate: the id is in use.
    pub alive: Vec<bool>,
    pub step: u32,
    pub counters: Counters,
    /// The destination arrays of a step (swapped in at its end).
    next: Next,
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
    /// Per destination: the best source cell seen (`NO_SOURCE` = none).
    winner: Vec<u32>,
    /// Per SOURCE cell: its destination this step (`NO_DEST` = past a pole).
    dest: Vec<u32>,
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
                let hm = |j: u64| h(H_MICRO, 8 * m + j + 3 * t);
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
            vel,
            acc: vec![[0.0; 2]; PLATES_MAX],
            alive,
            step: 0,
            counters: Counters::default(),
            next: Next::default(),
        }
    }

    /// One step of `PLATE_DT_MY`: move, resolve overlaps, fill gaps (Tasks 2 and 3 add to it).
    pub fn advance(&mut self, _key: &PlanetKey) {
        self.advance_in(0..PLANET_CELLS);
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
            winner,
            dest,
        } = &mut self.next;
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
            } else if crust[c] == CrustKind::Continental as u8 {
                counters.collided += 1;
            } else {
                counters.subducted += 1;
            }
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
        self.step += 1;
    }

    /// Model time, My.
    pub fn time_my(&self) -> f64 {
        self.step as f64 * PLATE_DT_MY
    }
}

/// A plate's mean `born_my` over its oceanic cells (`sum` over `count`);
/// `NEG_INFINITY` for a plate with none (never NaN). The sum is exact in
/// `f64` (at most 131,072 `f32` values below 2^12 in magnitude), so it does
/// not depend on the order it was added in.
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
    const PAIRS_MAX: usize = 3;
    let diag = std::f64::consts::SQRT_2 * unit;
    let forward = [(-1, 0, unit), (-1, -1, diag), (0, -1, unit), (1, -1, diag)];
    let backward = [(1, 0, unit), (1, 1, diag), (0, 1, unit), (-1, 1, diag)];
    let mut d: Vec<f64> = seed.iter().map(|&s| if s { 0.0 } else { f64::INFINITY }).collect();
    let relax = |d: &mut Vec<f64>, c: usize, dirs: &[(i32, i32, f64); 4]| -> bool {
        let mut changed = false;
        for &(dx, dz, w) in dirs {
            if let Some(n) = planet_step(c, dx, dz) {
                let v = d[n] + w;
                if v < d[c] {
                    d[c] = v;
                    changed = true;
                }
            }
        }
        changed
    };
    for _ in 0..PAIRS_MAX {
        let mut changed = false;
        for c in 0..PLANET_CELLS {
            changed |= relax(&mut d, c, &forward);
        }
        for c in (0..PLANET_CELLS).rev() {
            changed |= relax(&mut d, c, &backward);
        }
        if !changed {
            break;
        }
    }
    d
}

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
