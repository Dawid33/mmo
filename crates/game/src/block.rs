//! The block layer: a `Block` (16³ voxels = 1 m³) is the authoritative gameplay
//! unit; voxels are a subdivision derived on demand via [`derive_voxels`].
//! Sub-block detail ("chiseling") is sparse — only chiseled blocks carry a
//! [`ChiselData`], a canonical cuboid list. This module is Bevy-free and
//! deterministic.

use block_mesh::ndshape::ConstShape;
use parry3d::math::IVector;

use crate::registry::BlockId;
use crate::voxel::{BrickShape, Chunk, Voxel, BRICK_VOXEL_COUNT};

pub const BLOCK_VOXELS: usize = 16; // 1 block = 16³ voxels = 1 m³
/// Blocks per axis of a sim chunk — the unit that owns one entity, one rapier
/// body and one collider set. A BUILD-time dial (`VOXEL_CHUNK_BLOCKS`, set in
/// `.cargo/config.toml [env]`, `just chunk_blocks=N …`) for the same reason as
/// `REGION_BLOCKS`: client, server and the wasm LocalServer must agree on it
/// bit-exactly. Legal values are the powers of two `2..=32` (a chunk edge must
/// be a whole number of 32-voxel render bricks, i.e. an even block count, and
/// a `BlockIndex` must fit a `u16`); anything else fails the BUILD.
pub const CHUNK_BLOCKS: usize = parse_chunk_blocks(option_env!("VOXEL_CHUNK_BLOCKS"));

/// `option_env!` hands over a string; unset or empty means the default.
const fn parse_chunk_blocks(var: Option<&str>) -> usize {
    const DEFAULT: usize = 2;
    let Some(text) = var else { return DEFAULT };
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return DEFAULT;
    }
    let mut value = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit(), "VOXEL_CHUNK_BLOCKS must be a number");
        value = value * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    assert!(
        value.is_power_of_two() && value >= 2 && value <= 32,
        "VOXEL_CHUNK_BLOCKS must be a power of two, 2..=32"
    );
    value
}
pub const CHUNK_BLOCK_COUNT: usize = CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS;
/// Sim-chunk edge in voxels = world units. Not the render brick (`BRICK_VOXELS`).
pub const CHUNK_UNITS: usize = CHUNK_BLOCKS * BLOCK_VOXELS;
/// Bricks along one chunk edge.
pub const BRICKS_PER_CHUNK_EDGE: usize = CHUNK_UNITS / crate::voxel::BRICK_VOXELS;
const _: () = assert!(CHUNK_UNITS % crate::voxel::BRICK_VOXELS == 0);
pub const BLOCK_VOXEL_COUNT: usize = BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS; // = 4096
/// Edge of a half block, and the cell size of the half-block collider grid.
pub const HALF_BLOCK_VOXELS: usize = BLOCK_VOXELS / 2; // = 8

/// Linear voxel index within a 32³ render brick, matching `BrickShape` (x fastest).
/// Reuses `voxel::BrickShape` so the 32³ layout has a single source of truth.
pub fn voxel_index(x: usize, y: usize, z: usize) -> usize {
    BrickShape::linearize([x as u32, y as u32, z as u32]) as usize
}

/// Index of a block within a chunk, linear over `CHUNK_BLOCKS³`
/// (x fastest, then y, then z). `u16` so it covers a 16³ chunk (4096).
/// `Ord` so it can key a `BTreeMap` deterministically.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, state_hash::StateHash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct BlockIndex(pub u16);

const _: () = assert!(CHUNK_BLOCK_COUNT <= u16::MAX as usize + 1);

impl BlockIndex {
    /// The block at linear position `i` (`0..CHUNK_BLOCK_COUNT`).
    pub fn from_linear(i: usize) -> Self {
        debug_assert!(i < CHUNK_BLOCK_COUNT, "block index {i} out of a {CHUNK_BLOCK_COUNT}-block chunk");
        BlockIndex(i as u16)
    }
    /// Linear position of this block, `0..CHUNK_BLOCK_COUNT`.
    pub fn linear(self) -> usize {
        self.0 as usize
    }
    pub fn from_xyz(bx: usize, by: usize, bz: usize) -> Self {
        Self::from_linear(bx + by * CHUNK_BLOCKS + bz * CHUNK_BLOCKS * CHUNK_BLOCKS)
    }
    pub fn xyz(self) -> (usize, usize, usize) {
        let i = self.0 as usize;
        (
            i % CHUNK_BLOCKS,
            (i / CHUNK_BLOCKS) % CHUNK_BLOCKS,
            i / (CHUNK_BLOCKS * CHUNK_BLOCKS),
        )
    }
}

