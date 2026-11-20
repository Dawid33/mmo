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
use crate::wrap_frame::WrapFrame;

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
    /// The render world's `WrapFrame` (kept by [`update_tile_mirror`]): where
    /// world columns and grid keys (render space) belong, and the halo's
    /// canonical keys.
    frame: WrapFrame,
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

    /// The frame the mirror answers in (see the field).
    pub fn frame(&self) -> WrapFrame {
        self.frame
    }

    /// The home region's origin in the frame (render space).
    pub fn home_offset(&self) -> Option<Vec3> {
        self.home.map(|home| Vec3::from_array(self.frame.offset(home)))
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

    /// Whether world block column `(x, z)` (render space) lies in the home region.
    pub fn in_home(&self, x: i32, z: i32) -> bool {
        let n = REGION_BLOCKS as i32;
        self.home == Some(self.frame.canonical(game::RegionCoords::new(x.div_euclid(n), z.div_euclid(n))))
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

    /// What an unloaded sim chunk at world chunk `key` (render space, as the
    /// brick grid keys it) stands for when it lies
    /// below its column's materialised band in a loaded region: implied
    /// ground, answered with an opaque block of it (its bedrock — any opaque
    /// block will do for meshing, which only asks "solid?"). `None` inside or
    /// above the band, or without a recipe (the chunk is then air). The caller
    /// checks the chunk is not loaded: a dug-out chunk is real air.
    pub fn implied_chunk_block(&self, key: [i32; 3]) -> Option<BlockId> {
        let n = game::REGION_CHUNKS as i32;
        let region: RegionId = self.frame.canonical(game::RegionCoords::new(key[0].div_euclid(n), key[2].div_euclid(n)));
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
            (game::world_chunk(self.0.frame.extent, home, key), local)
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
            tiles.halo.contains_key(&game::world_chunk(self.0.frame.extent, home, [cx, cy, cz]))
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
    frame: Option<Res<WrapFrame>>,
) {
    let frame = frame.as_deref().copied().unwrap_or_default();
    if mirror.frame != frame {
        mirror.frame = frame;
        mirror.version += 1;
    }
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

