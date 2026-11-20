//! The world map's tile reader (plan `2026-11-20-block-level-world-map`, spec §5.2): one crate, two thin
//! fronts — the native client calls [`TileSource::tile`] against its world's store (single-player), and
//! the server's map route wraps the same call (multiplayer). A tile is worldgen's surface
//! (`worldgen::surface`) with the stored chunk tops (`chunks.top`) of its regions over it down to
//! [`MAP_EDIT_MAX_LEVEL`]; its etag hashes those regions' `terrain_version`s, so a wholly pristine tile's
//! etag never changes. It reads versions and chunk tops only — never ownership, a host or a coordinator.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::{Arc, Mutex, RwLock};

use game::map_tile::{MAP_TILE_SAMPLES, MapTile, MapTileKey, sample_flags};
use game::{BLOCK_VOXELS, BlockId, CHUNK_BLOCKS, MapExtent, REGION_BLOCKS, RegionCoords};
use world_store::{StoreError, StoreHandle};
use worldgen::forged::ForgedWorld;
use worldgen::material::Materials;

/// What tiles look like, in every etag: bump it on any change to what a tile built from the same world
/// and the same stored terrain holds (worldgen's surface, the overlay, the canopy), or every cache —
/// the browser's, the native disk's — keeps answering 304 with the old picture.
/// 2: pristine tree canopies (crowns at level 0, the lattice cells' leaves above).
pub const MAP_TILE_RENDER_VERSION: u32 = 2;

/// Stored chunk tops are overlaid on levels up to this one (8 m per sample); above it a tile is
/// worldgen's alone (spec §9, decision 3).
pub const MAP_EDIT_MAX_LEVEL: u8 = 3;
/// The tile cache of a serving process (bytes of encoded tiles): it shares a coordinator pod (512Mi) with
/// the forged world and the coordinator's own state.
pub const MAP_TILE_CACHE_BYTES_SERVER: usize = 32 << 20;
/// The tile cache of a client's local source.
pub const MAP_TILE_CACHE_BYTES_CLIENT: usize = 128 << 20;
/// Regions whose combined tops are kept by a client (each ~6 B per block column).
pub const MAP_REGION_TOPS_CACHE: usize = 512;
/// … and by a serving process.
pub const MAP_REGION_TOPS_CACHE_SERVER: usize = 64;

const EDGE: usize = MAP_TILE_SAMPLES as usize;
const RB: usize = REGION_BLOCKS;

#[derive(Clone, Copy, Debug)]
pub struct TileSourceConfig {
    pub cache_bytes: usize,
    pub region_tops_cache: usize,
}

impl TileSourceConfig {
    pub const SERVER: TileSourceConfig = TileSourceConfig {
        cache_bytes: MAP_TILE_CACHE_BYTES_SERVER,
        region_tops_cache: MAP_REGION_TOPS_CACHE_SERVER,
    };
    pub const CLIENT: TileSourceConfig = TileSourceConfig {
        cache_bytes: MAP_TILE_CACHE_BYTES_CLIENT,
        region_tops_cache: MAP_REGION_TOPS_CACHE,
    };
}

/// What [`TileSource::tile`] answers.
#[derive(Clone, Debug)]
pub enum Fetched {
    /// The caller's etag is current.
    Unchanged,
    /// The tile's bytes (`MapTile::encode`) and etag.
    Tile { etag: u64, bytes: Arc<Vec<u8>> },
}

enum Terrain {
    Forged { world: WorldRef, extent: MapExtent },
    Procedural(u64),
}

/// The forged world a source reads: its own, or the process's installed one (`forged::install`, role
/// `all`), which lives for the process.
enum WorldRef {
    Owned(Arc<ForgedWorld>),
    Installed(&'static ForgedWorld),
}

impl std::ops::Deref for WorldRef {
    type Target = ForgedWorld;
    fn deref(&self) -> &ForgedWorld {
        match self {
            WorldRef::Owned(w) => w,
            WorldRef::Installed(w) => w,
        }
    }
}

/// A region's stored surface: per block column (`lx + REGION_BLOCKS * lz`) the highest stored chunk's top
/// (height in m — the top of its top block — and that block); `None` = no stored chunk shows one there.
type RegionTop = Vec<Option<(i16, BlockId)>>;

/// The tile reader. `Send + Sync`; every call that reads the store blocks — run it on a worker thread.
pub struct TileSource {
    store: StoreHandle,
    terrain: Terrain,
    /// The world's identity in every etag (the forged world's content crc, the procedural seed): a
    /// re-forge under the same id and URL changes every tile's etag, so no cache serves the old world.
    tag: u64,
    mats: Materials,
    /// Every stored region's `terrain_version` (`None` until the first read).
    versions: RwLock<Option<Arc<BTreeMap<RegionCoords, u64>>>>,
    tiles: Mutex<Lru<MapTileKey, (u64, Arc<Vec<u8>>)>>,
    tops: Mutex<Lru<(RegionCoords, u64), Arc<RegionTop>>>,
}

impl TileSource {
    /// Over a forged world (the store opened `OpenMode::ReadOnly` by a reader beside a server).
    pub fn forged(
        store: StoreHandle,
        world: Arc<ForgedWorld>,
        mats: Materials,
        cfg: TileSourceConfig,
    ) -> TileSource {
        let extent = worldgen::surface::map_extent(&world);
        TileSource::new(
            store,
            Terrain::Forged {
                world: WorldRef::Owned(world),
                extent,
            },
            mats,
            cfg,
        )
    }