/// The authoritative per-cell gameplay value. Compact; `data` reserved for
/// future block-state (zeroed for now). Hashes trivially.
#[derive(
    Copy, Clone, Default, PartialEq, Eq, Hash, state_hash::StateHash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct Block {
    pub id: BlockId,
    pub data: [u8; 3],
}

impl Block {
    pub fn new(id: BlockId) -> Self {
        Self { id, data: [0; 3] }
    }
}

/// A chiseled block's 16³ voxels, decoded: per voxel (x fastest, then y,
/// then z) the palette index it holds, `None` where it is empty.
pub type ChiselGrid = Vec<Option<u8>>;

/// Sub-block detail for a chiseled block, stored the way Vintage Story stores
/// microblocks: axis-aligned cuboids on the 16³ voxel grid, one `u32` each
/// (min x/y/z and max-1 x/y/z in 4 bits apiece, palette index in the top 8),
/// plus a small material palette. A half block is one cuboid.
///
/// The list is canonical: every constructor and edit goes through the dense
/// [`ChiselGrid`] and one fixed greedy merge ([`ChiselData::from_grid`]), so
/// equal voxels always give an equal list — equal serialization, equal
/// hashes. Undo restores a block's whole chisel entry, so no per-voxel inverse is
/// needed. Edit in bulk through the grid; [`ChiselData::set`] re-merges on
/// every call.
#[derive(Clone, Default, PartialEq, Eq, Hash, state_hash::StateHash, serde::Serialize, serde::Deserialize, Debug)]
pub struct ChiselData {
    cuboids: Vec<u32>,
    palette: Vec<BlockId>, // ≤ 256 materials, indexed by a cuboid's top byte
}

pub(crate) fn local_index(vx: usize, vy: usize, vz: usize) -> usize {
    vx + vy * BLOCK_VOXELS + vz * BLOCK_VOXELS * BLOCK_VOXELS
}

fn pack_cuboid(min: [usize; 3], max: [usize; 3], material: u8) -> u32 {
    (min[0] | min[1] << 4 | min[2] << 8 | (max[0] - 1) << 12 | (max[1] - 1) << 16 | (max[2] - 1) << 20)
        as u32
        | (material as u32) << 24
}

/// (min, exclusive max, palette index) of a packed cuboid.
fn unpack_cuboid(c: u32) -> ([usize; 3], [usize; 3], u8) {
    let f = |shift: u32| ((c >> shift) & 0xF) as usize;
    ([f(0), f(4), f(8)], [f(12) + 1, f(16) + 1, f(20) + 1], (c >> 24) as u8)
}

impl ChiselData {
    /// No voxels yet.
    pub fn new(palette: Vec<BlockId>) -> Self {
        assert!(palette.len() <= 256, "a chisel palette holds at most 256 materials");
        Self { cuboids: Vec::new(), palette }
    }

    /// One solid cuboid of `material`, voxels `min..max` (exclusive). Packed
    /// directly: a lone box is already its own canonical merge (the greedy
    /// walk starts at its min corner and grows to exactly its bounds).
    pub fn cuboid(material: BlockId, min: [usize; 3], max: [usize; 3]) -> Self {
        let empty = (0..3).any(|a| min[a] >= max[a]);
        debug_assert!(max.iter().all(|&m| m <= BLOCK_VOXELS), "cuboid past the block: {max:?}");
        let cuboids = if empty { Vec::new() } else { vec![pack_cuboid(min, max, 0)] };
        Self { cuboids, palette: vec![material] }
    }

    /// Built voxel by voxel: `f(x, y, z)` is the palette index there, or
    /// `None` for empty.
    pub fn from_fn(palette: Vec<BlockId>, f: impl Fn(usize, usize, usize) -> Option<u8>) -> Self {
        let mut grid = vec![None; BLOCK_VOXEL_COUNT];
        for vz in 0..BLOCK_VOXELS {
            for vy in 0..BLOCK_VOXELS {
                for vx in 0..BLOCK_VOXELS {
                    grid[local_index(vx, vy, vz)] = f(vx, vy, vz);
                }
            }
        }
        Self::from_grid(palette, &grid)
    }

