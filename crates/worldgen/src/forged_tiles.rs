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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
/// used tile (Decision 4).
pub const TILE_CACHE_BYTES: usize = 256 << 20;
/// Attempts at loading one tile before the read fails.
pub const TILE_LOAD_RETRIES: u32 = 3;

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
/// # Panics
/// If `world` is tiled, or `tile_cells * cell_m` is not a whole number of
/// 64 m river buckets.
pub fn split(world: &ForgedWorld, tile_cells: u32) -> (ForgedMeta, Vec<Tile>) {
    let globals = world.globals();
    assert!(
        tile_cells > 0 && tile_is_whole_buckets(tile_cells, globals.cell_m),
        "tile of {tile_cells} cells x {} m must be a whole number of {BUCKET_M} m river buckets",
        globals.cell_m
    );
    let Rasters::Whole(whole) = world.rasters() else { panic!("split needs a whole world") };
    let artifact = &whole.artifact;
    let rasters = world.rasters();
    let geometry = TileGeometry::new(globals, tile_cells);
    assert!(
        geometry.buckets_per_side <= geometry.tiles_per_side as usize * geometry.buckets_per_tile,
        "river buckets reach past the last tile"
    );
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
    let meta = ForgedMeta { format_version: TILED_FORMAT_VERSION, tile_cells, globals: globals.clone(), lakes };
    (meta, tiles)
}

/// `magic`, the format version (u32 LE), then zstd (level 19) over `body`.
#[cfg(not(target_arch = "wasm32"))]
fn wrap(magic: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(&TILED_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&zstd::bulk::compress(body, crate::forged::ZSTD_LEVEL).expect("zstd compresses"));
    out
}

