//! The client height grid: one entry per loaded block column, holding the
//! top of its topmost voxel. The commander camera rides it, the ground pick
//! marches through it and the minimap is coloured from it.
//!
//! Built from the bridge's chunk voxels (the snapshot is authoritative — no
//! worldgen here) and refreshed whenever a chunk's `VoxelData` changes or goes
//! away. Each chunk contributes the tops of its 2×2 block columns, sampled at
//! the column centre; a column's height is the highest top over its stack of
//! chunks, so an edit that removes the top falls back to the chunk below.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use game::{BlockId, BLOCK_VOXELS, CHUNK_BLOCKS};

use crate::renderer::bridge::{SimEntity, VoxelData};

/// Voxels along a chunk edge.
const CHUNK_EDGE: usize = BLOCK_VOXELS * CHUNK_BLOCKS;

/// One chunk's contribution: per block column (`bx + CHUNK_BLOCKS * bz`), the
/// height above the chunk's floor (in voxels, `1..=32`) of the top face of its
/// topmost non-air voxel (`surface`) and topmost non-air, non-water voxel
/// (`ground`); `0` = none in this chunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkTops {
    pub surface: [u8; CHUNK_BLOCKS * CHUNK_BLOCKS],
    pub ground: [u8; CHUNK_BLOCKS * CHUNK_BLOCKS],
}

/// Scan a chunk's block-centre voxel columns, reading the RLE without
/// expanding it.
pub fn chunk_tops(voxels: &VoxelData, water: BlockId) -> ChunkTops {
    let centre = BLOCK_VOXELS / 2;
    // Dense index x + 32·y + 1024·z; z outermost, then y, then x keeps the
    // visit sorted, as `for_each_index_sorted` requires.
    let indices = (0..CHUNK_BLOCKS).flat_map(move |bz| {
        (0..CHUNK_EDGE).flat_map(move |y| {
            (0..CHUNK_BLOCKS).map(move |bx| {
                let (x, z) = (bx * BLOCK_VOXELS + centre, bz * BLOCK_VOXELS + centre);
                x + CHUNK_EDGE * y + CHUNK_EDGE * CHUNK_EDGE * z
            })
        })
    });
    let mut tops = ChunkTops::default();
    voxels.for_each_index_sorted(indices, |index, voxel| {
        if voxel.block == BlockId::AIR {
            return;
        }
        let x = index % CHUNK_EDGE;
        let y = (index / CHUNK_EDGE) % CHUNK_EDGE;
        let z = index / (CHUNK_EDGE * CHUNK_EDGE);
        let column = x / BLOCK_VOXELS + CHUNK_BLOCKS * (z / BLOCK_VOXELS);
        let top = (y + 1) as u8;
        // Visited bottom-up per column, so the last write is the topmost.
        tops.surface[column] = top;
        if voxel.block != water {
            tops.ground[column] = top;
        }
    });
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

    /// File a chunk's tops under its key (chunk coordinates, world / 32).
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
                let top = |pick: fn(&ChunkTops) -> &[u8; CHUNK_BLOCKS * CHUNK_BLOCKS]| {
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

/// Keep the grid in step with the bridge's chunk voxels.
pub fn update_height_grid(
    mut grid: ResMut<HeightGrid>,
    changed: Query<(Entity, &VoxelData, &Transform, &SimEntity), Changed<VoxelData>>,
    mut removed: RemovedComponents<VoxelData>,
) {
    for entity in removed.read() {
        grid.remove_entity(entity);
    }
    let water = grid.water;
    for (entity, voxels, tf, sim) in &changed {
        let offset = Vec3::from_array(sim.region.world_offset());
        let world = (tf.translation + offset) / CHUNK_EDGE as f32;
        let key = [world.x.round() as i32, world.y.round() as i32, world.z.round() as i32];
        grid.insert_chunk(entity, key, chunk_tops(voxels, water));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::Voxel;

    const WATER: BlockId = BlockId(5);
    const STONE: BlockId = BlockId(2);

    /// A chunk filled up to `h` voxels with `below`, then up to `w` with water.
    fn layered(h: usize, w: usize, below: BlockId) -> VoxelData {
        let mut dense = vec![Voxel::new(BlockId::AIR); game::CHUNK_VOXEL_COUNT];
        for (i, v) in dense.iter_mut().enumerate() {
            let y = (i / CHUNK_EDGE) % CHUNK_EDGE;
            if y < h {
                *v = Voxel::new(below);
            } else if y < w {
                *v = Voxel::new(WATER);
            }
        }
        VoxelData::new(dense)
    }

    #[test]
    fn chunk_tops_find_the_top_face_and_the_ground_under_water() {
        let tops = chunk_tops(&layered(8, 20, STONE), WATER);
        assert_eq!(tops.surface, [20; 4]);
        assert_eq!(tops.ground, [8; 4]);
        let air = chunk_tops(&layered(0, 0, STONE), WATER);
        assert_eq!(air, ChunkTops::default());
    }

    #[test]
    fn chunk_tops_are_per_block_column() {
        let mut dense = vec![Voxel::new(BlockId::AIR); game::CHUNK_VOXEL_COUNT];
        // Column (bx 1, bz 0) gets a voxel at y = 30 at its centre.
        dense[24 + CHUNK_EDGE * 30 + CHUNK_EDGE * CHUNK_EDGE * 8] = Voxel::new(STONE);
        let tops = chunk_tops(&VoxelData::new(dense), WATER);
        assert_eq!(tops.surface, [0, 31, 0, 0]);
    }

    #[test]
    fn column_height_is_the_highest_chunk_and_falls_back_on_removal() {
        let mut grid = HeightGrid::new(WATER);
        let (low, high) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        grid.insert_chunk(low, [3, 0, 4], chunk_tops(&layered(32, 32, STONE), WATER));
        grid.insert_chunk(high, [3, 1, 4], chunk_tops(&layered(5, 5, STONE), WATER));
        let (x, z) = (3.0 * 32.0 + 1.0, 4.0 * 32.0 + 1.0);
        assert_eq!(grid.surface_at(x, z), Some(37.0));
        let v = grid.version;
        grid.remove_entity(high);
        assert_eq!(grid.surface_at(x, z), Some(32.0));
        assert!(grid.version > v);
        grid.remove_entity(low);
        assert_eq!(grid.surface_at(x, z), None);
        assert_eq!(grid.column_at(x, z), None, "unloaded columns are dropped");
    }
}
