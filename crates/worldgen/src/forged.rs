//! Forged worlds: terrain read from a pre-generated WORLD ARTIFACT instead of
//! evaluated from noise. One artifact is one finite world ("super region":
//! 16 km, ocean all round), simulated offline by the world forge
//! (`examples/forge_proto`, `just forge-proto`) — whole-map erosion, real
//! drainage, lakes at their spill level — which a per-point function cannot do.
//! See docs/superpowers/specs/2026-09-20-df-worldgen-research/00-synthesis.md
//! (addendum) and docs/superpowers/specs/2026-09-20-world-forge-f1/findings.md.
//!
//! The artifact stores coarse rasters (8-16 m cells) plus rivers and lakes as
//! objects. This module turns them into what `generate_region`/`coarse_tile`
//! need at any point: a smooth ground height (bicubic + a little detail
//! noise), river channels carved at 1 m resolution from the river objects,
//! lake and river water ABOVE sea level, and banks that contain it.
//!
//! Opt-in: set `VOXEL_WORLD_FILE=<path>` and `generate_region`/`coarse_tile`
//! serve that world (the `seed` argument is then ignored); unset, nothing
//! changes. Only + - * /, floor and sqrt are used on the sampling path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use game::{Biome, BlockId, CellClimate};

use crate::forge::geology::rock_index;
use crate::forge::periodic::PeriodicX;
pub use crate::forge::regions::RegionRecord;
pub use crate::forge::world::EdgeKind;
use crate::material::Materials;
use crate::noise::OctaveNoise;
use crate::pipeline::{ColumnProfile, ProfileGrid, SEA_LEVEL, SEA_TOP_BLOCK};

/// Environment variable naming the world file to serve.
pub const WORLD_FILE_ENV: &str = "VOXEL_WORLD_FILE";
/// v3: quantised `u16` rasters (`QRaster`), sparse lakes, row-delta + zstd,
/// coast distance recomputed on load. v4: `edges` (what lies past each side).
/// v5: the latitude climate — `temperature_c` and `precip_mm` replace the
/// relative `rain`. v6: derived `drainage`, `volcanism`, `salinity` (`u8`
/// 0..100 rasters, `forge::fields`) and the volcano sites. v7: `biome`
/// (`game::Biome` per cell), `region` ids + `regions`, `savagery`
/// (`forge::regions`). v8: width x height (`width_m`, `height_m`, `nx`,
/// `nz` replace `size_m`, `n`) and the forge's `knobs`. v9: no byte change —
/// the Planet is the only world (`forge::world::PLANET_EDGES`); a world of a
/// deleted layout is refused (`validate_wrap`). v10: `climate`, the
/// monthly-normals family (`game::MonthlyClimate`, row-delta encoded per
/// plane: `climate_to_wire`).
pub const ARTIFACT_VERSION: u32 = 10;
/// zstd level of the world file body (offline writer; reading cost is level-independent).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const ZSTD_LEVEL: i32 = 19;
/// Read buffer between the streamed zstd decoder and bincode on load.
#[cfg(not(target_arch = "wasm32"))]
const LOAD_READ_BUFFER_BYTES: usize = 1 << 16;
/// Height raster quantisation step (m): ~3 cm worst-case error, 16-bit span of 4095.9 m.
pub const HEIGHT_STEP_M: f32 = 0.0625;
/// Uplift / bedding raster step (m).
pub const GEO_STEP_M: f32 = 0.0625;
/// Temperature raster step (deg C).
pub const TEMPERATURE_STEP_C: f32 = 0.1;
/// Precipitation raster step (mm/yr).
pub const PRECIP_STEP_MM: f32 = 1.0;
const MAGIC: &[u8; 4] = b"VXWF";

/// Engine column the artifact's spawn point lands on: block (8, 8) of region
/// (0, 0), where players are created (see `examples/seed_scan.rs`).
const ENGINE_SPAWN_M: f64 = 8.5;
/// Ground height past a SEA edge of the artifact (m relative to sea): open
/// ocean — on the Planet north and south; east–west it wraps round
/// ([`ForgedWorld::sample`]).
pub const OUTSIDE_HEIGHT_M: f64 = -90.0;

/// Detail noise added to the interpolated ground: first octave 2^5 = 32 m,
/// then 16 m. Amplitude grows with slope — smooth plains, rougher flanks.
const DETAIL_FIRST_OCTAVE: i32 = -5;
const DETAIL_BASE_M: f64 = 0.25;
const DETAIL_PER_SLOPE_M: f64 = 3.0;
const DETAIL_MAX_M: f64 = 1.8;
/// Detail fades in over this height range above the sea, so it never moves
/// the coastline.
const DETAIL_FADE_M: (f64, f64) = (0.5, 3.0);

/// Channel depth at the centerline: `CHANNEL_DEPTH_PER_SQRT_M * sqrt(width)`,
/// clamped. A 9 m river is ~1.5 m deep, a 25 m one ~2.5 m.
const CHANNEL_DEPTH_PER_SQRT_M: f64 = 0.5;
const CHANNEL_DEPTH_M: (f64, f64) = (0.8, 3.0);
/// Ring of raised bank outside the channel that keeps the water in (m).
const LEVEE_WIDTH_M: f64 = 2.0;
/// Banks stand at least this far above the water surface (m).
const FREEBOARD_M: f64 = 0.3;
/// Beyond the levee the valley side is cut back no steeper than this (rise/run).
const BANK_SLOPE: f64 = 0.7;
/// How far from the channel edge the bank cut can reach (m).
const BANK_REACH_M: f64 = 24.0;
/// River segments are indexed in square buckets of this size (m).
pub(crate) const BUCKET_M: f64 = 64.0;
/// Inside one patch, candidates are narrowed again to cells of this size (m):
/// a region samples ~1 M points (every voxel column of every surface block),
/// and checking all ~20 segments of a river region at each cost 4x the rest
/// of generation.
const PATCH_CELL_M: f64 = 8.0;

/// Biome borders at block scale (DF's "pick a neighbouring tile"): each block
/// column takes the biome of the raster cell nearest to its point displaced
/// by up to this many metres on each axis, so borders are irregular rather
/// than stepped along cell edges or blurred.
pub const BIOME_DITHER_M: f64 = 24.0;
/// The displacement noise: two octaves, first wavelength 2^5 = 32 m.
const BIOME_DITHER_FIRST_OCTAVE: i32 = -5;
/// Gain on the displacement noise (standard deviation ~0.5) before it is
/// clamped to +-1: most displacements reach well into the 24 m band.
const BIOME_DITHER_GAIN: f64 = 3.0;
/// The snow line is the height where the mean annual temperature reaches
/// this (deg C): ground above it is snow-covered.
pub const SNOW_LINE_C: f32 = -2.0;
/// Lake and sea ice: one whole block per this many degrees of mean
/// temperature below 0 C, `1..=ICE_MAX_BLOCKS` blocks.
pub const ICE_C_PER_BLOCK: f32 = 4.0;
pub const ICE_MAX_BLOCKS: i32 = 4;
/// Glacier ice above the ground: one block per degree below 0 C, clamped to
/// `GLACIER_MIN_BLOCKS..=GLACIER_MAX_BLOCKS`, thinning towards the glacier's
/// edge with the share of glacier cells around the point (no ice cliffs at
/// full height along every dithered border).
pub const GLACIER_MIN_BLOCKS: i32 = 4;
pub const GLACIER_MAX_BLOCKS: i32 = 40;
/// Tundra snow patches: a noise of this first octave (32 m) above
/// `TUNDRA_SNOW_PATCH_MIN` is snow.
const TUNDRA_SNOW_FIRST_OCTAVE: i32 = -5;
pub const TUNDRA_SNOW_PATCH_MIN: f64 = 0.25;
/// Salt flats: dry ground at least this salty (forge salinity, 0..100) and
/// farther than `habitat::SALT_REACH_M` from the sea — the salt of a closed
/// lake, not sea spray.
pub const SALT_FLAT_MIN_SALINITY: f64 = 40.0;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct RiverPoint {
    /// Artifact meters.
    pub x: f32,
    pub z: f32,
    /// Bed and water surface, meters relative to sea level.
    pub bed: f32,
    pub water: f32,
    pub in_lake: bool,
}

/// One river reach between two network nodes. `water` never rises downstream.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RiverReach {
    pub order: u32,
    pub discharge_m2: f64,
    pub width_m: f32,
    pub points: Vec<RiverPoint>,
}

/// One lake body: every cell in `cells` holds water at `level`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Lake {
    pub level: f32,
    pub area_m2: f32,
    pub max_depth: f32,
    pub outlet: [f32; 2],
    /// Raster cells as row-major `(start_index, len)` runs, ascending.
    pub cells: Vec<(u32, u32)>,
}

/// A raster stored as `u16` steps: value `i` is `min + data[i] * step`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QRaster {
    pub min: f32,
    pub step: f32,
    pub data: Vec<u16>,
}

impl QRaster {
    /// Quantises `v` onto `min + k * step`; values clamp into the 16-bit range.
    /// NaN is not allowed.
    pub fn from_f32(v: &[f32], min: f32, step: f32) -> QRaster {
        let inv = 1.0 / step as f64;
        let data = v
            .iter()
            .map(|&x| {
                debug_assert!(!x.is_nan(), "QRaster: NaN");
                ((x as f64 - min as f64) * inv).round().clamp(0.0, u16::MAX as f64) as u16
            })
            .collect();
        QRaster { min, step, data }
    }

    /// `(min, step)` covering all of `v`: `min` is the floor of the data
    /// minimum and `step` is `preferred_step` unless the span would not fit
    /// 16 bits, in which case it widens to `span / 65535`.
    pub fn range_for(v: &[f32], preferred_step: f32) -> (f32, f32) {
        let (lo, hi) = v.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)));
        if lo > hi {
            return (0.0, preferred_step); // empty
        }
        let min = lo.floor();
        let span = (hi - min) as f64;
        let step = if span > u16::MAX as f64 * preferred_step as f64 { (span / u16::MAX as f64) as f32 } else { preferred_step };
        (min, step)
    }

    /// [`QRaster::from_f32`] over [`QRaster::range_for`].
    pub fn fit(v: &[f32], preferred_step: f32) -> QRaster {
        let (min, step) = Self::range_for(v, preferred_step);
        Self::from_f32(v, min, step)
    }

    pub fn get(&self, i: usize) -> f32 {
        self.min + self.data[i] as f32 * self.step
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn to_f32(&self) -> Vec<f32> {
        (0..self.data.len()).map(|i| self.get(i)).collect()
    }
}

/// Raster cell reads shared by `f32` slices and quantised rasters.
pub trait RasterRead {
    fn value(&self, i: usize) -> f32;
}
impl RasterRead for [f32] {
    fn value(&self, i: usize) -> f32 {
        self[i]
    }
}
impl RasterRead for Vec<f32> {
    fn value(&self, i: usize) -> f32 {
        self[i]
    }
}
impl RasterRead for QRaster {
    fn value(&self, i: usize) -> f32 {
        self.get(i)
    }
}

/// Row-delta encode: every value but the first of each `n`-wide row becomes
/// its (wrapping) difference to the left neighbour. Smooth rasters turn into
/// small numbers zstd squeezes well.
pub fn delta_rows(data: &mut [u16], n: usize) {
    for row in data.chunks_mut(n.max(1)) {
        for i in (1..row.len()).rev() {
            row[i] = row[i].wrapping_sub(row[i - 1]);
        }
    }
}

/// Inverse of [`delta_rows`].
pub fn undelta_rows(data: &mut [u16], n: usize) {
    for row in data.chunks_mut(n.max(1)) {
        for i in 1..row.len() {
            row[i] = row[i].wrapping_add(row[i - 1]);
        }
    }
}

/// [`delta_rows`] for `u8` rasters.
pub fn delta_rows_u8(data: &mut [u8], n: usize) {
    for row in data.chunks_mut(n.max(1)) {
        for i in (1..row.len()).rev() {
            row[i] = row[i].wrapping_sub(row[i - 1]);
        }
    }
}