    /// Over the process's installed forged world (`worldgen::forged::active()`).
    pub fn forged_installed(
        store: StoreHandle,
        world: &'static ForgedWorld,
        mats: Materials,
        cfg: TileSourceConfig,
    ) -> TileSource {
        let extent = worldgen::surface::map_extent(world);
        TileSource::new(
            store,
            Terrain::Forged {
                world: WorldRef::Installed(world),
                extent,
            },
            mats,
            cfg,
        )
    }

    /// Over the procedural world of `seed`.
    pub fn procedural(
        store: StoreHandle,
        seed: u64,
        mats: Materials,
        cfg: TileSourceConfig,
    ) -> TileSource {
        TileSource::new(store, Terrain::Procedural(seed), mats, cfg)
    }

    fn new(
        store: StoreHandle,
        terrain: Terrain,
        mats: Materials,
        cfg: TileSourceConfig,
    ) -> TileSource {
        let tag = match &terrain {
            Terrain::Forged { world, .. } => u64::from(world.content_crc()),
            Terrain::Procedural(seed) => *seed,
        };
        TileSource {
            store,
            terrain,
            tag,
            mats,
            versions: RwLock::new(None),
            tiles: Mutex::new(Lru::new(cfg.cache_bytes)),
            tops: Mutex::new(Lru::new(cfg.region_tops_cache)),
        }
    }

    /// The world's extent in regions (`None`: procedural, no edge).
    pub fn extent(&self) -> Option<MapExtent> {
        match &self.terrain {
            Terrain::Forged { extent, .. } => Some(*extent),
            Terrain::Procedural(_) => None,
        }
    }

    /// Whether `key` is one a client may ask for: any sample inside the world, at most one level above
    /// its top, and on a world that wraps within one period of its columns either side (a wrapping world
    /// would otherwise accept every x: a public route's unbounded key space). Procedurally: any key up to
    /// `MAP_LEVEL_LIMIT`. Never overflows (i128).
    pub fn in_world(&self, key: MapTileKey) -> bool {
        if key.level > game::map_tile::MAP_LEVEL_LIMIT {
            return false;
        }
        let Some(e) = self.extent() else { return true };
        if key.level > game::map_tile::top_level_of(e).saturating_add(1) {
            return false;
        }
        let rb = RB as i128;
        let span = MAP_TILE_SAMPLES as i128 * (1i128 << key.level);
        let (ox, oz) = (key.x as i128 * span, key.z as i128 * span);
        let (z0, z1) = (
            e.origin.z as i128 * rb,
            (e.origin.z as i128 + e.height as i128) * rb,
        );
        let (mut x0, mut x1) = (
            e.origin.x as i128 * rb,
            (e.origin.x as i128 + e.width as i128) * rb,
        );
        if e.wraps() {
            let period = x1 - x0;
            (x0, x1) = (x0 - period, x1 + period);
        }
        oz < z1 && oz + span > z0 && ox < x1 && ox + span > x0
    }

    /// The tile of `key`, or [`Fetched::Unchanged`] when `have` is its current etag. Blocking.
    pub fn tile(&self, key: MapTileKey, have: Option<u64>) -> Result<Fetched, StoreError> {
        let versions = self.versions()?;
        let stored = self.stored_regions_of(key, &versions);
        let etag = etag_of(self.tag, key, &stored);
        if have == Some(etag) {
            return Ok(Fetched::Unchanged);
        }
        if let Some((cached, bytes)) = self.tiles.lock().expect("map tile cache").get(&key) {
            if *cached == etag {
                return Ok(Fetched::Tile {
                    etag,
                    bytes: bytes.clone(),
                });
            }
        }
        let mut tile = match &self.terrain {
            Terrain::Forged { world, .. } => {
                worldgen::surface::surface_tile_forged(world, key, &self.mats)
            }
            Terrain::Procedural(seed) => {
                worldgen::surface::surface_tile_procedural(*seed, key, &self.mats)
            }
        };
        for &(unwrapped, canonical, version) in &stored {
            let top = self.region_top(canonical, version)?;
            overlay(&mut tile, unwrapped, &top);
        }
        let bytes = Arc::new(tile.encode());
        let cost = bytes.len();
        self.tiles
            .lock()
            .expect("map tile cache")
            .put(key, (etag, bytes.clone()), cost);
        Ok(Fetched::Tile { etag, bytes })
    }

