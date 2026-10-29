//! The client height grid: one entry per loaded block column, holding the
//! top of its topmost voxel. The commander camera rides it, the ground pick
//! marches through it and the minimap is coloured from it.
//!
//! Built from the sim chunks' BLOCKS (the bridge's `SimChunk` mirror — the
//! snapshot is authoritative, no worldgen here, and no brick voxels, so a sim
//! chunk of many bricks reads the same as one) and refreshed whenever a
//! chunk's blocks change or the chunk goes away. Each chunk contributes the
//! tops of its block columns, sampled at the column centre; a column's height
//! is the highest top over its stack of chunks, so an edit that removes the
//! top falls back to the chunk below. Keyed by the sim chunk entity.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use game::{BlockId, BlockIndex, Chunk, BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_UNITS};

use crate::renderer::bricks::SimChunk;
use crate::renderer::bridge::SimEntity;

/// Voxels along a chunk edge.
const CHUNK_EDGE: usize = CHUNK_UNITS;

/// Block columns per chunk.
const COLUMNS: usize = CHUNK_BLOCKS * CHUNK_BLOCKS;

/// One chunk's contribution: per block column (`bx + CHUNK_BLOCKS * bz`), the
/// height above the chunk's floor (in voxels, `1..=CHUNK_UNITS`) of the top
/// face of its topmost non-air voxel (`surface`) and topmost non-air,
/// non-water voxel (`ground`); `0` = none in this chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkTops {
    pub surface: [u16; COLUMNS],
    pub ground: [u16; COLUMNS],
}

impl Default for ChunkTops {
    fn default() -> Self {
        Self { surface: [0; COLUMNS], ground: [0; COLUMNS] }
    }
}

/// Topmost voxel of voxel column `(vx, vz)` that is neither air nor `skip`
/// (local Y), read from blocks and chisel without expanding anything.
fn top_not(chunk: &Chunk, vx: usize, vz: usize, skip: Option<BlockId>) -> Option<usize> {
    let (bx, lx) = (vx / BLOCK_VOXELS, vx % BLOCK_VOXELS);
    let (bz, lz) = (vz / BLOCK_VOXELS, vz % BLOCK_VOXELS);
    let shown = |id: BlockId| id != BlockId::AIR && Some(id) != skip;
    for by in (0..CHUNK_BLOCKS).rev() {
        let bi = BlockIndex::from_xyz(bx, by, bz);
        let base = by * BLOCK_VOXELS;
        match chunk.chisel.get(&bi) {
            Some(chisel) => {
                if let Some(ly) = (0..BLOCK_VOXELS).rev().find(|&ly| shown(chisel.material_at(lx, ly, lz))) {
                    return Some(base + ly);
                }
            }
            None => {
                if shown(chunk.blocks.get(bi).id) {
                    return Some(base + BLOCK_VOXELS - 1);
                }
            }
        }
    }
    None
}

/// Scan a chunk's block-centre voxel columns, straight from its blocks.
pub fn chunk_tops(chunk: &Chunk, water: BlockId) -> ChunkTops {
    let mut tops = ChunkTops::default();
    if chunk.is_all_air() {
        return tops;
    }
    let centre = BLOCK_VOXELS / 2;
    for bz in 0..CHUNK_BLOCKS {
        for bx in 0..CHUNK_BLOCKS {
            let (vx, vz) = (bx * BLOCK_VOXELS + centre, bz * BLOCK_VOXELS + centre);
            let column = bx + CHUNK_BLOCKS * bz;
            let surface = chunk.column_top(vx as u32, vz as u32);
            tops.surface[column] = surface.map_or(0, |(y, _)| (y + 1) as u16);
            tops.ground[column] = match surface {
                Some((y, id)) if id != water => (y + 1) as u16,
                Some(_) => top_not(chunk, vx, vz, Some(water)).map_or(0, |y| (y + 1) as u16),
                None => 0,
            };
        }
    }
    tops
}

#[derive(Resource)]
pub struct HeightGrid {
    /// Per loaded block column (world block x, z): the top of its topmost
    /// non-air voxel and of its topmost solid (non-air, non-water) voxel,
    /// world y in sim units; NaN where the column has none. Sparse: it covers
    /// whatever the near window has loaded, across regions.
    columns: HashMap<(i32, i32), (f32, f32)>,
    /// Per chunk column (cx, cz), the tops of each loaded chunk by its y.
    stacks: HashMap<(i32, i32), BTreeMap<i32, ChunkTops>>,
    /// Which chunk an entity's voxels were filed under, for removals.
    by_entity: HashMap<Entity, [i32; 3]>,
    water: BlockId,
    /// Bumped on every change, so readers (the minimap) can rebuild lazily.
    pub version: u64,
}