/// Inverse of [`delta_rows_u8`].
pub fn undelta_rows_u8(data: &mut [u8], n: usize) {
    for row in data.chunks_mut(n.max(1)) {
        for i in 1..row.len() {
            row[i] = row[i].wrapping_add(row[i - 1]);
        }
    }
}

/// The stored world. Rasters are `nx * nz`, row-major (`nz` rows of `nx`
/// columns, `i = z * nx + x`), one value per `cell_m` cell CENTER; heights are
/// meters relative to sea level. `nx * cell_m == width_m` and
/// `nz * cell_m == height_m` exactly.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorldArtifact {
    pub version: u32,
    pub seed: u64,
    /// West-east extent (x), metres.
    pub width_m: f64,
    /// North-south extent (z), metres.
    pub height_m: f64,
    pub cell_m: f64,
    /// Columns (x).
    pub nx: u32,
    /// Rows (z).
    pub nz: u32,
    /// Ground (land > 0) and sea floor (< 0).
    pub height: QRaster,
    /// Geology: rock at (x, y, z) is `column` looked up at
    /// `s = (y - uplifted) - bedding` (meters relative to sea level); regions
    /// carry it in their ground recipes (`geo_offset_blocks`, `strata_blocks`).
    pub uplifted: QRaster,
    pub bedding: QRaster,
    /// Mean annual temperature at the ground, deg C (`forge::climate`).
    pub temperature_c: QRaster,
    /// Annual precipitation, mm/yr (`forge::climate`).
    pub precip_mm: QRaster,
    /// Formations from the top down: (top s, index into `rock_names`).
    pub column: Vec<(f32, u8)>,
    /// Names from `forge::geology::ROCKS`; an unknown one fails the load.
    pub rock_names: Vec<String>,
    pub rivers: Vec<RiverReach>,
    /// Lake bodies with their cells (the per-cell water level is derived on load).
    pub lakes: Vec<Lake>,
    /// Where players start, artifact meters. Maps to engine block (8, 8) of region (0, 0).
    pub spawn: [f64; 2],
    /// What lies past each side, in `forge::world::EDGE_NAMES` order: north
    /// (z = 0), east (x = max), south (z = max), west (x = 0). Past a land
    /// edge the sampler continues the edge's ground; past a sea edge it is
    /// ocean.
    pub edges: [EdgeKind; 4],
    /// Drainage 0..100 (`forge::fields::drainage`): low = waterlogged.
    pub drainage: Vec<u8>,
    /// Volcanism 0..100 (`forge::fields::volcanism`).
    pub volcanism: Vec<u8>,
    /// Salinity 0..100 (`forge::fields::salinity`): sea and closed lakes 100.
    pub salinity: Vec<u8>,
    /// Volcano sites, artifact metres (each the summit of a cone of uplift).
    pub volcanoes: Vec<[f32; 2]>,
    /// `game::Biome` per cell (as `u8`): `game::classify` of this file's own
    /// fields (`cell_climate_at`), so a reader re-classifying gets the same.
    pub biome: Vec<u8>,
    /// Region id per cell: an index into `regions` (`forge::regions::flood`).
    pub region: Vec<u16>,
    pub regions: Vec<RegionRecord>,
    /// Savagery 0..100 (`forge::regions::savagery`).
    pub savagery: Vec<u8>,
    /// The forge's knobs by key (`world_store::knobs_map`): what the world
    /// was forged with, read back by [`WorldGlobals::knobs`] (the lapse rate,
    /// the good / evil shares). Empty = the defaults.
    pub knobs: BTreeMap<String, f64>,
    /// The monthly climate normals on the climate grid (`forge::climate`'s
    /// placeholder until Phase 3): always resident — whole here, and in a
    /// tiled world's `ForgedMeta`, never in tiles. Its grid spans the world
    /// exactly and `wrap_x` matches `edges` (`validate_climate`).
    pub climate: game::MonthlyClimate,
}

/// [`crate::forge::Knobs`] defaults overlaid with a stored knob map; names
/// this build does not know are ignored.
pub fn knobs_from_map(map: &BTreeMap<String, f64>) -> crate::forge::Knobs {
    let mut k = crate::forge::Knobs::default();
    for (key, &value) in map {
        // A stored value that is not a number keeps the default; the rest
        // are held to the knob's range (an unknown key is ignored).
        let Some(spec) = crate::forge::knobs::SPECS.iter().find(|s| s.key == key) else { continue };
        if value.is_finite() {
            k.set(key, value.clamp(spec.min, spec.max));
        }
    }
    k
}

/// The format version in a world file's 8-byte header, without loading the
/// body (the main menu lists worlds with this).
pub fn read_artifact_version(path: &Path) -> Result<u32, ArtifactError> {
    use std::io::Read;
    let mut header = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(ArtifactError::Io)?;
    artifact_bytes_version(&header)
}

/// The format version in the header of a world file's bytes (the world
/// store reads its archived forge file's with this), without decoding the body.
pub fn artifact_bytes_version(bytes: &[u8]) -> Result<u32, ArtifactError> {
    if bytes.len() < 8 || &bytes[..4] != MAGIC {
        return Err(ArtifactError::Format("missing VXWF header".into()));
    }
    Ok(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]))
}

#[derive(Debug)]
pub enum ArtifactError {
    Io(std::io::Error),
    /// Not a world file, or corrupt / newer than this build.
    Format(String),
    /// Written by an older format version: the world has to be forged again.
    OldVersion { found: u32 },
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactError::Io(e) => write!(f, "world file: {e}"),
            ArtifactError::Format(m) => write!(f, "world file: {m}"),
            ArtifactError::OldVersion { found } => {
                write!(f, "world file version {found} is older than {ARTIFACT_VERSION}: re-forge this world")
            }
        }
    }
}

impl WorldArtifact {
    /// Every quantised raster, for the codec and for `validate`. A new
    /// raster is one line here (plus its field).
    #[cfg(not(target_arch = "wasm32"))]
    fn qrasters_mut(&mut self) -> [&mut QRaster; 5] {
        [&mut self.height, &mut self.uplifted, &mut self.bedding, &mut self.temperature_c, &mut self.precip_mm]
    }

    pub(crate) fn qrasters(&self) -> [&QRaster; 5] {
        [&self.height, &self.uplifted, &self.bedding, &self.temperature_c, &self.precip_mm]
    }

    /// Every `u8` (0..100) raster, likewise.
    #[cfg(not(target_arch = "wasm32"))]
    fn byte_rasters_mut(&mut self) -> [&mut Vec<u8>; 5] {
        [&mut self.drainage, &mut self.volcanism, &mut self.salinity, &mut self.biome, &mut self.savagery]
    }

    pub(crate) fn byte_rasters(&self) -> [&Vec<u8>; 5] {
        [&self.drainage, &self.volcanism, &self.salinity, &self.biome, &self.savagery]
    }

    /// The fields `game::classify` reads for raster cell `i`; `is_lake` from
    /// the caller (the lake runs).
    pub fn cell_climate_at(&self, i: usize, is_lake: bool) -> CellClimate {
        CellClimate {
            height_m: self.height.get(i),
            temperature_c: self.temperature_c.get(i),
            precip_mm: self.precip_mm.get(i),
            drainage: self.drainage[i],
            salinity: self.salinity[i],
            is_lake,
        }
    }

    /// This artifact with its biome, regions and savagery derived from its
    /// other fields at its stored good / evil shares (the defaults when
    /// `knobs` is empty; `forge::regions::derive`; test worlds and tools that
    /// build an artifact by hand).
    pub fn with_regions(mut self) -> WorldArtifact {
        let k = knobs_from_map(&self.knobs);
        crate::forge::regions::derive(&mut self, k.good_share, k.evil_share);
        self
    }

    /// File bytes: `VXWF`, the version (u32 LE), then zstd over bincode with
    /// every raster row-delta encoded.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut wire = self.clone();
        let n = wire.nx as usize;
        for raster in wire.qrasters_mut() {
            delta_rows(&mut raster.data, n);
        }
        for raster in wire.byte_rasters_mut() {
            delta_rows_u8(raster, n);
        }
        delta_rows(&mut wire.region, n);
        climate_to_wire(&mut wire.climate);
        let body = bincode::serialize(&wire).expect("artifact serializes");
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&zstd::bulk::compress(&body, ZSTD_LEVEL).expect("zstd compresses"));
        out
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_bytes(bytes: &[u8]) -> Result<WorldArtifact, ArtifactError> {
        if bytes.len() < 8 || &bytes[..4] != MAGIC {
            return Err(ArtifactError::Format("missing VXWF header".into()));
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version < ARTIFACT_VERSION {
            return Err(ArtifactError::OldVersion { found: version });
        }
        if version > ARTIFACT_VERSION {
            return Err(ArtifactError::Format(format!(
                "version {version} is newer than the {ARTIFACT_VERSION} this build reads"
            )));
        }
        // Streamed: decompressing into a buffer first would hold the whole
        // body (as big as the artifact) alongside it at the load's peak. The
        // BufReader turns bincode's per-element reads into memcpys.
        let decoder = zstd::stream::Decoder::new(&bytes[8..])
            .map_err(|e| ArtifactError::Format(format!("corrupt body: {e}")))?;
        let mut artifact: WorldArtifact =
            bincode::deserialize_from(std::io::BufReader::with_capacity(LOAD_READ_BUFFER_BYTES, decoder))
                .map_err(|e| ArtifactError::Format(format!("corrupt body: {e}")))?;
        artifact.validate()?;
        let n = artifact.nx as usize;
        for raster in artifact.qrasters_mut() {
            undelta_rows(&mut raster.data, n);
        }
        for raster in artifact.byte_rasters_mut() {
            undelta_rows_u8(raster, n);
        }
        undelta_rows(&mut artifact.region, n);
        climate_from_wire(&mut artifact.climate);
        artifact.validate_regions()?;
        Ok(artifact)
    }

    /// The invariants every reader relies on (`from_bytes` and
    /// `ForgedWorld::new` both check them): a grid of `nx * cell_m` by
    /// `nz * cell_m` that is exactly `width_m` by `height_m`, rasters of
    /// `nx * nz`, known rocks, a non-empty rock column sorted by descending
    /// top (the strata lookups binary-search it), and a monthly climate grid
    /// that spans the world (`validate_climate`).
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let artifact = self;
        validate_grid(artifact.nx, artifact.nz, artifact.cell_m, artifact.width_m, artifact.height_m)?;
        validate_wrap(&artifact.edges, artifact.width_m)?;
        validate_climate(&artifact.climate, artifact.width_m, artifact.height_m, &artifact.edges)?;
        let cells = artifact.nx as usize * artifact.nz as usize;
        if artifact.qrasters().iter().any(|raster| raster.len() != cells)
            || artifact.byte_rasters().iter().any(|raster| raster.len() != cells)
            || artifact.region.len() != cells
        {
            return Err(ArtifactError::Format("raster size does not match nx * nz".into()));
        }
        let run_ok = |&(start, len): &(u32, u32)| start as usize + len as usize <= cells;
        if artifact.lakes.iter().any(|lake| !lake.cells.iter().all(run_ok) || lake.level.is_nan()) {
            return Err(ArtifactError::Format("lake cells outside the raster".into()));
        }
        validate_tables(&artifact.column, &artifact.rock_names, &artifact.regions)
    }

    /// Value checks on the biome and region rasters (`validate` checks their
    /// sizes; on the wire the region ids are row-delta encoded, so the ids are
    /// checked once decoded).
    pub fn validate_regions(&self) -> Result<(), ArtifactError> {
        if self.biome.iter().any(|&b| Biome::from_u8(b).is_none()) {
            return Err(ArtifactError::Format("unknown biome".into()));
        }
        if self.region.iter().any(|&r| r as usize >= self.regions.len()) {
            return Err(ArtifactError::Format("region id out of range".into()));
        }
        Ok(())
    }

    /// Writes the world file atomically: the bytes go to a temp file next to
    /// `path`, then a rename puts it in place, so an interrupted save never
    /// leaves a truncated world behind (a stale temp file at worst).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(&self, path: &Path) -> Result<(), ArtifactError> {
        let tmp = save_temp_path(path);
        let written = std::fs::write(&tmp, self.to_bytes()).and_then(|()| std::fs::rename(&tmp, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written.map_err(ArtifactError::Io)
    }
}

