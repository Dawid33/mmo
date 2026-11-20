//! The client's map tiles (spec §5.3): where they come from ([`MapSource`]), the memory LRU of decoded
//! tiles and their GPU textures ([`MapTiles`]), and the fetch loop that keeps the views' wanted tiles
//! loaded and revalidated.
//!
//! **Reset rule:** the cache belongs to the WORLD. A session end only pauses it (`active = false`, the
//! fetches in flight dropped); the next session's [`MapWorld`] resumes it when its id is the same and
//! clears it when it is another world. The id of a local world is the crc32 of its folder's path and
//! its `world.json` (a re-forge in the same folder writes a new `world.json`).

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
#[cfg(not(target_arch = "wasm32"))]
use bevy::tasks::AsyncComputeTaskPool;
use bevy::tasks::{block_on, futures_lite::future, Task};
use game::map_tile::{sample_flags, MapTile, MapTileKey, MAP_TILE_SAMPLES};

use crate::session::SessionResetExt;

/// Tile fetches in flight at once.
pub const MAP_FETCH_IN_FLIGHT: usize = 6;
/// How often a shown tile with real regions is asked again (with its etag), and the source's terrain
/// versions re-read.
pub const MAP_REVALIDATE_MS: u64 = 30_000;
/// Bytes of decoded tiles and their textures kept (the oldest-used go first; wanted tiles never).
pub const MAP_TILE_CACHE_BYTES: usize = 128 << 20;
/// How long a failed fetch (or a source that would not open) waits before it is tried again.
pub const MAP_RETRY_MS: u64 = 2_000;
/// The `[map]` counters line's period.
pub const MAP_LOG_EVERY_MS: u64 = 10_000;

const SAMPLES: usize = (MAP_TILE_SAMPLES * MAP_TILE_SAMPLES) as usize;
/// Bytes one loaded tile holds: the decoded samples and the three textures.
const TILE_BYTES: usize = SAMPLES * (std::mem::size_of::<game::map_tile::MapSample>() + 6);

/// Where tiles come from.
#[derive(Clone, Default)]
pub enum MapSource {
    /// The world's own store and forged world, read in this process (single-player, native only).
    #[cfg(not(target_arch = "wasm32"))]
    Local(Arc<map_tiles::TileSource>),
    /// A map route (`{base}/v1/map/tiles/{level}/{x}/{z}`): multiplayer and the dev server (Task 11).
    Http { base: String },
    #[default]
    None,
}

/// The world the current session plays, as the map knows it: set when the session starts, removed
/// when it ends.
#[derive(Resource, Clone, Debug)]
pub struct MapWorld {
    /// Stable per world (see the module notes).
    pub id: u64,
    pub how: MapWorldSource,
}

#[derive(Clone, Debug)]
pub enum MapWorldSource {
    /// A local world: its store file (`terrain.sqlite`), opened read-only beside the child server.
    #[cfg(not(target_arch = "wasm32"))]
    Local { store: std::path::PathBuf },
    /// A map route's base URL.
    Http { base: String },
}

impl MapWorld {
    /// The local world in `entry_dir` whose store is `store`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn local(entry_dir: &std::path::Path, store: &std::path::Path) -> MapWorld {
        let mut h = crc32fast::Hasher::new();
        h.update(entry_dir.to_string_lossy().as_bytes());
        h.update(&std::fs::read(entry_dir.join("world.json")).unwrap_or_default());
        MapWorld { id: u64::from(h.finalize()), how: MapWorldSource::Local { store: store.to_path_buf() } }
    }
}

/// One tile's three textures (integer formats, sampled by `textureLoad`): height `R16Sint` (m),
/// block `R16Uint` (`BlockId`), water and flags `Rg8Uint`.
#[derive(Clone, Debug)]
pub struct GpuTile {
    pub height: Handle<Image>,
    pub block: Handle<Image>,
    pub water_flags: Handle<Image>,
}

struct Entry {
    tile: MapTile,
    gpu: GpuTile,
    etag: u64,
    /// Seconds (`Time::elapsed`) of its last answer.
    fetched_at: f64,
    /// Any sample from stored terrain: revalidated every `MAP_REVALIDATE_MS`.
    real: bool,
    /// Painted locally from loaded chunks (Task 9): replaced only by a served tile whose etag changed.
    pub painted: bool,
    used: u64,
}

/// A fetch's answer, decoded on the worker. (wasm builds none until its HTTP fetch, Task 11.)
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Answer {
    Unchanged,
    Tile { etag: u64, tile: MapTile },
    Failed(String),
}

