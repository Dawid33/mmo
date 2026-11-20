//! Habitat fields: what a point of ground is like for plants. Rain, temperature,
//! moisture, slope, soil depth and salinity at an engine column, from which
//! later tree placement picks a species. A forged world reads its stored
//! climate (temperature and precipitation, `forge::climate`) and the coast
//! distance recomputed on load; the procedural world derives cheap stand-ins
//! (a fixed lapse from `BASE_TEMPERATURE_C`, noise rain).
//!
//! Only + - * /, floor and sqrt are used (the `noise.rs` determinism policy).

use crate::forged::{ForgedWorld, QRasterId};
use game::Biome;
use crate::noise::OctaveNoise;
use crate::pipeline::{TerrainPipeline, SEA_TOP_BLOCK};

/// Procedural world: temperature at sea level (deg C).
pub const BASE_TEMPERATURE_C: f32 = 14.0;
/// Procedural world: cooling per metre of elevation above the sea's top
/// block (deg C / m).
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

/// Forged worlds: precipitation (mm/yr) that counts as `rain` 1.0, so the
/// species niches (tuned on rain relative to the land mean) keep their meaning.
pub const REFERENCE_PRECIP_MM: f32 = 900.0;

/// Forged worlds: moisture saturates here. Ground wetter than this is no
/// wetter for a tree (waterlogging is the biome's drainage — swamp, marsh),
/// so a very rainy temperate forest keeps its oaks and birches instead of
/// falling off their niches' wet ends. Procedural moisture is not capped.
pub const FORGED_MOISTURE_SATURATION: f32 = 2.5;

/// Procedural rain: `1 + RAIN_NOISE_AMPLITUDE * noise` at a ~2 km wavelength.
pub const PROCEDURAL_RAIN_FIRST_OCTAVE: i32 = -11;
pub const PROCEDURAL_RAIN_AMPLITUDE: f64 = 0.5;
/// Procedural stand-in for coast distance: blocks above the sea's top block
/// at which a column is no longer salty (see `ProceduralHabitat`).
pub const PROCEDURAL_SALT_BLOCKS: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Habitat {
    /// Rain, 1.0 = average: forged worlds `precip_mm / REFERENCE_PRECIP_MM`,
    /// the procedural world relative to its noise mean.
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
    /// The column's biome: a forged world's dithered biome
    /// (`ForgedWorld::biome_at`, the one its ground cover uses); `None` in
    /// the procedural world, which has none.
    pub biome: Option<Biome>,
}

pub trait HabitatSource {
    /// Habitat of the engine column at world meters `(x_m, z_m)` whose
    /// surface block is `surface_block`.
    fn habitat(&self, x_m: f64, z_m: f64, surface_block: i32) -> Habitat;
}

/// The procedural world's temperature: a fixed lapse from `BASE_TEMPERATURE_C`.
fn procedural_temperature_c(surface_block: i32) -> f32 {
    BASE_TEMPERATURE_C - LAPSE_C_PER_M * (surface_block - SEA_TOP_BLOCK) as f32
}