/// A grid of `nx` x `nz` cells of `cell_m` that is exactly `width_m` x
/// `height_m`: tile geometry, the river buckets and the sampler's edges all
/// assume the raster spans the map exactly.
fn validate_grid(nx: u32, nz: u32, cell_m: f64, width_m: f64, height_m: f64) -> Result<(), ArtifactError> {
    if nx == 0 || nz == 0 || !(cell_m > 0.0) || !(width_m > 0.0) || !(height_m > 0.0) {
        return Err(ArtifactError::Format("empty world".into()));
    }
    if f64::from(nx) * cell_m != width_m || f64::from(nz) * cell_m != height_m {
        return Err(ArtifactError::Format(format!(
            "{nx} x {nz} cells of {cell_m} m are not the world's {width_m} x {height_m} m"
        )));
    }
    Ok(())
}

/// The world is a Planet (`forge::world::PLANET_EDGES`: a world of a
/// deleted layout is refused, whatever its version says — a store checks only
/// its tile and map versions), wide enough to walk round
/// (`forge::play::MIN_WRAP_WIDTH_M`) and a whole number of
/// `forge::play::WRAP_WIDTH_STEP_M` wide (else its regions do not tile the
/// circle and `wrap_shift_blocks` silently stops folding them).
fn validate_wrap(edges: &[EdgeKind; 4], width_m: f64) -> Result<(), ArtifactError> {
    use crate::forge::play::{too_narrow_to_wrap, WRAP_WIDTH_STEP_M};
    if *edges != crate::forge::world::PLANET_EDGES {
        return Err(ArtifactError::Format(format!(
            "this world was forged with a layout this build no longer has (edges {edges:?}, not a Planet's): \
             re-forge this world"
        )));
    }
    if let Some(why) = too_narrow_to_wrap(width_m) {
        return Err(ArtifactError::Format(format!("{why}: re-forge this world")));
    }
    if (width_m / WRAP_WIDTH_STEP_M).fract() != 0.0 {
        return Err(ArtifactError::Format(format!(
            "this world is {width_m:.0} m wide, not a whole number of {WRAP_WIDTH_STEP_M:.0} m: re-forge this world"
        )));
    }
    Ok(())
}

/// The monthly climate's checks (a whole world's artifact, a tiled world's
/// meta — and `forged_tiles::decode_meta` before it un-deltas the planes):
/// its own grid and plane sizes (`MonthlyClimate::validate`), a grid that
/// spans the world exactly, and `wrap_x` equal to whether the east edge
/// wraps.
pub(crate) fn validate_climate(
    c: &game::MonthlyClimate,
    width_m: f64,
    height_m: f64,
    edges: &[EdgeKind; 4],
) -> Result<(), ArtifactError> {
    let bad = |why: String| ArtifactError::Format(format!("monthly climate: {why}"));
    c.validate().map_err(bad)?;
    if f64::from(c.nx) * c.cell_m != width_m || f64::from(c.nz) * c.cell_m != height_m {
        return Err(bad(format!(
            "{} x {} cells of {} m are not the world's {width_m} x {height_m} m",
            c.nx, c.nz, c.cell_m
        )));
    }
    let wraps = edges[1] == EdgeKind::Wrap;
    if c.wrap_x != wraps {
        return Err(bad(format!("wrap_x is {} but the world's east edge is {:?}", c.wrap_x, edges[1])));
    }
    Ok(())
}

/// Row-delta encodes a plane in rows of `n` (`delta_rows`' rule, wrapping
/// `sub`). A month plane is `n · nz` values, so rows never straddle two
/// months: every month is encoded on its own. Walks `chunks_mut`, so a plane
/// of any length is safe (a wrong one is refused by its size check).
#[cfg(not(target_arch = "wasm32"))]
fn delta_plane<T: Copy>(plane: &mut [T], n: usize, sub: fn(T, T) -> T) {
    for row in plane.chunks_mut(n.max(1)) {
        for i in (1..row.len()).rev() {
            row[i] = sub(row[i], row[i - 1]);
        }
    }
}

/// Inverse of [`delta_plane`] (`add` the wrapping inverse of its `sub`).
#[cfg(not(target_arch = "wasm32"))]
fn undelta_plane<T: Copy>(plane: &mut [T], n: usize, add: fn(T, T) -> T) {
    for row in plane.chunks_mut(n.max(1)) {
        for i in 1..row.len() {
            row[i] = add(row[i], row[i - 1]);
        }
    }
}

/// The monthly climate's wire form (the world file and the tiled meta):
/// every month plane of the six fields row-delta encoded in rows of the
/// CLIMATE grid's `nx`; bincode and zstd follow with the rest of the struct.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn climate_to_wire(c: &mut game::MonthlyClimate) {
    let n = c.nx as usize;
    delta_plane(&mut c.temp_sea_centi_c, n, i16::wrapping_sub);
    delta_plane(&mut c.diurnal_deci_c, n, u8::wrapping_sub);
    delta_plane(&mut c.precip_deci_mm, n, u16::wrapping_sub);
    delta_plane(&mut c.wet_day_255, n, u8::wrapping_sub);
    delta_plane(&mut c.wind_east_q, n, i8::wrapping_sub);
    delta_plane(&mut c.wind_south_q, n, i8::wrapping_sub);
}

/// Inverse of [`climate_to_wire`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn climate_from_wire(c: &mut game::MonthlyClimate) {
    let n = c.nx as usize;
    undelta_plane(&mut c.temp_sea_centi_c, n, i16::wrapping_add);
    undelta_plane(&mut c.diurnal_deci_c, n, u8::wrapping_add);
    undelta_plane(&mut c.precip_deci_mm, n, u16::wrapping_add);
    undelta_plane(&mut c.wet_day_255, n, u8::wrapping_add);
    undelta_plane(&mut c.wind_east_q, n, i8::wrapping_add);
    undelta_plane(&mut c.wind_south_q, n, i8::wrapping_add);
}

/// Bytes the monthly climate alone takes packed as the world file packs it
/// (`climate_to_wire`, bincode, zstd at the file's level): the forge
/// report's `climate_bytes_packed`.
#[cfg(not(target_arch = "wasm32"))]
pub fn climate_packed_len(c: &game::MonthlyClimate) -> usize {
    let mut wire = c.clone();
    climate_to_wire(&mut wire);
    let body = bincode::serialize(&wire).expect("monthly climate serializes");
    zstd::bulk::compress(&body, ZSTD_LEVEL).expect("zstd compresses").len()
}

/// The checks on a world's global tables that every reader relies on (whole
/// and tiled worlds): known rocks, a non-empty rock column sorted by
/// descending top (the strata lookups binary-search it), region records
/// numbered `0..`.
fn validate_tables(column: &[(f32, u8)], rock_names: &[String], regions: &[RegionRecord]) -> Result<(), ArtifactError> {
    // Every rock must be one this build has a block for: an unknown name
    // would otherwise quietly become some other stone.
    if let Some(name) = rock_names.iter().find(|name| rock_index(name).is_none()) {
        return Err(ArtifactError::Format(format!("unknown rock {name}")));
    }
    if column.is_empty() {
        return Err(ArtifactError::Format("empty rock column".into()));
    }
    let known = rock_names.len();
    if let Some(&(_, rock)) = column.iter().find(|&&(_, rock)| rock as usize >= known) {
        return Err(ArtifactError::Format(format!("rock index {rock} out of range")));
    }
    // `!(a >= b)` also rejects a NaN top.
    if column.windows(2).any(|w| !(w[0].0 >= w[1].0)) {
        return Err(ArtifactError::Format("rock column not sorted by descending top".into()));
    }
    if regions.len() > crate::forge::regions::MAX_REGIONS || regions.iter().enumerate().any(|(i, r)| r.id as usize != i)
    {
        return Err(ArtifactError::Format("region records are not numbered 0..".into()));
    }
    Ok(())
}

/// The temp file [`WorldArtifact::save`] writes before renaming it to `path`:
/// same directory (so the rename stays on one filesystem), `.<name>.tmp`.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map_or_else(|| "world".into(), |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.tmp"))
}

/// One straight piece of a river centerline, in artifact meters (tiles carry
/// them with their global ids, `forged_tiles::Tile::segments`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub(crate) a: (f64, f64),
    pub(crate) b: (f64, f64),
    pub(crate) water: (f64, f64),
    pub(crate) half_width: f64,
    pub(crate) depth: f64,
}

impl Segment {
    /// This segment moved `dx` metres along x (an image across the seam).
    fn shifted(self, dx: f64) -> Segment {
        Segment { a: (self.a.0 + dx, self.a.1), b: (self.b.0 + dx, self.b.1), ..self }
    }
}

/// The five quantised rasters (`WorldArtifact::qrasters` order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QRasterId {
    Height,
    Uplifted,
    Bedding,
    TemperatureC,
    PrecipMm,
}

impl QRasterId {
    pub const ALL: [QRasterId; 5] =
        [QRasterId::Height, QRasterId::Uplifted, QRasterId::Bedding, QRasterId::TemperatureC, QRasterId::PrecipMm];
}

/// The five `u8` (0..100) rasters (`WorldArtifact::byte_rasters` order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteRasterId {
    Drainage,
    Volcanism,
    Salinity,
    Biome,
    Savagery,
}

impl ByteRasterId {
    pub const ALL: [ByteRasterId; 5] = [
        ByteRasterId::Drainage,
        ByteRasterId::Volcanism,
        ByteRasterId::Salinity,
        ByteRasterId::Biome,
        ByteRasterId::Savagery,
    ];
}

/// A forged world's fields that are not per-cell rasters: small, always
/// resident, read through [`ForgedWorld::globals`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldGlobals {
    pub seed: u64,
    /// West-east extent (x), metres (`WorldArtifact::width_m`).
    pub width_m: f64,
    /// North-south extent (z), metres.
    pub height_m: f64,
    pub cell_m: f64,
    /// Columns (x).
    pub nx: u32,
    /// Rows (z).
    pub nz: u32,
    /// Where players start, artifact meters (`WorldArtifact::spawn`).
    pub spawn: [f64; 2],
    pub edges: [EdgeKind; 4],
    /// Formations from the top down (`WorldArtifact::column`).
    pub column: Vec<(f32, u8)>,
    pub rock_names: Vec<String>,
    pub regions: Vec<RegionRecord>,
    pub volcanoes: Vec<[f32; 2]>,
    /// `(min, step)` of each quantised raster, in [`QRasterId::ALL`] order.
    pub q_scale: [(f32, f32); 5],
    /// The forge's knobs by key (`WorldArtifact::knobs`); read through [`Self::knobs`].
    pub knobs: BTreeMap<String, f64>,
}

impl WorldGlobals {
    pub fn of(a: &WorldArtifact) -> WorldGlobals {
        WorldGlobals {
            seed: a.seed,
            width_m: a.width_m,
            height_m: a.height_m,
            cell_m: a.cell_m,
            nx: a.nx,
            nz: a.nz,
            spawn: a.spawn,
            edges: a.edges,
            column: a.column.clone(),
            rock_names: a.rock_names.clone(),
            regions: a.regions.clone(),
            volcanoes: a.volcanoes.clone(),
            q_scale: a.qrasters().map(|r| (r.min, r.step)),
            knobs: a.knobs.clone(),
        }
    }

    /// The knobs the world was forged with: the defaults overlaid with the
    /// stored map (names this build does not know are ignored).
    pub fn knobs(&self) -> crate::forge::Knobs {
        knobs_from_map(&self.knobs)
    }

    /// River buckets (`BUCKET_M` each) across the map: `(along x, along z)`;
    /// bucket `(bx, bz)` is index `bz * x + bx`.
    pub fn buckets(&self) -> (usize, usize) {
        (buckets_along(self.width_m), buckets_along(self.height_m))
    }

    /// What a tiled world checks before it samples (a whole world's
    /// artifact passes `WorldArtifact::validate` instead).
    pub fn validate(&self) -> Result<(), ArtifactError> {
        validate_grid(self.nx, self.nz, self.cell_m, self.width_m, self.height_m)?;
        validate_wrap(&self.edges, self.width_m)?;
        validate_tables(&self.column, &self.rock_names, &self.regions)
    }
}