/// The body under [`wrap`]'s header.
#[cfg(not(target_arch = "wasm32"))]
fn unwrap(magic: &[u8; 4], what: &str, bytes: &[u8]) -> Result<Vec<u8>, ArtifactError> {
    if bytes.len() < 8 || &bytes[..4] != magic {
        return Err(ArtifactError::Format(format!("not a {what}")));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != TILED_FORMAT_VERSION {
        return Err(ArtifactError::Format(format!(
            "{what} format version {version} is not the {TILED_FORMAT_VERSION} this build reads"
        )));
    }
    zstd::stream::decode_all(&bytes[8..]).map_err(|e| ArtifactError::Format(format!("corrupt {what}: {e}")))
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
/// the cache so the next read tries again.
struct Slot {
    tile: OnceLock<Result<Arc<Tile>, String>>,
    /// The cache clock at the last read (LRU).
    last_use: AtomicU64,
    /// Decoded bytes counted against the budget (0 until loaded).
    bytes: AtomicUsize,
}

/// Decoded tiles over a [`TileLoader`]: thread-safe, loads outside the
/// lock, least recently used tiles evicted once the decoded bytes pass the
/// budget. Readers hold `Arc<Tile>`s, so eviction never pulls a tile from
/// under a read.
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
    /// Tells this cache's tiles apart in the threads' `LAST_TILE` memos.
    id: u64,
}

/// Source of [`TileCache`] ids.
static NEXT_CACHE_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The tile this thread read last: `(cache id, coords, tile)`. It keeps
    /// at most one tile per thread alive past its eviction (tiles are
    /// immutable, so a remembered one is never stale); a memo hit does not
    /// refresh the tile's LRU stamp, so a hot tile may be evicted and later
    /// reloaded by another thread — results are unaffected.
    static LAST_TILE: std::cell::RefCell<Option<(u64, TileCoords, Arc<Tile>)>> =
        const { std::cell::RefCell::new(None) };
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
            id: NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Decoded bytes of the tiles the cache holds.
    pub fn resident_bytes(&self) -> usize {
        self.resident.load(Ordering::Relaxed)
    }

    /// The tile at `t`, loading it on a miss.
    ///
    /// # Panics
    /// If the tile cannot be loaded after [`TILE_LOAD_RETRIES`] attempts:
    /// generation never makes terrain without its tiles (generating paths
    /// load theirs up front).
    pub fn tile(&self, t: TileCoords) -> Arc<Tile> {
        self.try_tile(t).unwrap_or_else(|e| panic!("forged tile ({}, {}): {e}", t.tx, t.tz))
    }

    /// [`Self::tile`] that reports a failed load instead of panicking.
    pub fn try_tile(&self, t: TileCoords) -> Result<Arc<Tile>, String> {
        let per_side = self.geometry.tiles_per_side;
        if t.tx >= per_side || t.tz >= per_side {
            return Err(format!("outside the {per_side}x{per_side} tile grid"));
        }
        let index = self.slot_index(t);
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        let found = self.slots.read().expect("tile cache lock")[index].clone();
        let slot = found.unwrap_or_else(|| {
            let mut slots = self.slots.write().expect("tile cache lock");
            slots[index]
                .get_or_insert_with(|| {
                    Arc::new(Slot { tile: OnceLock::new(), last_use: AtomicU64::new(now), bytes: AtomicUsize::new(0) })
                })
                .clone()
        });
        slot.last_use.store(now, Ordering::Relaxed);
        if let Some(Ok(tile)) = slot.tile.get() {
            return Ok(tile.clone());
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
            Ok(tile) if loaded_here => self.admit(t, &slot, tile.decoded_bytes()),
            Err(_) => self.forget(t, &slot),
            Ok(_) => {}
        }
        result
    }

    fn slot_index(&self, t: TileCoords) -> usize {
        t.tz as usize * self.geometry.tiles_per_side as usize + t.tx as usize
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
    /// region ids and biomes in range).
    fn decode_checked(&self, t: TileCoords, bytes: &[u8]) -> Result<Tile, String> {
        let tile = decode_tile(bytes).map_err(|e| e.to_string())?;
        let (_, _, w, h) = self.geometry.cells(t);
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
        Ok(tile)
    }

    /// Counts a freshly loaded tile against the budget and evicts the least
    /// recently used others while the cache is over it.
    fn admit(&self, t: TileCoords, slot: &Arc<Slot>, bytes: usize) {
        let index = self.slot_index(t);
        let mut slots = self.slots.write().expect("tile cache lock");
        if !slots[index].as_ref().is_some_and(|s| Arc::ptr_eq(s, slot)) {
            return; // evicted while it loaded: not resident
        }
        slot.bytes.store(bytes, Ordering::Relaxed);
        let mut resident = self.resident.load(Ordering::Relaxed) + bytes;
        while resident > self.budget_bytes {
            let victim = slots
                .iter()
                .enumerate()
                .filter_map(|(k, s)| Some((k, s.as_ref()?)))
                .filter(|&(k, s)| k != index && s.bytes.load(Ordering::Relaxed) > 0)
                .min_by_key(|(_, s)| s.last_use.load(Ordering::Relaxed))
                .map(|(k, _)| k);
            let Some(victim) = victim else { break };
            let gone = slots[victim].take().expect("victim present");
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

    /// `f` over the tile holding CLAMPED global cell `i` and the cell's
    /// local index. The calling thread remembers the last tile it read
    /// (`LAST_TILE`), so the hot path is one thread-local lookup: no lock and
    /// no shared atomic, which would make every region thread contend on
    /// every raster read.
    fn with_cell<R>(&self, i: usize, f: impl FnOnce(&Tile, usize) -> R) -> R {
        let (t, lz, lx) = self.geometry.route_cell(i);
        LAST_TILE.with(|memo| {
            let mut memo = memo.borrow_mut();
            let hit = memo.as_ref().is_some_and(|(id, c, _)| *id == self.id && *c == t);
            if !hit {
                *memo = Some((self.id, t, self.tile(t)));
            }
            let tile = &memo.as_ref().expect("just filled").2;
            f(tile, lz * tile.w as usize + lx as usize)
        })
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

    /// Global bucket `b`'s segments (see `Rasters::bucket`).
    pub(crate) fn bucket(&self, b: usize) -> crate::forged::SegmentsRef<'static> {
        let (t, local) = self.geometry.route_bucket(b);
        crate::forged::SegmentsRef::Tiled(TileBucket { tile: self.tile(t), bucket: local, next: 0 })
    }
}

/// The segments of one bucket of a tile, `(global id, segment)` ascending;
/// keeps its tile alive.
pub(crate) struct TileBucket {
    tile: Arc<Tile>,
    bucket: usize,
    next: usize,
}

impl Iterator for TileBucket {
    type Item = (u32, Segment);

    fn next(&mut self) -> Option<Self::Item> {
        let &id = self.tile.buckets[self.bucket].get(self.next)?;
        self.next += 1;
        let k = self.tile.segments.binary_search_by_key(&id, |&(s, _)| s).expect("checked at load");
        Some(self.tile.segments[k])
    }
}
