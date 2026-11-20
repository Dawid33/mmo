//! The planet grid (Phase 2): one coarse `PLANET_NX` × `PLANET_NZ` raster of
//! the whole world, the same on every world size and forge cell, wrapping
//! east–west (column `PLANET_NX − 1` is beside column 0) and ending at the
//! poles (row 0 is the north edge). Every planetary length is in degrees of
//! latitude; a cell is `PLANET_DEG_PER_CELL` degrees both ways. The plate
//! simulation (`plate_sim`) runs on it and reads only a [`PlanetKey`].
//! Plan: docs/superpowers/plans/2026-11-20-plates-and-aged-relief-2.md.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::forge::belts::{self, Belt, Continent, PlanetScore};
use crate::forge::periodic::PeriodicX;
use crate::forge::plate_sim::{self, Counters};
use crate::forge::world::{smoothstep, ForgeParams};
use crate::noise::OctaveNoise;

/// Young vs middle belt boundary, My.
pub const YOUNG_MAX_MY: f64 = 80.0;
/// Middle vs old belt boundary, My.
pub const OLD_MIN_MY: f64 = 200.0;
/// Residual uplift activity retained by an old belt (untested design).
pub const BELT_RESIDUAL: f64 = 0.12;
/// Uplift activity e-folding for hotspots, My (untested design).
pub const HOTSPOT_DECAY_MY: f64 = 12.0;
/// Maximum plate-stage attempts per seed (Ruling 9).
pub const PLATE_ATTEMPTS_MAX: u8 = 6;

/// Planet grid columns (x, west to east; wraps). Ruling 2: one size for
/// every world.
pub const PLANET_NX: usize = 512;
/// Planet grid rows (z, north to south; ends at the poles).
pub const PLANET_NZ: usize = 256;
/// Cells of the planet grid.
pub const PLANET_CELLS: usize = PLANET_NX * PLANET_NZ;
/// Degrees of latitude (and of longitude, on a 2:1 world) per cell.
pub const PLANET_DEG_PER_CELL: f64 = 180.0 / PLANET_NZ as f64;

/// The cell `dx` columns east and `dz` rows south of `c`: x wraps, `None` past a pole.
pub fn planet_step(c: usize, dx: i32, dz: i32) -> Option<usize> {
    let (cx, cz) = ((c % PLANET_NX) as i32, (c / PLANET_NX) as i32);
    let z = cz + dz;
    if z < 0 || z >= PLANET_NZ as i32 {
        return None;
    }
    let x = (cx + dx).rem_euclid(PLANET_NX as i32);
    Some(z as usize * PLANET_NX + x as usize)
}

/// Latitude of row `cz`'s centre, degrees, north positive.
pub fn planet_lat_deg(cz: usize) -> f64 {
    90.0 - (cz as f64 + 0.5) * PLANET_DEG_PER_CELL
}

/// Degrees east of the west edge / south of the north edge of a cell's centre: ((cx + 0.5) * DEG, (cz + 0.5) * DEG).
pub fn planet_deg(c: usize) -> (f64, f64) {
    let (cx, cz) = (c % PLANET_NX, c / PLANET_NX);
    ((cx as f64 + 0.5) * PLANET_DEG_PER_CELL, (cz as f64 + 0.5) * PLANET_DEG_PER_CELL)
}

/// The kind of crust of a planet cell (stored bytes: append only).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrustKind {
    Oceanic = 0,
    Continental = 1,
}

/// The kind of the last orogeny of a planet cell (stored bytes: append only).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrogenyKind {
    None = 0,
    Arc = 1,
    Andean = 2,
    Collision = 3,
    Plateau = 4,
    RiftShoulder = 5,
    Hotspot = 6,
}

/// What the plate stage reads: nothing else may reach it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanetKey {
    pub seed: u64,
    pub land_fraction: f64,
    pub plate_duration_my: f64,
    pub rift_rate: f64,
    pub microcontinents: f64,
    pub belt_decay_my: f64,
    pub ocean_margin_start: f64,
}

