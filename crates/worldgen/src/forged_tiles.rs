//! Tiled forged worlds (docs/superpowers/plans/2026-11-07-forged-world-in-db.md):
//! a forged world's per-cell data cut into square tiles of [`TILE_CELLS`]
//! cells, plus the small global tables ([`ForgedMeta`]), so a host loads only
//! the tiles its sampling touches.
//!
//! - Tiles hold exactly their cells — no halo. A read routes by GLOBAL raster
//!   index: clamped exactly as the whole world clamps, then
//!   `tile = (ix / tile_cells, iz / tile_cells)`, local = the remainder, so a
//!   tiled world returns bit-for-bit what the whole world returns.
//! - Coast distance cannot be recomputed per tile (the chamfer is global with
//!   no range cap), so each tile stores its cells' exact `f32` distances,
//!   computed over the whole map when the world is split.
//! - River segments are indexed in 64 m buckets (`forged::BUCKET_M`); bucket
//!   `b` lives in the tile containing its origin (`tile_cells * cell_m` must
//!   be a multiple of the bucket), its list exactly the whole world's, and
//!   the tile carries every segment its buckets name, with its GLOBAL id, so
//!   the nearest-segment tie order (ascending id) is the whole world's.
//! - Lake water is per-cell runs of global indices, clipped to the tile.
//!
//! IO-free: tiles come through a [`TileLoader`] (the world store, or
//! [`MemTiles`]). The codecs need zstd, which is native only — on wasm32 the
//! decoders return an error.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::forged::{ArtifactError, ByteRasterId, ForgedWorld, QRasterId, Rasters, Segment, WorldGlobals, BUCKET_M};

/// Tile edge in raster cells (Decision 1): 4,096 m at 16 m cells, one
/// super-region of 16 x 16 256 m regions.
pub const TILE_CELLS: u32 = 256;
/// Version of the tiled format (meta + tiles), independent of the forge
/// file's `ARTIFACT_VERSION`. A mismatch is refused: re-tile or re-forge.
pub const TILED_FORMAT_VERSION: u32 = 1;
/// Decoded bytes the tile cache keeps before it evicts the least recently
/// used tile (Decision 4); `VOXEL_TILE_CACHE_MB` overrides it
/// ([`tile_cache_budget_from_env`]).
pub const TILE_CACHE_BYTES: usize = 256 << 20;
/// Env var: the tile cache budget in MiB (unset or unparsable = [`TILE_CACHE_BYTES`]).
pub const TILE_CACHE_MB_ENV: &str = "VOXEL_TILE_CACHE_MB";
/// Attempts at loading one tile before the read fails.
pub const TILE_LOAD_RETRIES: u32 = 3;
/// How far (raster cells, per axis, from the cell holding a sampled point)
/// any sampling read can land: [`ForgedWorld::prefetch_rect`] grows its rect
/// by this many cells, and a world whose [`read_radius_cells`] exceeds it is
/// refused at load ([`ForgedMeta::validate`]). 4 covers 8 m cells and up.
pub const MAX_READ_RADIUS_CELLS: u32 = 4;

/// The cross-cell reach of sampling at `cell_m` (Decision 1), in raster cells
/// from the cell holding the sampled point. With `c = cell_m`, a read's cells
/// cover these metre spans around the point:
/// - bicubic height: `±1.5 c`; plus its detail-slope gradient at `±c/2`: `±2 c`;
/// - the habitat slope probe (`habitat::SLOPE_PROBE_M`) of that ground:
///   `±(SLOPE_PROBE_M + 2 c)`;
/// - the biome dither (`forged::BIOME_DITHER_M`): nearest cell at the
///   displaced point, plus the salt-flat bilinear there: `±(BIOME_DITHER_M + c/2)`;
/// - every other bilinear: `±c/2`; the lake berm and lake window: `±1` cell.
///
/// so `ceil(max(SLOPE_PROBE_M + 2 c, BIOME_DITHER_M + c/2) / c)`, at least 1
/// (3 cells at 16 m, 4 at 8 m). River segments are read by 64 m bucket, which
/// `prefetch_rect` covers separately.
pub fn read_radius_cells(cell_m: f64) -> u32 {
    let reach_m =
        (crate::habitat::SLOPE_PROBE_M + 2.0 * cell_m).max(crate::forged::BIOME_DITHER_M + 0.5 * cell_m);
    ((reach_m / cell_m).ceil() as u32).max(1)
}

/// The tile cache budget for worlds the host loads: `VOXEL_TILE_CACHE_MB`
/// MiB, else [`TILE_CACHE_BYTES`]. The one place the env var is read; tests
/// pass explicit budgets (`ForgedWorld::from_tiles_with_budget`).
pub fn tile_cache_budget_from_env() -> usize {
    match std::env::var(TILE_CACHE_MB_ENV) {
        Err(_) => TILE_CACHE_BYTES,
        Ok(v) => match v.trim().parse::<usize>() {
            Ok(mb) if mb > 0 => mb.saturating_mul(1 << 20),
            _ => {
                log::warn!("[forged-tiles] {TILE_CACHE_MB_ENV}={v:?} is not a positive number of MiB; using the default");
                TILE_CACHE_BYTES
            }
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
const META_MAGIC: &[u8; 4] = b"VXTM";
#[cfg(not(target_arch = "wasm32"))]
const TILE_MAGIC: &[u8; 4] = b"VXTT";

/// A tile's position in the tile grid (`tx` columns, `tz` rows). Ordered row
/// by row (`tz`, then `tx`), the order the tiles of a world are listed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileCoords {
    pub tx: u32,
    pub tz: u32,
}

impl Ord for TileCoords {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.tz, self.tx).cmp(&(other.tz, other.tx))
    }
}

