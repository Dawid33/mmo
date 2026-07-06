//! The block layer: a `Block` (16³ voxels = 1 m³) is the authoritative gameplay
//! unit; voxels are a subdivision derived on demand via [`derive_voxels`].
//! Sub-block detail ("chiseling") is sparse — only chiseled blocks carry a
//! dense 16³ [`ChiselData`]. This module is Bevy-free and deterministic.

use std::collections::BTreeMap;

use block_mesh::ndshape::ConstShape;

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
}