/// River buckets along a side of `len_m`.
fn buckets_along(len_m: f64) -> usize {
    (len_m / BUCKET_M).ceil().max(1.0) as usize
}

/// The whole world's rasters in memory, plus what is derived from them at
/// load (river index, coast distance, lake runs).
pub(crate) struct WholeRasters {
    pub(crate) artifact: WorldArtifact,
    /// Distance (m) from the nearest sea cell (height <= 0), from `height`
    /// at load (`habitat::coast_distance_m`).
    coast_dist_m: Vec<f32>,
    pub(crate) segments: Vec<Segment>,
    /// Segment indices per BUCKET_M bucket (row-major, `WorldGlobals::buckets`),
    /// each list ascending (segments are pushed in id order).
    pub(crate) buckets: Vec<Vec<u32>>,
    /// Lake water per cell as sorted, non-overlapping `(start, end, level)`
    /// runs of raster indices (`end` exclusive), from `artifact.lakes`.
    lake_runs: Vec<(u32, u32, f32)>,
}

impl WholeRasters {
    fn new(artifact: WorldArtifact) -> WholeRasters {
        let (buckets_x, buckets_z) = (buckets_along(artifact.width_m), buckets_along(artifact.height_m));
        let mut segments = Vec::new();
        let mut buckets = vec![Vec::new(); buckets_x * buckets_z];
        let wrap = PeriodicX::for_edges(&artifact.edges, artifact.width_m);
        for reach in &artifact.rivers {
            let half_width = reach.width_m as f64 * 0.5;
            let depth = (CHANNEL_DEPTH_PER_SQRT_M * (reach.width_m as f64).sqrt())
                .clamp(CHANNEL_DEPTH_M.0, CHANNEL_DEPTH_M.1);
            let reach_m = half_width + LEVEE_WIDTH_M + BANK_REACH_M;
            let mut add = |seg: Segment| {
                let bucket = |v: f64, per: usize| ((v / BUCKET_M).floor().max(0.0) as usize).min(per - 1);
                let x0 = bucket(seg.a.0.min(seg.b.0) - reach_m, buckets_x);
                let x1 = bucket(seg.a.0.max(seg.b.0) + reach_m, buckets_x);
                let z0 = bucket(seg.a.1.min(seg.b.1) - reach_m, buckets_z);
                let z1 = bucket(seg.a.1.max(seg.b.1) + reach_m, buckets_z);
                for bz in z0..=z1 {
                    for bx in x0..=x1 {
                        buckets[bz * buckets_x + bx].push(segments.len() as u32);
                    }
                }
                segments.push(seg);
            };
            for pair in reach.points.windows(2) {
                let (p, q) = (pair[0], pair[1]);
                if p.in_lake && q.in_lake {
                    continue; // the lake's own water covers this stretch
                }
                let mut seg = Segment {
                    a: (p.x as f64, p.z as f64),
                    b: (q.x as f64, q.z as f64),
                    water: (p.water as f64, q.water as f64),
                    half_width,
                    depth,
                };
                let Some(wrap) = wrap else {
                    add(seg);
                    continue;
                };
                // Stored points are folded into the map's width: a pair that
                // steps across the seam is read the short way round (`b`
                // unwrapped past the edge), and the piece reaching past an
                // edge is bucketed again shifted by the width — the copy a
                // point on the other side of the seam finds.
                let width = wrap.period_m;
                if seg.b.0 - seg.a.0 > 0.5 * width {
                    seg.b.0 -= width;
                } else if seg.a.0 - seg.b.0 > 0.5 * width {
                    seg.b.0 += width;
                }
                add(seg);
                if seg.a.0.min(seg.b.0) - reach_m < 0.0 {
                    add(seg.shifted(width));
                }
                if seg.a.0.max(seg.b.0) + reach_m >= width {
                    add(seg.shifted(-width));
                }
            }
        }
        // Height is held once, as the artifact's `QRaster`: the sampler reads
        // it through `QRaster::get` (one multiply-add, the same values a
        // decoded `f32` copy held), which saves 4 B/cell over keeping both.
        let height = &artifact.height;
        let coast_dist_m =
            crate::habitat::coast_distance_by(
                artifact.nx as usize,
                artifact.nz as usize,
                artifact.cell_m,
                artifact.edges[1] == EdgeKind::Wrap,
                |i| height.get(i) <= 0.0,
            );
        let mut lake_runs: Vec<(u32, u32, f32)> = artifact
            .lakes
            .iter()
            .flat_map(|lake| lake.cells.iter().map(move |&(start, len)| (start, start + len, lake.level)))
            .collect();
        lake_runs.sort_by_key(|run| run.0);
        WholeRasters { artifact, coast_dist_m, segments, buckets, lake_runs }
    }
}

/// Where a forged world's per-cell data lives. Every read takes a CLAMPED
/// global raster index (`iz * nx + ix`, `ix` in `0..nx`, `iz` in `0..nz`) or a global bucket
/// index, so a tiled store can route it to the tile holding that cell.
pub(crate) enum Rasters {
    Whole(WholeRasters),
    /// Tiles pulled on demand through an LRU (`forged_tiles`).
    Tiled(std::sync::Arc<crate::forged_tiles::TileCache>),
}

impl Rasters {
    /// Quantised raster `r` at cell `i`.
    pub(crate) fn q(&self, r: QRasterId, i: usize) -> f32 {
        match self {
            Rasters::Whole(w) => {
                let a = &w.artifact;
                match r {
                    QRasterId::Height => a.height.get(i),
                    QRasterId::Uplifted => a.uplifted.get(i),
                    QRasterId::Bedding => a.bedding.get(i),
                    QRasterId::TemperatureC => a.temperature_c.get(i),
                    QRasterId::PrecipMm => a.precip_mm.get(i),
                }
            }
            Rasters::Tiled(t) => t.q(r, i),
        }
    }

    /// `u8` raster `r` at cell `i`.
    pub(crate) fn byte(&self, r: ByteRasterId, i: usize) -> u8 {
        match self {
            Rasters::Whole(w) => {
                let a = &w.artifact;
                match r {
                    ByteRasterId::Drainage => a.drainage[i],
                    ByteRasterId::Volcanism => a.volcanism[i],
                    ByteRasterId::Salinity => a.salinity[i],
                    ByteRasterId::Biome => a.biome[i],
                    ByteRasterId::Savagery => a.savagery[i],
                }
            }
            Rasters::Tiled(t) => t.byte(r, i),
        }
    }

    /// Forge region id of cell `i`.
    pub(crate) fn region_id(&self, i: usize) -> u16 {
        match self {
            Rasters::Whole(w) => w.artifact.region[i],
            Rasters::Tiled(t) => t.region_id(i),
        }
    }

    /// Distance (m) from cell `i` to the sea.
    pub(crate) fn coast(&self, i: usize) -> f32 {
        match self {
            Rasters::Whole(w) => w.coast_dist_m[i],
            Rasters::Tiled(t) => t.coast(i),
        }
    }

    /// Water level of cell `i` if it is lake.
    pub(crate) fn lake_level(&self, i: usize) -> Option<f32> {
        match self {
            Rasters::Whole(w) => {
                let i = i as u32;
                let k = w.lake_runs.partition_point(|run| run.0 <= i);
                let &(_, end, level) = w.lake_runs.get(k.checked_sub(1)?)?;
                (i < end).then_some(level)
            }
            Rasters::Tiled(t) => t.lake_level(i),
        }
    }

    /// The river segments that can touch `BUCKET_M` bucket `bucket_index`
    /// (row-major over `WorldGlobals::buckets`), in stored order
    /// (ascending segment id).
    ///
    /// # Panics
    /// If `bucket_index` is not below the bucket count: callers clamp
    /// the bucket coordinates to the map first (`bucket_segments`, `patch`).
    pub(crate) fn bucket(&self, bucket_index: usize) -> SegmentsRef<'_> {
        match self {
            Rasters::Whole(w) => {
                assert!(bucket_index < w.buckets.len(), "river bucket {bucket_index} outside the map");
                SegmentsRef::Whole { ids: w.buckets[bucket_index].iter(), segments: &w.segments }
            }
            Rasters::Tiled(t) => t.bucket(bucket_index),
        }
    }

    fn artifact(&self) -> Option<&WorldArtifact> {
        match self {
            Rasters::Whole(w) => Some(&w.artifact),
            Rasters::Tiled(_) => None,
        }
    }

    /// The monthly climate, whole (resident in both kinds of world: the
    /// artifact's, or the tile cache's copy of the meta's).
    pub(crate) fn climate(&self) -> &game::MonthlyClimate {
        match self {
            Rasters::Whole(w) => &w.artifact.climate,
            Rasters::Tiled(t) => t.climate(),
        }
    }
}

/// The segments of one bucket: `(segment id, segment)` in stored order
/// (ascending id). Segments come by value: a tiled bucket is copied out of
/// its tile.
pub(crate) enum SegmentsRef<'a> {
    Whole { ids: std::slice::Iter<'a, u32>, segments: &'a [Segment] },
    Tiled(std::vec::IntoIter<(u32, Segment)>),
}

impl Iterator for SegmentsRef<'_> {
    type Item = (u32, Segment);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            SegmentsRef::Whole { ids, segments } => ids.next().map(|&s| (s, segments[s as usize])),
            SegmentsRef::Tiled(bucket) => bucket.next(),
        }
    }
}

/// A loaded world plus what sampling needs: the rasters and detail noise.
pub struct ForgedWorld {
    globals: WorldGlobals,
    rasters: Rasters,
    /// crc32 of the file bytes: part of the worldgen disk-cache key.
    content_crc: u32,
    detail: OctaveNoise,
    /// Biome border displacement noise, x and z (`BIOME_DITHER_M`).
    dither: [OctaveNoise; 2],
    /// Tundra snow patches.
    snow_patch: OctaveNoise,
    /// x's period on a world that wraps east–west (`None`: it does not, and
    /// every read takes the edge-clamped path).
    wrap: Option<PeriodicX>,
    /// The same period with the blend band of the runtime noises above
    /// (`PeriodicX::sample_exact`), so they meet themselves at the seam.
    noise_wrap: Option<PeriodicX>,
    /// Cooling per metre of height (deg C / m): the lapse the world was
    /// forged with (`WorldGlobals::knobs`).
    lapse_c_per_m: f64,
}

/// What a forged block column's ground cover, snow and ice come from
/// (`ForgedWorld::column_climate`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnClimate {
    /// The dithered biome (`ForgedWorld::biome_at`).
    pub biome: Biome,
    /// Snow line, m above sea (`ForgedWorld::snow_line_m`).
    pub snow_line_m: f32,
    /// Mean annual temperature at the point (deg C).
    pub temperature_c: f32,
    /// Share (0..1) of glacier cells around the point, bilinear.
    pub glacier_share: f32,
    /// Dry ground here is a salt flat (`SALT_FLAT_MIN_SALINITY`).
    pub salt_flat: bool,
    /// A tundra snow patch.
    pub snow_patch: bool,
}

impl ColumnClimate {
    /// Blocks of ice on lake or sea water here (0 = liquid).
    pub fn ice_blocks(&self) -> i32 {
        if self.temperature_c >= 0.0 {
            return 0;
        }
        ((-self.temperature_c / ICE_C_PER_BLOCK).round() as i32).clamp(1, ICE_MAX_BLOCKS)
    }

    /// Blocks of glacier ice over dry ground here (0 = none).
    pub fn glacier_blocks(&self) -> i32 {
        if self.biome != Biome::Glacier {
            return 0;
        }
        let full = (-self.temperature_c).round().clamp(GLACIER_MIN_BLOCKS as f32, GLACIER_MAX_BLOCKS as f32);
        ((full * self.glacier_share).round() as i32).max(1)
    }
}

/// What stands at one point of a forged world (engine world y, meters).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointSample {
    pub ground_y: f64,
    /// Water surface over this column, if it holds lake or river water.
    /// (The sea is implicit: everything below SEA_LEVEL floods.)
    pub water_y: Option<f64>,
}