impl PartialOrd for TileCoords {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One tile: `w x h` cells (edge tiles are smaller), row-major, from global
/// cell `(origin.tx * tile_cells, origin.tz * tile_cells)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tile {
    pub origin: TileCoords,
    pub w: u32,
    pub h: u32,
    /// The quantised rasters' raw steps, in `QRasterId::ALL` order (value =
    /// `min + step * data`, with `WorldGlobals::q_scale`).
    pub q: [Vec<u16>; 5],
    /// The `u8` rasters, in `ByteRasterId::ALL` order.
    pub bytes: [Vec<u8>; 5],
    pub region: Vec<u16>,
    /// Exact distance to the sea (m), from the whole-map chamfer.
    pub coast: Vec<f32>,
    /// Lake water as sorted, non-overlapping `(start, end, level)` runs of
    /// GLOBAL raster indices (`end` exclusive), clipped to the tile's rows.
    pub lake_runs: Vec<(u32, u32, f32)>,
    /// Every segment the tile's buckets name, by ascending global id.
    pub segments: Vec<(u32, Segment)>,
    /// The tile's 64 m river buckets, row-major over the tile's bucket grid
    /// (`TileGeometry::bucket_span`), each list the whole world's (global
    /// segment ids, ascending).
    pub buckets: Vec<Vec<u32>>,
}

impl Tile {
    /// Heap bytes the decoded tile holds (what the cache budget counts).
    pub fn decoded_bytes(&self) -> usize {
        let cells = self.coast.len();
        std::mem::size_of::<Tile>()
            + self.q.iter().map(|r| r.len() * 2).sum::<usize>()
            + self.bytes.iter().map(Vec::len).sum::<usize>()
            + self.region.len() * 2
            + cells * 4
            + self.lake_runs.len() * std::mem::size_of::<(u32, u32, f32)>()
            + self.segments.len() * std::mem::size_of::<(u32, Segment)>()
            + self.buckets.iter().map(|b| std::mem::size_of::<Vec<u32>>() + b.len() * 4).sum::<usize>()
    }
}

/// A lake's record without its cells (they are in the tiles' lake runs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LakeRecord {
    pub level: f32,
    pub area_m2: f32,
    pub max_depth: f32,
    pub outlet: [f32; 2],
}

/// A tiled world's global part: small, always resident.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForgedMeta {
    /// [`TILED_FORMAT_VERSION`] when written.
    pub format_version: u32,
    pub tile_cells: u32,
    pub globals: WorldGlobals,
    pub lakes: Vec<LakeRecord>,
}

impl ForgedMeta {
    /// What a reader checks before it samples: this build's format, a tile
    /// edge that is a whole number of river buckets, sound global tables.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        if self.format_version != TILED_FORMAT_VERSION {
            return Err(ArtifactError::Format(format!(
                "tiled format version {} is not the {TILED_FORMAT_VERSION} this build reads",
                self.format_version
            )));
        }
        self.globals.validate()?;
        if self.tile_cells == 0 || !tile_is_whole_buckets(self.tile_cells, self.globals.cell_m) {
            return Err(ArtifactError::Format(format!(
                "tile of {} cells x {} m is not a whole number of {BUCKET_M} m river buckets",
                self.tile_cells, self.globals.cell_m
            )));
        }
        let reach = read_radius_cells(self.globals.cell_m);
        if reach > MAX_READ_RADIUS_CELLS {
            return Err(ArtifactError::Format(format!(
                "{} m cells: sampling reads {reach} cells away, past MAX_READ_RADIUS_CELLS ({MAX_READ_RADIUS_CELLS})",
                self.globals.cell_m
            )));
        }
        let g = TileGeometry::new(&self.globals, self.tile_cells);
        if g.buckets_per_side > g.tiles_per_side as usize * g.buckets_per_tile {
            return Err(ArtifactError::Format("river buckets reach past the last tile".into()));
        }
        Ok(())
    }
}

/// Whether `tile_cells * cell_m` is a multiple of `BUCKET_M`, so no river
/// bucket straddles two tiles.
fn tile_is_whole_buckets(tile_cells: u32, cell_m: f64) -> bool {
    let tile_m = f64::from(tile_cells) * cell_m;
    let buckets = tile_m / BUCKET_M;
    buckets >= 1.0 && buckets == buckets.floor()
}

/// The tile grid of one world: how global cells and buckets map to tiles.
#[derive(Clone, Copy, Debug)]
struct TileGeometry {
    n: u32,
    tile_cells: u32,
    tiles_per_side: u32,
    buckets_per_side: usize,
    buckets_per_tile: usize,
}

impl TileGeometry {
    fn new(globals: &WorldGlobals, tile_cells: u32) -> TileGeometry {
        let buckets_per_tile = (f64::from(tile_cells) * globals.cell_m / BUCKET_M) as usize;
        TileGeometry {
            n: globals.n,
            tile_cells,
            tiles_per_side: globals.n.div_ceil(tile_cells),
            buckets_per_side: globals.buckets_per_side(),
            buckets_per_tile,
        }
    }

