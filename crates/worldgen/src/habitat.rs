//! Habitat fields: what a point of ground is like for plants. Rain, temperature,
//! moisture, slope, soil depth and salinity at an engine column, from which
//! later tree placement picks a species. A forged world reads its stored rain
//! and coast-distance rasters; the procedural world derives cheap stand-ins.
//!
//! Only + - * /, floor and sqrt are used (the `noise.rs` determinism policy).

use crate::forged::ForgedWorld;
use crate::noise::OctaveNoise;
use crate::pipeline::{TerrainPipeline, SEA_TOP_BLOCK};

/// Temperature at sea level (deg C).
pub const BASE_TEMPERATURE_C: f32 = 14.0;
/// Cooling per metre of elevation above the sea's top block (deg C / m).
pub const LAPSE_C_PER_M: f32 = 0.0065;
/// Salt spray reaches this far inland from the sea (m); salinity falls
/// linearly from 1 at the shore to 0 here.
pub const SALT_REACH_M: f32 = 120.0;
/// Slope is measured by central difference over +- this many metres.
pub const SLOPE_PROBE_M: f64 = 4.0;
/// Soil thins by this many blocks per unit of slope (rise/run): a 1:1 slope
/// carries no soil. The soil band is `crate::MAX_SOIL_DEPTH` blocks at most.
pub const SOIL_LOSS_BLOCKS_PER_SLOPE: f32 = 3.0;
/// Slope (rise/run) at and above which ground counts as not flat at all.
pub const FLATNESS_SLOPE_LIMIT: f32 = 1.0;
/// Flat ground holds this much more moisture than steep ground:
/// `moisture = rain * (1 + FLAT_MOISTURE_BONUS * flatness)`.
pub const FLAT_MOISTURE_BONUS: f32 = 0.5;

/// Procedural rain: `1 + RAIN_NOISE_AMPLITUDE * noise` at a ~2 km wavelength.
pub const PROCEDURAL_RAIN_FIRST_OCTAVE: i32 = -11;
pub const PROCEDURAL_RAIN_AMPLITUDE: f64 = 0.5;
/// Procedural stand-in for coast distance: blocks above the sea's top block
/// at which a column is no longer salty (see `ProceduralHabitat`).
pub const PROCEDURAL_SALT_BLOCKS: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Habitat {
    /// Rain relative to the land mean (1.0 = average).
    pub rain: f32,
    pub temperature_c: f32,
    /// Wetness of the ground: rain, more on flat ground.
    pub moisture: f32,
    /// Magnitude of the surface gradient, blocks per block.
    pub slope: f32,
    /// Blocks of soil over the rock, 0..=`MAX_SOIL_DEPTH`.
    pub soil_depth: f32,
    /// 0 (fresh) .. 1 (at the shore).
    pub salinity: f32,
}

pub trait HabitatSource {
    /// Habitat of the engine column at world meters `(x_m, z_m)` whose
    /// surface block is `surface_block`.
    fn habitat(&self, x_m: f64, z_m: f64, surface_block: i32) -> Habitat;
}

fn temperature_c(surface_block: i32) -> f32 {
    BASE_TEMPERATURE_C - LAPSE_C_PER_M * (surface_block - SEA_TOP_BLOCK) as f32
}

fn assemble(rain: f32, slope: f32, salinity: f32, surface_block: i32) -> Habitat {
    let flatness = (1.0 - slope / FLATNESS_SLOPE_LIMIT).clamp(0.0, 1.0);
    Habitat {
        rain,
        temperature_c: temperature_c(surface_block),
        moisture: rain * (1.0 + FLAT_MOISTURE_BONUS * flatness),
        slope,
        soil_depth: (crate::MAX_SOIL_DEPTH as f32 - SOIL_LOSS_BLOCKS_PER_SLOPE * slope).max(0.0),
        salinity: salinity.clamp(0.0, 1.0),
    }
}

fn gradient_magnitude(f: impl Fn(f64, f64) -> f64, x: f64, z: f64) -> f32 {
    let e = SLOPE_PROBE_M;
    let (dx, dz) = ((f(x + e, z) - f(x - e, z)) / (2.0 * e), (f(x, z + e) - f(x, z - e)) / (2.0 * e));
    (dx * dx + dz * dz).sqrt() as f32
}