#[derive(Resource, Default)]
pub struct MapTiles {
    world: Option<u64>,
    /// False between a session's end and the next session's [`MapWorld`]: nothing is shown or fetched.
    active: bool,
    source: MapSource,
    entries: HashMap<MapTileKey, Entry>,
    bytes: usize,
    stamp: u64,
    /// This frame's wanted keys, in the order asked (each view asks nearest its centre first).
    wanted: Vec<MapTileKey>,
    in_flight: HashMap<MapTileKey, Task<Answer>>,
    retry_at: HashMap<MapTileKey, f64>,
    /// The source being opened (local), or the time to try again.
    #[cfg(not(target_arch = "wasm32"))]
    opening: Option<Task<Result<Arc<map_tiles::TileSource>, String>>>,
    #[cfg(not(target_arch = "wasm32"))]
    open_retry_at: f64,
    refresh: Option<Task<Result<(), String>>>,
    refreshed_at: f64,
    fetches: u64,
    failures: u64,
    logged_at: f64,
}

impl MapTiles {
    /// Ask for `keys` this frame (call every frame a view shows; nearest its centre first).
    pub fn want(&mut self, keys: impl IntoIterator<Item = MapTileKey>) {
        if self.active {
            self.wanted.extend(keys);
        }
    }

    /// `key`'s textures, or the nearest loaded ancestor's (its key says which sub-rectangle to use).
    pub fn best(&self, key: MapTileKey) -> Option<(MapTileKey, &GpuTile)> {
        if !self.active {
            return None;
        }
        let mut k = Some(key);
        while let Some(at) = k {
            if let Some(e) = self.entries.get(&at) {
                return Some((at, &e.gpu));
            }
            k = at.parent();
        }
        None
    }

    /// A loaded tile's samples.
    pub fn tile(&self, key: MapTileKey) -> Option<&MapTile> {
        self.active.then(|| self.entries.get(&key).map(|e| &e.tile)).flatten()
    }

    /// The source tiles come from.
    pub fn source(&self) -> &MapSource {
        &self.source
    }

    /// Tiles loaded.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Everything of the previous world goes.
    fn clear(&mut self) {
        *self = MapTiles { active: self.active, logged_at: self.logged_at, ..MapTiles::default() };
    }

    fn insert(&mut self, key: MapTileKey, entry: Entry) {
        if let Some(old) = self.entries.insert(key, entry) {
            drop(old);
        } else {
            self.bytes += TILE_BYTES;
        }
    }