    /// Cells of the tile at `t`: `(x0, z0, w, h)`.
    fn cells(&self, t: TileCoords) -> (u32, u32, u32, u32) {
        let (x0, z0) = (t.tx * self.tile_cells, t.tz * self.tile_cells);
        (x0, z0, self.tile_cells.min(self.n - x0), self.tile_cells.min(self.n - z0))
    }

    /// Buckets of the tile at `t`: `(bx0, bz0, bw, bh)` (bucket `b` lives in
    /// the tile containing its origin).
    fn bucket_span(&self, t: TileCoords) -> (usize, usize, usize, usize) {
        let per = self.buckets_per_tile;
        let (bx0, bz0) = (t.tx as usize * per, t.tz as usize * per);
        let span = |b0: usize| per.min(self.buckets_per_side.saturating_sub(b0));
        (bx0, bz0, span(bx0), span(bz0))
    }

    /// The tile and local index of CLAMPED global cell `i`.
    fn route_cell(&self, i: usize) -> (TileCoords, usize, u32) {
        let n = self.n as usize;
        debug_assert!(i < n * n, "cell {i} outside the raster");
        let (ix, iz) = ((i % n) as u32, (i / n) as u32);
        let t = TileCoords { tx: ix / self.tile_cells, tz: iz / self.tile_cells };
        let (lx, lz) = (ix % self.tile_cells, iz % self.tile_cells);
        (t, lz as usize, lx)
    }

    /// The tile and local bucket index of global bucket `b`.
    fn route_bucket(&self, b: usize) -> (TileCoords, usize) {
        let per_side = self.buckets_per_side;
        assert!(b < per_side * per_side, "river bucket {b} outside the map");
        let (bx, bz) = (b % per_side, b / per_side);
        let per = self.buckets_per_tile;
        let t = TileCoords { tx: (bx / per) as u32, tz: (bz / per) as u32 };
        let (bx0, bz0, bw, _) = self.bucket_span(t);
        (t, (bz - bz0) * bw + (bx - bx0))
    }
}

/// Splits a WHOLE world into its meta and tiles of `tile_cells` cells, row
/// by row (`tz`, then `tx`).
///
/// Refused (before any tile is cut) when `world` is tiled, or when the meta
/// it would write does not pass [`ForgedMeta::validate`] — a tile edge that
/// is not a whole number of 64 m river buckets, or a cell size whose
/// sampling reads farther than [`MAX_READ_RADIUS_CELLS`] (cells under 8 m):
/// a world the tiled store could not load back is never written.
pub fn split(world: &ForgedWorld, tile_cells: u32) -> Result<(ForgedMeta, Vec<Tile>), ArtifactError> {
    let globals = world.globals();
    let check = ForgedMeta {
        format_version: TILED_FORMAT_VERSION,
        tile_cells,
        globals: globals.clone(),
        lakes: Vec::new(),
    };
    check.validate()?;
    let Rasters::Whole(whole) = world.rasters() else {
        return Err(ArtifactError::Format("split needs a whole world, not a tiled one".into()));
    };
    let artifact = &whole.artifact;
    let rasters = world.rasters();
    let geometry = TileGeometry::new(globals, tile_cells);
    let n = globals.n as usize;
    let qrasters = artifact.qrasters();
    let byte_rasters = artifact.byte_rasters();
    let mut tiles = Vec::new();
    for tz in 0..geometry.tiles_per_side {
        for tx in 0..geometry.tiles_per_side {
            let origin = TileCoords { tx, tz };
            let (x0, z0, w, h) = geometry.cells(origin);
            let indices = || {
                (z0..z0 + h).flat_map(move |iz| (x0..x0 + w).map(move |ix| iz as usize * n + ix as usize))
            };
            let q = std::array::from_fn(|r| indices().map(|i| qrasters[r].data[i]).collect());
            let bytes = std::array::from_fn(|r| indices().map(|i| byte_rasters[r][i]).collect());
            let region = indices().map(|i| artifact.region[i]).collect();
            let coast = indices().map(|i| rasters.coast(i)).collect();
            // Lake runs from the whole world's per-cell answer, so a tile
            // answers exactly what the whole world answers.
            let mut lake_runs: Vec<(u32, u32, f32)> = Vec::new();
            for iz in z0..z0 + h {
                for ix in x0..x0 + w {
                    let i = iz as usize * n + ix as usize;
                    let Some(level) = rasters.lake_level(i) else { continue };
                    match lake_runs.last_mut() {
                        Some(run) if run.1 as usize == i && run.2.to_bits() == level.to_bits() => run.1 += 1,
                        _ => lake_runs.push((i as u32, i as u32 + 1, level)),
                    }
                }
            }
            let (bx0, bz0, bw, bh) = geometry.bucket_span(origin);
            let per_side = geometry.buckets_per_side;
            let buckets: Vec<Vec<u32>> = (bz0..bz0 + bh)
                .flat_map(|bz| (bx0..bx0 + bw).map(move |bx| bz * per_side + bx))
                .map(|b| whole.buckets[b].clone())
                .collect();
            let mut ids: Vec<u32> = buckets.iter().flatten().copied().collect();
            ids.sort_unstable();
            ids.dedup();
            let segments = ids.into_iter().map(|id| (id, whole.segments[id as usize])).collect();
            tiles.push(Tile { origin, w, h, q, bytes, region, coast, lake_runs, segments, buckets });
        }
    }
    let lakes = artifact
        .lakes
        .iter()
        .map(|l| LakeRecord { level: l.level, area_m2: l.area_m2, max_depth: l.max_depth, outlet: l.outlet })
        .collect();
    Ok((ForgedMeta { lakes, ..check }, tiles))
}

