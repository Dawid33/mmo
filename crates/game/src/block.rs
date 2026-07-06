//! The block layer: a `Block` (16³ voxels = 1 m³) is the authoritative gameplay
//! unit; voxels are a subdivision derived on demand via [`derive_voxels`].
//! Sub-block detail ("chiseling") is sparse — only chiseled blocks carry a
//! dense 16³ [`ChiselData`]. This module is Bevy-free and deterministic.

use std::collections::BTreeMap;

use block_mesh::ndshape::{ConstShape, ConstShape3u32};

use crate::voxel::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};

pub const BLOCK_VOXELS: usize = 16; // 1 block = 16³ voxels = 1 m³
pub const CHUNK_BLOCKS: usize = 32 / BLOCK_VOXELS; // = 2 blocks per axis
pub const CHUNK_BLOCK_COUNT: usize = CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS; // = 8
pub const BLOCK_VOXEL_COUNT: usize = BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS; // = 4096

/// Linear voxel index within a chunk, matching `ChunkShape` (x fastest).
pub fn voxel_index(x: usize, y: usize, z: usize) -> usize {
    ConstShape3u32::<32, 32, 32>::linearize([x as u32, y as u32, z as u32]) as usize
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
    pub kind: BlockKind,
    pub data: [u8; 3],
}

impl Block {
    pub fn new(kind: BlockKind) -> Self {
        Self { kind, data: [0; 3] }
    }
}

/// Gameplay material atom at block granularity. Extensible.
#[derive(
    Copy, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug,
)]
pub enum BlockKind {
    #[default]
    Air,
    Stone,
}

impl BlockKind {
    /// The voxel material this block kind derives to.
    pub fn voxel(self) -> VoxelType {
        match self {
            BlockKind::Air => VoxelType::Air,
            BlockKind::Stone => VoxelType::Black,
        }
    }
}

/// Dense 16³ sub-block detail for a chiseled block. Dense (not cuboid-packed)
/// because dense is naturally invertible and trivially hashable — the property
/// a merged-cuboid list lacks. `Vec`-backed to avoid serde big-array friction.
#[derive(Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug)]
pub struct ChiselData {
    occupancy: Vec<u64>,       // 64 words = 4096-bit bitset (1 = solid)
    material: Vec<u8>,         // len 4096, palette index per voxel
    palette: Vec<BlockKind>,   // small per-block material list (≤256)
}

impl ChiselData {
    pub fn new(palette: Vec<BlockKind>) -> Self {
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

    pub fn material_at(&self, vx: usize, vy: usize, vz: usize) -> BlockKind {
        let i = Self::local_index(vx, vy, vz);
        self.palette
            .get(self.material[i] as usize)
            .copied()
            .unwrap_or_default()
    }
}

/// Derive the 32³ voxel array from the authoritative block layer. Output layout
/// is byte-identical to the old `Chunk.voxels`, so the mesher and collider
/// consume it unchanged. Deterministic; the single source of truth for both sim
/// (collider) and client (mesh).
pub fn derive_voxels(blocks: &[Block], chisel: &BTreeMap<BlockIndex, ChiselData>) -> Vec<Voxel> {
    let mut voxels = vec![Voxel::new(VoxelType::Air); CHUNK_VOXEL_COUNT];
    for (idx, block) in blocks.iter().enumerate() {
        let bi = BlockIndex(idx as u8);
        let (bx, by, bz) = bi.xyz();
        let chiseled = chisel.get(&bi);
        for vx in 0..BLOCK_VOXELS {
            for vy in 0..BLOCK_VOXELS {
                for vz in 0..BLOCK_VOXELS {
                    let kind = match chiseled {
                        Some(c) if c.is_solid(vx, vy, vz) => c.material_at(vx, vy, vz),
                        Some(_) => BlockKind::Air,
                        None => block.kind,
                    };
                    let li = voxel_index(bx * BLOCK_VOXELS + vx, by * BLOCK_VOXELS + vy, bz * BLOCK_VOXELS + vz);
                    voxels[li] = Voxel::new(kind.voxel());
                }
            }
        }
    }
    voxels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voxel::VoxelType;
    use std::collections::BTreeMap;

    #[test]
    fn uniform_stone_block_fills_its_subcube() {
        let blocks = vec![Block::new(BlockKind::Stone); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&blocks, &BTreeMap::new());
        assert!(voxels.iter().all(|v| v.kind == VoxelType::Black));
    }

    #[test]
    fn all_air_blocks_derive_to_all_air() {
        let blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        let voxels = derive_voxels(&blocks, &BTreeMap::new());
        assert!(voxels.iter().all(|v| v.kind == VoxelType::Air));
    }

    #[test]
    fn chiseled_block_uses_sparse_occupancy() {
        // One Stone block at index 0; chisel only voxel (0,0,0) solid.
        let mut blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        blocks[0] = Block::new(BlockKind::Stone);
        let mut c = ChiselData::new(vec![BlockKind::Stone]);
        c.set(0, 0, 0, true, 0);
        let mut chisel = BTreeMap::new();
        chisel.insert(BlockIndex(0), c);

        let voxels = derive_voxels(&blocks, &chisel);
        assert_eq!(voxels[voxel_index(0, 0, 0)].kind, VoxelType::Black);
        assert_eq!(voxels[voxel_index(1, 0, 0)].kind, VoxelType::Air, "rest of the chiseled block is empty");
    }

    #[test]
    fn derive_is_deterministic() {
        let mut blocks = vec![Block::new(BlockKind::Air); CHUNK_BLOCK_COUNT];
        blocks[3] = Block::new(BlockKind::Stone);
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
