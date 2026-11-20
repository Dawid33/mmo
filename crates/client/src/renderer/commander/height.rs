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
    /// The block at `surface` (the map's colour; `AIR` where `surface` is 0).
    pub surface_block: [BlockId; COLUMNS],
}

impl Default for ChunkTops {
    fn default() -> Self {
        Self { surface: [0; COLUMNS], ground: [0; COLUMNS], surface_block: [BlockId::AIR; COLUMNS] }
    }
}

/// One loaded block column as the map paints it (`renderer/map/paint.rs`): the top of its top block
/// (whole metres, world y), that block, and its recipe water's top block, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapColumn {
    pub height_m: i16,
    pub block: BlockId,
    pub water_top: Option<i16>,
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
    // The surface is the map's chunk top (slopes as full blocks there too).
    let surface = game::map_tile::chunk_top(chunk);
    let centre = BLOCK_VOXELS / 2;
    for bz in 0..CHUNK_BLOCKS {
        for bx in 0..CHUNK_BLOCKS {
            let (vx, vz) = (bx * BLOCK_VOXELS + centre, bz * BLOCK_VOXELS + centre);
            let column = bx + CHUNK_BLOCKS * bz;
            tops.surface[column] = surface.0[column].map_or(0, |(y, _)| y);
            tops.surface_block[column] = surface.0[column].map_or(BlockId::AIR, |(_, id)| id);
            tops.ground[column] = match chunk.column_top(vx as u32, vz as u32) {
                Some((y, id)) if !game::block_props().is_liquid(id) => (y + 1) as u16,
                Some(_) => top_not(chunk, vx, vz).map_or(0, |y| (y + 1) as u16),
                None => 0,
            };
        }
    }
    // Slopes count as full blocks for the ground too (the centre-column sample is half height on some facings).
    for (bi, chisel) in &chunk.chisel {
        if game::slope::classify(chisel).is_none() {
            continue;
        }
        let id = chunk.blocks.get(*bi).id;
        if id == BlockId::AIR || game::block_props().is_liquid(id) {
            continue;
        }
        let (bx, by, bz) = bi.xyz();
        let column = bx + CHUNK_BLOCKS * bz;
        let top = ((by + 1) * BLOCK_VOXELS) as u16;
        tops.ground[column] = tops.ground[column].max(top);
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
    /// Per loaded block column with a top block: what the map paints there.
    map_columns: HashMap<(i32, i32), MapColumn>,
    /// Chunk columns (cx, cz) re-derived since the map last took them ([`Self::take_dirty`]).
    dirty: std::collections::BTreeSet<(i32, i32)>,
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
            map_columns: HashMap::new(),
            dirty: std::collections::BTreeSet::new(),
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

    /// Block column (x, z) as the map paints it; `None` where nothing (or only air) is loaded.
    pub fn map_column(&self, x: i32, z: i32) -> Option<MapColumn> {
        self.map_columns.get(&(x, z)).copied()
    }

    /// The chunk columns (cx, cz) re-derived since the last call.
    pub fn take_dirty(&mut self) -> std::collections::BTreeSet<(i32, i32)> {
        std::mem::take(&mut self.dirty)
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
    /// does not. Replaces the region's earlier recipe water. `region` is the
    /// copy its tree is drawn at (`RegionRoots::alias`), where its chunks are keyed.
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
                    self.map_columns.remove(&block);
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
                let (mut surface, ground) = (top(|t| &t.surface), top(|t| &t.ground));
                // The map's column: the highest chunk's top block, in whole metres.
                let map = stack.iter().rev().find(|(_, t)| t.surface[column] > 0).map(|(cy, t)| {
                    let voxels = *cy as i64 * CHUNK_EDGE as i64 + i64::from(t.surface[column]);
                    let m = (voxels + BLOCK_VOXELS as i64 - 1).div_euclid(BLOCK_VOXELS as i64);
                    MapColumn {
                        height_m: m.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
                        block: t.surface_block[column],
                        water_top: self.water_tops.get(&block).copied(),
                    }
                });
                match map {
                    Some(c) => self.map_columns.insert(block, c),
                    None => self.map_columns.remove(&block),
                };
                // Recipe water stands over the ground; the column must be loaded.
                if let Some(&water) = self.water_tops.get(&block) {
                    surface = surface.max((i32::from(water) + 1) as f32 * BLOCK_VOXELS as f32);
                }
                self.columns.insert(block, (surface, ground));
            }
        }
        self.dirty.insert((cx, cz));
        self.version += 1;
    }

    #[cfg(test)]
    pub fn set_column_for_test(&mut self, at: Vec2, height: f32) {
        self.columns.insert(block_of(at.x, at.y), (height, height));
        self.version += 1;
    }
}

/// World chunk coordinates (render space) of a sim chunk entity at `tf`
/// under a region root drawn at `alias` (`RegionRoots::alias`).
fn world_chunk_key(tf: &Transform, alias: game::RegionId) -> [i32; 3] {
    let offset = Vec3::from_array(alias.world_offset());
    let world = (tf.translation + offset) / CHUNK_EDGE as f32;
    [world.x.round() as i32, world.y.round() as i32, world.z.round() as i32]
}

/// Keep the grid in step with the sim chunks' blocks.
pub fn update_height_grid(
    mut grid: ResMut<HeightGrid>,
    mut msgs: MessageReader<super::tiles::TileMirrorMsg>,
    changed: Query<(Entity, &SimChunk, &Transform, &SimEntity), Changed<SimChunk>>,
    mut removed: RemovedComponents<SimChunk>,
    roots: Res<crate::renderer::bridge::RegionRoots>,
) {
    for msg in msgs.read() {
        if let super::tiles::TileMirrorMsg::Reset { region, ground, .. } = msg {
            // A region already released again has nothing to key.
            if let Some(alias) = roots.alias(*region) {
                grid.set_recipe_water(alias, ground);
            }
        }
    }
    for entity in removed.read() {
        grid.remove_entity(entity);
    }
    for (entity, chunk, tf, sim) in &changed {
        if let Some(alias) = roots.alias(sim.region) {
            grid.insert_chunk(entity, world_chunk_key(tf, alias), chunk_tops(&chunk.0));
        }
    }
}