impl HabitatSource for ForgedWorld {
    fn habitat(&self, x_m: f64, z_m: f64, surface_block: i32) -> Habitat {
        let (ax, az) = self.to_artifact_m(x_m, z_m);
        let a = self.artifact();
        let rain = self.bilinear(&a.rain, ax, az).max(0.0) as f32;
        let coast = self.bilinear(self.coast_dist_m(), ax, az) as f32;
        let slope = gradient_magnitude(|x, z| self.ground_engine_m(x, z), x_m, z_m);
        assemble(rain, slope, 1.0 - coast / SALT_REACH_M, surface_block)
    }
}

/// Habitat of the noise world, which has no rasters: rain is a 2 km noise,
/// slope comes from the profile's surface, and salinity approximates "distance
/// from the sea" by height above it — beaches within `PROCEDURAL_SALT_BLOCKS`
/// of the sea's top block are salty, falling off linearly — rather than a true
/// distance field.
pub struct ProceduralHabitat {
    pub seed: u64,
    pipeline: TerrainPipeline,
    rain_noise: OctaveNoise,
}

impl ProceduralHabitat {
    pub fn new(seed: u64) -> Self {
        ProceduralHabitat {
            seed,
            pipeline: TerrainPipeline::new(seed),
            rain_noise: OctaveNoise::new(seed ^ 0x4A17_0000, PROCEDURAL_RAIN_FIRST_OCTAVE, vec![1.0]),
        }
    }
}

impl HabitatSource for ProceduralHabitat {
    fn habitat(&self, x_m: f64, z_m: f64, surface_block: i32) -> Habitat {
        let rain = (1.0 + PROCEDURAL_RAIN_AMPLITUDE * self.rain_noise.sample2(x_m, z_m)).max(0.0) as f32;
        let slope = gradient_magnitude(|x, z| self.pipeline.column(x, z).surface_y(), x_m, z_m);
        let salinity = 1.0 - (surface_block - SEA_TOP_BLOCK) as f32 / PROCEDURAL_SALT_BLOCKS;
        assemble(rain, slope, salinity, surface_block)
    }
}