/// The block column containing world (x, z).
fn block_of(x: f32, z: f32) -> (i32, i32) {
    let b = BLOCK_VOXELS as f32;
    ((x / b).floor() as i32, (z / b).floor() as i32)
}

impl HeightGrid {
    pub fn new(water: BlockId) -> Self {
        Self {
            columns: HashMap::new(),
            stacks: HashMap::new(),
            by_entity: HashMap::new(),
            water,
            version: 0,
        }
    }

    /// Surface height (world y, sim units) of the column containing world
    /// (x, z); `None` where nothing is loaded.
    pub fn surface_at(&self, x: f32, z: f32) -> Option<f32> {
        self.columns.get(&block_of(x, z)).map(|c| c.0).filter(|h| !h.is_nan())
    }

    /// Surface and solid-ground heights of the column containing world
    /// (x, z); `None` where nothing is loaded (either may be NaN).
    pub fn column_at(&self, x: f32, z: f32) -> Option<(f32, f32)> {
        self.columns.get(&block_of(x, z)).copied()
    }

    /// Lowest and highest solid surface over the loaded columns (world y, sim
    /// units); `None` while nothing solid is loaded.
    pub fn surface_range(&self) -> Option<(f32, f32)> {
        self.columns
            .values()
            .map(|c| c.1)
            .filter(|h| !h.is_nan())
            .fold(None, |r, h| Some(r.map_or((h, h), |(lo, hi)| (lo.min(h), hi.max(h)))))
    }

    /// File a chunk's tops under its key (chunk coordinates, world / `CHUNK_UNITS`).
    pub fn insert_chunk(&mut self, entity: Entity, key: [i32; 3], tops: ChunkTops) {
        if let Some(old) = self.by_entity.insert(entity, key) {
            if old != key {
                self.remove_key(old);
            }
        }
        self.stacks.entry((key[0], key[2])).or_default().insert(key[1], tops);
        self.recompute(key[0], key[2]);
    }

    pub fn remove_entity(&mut self, entity: Entity) {
        if let Some(key) = self.by_entity.remove(&entity) {
            self.remove_key(key);
        }
    }

    fn remove_key(&mut self, key: [i32; 3]) {
        if let Some(stack) = self.stacks.get_mut(&(key[0], key[2])) {
            stack.remove(&key[1]);
            if stack.is_empty() {
                self.stacks.remove(&(key[0], key[2]));
            }
        }
        self.recompute(key[0], key[2]);
    }

    /// Re-derive the block columns under chunk column (cx, cz).
    fn recompute(&mut self, cx: i32, cz: i32) {
        let chunk = CHUNK_EDGE as f32;
        let blocks = CHUNK_BLOCKS as i32;
        let stack = self.stacks.get(&(cx, cz));
        for bz in 0..CHUNK_BLOCKS {
            for bx in 0..CHUNK_BLOCKS {
                let block = (cx * blocks + bx as i32, cz * blocks + bz as i32);
                let Some(stack) = stack else {
                    self.columns.remove(&block);
                    continue;
                };
                let column = bx + CHUNK_BLOCKS * bz;
                let top = |pick: fn(&ChunkTops) -> &[u16; COLUMNS]| {
                    // Highest chunk first: the first with a top in this column
                    // wins. A loaded column of only air reads as NaN: there is
                    // no ground to stand the camera or a pick on.
                    stack
                        .iter()
                        .rev()
                        .find(|(_, t)| pick(t)[column] > 0)
                        .map_or(f32::NAN, |(cy, t)| *cy as f32 * chunk + pick(t)[column] as f32)
                };
                self.columns.insert(block, (top(|t| &t.surface), top(|t| &t.ground)));
            }
        }
        self.version += 1;
    }

    #[cfg(test)]
    pub fn set_column_for_test(&mut self, at: Vec2, height: f32) {
        self.columns.insert(block_of(at.x, at.y), (height, height));
        self.version += 1;
    }
}

