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
use game::{BlockId, BlockIndex, Chunk, StudChunk, BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_UNITS, STUD_CHUNK_HEIGHT_UNITS};

use crate::renderer::bricks::SimChunk;
use crate::renderer::bridge::SimEntity;
use crate::renderer::stud::SimStudChunk;

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

/// Topmost voxel of voxel column `(vx, vz)` that is neither air nor liquid
/// (local Y), read from blocks and chisel without expanding anything.
fn top_not(chunk: &Chunk, vx: usize, vz: usize) -> Option<usize> {
    let (bx, lx) = (vx / BLOCK_VOXELS, vx % BLOCK_VOXELS);
    let (bz, lz) = (vz / BLOCK_VOXELS, vz % BLOCK_VOXELS);
    let shown = |id: BlockId| id != BlockId::AIR && !game::block_props().is_liquid(id);
    for by in (0..CHUNK_BLOCKS).rev() {
        let bi = BlockIndex::from_xyz(bx, by, bz);
        let base = by * BLOCK_VOXELS;
        match chunk.chisel.get(&bi) {
            // A slope reads as a full block: its centre column would give half height on some facings.
            Some(chisel) if game::slope::classify(chisel).is_some() => {
                if shown(chunk.blocks.get(bi).id) {
                    return Some(base + BLOCK_VOXELS - 1);
                }
            }
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
pub fn chunk_tops(chunk: &Chunk) -> ChunkTops {
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
                Some((y, id)) if !game::block_props().is_liquid(id) => (y + 1) as u16,
                Some(_) => top_not(chunk, vx, vz).map_or(0, |y| (y + 1) as u16),
                None => 0,
            };
        }
    }
    // Slopes count as full blocks (the centre-column sample is half height on some facings).
    for (bi, chisel) in &chunk.chisel {
        if game::slope::classify(chisel).is_none() {
            continue;
        }
        let id = chunk.blocks.get(*bi).id;
        if id == BlockId::AIR {
            continue;
        }
        let (bx, by, bz) = bi.xyz();
        let column = bx + CHUNK_BLOCKS * bz;
        let top = ((by + 1) * BLOCK_VOXELS) as u16;
        tops.surface[column] = tops.surface[column].max(top);
        if !game::block_props().is_liquid(id) {
            tops.ground[column] = tops.ground[column].max(top);
        }
    }
    tops
}

/// One stud chunk's contribution: per block column (`bx + CHUNK_BLOCKS * bz`,
/// each covering 2x2 stud columns), the highest brick top over those stud
/// columns in sim units above the stud chunk's floor; `0.0` = none. Studs have
/// no water, so a stud surface is its ground.
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
    /// non-air voxel and of its topmost solid (non-air, non-liquid) voxel,
    /// world y in sim units; NaN where the column has none. Sparse: it covers
    /// whatever the near window has loaded, across regions.
    columns: HashMap<(i32, i32), (f32, f32)>,
    /// Per chunk column (cx, cz), the tops of each loaded chunk by its y.
    stacks: HashMap<(i32, i32), BTreeMap<i32, ChunkTops>>,
    /// Which chunk an entity's voxels were filed under, for removals.
    by_entity: HashMap<Entity, [i32; 3]>,
    /// Recipe water per world block column: the topmost water block y
    /// (`ColumnGround::water_top`), from the regions' ground recipes.
    water_tops: HashMap<(i32, i32), i16>,
    /// Per chunk column (cx, cz), the tops of each loaded stud chunk by its layer.
    stud_stacks: HashMap<(i32, i32), BTreeMap<i32, StudTops>>,
    /// Which stud chunk (world cx, layer, cz) an entity was filed under.
    stud_by_entity: HashMap<Entity, [i32; 3]>,
    /// Bumped on every change, so readers (the minimap) can rebuild lazily.
    pub version: u64,
}

/// The block column containing world (x, z).
fn block_of(x: f32, z: f32) -> (i32, i32) {
    let b = BLOCK_VOXELS as f32;
    ((x / b).floor() as i32, (z / b).floor() as i32)
}

