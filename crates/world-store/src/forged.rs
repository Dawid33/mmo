//! The forged world in the store as tiles (forged-world-in-db plan, Task 4):
//! `forged_meta` (the [`ForgedMeta`] row), `forged_tiles` (one row per
//! [`TILE_CELLS`]² tile, `worldgen::forged_tiles::encode_tile`),
//! `forged_world_map` (the `WorldMap`, built once at write time) and
//! `forged_archive` (the original forge file, never read by hosts — kept for
//! re-tiling and export).
//!
//! [`write_tiled`] writes all of it and `world.content_crc` in one
//! transaction; [`load_forged`] builds a tiled [`ForgedWorld`] whose tiles
//! come from the store through [`StoreTiles`] as sampling needs them.

use std::sync::Arc;

use game::WorldMap;
use rayon::prelude::*;
use worldgen::forged::ForgedWorld;
use worldgen::forged_tiles::{
    ForgedMeta, TILE_CELLS, TILED_FORMAT_VERSION, TileCoords, TileLoader, decode_meta, encode_meta, encode_tile,
    split,
};

use crate::codec::{FORMAT_WORLD_MAP, decode, encode};
use crate::{FormatKind, StoreError, StoreHandle};

/// Everything [`write_tiled`] stores, encoded, for the one transaction.
pub(crate) struct TiledRows {
    pub(crate) format_version: u32,
    pub(crate) meta: Vec<u8>,
    /// In `(tz, tx)` order.
    pub(crate) tiles: Vec<(TileCoords, Vec<u8>)>,
    pub(crate) world_map: Vec<u8>,
    pub(crate) archive: Vec<u8>,
    pub(crate) content_crc: u32,
}

/// The `forged_meta` row, read with the world's content crc and whether the
/// archive row exists.
pub(crate) struct ForgedHead {
    pub(crate) format_version: u32,
    pub(crate) meta: Vec<u8>,
    pub(crate) content_crc: u32,
    pub(crate) archived: bool,
}

/// Decision 8: crc32 over the meta row's bytes, then every tile row's bytes
/// in `(tz, tx)` order, then the world-map row's bytes.
fn content_crc(meta: &[u8], tiles: &[(TileCoords, Vec<u8>)], world_map: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(meta);
    for (_, bytes) in tiles {
        h.update(bytes);
    }
    h.update(world_map);
    h.finalize()
}

/// Stores the forge file `artifact_bytes` as a tiled world, replacing any
/// stored one, in ONE transaction: the meta, the tiles, the world map
/// (`tile_map::build_world_map` of the whole world, at this build's region
/// size), the file itself as the archive, and `world.content_crc` — which it
/// returns. The whole world is decoded on the calling thread (tiles are
/// encoded on rayon's pool) and freed before the write.
///
/// Panics if the world's cells do not tile into whole river buckets
/// (`split`); every forge cell size (8 / 16 / 32 m and up) does.
pub fn write_tiled(handle: &StoreHandle, artifact_bytes: Vec<u8>) -> Result<u32, StoreError> {
    let whole = ForgedWorld::from_bytes(&artifact_bytes)
        .map_err(|e| StoreError::Corrupt(format!("the forged world to store does not load: {e}")))?;
    let (meta, tiles) = split(&whole, TILE_CELLS);
    let world_map = encode(FORMAT_WORLD_MAP, &worldgen::tile_map::build_world_map(&whole));
    drop(whole);
    let meta_bytes = encode_meta(&meta);
    let mut tiles: Vec<(TileCoords, Vec<u8>)> =
        tiles.into_par_iter().map(|t| (t.origin, encode_tile(&t))).collect();
    tiles.sort_by_key(|(t, _)| *t);
    let content_crc = content_crc(&meta_bytes, &tiles, &world_map);
    handle.put_tiled(Box::new(TiledRows {
        format_version: meta.format_version,
        meta: meta_bytes,
        tiles,
        world_map,
        archive: artifact_bytes,
        content_crc,
    }))?;
    Ok(content_crc)
}

/// The stored tiled world and its world map, or `None` if none is stored. A
/// tile format this build does not read is `OtherFormat` (naming `just
/// worldctl retile` when the archive can be re-tiled, else re-forge). Tiles
/// are loaded later, as sampling or prefetch needs them, through
/// [`StoreTiles`] on a clone of `handle`.
pub fn load_forged(handle: &StoreHandle) -> Result<Option<(ForgedWorld, WorldMap)>, StoreError> {
    let Some(head) = handle.forged_head()? else { return Ok(None) };
    if head.format_version != TILED_FORMAT_VERSION {
        return Err(StoreError::OtherFormat {
            found: head.format_version,
            kind: FormatKind::Tiles { archived: head.archived },
        });
    }
    let meta: ForgedMeta =
        decode_meta(&head.meta).map_err(|e| StoreError::Corrupt(format!("forged_meta: {e}")))?;
    if meta.format_version != head.format_version {
        return Err(StoreError::Corrupt(format!(
            "forged_meta: the row says format {} but its bytes say {}",
            head.format_version, meta.format_version
        )));
    }
    let map = handle
        .forged_world_map()?
        .ok_or_else(|| StoreError::Corrupt("a tiled forged world without its forged_world_map row".into()))?;
    let map: WorldMap = decode(FORMAT_WORLD_MAP, &map)?;
    let world = ForgedWorld::from_tiles(meta, Arc::new(StoreTiles(handle.clone())), head.content_crc)
        .map_err(|e| StoreError::Corrupt(format!("forged_meta: {e}")))?;
    Ok(Some((world, map)))
}

/// The store as a [`TileLoader`]: one `forged_tiles` row per load, blocking
/// on the `world-store` thread's answer. Call it from region / LOD workers or
/// any plain thread. On the `world-store` thread itself it returns an error
/// instead of waiting for its own answer (`StoreHandle` refuses blocking calls
/// there); on a tokio worker it blocks that worker for the read, like any
/// blocking `StoreHandle` call.
#[derive(Clone)]
pub struct StoreTiles(pub StoreHandle);

impl TileLoader for StoreTiles {
    fn load(&self, t: TileCoords) -> Result<Vec<u8>, String> {
        match self.0.forged_tile(t) {
            Ok(Some(bytes)) => Ok(bytes),
            Ok(None) => Err(format!("the store holds no forged tile ({}, {})", t.tx, t.tz)),
            Err(e) => Err(e.to_string()),
        }
    }
}
