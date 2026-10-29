//! The tile mirror: the home region's tiles on the main thread — block ids
//! per chunk, the underground ground recipes, designations and constructed
//! tiles — behind `game::TileSource`, so the fortress tools (hover pick,
//! validity preview, overlay) answer synchronously with the sim's own rules.
//!
//! Chunk blocks come from the bridge's `VoxelData` (like the height grid, but
//! keyed region-locally: no world offset). The rest arrives as
//! [`TileMirrorMsg`]s from the bridge: a `Reset` per received region snapshot,
//! then the sim's `SetDesignations` / `SetConstructed` updates (do and undo
//! alike are plain writes). The non-chunk state is kept for every loaded
//! region, so a home flip switches without a re-send; chunks only for home.
//! Cell mode has no ground semantics and no fortress UI: the mirror stays
//! empty there.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bevy::prelude::*;
use game::{
    Block, BlockId, BlockIndex, ColumnCoords, ColumnGround, Designation, RegionId, TilePos, TileSource,
    BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT,
};

use crate::renderer::bridge::{RegionRoots, SimEntity, VoxelData};
use crate::renderer::hud::HudStatus;

/// Voxels along a chunk edge.
const CHUNK_EDGE: usize = BLOCK_VOXELS * CHUNK_BLOCKS;

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
    /// Home region chunks (region-local chunk coords) → block ids by
    /// `BlockIndex`, with the bridge entity that supplied them
    /// (`Entity::PLACEHOLDER` for [`TileMirror::set_chunk`]).
    chunks: HashMap<[i32; 3], (Entity, [BlockId; CHUNK_BLOCK_COUNT])>,
    chunk_of: HashMap<Entity, [i32; 3]>,
    version: u64,
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

    pub fn set_chunk(&mut self, chunk: [i32; 3], blocks: [BlockId; CHUNK_BLOCK_COUNT]) {
        self.insert_chunk(Entity::PLACEHOLDER, chunk, blocks);
    }

    pub fn remove_chunk(&mut self, chunk: [i32; 3]) {
        if let Some((e, _)) = self.chunks.remove(&chunk) {
            self.chunk_of.remove(&e);
            self.version += 1;
        }
    }

    fn insert_chunk(&mut self, entity: Entity, chunk: [i32; 3], blocks: [BlockId; CHUNK_BLOCK_COUNT]) {
        if entity != Entity::PLACEHOLDER {
            self.chunk_of.insert(entity, chunk);
        }
        if self.chunks.insert(chunk, (entity, blocks)).is_none_or(|(_, old)| old != blocks) {
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
            Some((_, ids)) => ids[BlockIndex::from_xyz(local.0, local.1, local.2).0 as usize],
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
}

/// Each block's id: its first non-air voxel's (dense-index order), else AIR.
/// Walks the RLE runs, stepping a non-air run one 16-voxel row segment (one
/// block's share of an x-row) at a time; air runs cost nothing.
pub fn chunk_block_ids(voxels: &VoxelData) -> [BlockId; CHUNK_BLOCK_COUNT] {
    let mut ids = [BlockId::AIR; CHUNK_BLOCK_COUNT];
    let mut start = 0;
    for &(len, id) in voxels.run_slice() {
        let end = start + len as usize;
        if id != BlockId::AIR {
            let mut i = start;
            while i < end {
                let (x, y, z) = (i % CHUNK_EDGE, (i / CHUNK_EDGE) % CHUNK_EDGE, i / (CHUNK_EDGE * CHUNK_EDGE));
                let b = BlockIndex::from_xyz(x / BLOCK_VOXELS, y / BLOCK_VOXELS, z / BLOCK_VOXELS).0 as usize;
                if ids[b] == BlockId::AIR {
                    ids[b] = id;
                }
                i = (i / BLOCK_VOXELS + 1) * BLOCK_VOXELS;
            }
        }
        start = end;
    }
    ids
}

/// Region-local chunk coords of a bridge chunk entity.
fn chunk_key(tf: &Transform) -> [i32; 3] {
    let c = tf.translation / CHUNK_EDGE as f32;
    [c.x.round() as i32, c.y.round() as i32, c.z.round() as i32]
}

/// Keep the mirror in step with the bridge: its messages, the home region
/// and the home region's chunk voxels.
pub fn update_tile_mirror(
    mut mirror: ResMut<TileMirror>,
    mut msgs: MessageReader<TileMirrorMsg>,
    status: Res<HudStatus>,
    roots: Res<RegionRoots>,
    changed: Query<(Entity, &VoxelData, &Transform, &SimEntity), Changed<VoxelData>>,
    all: Query<(Entity, &VoxelData, &Transform, &SimEntity)>,
    mut removed: RemovedComponents<VoxelData>,
) {
    if game::CELL_CHUNKS != 0 {
        msgs.clear();
        removed.clear();
        return;
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
    let chunks: Box<dyn Iterator<Item = _>> =
        if mirror.set_home(Some(home)) { Box::new(all.iter()) } else { Box::new(changed.iter()) };
    for (entity, voxels, tf, sim) in chunks {
        if sim.region == home {
            mirror.insert_chunk(entity, chunk_key(tf), chunk_block_ids(voxels));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{DesignationKind, OpId, RegionCoords, Voxel, UNDERGROUND_DEPTH};

    const STONE: BlockId = BlockId(2);
    const BEDROCK: BlockId = BlockId(7);
    const SURFACE: i32 = 40;

    fn home() -> RegionId {
        RegionCoords::new(1, -2)
    }

    /// A mirror of `home()` whose column (0, 0) has a plain ground recipe
    /// implied below the surface's chunk.
    fn mirror() -> TileMirror {
        let mut m = TileMirror::default();
        m.set_home(Some(home()));
        let band_base = SURFACE.div_euclid(CHUNK_BLOCKS as i32) - 1;
        let ground = ColumnGround::plain([SURFACE as i16; CHUNK_BLOCKS * CHUNK_BLOCKS], band_base, STONE, BEDROCK);
        m.apply(TileMirrorMsg::Reset {
            region: home(),
            ground: BTreeMap::from([(ColumnCoords::new(0, 0), ground)]),
            designations: Vec::new(),
            constructed: Vec::new(),
        });
        m
    }

    #[test]
    fn chunk_block_ids_take_the_first_solid_voxel() {
        let mut dense = vec![Voxel::new(BlockId::AIR); game::CHUNK_VOXEL_COUNT];
        // Block 5 = (bx 1, by 0, bz 1): one voxel deep inside it.
        let (x, y, z) = (BLOCK_VOXELS + 3, 7, BLOCK_VOXELS + 9);
        dense[x + CHUNK_EDGE * y + CHUNK_EDGE * CHUNK_EDGE * z] = Voxel::new(STONE);
        let mut want = [BlockId::AIR; CHUNK_BLOCK_COUNT];
        want[5] = STONE;
        assert_eq!(BlockIndex::from_xyz(1, 0, 1).0, 5);
        assert_eq!(chunk_block_ids(&VoxelData::new(dense)), want);
    }

    #[test]
    fn missing_chunk_below_surface_is_implied_ground() {
        let m = mirror();
        assert_eq!(m.block([0, SURFACE - 3, 0]), STONE);
        assert!(!m.is_bedrock([0, SURFACE - 3, 0]));
        let deep = [1, SURFACE - UNDERGROUND_DEPTH, 1];
        assert_eq!(m.block(deep), BEDROCK);
        assert!(m.is_bedrock(deep));
        assert!(m.edge_locked([0, SURFACE - 3, 0]), "x = 0 borders the next region");
        assert!(!m.edge_locked([1, SURFACE - 3, 1]));
        assert_eq!(m.block([0, SURFACE + 1, 0]), BlockId::AIR, "the band is not implied");
    }

    #[test]
    fn mirrored_chunk_overrides_ground() {
        let mut m = mirror();
        let p = [0, SURFACE - 3, 0];
        let v = m.version();
        let mut ids = [BlockId::AIR; CHUNK_BLOCK_COUNT];
        ids[BlockIndex::from_xyz(1, 1, 0).0 as usize] = BlockId(9);
        m.set_chunk([0, p[1].div_euclid(CHUNK_BLOCKS as i32), 0], ids);
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
        m.set_chunk([0, 0, 0], [STONE; CHUNK_BLOCK_COUNT]);
        assert!(m.designations().is_empty());
        assert!(m.set_home(Some(other)));
        assert_eq!(m.region(), Some(other));
        assert_eq!(m.designations().len(), 1);
        assert_eq!(m.block([0, 0, 0]), BlockId::AIR, "the old home's chunks are gone");
    }
}
