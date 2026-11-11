//! The client height grid: one entry per loaded block column (a 1 m column,
//! 2x2 stud columns), holding the top of its topmost brick. The commander
//! camera rides it, the ground pick marches through it and the minimap is
//! coloured from it.
//!
//! Built from the stud chunks the bridge mirrors (`SimStudChunk` — the
//! snapshot is authoritative, no worldgen here) and refreshed whenever a
//! chunk changes or goes away. Each chunk contributes the tops of its block
//! columns; a column's height is the highest top over its stack of chunk
//! layers, so an edit that removes the top falls back to the layer below.
//! Keyed by the stud chunk entity.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use game::{StudChunk, BLOCK_UNITS, CHUNK_BLOCKS, CHUNK_UNITS, STUD_CHUNK_HEIGHT_UNITS};

use crate::renderer::bridge::SimEntity;
use crate::renderer::stud::SimStudChunk;

/// Sim units along a chunk's horizontal edge.
const CHUNK_EDGE: usize = CHUNK_UNITS;

/// Block columns per chunk.
const COLUMNS: usize = CHUNK_BLOCKS * CHUNK_BLOCKS;

/// One stud chunk's contribution: per block column (`bx + CHUNK_BLOCKS * bz`,
/// each covering 2x2 stud columns), the highest brick top over those stud
/// columns in sim units above the stud chunk's floor; `0.0` = none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StudTops {
    pub surface: [f32; COLUMNS],
}

impl Default for StudTops {
    fn default() -> Self {
        Self { surface: [0.0; COLUMNS] }
    }
}

/// Tops of a stud chunk's block columns, in one pass over its bricks.
pub fn stud_chunk_tops(chunk: &StudChunk) -> StudTops {
    let studs_per_block = game::STUD_CHUNK_STUDS / CHUNK_BLOCKS;
    let mut plates = [0u8; COLUMNS];
    for b in chunk.bricks() {
        let [w, h, d] = b.size();
        let top = b.min[1] + h;
        for sz in b.min[2]..b.min[2] + d {
            for sx in b.min[0]..b.min[0] + w {
                let column = sx as usize / studs_per_block + CHUNK_BLOCKS * (sz as usize / studs_per_block);
                plates[column] = plates[column].max(top);
            }
        }
    }
    let mut tops = StudTops::default();
    for (out, &p) in tops.surface.iter_mut().zip(&plates) {
        *out = game::plate_units(i32::from(p));
    }
    tops
}

#[derive(Resource)]
pub struct HeightGrid {
    /// Per loaded block column (world block x, z): the top of its topmost
    /// brick as `(surface, ground)`, world y in sim units — the two are equal
    /// (recipe water is not in the chunks); NaN where the column has none.
    /// Sparse: it covers whatever the near window has loaded, across regions.
    columns: HashMap<(i32, i32), (f32, f32)>,
    /// Per chunk column (cx, cz), the tops of each loaded stud chunk by its layer.
    stud_stacks: HashMap<(i32, i32), BTreeMap<i32, StudTops>>,
    /// Which stud chunk (world cx, layer, cz) an entity was filed under.
    stud_by_entity: HashMap<Entity, [i32; 3]>,
    /// Bumped on every change, so readers (the minimap) can rebuild lazily.
    pub version: u64,
}

/// The block column containing world (x, z).
fn block_of(x: f32, z: f32) -> (i32, i32) {
    let b = BLOCK_UNITS as f32;
    ((x / b).floor() as i32, (z / b).floor() as i32)
}

