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

/// An unbounded vertical column of sub-chunks over one region-local (x, z)
/// cell. Sparse: only `sub_chunks` is materialized; below the band reads as
/// `below`, above as `above`. Always stored normalized (canonical).
///
/// No `PartialEq`/`Eq` (would demand `Chunk: PartialEq`); compare via bincode
/// bytes when a test needs equality.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Column {
    base_sub_y: i32,
    below: Block,
    above: Block,
    sub_chunks: Vec<SubChunk>,
}

impl Column {
    const SUB_BLOCKS_Y: i32 = CHUNK_BLOCKS as i32;

    pub fn new(base_sub_y: i32, below: Block, above: Block, sub_chunks: Vec<SubChunk>) -> Self {
        let mut c = Self { base_sub_y, below, above, sub_chunks };
        c.normalize();
        c
    }

    pub fn base_sub_y(&self) -> i32 {
        self.base_sub_y
    }

    pub fn sub_chunks(&self) -> &[SubChunk] {
        &self.sub_chunks
    }

    /// The stored sub-chunk at absolute sub-Y, or None if outside the band.
    pub fn sub_chunk_at(&self, sub_y: i32) -> Option<&SubChunk> {
        let idx = sub_y.checked_sub(self.base_sub_y)?;
        usize::try_from(idx).ok().and_then(|i| self.sub_chunks.get(i))
    }

    /// Canonicalize: collapse all-air `Dense` to `Air`, then trim trailing
    /// `Air` (equal to the `above` sentinel). Leading trim only when the
    /// `below` sentinel is itself air (never, for terrain). Idempotent.
    pub fn normalize(&mut self) {
        let above_is_air = self.above.id == BlockId::AIR;
        for s in &mut self.sub_chunks {
            if let SubChunk::Dense(c) = s {
                if c.is_all_air() {
                    *s = SubChunk::Air;
                }
            }
        }
        if above_is_air {
            while matches!(self.sub_chunks.last(), Some(SubChunk::Air)) {
                self.sub_chunks.pop();
            }
        }
        if self.below.id == BlockId::AIR {
            while matches!(self.sub_chunks.first(), Some(SubChunk::Air)) {
                self.sub_chunks.remove(0);
                self.base_sub_y += 1;
            }
        }
    }
}

impl ReadVol for Column {
    fn get(&self, pos: BlockPos) -> Block {
        let sub_y = pos.y.div_euclid(Self::SUB_BLOCKS_Y);
        let local_y = pos.y.rem_euclid(Self::SUB_BLOCKS_Y);
        match self.sub_chunk_at(sub_y) {
            Some(s) => s.get(BlockPos::new(pos.x, local_y, pos.z)),
            None if sub_y < self.base_sub_y => self.below,
            None => self.above,
        }
    }
}

impl WriteVol for Column {
    fn set(&mut self, pos: BlockPos, block: Block) -> Block {
        let sub_y = pos.y.div_euclid(Self::SUB_BLOCKS_Y);
        let local_y = pos.y.rem_euclid(Self::SUB_BLOCKS_Y);
        // Grow the band to include sub_y.
        if self.sub_chunks.is_empty() {
            self.base_sub_y = sub_y;
            self.sub_chunks.push(SubChunk::Air);
        }
        while sub_y < self.base_sub_y {
            self.sub_chunks.insert(0, SubChunk::Air);
            self.base_sub_y -= 1;
        }
        let top = self.base_sub_y + self.sub_chunks.len() as i32;
        for _ in top..=sub_y {
            self.sub_chunks.push(SubChunk::Air);
        }
        let idx = (sub_y - self.base_sub_y) as usize;
        let prev = self.sub_chunks[idx].set(BlockPos::new(pos.x, local_y, pos.z), block);
        self.normalize();
        prev
    }
}

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

#[cfg(test)]
mod column_tests {
    use super::*;
    use crate::{Block, BlockId, Chunk};

    fn stone() -> Block { Block::new(BlockId(2)) }
    fn air() -> Block { Block::new(BlockId::AIR) }

    fn dense_stone() -> SubChunk {
        let mut c = Chunk::default();
        for bx in 0..CHUNK_BLOCKS { for by in 0..CHUNK_BLOCKS { for bz in 0..CHUNK_BLOCKS {
            c.set_block(bx, by, bz, stone());
        }}}
        SubChunk::Dense(Box::new(c))
    }

    #[test]
    fn reads_sentinels_outside_band() {
        // One dense stone sub-chunk at sub_y = 0; below = stone, above = air.
        let col = Column::new(0, stone(), air(), vec![dense_stone()]);
        // Below the band (sub_y = -1): stone sentinel.
        assert_eq!(col.get(BlockPos::new(0, -1, 0)), stone());
        // Inside the band: stored stone.
        assert_eq!(col.get(BlockPos::new(0, 0, 0)), stone());
        // Above the band (sub_y = 5): air sentinel.
        assert_eq!(col.get(BlockPos::new(0, 20, 0)), air());
    }

    #[test]
    fn dense_all_air_collapses_on_normalize() {
        let air_dense = SubChunk::Dense(Box::new(Chunk::default()));
        let col = Column::new(0, stone(), air(), vec![air_dense]);
        // After normalize the sole air-dense sub-chunk collapses + trailing-trims.
        assert!(col.sub_chunks.is_empty());
    }

    #[test]
    fn trailing_air_is_trimmed() {
        let col = Column::new(0, stone(), air(), vec![dense_stone(), SubChunk::Air, SubChunk::Air]);
        assert_eq!(col.sub_chunks.len(), 1);
        assert!(matches!(col.sub_chunks[0], SubChunk::Dense(_)));
    }

    #[test]
    fn normalize_is_idempotent() {
        let mut col = Column::new(0, stone(), air(), vec![dense_stone(), SubChunk::Air]);
        let before = bincode::serialize(&col).unwrap();
        col.normalize();
        let after = bincode::serialize(&col).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn serialize_roundtrip_is_bit_exact() {
        let col = Column::new(0, stone(), air(), vec![dense_stone()]);
        let bytes = bincode::serialize(&col).unwrap();
        let back: Column = bincode::deserialize(&bytes).unwrap();
        assert_eq!(bincode::serialize(&back).unwrap(), bytes);
    }

    #[test]
    fn write_above_band_grows_then_normalizes() {
        let mut col = Column::new(0, stone(), air(), vec![dense_stone()]);
        // Write a stone block well above the band.
        let far = BlockPos::new(0, 40, 0);
        assert_eq!(col.get(far), air()); // sentinel before
        col.set(far, stone());
        assert_eq!(col.get(far), stone()); // present after
        // Writing air back at the same spot returns to canonical (collapse/trim).
        col.set(far, air());
        assert_eq!(col.get(far), air());
        // Re-normalized: band is back to the single dense sub-chunk.
        assert_eq!(col.sub_chunks.len(), 1);
    }
}