impl HeightGrid {
    pub fn new() -> Self {
        Self {
            columns: HashMap::new(),
            stacks: HashMap::new(),
            by_entity: HashMap::new(),
            water_tops: HashMap::new(),
            stud_stacks: HashMap::new(),
            stud_by_entity: HashMap::new(),
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

    /// Take `region`'s recipe water (its `Reset`'s ground recipes): a column's
    /// surface then reaches the water's top (`water_top + 1`), its ground
    /// does not. Replaces the region's earlier recipe water.
    pub fn set_recipe_water(&mut self, region: game::RegionId, ground: &std::collections::BTreeMap<game::ColumnCoords, game::ColumnGround>) {
        let n = game::REGION_BLOCKS as i32;
        let (x0, z0) = (region.x * n, region.z * n);
        self.water_tops.retain(|&(x, z), _| !(x0..x0 + n).contains(&x) || !(z0..z0 + n).contains(&z));
        for (coords, g) in ground {
            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    if g.recipe_water(lx, lz).is_some() {
                        let key = (x0 + (coords.x * CHUNK_BLOCKS + lx) as i32, z0 + (coords.z * CHUNK_BLOCKS + lz) as i32);
                        self.water_tops.insert(key, g.water_top[lx * CHUNK_BLOCKS + lz]);
                    }
                }
            }
        }
        let per = game::REGION_CHUNKS as i32;
        for cz in region.z * per..(region.z + 1) * per {
            for cx in region.x * per..(region.x + 1) * per {
                if self.stacks.contains_key(&(cx, cz)) {
                    self.recompute(cx, cz);
                }
            }
        }
        self.version += 1;
    }

    /// File a chunk's tops under its key (chunk coordinates, world / `CHUNK_UNITS`).
    /// Whether the sim chunk at world chunk `key` is loaded (materialised),
    /// all-air chunks included.
    pub fn has_chunk(&self, key: [i32; 3]) -> bool {
        self.stacks.get(&(key[0], key[2])).is_some_and(|s| s.contains_key(&key[1]))
    }

