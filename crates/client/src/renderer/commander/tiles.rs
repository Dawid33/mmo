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
    Block, BlockId, BlockIndex, Chunk, ColumnCoords, ColumnGround, Designation, RegionId, TilePos, TileSource,
    CHUNK_BLOCK_COUNT,
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
}

/// One region's non-chunk tile state.
#[derive(Debug, Clone, Default)]
struct RegionTiles {
    ground: BTreeMap<ColumnCoords, ColumnGround>,
    designations: BTreeMap<TilePos, Designation>,
    constructed: BTreeSet<TilePos>,
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
    version: u64,
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
            if blocks[i.linear()] != block {
                blocks[i.linear()] = block;
                self.tiles_touched += 1;
                changed = true;
            }
        }
        if changed {
            self.version += 1;
        }
        true
    }

    pub fn set_chunk(&mut self, chunk: [i32; 3], blocks: Box<[Block]>) {
        self.insert_chunk(Entity::PLACEHOLDER, chunk, blocks);
    }

    pub fn remove_chunk(&mut self, chunk: [i32; 3]) {
        if let Some((e, _)) = self.chunks.remove(&chunk) {
            self.chunk_of.remove(&e);
            self.version += 1;
        }
    }

    fn insert_chunk(&mut self, entity: Entity, chunk: [i32; 3], blocks: Box<[Block]>) {
        if entity != Entity::PLACEHOLDER {
            self.chunk_of.insert(entity, chunk);
        }
        let same = self.chunks.get(&chunk).is_some_and(|(_, old)| *old == blocks);
        self.chunks.insert(chunk, (entity, blocks));
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
        self.version += 1;
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
        let region = match msg {
            TileMirrorMsg::Reset { region, ground, designations, constructed } => {
                let tiles = RegionTiles {
                    ground,
                    designations: designations.into_iter().collect(),
                    constructed: constructed.into_iter().collect(),
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
        };
        if self.home == Some(region) {
            self.version += 1;
        }
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

    fn edge_locked(&self, p: TilePos) -> bool {
        let Some((coords, _)) = game::edit::locate(p) else { return false };
        game::edit::edge_ring_locked(p, self.ground_at(coords.x, coords.z))
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.home_tiles().is_some_and(|t| t.constructed.contains(&p))
    }

    /// From the mirrored chunk blocks; implied ground holds no trees.
    fn tree_id(&self, p: TilePos) -> u32 {
        let Some((coords, local)) = game::edit::locate(p) else { return 0 };
        let key = [coords.x as i32, coords.y, coords.z as i32];
        self.chunks
            .get(&key)
            .map_or(0, |(_, blocks)| blocks[BlockIndex::from_xyz(local.0, local.1, local.2).linear()].tree_id())
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
                mirror.insert_chunk(entity, chunk_key(tf), chunk_blocks(&chunk.0));
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
            mirror.insert_chunk(entity, key, chunk_blocks(&chunk.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{DesignationKind, OpId, RegionCoords, CHUNK_BLOCKS, UNDERGROUND_DEPTH};

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
    fn chunk_blocks_read_the_sim_blocks() {
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        chunk.set_block(1, 0, 1, Block::new(STONE));
        let blocks = chunk_blocks(&chunk);
        assert_eq!(blocks.len(), CHUNK_BLOCK_COUNT);
        assert_eq!(blocks[BlockIndex::from_xyz(1, 0, 1).linear()].id, STONE);
        assert_eq!(blocks.iter().filter(|b| b.id != BlockId::AIR).count(), 1);
    }

    #[test]
    fn tree_id_comes_from_the_mirrored_blocks() {
        let mut m = mirror();
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        chunk.set_block(2, 1, 3, Block::with_tree_id(BlockId(13), 77));
        let key = [0, band_base() + 1, 0];
        m.set_chunk(key, chunk_blocks(&chunk));
        let y = key[1] * CHUNK_BLOCKS as i32 + 1;
        assert_eq!(m.tree_id([2, y, 3]), 77);
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
        assert!(m.edge_locked([0, y, 0]), "x = 0 borders the next region");
        assert!(!m.edge_locked([1, y, 1]));
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