    /// The canonical cuboid list of a dense grid. Visits voxels in grid order
    /// (x fastest); each filled voxel not yet covered starts a 1³ box that
    /// grows +x, +y, +z in turn, one layer at a time, while the new layer is
    /// entirely uncovered voxels of the same palette index.
    pub fn from_grid(palette: Vec<BlockId>, grid: &ChiselGrid) -> Self {
        assert!(palette.len() <= 256, "a chisel palette holds at most 256 materials");
        debug_assert_eq!(grid.len(), BLOCK_VOXEL_COUNT);
        let mut covered = vec![false; BLOCK_VOXEL_COUNT];
        let mut cuboids = Vec::new();
        for start in 0..BLOCK_VOXEL_COUNT {
            let Some(m) = grid[start] else { continue };
            if covered[start] {
                continue;
            }
            let min = [start % BLOCK_VOXELS, (start / BLOCK_VOXELS) % BLOCK_VOXELS, start / (BLOCK_VOXELS * BLOCK_VOXELS)];
            let mut max = [min[0] + 1, min[1] + 1, min[2] + 1];
            // The layer just past `max` on `axis` is growable.
            let growable = |max: &[usize; 3], axis: usize| {
                if max[axis] == BLOCK_VOXELS {
                    return false;
                }
                let (mut lo, mut hi) = (min, *max);
                lo[axis] = max[axis];
                hi[axis] = max[axis] + 1;
                (lo[2]..hi[2]).all(|z| {
                    (lo[1]..hi[1]).all(|y| {
                        (lo[0]..hi[0]).all(|x| {
                            let i = local_index(x, y, z);
                            grid[i] == Some(m) && !covered[i]
                        })
                    })
                })
            };
            loop {
                let mut grew = false;
                for axis in 0..3 {
                    if growable(&max, axis) {
                        max[axis] += 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            for z in min[2]..max[2] {
                for y in min[1]..max[1] {
                    for x in min[0]..max[0] {
                        covered[local_index(x, y, z)] = true;
                    }
                }
            }
            cuboids.push(pack_cuboid(min, max, m));
        }
        Self { cuboids, palette }
    }

    /// The dense grid (x fastest, then y, then z).
    pub fn grid(&self) -> ChiselGrid {
        let mut grid = vec![None; BLOCK_VOXEL_COUNT];
        for &c in &self.cuboids {
            let (min, max, m) = unpack_cuboid(c);
            for z in min[2]..max[2] {
                for y in min[1]..max[1] {
                    for x in min[0]..max[0] {
                        grid[local_index(x, y, z)] = Some(m);
                    }
                }
            }
        }
        grid
    }

    /// Per voxel (grid order) the material it shows: `AIR` where empty, or
    /// where its palette index has no material.
    pub fn materials(&self) -> Vec<BlockId> {
        self.grid().into_iter().map(|m| m.map_or(BlockId::AIR, |m| self.material(m))).collect()
    }

    fn material(&self, index: u8) -> BlockId {
        self.palette.get(index as usize).copied().unwrap_or(BlockId::AIR)
    }

    pub fn palette(&self) -> &[BlockId] {
        &self.palette
    }

    /// The cuboids as (min, exclusive max, material).
    pub fn cuboids(&self) -> impl Iterator<Item = ([usize; 3], [usize; 3], BlockId)> + '_ {
        self.cuboids.iter().map(|&c| {
            let (min, max, m) = unpack_cuboid(c);
            (min, max, self.material(m))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.cuboids.is_empty()
    }

    /// Fill (`solid`) or clear one voxel. Decodes and re-merges the whole
    /// block: for many voxels, edit a [`ChiselData::grid`] and rebuild with
    /// [`ChiselData::from_grid`] instead.
    pub fn set(&mut self, vx: usize, vy: usize, vz: usize, solid: bool, material_idx: u8) {
        let mut grid = self.grid();
        grid[local_index(vx, vy, vz)] = solid.then_some(material_idx);
        *self = Self::from_grid(std::mem::take(&mut self.palette), &grid);
    }

    fn index_at(&self, vx: usize, vy: usize, vz: usize) -> Option<u8> {
        self.cuboids.iter().find_map(|&c| {
            let (min, max, m) = unpack_cuboid(c);
            let inside = (min[0]..max[0]).contains(&vx)
                && (min[1]..max[1]).contains(&vy)
                && (min[2]..max[2]).contains(&vz);
            inside.then_some(m)
        })
    }

    pub fn is_solid(&self, vx: usize, vy: usize, vz: usize) -> bool {
        self.index_at(vx, vy, vz).is_some()
    }

    pub fn material_at(&self, vx: usize, vy: usize, vz: usize) -> BlockId {
        self.index_at(vx, vy, vz).map_or(BlockId::AIR, |m| self.material(m))
    }
}

/// Chunk collider cells split by grid scale, each solid region at the
/// coarsest aligned cell it fills: whole blocks on the block grid, 8³
/// half-block cells on the half-block grid, and the remaining chisel voxels
/// on the voxel grid. Together they cover exactly the solid cells of
/// [`derive_voxels`] — this is that function's collider-facing counterpart,
/// kept coarse instead of expanded to the 32³ grid. Chiseled and plain blocks
/// go through the same rule: a chisel that fills its block is a block cell, a
/// half-block chisel is four half cells.
#[derive(Debug, Clone, PartialEq)]
pub struct ColliderCells {
    /// Block-grid keys (0..CHUNK_BLOCKS per axis) of wholly solid blocks.
    pub blocks: Vec<IVector>,
    /// Half-block-grid keys (0..CHUNK_BLOCKS*2 per axis) of wholly solid 8³
    /// octants of chiseled blocks.
    pub halves: Vec<IVector>,
    /// Voxel-grid keys (0..CHUNK_BLOCKS*BLOCK_VOXELS per axis) of the other
    /// solid chisel voxels (occupied with a non-air material).
    pub voxels: Vec<IVector>,
}

impl ColliderCells {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.halves.is_empty() && self.voxels.is_empty()
    }

    /// The non-empty grids as (cell edge in voxels, keys), coarsest first:
    /// one parry `Voxels` collider each. Every collider builder walks this,
    /// so all of them attach the same shapes in the same order.
    pub fn grids(&self) -> impl Iterator<Item = (f32, &[IVector])> {
        [
            (BLOCK_VOXELS as f32, self.blocks.as_slice()),
            (HALF_BLOCK_VOXELS as f32, self.halves.as_slice()),
            (1.0, self.voxels.as_slice()),
        ]
        .into_iter()
        .filter(|(_, cells)| !cells.is_empty())
    }

    /// True iff every block of the chunk is wholly solid, i.e. its collision
    /// volume is exactly the full 32³ chunk cube.
    pub fn is_full_cube(&self) -> bool {
        self.blocks.len() == CHUNK_BLOCK_COUNT
    }
}

/// Decompose a chunk's block layer into collider cells. Solidity matches
/// [`derive_voxels`]: an unchiseled block is solid iff its id is non-air; a
/// chisel voxel is solid iff occupied with a non-air palette material.
/// Deterministic: blocks in `BlockIndex` order; within a chiseled block,
/// octants x-fastest, and an octant's voxels x-outer/z-inner.
pub fn collider_cells(chunk: &Chunk) -> ColliderCells {
    let mut out = ColliderCells { blocks: Vec::new(), halves: Vec::new(), voxels: Vec::new() };
    let chisel = &chunk.chisel;
    // A uniform unchiseled chunk: one block key per block, or none for air.
    if let (Some(block), true) = (chunk.blocks.uniform(), chisel.is_empty()) {
        if block.id != BlockId::AIR {
            out.blocks = (0..CHUNK_BLOCK_COUNT)
                .map(|i| {
                    let (bx, by, bz) = BlockIndex::from_linear(i).xyz();
                    IVector::new(bx as i32, by as i32, bz as i32)
                })
                .collect();
        }
        return out;
    }
    for (bi, block) in chunk.blocks.iter() {
        push_block_cells(&mut out, bi, block, chisel.get(&bi));
    }
    out
}

/// The collider cells of block `i` of `chunk` alone, in the chunk-local keys
/// [`collider_cells`] uses: a block edit diffs these before and after the
/// write instead of decomposing the whole chunk again.
pub fn collider_cells_of_block(chunk: &Chunk, i: BlockIndex) -> ColliderCells {
    let mut out = ColliderCells { blocks: Vec::new(), halves: Vec::new(), voxels: Vec::new() };
    push_block_cells(&mut out, i, chunk.blocks.get(i), chunk.chisel.get(&i));
    out
}

/// Appends block `bi`'s cells to `out`: one block key for a solid block,
/// half keys for a partial chisel's solid octants and voxel keys for its
/// other solid voxels, or nothing for air.
fn push_block_cells(out: &mut ColliderCells, bi: BlockIndex, block: Block, chisel: Option<&ChiselData>) {
    let h = HALF_BLOCK_VOXELS;
    let (bx, by, bz) = bi.xyz();
    let Some(c) = chisel else {
        if block.id != BlockId::AIR {
            out.blocks.push(IVector::new(bx as i32, by as i32, bz as i32));
        }
        return;
    };
    let solid: Vec<bool> = c.materials().into_iter().map(|m| m != BlockId::AIR).collect();
    if solid.iter().all(|&s| s) {
        out.blocks.push(IVector::new(bx as i32, by as i32, bz as i32));
        return;
    }
    for (ox, oy, oz) in (0..8).map(|o| (o % 2, (o / 2) % 2, o / 4)) {
        let (x0, y0, z0) = (ox * h, oy * h, oz * h);
        let octant = |f: &mut dyn FnMut(usize, usize, usize)| {
            for vx in x0..x0 + h {
                for vy in y0..y0 + h {
                    for vz in z0..z0 + h {
                        f(vx, vy, vz);
                    }
                }
            }
        };
        let mut full = true;
        octant(&mut |x, y, z| full &= solid[local_index(x, y, z)]);
        if full {
            out.halves.push(IVector::new(
                (bx * 2 + ox) as i32,
                (by * 2 + oy) as i32,
                (bz * 2 + oz) as i32,
            ));
            continue;
        }
        octant(&mut |x, y, z| {
            if solid[local_index(x, y, z)] {
                out.voxels.push(IVector::new(
                    (bx * BLOCK_VOXELS + x) as i32,
                    (by * BLOCK_VOXELS + y) as i32,
                    (bz * BLOCK_VOXELS + z) as i32,
                ));
            }
        });
    }
}

/// Derive the dense 32³ voxels of render brick `brick` (each axis
/// `0..BRICKS_PER_CHUNK_EDGE`) straight from the chunk's blocks and chisel —
/// an oracle independent of [`crate::BrickView::voxels`], kept for tests and
/// tools. Output order is `BrickShape` (index with [`voxel_index`]). Never a
/// whole chunk: dense voxel arrays are per brick.
pub fn derive_voxels(chunk: &Chunk, brick: [usize; 3]) -> Vec<Voxel> {
    use crate::voxel::BRICK_BLOCKS;
    let chisel = &chunk.chisel;
    let origin = brick.map(|b| b * BRICK_BLOCKS);
    let mut voxels = vec![Voxel::new(BlockId::AIR); BRICK_VOXEL_COUNT];
    for lz in 0..BRICK_BLOCKS {
        for ly in 0..BRICK_BLOCKS {
            for lx in 0..BRICK_BLOCKS {
                let bi = BlockIndex::from_xyz(origin[0] + lx, origin[1] + ly, origin[2] + lz);
                let block = chunk.blocks.get(bi);
                let materials = chisel.get(&bi).map(ChiselData::materials);
                for vz in 0..BLOCK_VOXELS {
                    for vy in 0..BLOCK_VOXELS {
                        for vx in 0..BLOCK_VOXELS {
                            let id = match &materials {
                                Some(m) => m[local_index(vx, vy, vz)],
                                None => block.id,
                            };
                            let li = voxel_index(
                                lx * BLOCK_VOXELS + vx,
                                ly * BLOCK_VOXELS + vy,
                                lz * BLOCK_VOXELS + vz,
                            );
                            voxels[li] = Voxel::new(id);
                        }
                    }
                }
            }
        }
    }
    voxels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::BlockId;
    use std::collections::BTreeMap;

    #[test]
    fn chunk_block_dial_parses() {
        assert_eq!(parse_chunk_blocks(None), 2);
        assert_eq!(parse_chunk_blocks(Some("")), 2);
        assert_eq!(parse_chunk_blocks(Some("16")), 16);
        assert_eq!(parse_chunk_blocks(Some("32")), 32);
        assert_eq!(CHUNK_BLOCKS, parse_chunk_blocks(option_env!("VOXEL_CHUNK_BLOCKS")));
    }

    /// A chunk from a linear block list and its chisel map.
    fn chunk_of(blocks: Vec<Block>, chisel: BTreeMap<BlockIndex, ChiselData>) -> Chunk {
        Chunk { blocks: crate::voxel::ChunkBlocks::from_vec(blocks), chisel }
    }

    /// Write block `i` (linear) without touching its chisel entry.
    fn put(c: &mut Chunk, i: usize, b: Block) {
        c.blocks.set(BlockIndex::from_linear(i), b);
    }

    #[test]
    fn uniform_stone_block_fills_its_subcube() {
        let blocks = vec![Block::new(BlockId(1)); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&chunk_of(blocks, BTreeMap::new()), [0, 0, 0]);
        assert!(voxels.iter().all(|v| v.block == BlockId(1)));
    }

    #[test]
    fn all_air_blocks_derive_to_all_air() {
        let blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&chunk_of(blocks, BTreeMap::new()), [0, 0, 0]);
        assert!(voxels.iter().all(|v| v.block == BlockId::AIR));
    }

    #[test]
    fn chiseled_block_uses_sparse_occupancy() {
        // One Stone block at index 0; chisel only voxel (0,0,0) solid.
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        blocks[0] = Block::new(BlockId(1));
        let mut c = ChiselData::new(vec![BlockId(1)]);
        c.set(0, 0, 0, true, 0);
        let mut chisel = BTreeMap::new();
        chisel.insert(BlockIndex::from_linear(0), c);

        let voxels = derive_voxels(&chunk_of(blocks, chisel), [0, 0, 0]);
        assert_eq!(voxels[voxel_index(0, 0, 0)].block, BlockId(1));
        assert_eq!(voxels[voxel_index(1, 0, 0)].block, BlockId::AIR, "rest of the chiseled block is empty");
    }

    #[test]
    fn derive_is_deterministic() {
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        blocks[3] = Block::new(BlockId(1));
        let a = derive_voxels(&chunk_of(blocks.clone(), BTreeMap::new()), [0, 0, 0]);
        let b = derive_voxels(&chunk_of(blocks, BTreeMap::new()), [0, 0, 0]);
        assert_eq!(a, b);
    }

    #[test]
    fn block_index_roundtrips() {
        for i in 0..CHUNK_BLOCK_COUNT {
            let (x, y, z) = BlockIndex::from_linear(i).xyz();
            assert_eq!(BlockIndex::from_xyz(x, y, z), BlockIndex::from_linear(i));
        }
    }

    /// Brick-local occupancy of the solid voxels of derived brick `b` — the oracle.
    fn solid_mask(chunk: &Chunk, b: [usize; 3]) -> Vec<bool> {
        derive_voxels(chunk, b).iter().map(|v| v.block != BlockId::AIR).collect()
    }

    /// Brick-local occupancy of brick `b` covered by the multi-scale cells,
    /// each clipped to the brick: block cells expand to 16³ voxels, half
    /// cells to 8³, chisel cells are already voxel-grid. Asserts no voxel is
    /// covered twice. Per brick, so a 16³-block chunk never needs a dense
    /// whole-chunk grid.
    fn covered_mask(cells: &ColliderCells, b: [usize; 3]) -> Vec<bool> {
        let edge = crate::voxel::BRICK_VOXELS as i32;
        let origin = b.map(|v| v as i32 * edge);
        let mut mask = vec![false; BRICK_VOXEL_COUNT];
        let mut cover = |min: [i32; 3], size: i32, what: &str| {
            // The cell's box clipped to the brick, brick-local.
            let lo = [0, 1, 2].map(|a| (min[a] - origin[a]).max(0));
            let hi = [0, 1, 2].map(|a| (min[a] + size - origin[a]).min(edge));
            for vz in lo[2]..hi[2] {
                for vy in lo[1]..hi[1] {
                    for vx in lo[0]..hi[0] {
                        let i = voxel_index(vx as usize, vy as usize, vz as usize);
                        assert!(!mask[i], "{what} cell at {min:?} overlaps a covered cell");
                        mask[i] = true;
                    }
                }
            }
        };
        let (bv, h) = (BLOCK_VOXELS as i32, HALF_BLOCK_VOXELS as i32);
        for c in &cells.blocks {
            cover([c.x * bv, c.y * bv, c.z * bv], bv, "block-scale");
        }
        for c in &cells.halves {
            cover([c.x * h, c.y * h, c.z * h], h, "half-scale");
        }
        for v in &cells.voxels {
            cover([v.x, v.y, v.z], 1, "voxel-scale");
        }
        mask
    }

    #[test]
    fn collider_cells_uniform_chunk_is_eight_blocks() {
        let c = Chunk::uniform(Block::new(BlockId(1)));
        let cells = collider_cells(&c);
        assert_eq!(cells.blocks.len(), CHUNK_BLOCK_COUNT);
        assert!(cells.voxels.is_empty());
        assert!(cells.is_full_cube());
        assert!(!cells.is_empty());
    }

    #[test]
    fn collider_cells_all_air_is_empty() {
        let c = Chunk::default();
        let cells = collider_cells(&c);
        assert!(cells.is_empty());
        assert!(!cells.is_full_cube());
    }

    #[test]
    fn collider_cells_mixed_floor_splits_scales() {
        // flat_floor(24): the by=0 block layer is whole solid, the by=1
        // layer is chiseled 8 voxel-layers deep: a half block each, so its
        // four lower octants per block land on the half-block grid.
        let c = Chunk::flat_floor(24);
        let cells = collider_cells(&c);
        let layer = CHUNK_BLOCKS * CHUNK_BLOCKS;
        assert_eq!(cells.blocks.len(), layer);
        assert_eq!(cells.halves.len(), layer * 4);
        assert!(cells.voxels.is_empty());
        assert!(!cells.is_full_cube());
    }

    #[test]
    fn collider_cells_chisel_only_floor_has_no_block_cells() {
        // flat_floor(8) is shallower than one block: half cells only.
        let c = Chunk::flat_floor(8);
        let cells = collider_cells(&c);
        assert!(cells.blocks.is_empty());
        assert_eq!(cells.halves.len(), CHUNK_BLOCKS * CHUNK_BLOCKS * 4);
        assert!(cells.voxels.is_empty());
    }

    #[test]
    fn collider_cells_skip_air_material_chisel_voxels() {
        // Occupied chisel voxel whose palette resolves to AIR (empty palette
        // -> material_at falls back to AIR) must not produce a collider cell,
        // matching derive_voxels solidity.
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        blocks[0] = Block::new(BlockId(1));
        let mut c = ChiselData::new(vec![]);
        c.set(0, 0, 0, true, 0);
        let mut chisel = BTreeMap::new();
        chisel.insert(BlockIndex::from_linear(0), c);
        let cells = collider_cells(&chunk_of(blocks, chisel));
        assert!(cells.is_empty());
    }

    /// A half block, a full chisel, a carved half block and a chisel with one
    /// solid octant beside plain blocks: every scale in one chunk.
    /// Blocks named by their 2-block-chunk linear index: 0 = (0,0,0),
    /// 1 = (1,0,0), 2 = (0,1,0), 6 = (0,1,1), 7 = (1,1,1).
    fn mixed_scales_chunk() -> Chunk {
        let at = |x, y, z| BlockIndex::from_xyz(x, y, z);
        let mut c = Chunk::default();
        put(&mut c, at(0, 0, 0).linear(), Block::new(BlockId(1)));
        c.chisel.insert(at(0, 0, 0), ChiselData::cuboid(BlockId(1), [0, 0, 0], [16, 8, 16]));
        put(&mut c, at(1, 0, 0).linear(), Block::new(BlockId(1)));
        put(&mut c, at(0, 1, 0).linear(), Block::new(BlockId(2)));
        c.chisel.insert(at(0, 1, 0), ChiselData::cuboid(BlockId(2), [0, 0, 0], [16, 16, 16]));
        put(&mut c, at(0, 1, 1).linear(), Block::new(BlockId(2)));
        let mut carved = ChiselData::cuboid(BlockId(2), [0, 0, 0], [16, 8, 16]);
        carved.set(15, 7, 15, false, 0);
        c.chisel.insert(at(0, 1, 1), carved);
        put(&mut c, at(1, 1, 1).linear(), Block::new(BlockId(2)));
        c.chisel.insert(at(1, 1, 1), ChiselData::cuboid(BlockId(2), [8, 8, 8], [16, 16, 16]));
        c
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn collider_cells_cover_exactly_the_derived_solid_voxels() {
        let mut single_chisel = Chunk::default();
        put(&mut single_chisel, 0, Block::new(BlockId(1)));
        let mut cd = ChiselData::new(vec![BlockId(1)]);
        cd.set(3, 7, 11, true, 0);
        single_chisel.chisel.insert(BlockIndex::from_linear(0), cd);

        // Chisel entry wins over the (air) block id: blocks[0] stays AIR, but
        // the chisel data marks a few voxels occupied with a non-air palette
        // material, so those voxels must still be solid/covered.
        let mut chisel_on_air_block = Chunk::default();
        let mut cd2 = ChiselData::new(vec![BlockId(1)]);
        cd2.set(1, 2, 3, true, 0);
        cd2.set(4, 5, 6, true, 0);
        cd2.set(7, 8, 9, true, 0);
        chisel_on_air_block.chisel.insert(BlockIndex::from_linear(0), cd2);

        let cases = [
            mixed_scales_chunk(),
            Chunk::uniform(Block::new(BlockId(1))),
            Chunk::default(),
            Chunk::flat_floor(8),
            Chunk::flat_floor(16),
            Chunk::flat_floor(24),
            Chunk::flat_floor(32),
            single_chisel,
            chisel_on_air_block,
        ];
        for (i, chunk) in cases.iter().enumerate() {
            let cells = collider_cells(chunk);
            for (b, _) in chunk.bricks() {
                assert!(
                    covered_mask(&cells, b) == solid_mask(chunk, b),
                    "case {i}, brick {b:?}: multi-scale cells must cover exactly the solid voxels"
                );
            }
        }
    }

    #[test]
    fn collider_cells_take_the_coarsest_solid_cell_of_a_chisel() {
        let c = mixed_scales_chunk();
        let cells = collider_cells(&c);
        // Plain block 1 and the full chisel of block 2.
        assert_eq!(cells.blocks, vec![IVector::new(1, 0, 0), IVector::new(0, 1, 0)]);
        // Block 0's half block (4 octants), block 6's carved half (3 of its
        // 4 lower octants intact), block 7's one solid octant.
        assert_eq!(cells.halves.len(), 4 + 3 + 1);
        assert_eq!(cells.voxels.len(), 8 * 8 * 8 - 1, "only the carved octant stays voxel-scale");
        let grids: Vec<f32> = cells.grids().map(|(size, _)| size).collect();
        assert_eq!(grids, vec![BLOCK_VOXELS as f32, HALF_BLOCK_VOXELS as f32, 1.0]);
    }

    #[test]
    fn a_half_block_is_one_cuboid_and_fills_the_lower_half() {
        let half = ChiselData::cuboid(BlockId(3), [0, 0, 0], [16, 8, 16]);
        let cuboids: Vec<_> = half.cuboids().collect();
        assert_eq!(cuboids, vec![([0, 0, 0], [16, 8, 16], BlockId(3))]);
        let mut chisel = BTreeMap::new();
        chisel.insert(BlockIndex::from_xyz(1, 1, 0), half);
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        blocks[BlockIndex::from_xyz(1, 1, 0).linear()] = Block::new(BlockId(3));
        let voxels = derive_voxels(&chunk_of(blocks, chisel), [0, 0, 0]);
        for y in 16..32 {
            let want = if y < 24 { BlockId(3) } else { BlockId::AIR };
            assert_eq!(voxels[voxel_index(16, y, 0)].block, want, "y {y}");
            assert_eq!(voxels[voxel_index(31, y, 15)].block, want, "y {y}");
        }
        assert_eq!(voxels[voxel_index(15, 16, 0)].block, BlockId::AIR, "neighbour cell untouched");
    }

    /// The cuboid list is a function of the voxels alone: the same voxels
    /// reached by different edit orders serialize and hash identically.
    #[test]
    fn chisel_data_is_canonical() {
        let palette = vec![BlockId(1), BlockId(2)];
        let target = |x: usize, y: usize, z: usize| -> Option<u8> {
            if y < 8 {
                Some(((x / 5 + z / 3) % 2) as u8)
            } else {
                (x == z && y < 12).then_some(1)
            }
        };
        let direct = ChiselData::from_fn(palette.clone(), target);

        // Scramble a spread of voxels, then put them back one `set` at a
        // time, last first: a different history, the same voxels.
        let spots: Vec<usize> = (0..BLOCK_VOXEL_COUNT).step_by(211).collect();
        let mut grid = direct.grid();
        for &i in &spots {
            grid[i] = match grid[i] {
                Some(0) => Some(1),
                Some(_) => None,
                None => Some(0),
            };
        }
        let mut edited = ChiselData::from_grid(palette.clone(), &grid);
        assert_ne!(edited, direct);
        for &i in spots.iter().rev() {
            let (x, y, z) = (i % 16, (i / 16) % 16, i / 256);
            match target(x, y, z) {
                Some(m) => edited.set(x, y, z, true, m),
                None => edited.set(x, y, z, false, 0),
            }
        }
        assert_eq!(edited, direct);
        let crc = |c: &ChiselData| crc32fast::hash(&bincode::serialize(c).unwrap());
        assert_eq!(crc(&edited), crc(&direct));

        // And it round-trips through the grid unchanged.
        assert_eq!(ChiselData::from_grid(palette, &direct.grid()), direct);
        for i in 0..BLOCK_VOXEL_COUNT {
            let (x, y, z) = (i % 16, (i / 16) % 16, i / 256);
            assert_eq!(direct.grid()[i], target(x, y, z));
            assert_eq!(direct.is_solid(x, y, z), target(x, y, z).is_some());
        }
    }

    /// `cuboid`'s direct packing must equal the canonical merge of the same
    /// voxels, or a half block built each way would hash differently.
    #[test]
    fn a_packed_cuboid_is_its_own_canonical_form() {
        for (min, max) in [
            ([0, 0, 0], [16, 8, 16]),
            ([0, 0, 0], [16, 16, 16]),
            ([3, 5, 7], [4, 16, 9]),
            ([15, 15, 15], [16, 16, 16]),
            ([2, 2, 2], [2, 9, 9]),
        ] {
            let inside = |x: usize, y: usize, z: usize| {
                (min[0]..max[0]).contains(&x) && (min[1]..max[1]).contains(&y) && (min[2]..max[2]).contains(&z)
            };
            let merged = ChiselData::from_fn(vec![BlockId(4)], |x, y, z| inside(x, y, z).then_some(0));
            assert_eq!(ChiselData::cuboid(BlockId(4), min, max), merged, "{min:?}..{max:?}");
        }
    }

    #[test]
    fn cuboid_packing_round_trips_the_extremes() {
        for (min, max, m) in [([0, 0, 0], [16, 16, 16], 255u8), ([15, 0, 7], [16, 1, 8], 0)] {
            assert_eq!(unpack_cuboid(pack_cuboid(min, max, m)), (min, max, m));
        }
    }

    #[test]
    fn collider_cells_is_deterministic() {
        let c = mixed_scales_chunk();
        let a = collider_cells(&c);
        let b = collider_cells(&c);
        assert_eq!(a, b);
    }
}
