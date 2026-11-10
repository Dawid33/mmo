//! The tile mirror: the home region's tiles on the main thread — blocks
//! per chunk, the underground ground recipes, designations and constructed
//! tiles — behind `game::TileSource`, so the fortress tools (hover pick,
//! validity preview, overlay) answer synchronously with the sim's own rules.
//!
//! Chunk blocks come from the bridge's `SimChunk` block mirror (like the
//! height grid, but keyed region-locally: no world offset), keyed by sim
//! chunk; an edit updates only its blocks' tiles (`ChunkEdits`). The rest arrives as
//! [`TileMirrorMsg`]s from the bridge: a `Reset` per received region snapshot,
//! then the sim's `SetDesignations` / `SetConstructed` updates (do and undo
//! alike are plain writes). The non-chunk state is kept for every loaded
//! region, so a home flip switches without a re-send; chunks only for home.
//! Cell mode has no ground semantics and no fortress UI: the mirror stays
//! empty there.
//!
//! **Stud terrain.** A region whose terrain is studs ([`TileMirror::terrain`],
//! from its snapshot's `StudWorldMsg`) answers `game::tiles`' way instead: per
//! mirrored stud chunk a table of [`game::StudTile`]s (the sim's own
//! `stud_chunk_tile_table`, rebuilt when the chunk changes), else the stud
//! recipe's implied ground, own region ∪ stud halo — the read rule of
//! `Rollback::stud_tile`. Tiles are 1.2 m tall there: every tile ↔ unit
//! conversion goes through [`TileMirror::terrain`] ([`TileMirror::tile_ray`],
//! `level.rs`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bevy::prelude::*;
use game::rapier3d::math::Vector;
use game::{
    Block, BlockId, BlockIndex, Chunk, ColumnCoords, ColumnGround, Designation, RegionId, SlopeShape, StudChunk,
    StudColumnGround, StudTile, TerrainKind, TileFill, TilePos, TileSource, WorldChunkPos, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT,
    REGION_BLOCKS, REGION_CHUNKS, STUD_CHUNK_STUDS, STUD_CHUNK_TILES, STUD_LAYER_TILES, TILE_PLATES,
    TILE_STUDS,
};

use crate::renderer::bricks::{ChunkEdits, SimChunk};
use crate::renderer::bridge::{RegionRoots, SimEntity};
use crate::renderer::hud::HudStatus;
use crate::renderer::stud::world::{StudGround, StudWorldMsg};
use crate::renderer::stud::SimStudChunk;

/// Tile state from the bridge, per region.
#[derive(Message, Debug, Clone)]
pub enum TileMirrorMsg {
    /// A region snapshot arrived: its whole non-chunk tile state.
    Reset {
        region: RegionId,
        ground: BTreeMap<ColumnCoords, ColumnGround>,
        designations: Vec<(TilePos, Designation)>,
        constructed: Vec<TilePos>,
    },
    /// Per tile: the new designation (`None` = cleared), applied in order.
    Designations { region: RegionId, changes: Vec<(TilePos, Option<Designation>)> },
    /// Per tile: whether it is now a construction, applied in order.
    Constructed { region: RegionId, changes: Vec<(TilePos, bool)> },
    /// `region`'s terrain halo (`game::halo`): chunks and ground recipes set
    /// (`Some`) or gone (`None`), by world chunk — from its snapshot after
    /// the `Reset`, then from `SetHaloTerrain` / `SetHaloGround` updates.
    Halo {
        region: RegionId,
        chunks: Vec<(game::WorldChunkPos, Option<Chunk>)>,
        ground: Vec<(game::WorldChunkPos, Option<ColumnGround>)>,
    },
}

impl TileMirrorMsg {
    /// The halo `data` holds (a region's snapshot), as one [`Self::Halo`].
    pub fn halo_of(region: RegionId, data: &game::GameData) -> Self {
        TileMirrorMsg::Halo {
            region,
            chunks: data.halo_terrain().iter().map(|(k, h)| (*k, h.chunk.clone())).collect(),
            ground: data.halo_grounds().iter().map(|(k, g)| (*k, Some(g.clone()))).collect(),
        }
    }

    /// The tile-mirror message a region's render update carries, if any.
    pub fn from_update(region: RegionId, kind: &game::GameDataUpdateKind) -> Option<Self> {
        use game::GameDataUpdateKind as K;
        Some(match kind {
            K::SetDesignations(changes) => TileMirrorMsg::Designations { region, changes: changes.clone() },
            K::SetConstructed(changes) => TileMirrorMsg::Constructed { region, changes: changes.clone() },
            K::SetHaloTerrain(chunks) => TileMirrorMsg::Halo { region, chunks: chunks.clone(), ground: Vec::new() },
            K::SetHaloGround(ground) => TileMirrorMsg::Halo { region, chunks: Vec::new(), ground: ground.clone() },
            _ => return None,
        })
    }
}

/// One mirrored halo chunk: its blocks and slope shapes.
#[derive(Debug, Clone, Default)]
struct HaloTiles {
    blocks: Box<[Block]>,
    slopes: BTreeMap<u16, SlopeShape>,
}

/// One mirrored stud chunk as tiles: the sim's tile table
/// (`game::stud_chunk_tile_table`) and, per tile holding tree wood, the tree
/// (`Rollback::stud_tree_at`'s rule: the first wood brick in chunk order).
#[derive(Debug, Clone, PartialEq)]
struct StudChunkTiles {
    tiles: Box<[StudTile]>,
    trees: BTreeMap<u16, u16>,
}

impl StudChunkTiles {
    fn of(chunk: &StudChunk) -> Self {
        let mut trees = BTreeMap::new();
        for b in chunk.bricks().filter(|b| b.data != 0 && b.material == game::MAT_WOOD) {
            let size = b.size();
            let first = [b.min[0] as usize / TILE_STUDS, b.min[1] as usize / TILE_PLATES, b.min[2] as usize / TILE_STUDS];
            let last = [
                (b.min[0] as usize + size[0] as usize - 1) / TILE_STUDS,
                (b.min[1] as usize + size[1] as usize - 1) / TILE_PLATES,
                (b.min[2] as usize + size[2] as usize - 1) / TILE_STUDS,
            ];
            for ty in first[1]..=last[1] {
                for tz in first[2]..=last[2] {
                    for tx in first[0]..=last[0] {
                        trees.entry(game::stud_tile_table_index([tx, ty, tz]) as u16).or_insert(b.data);
                    }
                }
            }
        }
        Self { tiles: game::stud_chunk_tile_table(chunk), trees }
    }
}

const _: () = assert!(STUD_CHUNK_TILES * STUD_LAYER_TILES * STUD_CHUNK_TILES <= u16::MAX as usize);

/// One region's non-chunk tile state.
#[derive(Debug, Clone, Default)]
struct RegionTiles {
    /// What the region's terrain is made of (voxel until its `StudWorldMsg` says studs).
    terrain: TerrainKind,
    /// Stud terrain: the ground recipes, by chunk column.
    stud_ground: StudGround,
    /// Stud terrain: the stud halo's chunks as tiles, by world chunk (`y` = layer).
    stud_halo: BTreeMap<WorldChunkPos, StudChunkTiles>,
    /// Stud terrain: the stud halo's recipes, by world chunk column (`y` 0).
    stud_halo_ground: BTreeMap<WorldChunkPos, StudColumnGround>,
    ground: BTreeMap<ColumnCoords, ColumnGround>,
    designations: BTreeMap<TilePos, Designation>,
    constructed: BTreeSet<TilePos>,
    /// The region's terrain halo: its neighbours' chunks along its edges.
    halo: BTreeMap<game::WorldChunkPos, HaloTiles>,
    /// The halo's ground recipes, by world chunk column (`y` 0).
    halo_ground: BTreeMap<game::WorldChunkPos, ColumnGround>,
}

static NO_DESIGNATIONS: BTreeMap<TilePos, Designation> = BTreeMap::new();

#[derive(Resource, Default)]
pub struct TileMirror {
    home: Option<RegionId>,
    /// Every loaded region's state; only `home`'s is read.
    regions: BTreeMap<RegionId, RegionTiles>,
    /// Home region chunks (region-local chunk coords) → blocks (id + data,
    /// for the tree id) by `BlockIndex`, with the bridge entity that supplied them
    /// (`Entity::PLACEHOLDER` for [`TileMirror::set_chunk`]).
    chunks: HashMap<[i32; 3], (Entity, Box<[Block]>)>,
    chunk_of: HashMap<Entity, [i32; 3]>,
    /// Per home chunk: the slope blocks (by `BlockIndex::linear`), classified
    /// from the chunk's chisels when it is mirrored.
    slopes: HashMap<[i32; 3], BTreeMap<u16, SlopeShape>>,
    /// Home region stud chunks (region-local `[x, layer, z]`) as tiles, with the
    /// bridge entity that supplied them.
    stud_chunks: HashMap<[i32; 3], (Entity, StudChunkTiles)>,
    stud_chunk_of: HashMap<Entity, [i32; 3]>,
    version: u64,
    /// Bumped whenever the home region's ground recipe may have appeared
    /// (its `Reset`, a home switch): bricks meshed without it re-mesh then
    /// (`meshing::queue::remesh_awaiting_ground`).
    ground_epoch: u64,
    /// Tiles (blocks) rewritten by incremental updates, for tests/diagnostics.
    tiles_touched: u64,
}

