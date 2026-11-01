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

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use game::BlockId;

use crate::forge::geology::rock_index;
pub use crate::forge::world::EdgeKind;
use crate::material::Materials;
use crate::noise::OctaveNoise;
use crate::pipeline::{ColumnProfile, ProfileGrid, SEA_LEVEL, SEA_TOP_BLOCK};

/// Environment variable naming the world file to serve.
pub const WORLD_FILE_ENV: &str = "VOXEL_WORLD_FILE";
/// v3: quantised `u16` rasters (`QRaster`), sparse lakes, row-delta + zstd,
/// coast distance recomputed on load. v4: `edges` (what lies past each side).
/// v5: the latitude climate — `temperature_c` and `precip_mm` replace the
/// relative `rain`.
pub const ARTIFACT_VERSION: u32 = 5;
/// zstd level of the world file body (offline writer; reading cost is level-independent).
#[cfg(not(target_arch = "wasm32"))]
const ZSTD_LEVEL: i32 = 19;
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
/// ocean. Past a LAND edge the edge's own ground continues instead
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
const BUCKET_M: f64 = 64.0;
/// Inside one patch, candidates are narrowed again to cells of this size (m):
/// a region samples ~1 M points (every voxel column of every surface block),
/// and checking all ~20 segments of a river region at each cost 4x the rest
/// of generation.
const PATCH_CELL_M: f64 = 8.0;

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

/// The stored world. Rasters are `n * n`, row-major (z rows, x columns), one
/// value per `cell_m` cell CENTER; heights are meters relative to sea level.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorldArtifact {
    pub version: u32,
    pub seed: u64,
    pub size_m: f64,
    pub cell_m: f64,
    pub n: u32,
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
}