fn assemble(rain: f32, temperature_c: f32, slope: f32, salinity: f32, biome: Option<Biome>) -> Habitat {
    let flatness = (1.0 - slope / FLATNESS_SLOPE_LIMIT).clamp(0.0, 1.0);
    Habitat {
        biome,
        rain,
        temperature_c,
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
    /// Temperature comes from the stored field (latitude and the raster's
    /// ground height), so the tree line follows the climate; `surface_block`
    /// is not used. The biome is the column's dithered one.
    fn habitat(&self, x_m: f64, z_m: f64, _surface_block: i32) -> Habitat {
        let (ax, az) = self.to_artifact_m(x_m, z_m);
        let rain = (self.bilinear_q(QRasterId::PrecipMm, ax, az).max(0.0) as f32) / REFERENCE_PRECIP_MM;
        let temperature_c = self.bilinear_q(QRasterId::TemperatureC, ax, az) as f32;
        let coast = self.bilinear_coast(ax, az) as f32;
        let slope = gradient_magnitude(|x, z| self.ground_engine_m(x, z), x_m, z_m);
        let h = assemble(rain, temperature_c, slope, 1.0 - coast / SALT_REACH_M, Some(self.biome_at(x_m, z_m)));
        Habitat { moisture: h.moisture.min(FORGED_MOISTURE_SATURATION), ..h }
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
        assemble(rain, procedural_temperature_c(surface_block), slope, salinity, None)
    }
}

/// Distance in metres from each cell of an `nx * nz` height raster (`cell_m`
/// cells, row-major) to the nearest sea cell (height <= 0): 0 at sea. A
/// two-pass chamfer (axis step 1, diagonal sqrt 2 cells). A raster with no sea
/// reports the raster's longer side everywhere. `wrap_x`: columns 0 and
/// `nx - 1` are neighbours (a world that wraps east-west).
pub fn coast_distance_m<T: Copy + Into<f64>>(height: &[T], nx: usize, nz: usize, cell_m: f64, wrap_x: bool) -> Vec<f32> {
    coast_distance_by(nx, nz, cell_m, wrap_x, |i| height[i].into() <= 0.0)
}

/// A wrapping chamfer distance repeats its pass pair at most this many more
/// times, until nothing changes: a forward pass reads a row's far column
/// before it was relaxed.
pub const CHAMFER_WRAP_EXTRA_PASSES: usize = 2;

/// [`coast_distance_m`] with the sea test as a closure over the raster index
/// (`ForgedWorld` reads its quantised height without decoding a copy).
pub fn coast_distance_by(nx: usize, nz: usize, cell_m: f64, wrap_x: bool, is_sea: impl Fn(usize) -> bool) -> Vec<f32> {
    const DIAGONAL: f32 = 1.414_213_5;
    let far = (nx.max(nz) as f64 * cell_m) as f32;
    let (axis, diag) = (cell_m as f32, cell_m as f32 * DIAGONAL);
    let mut d: Vec<f32> = (0..nx * nz).map(|i| if is_sea(i) { 0.0 } else { far }).collect();
    let relax = |d: &mut Vec<f32>, x: usize, z: usize, dx: isize, dz: isize, step: f32| {
        let (jx, jz) = (x as isize + dx, z as isize + dz);
        let jx = if wrap_x { jx.rem_euclid(nx as isize) } else { jx };
        if jx >= 0 && jz >= 0 && (jx as usize) < nx && (jz as usize) < nz {
            let via = d[jz as usize * nx + jx as usize] + step;
            let i = z * nx + x;
            if via < d[i] {
                d[i] = via;
                return true;
            }
        }
        false
    };
    let pair = |d: &mut Vec<f32>| {
        let mut changed = false;
        for z in 0..nz {
            for x in 0..nx {
                changed |= relax(d, x, z, -1, 0, axis);
                changed |= relax(d, x, z, 0, -1, axis);
                changed |= relax(d, x, z, -1, -1, diag);
                changed |= relax(d, x, z, 1, -1, diag);
            }
        }
        for z in (0..nz).rev() {
            for x in (0..nx).rev() {
                changed |= relax(d, x, z, 1, 0, axis);
                changed |= relax(d, x, z, 0, 1, axis);
                changed |= relax(d, x, z, 1, 1, diag);
                changed |= relax(d, x, z, -1, 1, diag);
            }
        }
        changed
    };
    pair(&mut d);
    if wrap_x {
        let mut settled = false;
        for _ in 0..CHAMFER_WRAP_EXTRA_PASSES {
            if !pair(&mut d) {
                settled = true;
                break;
            }
        }
        // The cap is a bound, not a proof: a debug build checks that one
        // more pass pair would change nothing.
        debug_assert!(settled || !pair(&mut d), "chamfer distance not settled after the capped pass pairs");
    }
    d
}