impl TileMirror {
    /// The mirrored (home) region.
    pub fn region(&self) -> Option<RegionId> {
        self.home
    }

    pub fn designations(&self) -> &BTreeMap<TilePos, Designation> {
        self.home_tiles().map_or(&NO_DESIGNATIONS, |t| &t.designations)
    }

    /// What the mirrored (home) region's terrain is made of.
    pub fn terrain(&self) -> TerrainKind {
        self.home_tiles().map_or(TerrainKind::Voxel, |t| t.terrain)
    }

    /// Whether tile `p` stops a pick ray: a block that blocks rays on voxel
    /// terrain, any tile with something in it on studs.
    pub fn pick_solid(&self, p: TilePos) -> bool {
        match self.terrain() {
            TerrainKind::Voxel => game::block_props().blocks_ray(self.block(p)),
            TerrainKind::Stud => self.stud_tile(p).fill() != TileFill::Open,
        }
    }

    /// `ray` (region-local sim units, direction normalised) in TILE space: the
    /// frame in which every tile of the home terrain is a `BLOCK_VOXELS` cube,
    /// which is what `game::pick_level_cell` / `pick_visible_cell` /
    /// `raycast_blocks` walk. The identity on voxel terrain; on studs `y` is
    /// scaled by block height over tile height (tiles are as wide as blocks).
    pub fn tile_ray(&self, ray: (Vector, Vector)) -> (Vector, Vector) {
        tile_ray(self.terrain(), ray)
    }

    /// The home stud tile at `p` (region-local; it may lie past the region's
    /// edge), by `Rollback::stud_tile`'s rule: the materialised chunk, else the
    /// recipe's implied ground; past the edge the stud halo's chunk, else the
    /// neighbour's recipe. Unknown reads as open.
    pub fn stud_tile(&self, p: TilePos) -> StudTile {
        let Some((chunk, local)) = game::split_stud_tile(p) else { return StudTile::default() };
        let Some(tiles) = self.home_tiles() else { return StudTile::default() };
        let n = REGION_CHUNKS as i32;
        let [cx, layer, cz] = chunk;
        if (0..n).contains(&cx) && (0..n).contains(&cz) {
            if let Some((_, c)) = self.stud_chunks.get(&chunk) {
                return c.tiles[game::stud_tile_table_index(local)];
            }
            return match tiles.stud_ground.get(&ColumnCoords::new(cx as usize, cz as usize)) {
                Some(g) => StudTile::of_ground(g, layer, local),
                None => StudTile::default(),
            };
        }
        let (Some(home), true) = (self.home, game::halo::in_halo_band(cx, cz)) else { return StudTile::default() };
        let key = WorldChunkPos::from_local(home, chunk);
        if let Some(c) = tiles.stud_halo.get(&key) {
            return c.tiles[game::stud_tile_table_index(local)];
        }
        match tiles.stud_halo_ground.get(&WorldChunkPos::new(key.x, 0, key.z)) {
            Some(g) => StudTile::of_ground(g, layer, local),
            None => StudTile::default(),
        }
    }

    /// `Rollback::stud_tree_at`: the tree whose wood is in home stud tile `p`.
    fn stud_tree_at(&self, p: TilePos) -> Option<u16> {
        let (chunk, local) = game::split_stud_tile(p)?;
        self.stud_chunks.get(&chunk)?.1.trees.get(&(game::stud_tile_table_index(local) as u16)).copied()
    }

    /// `Rollback::stud_column_band`: the tile levels worth scanning in stud tile
    /// column `[x, z]` — its mirrored chunks' layers and the recipe's surface.
    fn stud_column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        let tiles = self.home_tiles()?;
        let (n, l) = (STUD_CHUNK_TILES as i32, STUD_LAYER_TILES as i32);
        let (cx, cz) = (column[0].div_euclid(n), column[1].div_euclid(n));
        let (lx, lz) = (column[0].rem_euclid(n) as usize, column[1].rem_euclid(n) as usize);
        let mut band: Option<(i32, i32)> = None;
        let mut add = |lo: i32, hi: i32| band = Some(band.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        let ground_band = |g: &StudColumnGround| {
            let tp = TILE_PLATES as i32;
            let tops = (0..TILE_STUDS)
                .flat_map(|dz| (0..TILE_STUDS).map(move |dx| (dx, dz)))
                .map(|(dx, dz)| g.surface(lx * TILE_STUDS + dx, lz * TILE_STUDS + dz));
            let (lo, hi) = tops.fold((i32::MAX, i32::MIN), |(lo, hi), s| (lo.min(s), hi.max(s)));
            (lo.saturating_sub(1).div_euclid(tp), hi.saturating_sub(1).div_euclid(tp).saturating_add(1))
        };
        let rn = REGION_CHUNKS as i32;
        if (0..rn).contains(&cx) && (0..rn).contains(&cz) {
            for key in self.stud_chunks.keys().filter(|k| (k[0], k[2]) == (cx, cz)) {
                add(key[1] * l, key[1] * l + l - 1);
            }
            if let Some(g) = tiles.stud_ground.get(&ColumnCoords::new(cx as usize, cz as usize)) {
                let (lo, hi) = ground_band(g);
                add(lo, hi);
            }
        } else if game::halo::in_halo_band(cx, cz) {
            let key = WorldChunkPos::from_local(self.home?, [cx, 0, cz]);
            let layers = tiles.stud_halo.range(WorldChunkPos::new(key.x, i32::MIN, key.z)..=WorldChunkPos::new(key.x, i32::MAX, key.z));
            for (k, _) in layers {
                add(k.y * l, k.y * l + l - 1);
            }
            if let Some(g) = tiles.stud_halo_ground.get(&key) {
                let (lo, hi) = ground_band(g);
                add(lo, hi);
            }
        }
        band
    }

    /// The recipe of region stud column `[x, z]` (own, or the halo's past the
    /// edge) and the column inside it.
    fn stud_recipe_at(&self, column: [i64; 2]) -> Option<(&StudColumnGround, usize, usize)> {
        let tiles = self.home_tiles()?;
        let n = STUD_CHUNK_STUDS as i64;
        let (cx, cz) = (column[0].div_euclid(n), column[1].div_euclid(n));
        let (sx, sz) = (column[0].rem_euclid(n) as usize, column[1].rem_euclid(n) as usize);
        let rn = REGION_CHUNKS as i64;
        let g = if (0..rn).contains(&cx) && (0..rn).contains(&cz) {
            tiles.stud_ground.get(&ColumnCoords::new(cx as usize, cz as usize))?
        } else {
            tiles.stud_halo_ground.get(&WorldChunkPos::from_local(self.home?, [cx as i32, 0, cz as i32]))?
        };
        Some((g, sx, sz))
    }

    /// `Rollback::stud_water_units`: recipe water standing over stud tile `p`'s
    /// own solid top, out of a tile's plates.
    fn stud_water_units(&self, p: TilePos) -> u16 {
        let (ts, tp) = (TILE_STUDS as i64, TILE_PLATES as i64);
        let bottom = i64::from(p[1]) * tp;
        let mut level = 0;
        for dz in 0..ts {
            for dx in 0..ts {
                let column = [i64::from(p[0]) * ts + dx, i64::from(p[2]) * ts + dz];
                let span = self.stud_recipe_at(column).and_then(|(g, sx, sz)| {
                    let bed = g.surface(sx, sz);
                    g.water_top(sx, sz).filter(|&top| top > bed).map(|top| (bed, top))
                });
                if let Some((_, top)) = span.filter(|&(bed, _)| i64::from(bed) < bottom + tp) {
                    level = level.max((i64::from(top) - bottom).clamp(0, tp));
                }
            }
        }
        if level == 0 {
            return 0;
        }
        let solid = i64::from(self.stud_tile(p).floor_plates());
        if level <= solid {
            return 0;
        }
        (i64::from(game::WATER_FULL) * (level - solid) / tp) as u16
    }

    /// File a home stud chunk (region-local `[x, layer, z]`) as tiles.
    pub fn set_stud_chunk(&mut self, entity: Entity, key: [i32; 3], chunk: &StudChunk) {
        if let Some(old) = self.stud_chunk_of.insert(entity, key) {
            if old != key && self.stud_chunks.get(&old).is_some_and(|(owner, _)| *owner == entity) {
                self.stud_chunks.remove(&old);
            }
        }
        let tiles = StudChunkTiles::of(chunk);
        let same = self.stud_chunks.get(&key).is_some_and(|(_, old)| *old == tiles);
        self.stud_chunks.insert(key, (entity, tiles));
        if !same {
            self.version += 1;
        }
    }

    /// A stud chunk entity went away: drop its tiles unless another entity
    /// holds that key by now.
    fn remove_stud_entity(&mut self, entity: Entity) {
        let Some(key) = self.stud_chunk_of.remove(&entity) else { return };
        if self.stud_chunks.get(&key).is_some_and(|(owner, _)| *owner == entity) {
            self.stud_chunks.remove(&key);
            self.version += 1;
        }
    }