impl ForgedWorld {
    /// # Panics
    /// If `artifact` breaks an invariant [`WorldArtifact::validate`] checks
    /// (the same ones a loaded file must pass).
    pub fn new(artifact: WorldArtifact) -> ForgedWorld {
        if let Err(e) = artifact.validate().and_then(|()| artifact.validate_regions()) {
            panic!("ForgedWorld::new: {e}");
        }
        #[cfg(not(target_arch = "wasm32"))]
        let content_crc = crc32fast::hash(&artifact.to_bytes());
        #[cfg(target_arch = "wasm32")]
        let content_crc = 0; // no world files on wasm: nothing keys a cache by it
        Self::with_crc(artifact, content_crc)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &Path) -> Result<ForgedWorld, ArtifactError> {
        let bytes = std::fs::read(path).map_err(ArtifactError::Io)?;
        Self::from_bytes(&bytes)
    }

    /// A world from a world file's bytes (as [`Self::load`] reads them, or
    /// as the world store keeps them); `content_crc` is their crc32.
    /// Native only, like the file codec.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_bytes(bytes: &[u8]) -> Result<ForgedWorld, ArtifactError> {
        let artifact = WorldArtifact::from_bytes(bytes)?;
        Ok(Self::with_crc(artifact, crc32fast::hash(bytes)))
    }

    fn with_crc(artifact: WorldArtifact, content_crc: u32) -> ForgedWorld {
        let globals = WorldGlobals::of(&artifact);
        let rasters = Rasters::Whole(WholeRasters::new(artifact));
        Self::with_rasters(globals, rasters, content_crc)
    }

    /// A world sampled through a tile cache over `loader` (the world store,
    /// or [`crate::forged_tiles::MemTiles`]); bit-identical to the whole
    /// world the tiles were split from. `content_crc` keys the worldgen
    /// cache (the writer's crc over meta, tiles and world map).
    pub fn from_tiles(
        meta: crate::forged_tiles::ForgedMeta,
        loader: std::sync::Arc<dyn crate::forged_tiles::TileLoader>,
        content_crc: u32,
    ) -> Result<ForgedWorld, ArtifactError> {
        let budget = crate::forged_tiles::tile_cache_budget_from_env();
        Self::from_tiles_with_budget(meta, loader, content_crc, budget)
    }

    /// [`Self::from_tiles`] with an explicit tile cache budget (decoded
    /// bytes) instead of `VOXEL_TILE_CACHE_MB` / `TILE_CACHE_BYTES`.
    pub fn from_tiles_with_budget(
        meta: crate::forged_tiles::ForgedMeta,
        loader: std::sync::Arc<dyn crate::forged_tiles::TileLoader>,
        content_crc: u32,
        budget_bytes: usize,
    ) -> Result<ForgedWorld, ArtifactError> {
        meta.validate()?;
        let crate::forged_tiles::ForgedMeta { tile_cells, globals, climate, .. } = meta;
        let cache = crate::forged_tiles::TileCache::new(&globals, tile_cells, climate, loader, budget_bytes);
        Ok(Self::with_rasters(globals, Rasters::Tiled(std::sync::Arc::new(cache)), content_crc))
    }

    /// The world's monthly climate normals (always resident; map metres —
    /// a reader at engine metres converts first).
    pub fn monthly_climate(&self) -> &game::MonthlyClimate {
        self.rasters.climate()
    }

    /// The tile cache of a tiled world (`None` for a whole one).
    pub fn tile_cache(&self) -> Option<&crate::forged_tiles::TileCache> {
        match &self.rasters {
            Rasters::Tiled(cache) => Some(cache.as_ref()),
            Rasters::Whole(_) => None,
        }
    }

    /// Loads and pins every tile a sampling read at engine metres inside
    /// `[min_m, max_m]` can touch: the rect grown by
    /// [`crate::forged_tiles::MAX_READ_RADIUS_CELLS`] cells (every cross-cell
    /// read) and one river bucket (`BUCKET_M`; a point's bucket lives in its
    /// own tile, the extra bucket is slack), clamped to the map as reads are.
    /// While the returned pins live, reads of those tiles never load. A tile
    /// that fails to load is an `Err` naming it — never terrain. A no-op
    /// `Ok` on a whole world.
    pub fn prefetch_rect(&self, min_m: [f64; 2], max_m: [f64; 2]) -> Result<crate::forged_tiles::TilePins, String> {
        let Rasters::Tiled(cache) = &self.rasters else {
            return Ok(crate::forged_tiles::TilePins::default());
        };
        let g = &self.globals;
        let grow = f64::from(crate::forged_tiles::MAX_READ_RADIUS_CELLS) * g.cell_m + BUCKET_M;
        let cell = |v: f64, n: u32| ((v / g.cell_m).floor() as i64).clamp(0, i64::from(n) - 1) as u32;
        if self.wrap.is_some() {
            // The rect in the frame of its west edge's image (x may run past
            // the east edge); the columns it reads fold onto the map.
            let shift = min_m[0] - self.fold_engine_x(min_m[0]);
            let (a0, a1) = (self.to_artifact_raw(min_m[0] - shift, min_m[1]), self.to_artifact_raw(max_m[0] - shift, max_m[1]));
            let column = |v: f64| (v / g.cell_m).floor() as i64;
            let columns =
                cache.tile_columns_wrapping(column(a0.0.min(a1.0) - grow), column(a0.0.max(a1.0) + grow));
            let (t0, t1) = cache.tiles_of_cells(
                (0, cell(a0.1.min(a1.1) - grow, g.nz)),
                (0, cell(a0.1.max(a1.1) + grow, g.nz)),
            );
            return cache.pin_tiles(&columns, (t0.1, t1.1));
        }
        let (a0, a1) = (self.to_artifact(min_m[0], min_m[1]), self.to_artifact(max_m[0], max_m[1]));
        let (t0, t1) = cache.tiles_of_cells(
            (cell(a0.0.min(a1.0) - grow, g.nx), cell(a0.1.min(a1.1) - grow, g.nz)),
            (cell(a0.0.max(a1.0) + grow, g.nx), cell(a0.1.max(a1.1) + grow, g.nz)),
        );
        cache.pin_rect(t0, t1)
    }

    fn with_rasters(globals: WorldGlobals, rasters: Rasters, content_crc: u32) -> ForgedWorld {
        let seed = globals.seed;
        let detail = OctaveNoise::new(seed ^ 0xDE7A_11, DETAIL_FIRST_OCTAVE, vec![1.0, 0.6]);
        let dither = [
            OctaveNoise::new(seed ^ 0xB10E_D17A, BIOME_DITHER_FIRST_OCTAVE, vec![1.0, 1.0]),
            OctaveNoise::new(seed ^ 0xB10E_D17B, BIOME_DITHER_FIRST_OCTAVE, vec![1.0, 1.0]),
        ];
        let snow_patch = OctaveNoise::new(seed ^ 0x5_0B47, TUNDRA_SNOW_FIRST_OCTAVE, vec![1.0, 0.6]);
        let lapse_c_per_m = globals.knobs().lapse_c_per_km / 1000.0;
        let wrap = PeriodicX::from_globals(&globals);
        // The longest wavelength of the three noises (each first octave 2^-o m).
        let longest_m = 2f64.powi(-DETAIL_FIRST_OCTAVE.min(BIOME_DITHER_FIRST_OCTAVE).min(TUNDRA_SNOW_FIRST_OCTAVE));
        let noise_wrap = wrap.map(|w| w.for_noise(longest_m));
        ForgedWorld {
            globals,
            rasters,
            content_crc,
            detail,
            dither,
            snow_patch,
            wrap,
            noise_wrap,
            lapse_c_per_m,
        }
    }

    /// The whole artifact, when the world holds it in memory (`None` for a
    /// tiled world). Tools that need every raster at once (the world map,
    /// examples) run on whole worlds only.
    pub fn artifact(&self) -> Option<&WorldArtifact> {
        self.rasters.artifact()
    }

    /// The world's non-raster fields.
    pub fn globals(&self) -> &WorldGlobals {
        &self.globals
    }

    /// The world's seed (tree placement, detail noise).
    pub fn seed(&self) -> u64 {
        self.globals.seed
    }

    /// The per-cell data (the tile splitter, crate-internal tests).
    pub(crate) fn rasters(&self) -> &Rasters {
        &self.rasters
    }

    pub fn content_crc(&self) -> u32 {
        self.content_crc
    }

    /// Engine world meters -> artifact meters; on a world that wraps
    /// east–west x is folded onto the map first ([`Self::fold_engine_x`]),
    /// so every image of a point reads the same.
    fn to_artifact(&self, x: f64, z: f64) -> (f64, f64) {
        self.to_artifact_raw(self.fold_engine_x(x), z)
    }

    /// Engine world meters -> artifact meters, x not folded (a patch's own
    /// frame, which may run past the east edge).
    fn to_artifact_raw(&self, x: f64, z: f64) -> (f64, f64) {
        (x - ENGINE_SPAWN_M + self.globals.spawn[0], z - ENGINE_SPAWN_M + self.globals.spawn[1])
    }

    /// The image of engine x whose artifact x lies in `[0, width)`:
    /// `x − k · width` for a whole `k` (exact for block and voxel
    /// coordinates), the smallest such image whose artifact x is not
    /// negative. `x` itself on a world that does not wrap.
    fn fold_engine_x(&self, x: f64) -> f64 {
        if self.wrap.is_none() || !x.is_finite() {
            return x;
        }
        let width = self.globals.width_m;
        let ax = |x: f64| self.to_artifact_raw(x, 0.0).0;
        let mut x = x - (ax(x) / width).floor() * width;
        // The division may round across a whole period: settle exactly.
        while ax(x) < 0.0 {
            x += width;
        }
        while ax(x - width) >= 0.0 {
            x -= width;
        }
        x
    }

    /// A noise of this world at `(x, z)`: periodic in x on a world that
    /// wraps (`PeriodicX::sample_exact`), the plain noise otherwise.
    fn noise2(&self, noise: &OctaveNoise, x: f64, z: f64) -> f64 {
        match &self.noise_wrap {
            Some(wrap) => wrap.sample_exact(x, z, |x, z| noise.sample2(x, z)),
            None => noise.sample2(x, z),
        }
    }

    fn cell(&self, ix: i64, iz: i64) -> f64 {
        self.q_cell(QRasterId::Height, ix, iz)
    }

    /// Global index of raster cell `(ix, iz)`, clamped to the edge — x
    /// wrapped round instead on a world that wraps east–west.
    fn clamped(&self, ix: i64, iz: i64) -> usize {
        let (nx, nz) = (i64::from(self.globals.nx), i64::from(self.globals.nz));
        let ix = if self.wrap.is_some() { ix.rem_euclid(nx) } else { ix.clamp(0, nx - 1) };
        let iz = iz.clamp(0, nz - 1);
        (iz * nx + ix) as usize
    }

    /// One cell of quantised raster `r`, clamped to the edge.
    fn q_cell(&self, r: QRasterId, ix: i64, iz: i64) -> f64 {
        self.rasters.q(r, self.clamped(ix, iz)) as f64
    }

    /// Water level of raster cell `i` if it is lake.
    pub(crate) fn lake_level_of(&self, i: usize) -> Option<f32> {
        self.rasters.lake_level(i)
    }

    /// Bilinear interpolation of `uplifted + bedding` between cell centers
    /// (artifact meters), clamped to the edge outside the artifact.
    fn geo_offset_m(&self, ax: f64, az: f64) -> f64 {
        let a = &self.globals;
        let at = |ix: i64, iz: i64| self.q_cell(QRasterId::Uplifted, ix, iz) + self.q_cell(QRasterId::Bedding, ix, iz);
        let (u, v) = (ax / a.cell_m - 0.5, az / a.cell_m - 0.5);
        let (iu, iv) = (u.floor(), v.floor());
        let (tu, tv) = (u - iu, v - iv);
        let (iu, iv) = (iu as i64, iv as i64);
        let row = |iz: i64| at(iu, iz) + tu * (at(iu + 1, iz) - at(iu, iz));
        row(iv) + tv * (row(iv + 1) - row(iv))
    }

    pub(crate) fn to_artifact_m(&self, x: f64, z: f64) -> (f64, f64) {
        self.to_artifact(x, z)
    }

    /// Bilinear interpolation of quantised raster `r` at artifact meters
    /// (same lattice as `geo_offset_m`, edge-clamped).
    pub(crate) fn bilinear_q(&self, r: QRasterId, ax: f64, az: f64) -> f64 {
        self.bilinear_cells(ax, az, |i| f64::from(self.rasters.q(r, i)))
    }

    /// Bilinear interpolation of the coast distance (m) at artifact meters.
    pub(crate) fn bilinear_coast(&self, ax: f64, az: f64) -> f64 {
        self.bilinear_cells(ax, az, |i| f64::from(self.rasters.coast(i)))
    }

    /// Ground height (m relative to sea) at engine meters, as `sample` sees it
    /// before river carving; used by habitat slope.
    pub(crate) fn ground_engine_m(&self, x_m: f64, z_m: f64) -> f64 {
        let (ax, az) = self.to_artifact(x_m, z_m);
        self.ground(ax, az)
    }

    /// Stratigraphic offset (blocks) of the engine column at world meters
    /// `(x, z)`: rock at block Y `y` is `strata_blocks` looked up at
    /// `s = y - geo`. The forge's `s` is meters relative to sea level, and
    /// block `SEA_TOP_BLOCK + 1` is 0 m.
    pub fn geo_offset_blocks(&self, x_m: f64, z_m: f64) -> i16 {
        let (ax, az) = self.to_artifact(x_m, z_m);
        let geo = self.geo_offset_m(ax, az).round() + (SEA_TOP_BLOCK + 1) as f64;
        geo.clamp(i16::MIN as f64, i16::MAX as f64) as i16
    }

    /// The artifact's rock column in blocks: formation tops (in `s`, top
    /// down) rounded to whole blocks, each rock resolved to its block.
    pub fn strata_blocks(&self, mats: &Materials) -> Vec<(i16, BlockId)> {
        let a = &self.globals;
        a.column
            .iter()
            .map(|&(top, rock)| {
                let name = &a.rock_names[rock as usize];
                let index = rock_index(name).unwrap_or_else(|| panic!("world file names unknown rock {name}"));
                ((top as f64).round().clamp(i16::MIN as f64, i16::MAX as f64) as i16, mats.rocks[index])
            })
            .collect()
    }

    /// Engine world meters covered by the artifact: `([min_x, min_z], [max_x, max_z])`,
    /// max exclusive.
    pub fn artifact_bounds_engine_m(&self) -> ([f64; 2], [f64; 2]) {
        let a = &self.globals;
        let min = [ENGINE_SPAWN_M - a.spawn[0], ENGINE_SPAWN_M - a.spawn[1]];
        (min, [min[0] + a.width_m, min[1] + a.height_m])
    }

    /// The rock formation (index into the artifact's `rock_names`) at the
    /// ground surface of the engine column at world meters `(x, z)`, before
    /// soil: the artifact `column` looked up at `s = ground - geo` with
    /// `game::ColumnGround::block_at`'s rule
    /// (the last formation whose top is above `s`, else the top one). Ground
    /// is the interpolated raster (exact at cell centres; no detail noise or
    /// river carving). `None` outside the artifact.
    pub fn surface_rock(&self, x_m: f64, z_m: f64) -> Option<u8> {
        let (ax, az) = self.to_artifact(x_m, z_m);
        if !self.inside(ax, az) {
            return None;
        }
        let s = self.bicubic(ax, az) - self.geo_offset_m(ax, az);
        let column = &self.globals.column;
        column.iter().rev().find(|&&(top, _)| f64::from(top) > s).or(column.first()).map(|&(_, rock)| rock)
    }

    /// Raster cell nearest engine world metres `(x, z)` (clamped to the map).
    fn nearest_cell(&self, x_m: f64, z_m: f64) -> usize {
        let (ax, az) = self.to_artifact(x_m, z_m);
        let at = |v: f64| (v / self.globals.cell_m).floor() as i64;
        self.clamped(at(ax), at(az))
    }

    /// The classification inputs of the raster cell nearest engine world
    /// metres `(x, z)` (clamped to the map).
    pub fn cell_climate(&self, x_m: f64, z_m: f64) -> CellClimate {
        let i = self.nearest_cell(x_m, z_m);
        let r = &self.rasters;
        CellClimate {
            height_m: r.q(QRasterId::Height, i),
            temperature_c: r.q(QRasterId::TemperatureC, i),
            precip_mm: r.q(QRasterId::PrecipMm, i),
            drainage: r.byte(ByteRasterId::Drainage, i),
            salinity: r.byte(ByteRasterId::Salinity, i),
            is_lake: self.lake_level_of(i).is_some(),
        }
    }

    /// The stored biome of the raster cell nearest engine world metres
    /// `(x, z)` (clamped to the map); no dither.
    pub fn biome_cell(&self, x_m: f64, z_m: f64) -> Biome {
        let b = self.rasters.byte(ByteRasterId::Biome, self.nearest_cell(x_m, z_m));
        Biome::from_u8(b).expect("validated biome raster")
    }

    /// The forge region of the raster cell nearest engine world metres
    /// `(x, z)` (clamped to the map): its id and type.
    pub fn region_cell(&self, x_m: f64, z_m: f64) -> (u16, Option<game::RegionType>) {
        let id = self.rasters.region_id(self.nearest_cell(x_m, z_m));
        (id, self.globals.regions.get(usize::from(id)).map(|r| r.kind))
    }

    /// Bilinear interpolation (artifact metres, cell-centre lattice,
    /// edge-clamped) of `f` over raster cell indices. Total: the neighbour
    /// index saturates, so a coordinate past `i64` cells (or infinite) reads
    /// an edge or wrapped cell instead of overflowing.
    fn bilinear_cells(&self, ax: f64, az: f64, f: impl Fn(usize) -> f64) -> f64 {
        let a = &self.globals;
        let at = |ix: i64, iz: i64| f(self.clamped(ix, iz));
        let (u, v) = (ax / a.cell_m - 0.5, az / a.cell_m - 0.5);
        let (iu, iv) = (u.floor(), v.floor());
        let (tu, tv) = (u - iu, v - iv);
        let (iu, iv) = (iu as i64, iv as i64);
        let row = |iz: i64| at(iu, iz) + tu * (at(iu.saturating_add(1), iz) - at(iu, iz));
        row(iv) + tv * (row(iv.saturating_add(1)) - row(iv))
    }

    /// The point a block column reads its biome at: engine metres `(x, z)`
    /// displaced by up to `BIOME_DITHER_M` on each axis.
    fn dithered(&self, x_m: f64, z_m: f64) -> (f64, f64) {
        let d = |k: usize| BIOME_DITHER_M * (BIOME_DITHER_GAIN * self.noise2(&self.dither[k], x_m, z_m)).clamp(-1.0, 1.0);
        (x_m + d(0), z_m + d(1))
    }

    /// The biome of the block column at engine metres `(x, z)`: the stored
    /// biome of the cell nearest the dithered point (`BIOME_DITHER_M`), so
    /// borders between biomes are irregular at block scale. A pure function
    /// of the point.
    pub fn biome_at(&self, x_m: f64, z_m: f64) -> Biome {
        let (x, z) = self.dithered(x_m, z_m);
        self.biome_cell(x, z)
    }

    /// Snow line (m above sea) at engine metres `(x, z)`: the height where
    /// the mean annual temperature reaches `SNOW_LINE_C`, from each cell's
    /// stored temperature and ground (the sea's at 0) and the forge's lapse,
    /// bilinear between cells.
    pub fn snow_line_m(&self, x_m: f64, z_m: f64) -> f32 {
        let (ax, az) = self.to_artifact(x_m, z_m);
        let r = &self.rasters;
        let line = |i: usize| {
            let ground = f64::from(r.q(QRasterId::Height, i)).max(0.0);
            ground + f64::from(r.q(QRasterId::TemperatureC, i) - SNOW_LINE_C) / self.lapse_c_per_m
        };
        self.bilinear_cells(ax, az, line) as f32
    }

    /// Everything a block column's ground cover, snow and ice depend on, at
    /// engine metres `(x, z)` (edge-clamped past the map).
    pub fn column_climate(&self, x_m: f64, z_m: f64) -> ColumnClimate {
        let (dx, dz) = self.dithered(x_m, z_m);
        let biome = self.biome_cell(dx, dz); // = biome_at(x_m, z_m)
        let (ax, az) = self.to_artifact(x_m, z_m);
        let r = &self.rasters;
        let temperature_c = self.bilinear_q(QRasterId::TemperatureC, ax, az) as f32;
        let glacier = Biome::Glacier as u8;
        let glacier_share =
            self.bilinear_cells(ax, az, |i| f64::from(u8::from(r.byte(ByteRasterId::Biome, i) == glacier))) as f32;
        let salt_flat = {
            let (dax, daz) = self.to_artifact(dx, dz);
            self.bilinear_cells(dax, daz, |i| f64::from(r.byte(ByteRasterId::Salinity, i))) >= SALT_FLAT_MIN_SALINITY
                && self.bilinear_coast(ax, az) > f64::from(crate::habitat::SALT_REACH_M)
        };
        let snow_patch = biome == Biome::Tundra && self.noise2(&self.snow_patch, x_m, z_m) > TUNDRA_SNOW_PATCH_MIN;
        ColumnClimate {
            biome,
            snow_line_m: self.snow_line_m(x_m, z_m),
            temperature_c,
            glacier_share,
            salt_flat,
            snow_patch,
        }
    }

    /// Where artifact point `(ax, az)` lies: inside, or past a sea edge (on
    /// the Planet north or south). On a world that wraps east–west (the
    /// Planet) no point is past the east or west edge: x runs on round the
    /// world. `Place::Land` (the point clamped onto the border) is for land
    /// edges, which no world that loads has.
    fn place(&self, ax: f64, az: f64) -> Place {
        let (width, height) = (self.globals.width_m, self.globals.height_m);
        let wrap = self.wrap.is_some();
        // `edges` order: north (z < 0), east (x >= width), south (z >= height), west (x < 0).
        let past = [az < 0.0, !wrap && ax >= width, az >= height, !wrap && ax < 0.0];
        if !past.contains(&true) {
            return Place::Inside;
        }
        if (0..4).any(|e| past[e] && self.globals.edges[e] == EdgeKind::Sea) {
            return Place::Sea;
        }
        Place::Land(if wrap { ax } else { ax.clamp(0.0, width) }, az.clamp(0.0, height))
    }

    /// Whether artifact point `(ax, az)` lies on the map (any x on a world
    /// that wraps east–west).
    fn inside(&self, ax: f64, az: f64) -> bool {
        let x_inside = self.wrap.is_some() || (ax >= 0.0 && ax < self.globals.width_m);
        x_inside && az >= 0.0 && az < self.globals.height_m
    }

    /// Height of raster cell `(ix, iz)` as the sampler extends the map: the
    /// cell itself inside and [`OUTSIDE_HEIGHT_M`] past a sea edge (north or
    /// south on the Planet; `place`'s rules, by cell; x wraps round on a world
    /// that wraps east–west).
    pub fn extended_cell_height(&self, ix: i64, iz: i64) -> f64 {
        let (nx, nz) = (i64::from(self.globals.nx), i64::from(self.globals.nz));
        let wrap = self.wrap.is_some();
        let past = [iz < 0, !wrap && ix >= nx, iz >= nz, !wrap && ix < 0];
        if (0..4).any(|e| past[e] && self.globals.edges[e] == EdgeKind::Sea) {
            return OUTSIDE_HEIGHT_M;
        }
        self.cell(ix, iz)
    }

    /// Catmull-Rom interpolation of the height raster (artifact meters),
    /// edge-clamped: callers decide what lies outside (`place`).
    fn bicubic(&self, ax: f64, az: f64) -> f64 {
        fn cubic(p: [f64; 4], t: f64) -> f64 {
            p[1] + 0.5
                * t
                * (p[2] - p[0]
                    + t * (2.0 * p[0] - 5.0 * p[1] + 4.0 * p[2] - p[3]
                        + t * (3.0 * (p[1] - p[2]) + p[3] - p[0])))
        }
        let (u, v) = (ax / self.globals.cell_m - 0.5, az / self.globals.cell_m - 0.5);
        let (iu, iv) = (u.floor(), v.floor());
        let (tu, tv) = (u - iu, v - iv);
        let (iu, iv) = (iu as i64, iv as i64);
        let mut rows = [0.0; 4];
        for (k, row) in rows.iter_mut().enumerate() {
            let z = iv - 1 + k as i64;
            *row = cubic(
                [self.cell(iu - 1, z), self.cell(iu, z), self.cell(iu + 1, z), self.cell(iu + 2, z)],
                tu,
            );
        }
        cubic(rows, tv)
    }

    /// Smooth ground before water features: interpolated raster + detail
    /// noise. Meters relative to sea; artifact coordinates. Past a sea edge
    /// (north or south on the Planet) it is ocean at [`OUTSIDE_HEIGHT_M`];
    /// east–west x runs on round the world.
    fn ground(&self, ax: f64, az: f64) -> f64 {
        let (ax, az) = match self.place(ax, az) {
            Place::Inside => (ax, az),
            Place::Land(cx, cz) => (cx, cz),
            Place::Sea => return OUTSIDE_HEIGHT_M,
        };
        let g = self.bicubic(ax, az);
        let fade = ((g - DETAIL_FADE_M.0) / (DETAIL_FADE_M.1 - DETAIL_FADE_M.0)).clamp(0.0, 1.0);
        if fade <= 0.0 {
            return g;
        }
        let e = self.globals.cell_m * 0.5;
        let (dx, dz) = (
            (self.bicubic(ax + e, az) - self.bicubic(ax - e, az)) / (2.0 * e),
            (self.bicubic(ax, az + e) - self.bicubic(ax, az - e)) / (2.0 * e),
        );
        let slope = (dx * dx + dz * dz).sqrt();
        let amplitude = (DETAIL_BASE_M + DETAIL_PER_SLOPE_M * slope).min(DETAIL_MAX_M);
        g + fade * amplitude * 2.0 * self.noise2(&self.detail, ax, az)
    }

    /// Lake level of raster cell `(ix, iz)`; none off the map (x wraps
    /// round on a world that wraps east–west).
    fn lake_level_at(&self, ix: i64, iz: i64) -> Option<f64> {
        let (nx, nz) = (i64::from(self.globals.nx), i64::from(self.globals.nz));
        let ix = if self.wrap.is_some() { ix.rem_euclid(nx) } else { ix };
        if ix < 0 || iz < 0 || ix >= nx || iz >= nz {
            return None;
        }
        self.lake_level_of((iz * nx + ix) as usize).map(f64::from)
    }

    /// Water features applied to an already-interpolated ground height `g`
    /// (meters relative to sea; artifact coordinates). `segments` are the
    /// candidates near this point, in ascending order of segment id.
    /// The third value says whether the water is a lake's (else a river's).
    fn refine(
        &self,
        ax: f64,
        az: f64,
        mut g: f64,
        segments: impl IntoIterator<Item = Segment>,
        lakes: bool,
    ) -> (f64, Option<f64>, bool) {
        // Water surfaces sit on whole blocks (sea level is one), so every
        // level is snapped BEFORE banks are measured against it: a bank built
        // to the raw level could end up under a surface that rounded up.
        let snap = |level: f64| (level + 0.5).floor();
        let mut water = None;
        // Lakes: water where the cell is lake and the ground lies below its
        // level; a berm on the dry cells around it, so the basin holds water
        // even where interpolation left the rim a little low.
        let (ix, iz) = ((ax / self.globals.cell_m).floor() as i64, (az / self.globals.cell_m).floor() as i64);
        if !lakes {
            // The caller knows no lake cell is near this window.
        } else if let Some(level) = self.lake_level_at(ix, iz) {
            if g < snap(level) {
                water = Some(snap(level));
            }
        } else {
            for (ox, oz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                if let Some(level) = self.lake_level_at(ix + ox, iz + oz) {
                    g = g.max(snap(level) + FREEBOARD_M);
                }
            }
        }

        let lake = water.is_some();

        // Rivers: nearest centerline decides. Inside the channel the bed is
        // cut below the water surface; a levee ring outside it stands above
        // the water; further out the valley side is cut back to a bank slope.
        let mut nearest: Option<(f64, f64, Segment)> = None; // (distance, water, segment)
        for seg in segments {
            let (vx, vz) = (seg.b.0 - seg.a.0, seg.b.1 - seg.a.1);
            let len2 = vx * vx + vz * vz;
            let t = if len2 > 0.0 {
                (((ax - seg.a.0) * vx + (az - seg.a.1) * vz) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (px, pz) = (seg.a.0 + vx * t - ax, seg.a.1 + vz * t - az);
            let d = (px * px + pz * pz).sqrt();
            // Compare in channel-relative distance so a wide river wins over a
            // brook whose centerline happens to be a little closer.
            let edge = d - seg.half_width;
            if nearest.as_ref().is_none_or(|n| edge < n.0 - n.2.half_width) {
                nearest = Some((d, seg.water.0 + (seg.water.1 - seg.water.0) * t, seg));
            }
        }
        if let Some((d, w, seg)) = nearest {
            let w = snap(w);
            let a = seg.half_width;
            if water.is_none() || d < a {
                if d < a {
                    let r = d / a;
                    g = g.min(w - seg.depth * (1.0 - r * r));
                    // Inside a lake the lake's level stands; elsewhere the river's.
                    water = water.or(Some(w));
                } else if d < a + LEVEE_WIDTH_M {
                    g = g.max(w + FREEBOARD_M).min(w + FREEBOARD_M + 1.0);
                } else if d < a + LEVEE_WIDTH_M + BANK_REACH_M {
                    g = g.min(w + FREEBOARD_M + 1.0 + (d - a - LEVEE_WIDTH_M) * BANK_SLOPE);
                }
            }
        }
        // Water at or below the sea is just the sea.
        let water = water.filter(|&w| w > 0.0);
        (g, water, lake && water.is_some())
    }

    /// The river segments of the bucket holding artifact point `(ax, az)`
    /// (none off the map).
    fn bucket_segments(&self, ax: f64, az: f64) -> impl Iterator<Item = Segment> + '_ {
        let index = self.inside(ax, az).then(|| {
            let (per_x, per_z) = self.globals.buckets();
            // (A folded x can round up to the width itself: the last bucket.)
            let (bx, bz) = ((ax / BUCKET_M) as usize, (az / BUCKET_M) as usize);
            bz.min(per_z - 1) * per_x + bx.min(per_x - 1)
        });
        index.map(|b| self.rasters.bucket(b)).into_iter().flatten().map(|(_, seg)| seg)
    }

    /// Exact (un-latticed) sample at one engine-world point: map renderers, tests.
    /// Past the north and south sea edges there are no rivers or lakes, only
    /// ocean; east–west the Planet wraps round.
    pub fn sample(&self, x: f64, z: f64) -> PointSample {
        let (ax, az) = self.to_artifact(x, z);
        if self.place(ax, az) != Place::Inside {
            return PointSample { ground_y: SEA_LEVEL + self.ground(ax, az), water_y: None };
        }
        let (g, water, _) = self.refine(ax, az, self.ground(ax, az), self.bucket_segments(ax, az), true);
        PointSample { ground_y: SEA_LEVEL + g, water_y: water.map(|w| SEA_LEVEL + w) }
    }

    /// The terrain of one region- or LOD-tile-sized window (engine meters).
    pub fn patch(&self, origin_x: f64, origin_z: f64, span_m: f64) -> ForgedPatch<'_> {
        // Ground on the same corner lattice the procedural path uses: corners
        // are pure functions of world coordinates, so neighboring windows
        // agree bit-for-bit along shared edges.
        let grid = ProfileGrid::from_fn(origin_x, origin_z, span_m, |x, z| {
            let (ax, az) = self.to_artifact(x, z);
            ColumnProfile::from_surface(SEA_LEVEL + self.ground(ax, az))
        });
        // The window's own frame: artifact metres from its west edge's image
        // on (`shift` = 0 unless the world wraps), so x runs on past the
        // east edge of a world that wraps instead of jumping back.
        let shift = origin_x - self.fold_engine_x(origin_x);
        // Every river segment that can touch the window.
        let (a0, a1) =
            (self.to_artifact_raw(origin_x - shift, origin_z), self.to_artifact_raw(origin_x + span_m - shift, origin_z + span_m));
        let (width, height) = (self.globals.width_m, self.globals.height_m);
        let wrap = self.wrap.is_some();
        let overlaps = (wrap || (a1.0 >= 0.0 && a0.0 < width)) && a1.1 >= 0.0 && a0.1 < height;
        // (segment id, image k, the segment moved k widths east), by id then k.
        let mut found: Vec<(u32, u32, Segment)> = Vec::new();
        if overlaps {
            let (per_x, per_z) = self.globals.buckets();
            let bucket = |v: f64, per: usize| ((v / BUCKET_M).floor().max(0.0) as usize).min(per - 1);
            // Each image k of the map the window reaches, as a folded x span.
            let images = if wrap { (a1.0 / width).floor().max(0.0) as u32 } else { 0 };
            for k in 0..=images {
                let off = f64::from(k) * width;
                let (lo, hi) = if wrap { ((a0.0 - off).max(0.0), (a1.0 - off).min(width)) } else { (a0.0, a1.0) };
                for bz in bucket(a0.1, per_z)..=bucket(a1.1, per_z) {
                    for bx in bucket(lo, per_x)..=bucket(hi, per_x) {
                        let segs = self.rasters.bucket(bz * per_x + bx);
                        found.extend(segs.map(|(id, seg)| (id, k, if k == 0 { seg } else { seg.shifted(off) })));
                    }
                }
            }
            found.sort_unstable_by_key(|&(id, k, _)| (id, k));
            found.dedup_by_key(|&mut (id, k, _)| (id, k));
        }
        // Ascending segment id, as `refine` expects; cells index into it.
        let segments: Vec<Segment> = found.into_iter().map(|(_, _, seg)| seg).collect();
        // Narrow to PATCH_CELL_M cells (artifact coordinates, window-local).
        let cells_per_side = (span_m / PATCH_CELL_M).ceil() as usize + 1;
        let mut cells = vec![Vec::new(); if segments.is_empty() { 0 } else { cells_per_side * cells_per_side }];
        if !segments.is_empty() {
            let cell = |v: f64, origin: f64| {
                (((v - origin) / PATCH_CELL_M).floor().max(0.0) as usize).min(cells_per_side - 1)
            };
            for (s, seg) in segments.iter().enumerate() {
                let s = s as u32;
                let reach = seg.half_width + LEVEE_WIDTH_M + BANK_REACH_M;
                let (x0, x1) = (seg.a.0.min(seg.b.0) - reach, seg.a.0.max(seg.b.0) + reach);
                let (z0, z1) = (seg.a.1.min(seg.b.1) - reach, seg.a.1.max(seg.b.1) + reach);
                if x1 < a0.0 || z1 < a0.1 || x0 > a1.0 || z0 > a1.1 {
                    continue;
                }
                for cz in cell(z0, a0.1)..=cell(z1, a0.1) {
                    for cx in cell(x0, a0.0)..=cell(x1, a0.0) {
                        cells[cz * cells_per_side + cx].push(s);
                    }
                }
            }
        }
        // Any lake cell in the window, one cell of margin for the shore berm?
        let cell_m = self.globals.cell_m;
        let (nx, nz) = (i64::from(self.globals.nx), i64::from(self.globals.nz));
        let range = |lo: f64, hi: f64, n: i64| {
            (((lo / cell_m).floor() as i64 - 1).clamp(0, n - 1), ((hi / cell_m).floor() as i64 + 1).clamp(0, n - 1))
        };
        let ((x0, x1), (z0, z1)) = (range(a0.0, a1.0, nx), range(a0.1, a1.1, nz));
        let lakes = overlaps
            && if wrap {
                // Columns unclamped (`lake_level_at` wraps them), at most one lap.
                let (x0, x1) = ((a0.0 / cell_m).floor() as i64 - 1, (a1.0 / cell_m).floor() as i64 + 1);
                let x1 = x1.min(x0 + nx - 1);
                (z0..=z1).any(|iz| (x0..=x1).any(|ix| self.lake_level_at(ix, iz).is_some()))
            } else {
                (z0..=z1).any(|iz| (x0..=x1).any(|ix| self.lake_level_of((iz * nx + ix) as usize).is_some()))
            };
        ForgedPatch { world: self, grid, origin: a0, shift, segments, cells, cells_per_side, lakes }
    }
}