/// Bytes before the zstd body: magic (4), format version (u32 LE), crc32 of
/// the zstd body (u32 LE).
#[cfg(not(target_arch = "wasm32"))]
const HEADER_BYTES: usize = 12;

/// `magic`, the format version (u32 LE), the crc32 of the compressed body
/// (u32 LE), then zstd (level 19) over `body`. The crc makes a stored bit
/// flip that would still decompress and deserialize a load error, never
/// silently wrong terrain.
#[cfg(not(target_arch = "wasm32"))]
fn wrap(magic: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let packed = zstd::bulk::compress(body, crate::forged::ZSTD_LEVEL).expect("zstd compresses");
    let mut out = Vec::with_capacity(HEADER_BYTES + packed.len());
    out.extend_from_slice(magic);
    out.extend_from_slice(&TILED_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&packed).to_le_bytes());
    out.extend_from_slice(&packed);
    out
}

/// The body under [`wrap`]'s header, its crc checked.
#[cfg(not(target_arch = "wasm32"))]
fn unwrap(magic: &[u8; 4], what: &str, bytes: &[u8]) -> Result<Vec<u8>, ArtifactError> {
    if bytes.len() < HEADER_BYTES || &bytes[..4] != magic {
        return Err(ArtifactError::Format(format!("not a {what}")));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != TILED_FORMAT_VERSION {
        return Err(ArtifactError::Format(format!(
            "{what} format version {version} is not the {TILED_FORMAT_VERSION} this build reads"
        )));
    }
    let crc = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let packed = &bytes[HEADER_BYTES..];
    if crc32fast::hash(packed) != crc {
        return Err(ArtifactError::Format(format!("corrupt {what}: crc32 mismatch")));
    }
    zstd::stream::decode_all(packed).map_err(|e| ArtifactError::Format(format!("corrupt {what}: {e}")))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn encode_meta(meta: &ForgedMeta) -> Vec<u8> {
    wrap(META_MAGIC, &bincode::serialize(meta).expect("meta serializes"))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn decode_meta(bytes: &[u8]) -> Result<ForgedMeta, ArtifactError> {
    let body = unwrap(META_MAGIC, "tiled world meta", bytes)?;
    bincode::deserialize(&body).map_err(|e| ArtifactError::Format(format!("corrupt tiled world meta: {e}")))
}

/// The tile codec: every raster row-delta encoded (rows of `w`), then
/// bincode, then zstd level 19 — the world file's codec, per tile.
#[cfg(not(target_arch = "wasm32"))]
pub fn encode_tile(tile: &Tile) -> Vec<u8> {
    use crate::forged::{delta_rows, delta_rows_u8};
    let mut wire = tile.clone();
    let w = tile.w as usize;
    for r in &mut wire.q {
        delta_rows(r, w);
    }
    for r in &mut wire.bytes {
        delta_rows_u8(r, w);
    }
    delta_rows(&mut wire.region, w);
    wrap(TILE_MAGIC, &bincode::serialize(&wire).expect("tile serializes"))
}

/// Inverse of [`encode_tile`]; checks that every per-cell vector holds
/// `w * h` cells.
#[cfg(not(target_arch = "wasm32"))]
pub fn decode_tile(bytes: &[u8]) -> Result<Tile, ArtifactError> {
    use crate::forged::{undelta_rows, undelta_rows_u8};
    let body = unwrap(TILE_MAGIC, "forged tile", bytes)?;
    let mut tile: Tile =
        bincode::deserialize(&body).map_err(|e| ArtifactError::Format(format!("corrupt forged tile: {e}")))?;
    let cells = tile.w as usize * tile.h as usize;
    if tile.q.iter().any(|r| r.len() != cells)
        || tile.bytes.iter().any(|r| r.len() != cells)
        || tile.region.len() != cells
        || tile.coast.len() != cells
    {
        return Err(ArtifactError::Format("forged tile: raster size does not match w * h".into()));
    }
    let w = tile.w as usize;
    for r in &mut tile.q {
        undelta_rows(r, w);
    }
    for r in &mut tile.bytes {
        undelta_rows_u8(r, w);
    }
    undelta_rows(&mut tile.region, w);
    Ok(tile)
}

/// No zstd in the browser: tiled worlds are native only.
#[cfg(target_arch = "wasm32")]
pub fn decode_meta(_bytes: &[u8]) -> Result<ForgedMeta, ArtifactError> {
    Err(ArtifactError::Format("tiled worlds are native only".into()))
}

/// No zstd in the browser: tiled worlds are native only.
#[cfg(target_arch = "wasm32")]
pub fn decode_tile(_bytes: &[u8]) -> Result<Tile, ArtifactError> {
    Err(ArtifactError::Format("tiled worlds are native only".into()))
}

/// Where tiles come from: their encoded bytes ([`encode_tile`]) by
/// coordinates. IO lives in the implementor (the world store).
pub trait TileLoader: Send + Sync {
    fn load(&self, t: TileCoords) -> Result<Vec<u8>, String>;
}

/// Encoded tiles in memory (tests, the importer's verification).
pub struct MemTiles(pub BTreeMap<TileCoords, Vec<u8>>);

impl TileLoader for MemTiles {
    fn load(&self, t: TileCoords) -> Result<Vec<u8>, String> {
        self.0.get(&t).cloned().ok_or_else(|| format!("no tile ({}, {})", t.tx, t.tz))
    }
}

/// One cached tile. Concurrent misses share the `OnceLock`, so a tile is
/// loaded once however many threads want it; a failed load is dropped from
/// the cache so the next read tries again. `bytes`, `pins` and `evicted`
/// change only under the cache's write lock (atomics so readers can look
/// without it).
struct Slot {
    tile: OnceLock<Result<Arc<Tile>, String>>,
    /// The cache clock at the last read (LRU).
    last_use: AtomicU64,
    /// Decoded bytes counted against the budget (0 until admitted).
    bytes: AtomicUsize,
    /// Live [`TilePins`] holding this tile: a pinned tile is never evicted.
    pins: AtomicUsize,
    /// Set when the slot leaves the cache: the threads' memos stop serving it.
    evicted: AtomicBool,
}

impl Slot {
    fn new(now: u64) -> Slot {
        Slot {
            tile: OnceLock::new(),
            last_use: AtomicU64::new(now),
            bytes: AtomicUsize::new(0),
            pins: AtomicUsize::new(0),
            evicted: AtomicBool::new(false),
        }
    }
}

/// Decoded tiles over a [`TileLoader`]: thread-safe, loads outside the
/// lock, least recently used unpinned tiles evicted once the decoded bytes
/// pass the budget. Readers hold `Arc<Tile>`s, so eviction never pulls a
/// tile from under a read.
///
/// A read checks the calling thread's last-tile memo, then the cache; a
/// pinned tile ([`ForgedWorld::prefetch_rect`]) is always in the cache, so a
/// read of it never loads.
///
/// Memory bound: [`Self::resident_bytes`] ≤ budget + [`Self::pinned_bytes`]
/// whenever the budget holds the largest tile (the cache goes over only
/// while jobs pin more than fits; it logs that once). Not counted there: a
/// thread's memo keeps the last tile it read alive until its next read of a
/// tiled world, so up to one evicted tile per thread that has not read since
/// the eviction (or since the world was dropped) — memo hits never serve an
/// evicted tile, they fall through to the cache.
pub struct TileCache {
    loader: Arc<dyn TileLoader>,
    budget_bytes: usize,
    geometry: TileGeometry,
    q_scale: [(f32, f32); 5],
    region_count: usize,
    /// One entry per tile of the grid (`tz * tiles_per_side + tx`): `Some`
    /// while the tile is cached or loading.
    slots: RwLock<Vec<Option<Arc<Slot>>>>,
    clock: AtomicU64,
    resident: AtomicUsize,
    /// Decoded bytes of the resident tiles that are pinned.
    pinned: AtomicUsize,
    /// The over-budget warning has been logged.
    warned: AtomicBool,
    /// Tiles loaded (decoded and admitted) since the cache was made.
    loads: AtomicU64,
    /// Tile loads that failed (after [`TILE_LOAD_RETRIES`] attempts) since
    /// the cache was made.
    load_errors: AtomicU64,
    /// Tells this cache's tiles apart in the threads' `LAST_TILE` memos.
    id: u64,
}

/// A snapshot of a [`TileCache`]'s counters ([`TileCache::stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileCacheStats {
    /// Decoded bytes of the tiles the cache holds (pinned ones included).
    pub resident_bytes: usize,
    /// Decoded bytes of the resident tiles some `TilePins` holds.
    pub pinned_bytes: usize,
    pub budget_bytes: usize,
    /// Tiles loaded since the cache was made (cumulative).
    pub loads: u64,
    /// Failed tile loads since the cache was made (cumulative).
    pub load_errors: u64,
}

/// Source of [`TileCache`] ids.
static NEXT_CACHE_ID: AtomicU64 = AtomicU64::new(0);

/// A thread's last tile read (`LAST_TILE`).
struct Memo {
    cache: u64,
    coords: TileCoords,
    slot: Arc<Slot>,
    tile: Arc<Tile>,
}

thread_local! {
    /// The tile this thread read last. A hit costs no lock and no shared
    /// write (the LRU stamp is refreshed only when the cache clock has moved
    /// since); a hit on a tile evicted since falls through to the cache.
    static LAST_TILE: std::cell::RefCell<Option<Memo>> = const { std::cell::RefCell::new(None) };
}

/// Tiles held in the cache for a job (region generation, an LOD build):
/// while it lives they are never evicted, so the job's reads never load.
/// Dropping it unpins them (and trims the cache back to its budget).
#[derive(Default)]
pub struct TilePins {
    tiles: Vec<(TileCoords, Arc<Tile>)>,
    cache: Option<Arc<TileCache>>,
}

impl TilePins {
    /// Number of pinned tiles (0 for a whole world).
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// The pinned tiles' coordinates, row by row.
    pub fn coords(&self) -> impl Iterator<Item = TileCoords> + '_ {
        self.tiles.iter().map(|(t, _)| *t)
    }
}

impl Drop for TilePins {
    fn drop(&mut self) {
        if let Some(cache) = &self.cache {
            cache.unpin(&self.tiles);
        }
    }
}

impl TileCache {
    /// `meta` must have passed [`ForgedMeta::validate`].
    pub fn new(meta: &ForgedMeta, loader: Arc<dyn TileLoader>, budget_bytes: usize) -> TileCache {
        let geometry = TileGeometry::new(&meta.globals, meta.tile_cells);
        TileCache {
            loader,
            budget_bytes,
            geometry,
            q_scale: meta.globals.q_scale,
            region_count: meta.globals.regions.len(),
            slots: RwLock::new(vec![None; (geometry.tiles_per_side as usize).pow(2)]),
            clock: AtomicU64::new(0),
            resident: AtomicUsize::new(0),
            pinned: AtomicUsize::new(0),
            warned: AtomicBool::new(false),
            loads: AtomicU64::new(0),
            load_errors: AtomicU64::new(0),
            id: NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Decoded bytes of the tiles the cache holds (pinned ones included).
    pub fn resident_bytes(&self) -> usize {
        self.resident.load(Ordering::Relaxed)
    }

    /// Decoded bytes of the resident tiles some [`TilePins`] holds.
    pub fn pinned_bytes(&self) -> usize {
        self.pinned.load(Ordering::Relaxed)
    }

    /// The budget the cache evicts down to.
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// The cache's counters, read with relaxed loads (observability: the
    /// server's periodic `[server]` line). `loads` and `load_errors` count
    /// from the cache's creation; a reader diffs them per window.
    pub fn stats(&self) -> TileCacheStats {
        TileCacheStats {
            resident_bytes: self.resident_bytes(),
            pinned_bytes: self.pinned_bytes(),
            budget_bytes: self.budget_bytes,
            loads: self.loads.load(Ordering::Relaxed),
            load_errors: self.load_errors.load(Ordering::Relaxed),
        }
    }

    /// The tile at `t`, loading it on a miss.
    ///
    /// # Panics
    /// If the tile cannot be loaded after [`TILE_LOAD_RETRIES`] attempts:
    /// generation never makes terrain without its tiles (generating paths
    /// pin theirs up front).
    pub fn tile(&self, t: TileCoords) -> Arc<Tile> {
        self.try_tile(t).unwrap_or_else(|e| panic!("forged tile ({}, {}): {e}", t.tx, t.tz))
    }

    /// [`Self::tile`] that reports a failed load instead of panicking.
    pub fn try_tile(&self, t: TileCoords) -> Result<Arc<Tile>, String> {
        self.fetch(t).map(|(_, tile)| tile)
    }

    /// The slot and tile at `t`, loading on a miss; stamps the slot.
    fn fetch(&self, t: TileCoords) -> Result<(Arc<Slot>, Arc<Tile>), String> {
        let per_side = self.geometry.tiles_per_side;
        if t.tx >= per_side || t.tz >= per_side {
            return Err(format!("outside the {per_side}x{per_side} tile grid"));
        }
        let index = self.slot_index(t);
        let now = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let found = self.slots.read().expect("tile cache lock")[index].clone();
        let slot = found.unwrap_or_else(|| {
            let mut slots = self.slots.write().expect("tile cache lock");
            slots[index].get_or_insert_with(|| Arc::new(Slot::new(now))).clone()
        });
        slot.last_use.store(now, Ordering::Relaxed);
        if let Some(Ok(tile)) = slot.tile.get() {
            let tile = tile.clone();
            return Ok((slot, tile));
        }
        let mut loaded_here = false;
        let result = slot
            .tile
            .get_or_init(|| {
                loaded_here = true;
                self.load(t).map(Arc::new)
            })
            .clone();
        match &result {
            Ok(tile) if loaded_here => {
                self.loads.fetch_add(1, Ordering::Relaxed);
                self.admit(t, &slot, tile.decoded_bytes())
            }
            Err(_) => {
                if loaded_here {
                    self.load_errors.fetch_add(1, Ordering::Relaxed);
                }
                self.forget(t, &slot)
            }
            Ok(_) => {}
        }
        result.map(|tile| (slot, tile))
    }

    fn slot_index(&self, t: TileCoords) -> usize {
        t.tz as usize * self.geometry.tiles_per_side as usize + t.tx as usize
    }

    /// Loads and pins every tile in `tx0..=tx1` x `tz0..=tz1` (clamped to the
    /// grid), row by row. The first tile that fails ends it with an error
    /// naming the tile; the tiles pinned so far are unpinned.
    pub(crate) fn pin_rect(self: &Arc<Self>, (tx0, tz0): (u32, u32), (tx1, tz1): (u32, u32)) -> Result<TilePins, String> {
        let last = self.geometry.tiles_per_side - 1;
        let mut pins = TilePins { tiles: Vec::new(), cache: Some(self.clone()) };
        for tz in tz0.min(last)..=tz1.min(last) {
            for tx in tx0.min(last)..=tx1.min(last) {
                let t = TileCoords { tx, tz };
                let tile = self.pin(t).map_err(|e| format!("forged tile ({tx}, {tz}): {e}"))?;
                pins.tiles.push((t, tile));
            }
        }
        Ok(pins)
    }

    /// Loads tile `t` and pins it in the cache.
    fn pin(&self, t: TileCoords) -> Result<Arc<Tile>, String> {
        let index = self.slot_index(t);
        loop {
            let tile = self.try_tile(t)?;
            let slots = self.slots.write().expect("tile cache lock");
            let Some(slot) = &slots[index] else { continue }; // evicted since: load again
            if !slot.tile.get().is_some_and(|r| r.as_ref().is_ok_and(|cur| Arc::ptr_eq(cur, &tile))) {
                continue;
            }
            if slot.pins.fetch_add(1, Ordering::Relaxed) == 0 {
                // 0 while not yet admitted: `admit` counts it then.
                self.pinned.fetch_add(slot.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
            }
            return Ok(tile);
        }
    }

    /// Releases pins taken by [`Self::pin_rect`], then trims the cache back
    /// to its budget.
    fn unpin(&self, tiles: &[(TileCoords, Arc<Tile>)]) {
        let mut slots = self.slots.write().expect("tile cache lock");
        for (t, _) in tiles {
            let slot = slots[self.slot_index(*t)].as_ref().expect("a pinned tile stays cached");
            if slot.pins.fetch_sub(1, Ordering::Relaxed) == 1 {
                self.pinned.fetch_sub(slot.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
            }
        }
        self.trim(&mut slots, None);
    }

    /// Loads and checks tile `t`, retrying the loader.
    fn load(&self, t: TileCoords) -> Result<Tile, String> {
        let mut last = String::new();
        for _ in 0..TILE_LOAD_RETRIES {
            match self.loader.load(t) {
                Ok(bytes) => return self.decode_checked(t, &bytes),
                Err(e) => last = e,
            }
        }
        Err(format!("{TILE_LOAD_RETRIES} load attempts failed: {last}"))
    }

    /// A decoded tile that is the one asked for and fits this world: what
    /// the read path relies on (sizes, bucket lists naming carried segments,
    /// region ids and biomes in range, lake runs inside the tile, no NaN).
    fn decode_checked(&self, t: TileCoords, bytes: &[u8]) -> Result<Tile, String> {
        let tile = decode_tile(bytes).map_err(|e| e.to_string())?;
        let (x0, z0, w, h) = self.geometry.cells(t);
        if tile.origin != t || tile.w != w || tile.h != h {
            return Err(format!("holds tile {:?} of {}x{} cells, not the {w}x{h} asked for", tile.origin, tile.w, tile.h));
        }
        let (_, _, bw, bh) = self.geometry.bucket_span(t);
        if tile.buckets.len() != bw * bh {
            return Err("bucket count does not match the tile".into());
        }
        if tile.segments.windows(2).any(|p| p[0].0 >= p[1].0) {
            return Err("segments not in ascending id order".into());
        }
        let carried = |id: &u32| tile.segments.binary_search_by_key(id, |&(s, _)| s).is_ok();
        if !tile.buckets.iter().flatten().all(carried) {
            return Err("a bucket names a segment the tile does not carry".into());
        }
        if tile.region.iter().any(|&r| r as usize >= self.region_count) {
            return Err("region id out of range".into());
        }
        if tile.bytes[ByteRasterId::Biome as usize].iter().any(|&b| game::Biome::from_u8(b).is_none()) {
            return Err("unknown biome".into());
        }
        if tile.coast.iter().any(|c| c.is_nan()) {
            return Err("NaN coast distance".into());
        }
        // Lake runs: non-empty, ascending and disjoint, each inside one row
        // of the tile's own cells, with a real level — `lake_level` looks a
        // cell up by binary search and trusts all of that.
        let n = self.geometry.n as u64;
        let inside = |start: u32, end: u32| {
            let (first, last) = (u64::from(start), u64::from(end) - 1);
            let (row, col) = (first / n, first % n);
            last / n == row
                && (u64::from(z0)..u64::from(z0 + h)).contains(&row)
                && col >= u64::from(x0)
                && last % n < u64::from(x0 + w)
        };
        let mut prev_end = 0u32;
        for &(start, end, level) in &tile.lake_runs {
            if start >= end || start < prev_end || !inside(start, end) || !level.is_finite() {
                return Err(format!("bad lake run ({start}, {end}, {level})"));
            }
            prev_end = end;
        }
        Ok(tile)
    }

    /// Counts a freshly loaded tile against the budget and evicts the least
    /// recently used unpinned others while the cache is over it.
    fn admit(&self, t: TileCoords, slot: &Arc<Slot>, bytes: usize) {
        let index = self.slot_index(t);
        let mut slots = self.slots.write().expect("tile cache lock");
        if !slots[index].as_ref().is_some_and(|s| Arc::ptr_eq(s, slot)) {
            return; // not resident (cannot happen today: loading slots are never victims)
        }
        slot.bytes.store(bytes, Ordering::Relaxed);
        self.resident.fetch_add(bytes, Ordering::Relaxed);
        if slot.pins.load(Ordering::Relaxed) > 0 {
            self.pinned.fetch_add(bytes, Ordering::Relaxed);
        }
        self.trim(&mut slots, Some(index));
    }

    /// Evicts least recently used, unpinned, loaded tiles (never `keep`)
    /// until the cache is within budget; logs once if pins hold it over.
    fn trim(&self, slots: &mut [Option<Arc<Slot>>], keep: Option<usize>) {
        let mut resident = self.resident.load(Ordering::Relaxed);
        while resident > self.budget_bytes {
            let victim = slots
                .iter()
                .enumerate()
                .filter_map(|(k, s)| Some((k, s.as_ref()?)))
                .filter(|&(k, s)| {
                    Some(k) != keep && s.bytes.load(Ordering::Relaxed) > 0 && s.pins.load(Ordering::Relaxed) == 0
                })
                .min_by_key(|(_, s)| s.last_use.load(Ordering::Relaxed))
                .map(|(k, _)| k);
            let Some(victim) = victim else {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    log::warn!(
                        "[forged-tiles] tile cache over its budget: {resident} B resident, {} B pinned, budget {} B \
                         (jobs pin more tiles than fit; raise {TILE_CACHE_MB_ENV}) — logged once",
                        self.pinned.load(Ordering::Relaxed),
                        self.budget_bytes
                    );
                }
                break;
            };
            let gone = slots[victim].take().expect("victim present");
            gone.evicted.store(true, Ordering::Relaxed);
            resident -= gone.bytes.load(Ordering::Relaxed);
        }
        self.resident.store(resident, Ordering::Relaxed);
    }

    /// Drops a slot whose load failed, so the next read tries again.
    fn forget(&self, t: TileCoords, slot: &Arc<Slot>) {
        let index = self.slot_index(t);
        let mut slots = self.slots.write().expect("tile cache lock");
        if slots[index].as_ref().is_some_and(|s| Arc::ptr_eq(s, slot)) {
            slots[index] = None;
        }
    }

    /// `f` over tile `t`. The calling thread's memo of the last tile it read
    /// (`LAST_TILE`) makes the hot path one thread-local lookup: no lock, no
    /// shared write unless the cache clock moved since the tile's stamp. The
    /// memo is not borrowed across a load, so a loader may itself sample a
    /// tiled world on this thread.
    fn with_tile<R>(&self, t: TileCoords, f: impl FnOnce(&Tile) -> R) -> R {
        let mut f = Some(f);
        let hit = LAST_TILE.with(|memo| {
            let memo = memo.borrow();
            let m = memo.as_ref().filter(|m| m.cache == self.id && m.coords == t)?;
            if m.slot.evicted.load(Ordering::Relaxed) {
                return None;
            }
            let now = self.clock.load(Ordering::Relaxed);
            if m.slot.last_use.load(Ordering::Relaxed) < now {
                m.slot.last_use.store(now, Ordering::Relaxed);
            }
            Some((f.take().expect("unused"))(&m.tile))
        });
        if let Some(r) = hit {
            return r;
        }
        let (slot, tile) = self.fetch(t).unwrap_or_else(|e| panic!("forged tile ({}, {}): {e}", t.tx, t.tz));
        let r = (f.take().expect("unused"))(&tile);
        let old = LAST_TILE.with(|memo| memo.replace(Some(Memo { cache: self.id, coords: t, slot, tile })));
        drop(old); // outside the borrow
        r
    }

    /// `f` over the tile holding CLAMPED global cell `i` and the cell's
    /// local index.
    fn with_cell<R>(&self, i: usize, f: impl FnOnce(&Tile, usize) -> R) -> R {
        let (t, lz, lx) = self.geometry.route_cell(i);
        self.with_tile(t, |tile| f(tile, lz * tile.w as usize + lx as usize))
    }

    pub(crate) fn q(&self, r: QRasterId, i: usize) -> f32 {
        let (min, step) = self.q_scale[r as usize];
        // `QRaster::get`'s arithmetic exactly.
        self.with_cell(i, |tile, local| min + tile.q[r as usize][local] as f32 * step)
    }

    pub(crate) fn byte(&self, r: ByteRasterId, i: usize) -> u8 {
        self.with_cell(i, |tile, local| tile.bytes[r as usize][local])
    }

    pub(crate) fn region_id(&self, i: usize) -> u16 {
        self.with_cell(i, |tile, local| tile.region[local])
    }

    pub(crate) fn coast(&self, i: usize) -> f32 {
        self.with_cell(i, |tile, local| tile.coast[local])
    }

    pub(crate) fn lake_level(&self, i: usize) -> Option<f32> {
        self.with_cell(i, |tile, _| {
            let i = i as u32;
            let k = tile.lake_runs.partition_point(|run| run.0 <= i);
            let &(_, end, level) = tile.lake_runs.get(k.checked_sub(1)?)?;
            (i < end).then_some(level)
        })
    }

    /// Global bucket `b`'s segments (see `Rasters::bucket`), copied out of
    /// the tile through the memo (no lock, no shared refcount on a hit).
    pub(crate) fn bucket(&self, b: usize) -> crate::forged::SegmentsRef<'static> {
        let (t, local) = self.geometry.route_bucket(b);
        let segments = self.with_tile(t, |tile| {
            tile.buckets[local]
                .iter()
                .map(|&id| {
                    let k = tile.segments.binary_search_by_key(&id, |&(s, _)| s).expect("checked at load");
                    tile.segments[k]
                })
                .collect::<Vec<_>>()
        });
        crate::forged::SegmentsRef::Tiled(segments.into_iter())
    }

    /// The tiles holding raster cells `x0..=x1` x `z0..=z1` (clamped).
    pub(crate) fn tiles_of_cells(&self, (x0, z0): (u32, u32), (x1, z1): (u32, u32)) -> ((u32, u32), (u32, u32)) {
        let c = self.geometry.tile_cells;
        ((x0 / c, z0 / c), (x1 / c, z1 / c))
    }
}