impl HeightGrid {
    pub fn new() -> Self {
        Self { columns: HashMap::new(), stud_stacks: HashMap::new(), stud_by_entity: HashMap::new(), version: 0 }
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

    /// File a stud chunk's tops under `key` (world cx, stud layer, cz).
    pub fn insert_stud_chunk(&mut self, entity: Entity, key: [i32; 3], tops: StudTops) {
        if let Some(old) = self.stud_by_entity.insert(entity, key) {
            if old != key {
                self.remove_stud_key(old);
            }
        }
        self.stud_stacks.entry((key[0], key[2])).or_default().insert(key[1], tops);
        self.recompute(key[0], key[2]);
    }

    fn remove_stud_key(&mut self, key: [i32; 3]) {
        if let Some(stack) = self.stud_stacks.get_mut(&(key[0], key[2])) {
            stack.remove(&key[1]);
            if stack.is_empty() {
                self.stud_stacks.remove(&(key[0], key[2]));
            }
        }
        self.recompute(key[0], key[2]);
    }

    pub fn remove_entity(&mut self, entity: Entity) {
        if let Some(key) = self.stud_by_entity.remove(&entity) {
            self.remove_stud_key(key);
        }
    }

    /// Re-derive the block columns under chunk column (cx, cz).
    fn recompute(&mut self, cx: i32, cz: i32) {
        let blocks = CHUNK_BLOCKS as i32;
        let studs = self.stud_stacks.get(&(cx, cz));
        for bz in 0..CHUNK_BLOCKS {
            for bx in 0..CHUNK_BLOCKS {
                let block = (cx * blocks + bx as i32, cz * blocks + bz as i32);
                let Some(studs) = studs else {
                    self.columns.remove(&block);
                    continue;
                };
                let column = bx + CHUNK_BLOCKS * bz;
                // Highest layer first: the first with a top in this column
                // wins. A loaded column with no brick reads as NaN: there is
                // no ground to stand the camera or a pick on.
                let y = studs
                    .iter()
                    .rev()
                    .find(|(_, t)| t.surface[column] > 0.0)
                    .map_or(f32::NAN, |(layer, t)| *layer as f32 * STUD_CHUNK_HEIGHT_UNITS as f32 + t.surface[column]);
                self.columns.insert(block, (y, y));
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

/// World chunk key (cx, stud layer, cz) of stud chunk `coords` in `sim`'s region.
fn world_stud_key(coords: game::StudChunkCoords, sim: &SimEntity) -> [i32; 3] {
    let [ox, _, oz] = sim.region.world_offset().map(|v| (v / CHUNK_EDGE as f32).round() as i32);
    [ox + coords.x as i32, coords.y, oz + coords.z as i32]
}

/// Keep the grid in step with the stud chunks.
pub fn update_height_grid(
    mut grid: ResMut<HeightGrid>,
    changed_studs: Query<(Entity, &SimStudChunk, &SimEntity), Changed<SimStudChunk>>,
    mut removed_studs: RemovedComponents<SimStudChunk>,
) {
    for entity in removed_studs.read() {
        grid.remove_entity(entity);
    }
    for (entity, stud, sim) in &changed_studs {
        grid.insert_stud_chunk(entity, world_stud_key(stud.coords, sim), stud_chunk_tops(&stud.chunk));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filler(min: [u8; 3], size: [u8; 3]) -> game::StudBrick {
        game::StudBrick { min, shape: game::StudShape::Filler { size }, material: 4, data: 0 }
    }

    /// A chunk filled to `plates` over its whole footprint.
    fn slab(plates: u8) -> StudTops {
        stud_chunk_tops(&StudChunk::new([filler([0, 0, 0], [32, plates, 32])]).unwrap())
    }

    #[test]
    fn stud_chunk_sets_surface() {
        let mut grid = HeightGrid::new();
        grid.insert_stud_chunk(Entity::from_raw_u32(1).unwrap(), [0, 0, 0], slab(50));
        let c = CHUNK_EDGE as f32 / 2.0;
        assert_eq!(grid.surface_at(c, c), Some(game::plate_units(50)));
        assert_eq!(grid.column_at(c, c), Some((game::plate_units(50), game::plate_units(50))));
        assert_eq!(grid.surface_range(), Some((game::plate_units(50), game::plate_units(50))));
    }

    #[test]
    fn column_height_is_the_highest_layer_and_falls_back_on_removal() {
        let mut grid = HeightGrid::new();
        let (low, high) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        let e = CHUNK_EDGE as i32;
        let layer = STUD_CHUNK_HEIGHT_UNITS as f32;
        let full = game::plate_units(game::STUD_CHUNK_PLATES as i32);
        grid.insert_stud_chunk(low, [3, 0, 4], slab(game::STUD_CHUNK_PLATES as u8));
        grid.insert_stud_chunk(high, [3, 1, 4], slab(5));
        let (x, z) = ((3 * e + 1) as f32, (4 * e + 1) as f32);
        assert_eq!(grid.surface_at(x, z), Some(layer + game::plate_units(5)));
        let v = grid.version;
        grid.remove_entity(high);
        assert_eq!(grid.surface_at(x, z), Some(full), "the full layer below");
        assert!(grid.version > v);
        grid.remove_entity(low);
        assert_eq!(grid.surface_at(x, z), None);
        assert_eq!(grid.column_at(x, z), None, "unloaded columns are dropped");
    }

    /// World heights add the layer's own y: layers below 0 count down from the
    /// region floor.
    #[test]
    fn heights_span_negative_layers() {
        let layer = STUD_CHUNK_HEIGHT_UNITS as f32;
        let mut grid = HeightGrid::new();
        let (deep, top) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        let full = game::plate_units(game::STUD_CHUNK_PLATES as i32);
        grid.insert_stud_chunk(deep, [0, -2, 0], slab(game::STUD_CHUNK_PLATES as u8));
        assert_eq!(grid.surface_at(1.0, 1.0), Some(-2.0 * layer + full), "layer -2's full top is one layer under 0");
        assert!((full - layer).abs() < 1e-3);
        grid.insert_stud_chunk(top, [0, -1, 0], slab(3));
        assert_eq!(grid.surface_at(1.0, 1.0), Some(-layer + game::plate_units(3)), "layer -1 stacks on top");
        // An entity filed again under another key leaves its old one.
        grid.insert_stud_chunk(top, [0, 1, 0], slab(3));
        assert_eq!(grid.surface_at(1.0, 1.0), Some(layer + game::plate_units(3)));
        grid.remove_entity(top);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(-2.0 * layer + full));
    }

    #[test]
    fn removing_stud_entity_clears_columns() {
        let mut grid = HeightGrid::new();
        let s = Entity::from_raw_u32(2).unwrap();
        grid.insert_stud_chunk(s, [1, 2, 3], stud_chunk_tops(&StudChunk::new([filler([0, 0, 0], [2, 5, 2])]).unwrap()));
        let (x, z) = (CHUNK_EDGE as f32 + 1.0, 3.0 * CHUNK_EDGE as f32 + 1.0);
        assert_eq!(grid.surface_at(x, z), Some(2.0 * STUD_CHUNK_HEIGHT_UNITS as f32 + game::plate_units(5)));
        assert_eq!(grid.surface_at(x + 20.0, z), None, "a column beside stays empty (no top)");
        grid.remove_entity(s);
        assert_eq!(grid.column_at(x, z), None);
    }

    #[test]
    fn block_column_takes_the_max_of_its_stud_columns() {
        // Block column 0 covers studs x 0..2, z 0..2: several bricks of differing top.
        let chunk = StudChunk::new([
            filler([0, 0, 0], [1, 3, 1]),
            filler([1, 0, 0], [1, 9, 1]),
            filler([0, 0, 1], [1, 6, 1]),
        ])
        .unwrap();
        let tops = stud_chunk_tops(&chunk);
        assert_eq!(tops.surface[0], game::plate_units(9));
        assert_eq!(tops.surface[1], 0.0);
    }

    #[test]
    fn empty_upper_stud_layer_falls_back_to_the_lower() {
        let mut grid = HeightGrid::new();
        let (lo, hi) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        grid.insert_stud_chunk(lo, [0, 0, 0], stud_chunk_tops(&StudChunk::new([filler([0, 0, 0], [32, 7, 32])]).unwrap()));
        // The upper layer only covers stud column (30, 30), not the sampled block column 0.
        grid.insert_stud_chunk(hi, [0, 1, 0], stud_chunk_tops(&StudChunk::new([filler([30, 0, 30], [1, 4, 1])]).unwrap()));
        assert_eq!(grid.surface_at(1.0, 1.0), Some(game::plate_units(7)));
        let far = CHUNK_EDGE as f32 - 1.0;
        assert_eq!(grid.surface_at(far, far), Some(STUD_CHUNK_HEIGHT_UNITS as f32 + game::plate_units(4)));
    }

    /// A stud chunk's key is its region-local chunk plus the region's world
    /// offset, in chunks — negative regions and layers included.
    #[test]
    fn world_stud_key_adds_the_region_offset() {
        let per_region = game::REGION_CHUNKS as i32;
        let sim = |x, z| SimEntity { region: game::RegionCoords::new(x, z), key: Default::default() };
        let local = game::StudChunkCoords { x: 3, y: -2, z: 1 };
        assert_eq!(world_stud_key(local, &sim(0, 0)), [3, -2, 1]);
        assert_eq!(world_stud_key(local, &sim(2, -1)), [2 * per_region + 3, -2, -per_region + 1]);
        assert_eq!(world_stud_key(local, &sim(-1, 3)), [-per_region + 3, -2, 3 * per_region + 1]);
    }
}