    /// Re-reads every region's `terrain_version`; call after a cut notice or on a timer. Region tops of
    /// a region whose version moved are dropped; cached tiles are re-checked against the versions at
    /// their next ask (their etag) and rebuilt then.
    pub fn refresh_versions(&self) -> Result<(), StoreError> {
        let fresh = Arc::new(self.store.stored_regions()?);
        let mut slot = self.versions.write().expect("map versions");
        if let Some(old) = slot.as_ref() {
            let moved: Vec<RegionCoords> = fresh
                .iter()
                .filter(|(rc, v)| old.get(rc) != Some(v))
                .map(|(rc, _)| *rc)
                .collect();
            if !moved.is_empty() {
                let mut tops = self.tops.lock().expect("map region tops");
                tops.retain(|(rc, _), _| !moved.contains(rc));
            }
        }
        *slot = Some(fresh);
        Ok(())
    }

    /// Bytes of encoded tiles cached.
    pub fn cache_bytes(&self) -> usize {
        self.tiles.lock().expect("map tile cache").cost
    }

    fn versions(&self) -> Result<Arc<BTreeMap<RegionCoords, u64>>, StoreError> {
        if let Some(v) = self.versions.read().expect("map versions").as_ref() {
            return Ok(v.clone());
        }
        self.refresh_versions()?;
        Ok(self
            .versions
            .read()
            .expect("map versions")
            .clone()
            .expect("just read"))
    }

    /// The stored regions under `key` (none above [`MAP_EDIT_MAX_LEVEL`]): each as the tile sees it
    /// (unwrapped), its canonical coordinates and its version.
    fn stored_regions_of(
        &self,
        key: MapTileKey,
        versions: &BTreeMap<RegionCoords, u64>,
    ) -> Vec<(RegionCoords, RegionCoords, u64)> {
        if key.level > MAP_EDIT_MAX_LEVEL || versions.is_empty() {
            return Vec::new();
        }
        let extent = self.extent();
        key.regions()
            .filter_map(|rc| {
                let canonical = game::canonical(extent, rc);
                versions.get(&canonical).map(|v| (rc, canonical, *v))
            })
            .collect()
    }