    pub fn insert_chunk(&mut self, entity: Entity, key: [i32; 3], tops: ChunkTops) {
        if let Some(old) = self.by_entity.insert(entity, key) {
            if old != key {
                self.remove_key(old);
            }
        }
        self.stacks.entry((key[0], key[2])).or_default().insert(key[1], tops);
        self.recompute(key[0], key[2]);
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
        let studs = self.stud_stacks.get(&(cx, cz));
        for bz in 0..CHUNK_BLOCKS {
            for bx in 0..CHUNK_BLOCKS {
                let block = (cx * blocks + bx as i32, cz * blocks + bz as i32);
                if stack.is_none() && studs.is_none() {
                    self.columns.remove(&block);
                    continue;
                }
                let column = bx + CHUNK_BLOCKS * bz;
                let top = |pick: fn(&ChunkTops) -> &[u16; COLUMNS]| {
                    // Highest chunk first: the first with a top in this column
                    // wins. A loaded column of only air reads as NaN: there is
                    // no ground to stand the camera or a pick on.
                    stack
                        .and_then(|s| s.iter().rev().find(|(_, t)| pick(t)[column] > 0))
                        .map_or(f32::NAN, |(cy, t)| *cy as f32 * chunk + pick(t)[column] as f32)
                };
                let (mut surface, mut ground) = (top(|t| &t.surface), top(|t| &t.ground));
                // Stud chunks: highest layer with a top; f32::max drops the NaN of "no voxels".
                if let Some((layer, t)) = studs.and_then(|s| s.iter().rev().find(|(_, t)| t.surface[column] > 0.0)) {
                    let y = *layer as f32 * STUD_CHUNK_HEIGHT_UNITS as f32 + t.surface[column];
                    surface = surface.max(y);
                    ground = ground.max(y);
                }
                // Recipe water stands over the ground; the column must be loaded.
                if let Some(&water) = self.water_tops.get(&block) {
                    surface = surface.max((i32::from(water) + 1) as f32 * BLOCK_VOXELS as f32);
                }
                self.columns.insert(block, (surface, ground));
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
    mut msgs: MessageReader<super::tiles::TileMirrorMsg>,
    changed: Query<(Entity, &SimChunk, &Transform, &SimEntity), Changed<SimChunk>>,
    mut removed: RemovedComponents<SimChunk>,
    changed_studs: Query<(Entity, &SimStudChunk, &SimEntity), Changed<SimStudChunk>>,
    mut removed_studs: RemovedComponents<SimStudChunk>,
) {
    for msg in msgs.read() {
        if let super::tiles::TileMirrorMsg::Reset { region, ground, .. } = msg {
            grid.set_recipe_water(*region, ground);
        }
    }
    for entity in removed.read() {
        grid.remove_entity(entity);
    }
    for entity in removed_studs.read() {
        grid.remove_entity(entity);
    }
    for (entity, stud, sim) in &changed_studs {
        let [ox, _, oz] = sim.region.world_offset().map(|v| (v / CHUNK_EDGE as f32).round() as i32);
        let key = [ox + stud.coords.x as i32, stud.coords.y, oz + stud.coords.z as i32];
        grid.insert_stud_chunk(entity, key, stud_chunk_tops(&stud.chunk));
    }
    for (entity, chunk, tf, sim) in &changed {
        grid.insert_chunk(entity, world_chunk_key(tf, sim), chunk_tops(&chunk.0));
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
        game::install_block_props(crate::blocks::load_registry().props());
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
            assert_eq!(chunk_tops(&chunk), tops_from_voxels(&chunk), "h={h} w={w}");
        }
        // A tall chunk's heights are voxel heights past u8 range.
        let tops = chunk_tops(&layered(CHUNK_EDGE, CHUNK_EDGE, STONE));
        assert_eq!(tops.surface[0], CHUNK_EDGE as u16);
    }

    #[test]
    fn chunk_tops_find_the_top_face_and_the_ground_under_water() {
        let tops = chunk_tops(&layered(8, 20, STONE));
        assert_eq!(tops.surface, [20; COLUMNS]);
        assert_eq!(tops.ground, [8; COLUMNS]);
        assert_eq!(chunk_tops(&layered(0, 0, STONE)), ChunkTops::default());
    }

    /// Recipe water raises a column's surface to its top (`water_top + 1`)
    /// but never its ground, with or without a water block in the chunks.
    #[test]
    fn recipe_water_raises_surface_not_ground() {
        use std::collections::BTreeMap;
        let bv = BLOCK_VOXELS as f32;
        let mut grid = HeightGrid::new();
        // One stone block at world block y 64 (chunk y 64 / CHUNK_BLOCKS, block 0 of it).
        let cy = 64 / CHUNK_BLOCKS as i32;
        assert_eq!(64 % CHUNK_BLOCKS, 0);
        let entity = Entity::from_raw_u32(1).unwrap();
        grid.insert_chunk(entity, [0, cy, 0], chunk_tops(&layered(BLOCK_VOXELS, BLOCK_VOXELS, STONE)));
        let (x, z) = (bv * 3.0 + 1.0, bv * 2.0 + 1.0);
        assert_eq!(grid.column_at(x, z), Some((65.0 * bv, 65.0 * bv)), "dry before the recipe");
        let mut ground = game::ColumnGround::plain(vec![64; game::COLUMN_FOOTPRINT].into_boxed_slice(), 0, STONE, STONE);
        ground.water_top[3 * CHUNK_BLOCKS + 2] = 70;
        let v = grid.version;
        grid.set_recipe_water(game::RegionCoords::new(0, 0), &BTreeMap::from([(game::ColumnCoords::new(0, 0), ground)]));
        assert!(grid.version > v);
        assert_eq!(grid.column_at(x, z), Some((71.0 * bv, 65.0 * bv)));
        // A column beside it stays dry.
        assert_eq!(grid.column_at(bv * 4.0 + 1.0, z), Some((65.0 * bv, 65.0 * bv)));
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
        let tops = chunk_tops(&chunk);
        let mut want = [0u16; COLUMNS];
        want[1] = (BLOCK_VOXELS - 1) as u16;
        assert_eq!(tops.surface, want);
    }

    #[test]
    fn slope_counts_as_full_height() {
        use game::slope::{slope_chisel, Side, SlopeShape};
        for side in [Side::NegX, Side::PosX, Side::NegZ, Side::PosZ] {
            let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
            let bi = BlockIndex::from_xyz(2, 1, 3);
            chunk.blocks.set(bi, Block::new(STONE));
            chunk.chisel.insert(bi, slope_chisel(STONE, SlopeShape::Straight { low: side }));
            let tops = chunk_tops(&chunk);
            let column = 2 + CHUNK_BLOCKS * 3;
            let full = (2 * BLOCK_VOXELS) as u16;
            assert_eq!(tops.surface[column], full, "{side:?}");
            assert_eq!(tops.ground[column], full, "{side:?}");
            assert_eq!(top_not(&chunk, 2 * BLOCK_VOXELS, 3 * BLOCK_VOXELS), Some(full as usize - 1), "{side:?}");
        }
    }

    #[test]
    fn column_height_is_the_highest_chunk_and_falls_back_on_removal() {
        let mut grid = HeightGrid::new();
        let (low, high) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        let e = CHUNK_EDGE as i32;
        grid.insert_chunk(low, [3, 0, 4], chunk_tops(&layered(CHUNK_EDGE, CHUNK_EDGE, STONE)));
        grid.insert_chunk(high, [3, 1, 4], chunk_tops(&layered(5, 5, STONE)));
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
        let full = chunk_tops(&layered(CHUNK_EDGE, CHUNK_EDGE, STONE));
        assert_eq!(full.surface, [CHUNK_EDGE as u16; COLUMNS]);
        let e = CHUNK_EDGE as f32;
        let mut grid = HeightGrid::new();
        let (deep, top) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        grid.insert_chunk(deep, [0, -2, 0], full);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(-e), "chunk y -2's full top is at -CHUNK_UNITS");
        grid.insert_chunk(top, [0, -1, 0], chunk_tops(&layered(3, 3, STONE)));
        assert_eq!(grid.surface_at(1.0, 1.0), Some(3.0 - e), "chunk y -1 stacks on top");
        grid.remove_entity(top);
        grid.insert_chunk(top, [0, 1, 0], full);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(2.0 * e), "a full chunk at y 1 tops out at 2·CHUNK_UNITS");
    }