    /// Apply one stud mirror message (after the region's `Reset`): the region's
    /// terrain kind and stud recipes, and its stud halo. Updates for a region
    /// without a `Reset` yet are dropped.
    pub fn apply_stud(&mut self, msg: &StudWorldMsg) {
        let halo = |tiles: &mut RegionTiles,
                    chunks: &[(WorldChunkPos, Option<StudChunk>)],
                    ground: &[(WorldChunkPos, Option<StudColumnGround>)]| {
            for (k, chunk) in chunks {
                match chunk {
                    Some(c) => tiles.stud_halo.insert(*k, StudChunkTiles::of(c)),
                    None => tiles.stud_halo.remove(k),
                };
            }
            for (k, g) in ground {
                match g {
                    Some(g) => tiles.stud_halo_ground.insert(*k, g.clone()),
                    None => tiles.stud_halo_ground.remove(k),
                };
            }
        };
        let region = match msg {
            StudWorldMsg::Region { region, terrain, ground, halo: chunks, halo_ground } => {
                let Some(tiles) = self.regions.get_mut(region) else { return };
                tiles.terrain = *terrain;
                tiles.stud_ground = ground.clone();
                tiles.stud_halo.clear();
                tiles.stud_halo_ground.clear();
                halo(tiles, chunks, halo_ground);
                *region
            }
            StudWorldMsg::Halo { region, chunks, ground } => {
                let Some(tiles) = self.regions.get_mut(region) else { return };
                halo(tiles, chunks, ground);
                *region
            }
        };
        if self.home == Some(region) {
            self.version += 1;
        }
    }

    /// Bumped on any change to what the mirror answers.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// See the field: changes when the home ground may have arrived.
    pub fn ground_epoch(&self) -> u64 {
        self.ground_epoch
    }

    /// Whether the home region's ground recipe is mirrored.
    pub fn has_home_ground(&self) -> bool {
        self.home_tiles().is_some_and(|t| !t.ground.is_empty())
    }

    /// Whether world block column `(x, z)` lies in the home region.
    pub fn in_home(&self, x: i32, z: i32) -> bool {
        let n = REGION_BLOCKS as i32;
        self.home == Some(game::RegionCoords::new(x.div_euclid(n), z.div_euclid(n)))
    }

    /// The ORIGINAL worldgen surface block y of world block column `(x, z)`
    /// — home region only (the x-ray opens dug spaces there), `None`
    /// elsewhere or before its ground arrived.
    pub fn home_surface(&self, x: i32, z: i32) -> Option<i16> {
        if !self.in_home(x, z) {
            return None;
        }
        let n = REGION_BLOCKS as i32;
        self.surface_local(x.rem_euclid(n), z.rem_euclid(n))
    }

    /// [`TileMirror::home_surface`] by region-local block column.
    fn surface_local(&self, x: i32, z: i32) -> Option<i16> {
        let cb = CHUNK_BLOCKS as i32;
        if x < 0 || z < 0 {
            return None;
        }
        let ground = self.ground_at((x / cb) as usize, (z / cb) as usize)?;
        ground.surface.get((x % cb * cb + z % cb) as usize).copied()
    }

    /// Whether region-local tile `p` is DUG air: air strictly below its
    /// column's original surface ([`game::is_dug_air`]) — what the x-ray
    /// pick looks through.
    pub fn is_dug(&self, p: TilePos) -> bool {
        // The dug-space x-ray is a voxel shader feature: nothing is "dug" on studs.
        self.terrain() == TerrainKind::Voxel
            && self.block(p) == BlockId::AIR
            && self.surface_local(p[0], p[2]).is_some_and(|s| game::is_dug_air(p[1], i32::from(s)))
    }

    /// Tiles rewritten by [`TileMirror::update_blocks`] so far.
    pub fn tiles_touched(&self) -> u64 {
        self.tiles_touched
    }

    /// Whether `entity`'s chunk is mirrored.
    fn has_entity(&self, entity: Entity) -> bool {
        self.chunk_of.contains_key(&entity)
    }

    /// Re-read just `indices` of an already mirrored chunk from `chunk` (a
    /// `SetBlocks` edit): no full-chunk rescan. `false` when the chunk is not
    /// mirrored (the caller then files it whole).
    pub fn update_blocks(&mut self, key: [i32; 3], chunk: &Chunk, indices: impl IntoIterator<Item = BlockIndex>) -> bool {
        let Some((_, blocks)) = self.chunks.get_mut(&key) else { return false };
        let mut changed = false;
        for i in indices {
            let (x, y, z) = i.xyz();
            let block = chunk.block_at(x, y, z);
            let shape = chunk.chisel.get(&i).and_then(game::slope::classify);
            let slot = i.linear() as u16;
            let slopes = self.slopes.entry(key).or_default();
            if slopes.get(&slot).copied() != shape {
                match shape {
                    Some(shape) => slopes.insert(slot, shape),
                    None => slopes.remove(&slot),
                };
                changed = true;
            }
            if blocks[i.linear()] != block {
                blocks[i.linear()] = block;
                self.tiles_touched += 1;
                changed = true;
            }
        }
        if self.slopes.get(&key).is_some_and(|m| m.is_empty()) {
            self.slopes.remove(&key);
        }
        if changed {
            self.version += 1;
        }
        true
    }

    pub fn set_chunk(&mut self, chunk: [i32; 3], blocks: Box<[Block]>) {
        self.insert_chunk(Entity::PLACEHOLDER, chunk, blocks, BTreeMap::new());
    }

    /// [`TileMirror::set_chunk`] from a sim chunk, slopes included.
    pub fn set_chunk_from(&mut self, key: [i32; 3], chunk: &Chunk) {
        self.insert_chunk(Entity::PLACEHOLDER, key, chunk_blocks(chunk), chunk_slopes(chunk));
    }

    pub fn remove_chunk(&mut self, chunk: [i32; 3]) {
        self.slopes.remove(&chunk);
        if let Some((e, _)) = self.chunks.remove(&chunk) {
            self.chunk_of.remove(&e);
            self.version += 1;
        }
    }

    fn insert_chunk(&mut self, entity: Entity, chunk: [i32; 3], blocks: Box<[Block]>, slopes: BTreeMap<u16, SlopeShape>) {
        if entity != Entity::PLACEHOLDER {
            self.chunk_of.insert(entity, chunk);
        }
        let same = self.chunks.get(&chunk).is_some_and(|(_, old)| *old == blocks)
            && self.slopes.get(&chunk).map_or(slopes.is_empty(), |old| *old == slopes);
        self.chunks.insert(chunk, (entity, blocks));
        if slopes.is_empty() {
            self.slopes.remove(&chunk);
        } else {
            self.slopes.insert(chunk, slopes);
        }
        if !same {
            self.version += 1;
        }
    }

    /// A bridge entity lost its voxels: drop its chunk unless another entity
    /// (a replacement tree's) holds that key by now.
    fn remove_entity(&mut self, entity: Entity) {
        let Some(chunk) = self.chunk_of.remove(&entity) else { return };
        if self.chunks.get(&chunk).is_some_and(|(owner, _)| *owner == entity) {
            self.chunks.remove(&chunk);
            self.slopes.remove(&chunk);
            self.version += 1;
        }
    }

    /// Switch the mirrored region; `true` when it changed (chunks dropped,
    /// to be re-read for the new home).
    pub(crate) fn set_home(&mut self, home: Option<RegionId>) -> bool {
        if self.home == home {
            return false;
        }
        self.home = home;
        self.chunks.clear();
        self.chunk_of.clear();
        self.slopes.clear();
        self.stud_chunks.clear();
        self.stud_chunk_of.clear();
        self.version += 1;
        self.ground_epoch += 1;
        true
    }

    /// Drop the state of regions no longer loaded. Losing the home region's
    /// changes what the mirror answers (no designations, no ground), so that
    /// bumps the version — the overlay rebuilds on it.
    fn prune_regions(&mut self, loaded: impl Fn(&RegionId) -> bool) {
        let had_home = self.home_tiles().is_some();
        self.regions.retain(|r, _| loaded(r));
        if had_home && self.home_tiles().is_none() {
            self.version += 1;
        }
    }

    fn home_tiles(&self) -> Option<&RegionTiles> {
        self.regions.get(&self.home?)
    }

    /// Apply one bridge message. Updates for a region without a `Reset`
    /// yet are dropped (its snapshot will carry them).
    pub fn apply(&mut self, msg: TileMirrorMsg) {
        let is_reset = matches!(msg, TileMirrorMsg::Reset { .. });
        let region = match msg {
            TileMirrorMsg::Reset { region, ground, designations, constructed } => {
                let tiles = RegionTiles {
                    ground,
                    designations: designations.into_iter().collect(),
                    constructed: constructed.into_iter().collect(),
                    ..RegionTiles::default()
                };
                self.regions.insert(region, tiles);
                region
            }
            TileMirrorMsg::Designations { region, changes } => {
                let Some(tiles) = self.regions.get_mut(&region) else { return };
                for (p, d) in changes {
                    match d {
                        Some(d) => tiles.designations.insert(p, d),
                        None => tiles.designations.remove(&p),
                    };
                }
                region
            }
            TileMirrorMsg::Constructed { region, changes } => {
                let Some(tiles) = self.regions.get_mut(&region) else { return };
                for (p, built) in changes {
                    if built {
                        tiles.constructed.insert(p);
                    } else {
                        tiles.constructed.remove(&p);
                    }
                }
                region
            }
            // Read only by walk planning (`WithHalo`), which re-plans on its
            // own key: no version bump.
            TileMirrorMsg::Halo { region, chunks, ground } => {
                let Some(tiles) = self.regions.get_mut(&region) else { return };
                for (k, chunk) in chunks {
                    match chunk {
                        Some(c) => tiles.halo.insert(k, HaloTiles { blocks: chunk_blocks(&c), slopes: chunk_slopes(&c) }),
                        None => tiles.halo.remove(&k),
                    };
                }
                for (k, g) in ground {
                    match g {
                        Some(g) => tiles.halo_ground.insert(k, g),
                        None => tiles.halo_ground.remove(&k),
                    };
                }
                return;
            }
        };
        if self.home == Some(region) {
            self.version += 1;
            if is_reset {
                self.ground_epoch += 1;
            }
        }
    }