/// The five plate numbers until they are knobs (Task 7): land_fraction 0.32, plate_duration_my 450.0,
/// rift_rate 1.0, microcontinents 4.0, belt_decay_my 120.0.
pub const KEY_DEFAULTS: [(&str, f64); 5] = [
    ("land_fraction", 0.32),
    ("plate_duration_my", 450.0),
    ("rift_rate", 1.0),
    ("microcontinents", 4.0),
    ("belt_decay_my", 120.0),
];

impl PlanetKey {
    /// The seed and `ocean_margin_start` from `p`; the rest `KEY_DEFAULTS` until Task 7.
    pub fn of(p: &ForgeParams) -> PlanetKey {
        let default = |name: &str| {
            KEY_DEFAULTS.iter().find(|(k, _)| *k == name).map(|&(_, v)| v).expect("a KEY_DEFAULTS name")
        };
        PlanetKey {
            seed: p.seed,
            land_fraction: default("land_fraction"),
            plate_duration_my: default("plate_duration_my"),
            rift_rate: default("rift_rate"),
            microcontinents: default("microcontinents"),
            belt_decay_my: default("belt_decay_my"),
            ocean_margin_start: p.knobs.ocean_margin_start,
        }
    }
}

/// A noise on the planet: `OctaveNoise::new(seed ^ salt, -5, vec![1.0, 0.5])` (first wavelength 32 units) sampled
/// at `(lon_deg * 32.0 / wavelength_deg, lat_deg * 32.0 / wavelength_deg)` and made periodic in longitude by
/// `PeriodicX::for_width(360.0).for_noise(wavelength_deg).sample(lon_deg, lat_deg, …)`.
pub struct PlanetNoise {
    noise: OctaveNoise,
    /// Noise units per degree: `32 / wavelength_deg`.
    scale: f64,
    wrap: PeriodicX,
}

impl PlanetNoise {
    pub fn new(seed: u64, salt: u64, wavelength_deg: f64) -> PlanetNoise {
        PlanetNoise {
            noise: OctaveNoise::new(seed ^ salt, -5, vec![1.0, 0.5]),
            scale: 32.0 / wavelength_deg,
            wrap: PeriodicX::for_width(360.0).for_noise(wavelength_deg),
        }
    }

    pub fn at(&self, lon_deg: f64, lat_deg: f64) -> f64 {
        self.wrap.sample(lon_deg, lat_deg, |x, z| self.noise.sample2(x * self.scale, z * self.scale))
    }
}