/// World chunk coordinates of a sim chunk entity at `tf` in `sim`'s region.
fn world_chunk_key(tf: &Transform, sim: &SimEntity) -> [i32; 3] {
    let offset = Vec3::from_array(sim.region.world_offset());
    let world = (tf.translation + offset) / CHUNK_EDGE as f32;
    [world.x.round() as i32, world.y.round() as i32, world.z.round() as i32]
}

/// Keep the grid in step with the sim chunks' blocks.
pub fn update_height_grid(
    mut grid: ResMut<HeightGrid>,
    changed: Query<(Entity, &SimChunk, &Transform, &SimEntity), Changed<SimChunk>>,
    mut removed: RemovedComponents<SimChunk>,
) {
    for entity in removed.read() {
        grid.remove_entity(entity);
    }
    let water = grid.water;
    for (entity, chunk, tf, sim) in &changed {
        grid.insert_chunk(entity, world_chunk_key(tf, sim), chunk_tops(&chunk.0, water));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Block, ChiselData};

    const WATER: BlockId = BlockId(5);
    const STONE: BlockId = BlockId(2);

    /// A chunk whose blocks are filled up to `h` voxels with `below` (a
    /// partial block as a chisel cuboid), then up to `w` with water.
    fn layered(h: usize, w: usize, below: BlockId) -> Chunk {
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        for bz in 0..CHUNK_BLOCKS {
            for bx in 0..CHUNK_BLOCKS {
                for by in 0..CHUNK_BLOCKS {
                    let (lo, hi) = (by * BLOCK_VOXELS, (by + 1) * BLOCK_VOXELS);
                    let rock = h.clamp(lo, hi) - lo;
                    let wet = w.clamp(lo, hi) - lo;
                    let bi = BlockIndex::from_xyz(bx, by, bz);
                    if wet == BLOCK_VOXELS && rock == BLOCK_VOXELS {
                        chunk.set_block(bx, by, bz, Block::new(below));
                    } else if rock == 0 && wet == BLOCK_VOXELS {
                        chunk.set_block(bx, by, bz, Block::new(WATER));
                    } else if wet > 0 {
                        // Mixed block: rock below, water above, as a chisel
                        // (built in one canonical merge, not voxel by voxel).
                        let c = ChiselData::from_fn(vec![below, WATER], |_, y, _| {
                            (y < wet).then_some(u8::from(y >= rock))
                        });
                        chunk.blocks.set(bi, Block::new(below));
                        chunk.chisel.insert(bi, c);
                    }
                }
            }
        }
        chunk
    }

    /// The old reference: tops read from the dense voxels of the bricks
    /// stacked over each column (one brick at 2-block chunks).
    fn tops_from_voxels(chunk: &Chunk) -> ChunkTops {
        let edge = game::BRICK_VOXELS;
        let mut bricks: HashMap<[usize; 3], Vec<game::Voxel>> = HashMap::new();
        let mut tops = ChunkTops::default();
        let centre = BLOCK_VOXELS / 2;
        for bz in 0..CHUNK_BLOCKS {
            for bx in 0..CHUNK_BLOCKS {
                let (x, z) = (bx * BLOCK_VOXELS + centre, bz * BLOCK_VOXELS + centre);
                let column = bx + CHUNK_BLOCKS * bz;
                for y in 0..CHUNK_EDGE {
                    let b = [x / edge, y / edge, z / edge];
                    let voxels = bricks.entry(b).or_insert_with(|| chunk.brick(b).voxels());
                    let id = voxels[game::voxel_index(x % edge, y % edge, z % edge)].block;
                    if id == BlockId::AIR {
                        continue;
                    }
                    tops.surface[column] = (y + 1) as u16;
                    if id != WATER {
                        tops.ground[column] = (y + 1) as u16;
                    }
                }
            }
        }
        tops
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn height_from_blocks_matches_voxels() {
        for (h, w) in [(8, 20), (0, 0), (CHUNK_EDGE, CHUNK_EDGE), (5, 5), (1, CHUNK_EDGE)] {
            let chunk = layered(h, w, STONE);
            assert_eq!(chunk_tops(&chunk, WATER), tops_from_voxels(&chunk), "h={h} w={w}");
        }
        // A tall chunk's heights are voxel heights past u8 range.
        let tops = chunk_tops(&layered(CHUNK_EDGE, CHUNK_EDGE, STONE), WATER);
        assert_eq!(tops.surface[0], CHUNK_EDGE as u16);
    }

    #[test]
    fn chunk_tops_find_the_top_face_and_the_ground_under_water() {
        let tops = chunk_tops(&layered(8, 20, STONE), WATER);
        assert_eq!(tops.surface, [20; COLUMNS]);
        assert_eq!(tops.ground, [8; COLUMNS]);
        assert_eq!(chunk_tops(&layered(0, 0, STONE), WATER), ChunkTops::default());
    }

    #[test]
    fn chunk_tops_are_per_block_column() {
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        // Column (bx 1, bz 0) gets one voxel at its centre, y = BLOCK_VOXELS - 2.
        let at = BLOCK_VOXELS / 2;
        let bi = BlockIndex::from_xyz(1, 0, 0);
        let mut c = ChiselData::new(vec![STONE]);
        c.set(at, BLOCK_VOXELS - 2, at, true, 0);
        chunk.blocks.set(bi, Block::new(STONE));
        chunk.chisel.insert(bi, c);
        let tops = chunk_tops(&chunk, WATER);
        let mut want = [0u16; COLUMNS];
        want[1] = (BLOCK_VOXELS - 1) as u16;
        assert_eq!(tops.surface, want);
    }

    #[test]
    fn column_height_is_the_highest_chunk_and_falls_back_on_removal() {
        let mut grid = HeightGrid::new(WATER);
        let (low, high) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        let e = CHUNK_EDGE as i32;
        grid.insert_chunk(low, [3, 0, 4], chunk_tops(&layered(CHUNK_EDGE, CHUNK_EDGE, STONE), WATER));
        grid.insert_chunk(high, [3, 1, 4], chunk_tops(&layered(5, 5, STONE), WATER));
        let (x, z) = ((3 * e + 1) as f32, (4 * e + 1) as f32);
        assert_eq!(grid.surface_at(x, z), Some((e + 5) as f32));
        let v = grid.version;
        grid.remove_entity(high);
        assert_eq!(grid.surface_at(x, z), Some(e as f32));
        assert!(grid.version > v);
        grid.remove_entity(low);
        assert_eq!(grid.surface_at(x, z), None);
        assert_eq!(grid.column_at(x, z), None, "unloaded columns are dropped");
    }

    /// Heights are u16 voxel counts and world heights add the chunk's own y:
    /// a full chunk's top is `CHUNK_UNITS` (256 at 16-block chunks, past u8
    /// range), and chunks below y = 0 count down from the region floor.
    #[test]
    fn heights_span_u16_and_negative_chunk_y() {
        let full = chunk_tops(&layered(CHUNK_EDGE, CHUNK_EDGE, STONE), WATER);
        assert_eq!(full.surface, [CHUNK_EDGE as u16; COLUMNS]);
        let e = CHUNK_EDGE as f32;
        let mut grid = HeightGrid::new(WATER);
        let (deep, top) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        grid.insert_chunk(deep, [0, -2, 0], full);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(-e), "chunk y -2's full top is at -CHUNK_UNITS");
        grid.insert_chunk(top, [0, -1, 0], chunk_tops(&layered(3, 3, STONE), WATER));
        assert_eq!(grid.surface_at(1.0, 1.0), Some(3.0 - e), "chunk y -1 stacks on top");
        grid.remove_entity(top);
        grid.insert_chunk(top, [0, 1, 0], full);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(2.0 * e), "a full chunk at y 1 tops out at 2·CHUNK_UNITS");
    }

    /// A sim chunk's key is its region-local position plus the region's
    /// world offset, in chunks — negative regions and chunk y included.
    #[test]
    fn world_chunk_key_adds_the_region_offset() {
        let e = CHUNK_EDGE as f32;
        let per_region = game::REGION_CHUNKS as i32;
        let sim = |x, z| SimEntity { region: game::RegionCoords::new(x, z), key: Default::default() };
        let local = Transform::from_xyz(3.0 * e, -2.0 * e, 1.0 * e);
        assert_eq!(world_chunk_key(&local, &sim(0, 0)), [3, -2, 1]);
        assert_eq!(world_chunk_key(&local, &sim(2, -1)), [2 * per_region + 3, -2, -per_region + 1]);
        assert_eq!(world_chunk_key(&local, &sim(-1, 3)), [-per_region + 3, -2, 3 * per_region + 1]);
    }
}