    /// The block at `p` from a mirrored chunk (`None` in implied ground).
    fn mirrored_block(&self, p: TilePos) -> Option<Block> {
        let (coords, local) = game::edit::locate(p)?;
        let key = [coords.x as i32, coords.y, coords.z as i32];
        self.chunks.get(&key).map(|(_, blocks)| blocks[BlockIndex::from_xyz(local.0, local.1, local.2).linear()])
    }

    /// What an unloaded sim chunk at world chunk `key` stands for when it lies
    /// below its column's materialised band in a loaded region: implied
    /// ground, answered with an opaque block of it (its bedrock — any opaque
    /// block will do for meshing, which only asks "solid?"). `None` inside or
    /// above the band, or without a recipe (the chunk is then air). The caller
    /// checks the chunk is not loaded: a dug-out chunk is real air.
    pub fn implied_chunk_block(&self, key: [i32; 3]) -> Option<BlockId> {
        let n = game::REGION_CHUNKS as i32;
        let region: RegionId = game::RegionCoords::new(key[0].div_euclid(n), key[2].div_euclid(n));
        let column = ColumnCoords::new(key[0].rem_euclid(n) as usize, key[2].rem_euclid(n) as usize);
        let ground = self.regions.get(&region)?.ground.get(&column)?;
        ground.is_implied(key[1]).then_some(ground.bedrock)
    }

    fn ground_at(&self, x: usize, z: usize) -> Option<&ColumnGround> {
        self.home_tiles()?.ground.get(&ColumnCoords::new(x, z))
    }
}

/// Same rules as `impl TileSource for Rollback`, over the mirrored data.
impl TileSource for TileMirror {
    fn block(&self, p: TilePos) -> BlockId {
        let Some((coords, local)) = game::edit::locate(p) else { return BlockId::AIR };
        let key = [coords.x as i32, coords.y, coords.z as i32];
        match self.chunks.get(&key) {
            Some((_, blocks)) => blocks[BlockIndex::from_xyz(local.0, local.1, local.2).linear()].id,
            None => game::ground::implied_block(self.ground_at(coords.x, coords.z), coords, local)
                .map_or(BlockId::AIR, |b| b.id),
        }
    }

    fn terrain(&self) -> TerrainKind {
        TileMirror::terrain(self)
    }

    fn tile_fill(&self, p: TilePos) -> TileFill {
        match TileMirror::terrain(self) {
            TerrainKind::Voxel => game::voxel_tile_fill(self.block(p)),
            TerrainKind::Stud => self.stud_tile(p).fill(),
        }
    }

    fn tile_material(&self, p: TilePos) -> Option<u16> {
        match TileMirror::terrain(self) {
            TerrainKind::Voxel => None,
            TerrainKind::Stud => self.stud_tile(p).material(),
        }
    }

    fn is_bedrock(&self, p: TilePos) -> bool {
        if TileMirror::terrain(self) == TerrainKind::Stud {
            return self.stud_tile(p).is_bedrock();
        }
        let Some((coords, _)) = game::edit::locate(p) else { return false };
        game::edit::is_bedrock_at(self.ground_at(coords.x, coords.z), Block::new(self.block(p)))
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.home_tiles().is_some_and(|t| t.constructed.contains(&p))
    }

    /// From the mirrored chunk blocks; implied ground holds no trees. On stud
    /// terrain the tree whose wood is in the tile.
    fn tree_id(&self, p: TilePos) -> u32 {
        if TileMirror::terrain(self) == TerrainKind::Stud {
            return self.stud_tree_at(p).map_or(0, u32::from);
        }
        self.mirrored_block(p).map_or(0, |b| b.tree_id())
    }

    fn tree_limb(&self, p: TilePos) -> u8 {
        self.mirrored_block(p).map_or(0, |b| b.limb())
    }

    /// Static water, as the sim's `water_units_at` (`game::static_water_units`):
    /// air inside its column's recipe water.
    fn water_units(&self, p: TilePos) -> u16 {
        if TileMirror::terrain(self) == TerrainKind::Stud {
            return self.stud_water_units(p);
        }
        let Some((coords, (lx, _, lz))) = game::edit::locate(p) else { return 0 };
        let recipe = self.ground_at(coords.x, coords.z).and_then(|g| g.recipe_water(lx, lz));
        game::static_water_units(self.block(p), recipe, p[1])
    }

    /// Same probe as `impl TileSource for Rollback`, over the mirrored chunks.
    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        if TileMirror::terrain(self) == TerrainKind::Stud {
            return self.stud_column_band(column);
        }
        let cb = game::CHUNK_BLOCKS as i32;
        let (cx, cz) = (column[0].div_euclid(cb), column[1].div_euclid(cb));
        let n = game::REGION_CHUNKS as i32;
        if !(0..n).contains(&cx) || !(0..n).contains(&cz) {
            return None;
        }
        game::probe_column_band(column, self.ground_at(cx as usize, cz as usize), |cy| self.chunks.contains_key(&[cx, cy, cz]))
    }

    fn slope(&self, p: TilePos) -> Option<SlopeShape> {
        let (coords, (bx, by, bz)) = game::edit::locate(p)?;
        let key = [coords.x as i32, coords.y, coords.z as i32];
        self.slopes.get(&key)?.get(&(BlockIndex::from_xyz(bx, by, bz).linear() as u16)).copied()
    }
}

/// A [`TileMirror`] read as the sim reads its region: the home region ∪ its
/// terrain halo (`game::halo`) as the predicted home region holds it
/// ([`TileMirrorMsg::Halo`]) — the neighbours' chunks along its edges and
/// their implied ground from the halo's ground recipes, never bedrock or
/// constructions there — whether or not the neighbours are loaded. What
/// walks are planned over (`move_order.rs`, `game::PathPlan`).
pub struct WithHalo<'a>(&'a TileMirror);

impl TileMirror {
    /// The mirror with the home region's halo ([`WithHalo`]).
    pub fn with_halo(&self) -> WithHalo<'_> {
        WithHalo(self)
    }
}

impl WithHalo<'_> {
    /// The home region's halo world chunk and in-chunk block of `p` (a tile
    /// past the home region's edge, in its frame).
    fn halo_locate(&self, p: TilePos) -> Option<(game::WorldChunkPos, (usize, usize, usize))> {
        let home = self.0.home?;
        let cb = CHUNK_BLOCKS as i32;
        let key = [p[0].div_euclid(cb), p[1].div_euclid(cb), p[2].div_euclid(cb)];
        game::halo::in_halo_band(key[0], key[2]).then(|| {
            let local = (p[0].rem_euclid(cb) as usize, p[1].rem_euclid(cb) as usize, p[2].rem_euclid(cb) as usize);
            (game::WorldChunkPos::from_local(home, key), local)
        })
    }

    fn halo_block(&self, p: TilePos) -> Option<Block> {
        let (key, local) = self.halo_locate(p)?;
        let tiles = self.0.home_tiles()?;
        match tiles.halo.get(&key) {
            Some(h) => Some(h.blocks[BlockIndex::from_xyz(local.0, local.1, local.2).linear()]),
            None => {
                let ground = tiles.halo_ground.get(&game::WorldChunkPos::new(key.x, 0, key.z));
                game::ground::implied_block(ground, game::ChunkCoords::new(0, key.y, 0), local)
            }
        }
    }
}