/// The 64-bit finaliser `world::hash01` uses, without its last shift: z = seed ^ a*0x9E3779B97F4A7C15 ^
/// b*0xC2B2AE3D27D4EB4F; z = (z ^ z>>30)*0xBF58476D1CE4E5B9; z = (z ^ z>>27)*0x94D049BB133111EB; z ^ z>>31.
pub fn mix64(seed: u64, a: u64, b: u64) -> u64 {
    let mut z = seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const KIND_UPLIFT: [f64; 7] = [0.0, 0.6, 1.0, 1.0, 0.8, 0.3, 0.7];

/// Share of a belt's uplift rate still acting after `age_my`.
pub fn activity(age_my: f64, decay_my: f64) -> f64 {
    if !age_my.is_finite() {
        return BELT_RESIDUAL;
    }
    if age_my <= 0.0 {
        return 1.0;
    }
    BELT_RESIDUAL + (1.0 - BELT_RESIDUAL) * (-age_my / decay_my.max(1e-9)).exp()
}

/// Old-belt age weight in `0..=1`: 0 when young, 1 when old.
pub fn age_weight(age_my: f64) -> f64 {
    if !age_my.is_finite() {
        1.0
    } else {
        smoothstep(YOUNG_MAX_MY, OLD_MIN_MY, age_my.clamp(YOUNG_MAX_MY, OLD_MIN_MY))
    }
}

/// Derived seed for plate-stage attempt `attempt` (`0` = the input seed itself).
pub fn plate_seed(seed: u64, attempt: u8) -> u64 {
    if attempt == 0 {
        seed
    } else {
        mix64(seed, 0x9A7E, u64::from(attempt))
    }
}

fn inside_polar_margin(cz: usize, ocean_margin_start: f64) -> bool {
    (2.0 * (cz as f64 + 0.5) / PLANET_NZ as f64 - 1.0).abs() < ocean_margin_start
}

fn solve_sea_level_m(elev_abs: &[f64], key: &PlanetKey) -> f64 {
    let mut inside_vals: Vec<f64> = (0..PLANET_CELLS)
        .filter(|&c| inside_polar_margin(c / PLANET_NX, key.ocean_margin_start))
        .map(|c| elev_abs[c])
        .collect();
    inside_vals.sort_by(f64::total_cmp);

    let n = inside_vals.len();
    let target = (key.land_fraction * PLANET_CELLS as f64).round() as usize;
    if target == 0 || n == 0 {
        return inside_vals.last().copied().unwrap_or(0.0) + 1.0;
    }
    if target >= n {
        return inside_vals[0] - 1.0;
    }

    let lo = inside_vals[n - target - 1];
    let hi = inside_vals[n - target];
    if hi > lo {
        lo + 0.5 * (hi - lo)
    } else {
        let prev = inside_vals[..n - target].iter().rev().copied().find(|&v| v < hi);
        let eps = prev.map_or(1e-5, |p| 0.5 * (hi - p).min(1e-5));
        hi - eps
    }
}

fn majority_3x3_land(raw_land: &[bool]) -> Vec<bool> {
    let mut land = vec![false; PLANET_CELLS];
    for cz in 0..PLANET_NZ {
        for cx in 0..PLANET_NX {
            let mut count = 0u8;
            for dz in -1isize..=1 {
                let nz = cz as isize + dz;
                if !(0..PLANET_NZ as isize).contains(&nz) {
                    continue;
                }
                let row = nz as usize * PLANET_NX;
                for dx in -1isize..=1 {
                    let nx = (cx as isize + dx).rem_euclid(PLANET_NX as isize) as usize;
                    count += u8::from(raw_land[row + nx]);
                }
            }
            land[cz * PLANET_NX + cx] = count >= 5;
        }
    }
    land
}

fn box_blur_3x3(src: &[f32]) -> Vec<f32> {
    let mut dst = vec![0.0f32; PLANET_CELLS];
    for cz in 0..PLANET_NZ {
        for cx in 0..PLANET_NX {
            let mut sum = 0.0_f64;
            for dz in -1isize..=1 {
                let nz = (cz as isize + dz).clamp(0, PLANET_NZ as isize - 1) as usize;
                let row = nz * PLANET_NX;
                for dx in -1isize..=1 {
                    let nx = (cx as isize + dx).rem_euclid(PLANET_NX as isize) as usize;
                    sum += f64::from(src[row + nx]);
                }
            }
            dst[cz * PLANET_NX + cx] = (sum / 9.0) as f32;
        }
    }
    dst
}

fn signed_coast_sd_deg(land: &[bool]) -> Vec<f32> {
    let d_sea = plate_sim::chamfer_deg(|c| !land[c]);
    let d_land = plate_sim::chamfer_deg(|c| land[c]);
    let half = 0.5 * PLANET_DEG_PER_CELL;
    let mut raw = vec![0.0f32; PLANET_CELLS];
    for c in 0..PLANET_CELLS {
        raw[c] = if land[c] {
            (d_sea[c].min(180.0) - half) as f32
        } else {
            -((d_land[c].min(180.0) - half) as f32)
        };
    }
    box_blur_3x3(&box_blur_3x3(&raw))
}

/// Derived planet-grid outputs of the plate stage (Tasks 4, 7, 13).
#[derive(Clone, Debug)]
pub struct Planet {
    pub key: PlanetKey,
    pub attempt: u8,
    pub ok: bool,
    pub score: PlanetScore,
    pub sea_level_m: f32,
    pub plate: Vec<u8>,
    pub crust: Vec<u8>,
    pub crust_age_my: Vec<f32>,
    pub thickness_km: Vec<f32>,
    pub kind: Vec<u8>,
    pub orogeny_age_my: Vec<f32>,
    pub strength: Vec<f32>,
    pub fold: Vec<[f32; 2]>,
    pub elevation_m: Vec<f32>,
    pub land: Vec<bool>,
    pub coast_sd_deg: Vec<f32>,
    pub uplift_now: Vec<f32>,
    pub root_rel: Vec<f32>,
    pub age_w: Vec<f32>,
    pub volcanic: Vec<f32>,
    pub province: Vec<u8>,
    pub continent: Vec<u8>,
    pub belt: Vec<u16>,
    pub continents: Vec<Continent>,
    pub belts: Vec<Belt>,
    pub belt_depth_deg: f64,
    pub counters: Counters,
    pub stage_ms: f64,
    pub attempts_ms: f64,
}

static MEMO: Mutex<Option<(PlanetKey, Arc<Planet>)>> = Mutex::new(None);

impl Planet {
    /// One attempt: simulate with `plate_seed(key.seed, attempt)`, derive every field, label and score.
    pub fn build(key: &PlanetKey, attempt: u8) -> Planet {
        let t0 = Instant::now();
        let sim_key = PlanetKey { seed: plate_seed(key.seed, attempt), ..*key };
        let state = plate_sim::simulate(&sim_key, 0, &mut |_| {});
        let t_end = state.time_my();

        let mut crust_age_my = vec![0.0f32; PLANET_CELLS];
        let mut orogeny_age_my = vec![f32::INFINITY; PLANET_CELLS];
        let mut elev_abs = vec![0.0f64; PLANET_CELLS];
        let mut thickness_km = vec![7.0f32; PLANET_CELLS];

        for c in 0..PLANET_CELLS {
            crust_age_my[c] = (t_end - f64::from(state.born_my[c])) as f32;
            if state.kind[c] != OrogenyKind::None as u8 {
                orogeny_age_my[c] = (t_end - f64::from(state.orogeny_my[c])) as f32;
            }
            let e = plate_sim::elevation_m(&state, c);
            elev_abs[c] = e;
            if state.crust[c] == CrustKind::Continental as u8 {
                thickness_km[c] = (35.0 + (e - plate_sim::CONT_BASE_M) / (1000.0 * 0.18)) as f32;
            }
        }

        let sea_level_f64 = solve_sea_level_m(&elev_abs, key);
        let mut elevation_m = vec![0.0f32; PLANET_CELLS];
        let mut raw_land = vec![false; PLANET_CELLS];
        for c in 0..PLANET_CELLS {
            let e = (elev_abs[c] - sea_level_f64) as f32;
            elevation_m[c] = e;
            raw_land[c] = inside_polar_margin(c / PLANET_NX, key.ocean_margin_start) && e > 0.0;
        }

        let land = majority_3x3_land(&raw_land);
        let coast_sd_deg = signed_coast_sd_deg(&land);

        let mut uplift_raw = vec![0.0f32; PLANET_CELLS];
        let mut root_raw = vec![0.0f32; PLANET_CELLS];
        let mut age_w_raw = vec![0.0f32; PLANET_CELLS];
        let mut volcanic_raw = vec![0.0f32; PLANET_CELLS];
        for c in 0..PLANET_CELLS {
            let k = state.kind[c];
            let k_idx = usize::from(k);
            let age = f64::from(orogeny_age_my[c]);
            let decay = if k == OrogenyKind::Hotspot as u8 { HOTSPOT_DECAY_MY } else { key.belt_decay_my };
            uplift_raw[c] = (KIND_UPLIFT[k_idx] * f64::from(state.strength[c]) * activity(age, decay)) as f32;
            root_raw[c] = ((thickness_km[c] - 35.0) / 33.0).clamp(0.0, 1.0);
            if k == OrogenyKind::Arc as u8
                || k == OrogenyKind::Andean as u8
                || k == OrogenyKind::Collision as u8
                || k == OrogenyKind::Plateau as u8
            {
                age_w_raw[c] = age_weight(age) as f32;
            }
            if (k == OrogenyKind::Arc as u8 || k == OrogenyKind::Andean as u8) && age < 30.0 {
                volcanic_raw[c] = state.strength[c];
            } else if k == OrogenyKind::Hotspot as u8 && age < 10.0 {
                volcanic_raw[c] = 0.5;
            }
        }

        let uplift_now = box_blur_3x3(&uplift_raw);
        let root_rel = box_blur_3x3(&root_raw);
        let age_w = box_blur_3x3(&age_w_raw);
        let volcanic = box_blur_3x3(&volcanic_raw);

        let mut planet = Planet {
            key: *key,
            attempt,
            ok: false,
            score: PlanetScore::default(),
            sea_level_m: sea_level_f64 as f32,
            plate: state.plate,
            crust: state.crust,
            crust_age_my,
            thickness_km,
            kind: state.kind,
            orogeny_age_my,
            strength: state.strength,
            fold: state.fold,
            elevation_m,
            land,
            coast_sd_deg,
            uplift_now,
            root_rel,
            age_w,
            volcanic,
            province: vec![0; PLANET_CELLS],
            continent: vec![belts::NO_LABEL; PLANET_CELLS],
            belt: vec![belts::NO_BELT; PLANET_CELLS],
            continents: Vec::new(),
            belts: Vec::new(),
            belt_depth_deg: 4.0,
            counters: state.counters,
            stage_ms: 0.0,
            attempts_ms: 0.0,
        };
        belts::label(&mut planet);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        planet.stage_ms = ms;
        planet.attempts_ms = ms;
        planet
    }
}

/// Run up to `PLATE_ATTEMPTS_MAX` attempts without memoisation.
pub fn build_with_retries(key: &PlanetKey) -> Planet {
    let t0 = Instant::now();
    let mut best: Option<Planet> = None;
    for attempt in 0..PLATE_ATTEMPTS_MAX {
        let mut candidate = Planet::build(key, attempt);
        if candidate.ok {
            candidate.attempts_ms = t0.elapsed().as_secs_f64() * 1000.0;
            return candidate;
        }
        let replace = match best.as_ref() {
            None => true,
            Some(prev) => candidate.score.met() > prev.score.met(),
        };
        if replace {
            best = Some(candidate);
        }
    }
    let mut winner = best.expect("PLATE_ATTEMPTS_MAX >= 1");
    winner.attempts_ms = t0.elapsed().as_secs_f64() * 1000.0;
    winner
}

/// The planet of `p`: the first of up to `PLATE_ATTEMPTS_MAX` attempts that passes, else the best (Ruling 9).
/// Memoised on the last `PlanetKey`.
pub fn planet_for(p: &ForgeParams) -> Arc<Planet> {
    let key = PlanetKey::of(p);
    if let Some((cached_key, cached)) = MEMO.lock().expect("planet memo poisoned").as_ref() {
        if *cached_key == key {
            return Arc::clone(cached);
        }
    }
    let built = Arc::new(build_with_retries(&key));
    *MEMO.lock().expect("planet memo poisoned") = Some((key, Arc::clone(&built)));
    built
}

/// `planet_for` without the retries and without the memo (the tool's `--no-retry`).
pub fn planet_first_attempt(p: &ForgeParams) -> Planet {
    Planet::build(&PlanetKey::of(p), 0)
}