// ---- Phase 1b: monthly climate normals at a world position ----

/// A place's monthly normals at the ground from its sea-level normals `sea`:
/// every temperature lowered by `lapse_c_per_m * ground_m` (`ground_m` is
/// already floored at 0 by the caller: metres above the sea), and the twelve
/// precipitations replaced by `annual_precip_mm` split in `sea`'s own
/// monthly shares (`P_m = A · N_m / Σ N`, months added in order as `f64`; an
/// equal twelfth each when `Σ N` is not above 0, so no division by 0 and no
/// NaN from it). Diurnal range, wet-day fraction and wind are unchanged.
/// Exact arithmetic (multiply, subtract, divide), `f64` inside, each value
/// cast to `f32` last.
///
/// This is the CANONICAL ground read (`monthly_at_ground` uses it):
/// `game::MonthlyNormals::lowered_by` lowers in `f32` and does not bit-match
/// it, so code that must match the forge or Phase 5 (Phase 6's weather
/// included) uses this formula — `(f64::from(sea) − lapse · ground) as f32`.
///
/// For a forge-side caller with no `ForgedWorld` (Phase 5's classifier on
/// `WorldArtifact` cells): fine cell `i` at its centre `(cx_m, cz_m)` in MAP
/// metres is `ground_normals(a.climate.normals_at(cx_m, cz_m),
/// f64::from(a.height.get(i)).max(0.0), f64::from(a.precip_mm.get(i)), lapse)`
/// — bit-identical to [`ForgedWorld::monthly_at_ground_map`]`(cx_m, cz_m)`,
/// because a bilinear read at a cell centre returns the cell. (Through engine
/// metres the identity holds only when the spawn offset round-trips exactly,
/// which nothing guarantees — hence the map-metre function.)
pub fn ground_normals(
    sea: game::MonthlyNormals,
    ground_m: f64,
    annual_precip_mm: f64,
    lapse_c_per_m: f64,
) -> game::MonthlyNormals {
    let mut n = sea;
    let lowered = lapse_c_per_m * ground_m;
    let mut total = 0.0f64;
    for p in sea.precip_mm {
        total += f64::from(p);
    }
    for m in 0..game::CLIMATE_MONTHS {
        n.temp_c[m] = (f64::from(sea.temp_c[m]) - lowered) as f32;
        n.precip_mm[m] = if total > 0.0 {
            (annual_precip_mm * (f64::from(sea.precip_mm[m]) / total)) as f32
        } else {
            (annual_precip_mm / game::CLIMATE_MONTHS as f64) as f32
        };
    }
    n
}

