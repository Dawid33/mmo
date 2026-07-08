//! The block layer: a `Block` (16³ voxels = 1 m³) is the authoritative gameplay
//! unit; voxels are a subdivision derived on demand via [`derive_voxels`].
//! Sub-block detail ("chiseling") is sparse — only chiseled blocks carry a
//! dense 16³ [`ChiselData`]. This module is Bevy-free and deterministic.

use std::collections::BTreeMap;

use block_mesh::ndshape::ConstShape;
use parry3d::math::Point;

use crate::registry::BlockId;
use crate::voxel::{ChunkShape, Voxel, CHUNK_VOXEL_COUNT};

pub const BLOCK_VOXELS: usize = 16; // 1 block = 16³ voxels = 1 m³
pub const CHUNK_BLOCKS: usize = 32 / BLOCK_VOXELS; // = 2 blocks per axis
pub const CHUNK_BLOCK_COUNT: usize = CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS; // = 8
pub const BLOCK_VOXEL_COUNT: usize = BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS; // = 4096

/// Linear voxel index within a chunk, matching `ChunkShape` (x fastest).
/// Reuses `voxel::ChunkShape` so the 32³ layout has a single source of truth.
pub fn voxel_index(x: usize, y: usize, z: usize) -> usize {
    ChunkShape::linearize([x as u32, y as u32, z as u32]) as usize
}

/// Index of a block within a chunk, linear over `CHUNK_BLOCKS³` (0..8).
/// `Ord` so it can key a `BTreeMap` deterministically.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct BlockIndex(pub u8);

