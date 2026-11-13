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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bevy::prelude::*;
use game::{
    Block, BlockId, BlockIndex, Chunk, ColumnCoords, ColumnGround, Designation, RegionId, SlopeShape, TilePos, TileSource,
    CHUNK_BLOCKS, CHUNK_BLOCK_COUNT, REGION_BLOCKS,
};

use crate::renderer::bricks::{ChunkEdits, SimChunk};
use crate::renderer::bridge::{RegionRoots, SimEntity};
use crate::renderer::hud::HudStatus;

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

/// One region's non-chunk tile state.
#[derive(Debug, Clone, Default)]
struct RegionTiles {
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
        self.block(p) == BlockId::AIR
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

    fn is_bedrock(&self, p: TilePos) -> bool {
        let Some((coords, _)) = game::edit::locate(p) else { return false };
        game::edit::is_bedrock_at(self.ground_at(coords.x, coords.z), Block::new(self.block(p)))
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.home_tiles().is_some_and(|t| t.constructed.contains(&p))
    }

    /// From the mirrored chunk blocks; implied ground holds no trees.
    fn tree_id(&self, p: TilePos) -> u32 {
        self.mirrored_block(p).map_or(0, |b| b.tree_id())
    }

    fn tree_limb(&self, p: TilePos) -> u8 {
        self.mirrored_block(p).map_or(0, |b| b.limb())
    }

    /// Static water, as the sim's `water_units_at` (`game::static_water_units`):
    /// air inside its column's recipe water.
    fn water_units(&self, p: TilePos) -> u16 {
        let Some((coords, (lx, _, lz))) = game::edit::locate(p) else { return 0 };
        let recipe = self.ground_at(coords.x, coords.z).and_then(|g| g.recipe_water(lx, lz));
        game::static_water_units(self.block(p), recipe, p[1])
    }

    /// Same probe as `impl TileSource for Rollback`, over the mirrored chunks.
    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
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
/// halo, as `impl TileSource for Rollback` reads it.
impl TileSource for WithHalo<'_> {
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
        if game::edit::locate(p).is_some() {
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
        if game::edit::locate(p).is_some() {
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
        if !game::halo::in_halo_band(cx, cz) {
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
) {
    for msg in msgs.read() {
        mirror.apply(msg.clone());
    }
    mirror.prune_regions(|r| roots.0.contains_key(r));
    for entity in removed.read() {
        mirror.remove_entity(entity);
    }
    let Some(home) = status.home_region else {
        mirror.set_home(None);
        return;
    };
    if mirror.set_home(Some(home)) {
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
}
