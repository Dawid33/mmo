//! Sparse vertical block storage. A `Column` is an unbounded stack of 32³
//! `SubChunk`s over one region-local (x, z) cell, with constant sentinels
//! below/above the materialized band. Bevy-free and deterministic.

use crate::block::CHUNK_BLOCKS;
use crate::registry::BlockId;
use crate::voxel::Chunk;
use crate::Block;

/// Whole-block position. `x`/`z` are block coords within a footprint
/// (0..CHUNK_BLOCKS); `y` is a block Y whose frame depends on the impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

/// Read blocks from a volume at whole-block granularity.
pub trait ReadVol {
    fn get(&self, pos: BlockPos) -> Block;
}

/// Write a whole block; returns the previous block.
pub trait WriteVol {
    fn set(&mut self, pos: BlockPos, block: Block) -> Block;
}

/// One 32³ slice of a column. Only the all-air case is collapsed; every
/// other uniform case (including all-stone) stays `Dense` (gets no collider,
/// see plan §Task 5). This keeps a single canonical form per content.
///
/// Derive only `Serialize`/`Deserialize` (+ Debug/Clone). `Chunk` derives
/// `Hash` but NOT `PartialEq`/`Eq`, so `SubChunk` must not derive equality —
/// canonical comparison is done over bincode bytes, never `==`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SubChunk {
    Air,
    Dense(Box<Chunk>),
}

impl SubChunk {
    pub fn is_air(&self) -> bool {
        matches!(self, SubChunk::Air)
    }
}

impl ReadVol for SubChunk {
    fn get(&self, pos: BlockPos) -> Block {
        match self {
            SubChunk::Air => Block::new(BlockId::AIR),
            SubChunk::Dense(c) => c.block_at(pos.x as usize, pos.y as usize, pos.z as usize),
        }
    }
}

impl WriteVol for SubChunk {
    fn set(&mut self, pos: BlockPos, block: Block) -> Block {
        // Materialize on first write into an Air slice.
        if let SubChunk::Air = self {
            *self = SubChunk::Dense(Box::new(Chunk::default()));
        }
        let SubChunk::Dense(c) = self else { unreachable!() };
        let prev = c.block_at(pos.x as usize, pos.y as usize, pos.z as usize);
        c.set_block(pos.x as usize, pos.y as usize, pos.z as usize, block);
        prev
    }
}

// Referenced so CHUNK_BLOCKS stays a compile-time anchor for callers that
// bound BlockPos; also documents the footprint size.
const _: () = assert!(CHUNK_BLOCKS == 2);

#[cfg(test)]
mod subchunk_tests {
    use super::*;
    use crate::{Block, BlockId, Chunk};

    fn stone() -> Block { Block::new(BlockId(2)) }

    #[test]
    fn air_subchunk_reads_air_everywhere() {
        let s = SubChunk::Air;
        assert_eq!(s.get(BlockPos::new(0, 0, 0)).id, BlockId::AIR);
        assert_eq!(s.get(BlockPos::new(1, 1, 1)).id, BlockId::AIR);
        assert!(s.is_air());
    }

    #[test]
    fn dense_subchunk_reads_written_block() {
        let mut s = SubChunk::Dense(Box::new(Chunk::default()));
        let prev = s.set(BlockPos::new(1, 0, 1), stone());
        assert_eq!(prev.id, BlockId::AIR);
        assert_eq!(s.get(BlockPos::new(1, 0, 1)), stone());
        assert!(!s.is_air());
    }

    #[test]
    fn chunk_all_air_detection() {
        assert!(Chunk::default().is_all_air());
        let mut c = Chunk::default();
        c.set_block(0, 0, 0, stone());
        assert!(!c.is_all_air());
    }
}