    fn region_top(&self, rc: RegionCoords, version: u64) -> Result<Arc<RegionTop>, StoreError> {
        if let Some(top) = self
            .tops
            .lock()
            .expect("map region tops")
            .get(&(rc, version))
        {
            return Ok(top.clone());
        }
        let mut top: RegionTop = vec![None; RB * RB];
        let mut read_version = version;
        if let Some((v, chunks)) = self.store.region_tops(rc)? {
            read_version = v;
            // Ascending chunk y: a higher chunk's top replaces a lower one's.
            let mut chunks = chunks;
            chunks.sort_by_key(|(c, _)| c.y);
            for (c, chunk_top) in chunks {
                let base = c.y as i64 * CHUNK_BLOCKS as i64;
                for (i, t) in chunk_top.0.iter().enumerate() {
                    let Some((vy, block)) = *t else { continue };
                    let (bx, bz) = (i % CHUNK_BLOCKS, i / CHUNK_BLOCKS);
                    let (lx, lz) = (c.x * CHUNK_BLOCKS + bx, c.z * CHUNK_BLOCKS + bz);
                    if lx >= RB || lz >= RB {
                        continue;
                    }
                    let blocks = (vy as i64 + BLOCK_VOXELS as i64 - 1) / BLOCK_VOXELS as i64;
                    let height = (base + blocks).clamp(i16::MIN as i64, i16::MAX as i64) as i16;
                    top[lx + RB * lz] = Some((height, block));
                }
            }
        }
        let top = Arc::new(top);
        self.tops
            .lock()
            .expect("map region tops")
            .put((rc, read_version), top.clone(), 1);
        Ok(top)
    }
}

/// crc32 over the world's tag, the key and, down to [`MAP_EDIT_MAX_LEVEL`], every stored region under it
/// with its version.
fn etag_of(tag: u64, key: MapTileKey, stored: &[(RegionCoords, RegionCoords, u64)]) -> u64 {
    let mut h = crc32fast::Hasher::new();
    h.update(&MAP_TILE_RENDER_VERSION.to_le_bytes());
    h.update(&tag.to_le_bytes());
    h.update(&[key.level]);
    h.update(&key.x.to_le_bytes());
    h.update(&key.z.to_le_bytes());
    let mut pairs: Vec<(RegionCoords, u64)> = stored.iter().map(|&(rc, _, v)| (rc, v)).collect();
    pairs.sort();
    if pairs.is_empty() {
        h.update(&0u64.to_le_bytes());
    }
    for (rc, v) in pairs {
        h.update(&rc.x.to_le_bytes());
        h.update(&rc.z.to_le_bytes());
        h.update(&v.to_le_bytes());
    }
    u64::from(h.finalize())
}

/// Every sample of `tile` whose centre lies in region `rc` (as the tile sees it) set to its stored top,
/// flagged `REAL`. The water's surface stays worldgen's (water is recipe data): its depth is re-taken
/// over the new top, and ice kept only where water still stands.
fn overlay(tile: &mut MapTile, rc: RegionCoords, top: &RegionTop) {
    let key = tile.key;
    let (ox, oz) = key.origin_block();
    let sm = key.sample_m();
    let rb = RB as i64;
    let (rx0, rz0) = (rc.x as i64 * rb, rc.z as i64 * rb);
    // Sample index range whose centre block `o + i·sm + sm/2` lies in [r0, r0 + rb).
    let range = |o: i64, r0: i64| {
        let first =
            (r0 - o - sm / 2).div_euclid(sm) + i64::from((r0 - o - sm / 2).rem_euclid(sm) != 0);
        let last = (r0 + rb - 1 - o - sm / 2).div_euclid(sm);
        (first.max(0), last.min(EDGE as i64 - 1))
    };
    let ((sx0, sx1), (sz0, sz1)) = (range(ox, rx0), range(oz, rz0));
    for sz in sz0..=sz1 {
        for sx in sx0..=sx1 {
            let (bx, bz) = (ox + sx * sm + sm / 2, oz + sz * sm + sm / 2);
            let Some((height, block)) = top[(bx - rx0) as usize + RB * (bz - rz0) as usize] else {
                continue;
            };
            let s = &mut tile.samples[sx as usize + EDGE * sz as usize];
            let surface = s.height as i32 + s.water as i32;
            s.water = (surface - height as i32).clamp(0, u8::MAX as i32) as u8;
            if s.water == 0 {
                s.flags &= !sample_flags::ICE;
            }
            s.height = height;
            s.block = block;
            s.flags |= sample_flags::REAL;
        }
    }
}

/// A least-recently-used map bounded by a total cost.
struct Lru<K, V> {
    map: HashMap<K, (V, u64, usize)>,
    order: BTreeMap<u64, K>,
    stamp: u64,
    cost: usize,
    max: usize,
}

impl<K: Clone + Eq + Hash, V> Lru<K, V> {
    fn new(max: usize) -> Self {
        Lru {
            map: HashMap::new(),
            order: BTreeMap::new(),
            stamp: 0,
            cost: 0,
            max,
        }
    }

    fn get(&mut self, k: &K) -> Option<&V> {
        self.stamp += 1;
        let stamp = self.stamp;
        let entry = self.map.get_mut(k)?;
        self.order.remove(&entry.1);
        entry.1 = stamp;
        self.order.insert(stamp, k.clone());
        Some(&entry.0)
    }

    fn put(&mut self, k: K, v: V, cost: usize) {
        self.stamp += 1;
        if let Some((_, old_stamp, old_cost)) = self.map.remove(&k) {
            self.order.remove(&old_stamp);
            self.cost -= old_cost;
        }
        self.map.insert(k.clone(), (v, self.stamp, cost));
        self.order.insert(self.stamp, k);
        self.cost += cost;
        while self.cost > self.max {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, _, c)) = self.map.remove(&oldest) {
                self.cost -= c;
            }
        }
    }

    fn retain(&mut self, keep: impl Fn(&K, &V) -> bool) {
        let gone: Vec<K> = self
            .map
            .iter()
            .filter(|(k, (v, _, _))| !keep(k, v))
            .map(|(k, _)| k.clone())
            .collect();
        for k in gone {
            if let Some((_, stamp, c)) = self.map.remove(&k) {
                self.order.remove(&stamp);
                self.cost -= c;
            }
        }
    }
}