impl BlockIndex {
    pub fn from_xyz(bx: usize, by: usize, bz: usize) -> Self {
        BlockIndex((bx + by * CHUNK_BLOCKS + bz * CHUNK_BLOCKS * CHUNK_BLOCKS) as u8)
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
    Copy, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug,
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

/// Dense 16³ sub-block detail for a chiseled block. Dense (not cuboid-packed)
/// because dense is naturally invertible and trivially hashable — the property
/// a merged-cuboid list lacks. `Vec`-backed to avoid serde big-array friction.
#[derive(Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug)]
pub struct ChiselData {
    occupancy: Vec<u64>,       // 64 words = 4096-bit bitset (1 = solid)
    material: Vec<u8>,         // len 4096, palette index per voxel
    palette: Vec<BlockId>,     // small per-block material list (≤256)
}

impl ChiselData {
    pub fn new(palette: Vec<BlockId>) -> Self {
        Self {
            occupancy: vec![0u64; BLOCK_VOXEL_COUNT / 64],
            material: vec![0u8; BLOCK_VOXEL_COUNT],
            palette,
        }
    }

    fn local_index(vx: usize, vy: usize, vz: usize) -> usize {
        vx + vy * BLOCK_VOXELS + vz * BLOCK_VOXELS * BLOCK_VOXELS
    }

    pub fn set(&mut self, vx: usize, vy: usize, vz: usize, solid: bool, material_idx: u8) {
        let i = Self::local_index(vx, vy, vz);
        let (w, b) = (i / 64, i % 64);
        if solid {
            self.occupancy[w] |= 1u64 << b;
        } else {
            self.occupancy[w] &= !(1u64 << b);
        }
        self.material[i] = material_idx;
    }

    pub fn is_solid(&self, vx: usize, vy: usize, vz: usize) -> bool {
        let i = Self::local_index(vx, vy, vz);
        (self.occupancy[i / 64] >> (i % 64)) & 1 == 1
    }

    pub fn material_at(&self, vx: usize, vy: usize, vz: usize) -> BlockId {
        let i = Self::local_index(vx, vy, vz);
        self.palette
            .get(self.material[i] as usize)
            .copied()
            .unwrap_or(BlockId::AIR)
    }
}

/// Chunk collider cells split by grid scale: whole (unchiseled) solid blocks
/// on the block grid, chisel detail on the voxel grid. Together they cover
/// exactly the solid cells of [`derive_voxels`] — this is that function's
/// collider-facing counterpart, kept at the coarsest exact scale instead of
/// expanded to the 32³ grid.
#[derive(Debug, Clone, PartialEq)]
pub struct ColliderCells {
    /// Block-grid keys (0..CHUNK_BLOCKS per axis) of solid, unchiseled blocks.
    pub blocks: Vec<Point<i32>>,
    /// Voxel-grid keys (0..CHUNK_BLOCKS*BLOCK_VOXELS per axis) of solid
    /// chisel voxels (occupied with a non-air material).
    pub voxels: Vec<Point<i32>>,
}

impl ColliderCells {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.voxels.is_empty()
    }

    /// True iff the chunk is entirely whole solid blocks (no chisel), i.e.
    /// its collision volume is exactly the full 32³ chunk cube. (All eight
    /// blocks present implies no chiseled block, hence no voxel cells.)
    pub fn is_full_cube(&self) -> bool {
        self.blocks.len() == CHUNK_BLOCK_COUNT
    }
}

/// Decompose a chunk's block layer into collider cells. Solidity matches
/// [`derive_voxels`]: an unchiseled block is solid iff its id is non-air; a
/// chisel voxel is solid iff occupied and its palette material is non-air.
/// Deterministic: blocks in `BlockIndex` order, chisel voxels in fixed
/// x-outer/z-inner nested order per block.
pub fn collider_cells(
    blocks: &[Block],
    chisel: &BTreeMap<BlockIndex, ChiselData>,
) -> ColliderCells {
    debug_assert_eq!(blocks.len(), CHUNK_BLOCK_COUNT);
    let mut out = ColliderCells { blocks: Vec::new(), voxels: Vec::new() };
    for (idx, block) in blocks.iter().enumerate() {
        let bi = BlockIndex(idx as u8);
        let (bx, by, bz) = bi.xyz();
        match chisel.get(&bi) {
            None => {
                if block.id != BlockId::AIR {
                    out.blocks.push(Point::new(bx as i32, by as i32, bz as i32));
                }
            }
            Some(c) => {
                for vx in 0..BLOCK_VOXELS {
                    for vy in 0..BLOCK_VOXELS {
                        for vz in 0..BLOCK_VOXELS {
                            if c.is_solid(vx, vy, vz)
                                && c.material_at(vx, vy, vz) != BlockId::AIR
                            {
                                out.voxels.push(Point::new(
                                    (bx * BLOCK_VOXELS + vx) as i32,
                                    (by * BLOCK_VOXELS + vy) as i32,
                                    (bz * BLOCK_VOXELS + vz) as i32,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Derive the 32³ voxel array from the authoritative block layer. Output layout
/// is byte-identical to the old `Chunk.voxels`, so the mesher and collider
/// consume it unchanged. Deterministic; the single source of truth for both sim
/// (collider) and client (mesh).
pub fn derive_voxels(blocks: &[Block], chisel: &BTreeMap<BlockIndex, ChiselData>) -> Vec<Voxel> {
    // Contract: `blocks` is indexed by `BlockIndex` (ordinal position), so it
    // must hold exactly the chunk's 8 blocks. A wrong length would silently
    // under/over-fill the voxel array (or panic on an out-of-range block coord).
    debug_assert_eq!(blocks.len(), CHUNK_BLOCK_COUNT);
    let mut voxels = vec![Voxel::new(BlockId::AIR); CHUNK_VOXEL_COUNT];
    for (idx, block) in blocks.iter().enumerate() {
        let bi = BlockIndex(idx as u8);
        let (bx, by, bz) = bi.xyz();
        let chiseled = chisel.get(&bi);
        for vx in 0..BLOCK_VOXELS {
            for vy in 0..BLOCK_VOXELS {
                for vz in 0..BLOCK_VOXELS {
                    let id = match chiseled {
                        Some(c) if c.is_solid(vx, vy, vz) => c.material_at(vx, vy, vz),
                        Some(_) => BlockId::AIR,
                        None => block.id,
                    };
                    let li = voxel_index(bx * BLOCK_VOXELS + vx, by * BLOCK_VOXELS + vy, bz * BLOCK_VOXELS + vz);
                    voxels[li] = Voxel::new(id);
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
    fn uniform_stone_block_fills_its_subcube() {
        let blocks = vec![Block::new(BlockId(1)); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&blocks, &BTreeMap::new());
        assert!(voxels.iter().all(|v| v.block == BlockId(1)));
    }

    #[test]
    fn all_air_blocks_derive_to_all_air() {
        let blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&blocks, &BTreeMap::new());
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
        chisel.insert(BlockIndex(0), c);

        let voxels = derive_voxels(&blocks, &chisel);
        assert_eq!(voxels[voxel_index(0, 0, 0)].block, BlockId(1));
        assert_eq!(voxels[voxel_index(1, 0, 0)].block, BlockId::AIR, "rest of the chiseled block is empty");
    }

    #[test]
    fn derive_is_deterministic() {
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        blocks[3] = Block::new(BlockId(1));
        let a = derive_voxels(&blocks, &BTreeMap::new());
        let b = derive_voxels(&blocks, &BTreeMap::new());
        assert_eq!(a, b);
    }

    #[test]
    fn block_index_roundtrips() {
        for i in 0..CHUNK_BLOCK_COUNT as u8 {
            let (x, y, z) = BlockIndex(i).xyz();
            assert_eq!(BlockIndex::from_xyz(x, y, z), BlockIndex(i));
        }
    }

    use crate::voxel::Chunk;
    use std::collections::BTreeSet;

    /// Voxel-grid coords of solid cells in a derived 32³ grid — the oracle.
    fn solid_set(voxels: &[Voxel]) -> BTreeSet<(i32, i32, i32)> {
        (0..CHUNK_VOXEL_COUNT)
            .filter(|i| voxels[*i].block != BlockId::AIR)
            .map(|i| {
                let [x, y, z] = ChunkShape::delinearize(i as u32);
                (x as i32, y as i32, z as i32)
            })
            .collect()
    }

    /// Voxel-grid coords covered by the two-scale cells: each block cell
    /// expands to its 16³ voxels, chisel cells are already voxel-grid.
    fn covered_set(cells: &ColliderCells) -> BTreeSet<(i32, i32, i32)> {
        let mut s = BTreeSet::new();
        for b in &cells.blocks {
            for vx in 0..BLOCK_VOXELS as i32 {
                for vy in 0..BLOCK_VOXELS as i32 {
                    for vz in 0..BLOCK_VOXELS as i32 {
                        s.insert((
                            b.x * BLOCK_VOXELS as i32 + vx,
                            b.y * BLOCK_VOXELS as i32 + vy,
                            b.z * BLOCK_VOXELS as i32 + vz,
                        ));
                    }
                }
            }
        }
        for v in &cells.voxels {
            let dup = !s.insert((v.x, v.y, v.z));
            assert!(!dup, "voxel-scale cell {v:?} overlaps a block-scale cell");
        }
        s
    }

    #[test]
    fn collider_cells_uniform_chunk_is_eight_blocks() {
        let c = Chunk::uniform(Block::new(BlockId(1)));
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert_eq!(cells.blocks.len(), CHUNK_BLOCK_COUNT);
        assert!(cells.voxels.is_empty());
        assert!(cells.is_full_cube());
        assert!(!cells.is_empty());
    }

    #[test]
    fn collider_cells_all_air_is_empty() {
        let c = Chunk::default();
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert!(cells.is_empty());
        assert!(!cells.is_full_cube());
    }

    #[test]
    fn collider_cells_mixed_floor_splits_scales() {
        // flat_floor(24): the by=0 block layer (4 blocks) is whole solid,
        // the by=1 layer is chiseled 8 voxel-layers deep (16*8*16 per block).
        let c = Chunk::flat_floor(24);
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert_eq!(cells.blocks.len(), 4);
        assert_eq!(cells.voxels.len(), 4 * 16 * 8 * 16);
        assert!(!cells.is_full_cube());
    }

    #[test]
    fn collider_cells_chisel_only_floor_has_no_block_cells() {
        // flat_floor(8) is shallower than one block: chisel cells only.
        let c = Chunk::flat_floor(8);
        let cells = collider_cells(&c.blocks, &c.chisel);
        assert!(cells.blocks.is_empty());
        assert_eq!(cells.voxels.len(), 4 * 16 * 8 * 16);
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
        chisel.insert(BlockIndex(0), c);
        let cells = collider_cells(&blocks, &chisel);
        assert!(cells.is_empty());
    }

    #[test]
    fn collider_cells_cover_exactly_the_derived_solid_voxels() {
        let mut single_chisel = Chunk::default();
        single_chisel.blocks[0] = Block::new(BlockId(1));
        let mut cd = ChiselData::new(vec![BlockId(1)]);
        cd.set(3, 7, 11, true, 0);
        single_chisel.chisel.insert(BlockIndex(0), cd);

        let cases = [
            Chunk::uniform(Block::new(BlockId(1))),
            Chunk::default(),
            Chunk::flat_floor(8),
            Chunk::flat_floor(16),
            Chunk::flat_floor(24),
            Chunk::flat_floor(32),
            single_chisel,
        ];
        for (i, chunk) in cases.iter().enumerate() {
            let cells = collider_cells(&chunk.blocks, &chunk.chisel);
            let derived = derive_voxels(&chunk.blocks, &chunk.chisel);
            assert_eq!(
                covered_set(&cells),
                solid_set(&derived),
                "case {i}: two-scale cells must cover exactly the solid voxels"
            );
        }
    }

    #[test]
    fn collider_cells_is_deterministic() {
        let c = Chunk::flat_floor(24);
        let a = collider_cells(&c.blocks, &c.chisel);
        let b = collider_cells(&c.blocks, &c.chisel);
        assert_eq!(a.blocks, b.blocks);
        assert_eq!(a.voxels, b.voxels);
    }
}