/// The home region's tiles as [`TileMirror`] reads them; past its edge its
/// halo, as `impl TileSource for Rollback` reads it. On stud terrain the
/// mirror itself already reads own ∪ stud halo, so everything forwards to it.
impl TileSource for WithHalo<'_> {
    fn terrain(&self) -> TerrainKind {
        self.0.terrain()
    }

    fn tile_fill(&self, p: TilePos) -> TileFill {
        match self.0.terrain() {
            TerrainKind::Voxel => game::voxel_tile_fill(self.block(p)),
            TerrainKind::Stud => self.0.stud_tile(p).fill(),
        }
    }

    fn tile_material(&self, p: TilePos) -> Option<u16> {
        TileSource::tile_material(self.0, p)
    }

    fn block(&self, p: TilePos) -> BlockId {
        if game::edit::locate(p).is_some() {
            return self.0.block(p);
        }
        self.halo_block(p).map_or(BlockId::AIR, |b| b.id)
    }

    fn is_bedrock(&self, p: TilePos) -> bool {
        self.0.is_bedrock(p)
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.0.is_constructed(p)
    }

    fn tree_id(&self, p: TilePos) -> u32 {
        if self.0.terrain() == TerrainKind::Stud || game::edit::locate(p).is_some() {
            return self.0.tree_id(p);
        }
        self.halo_block(p).map_or(0, |b| b.tree_id())
    }

    fn tree_limb(&self, p: TilePos) -> u8 {
        if game::edit::locate(p).is_some() {
            return self.0.tree_limb(p);
        }
        self.halo_block(p).map_or(0, |b| b.limb())
    }

    /// As the sim's: past the home region's edge, the recipe water of the
    /// halo's ground (`halo_ground`).
    fn water_units(&self, p: TilePos) -> u16 {
        if self.0.terrain() == TerrainKind::Stud || game::edit::locate(p).is_some() {
            return self.0.water_units(p);
        }
        let Some((key, (lx, _, lz))) = self.halo_locate(p) else { return 0 };
        let recipe = self
            .0
            .home_tiles()
            .and_then(|t| t.halo_ground.get(&game::WorldChunkPos::new(key.x, 0, key.z)))
            .and_then(|g| g.recipe_water(lx, lz));
        game::static_water_units(self.block(p), recipe, p[1])
    }

    /// As the sim's halo column: probed over the halo's chunks, no ground.
    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        let cb = CHUNK_BLOCKS as i32;
        let (cx, cz) = (column[0].div_euclid(cb), column[1].div_euclid(cb));
        if self.0.terrain() == TerrainKind::Stud || !game::halo::in_halo_band(cx, cz) {
            return self.0.column_band(column);
        }
        let home = self.0.home?;
        let tiles = self.0.home_tiles()?;
        game::probe_column_band(column, None, |cy| {
            tiles.halo.contains_key(&game::WorldChunkPos::from_local(home, [cx, cy, cz]))
        })
    }

    fn slope(&self, p: TilePos) -> Option<SlopeShape> {
        if game::edit::locate(p).is_some() {
            return self.0.slope(p);
        }
        let (key, (bx, by, bz)) = self.halo_locate(p)?;
        self.0.home_tiles()?.halo.get(&key)?.slopes.get(&(BlockIndex::from_xyz(bx, by, bz).linear() as u16)).copied()
    }
}

/// [`TileMirror::tile_ray`] for terrain `kind`.
pub fn tile_ray(kind: TerrainKind, (origin, dir): (Vector, Vector)) -> (Vector, Vector) {
    let s = game::BLOCK_VOXELS as f32 / kind.tile_height_units();
    if s == 1.0 {
        return (origin, dir);
    }
    let d = Vec3::new(dir.x, dir.y * s, dir.z).normalize_or_zero();
    (Vector::new(origin.x, origin.y * s, origin.z), Vector::new(d.x, d.y, d.z))
}

/// Region-local `[x, layer, z]` of a stud chunk.
fn stud_chunk_key(c: game::StudChunkCoords) -> [i32; 3] {
    [c.x as i32, c.y, c.z as i32]
}

/// Each block (id + data), straight from the sim chunk's blocks.
pub fn chunk_blocks(chunk: &Chunk) -> Box<[Block]> {
    (0..CHUNK_BLOCK_COUNT)
        .map(|i| {
            let (x, y, z) = BlockIndex::from_linear(i).xyz();
            chunk.block_at(x, y, z)
        })
        .collect()
}

/// The chunk's slope blocks by `BlockIndex::linear`.
pub fn chunk_slopes(chunk: &Chunk) -> BTreeMap<u16, SlopeShape> {
    chunk.chisel.iter().filter_map(|(i, c)| Some((i.linear() as u16, game::slope::classify(c)?))).collect()
}

/// Region-local chunk coords of a sim chunk entity.
fn chunk_key(tf: &Transform) -> [i32; 3] {
    let c = game::chunk_coords_from_translation(game::rapier3d::math::Vector::new(tf.translation.x, tf.translation.y, tf.translation.z));
    [c.x as i32, c.y, c.z as i32]
}