/// The format version in a world file's 8-byte header, without loading the
/// body (the main menu lists worlds with this).
pub fn read_artifact_version(path: &Path) -> Result<u32, ArtifactError> {
    use std::io::Read;
    let mut header = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(ArtifactError::Io)?;
    if &header[..4] != MAGIC {
        return Err(ArtifactError::Format("missing VXWF header".into()));
    }
    Ok(u32::from_le_bytes([header[4], header[5], header[6], header[7]]))
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
    fn qrasters_mut(&mut self) -> [&mut QRaster; 5] {
        [&mut self.height, &mut self.uplifted, &mut self.bedding, &mut self.temperature_c, &mut self.precip_mm]
    }

    fn qrasters(&self) -> [&QRaster; 5] {
        [&self.height, &self.uplifted, &self.bedding, &self.temperature_c, &self.precip_mm]
    }

    /// File bytes: `VXWF`, the version (u32 LE), then zstd over bincode with
    /// every raster row-delta encoded.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut wire = self.clone();
        let n = wire.n as usize;
        for raster in wire.qrasters_mut() {
            delta_rows(&mut raster.data, n);
        }
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
        let body = zstd::stream::decode_all(&bytes[8..])
            .map_err(|e| ArtifactError::Format(format!("corrupt body: {e}")))?;
        let mut artifact: WorldArtifact =
            bincode::deserialize(&body).map_err(|e| ArtifactError::Format(format!("corrupt body: {e}")))?;
        artifact.validate()?;
        let n = artifact.n as usize;
        for raster in artifact.qrasters_mut() {
            undelta_rows(&mut raster.data, n);
        }
        Ok(artifact)
    }

    /// The invariants every reader relies on (`from_bytes` and
    /// `ForgedWorld::new` both check them): rasters of `n * n`, known rocks,
    /// and a non-empty rock column sorted by descending top (the strata
    /// lookups binary-search it).
    pub fn validate(&self) -> Result<(), ArtifactError> {
        let artifact = self;
        let cells = artifact.n as usize * artifact.n as usize;
        if artifact.qrasters().iter().any(|raster| raster.len() != cells) {
            return Err(ArtifactError::Format("raster size does not match n".into()));
        }
        let run_ok = |&(start, len): &(u32, u32)| start as usize + len as usize <= cells;
        if artifact.lakes.iter().any(|lake| !lake.cells.iter().all(run_ok) || lake.level.is_nan()) {
            return Err(ArtifactError::Format("lake cells outside the raster".into()));
        }
        // Every rock must be one this build has a block for: an unknown name
        // would otherwise quietly become some other stone.
        if let Some(name) = artifact.rock_names.iter().find(|name| rock_index(name).is_none()) {
            return Err(ArtifactError::Format(format!("unknown rock {name}")));
        }
        if artifact.column.is_empty() {
            return Err(ArtifactError::Format("empty rock column".into()));
        }
        let known = artifact.rock_names.len();
        if let Some(&(_, rock)) = artifact.column.iter().find(|&&(_, rock)| rock as usize >= known) {
            return Err(ArtifactError::Format(format!("rock index {rock} out of range")));
        }
        // `!(a >= b)` also rejects a NaN top.
        if artifact.column.windows(2).any(|w| !(w[0].0 >= w[1].0)) {
            return Err(ArtifactError::Format("rock column not sorted by descending top".into()));
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

/// The temp file [`WorldArtifact::save`] writes before renaming it to `path`:
/// same directory (so the rename stays on one filesystem), `.<name>.tmp`.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map_or_else(|| "world".into(), |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.tmp"))
}

/// One straight piece of a river centerline, in artifact meters.
struct Segment {
    a: (f64, f64),
    b: (f64, f64),
    water: (f64, f64),
    half_width: f64,
    depth: f64,
}

/// A loaded world plus what sampling needs: the river index and detail noise.
pub struct ForgedWorld {
    artifact: WorldArtifact,
    /// crc32 of the file bytes: part of the worldgen disk-cache key.
    content_crc: u32,
    segments: Vec<Segment>,
    /// Segment indices per BUCKET_M bucket (row-major, `buckets_per_side`^2).
    buckets: Vec<Vec<u32>>,
    buckets_per_side: usize,
    detail: OctaveNoise,
    /// Height decoded to meters (the sampler's hot raster).
    height: Vec<f32>,
    /// Distance (m) from the nearest sea cell (height <= 0), from `height`
    /// at load (`habitat::coast_distance_m`).
    coast_dist_m: Vec<f32>,
    /// Lake water per cell as sorted, non-overlapping `(start, end, level)`
    /// runs of raster indices (`end` exclusive), from `artifact.lakes`.
    lake_runs: Vec<(u32, u32, f32)>,
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
        if let Err(e) = artifact.validate() {
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
        let artifact = WorldArtifact::from_bytes(&bytes)?;
        Ok(Self::with_crc(artifact, crc32fast::hash(&bytes)))
    }

    fn with_crc(artifact: WorldArtifact, content_crc: u32) -> ForgedWorld {
        let buckets_per_side = (artifact.size_m / BUCKET_M).ceil().max(1.0) as usize;
        let mut segments = Vec::new();
        let mut buckets = vec![Vec::new(); buckets_per_side * buckets_per_side];
        for reach in &artifact.rivers {
            let half_width = reach.width_m as f64 * 0.5;
            let depth = (CHANNEL_DEPTH_PER_SQRT_M * (reach.width_m as f64).sqrt())
                .clamp(CHANNEL_DEPTH_M.0, CHANNEL_DEPTH_M.1);
            for pair in reach.points.windows(2) {
                let (p, q) = (pair[0], pair[1]);
                if p.in_lake && q.in_lake {
                    continue; // the lake's own water covers this stretch
                }
                let seg = Segment {
                    a: (p.x as f64, p.z as f64),
                    b: (q.x as f64, q.z as f64),
                    water: (p.water as f64, q.water as f64),
                    half_width,
                    depth,
                };
                let reach_m = half_width + LEVEE_WIDTH_M + BANK_REACH_M;
                let bucket = |v: f64| ((v / BUCKET_M).floor().max(0.0) as usize).min(buckets_per_side - 1);
                let (x0, x1) = (bucket(seg.a.0.min(seg.b.0) - reach_m), bucket(seg.a.0.max(seg.b.0) + reach_m));
                let (z0, z1) = (bucket(seg.a.1.min(seg.b.1) - reach_m), bucket(seg.a.1.max(seg.b.1) + reach_m));
                for bz in z0..=z1 {
                    for bx in x0..=x1 {
                        buckets[bz * buckets_per_side + bx].push(segments.len() as u32);
                    }
                }
                segments.push(seg);
            }
        }
        let detail = OctaveNoise::new(artifact.seed ^ 0xDE7A_11, DETAIL_FIRST_OCTAVE, vec![1.0, 0.6]);
        let height = artifact.height.to_f32();
        let coast_dist_m = crate::habitat::coast_distance_m(&height, artifact.n as usize, artifact.cell_m);
        let mut lake_runs: Vec<(u32, u32, f32)> = artifact
            .lakes
            .iter()
            .flat_map(|lake| lake.cells.iter().map(move |&(start, len)| (start, start + len, lake.level)))
            .collect();
        lake_runs.sort_by_key(|run| run.0);
        ForgedWorld { artifact, content_crc, segments, buckets, buckets_per_side, detail, height, coast_dist_m, lake_runs }
    }

    pub fn artifact(&self) -> &WorldArtifact {
        &self.artifact
    }

    pub fn content_crc(&self) -> u32 {
        self.content_crc
    }

    /// Engine world meters -> artifact meters.
    fn to_artifact(&self, x: f64, z: f64) -> (f64, f64) {
        (x - ENGINE_SPAWN_M + self.artifact.spawn[0], z - ENGINE_SPAWN_M + self.artifact.spawn[1])
    }

    fn cell(&self, ix: i64, iz: i64) -> f64 {
        Self::raster_cell(&self.height, self.artifact.n, ix, iz)
    }

    /// One cell of an `n * n` raster, clamped to the edge.
    fn raster_cell<R: RasterRead + ?Sized>(raster: &R, n: u32, ix: i64, iz: i64) -> f64 {
        let n = n as i64;
        let (ix, iz) = (ix.clamp(0, n - 1), iz.clamp(0, n - 1));
        raster.value((iz * n + ix) as usize) as f64
    }

    /// Distance (m) to the sea per raster cell.
    pub(crate) fn coast_dist_m(&self) -> &[f32] {
        &self.coast_dist_m
    }

    /// Water level of raster cell `i` if it is lake.
    pub(crate) fn lake_level_of(&self, i: usize) -> Option<f32> {
        let i = i as u32;
        let k = self.lake_runs.partition_point(|run| run.0 <= i);
        let &(_, end, level) = self.lake_runs.get(k.checked_sub(1)?)?;
        (i < end).then_some(level)
    }

    /// Bilinear interpolation of `uplifted + bedding` between cell centers
    /// (artifact meters), clamped to the edge outside the artifact.
    fn geo_offset_m(&self, ax: f64, az: f64) -> f64 {
        let a = &self.artifact;
        let at = |ix: i64, iz: i64| {
            Self::raster_cell(&a.uplifted, a.n, ix, iz) + Self::raster_cell(&a.bedding, a.n, ix, iz)
        };
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

    /// Bilinear interpolation of one of the artifact's `n * n` rasters at
    /// artifact meters (same lattice as `geo_offset_m`, edge-clamped).
    pub(crate) fn bilinear<R: RasterRead + ?Sized>(&self, raster: &R, ax: f64, az: f64) -> f64 {
        let a = &self.artifact;
        let at = |ix: i64, iz: i64| Self::raster_cell(raster, a.n, ix, iz);
        let (u, v) = (ax / a.cell_m - 0.5, az / a.cell_m - 0.5);
        let (iu, iv) = (u.floor(), v.floor());
        let (tu, tv) = (u - iu, v - iv);
        let (iu, iv) = (iu as i64, iv as i64);
        let row = |iz: i64| at(iu, iz) + tu * (at(iu + 1, iz) - at(iu, iz));
        row(iv) + tv * (row(iv + 1) - row(iv))
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
        let a = &self.artifact;
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
        let a = &self.artifact;
        let min = [ENGINE_SPAWN_M - a.spawn[0], ENGINE_SPAWN_M - a.spawn[1]];
        (min, [min[0] + a.size_m, min[1] + a.size_m])
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
        let size = self.artifact.size_m;
        if ax < 0.0 || az < 0.0 || ax >= size || az >= size {
            return None;
        }
        let s = self.bicubic(ax, az) - self.geo_offset_m(ax, az);
        let column = &self.artifact.column;
        column.iter().rev().find(|&&(top, _)| f64::from(top) > s).or(column.first()).map(|&(_, rock)| rock)
    }

    /// Where artifact point `(ax, az)` lies: inside, past a land edge (the
    /// point clamped onto the map's border, whose ground continues outward),
    /// or past a sea edge. A point past two edges (a corner) is sea if
    /// either is.
    fn place(&self, ax: f64, az: f64) -> Place {
        let size = self.artifact.size_m;
        // `edges` order: north (z < 0), east (x >= size), south (z >= size), west (x < 0).
        let past = [az < 0.0, ax >= size, az >= size, ax < 0.0];
        if !past.contains(&true) {
            return Place::Inside;
        }
        if (0..4).any(|e| past[e] && self.artifact.edges[e] == EdgeKind::Sea) {
            return Place::Sea;
        }
        Place::Land(ax.clamp(0.0, size), az.clamp(0.0, size))
    }

    /// Height of raster cell `(ix, iz)` as the sampler extends the map: the
    /// cell itself inside, the nearest border cell past a land edge, and
    /// [`OUTSIDE_HEIGHT_M`] past a sea edge (`place`'s rules, by cell).
    pub fn extended_cell_height(&self, ix: i64, iz: i64) -> f64 {
        let n = i64::from(self.artifact.n);
        let past = [iz < 0, ix >= n, iz >= n, ix < 0];
        if (0..4).any(|e| past[e] && self.artifact.edges[e] == EdgeKind::Sea) {
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
        let (u, v) = (ax / self.artifact.cell_m - 0.5, az / self.artifact.cell_m - 0.5);
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
    /// it is ocean at [`OUTSIDE_HEIGHT_M`]; past a land edge, the ground of
    /// the border point it clamps to (constant outward: no cliff, and the
    /// far view shows land continuing).
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
        let e = self.artifact.cell_m * 0.5;
        let (dx, dz) = (
            (self.bicubic(ax + e, az) - self.bicubic(ax - e, az)) / (2.0 * e),
            (self.bicubic(ax, az + e) - self.bicubic(ax, az - e)) / (2.0 * e),
        );
        let slope = (dx * dx + dz * dz).sqrt();
        let amplitude = (DETAIL_BASE_M + DETAIL_PER_SLOPE_M * slope).min(DETAIL_MAX_M);
        g + fade * amplitude * 2.0 * self.detail.sample2(ax, az)
    }

    fn lake_level_at(&self, ix: i64, iz: i64) -> Option<f64> {
        let n = self.artifact.n as i64;
        if ix < 0 || iz < 0 || ix >= n || iz >= n {
            return None;
        }
        self.lake_level_of((iz * n + ix) as usize).map(f64::from)
    }

    /// Water features applied to an already-interpolated ground height `g`
    /// (meters relative to sea; artifact coordinates). `segments` are the
    /// candidates near this point.
    fn refine(&self, ax: f64, az: f64, mut g: f64, segments: &[u32], lakes: bool) -> (f64, Option<f64>) {
        // Water surfaces sit on whole blocks (sea level is one), so every
        // level is snapped BEFORE banks are measured against it: a bank built
        // to the raw level could end up under a surface that rounded up.
        let snap = |level: f64| (level + 0.5).floor();
        let mut water = None;
        // Lakes: water where the cell is lake and the ground lies below its
        // level; a berm on the dry cells around it, so the basin holds water
        // even where interpolation left the rim a little low.
        let (ix, iz) = ((ax / self.artifact.cell_m).floor() as i64, (az / self.artifact.cell_m).floor() as i64);
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

        // Rivers: nearest centerline decides. Inside the channel the bed is
        // cut below the water surface; a levee ring outside it stands above
        // the water; further out the valley side is cut back to a bank slope.
        let mut nearest: Option<(f64, f64, &Segment)> = None; // (distance, water, segment)
        for &s in segments {
            let seg = &self.segments[s as usize];
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
        (g, water.filter(|&w| w > 0.0))
    }

    fn bucket_segments(&self, ax: f64, az: f64) -> &[u32] {
        if ax < 0.0 || az < 0.0 || ax >= self.artifact.size_m || az >= self.artifact.size_m {
            return &[];
        }
        let (bx, bz) = ((ax / BUCKET_M) as usize, (az / BUCKET_M) as usize);
        &self.buckets[bz.min(self.buckets_per_side - 1) * self.buckets_per_side + bx.min(self.buckets_per_side - 1)]
    }

    /// Exact (un-latticed) sample at one engine-world point: map renderers, tests.
    /// Outside the artifact there are no rivers or lakes: ocean past a sea
    /// edge, the border's ground continued past a land edge.
    pub fn sample(&self, x: f64, z: f64) -> PointSample {
        let (ax, az) = self.to_artifact(x, z);
        if self.place(ax, az) != Place::Inside {
            return PointSample { ground_y: SEA_LEVEL + self.ground(ax, az), water_y: None };
        }
        let (g, water) = self.refine(ax, az, self.ground(ax, az), self.bucket_segments(ax, az), true);
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
        // Every river segment that can touch the window.
        let (a0, a1) = (self.to_artifact(origin_x, origin_z), self.to_artifact(origin_x + span_m, origin_z + span_m));
        let mut segments: Vec<u32> = Vec::new();
        if a1.0 >= 0.0 && a1.1 >= 0.0 && a0.0 < self.artifact.size_m && a0.1 < self.artifact.size_m {
            let bucket = |v: f64| ((v / BUCKET_M).floor().max(0.0) as usize).min(self.buckets_per_side - 1);
            for bz in bucket(a0.1)..=bucket(a1.1) {
                for bx in bucket(a0.0)..=bucket(a1.0) {
                    segments.extend_from_slice(&self.buckets[bz * self.buckets_per_side + bx]);
                }
            }
            segments.sort_unstable();
            segments.dedup();
        }
        // Narrow to PATCH_CELL_M cells (artifact coordinates, window-local).
        let cells_per_side = (span_m / PATCH_CELL_M).ceil() as usize + 1;
        let mut cells = vec![Vec::new(); if segments.is_empty() { 0 } else { cells_per_side * cells_per_side }];
        if !segments.is_empty() {
            let cell = |v: f64, origin: f64| {
                (((v - origin) / PATCH_CELL_M).floor().max(0.0) as usize).min(cells_per_side - 1)
            };
            for &s in &segments {
                let seg = &self.segments[s as usize];
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
        let cell_m = self.artifact.cell_m;
        let n = self.artifact.n as i64;
        let range = |lo: f64, hi: f64| {
            (((lo / cell_m).floor() as i64 - 1).clamp(0, n - 1), ((hi / cell_m).floor() as i64 + 1).clamp(0, n - 1))
        };
        let ((x0, x1), (z0, z1)) = (range(a0.0, a1.0), range(a0.1, a1.1));
        let inside = a1.0 >= 0.0 && a1.1 >= 0.0 && a0.0 < self.artifact.size_m && a0.1 < self.artifact.size_m;
        let lakes = inside
            && (z0..=z1).any(|iz| (x0..=x1).any(|ix| self.lake_level_of((iz * n + ix) as usize).is_some()));
        ForgedPatch { world: self, grid, origin: a0, cells, cells_per_side, lakes }
    }
}

/// Where an artifact point lies relative to the map (`ForgedWorld::place`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    Inside,
    /// Past land edges only: the point clamped onto the border (artifact m).
    Land(f64, f64),
    /// Past a sea edge.
    Sea,
}

/// One window of a forged world: latticed ground + the rivers crossing it.
pub struct ForgedPatch<'a> {
    world: &'a ForgedWorld,
    grid: ProfileGrid,
    /// Window origin in artifact meters.
    origin: (f64, f64),
    /// River segment candidates per PATCH_CELL_M cell; empty = no rivers here.
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

    fn sample(&self, x: f64, z: f64) -> (ColumnProfile, i32) {
        let prof = self.grid.profile_at(x, z);
        let (ax, az) = self.world.to_artifact(x, z);
        if self.world.place(ax, az) != Place::Inside {
            // No rivers or lakes off the map: they stop at its edge.
            return (ColumnProfile::from_surface(prof.surface_y()), SEA_TOP_BLOCK);
        }
        let segments: &[u32] = if self.cells.is_empty() {
            &[]
        } else {
            let cell = |v: f64, origin: f64| {
                (((v - origin) / PATCH_CELL_M).floor().max(0.0) as usize).min(self.cells_per_side - 1)
            };
            &self.cells[cell(az, self.origin.1) * self.cells_per_side + cell(ax, self.origin.0)]
        };
        let (g, water) = self.world.refine(ax, az, prof.surface_y() - SEA_LEVEL, segments, self.lakes);
        // `refine` snapped the surface to a whole block: the topmost water
        // block is the one just under it.
        let water_top = water.map_or(SEA_TOP_BLOCK, |w| ((SEA_LEVEL + w) as i32 - 1).max(SEA_TOP_BLOCK));
        (ColumnProfile::from_surface(SEA_LEVEL + g), water_top)
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
            let a = world.artifact();
            log::info!(
                "worldgen: serving forged world {path} (seed {:#X}, {} m, {} m cells, {} river reaches, {} lakes)",
                a.seed, a.size_m, a.cell_m, a.rivers.len(), a.lakes.len()
            );
            Some(world)
        }
        Err(e) => panic!("{WORLD_FILE_ENV}={path}: {e}"),
    }
}