    /// Drop the oldest-used tiles above the cap, never one wanted this frame.
    fn evict(&mut self, images: &mut Assets<Image>) {
        if self.bytes <= MAP_TILE_CACHE_BYTES {
            return;
        }
        let mut by_age: Vec<(u64, MapTileKey)> = self.entries.iter().map(|(k, e)| (e.used, *k)).collect();
        by_age.sort();
        for (_, key) in by_age {
            if self.bytes <= MAP_TILE_CACHE_BYTES {
                break;
            }
            if self.wanted.contains(&key) {
                continue;
            }
            if let Some(e) = self.entries.remove(&key) {
                images.remove(&e.gpu.height);
                images.remove(&e.gpu.block);
                images.remove(&e.gpu.water_flags);
                self.bytes -= TILE_BYTES;
            }
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<MapTiles>()
        .add_session_reset_with(pause_map_tiles)
        .add_systems(Update, (follow_map_world, fetch_tiles).chain());
}

/// Session end: pause the cache (it belongs to the world, not the session) and drop what is in flight.
fn pause_map_tiles(world: &mut World) {
    world.remove_resource::<MapWorld>();
    if let Some(mut tiles) = world.get_resource_mut::<MapTiles>() {
        tiles.active = false;
        tiles.wanted.clear();
        tiles.in_flight.clear();
        tiles.refresh = None;
    }
}

/// A session's [`MapWorld`]: resume the same world, clear for another, and open its source.
#[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
fn follow_map_world(world: Option<Res<MapWorld>>, mut tiles: ResMut<MapTiles>, images: ResMut<Assets<Image>>, time: Res<Time>) {
    let Some(world) = world else { return };
    if tiles.world != Some(world.id) {
        let mut images = images;
        for e in tiles.entries.values() {
            images.remove(&e.gpu.height);
            images.remove(&e.gpu.block);
            images.remove(&e.gpu.water_flags);
        }
        let n = tiles.entries.len();
        tiles.clear();
        tiles.world = Some(world.id);
        info!("[map] world {:016x}: tile cache cleared ({n} tiles of the previous world)", world.id);
    }
    tiles.active = true;
    match &world.how {
        #[cfg(not(target_arch = "wasm32"))]
        MapWorldSource::Local { store } => {
            let now = time.elapsed_secs_f64();
            if matches!(tiles.source, MapSource::Local(_)) {
                return;
            }
            if let Some(task) = tiles.opening.as_mut() {
                let Some(opened) = block_on(future::poll_once(task)) else { return };
                tiles.opening = None;
                match opened {
                    Ok(source) => {
                        info!("[map] local source open: {}", store.display());
                        tiles.source = MapSource::Local(source);
                    }
                    Err(e) => {
                        // A store the child server has not migrated yet is refused read-only: try again.
                        debug!("[map] local source not open yet ({}): {e}", store.display());
                        tiles.open_retry_at = now + MAP_RETRY_MS as f64 / 1000.0;
                    }
                }
                return;
            }
            if now >= tiles.open_retry_at {
                let store = store.clone();
                tiles.opening = Some(AsyncComputeTaskPool::get().spawn(async move { open_local(&store) }));
            }
        }
        MapWorldSource::Http { base } => {
            if !matches!(&tiles.source, MapSource::Http { base: b } if b == base) {
                tiles.source = MapSource::Http { base: base.clone() };
            }
        }
    }
}

/// The world's store, read-only, and its forged world: a [`map_tiles::TileSource`]. Blocking.
#[cfg(not(target_arch = "wasm32"))]
fn open_local(store: &std::path::Path) -> Result<Arc<map_tiles::TileSource>, String> {
    use world_store::{Dials, OpenMode, StoreHandle, StoreUrl};
    if !store.exists() {
        return Err("no store file yet".into());
    }
    let dials = Dials { region_blocks: game::REGION_BLOCKS as u32, chunk_blocks: game::CHUNK_BLOCKS as u32 };
    let handle = StoreHandle::open(&StoreUrl::Sqlite(store.to_path_buf()), dials, OpenMode::ReadOnly).map_err(|e| e.to_string())?;
    let (world, _map) = world_store::load_forged(&handle).map_err(|e| e.to_string())?.ok_or("the store holds no forged world")?;
    let registry = crate::blocks::load_registry();
    let mats = worldgen::material::Materials::from_names(|name| registry.id_of(name).unwrap_or(game::BlockId::AIR));
    Ok(Arc::new(map_tiles::TileSource::forged(handle, Arc::new(world), mats, map_tiles::TileSourceConfig::CLIENT)))
}

/// One fetch on a worker: the source's answer, decoded.
#[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
fn spawn_fetch(source: &MapSource, key: MapTileKey, have: Option<u64>) -> Option<Task<Answer>> {
    match source {
        #[cfg(not(target_arch = "wasm32"))]
        MapSource::Local(src) => {
            let src = src.clone();
            Some(AsyncComputeTaskPool::get().spawn(async move {
                match src.tile(key, have) {
                    Ok(map_tiles::Fetched::Unchanged) => Answer::Unchanged,
                    Ok(map_tiles::Fetched::Tile { etag, bytes }) => match MapTile::decode(&bytes) {
                        Ok(tile) => Answer::Tile { etag, tile },
                        Err(e) => Answer::Failed(e.to_string()),
                    },
                    Err(e) => Answer::Failed(e.to_string()),
                }
            }))
        }
        // Task 11.
        MapSource::Http { .. } | MapSource::None => None,
    }
}

fn spawn_refresh(source: &MapSource) -> Option<Task<Result<(), String>>> {
    match source {
        #[cfg(not(target_arch = "wasm32"))]
        MapSource::Local(src) => {
            let src = src.clone();
            Some(AsyncComputeTaskPool::get().spawn(async move { src.refresh_versions().map_err(|e| e.to_string()) }))
        }
        MapSource::Http { .. } | MapSource::None => None,
    }
}

/// The three textures of `tile`.
fn upload(images: &mut Assets<Image>, tile: &MapTile) -> GpuTile {
    let n = MAP_TILE_SAMPLES;
    let image = |data: Vec<u8>, format: TextureFormat| {
        let mut image = Image::new(
            Extent3d { width: n, height: n, depth_or_array_layers: 1 },
            TextureDimension::D2,
            data,
            format,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::nearest());
        image
    };
    let height = tile.samples.iter().flat_map(|s| s.height.to_le_bytes()).collect();
    let block = tile.samples.iter().flat_map(|s| s.block.0.to_le_bytes()).collect();
    let water_flags = tile.samples.iter().flat_map(|s| [s.water, s.flags]).collect();
    GpuTile {
        height: images.add(image(height, TextureFormat::R16Sint)),
        block: images.add(image(block, TextureFormat::R16Uint)),
        water_flags: images.add(image(water_flags, TextureFormat::Rg8Uint)),
    }
}

/// The fetch loop: answers in, new and stale wanted tiles out (at most `MAP_FETCH_IN_FLIGHT`), the
/// source's versions re-read every `MAP_REVALIDATE_MS`, the cache kept under its cap.
fn fetch_tiles(mut tiles: ResMut<MapTiles>, mut images: ResMut<Assets<Image>>, time: Res<Time>) {
    let now = time.elapsed_secs_f64();
    let tiles = &mut *tiles;
    // Answers.
    let mut done = Vec::new();
    for (key, task) in tiles.in_flight.iter_mut() {
        if let Some(answer) = block_on(future::poll_once(task)) {
            done.push((*key, answer));
        }
    }
    for (key, answer) in done {
        tiles.in_flight.remove(&key);
        match answer {
            Answer::Unchanged => {
                if let Some(e) = tiles.entries.get_mut(&key) {
                    e.fetched_at = now;
                }
            }
            Answer::Tile { etag, tile } => {
                if let Some(old) = tiles.entries.get(&key) {
                    if old.etag == etag && old.painted {
                        continue; // painted since: the served tile has not moved past it
                    }
                }
                let gpu = upload(&mut images, &tile);
                if let Some(old) = tiles.entries.get(&key) {
                    images.remove(&old.gpu.height);
                    images.remove(&old.gpu.block);
                    images.remove(&old.gpu.water_flags);
                }
                let real = tile.samples.iter().any(|s| s.flags & sample_flags::REAL != 0);
                tiles.stamp += 1;
                let used = tiles.stamp;
                tiles.insert(key, Entry { tile, gpu, etag, fetched_at: now, real, painted: false, used });
            }
            Answer::Failed(e) => {
                tiles.failures += 1;
                if tiles.failures.is_power_of_two() {
                    warn!("[map] tile {key:?}: {e} ({} failures)", tiles.failures);
                }
                tiles.retry_at.insert(key, now + MAP_RETRY_MS as f64 / 1000.0);
            }
        }
    }
    if let Some(task) = tiles.refresh.as_mut() {
        if let Some(result) = block_on(future::poll_once(task)) {
            tiles.refresh = None;
            if let Err(e) = result {
                warn!("[map] re-reading terrain versions: {e}");
            }
        }
    }
    if !tiles.active {
        tiles.wanted.clear();
        return;
    }
    if tiles.refresh.is_none() && now - tiles.refreshed_at >= MAP_REVALIDATE_MS as f64 / 1000.0 {
        tiles.refreshed_at = now;
        tiles.refresh = spawn_refresh(&tiles.source);
    }
    // Requests, in the order wanted.
    let wanted = std::mem::take(&mut tiles.wanted);
    for &key in &wanted {
        tiles.stamp += 1;
        let stamp = tiles.stamp;
        let have = match tiles.entries.get_mut(&key) {
            Some(e) => {
                e.used = stamp;
                let stale = e.real && now - e.fetched_at >= MAP_REVALIDATE_MS as f64 / 1000.0;
                if !stale {
                    continue;
                }
                Some(e.etag)
            }
            None => None,
        };
        if tiles.in_flight.len() >= MAP_FETCH_IN_FLIGHT || tiles.in_flight.contains_key(&key) {
            continue;
        }
        if tiles.retry_at.get(&key).is_some_and(|&t| now < t) {
            continue;
        }
        if let Some(task) = spawn_fetch(&tiles.source, key, have) {
            tiles.fetches += 1;
            tiles.in_flight.insert(key, task);
        }
    }
    tiles.wanted = wanted;
    tiles.evict(&mut images);
    tiles.wanted.clear();
    if now - tiles.logged_at >= MAP_LOG_EVERY_MS as f64 / 1000.0 {
        tiles.logged_at = now;
        info!("[map] tiles={} bytes={} fetches={} failures={}", tiles.entries.len(), tiles.bytes, tiles.fetches, tiles.failures);
    }
}