    fn filler(min: [u8; 3], size: [u8; 3]) -> game::StudBrick {
        game::StudBrick { min, shape: game::StudShape::Filler { size }, material: 4, data: 0 }
    }

    #[test]
    fn stud_chunk_sets_surface() {
        let chunk = StudChunk::new([filler([0, 0, 0], [32, 50, 32])]).unwrap();
        let mut grid = HeightGrid::new();
        grid.insert_stud_chunk(Entity::from_raw_u32(1).unwrap(), [0, 0, 0], stud_chunk_tops(&chunk));
        let c = CHUNK_EDGE as f32 / 2.0;
        assert_eq!(grid.surface_at(c, c), Some(game::plate_units(50)));
        assert_eq!(grid.column_at(c, c), Some((game::plate_units(50), game::plate_units(50))));
    }

    #[test]
    fn stud_and_voxel_take_the_max() {
        let mut grid = HeightGrid::new();
        let (v, s) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        let stud = stud_chunk_tops(&StudChunk::new([filler([0, 0, 0], [32, 50, 32])]).unwrap());
        grid.insert_chunk(v, [0, 0, 0], chunk_tops(&layered(5, 5, STONE)));
        grid.insert_stud_chunk(s, [0, 0, 0], stud);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(game::plate_units(50)));
        grid.remove_entity(s);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(5.0));
        grid.insert_chunk(v, [0, -1, 0], chunk_tops(&layered(CHUNK_EDGE, CHUNK_EDGE, STONE)));
        grid.insert_stud_chunk(s, [0, 0, 0], stud);
        assert_eq!(grid.surface_at(1.0, 1.0), Some(game::plate_units(50)));
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
    fn voxel_higher_than_stud_wins() {
        let mut grid = HeightGrid::new();
        let (v, s) = (Entity::from_raw_u32(1).unwrap(), Entity::from_raw_u32(2).unwrap());
        grid.insert_stud_chunk(s, [0, 0, 0], stud_chunk_tops(&StudChunk::new([filler([0, 0, 0], [32, 5, 32])]).unwrap()));
        grid.insert_chunk(v, [0, 0, 0], chunk_tops(&layered(100, 100, STONE)));
        assert_eq!(grid.surface_at(1.0, 1.0), Some(100.0));
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
