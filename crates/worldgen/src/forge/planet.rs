//! The planet grid (Phase 2): one coarse `PLANET_NX` × `PLANET_NZ` raster of
//! the whole world, the same on every world size and forge cell, wrapping
//! east–west (column `PLANET_NX − 1` is beside column 0) and ending at the
//! poles (row 0 is the north edge). Every planetary length is in degrees of
//! latitude; a cell is `PLANET_DEG_PER_CELL` degrees both ways. The plate
//! simulation (`plate_sim`) runs on it and reads only a [`PlanetKey`].
//! Plan: docs/superpowers/plans/2026-11-20-plates-and-aged-relief-2.md.

use crate::forge::periodic::PeriodicX;
use crate::forge::world::ForgeParams;
use crate::noise::OctaveNoise;

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