impl ForgedWorld {
    /// Cooling per metre of height the world was forged with (deg C / m;
    /// `WorldGlobals::knobs().lapse_c_per_km / 1000`).
    pub fn lapse_per_m(&self) -> f64 {
        self.lapse_c_per_m
    }

    /// The annual rasters at engine metres, bilinear: (mean annual
    /// temperature AT THE GROUND in deg C, annual precipitation in mm/yr) —
    /// what `column_climate` and the habitat read. On a tiled world it can
    /// load a tile (as every fine-raster read).
    pub fn annual_climate(&self, x_m: f64, z_m: f64) -> (f64, f64) {
        let (ax, az) = self.to_artifact(x_m, z_m);
        (self.bilinear_q(QRasterId::TemperatureC, ax, az), self.bilinear_q(QRasterId::PrecipMm, ax, az))
    }

    /// The climate grid's normals at engine metres: SEA-LEVEL temperature,
    /// the grid's own (coarse) precipitation. Bilinear between climate cells
    /// (`MonthlyClimate::normals_at`); x folds on a world that wraps (the
    /// samplers' own fold), clamped north and south. Reads no tile: the
    /// family is always resident.
    ///
    /// For Phase 6: the API takes no date — interpolating between months is
    /// the caller's; so are a per-column lapse above the bilinear ground
    /// [`Self::monthly_at_ground`] uses, and the engine-frame offset a
    /// `game`-side reader of the container needs.
    pub fn monthly_normals(&self, x_m: f64, z_m: f64) -> game::MonthlyNormals {
        let (ax, az) = self.to_artifact(x_m, z_m);
        self.monthly_climate().normals_at(ax, az)
    }