/// Distance in metres from each cell of an `n * n` height raster (`cell_m`
/// cells, row-major) to the nearest sea cell (height <= 0): 0 at sea. A
/// two-pass chamfer (axis step 1, diagonal sqrt 2 cells). A raster with no sea
/// reports the raster's full width everywhere.
pub fn coast_distance_m<T: Copy + Into<f64>>(height: &[T], n: usize, cell_m: f64) -> Vec<f32> {
    const DIAGONAL: f32 = 1.414_213_5;
    let far = (n as f64 * cell_m) as f32;
    let (axis, diag) = (cell_m as f32, cell_m as f32 * DIAGONAL);
    let mut d: Vec<f32> = height.iter().map(|&h| if h.into() <= 0.0 { 0.0 } else { far }).collect();
    let relax = |d: &mut Vec<f32>, x: usize, z: usize, dx: isize, dz: isize, step: f32| {
        let (nx, nz) = (x as isize + dx, z as isize + dz);
        if nx >= 0 && nz >= 0 && (nx as usize) < n && (nz as usize) < n {
            let via = d[nz as usize * n + nx as usize] + step;
            let i = z * n + x;
            if via < d[i] {
                d[i] = via;
            }
        }
    };
    for z in 0..n {
        for x in 0..n {
            relax(&mut d, x, z, -1, 0, axis);
            relax(&mut d, x, z, 0, -1, axis);
            relax(&mut d, x, z, -1, -1, diag);
            relax(&mut d, x, z, 1, -1, diag);
        }
    }
    for z in (0..n).rev() {
        for x in (0..n).rev() {
            relax(&mut d, x, z, 1, 0, axis);
            relax(&mut d, x, z, 0, 1, axis);
            relax(&mut d, x, z, 1, 1, diag);
            relax(&mut d, x, z, -1, 1, diag);
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forged::{QRaster, WorldArtifact, ARTIFACT_VERSION, GEO_STEP_M, HEIGHT_STEP_M, RAIN_STEP};

    const N: usize = 64;
    const CELL_M: f64 = 4.0;
    const SEA_CELLS: usize = 8;

    /// Sea in the west (x < 32 m), a flat 10 m plain east of it. Engine and
    /// artifact meters coincide (spawn at the engine spawn column).
    fn coast_world() -> ForgedWorld {
        let height: Vec<f32> =
            (0..N * N).map(|i| if i % N < SEA_CELLS { -5.0 } else { 10.0 }).collect();
        ForgedWorld::new(WorldArtifact {
            version: ARTIFACT_VERSION,
            seed: 7,
            size_m: N as f64 * CELL_M,
            cell_m: CELL_M,
            n: N as u32,
            height: QRaster::fit(&height, HEIGHT_STEP_M),
            uplifted: QRaster::fit(&vec![0.0; N * N], GEO_STEP_M),
            bedding: QRaster::fit(&vec![0.0; N * N], GEO_STEP_M),
            rain: QRaster::fit(&vec![1.0; N * N], RAIN_STEP),
            column: vec![(700.0, 0)],
            rock_names: vec!["sandstone".into()],
            rivers: vec![],
            lakes: vec![],
            spawn: [8.5, 8.5],
        })
    }

    #[test]
    fn temperature_drops_with_elevation() {
        let world = coast_world();
        let low = world.habitat(120.0, 120.0, SEA_TOP_BLOCK);
        let high = world.habitat(120.0, 120.0, SEA_TOP_BLOCK + 1000);
        assert_eq!(low.temperature_c, BASE_TEMPERATURE_C);
        assert!((low.temperature_c - high.temperature_c - 1000.0 * LAPSE_C_PER_M).abs() < 1e-3);
    }

    #[test]
    fn salinity_is_one_at_the_coast_and_zero_inland() {
        let world = coast_world();
        let shore = world.habitat(34.0, 120.0, SEA_TOP_BLOCK + 2);
        let inland = world.habitat(200.0, 120.0, SEA_TOP_BLOCK + 2);
        assert!(shore.salinity > 0.9, "shore {}", shore.salinity);
        assert_eq!(inland.salinity, 0.0);
        // Flat plain: full soil, flatness bonus on moisture.
        // (Detail noise keeps the plain from being perfectly flat.)
        assert!(inland.soil_depth > crate::MAX_SOIL_DEPTH as f32 - 0.5, "{}", inland.soil_depth);
        assert!(inland.moisture > 1.4, "{}", inland.moisture);
    }

    #[test]
    fn artifact_round_trips_rain_and_coast() {
        let mut artifact = coast_world().artifact().clone();
        artifact.rain.data[5] = 1750;
        let back = WorldArtifact::from_bytes(&artifact.to_bytes()).expect("loads");
        assert_eq!(back.rain, artifact.rain);
        // Coast distance is not stored: it is recomputed from the height.
        assert_eq!(ForgedWorld::new(back.clone()).coast_dist_m(), coast_world().coast_dist_m());
        assert_eq!(back.height, artifact.height);
        assert_eq!(back.version, ARTIFACT_VERSION);
    }

    #[test]
    fn coast_distance_is_zero_at_sea_and_grows_inland() {
        let height: Vec<f32> = (0..N * N).map(|i| if i % N < SEA_CELLS { -1.0 } else { 1.0 }).collect();
        let d = coast_distance_m(&height, N, CELL_M);
        assert_eq!(d[0], 0.0);
        assert_eq!(d[SEA_CELLS], CELL_M as f32);
        assert_eq!(d[SEA_CELLS + 10], 11.0 * CELL_M as f32);
    }

    #[test]
    fn procedural_habitat_is_deterministic() {
        let (a, b) = (ProceduralHabitat::new(9), ProceduralHabitat::new(9));
        let at = |h: &ProceduralHabitat| h.habitat(1234.5, -678.9, SEA_TOP_BLOCK + 20);
        assert_eq!(at(&a), at(&b));
        let h = at(&a);
        assert!(h.rain >= 0.0 && h.salinity == 0.0 && (0.0..=3.0).contains(&h.soil_depth));
    }
}