/// Keep the mirror in step with the bridge: its messages, the home region
/// and the home region's sim chunk blocks.
pub fn update_tile_mirror(
    mut mirror: ResMut<TileMirror>,
    mut msgs: MessageReader<TileMirrorMsg>,
    status: Res<HudStatus>,
    roots: Res<RegionRoots>,
    mut changed: Query<(Entity, &SimChunk, &Transform, &SimEntity, Option<&mut ChunkEdits>), Changed<SimChunk>>,
    all: Query<(Entity, &SimChunk, &Transform, &SimEntity)>,
    mut removed: RemovedComponents<SimChunk>,
    (stud_msgs, mut stud_cursor): (Option<Res<Messages<StudWorldMsg>>>, Local<bevy::ecs::message::MessageCursor<StudWorldMsg>>),
    changed_studs: Query<(Entity, &SimStudChunk, &SimEntity), Changed<SimStudChunk>>,
    all_studs: Query<(Entity, &SimStudChunk, &SimEntity)>,
    mut removed_studs: RemovedComponents<SimStudChunk>,
) {
    for msg in msgs.read() {
        mirror.apply(msg.clone());
    }
    // After the `Reset`s: a region's stud state hangs off its `RegionTiles`.
    if let Some(stud_msgs) = stud_msgs {
        for msg in stud_cursor.read(&stud_msgs) {
            mirror.apply_stud(msg);
        }
    }
    mirror.prune_regions(|r| roots.0.contains_key(r));
    for entity in removed.read() {
        mirror.remove_entity(entity);
    }
    for entity in removed_studs.read() {
        mirror.remove_stud_entity(entity);
    }
    let Some(home) = status.home_region else {
        mirror.set_home(None);
        return;
    };
    let switched = mirror.set_home(Some(home));
    let studs: Vec<(Entity, &SimStudChunk)> = if switched {
        all_studs.iter().filter(|(_, _, sim)| sim.region == home).map(|(e, c, _)| (e, c)).collect()
    } else {
        changed_studs.iter().filter(|(_, _, sim)| sim.region == home).map(|(e, c, _)| (e, c)).collect()
    };
    for (entity, c) in studs {
        mirror.set_stud_chunk(entity, stud_chunk_key(c.coords), &c.chunk);
    }
    if switched {
        for (entity, chunk, tf, sim) in &all {
            if sim.region == home {
                mirror.insert_chunk(entity, chunk_key(tf), chunk_blocks(&chunk.0), chunk_slopes(&chunk.0));
            }
        }
        // Everything was just read whole: pending edit lists are moot.
        for (.., edits) in &mut changed {
            if let Some(mut edits) = edits {
                *edits = ChunkEdits::default();
            }
        }
        return;
    }
    for (entity, chunk, tf, sim, edits) in &mut changed {
        let (full, blocks) = match edits {
            Some(mut e) => (e.full || !mirror.has_entity(entity), {
                e.full = false;
                std::mem::take(&mut e.blocks)
            }),
            None => (true, Default::default()),
        };
        if sim.region != home {
            continue;
        }
        let key = chunk_key(tf);
        if full || !mirror.update_blocks(key, &chunk.0, blocks) {
            mirror.insert_chunk(entity, key, chunk_blocks(&chunk.0), chunk_slopes(&chunk.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{ChiselData, DesignationKind, OpId, RegionCoords, CHUNK_BLOCKS, UNDERGROUND_DEPTH};

    const STONE: BlockId = BlockId(2);
    const BEDROCK: BlockId = BlockId(7);
    const SURFACE: i32 = 40;

    fn home() -> RegionId {
        RegionCoords::new(1, -2)
    }

    /// The band's bottom chunk (`mirror`'s `band_base`): chunks below it
    /// are implied ground.
    fn band_base() -> i32 {
        SURFACE.div_euclid(CHUNK_BLOCKS as i32) - 1
    }

    /// A block y in implied ground: 3 under the surface (y 37) when that is
    /// below the band, else the top block under the band (16-block chunks:
    /// the band starts at y 16).
    fn implied_y() -> i32 {
        (SURFACE - 3).min(band_base() * CHUNK_BLOCKS as i32 - 1)
    }

    /// A mirror of `home()` whose column (0, 0) has a plain ground recipe
    /// implied below the surface's chunk.
    fn mirror() -> TileMirror {
        let mut m = TileMirror::default();
        m.set_home(Some(home()));
        let band_base = band_base();
        let ground = ColumnGround::plain(vec![SURFACE as i16; game::COLUMN_FOOTPRINT].into_boxed_slice(), band_base, STONE, BEDROCK);
        m.apply(TileMirrorMsg::Reset {
            region: home(),
            ground: BTreeMap::from([(ColumnCoords::new(0, 0), ground)]),
            designations: Vec::new(),
            constructed: Vec::new(),
        });
        m
    }

    #[test]
    fn home_surface_and_dug_air_follow_the_original_surface() {
        let mut m = TileMirror::default();
        let epoch = m.ground_epoch();
        m.set_home(Some(home()));
        assert!(m.ground_epoch() > epoch, "a home switch may bring ground");
        assert!(!m.has_home_ground());
        let epoch = m.ground_epoch();
        m.apply(TileMirrorMsg::Reset {
            region: home(),
            ground: BTreeMap::new(),
            designations: Vec::new(),
            constructed: Vec::new(),
        });
        assert!(m.ground_epoch() > epoch, "the home's Reset may bring ground");
        let mut m = mirror();
        m.set_chunk([0, band_base(), 0], chunk_blocks(&Chunk::uniform(Block::new(BlockId::AIR))));
        assert!(m.has_home_ground());
        let n = REGION_BLOCKS as i32;
        let (wx, wz) = (home().x * n, home().z * n);
        assert_eq!(m.home_surface(wx + 3, wz + 5), Some(SURFACE as i16));
        assert_eq!(m.home_surface(wx - 1, wz), None, "outside home");
        assert_eq!(m.home_surface(wx + CHUNK_BLOCKS as i32, wz), None, "a column without ground");
        // The band's bottom chunk is mirrored as air: dug below the surface.
        let y = band_base() * CHUNK_BLOCKS as i32;
        assert!(m.is_dug([3, y, 5]));
        assert!(!m.is_dug([3, SURFACE, 5]), "the surface block itself");
        assert!(!m.is_dug([3, implied_y(), 5]), "implied rock is not air");
    }

    #[test]
    fn a_missing_chunk_under_the_band_is_implied_rock_in_any_loaded_region() {
        let m = mirror();
        let n = game::REGION_CHUNKS as i32;
        // World chunk keys of home() = (1, -2), column (0, 0).
        let at = |y: i32| [n, y, -2 * n];
        assert_eq!(m.implied_chunk_block(at(band_base() - 1)), Some(BEDROCK), "below the band: rock");
        assert_eq!(m.implied_chunk_block(at(band_base())), None, "the band itself is real chunks");
        assert_eq!(m.implied_chunk_block([n + 1, band_base() - 1, -2 * n]), None, "a column without a recipe");
        assert_eq!(m.implied_chunk_block([0, band_base() - 1, 0]), None, "a region not loaded");
    }

    /// Recipe water reads as the sim's `water_units_at`: air inside a
    /// column's recipe range is full — the home ground's in the home region,
    /// the halo ground's past its edge — and a block there wins.
    #[test]
    fn recipe_water_follows_the_sim_rule() {
        let mut m = mirror();
        let top = (SURFACE + 2) as i16;
        let mut ground = ColumnGround::plain(vec![SURFACE as i16; game::COLUMN_FOOTPRINT].into_boxed_slice(), band_base(), STONE, BEDROCK);
        ground.water_top[2 * CHUNK_BLOCKS + 3] = top;
        m.apply(TileMirrorMsg::Reset {
            region: home(),
            ground: BTreeMap::from([(ColumnCoords::new(0, 0), ground.clone())]),
            designations: Vec::new(),
            constructed: Vec::new(),
        });
        let full = game::WATER_FULL;
        for y in SURFACE + 1..=SURFACE + 2 {
            assert_eq!(m.water_units([2, y, 3]), full, "y {y}");
            assert_eq!(m.with_halo().water_units([2, y, 3]), full, "y {y}, with the halo");
        }
        assert_eq!(m.water_units([2, SURFACE, 3]), 0, "the bed");
        assert_eq!(m.water_units([2, SURFACE + 3, 3]), 0, "above the surface");
        assert_eq!(m.water_units([3, SURFACE + 1, 3]), 0, "a dry column");
        // A block placed into the water wins.
        let cb = CHUNK_BLOCKS as i32;
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        chunk.set_block(2, (SURFACE + 1).rem_euclid(cb) as usize, 3, Block::new(STONE));
        m.set_chunk([0, (SURFACE + 1).div_euclid(cb), 0], chunk_blocks(&chunk));
        assert_eq!(m.water_units([2, SURFACE + 1, 3]), 0, "stone in the water");
        assert_eq!(m.water_units([2, SURFACE + 2, 3]), full, "the cell above it");

        // The halo's recipe, two blocks west of the home region.
        let key = game::WorldChunkPos::from_local(home(), [-1, 0, 0]);
        let mut halo = ground;
        halo.water_top[2 * CHUNK_BLOCKS + 3] = game::NO_WATER;
        halo.water_top[(CHUNK_BLOCKS - 2) * CHUNK_BLOCKS + 3] = top;
        m.apply(TileMirrorMsg::Halo { region: home(), chunks: Vec::new(), ground: vec![(key, Some(halo))] });
        assert_eq!(m.with_halo().water_units([-2, SURFACE + 1, 3]), full, "halo recipe water");
        assert_eq!(m.with_halo().water_units([-2, SURFACE + 3, 3]), 0, "above it");
        assert_eq!(m.with_halo().water_units([-1, SURFACE + 1, 3]), 0, "a dry halo column");
    }

    #[test]
    fn chunk_blocks_read_the_sim_blocks() {
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        chunk.set_block(1, 0, 1, Block::new(STONE));
        let blocks = chunk_blocks(&chunk);
        assert_eq!(blocks.len(), CHUNK_BLOCK_COUNT);
        assert_eq!(blocks[BlockIndex::from_xyz(1, 0, 1).linear()].id, STONE);
        assert_eq!(blocks.iter().filter(|b| b.id != BlockId::AIR).count(), 1);
    }

    #[test]
    fn mirror_reports_slopes() {
        use game::slope::{slope_chisel, Side};
        let mut m = mirror();
        let key = [0, band_base() + 1, 0];
        let y0 = key[1] * CHUNK_BLOCKS as i32;
        let shape = SlopeShape::Straight { low: Side::PosX };
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        let (slope_at, half_at, plain_at) = (BlockIndex::from_xyz(1, 0, 1), BlockIndex::from_xyz(2, 0, 1), BlockIndex::from_xyz(3, 0, 1));
        chunk.set_block(1, 0, 1, Block::new(STONE));
        chunk.chisel.insert(slope_at, slope_chisel(STONE, shape));
        chunk.set_block(2, 0, 1, Block::new(STONE));
        chunk.chisel.insert(half_at, ChiselData::cuboid(STONE, [0, 0, 0], [game::BLOCK_VOXELS, game::BLOCK_VOXELS / 2, game::BLOCK_VOXELS]));
        chunk.set_block(3, 0, 1, Block::new(STONE));
        m.set_chunk_from(key, &chunk);
        assert_eq!(m.slope([1, y0, 1]), Some(shape));
        assert_eq!(m.slope([2, y0, 1]), None, "a half block is no slope");
        assert_eq!(m.slope([3, y0, 1]), None);
        assert_eq!(m.slope([0, implied_y(), 0]), None, "implied ground");
        // An edit that removes the slope clears it.
        chunk.chisel.remove(&slope_at);
        assert!(m.update_blocks(key, &chunk, [slope_at, plain_at]));
        assert_eq!(m.slope([1, y0, 1]), None);
    }

    #[test]
    fn tree_id_comes_from_the_mirrored_blocks() {
        let mut m = mirror();
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        chunk.set_block(2, 1, 3, Block::with_tree(BlockId(13), 77, 5));
        let key = [0, band_base() + 1, 0];
        m.set_chunk(key, chunk_blocks(&chunk));
        let y = key[1] * CHUNK_BLOCKS as i32 + 1;
        assert_eq!((m.tree_id([2, y, 3]), m.tree_limb([2, y, 3])), (77, 5));
        assert_eq!(m.tree_id([1, y, 3]), 0);
        assert_eq!(m.tree_id([0, implied_y(), 0]), 0, "implied ground");
        // An edit that only rewrites the data (a new tree id) still updates.
        chunk.set_block(2, 1, 3, Block::with_tree_id(BlockId(13), 78));
        assert!(m.update_blocks(key, &chunk, [BlockIndex::from_xyz(2, 1, 3)]));
        assert_eq!(m.tree_id([2, y, 3]), 78);
    }

    #[test]
    fn tiles_update_from_set_blocks_touches_one_tile() {
        let mut m = mirror();
        let key = [0, 0, 0];
        let mut chunk = Chunk::uniform(Block::new(STONE));
        m.set_chunk(key, chunk_blocks(&chunk));
        let (v, touched) = (m.version(), m.tiles_touched());
        // One SetBlocks delta: block (1, 0, 1) becomes air.
        let at = BlockIndex::from_xyz(1, 0, 1);
        chunk.set_block(1, 0, 1, Block::new(BlockId::AIR));
        assert!(m.update_blocks(key, &chunk, [at]));
        assert_eq!(m.tiles_touched(), touched + 1, "exactly one tile rewritten");
        assert!(m.version() > v);
        assert_eq!(m.block([1, 0, 1]), BlockId::AIR);
        assert_eq!(m.block([0, 0, 0]), STONE);
        // Re-sending the same state rewrites nothing.
        let v = m.version();
        assert!(m.update_blocks(key, &chunk, [at]));
        assert_eq!(m.tiles_touched(), touched + 1);
        assert_eq!(m.version(), v);
        // An unmirrored chunk is left for a whole-chunk insert.
        assert!(!m.update_blocks([9, 9, 9], &chunk, [at]));
    }

    #[test]
    fn missing_chunk_below_surface_is_implied_ground() {
        let m = mirror();
        let y = implied_y();
        assert_eq!(m.block([0, y, 0]), STONE);
        assert!(!m.is_bedrock([0, y, 0]));
        let deep = [1, SURFACE - UNDERGROUND_DEPTH, 1];
        assert_eq!(m.block(deep), BEDROCK);
        assert!(m.is_bedrock(deep));
        assert_eq!(m.block([0, SURFACE + 1, 0]), BlockId::AIR, "the band is not implied");
    }

    #[test]
    fn mirrored_chunk_overrides_ground() {
        let mut m = mirror();
        let p = [0, implied_y(), 0];
        let v = m.version();
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT].into_boxed_slice();
        blocks[BlockIndex::from_xyz(1, p[1].rem_euclid(CHUNK_BLOCKS as i32) as usize, 0).linear()] = Block::new(BlockId(9));
        m.set_chunk([0, p[1].div_euclid(CHUNK_BLOCKS as i32), 0], blocks);
        assert_eq!(m.block(p), BlockId::AIR);
        assert_eq!(m.block([1, p[1], 0]), BlockId(9));
        assert!(m.version() > v);
        m.remove_chunk([0, p[1].div_euclid(CHUNK_BLOCKS as i32), 0]);
        assert_eq!(m.block(p), STONE);
    }

    #[test]
    fn pruning_the_home_region_bumps_version() {
        let mut m = mirror();
        let v = m.version();
        m.prune_regions(|_| true);
        assert_eq!(m.version(), v, "nothing pruned");
        m.prune_regions(|r| *r != home());
        assert!(m.version() > v, "the home region's state went away");
        assert!(m.designations().is_empty());
        let v = m.version();
        m.prune_regions(|_| false);
        assert_eq!(m.version(), v, "the home region was already gone");
    }

    #[test]
    fn designation_messages_apply_and_bump_version() {
        let mut m = mirror();
        let d = Designation { kind: DesignationKind::Dig, op: OpId::default(), priority: 4 };
        let (a, b) = ([3, 10, 4], [5, 10, 4]);
        let v = m.version();
        m.apply(TileMirrorMsg::Designations { region: home(), changes: vec![(a, Some(d)), (b, Some(d))] });
        assert_eq!(m.designations().len(), 2);
        assert!(m.version() > v);
        // Undo sends the prior values the same way.
        m.apply(TileMirrorMsg::Designations { region: home(), changes: vec![(b, None)] });
        assert_eq!(m.designations().keys().copied().collect::<Vec<_>>(), vec![a]);
        m.apply(TileMirrorMsg::Constructed { region: home(), changes: vec![(b, true)] });
        assert!(m.is_constructed(b));
        // Another region's updates never touch the mirrored one.
        let v = m.version();
        m.apply(TileMirrorMsg::Designations { region: RegionCoords::new(9, 9), changes: vec![(b, Some(d))] });
        m.apply(TileMirrorMsg::Constructed { region: home(), changes: vec![(b, false)] });
        assert!(!m.is_constructed(b));
        assert_eq!(m.designations().len(), 1);
        assert!(m.version() > v);
    }

    #[test]
    fn home_flip_switches_region_state_and_drops_chunks() {
        let mut m = mirror();
        let other = RegionCoords::new(0, 0);
        let d = Designation { kind: DesignationKind::Dig, op: OpId::default(), priority: 4 };
        m.apply(TileMirrorMsg::Reset {
            region: other,
            ground: BTreeMap::new(),
            designations: vec![([1, 1, 1], d)],
            constructed: Vec::new(),
        });
        m.set_chunk([0, 0, 0], vec![Block::new(STONE); CHUNK_BLOCK_COUNT].into_boxed_slice());
        assert!(m.designations().is_empty());
        assert!(m.set_home(Some(other)));
        assert_eq!(m.region(), Some(other));
        assert_eq!(m.designations().len(), 1);
        assert_eq!(m.block([0, 0, 0]), BlockId::AIR, "the old home's chunks are gone");
    }

    // --- Stud terrain ------------------------------------------------------------

    mod stud {
        use super::super::*;
        use crate::renderer::commander::designate::{pick_level, pick_visible};
        use game::{
            plate_units, DesignationKind, GameEventKind, Order, Region, RegionCoords, Rollback, StudBrick, StudChunkCoords,
            StudProfile, StudRegionSeed, MAT_BEDROCK, MAT_DIRT, MAT_GRASS, MAT_STONE, STUD_CHUNK_PLATES, STUD_COLUMN_FOOTPRINT,
            STUD_NO_WATER,
        };

        /// Surface plate of the dry ground: the top of tile level 4.
        const TOP: i32 = 30;
        const BEDROCK: i32 = -12;
        const SOIL: StudProfile = StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 };
        const CLIENT: game::ClientId = 7;
        /// The pond: stud columns 40..56 x 8..24 (chunk column (1, 0)), bed at plate
        /// 22, water to plate 28.
        const POND_BED: i32 = 22;
        const POND_TOP: i32 = 28;

        fn home() -> RegionId {
            RegionCoords::new(0, 0)
        }

        fn recipe(cx: usize, cz: usize) -> StudColumnGround {
            let n = STUD_CHUNK_STUDS;
            let pond = |i: usize| {
                let (x, z) = (cx * n + i % n, cz * n + i / n);
                (40..56).contains(&x) && (8..24).contains(&z)
            };
            StudColumnGround::new(
                (0..STUD_COLUMN_FOOTPRINT).map(|i| if pond(i) { POND_BED } else { TOP }).collect(),
                (0..STUD_COLUMN_FOOTPRINT).map(|i| if pond(i) { POND_TOP } else { STUD_NO_WATER }).collect(),
                vec![0; STUD_COLUMN_FOOTPRINT].into(),
                vec![SOIL],
                vec![0; STUD_COLUMN_FOOTPRINT].into(),
                vec![(i32::MAX, MAT_STONE)],
                BEDROCK,
            )
            .unwrap()
        }

        /// A 2 x 2 chunk-column stud region with a pond, then edited through the
        /// sim's own orders: a pit, a refilled tile, single bricks on the ground.
        fn region() -> Region {
            let mut r = Region::new(Rollback::new(None), None, home(), None);
            let ground = (0..2).flat_map(|x| (0..2).map(move |z| (ColumnCoords::new(x, z), recipe(x, z)))).collect();
            r.seed_stud_world(StudRegionSeed { ground, features: Vec::new() });
            let mut order = |o: Order| r.handle_event(GameEventKind::Order(CLIENT, o)).unwrap();
            // A pit two tiles wide, three deep, down from the surface.
            order(Order::CarveStud { lo: [8, 12, 8], hi: [12, 30, 12] });
            // Half a tile back in, of another material.
            order(Order::FillStud { lo: [8, 12, 8], hi: [10, 15, 10], material: MAT_DIRT });
            // A deep shaft into implied ground (materialises the layer below).
            order(Order::CarveStud { lo: [20, -8, 20], hi: [22, 30, 22] });
            // A wall two tiles tall and a single plate on the ground.
            order(Order::FillStud { lo: [20, 30, 4], hi: [22, 42, 6], material: MAT_STONE });
            order(Order::FillStud { lo: [26, 30, 4], hi: [27, 31, 5], material: MAT_STONE });
            r
        }

        /// The mirror as the bridge would leave it for `r`: the reset, the stud
        /// snapshot message, every materialised chunk.
        fn mirror_of(r: &Region) -> TileMirror {
            let mut m = TileMirror::default();
            m.set_home(Some(home()));
            m.apply(TileMirrorMsg::Reset { region: home(), ground: BTreeMap::new(), designations: Vec::new(), constructed: Vec::new() });
            m.apply_stud(&StudWorldMsg::of_snapshot(home(), r.data()));
            let mut n = 0;
            for x in 0..2 {
                for z in 0..2 {
                    for y in -4..4 {
                        let c = StudChunkCoords { x, y, z };
                        if let Some(chunk) = r.data().stud_chunk_at(c) {
                            n += 1;
                            m.set_stud_chunk(Entity::from_raw_u32(n).unwrap(), stud_chunk_key(c), chunk);
                        }
                    }
                }
            }
            assert!(n >= 5, "the surface layer of each column and the shaft's lower layer: {n}");
            m
        }

        #[test]
        fn the_stud_mirror_agrees_with_the_sim_tile_for_tile() {
            let r = region();
            let m = mirror_of(&r);
            assert_eq!(TileSource::terrain(&m), TerrainKind::Stud);
            let sim = r.data().tiles();
            let mut seen = [0u32; 3];
            for x in -1..33 {
                for z in -1..33 {
                    assert_eq!(m.column_band([x, z]), sim.column_band([x, z]), "band of column [{x}, {z}]");
                    for y in -20..9 {
                        let p = [x, y, z];
                        let fill = sim.tile_fill(p);
                        assert_eq!(m.tile_fill(p), fill, "fill of {p:?}");
                        assert_eq!(m.tile_material(p), sim.tile_material(p), "material of {p:?}");
                        assert_eq!(m.is_bedrock(p), sim.is_bedrock(p), "bedrock at {p:?}");
                        assert_eq!(m.water_units(p), sim.water_units(p), "water in {p:?}");
                        assert_eq!(m.tree_id(p), sim.tree_id(p), "tree at {p:?}");
                        for kind in [DesignationKind::Dig, DesignationKind::Build(game::edit::PLACE_BLOCK), DesignationKind::Chop] {
                            assert_eq!(game::validate(&m, kind, p), game::validate(&sim, kind, p), "{kind:?} on {p:?}");
                        }
                        // The walk planner's view is the same thing on studs.
                        assert_eq!(m.with_halo().tile_fill(p), fill);
                        seen[match fill {
                            TileFill::Open => 0,
                            TileFill::Floor(_) => 1,
                            TileFill::Solid => 2,
                        }] += 1;
                    }
                }
            }
            assert!(seen.iter().all(|&n| n > 0), "open, floor and solid tiles were all compared: {seen:?}");
            // Spot checks, so the comparison is not two equal wrongs.
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Solid, "the top tile of the dry ground");
            assert_eq!(m.tile_fill([0, 5, 0]), TileFill::Open);
            assert_eq!(m.tile_material([0, 4, 0]), Some(MAT_GRASS));
            assert_eq!(m.tile_fill([4, 3, 4]), TileFill::Open, "the pit");
            assert_eq!(m.tile_fill([4, 2, 4]), TileFill::Floor(3), "half refilled");
            assert_eq!(m.tile_material([4, 2, 4]), Some(MAT_DIRT));
            assert_eq!(m.tile_fill([13, 5, 2]), TileFill::Floor(1), "one plate on the ground");
            assert_eq!(m.tile_fill([10, -1, 10]), TileFill::Open, "the shaft, in the layer below");
            assert_eq!(m.tile_fill([10, -2, 10]), TileFill::Floor(4), "its bottom: carved down to plate -8");
            assert_eq!(m.tile_fill([11, -1, 10]), TileFill::Solid, "implied or materialised, rock beside the shaft");
            assert_eq!(m.tile_material([0, -3, 0]), Some(MAT_BEDROCK));
            assert!(m.is_bedrock([0, -3, 0]) && !m.is_bedrock([0, 0, 0]));
            // The pond: 6 plates of water over a bed at plate 22 (tile 3 holds 4 of
            // the bed, tile 4 is all water above it).
            assert!(m.water_units([22, 3, 6]) > 0 && m.water_units([22, 4, 6]) > 0);
            assert_eq!(m.water_units([0, 4, 0]), 0);
        }

        #[test]
        fn hand_built_chunks_implied_ground_and_the_halo_read_by_the_sims_rule() {
            let mut m = TileMirror::default();
            m.set_home(Some(home()));
            m.apply(TileMirrorMsg::Reset { region: home(), ground: BTreeMap::new(), designations: Vec::new(), constructed: Vec::new() });
            let flat = StudColumnGround::flat(TOP, SOIL, MAT_STONE, BEDROCK);
            let west = WorldChunkPos::from_local(home(), [-1, 0, 0]);
            let halo_chunk = StudChunk::new([StudBrick::filler([30, 0, 0], [2, 3, 2], MAT_DIRT)]).unwrap();
            m.apply_stud(&StudWorldMsg::Region {
                region: home(),
                terrain: TerrainKind::Stud,
                ground: std::sync::Arc::new([(ColumnCoords::new(0, 0), flat.clone())].into_iter().collect()),
                halo: vec![(west, Some(halo_chunk))],
                halo_ground: vec![(WorldChunkPos::new(west.x, 0, west.z), Some(flat.clone()))],
            });
            // No chunk mirrored yet: the recipe's implied ground.
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Solid);
            assert_eq!(m.tile_fill([0, 5, 0]), TileFill::Open);
            assert_eq!(m.stud_tile([3, 2, 3]), StudTile::of_ground(&flat, 0, [3, 2, 3]));
            // A mirrored chunk is the truth, whatever the recipe says.
            let chunk = StudChunk::new([
                StudBrick::filler([0, 0, 0], [2, 6, 2], MAT_STONE),
                StudBrick::filler([2, 0, 0], [2, 1, 2], MAT_DIRT),
                StudBrick::filler([4, 0, 0], [1, 6, 1], MAT_STONE),
            ])
            .unwrap();
            let e = Entity::from_raw_u32(1).unwrap();
            let v = m.version();
            m.set_stud_chunk(e, [0, 0, 0], &chunk);
            assert!(m.version() > v);
            assert_eq!(m.tile_fill([0, 0, 0]), TileFill::Solid);
            assert_eq!(m.tile_fill([1, 0, 0]), TileFill::Floor(1));
            assert_eq!(m.tile_fill([2, 0, 0]), TileFill::Floor(6), "one column to the top: not solid");
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Open, "the chunk has nothing there");
            for local in [[0, 0, 0], [1, 0, 0], [2, 0, 0], [5, 3, 5]] {
                let p = [local[0] as i32, local[1] as i32, local[2] as i32];
                assert_eq!(m.stud_tile(p), StudTile::of_chunk(&chunk, local));
            }
            // Layer 1 has no chunk: the recipe again (air above the surface).
            let l = STUD_LAYER_TILES as i32;
            assert_eq!(m.tile_fill([0, l, 0]), TileFill::Open);
            // Past the west edge: the halo chunk where there is one…
            assert_eq!(m.tile_fill([-1, 0, 0]), TileFill::Floor(3));
            assert_eq!(m.tile_material([-1, 0, 0]), Some(MAT_DIRT));
            assert_eq!(m.tile_fill([-1, 4, 0]), TileFill::Open, "the halo chunk is the truth for its layer");
            // …else the neighbour's recipe (the layer below has no halo chunk).
            assert_eq!(m.tile_fill([-1, -1, 0]), TileFill::Solid);
            // Beyond the halo band nothing is known.
            let far = -(STUD_CHUNK_TILES as i32) * (game::halo::HALO_CHUNKS + 1);
            assert_eq!(m.tile_fill([far, 0, 0]), TileFill::Open);
            // The entity going away drops its tiles; an undo of the halo drops its.
            m.remove_stud_entity(e);
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Solid, "back to implied ground");
            m.apply_stud(&StudWorldMsg::Halo { region: home(), chunks: vec![(west, None)], ground: Vec::new() });
            assert_eq!(m.tile_fill([-1, 0, 0]), TileFill::Solid, "the neighbour's recipe");
            assert_eq!(STUD_CHUNK_PLATES % TILE_PLATES, 0);
        }

        #[test]
        fn picking_returns_stud_tiles() {
            let r = region();
            let m = mirror_of(&r);
            let kind = TerrainKind::Stud;
            let (w, h) = (kind.tile_width_units(), kind.tile_height_units());
            let down = Vector::new(0.0, -1.0, 0.0);
            // Straight down onto the dry ground (its top tile is level 4; in voxel
            // blocks the same height would be level 5).
            let over = |tx: f32, tz: f32| Vector::new(tx * w, 400.0, tz * w);
            assert_eq!(pick_visible((over(3.5, 7.25), down), false, &m), Some([3, 4, 7]));
            assert_eq!(pick_visible((over(3.5, 7.25), down), true, &m), Some([3, 5, 7]), "a build goes on top");
            assert_eq!(plate_units(TOP), 5.0 * h);
            // Into the pit: its half-refilled floor tile, three levels down.
            assert_eq!(pick_visible((over(4.5, 4.5), down), false, &m), Some([4, 2, 4]));
            // Working level 5 (standing on the ground): the tile under the cursor at
            // that level, over ground, over the pit and on the wall.
            assert_eq!(pick_level((over(3.5, 7.25), down), 5, &m), Some([3, 5, 7]));
            assert_eq!(pick_level((over(4.5, 4.5), down), 5, &m), Some([4, 5, 4]), "open space clamps to the level");
            assert_eq!(pick_level((over(10.5, 2.5), down), 5, &m), Some([10, 5, 2]), "the wall's lower tile");
            assert_eq!(pick_level((over(4.5, 4.5), down), 2, &m), Some([4, 2, 4]), "the pit floor at its own level");
            // A slanted ray from the west at the wall (stud tiles [10, 5..7, 2]): the
            // tile it enters, and the air tile in front of that face.
            let origin = Vector::new(2.0 * w, 5.5 * h, 2.5 * w);
            let dir = Vector::new(1.0, 0.0, 0.0);
            assert_eq!(pick_visible((origin, dir), false, &m), Some([10, 5, 2]));
            assert_eq!(pick_visible((origin, dir), true, &m), Some([9, 5, 2]));
            let origin = Vector::new(2.0 * w, 6.5 * h, 2.5 * w);
            assert_eq!(pick_visible((origin, dir), false, &m), Some([10, 6, 2]), "1.2 m levels: the wall's upper tile");
            assert_eq!(pick_visible((Vector::new(2.0 * w, 7.5 * h, 2.5 * w), dir), false, &m), None, "over the wall");
        }

        #[test]
        fn tile_space_is_the_identity_on_voxels_and_squashes_stud_levels() {
            let ray = (Vector::new(3.0, 96.0, 5.0), Vector::new(0.0, -1.0, 0.0));
            assert_eq!(tile_ray(TerrainKind::Voxel, ray), ray);
            let (o, d) = tile_ray(TerrainKind::Stud, ray);
            // 96 units is the top of stud level 4: 5 tiles up, i.e. 5 blocks in tile space.
            assert!((o.y - 5.0 * game::BLOCK_VOXELS as f32).abs() < 1e-3, "{o:?}");
            assert_eq!((o.x, o.z), (3.0, 5.0));
            assert!((d.y + 1.0).abs() < 1e-6);
            let (_, d) = tile_ray(TerrainKind::Stud, (ray.0, Vector::new(1.0, -1.0, 0.0).normalize()));
            assert!((d.length() - 1.0).abs() < 1e-5 && d.x > -d.y, "flatter in tile space, still unit length");
        }
    }
}