    /// The normals AT THE GROUND at engine metres ([`ground_normals`] of
    /// [`Self::monthly_normals`], the ground and annual precipitation read
    /// from the fine rasters). Reads two fine rasters: on a tiled world it
    /// can load (or, on a store error, fail on) a tile unless the caller
    /// holds a prefetch — only `monthly_normals` is load-free.
    pub fn monthly_at_ground(&self, x_m: f64, z_m: f64) -> game::MonthlyNormals {
        let (ax, az) = self.to_artifact(x_m, z_m);
        self.monthly_at_ground_map(ax, az)
    }

    /// [`Self::monthly_at_ground`] at MAP metres (x east from the west edge,
    /// z south from the north edge): the function it and a forge-side caller
    /// share. `ground` is the bilinear height with each CELL floored at 0
    /// before interpolating (`snow_line_m`'s read: a sea-floor neighbour's
    /// depth never warms a coast), the lapse the world's own.
    pub fn monthly_at_ground_map(&self, ax_m: f64, az_m: f64) -> game::MonthlyNormals {
        let ground = self.bilinear_cells(ax_m, az_m, |i| f64::from(self.rasters.q(QRasterId::Height, i)).max(0.0));
        let annual = self.bilinear_q(QRasterId::PrecipMm, ax_m, az_m);
        ground_normals(self.monthly_climate().normals_at(ax_m, az_m), ground, annual, self.lapse_c_per_m)
    }
}

// ---- end Phase 1b ----

/// Where an artifact point lies relative to the map (`ForgedWorld::place`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    Inside,
    /// Past land edges only: the point clamped onto the border (artifact m).
    /// No world that loads has one (the Planet's edges are sea and wrap).
    Land(f64, f64),
    /// Past a sea edge.
    Sea,
}

/// One window of a forged world: latticed ground + the rivers crossing it.
pub struct ForgedPatch<'a> {
    world: &'a ForgedWorld,
    grid: ProfileGrid,
    /// Window origin in artifact meters (the window's own frame: see `shift`).
    origin: (f64, f64),
    /// Engine metres between the window and the image of it that the
    /// artifact frame starts from (a multiple of the width; 0 unless the
    /// world wraps): a point's artifact x in the window's frame is
    /// `to_artifact_raw(x - shift)`, past the east edge where the window is.
    shift: f64,
    /// Every river segment that can touch the window, by ascending id.
    segments: Vec<Segment>,
    /// River segment candidates per PATCH_CELL_M cell (indices into
    /// `segments`, ascending); empty = no rivers here.
    cells: Vec<Vec<u32>>,
    cells_per_side: usize,
    /// Whether any lake cell lies in or next to the window.
    lakes: bool,
}

impl crate::Patch for ForgedPatch<'_> {
    fn geo(&self, x: f64, z: f64) -> i16 {
        self.world.geo_offset_blocks(x, z)
    }

    fn strata(&self, mats: &Materials) -> Vec<(i16, BlockId)> {
        self.world.strata_blocks(mats)
    }

    fn climate(&self, x: f64, z: f64) -> Option<ColumnClimate> {
        Some(self.world.column_climate(x, z))
    }

    fn sample(&self, x: f64, z: f64) -> (ColumnProfile, i32, bool) {
        let prof = self.grid.profile_at(x, z);
        let (ax, az) = self.world.to_artifact_raw(x - self.shift, z);
        if self.world.place(ax, az) != Place::Inside {
            // No rivers or lakes off the map: they stop at its edge.
            return (ColumnProfile::from_surface(prof.surface_y()), SEA_TOP_BLOCK, false);
        }
        let candidates: &[u32] = if self.cells.is_empty() {
            &[]
        } else {
            let cell = |v: f64, origin: f64| {
                (((v - origin) / PATCH_CELL_M).floor().max(0.0) as usize).min(self.cells_per_side - 1)
            };
            &self.cells[cell(az, self.origin.1) * self.cells_per_side + cell(ax, self.origin.0)]
        };
        let segments = candidates.iter().map(|&k| self.segments[k as usize]);
        let (g, water, lake) = self.world.refine(ax, az, prof.surface_y() - SEA_LEVEL, segments, self.lakes);
        // `refine` snapped the surface to a whole block: the topmost water
        // block is the one just under it.
        let water_top = water.map_or(SEA_TOP_BLOCK, |w| ((SEA_LEVEL + w) as i32 - 1).max(SEA_TOP_BLOCK));
        (ColumnProfile::from_surface(SEA_LEVEL + g), water_top, lake)
    }
}

/// `VOXEL_WORLD_FILE` value that selects the procedural noise world.
pub const PROCEDURAL_KEYWORD: &str = "procedural";

/// Relative world paths resolve from the repo root (like the block manifest),
/// so every process reads the same file whatever its working directory.
pub fn from_repo_root(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
    }
}

/// The world file a `VOXEL_WORLD_FILE` value names for [`active`]: `None` when
/// unset, empty or [`PROCEDURAL_KEYWORD`], else the path from the repo root.
/// (The server instead treats unset/empty as its default world file and
/// installs its choice before anything calls `active`.)
pub fn world_file_path(env_value: Option<&str>) -> Option<PathBuf> {
    match env_value {
        None | Some("") | Some(PROCEDURAL_KEYWORD) => None,
        Some(path) => Some(from_repo_root(Path::new(path))),
    }
}

static ACTIVE: OnceLock<Option<ForgedWorld>> = OnceLock::new();

/// Choose the process's world before anything asks for it: `Some` serves that
/// forged world, `None` pins procedural terrain (whatever `VOXEL_WORLD_FILE`
/// says). Panics if [`active`] was already initialised — two worlds in one
/// process would be a save mixing both.
pub fn install(world: Option<ForgedWorld>) {
    if ACTIVE.set(world).is_err() {
        panic!("forged::install called after the active world was already chosen");
    }
}

/// The world in use, loaded once: whatever [`install`] set, else the file named
/// by `VOXEL_WORLD_FILE` (tools and tests; see [`world_file_path`] — relative
/// paths resolve from the repo root, `procedural` means none). `None` when
/// neither applies. A
/// set-but-unreadable file is fatal: silently falling back to procedural
/// terrain would mix two worlds in one save.
pub fn active() -> Option<&'static ForgedWorld> {
    ACTIVE.get_or_init(load_env_world).as_ref()
}

/// The world in use if it has been chosen already ([`install`] or a first
/// [`active`]), else `None` — never loads one (observability readers).
pub fn chosen() -> Option<&'static ForgedWorld> {
    ACTIVE.get().and_then(Option::as_ref)
}

/// The browser has no world files: procedural unless a world was installed.
#[cfg(target_arch = "wasm32")]
fn load_env_world() -> Option<ForgedWorld> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn load_env_world() -> Option<ForgedWorld> {
    let file = world_file_path(std::env::var(WORLD_FILE_ENV).ok().as_deref())?;
    let path = file.display();
    match ForgedWorld::load(&file) {
        Ok(world) => {
            let g = world.globals();
            let (rivers, lakes) = world.artifact().map_or((0, 0), |a| (a.rivers.len(), a.lakes.len()));
            log::info!(
                "worldgen: serving forged world {path} (seed {:#X}, {} x {} m, {} m cells, {rivers} river reaches, {lakes} lakes)",
                g.seed, g.width_m, g.height_m, g.cell_m
            );
            Some(world)
        }
        Err(e) => panic!("{WORLD_FILE_ENV}={path}: {e}"),
    }
}
