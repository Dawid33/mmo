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

    /// Collapsed form of a uniform block: `Air` for the air block, else a
    /// `Dense` chunk filled with `block`.
    pub fn uniform(block: Block) -> Self {
        if block == Block::new(BlockId::AIR) {
            SubChunk::Air
        } else {
            SubChunk::Dense(Box::new(Chunk::uniform(block)))
        }
    }

    /// True iff this sub-chunk reads uniformly as `block`.
    pub fn is_uniform_block(&self, block: Block) -> bool {
        match self {
            SubChunk::Air => block == Block::new(BlockId::AIR),
            SubChunk::Dense(c) => c.is_uniform(block),
        }
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
            *self = SubChunk::Dense(Box::default());
        }
        let SubChunk::Dense(c) = self else { unreachable!() };
        let prev = c.block_at(pos.x as usize, pos.y as usize, pos.z as usize);
        c.set_block(pos.x as usize, pos.y as usize, pos.z as usize, block);
        prev
    }
}

// A column footprint is `CHUNK_BLOCKS` blocks per horizontal axis; `BlockPos`
// x/z components are bounded 0..CHUNK_BLOCKS. The volume math is parameterized
// over CHUNK_BLOCKS (see `SUB_BLOCKS_Y`), so no fixed block count is assumed.

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
    /// Recipe for the ground below the band (lazy underground), if any.
    #[serde(default)]
    ground: Option<crate::ground::ColumnGround>,
}

impl Column {
    const SUB_BLOCKS_Y: i32 = CHUNK_BLOCKS as i32;

    pub fn new(base_sub_y: i32, below: Block, above: Block, sub_chunks: Vec<SubChunk>) -> Self {
        let mut c = Self { base_sub_y, below, above, sub_chunks, ground: None };
        c.normalize();
        c
    }

    pub fn with_ground(mut self, ground: crate::ground::ColumnGround) -> Self {
        self.ground = Some(ground);
        self
    }

    /// A column built with its ground recipe attached BEFORE normalising, so
    /// the band keeps its bottom: worldgen materialises a cliff's face down to
    /// where the neighbouring terrain opens it, and those slices are uniform
    /// stone that `new` would trim into implied ground behind open air.
    /// `base_sub_y` stays `ground.band_base_sub_y`.
    pub fn grounded(
        base_sub_y: i32,
        below: Block,
        above: Block,
        sub_chunks: Vec<SubChunk>,
        ground: crate::ground::ColumnGround,
    ) -> Self {
        debug_assert_eq!(base_sub_y, ground.band_base_sub_y);
        let mut c = Self { base_sub_y, below, above, sub_chunks, ground: Some(ground) };
        c.normalize();
        c
    }

    pub fn ground(&self) -> Option<&crate::ground::ColumnGround> {
        self.ground.as_ref()
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

    /// The sentinel block that applies at absolute `sub_y` (outside the band).
    fn sentinel_at(&self, sub_y: i32) -> Block {
        if sub_y < self.base_sub_y { self.below } else { self.above }
    }

    /// Canonicalize: collapse all-air `Dense` to `Air`, trim edge slices that
    /// read uniformly as the adjacent sentinel (advancing `base_sub_y` on the
    /// low edge — not with a ground recipe, whose band bottom is deliberate,
    /// see `grounded`), and reset `base_sub_y` to 0 when the band is empty.
    /// Idempotent.
    pub fn normalize(&mut self) {
        for s in &mut self.sub_chunks {
            if let SubChunk::Dense(c) = s {
                if c.is_all_air() {
                    *s = SubChunk::Air;
                }
            }
        }
        while matches!(self.sub_chunks.last(), Some(s) if s.is_uniform_block(self.above)) {
            self.sub_chunks.pop();
        }
        while self.ground.is_none() && matches!(self.sub_chunks.first(), Some(s) if s.is_uniform_block(self.below)) {
            self.sub_chunks.remove(0);
            self.base_sub_y += 1;
        }
        // Only reset the boundary when the two sentinels agree: `base_sub_y`
        // still carries meaningful information (which sentinel applies where)
        // whenever `below != above`, even with an empty band — zeroing it
        // unconditionally would silently reclassify already-correct `below`
        // reads as `above` (or vice versa). See
        // `column_tests::reads_sentinels_outside_band`.
        if self.sub_chunks.is_empty() && self.below == self.above {
            self.base_sub_y = 0;
        }
    }

    /// Materialized (`Dense`) sub-chunks as `(absolute sub_y, chunk)` pairs;
    /// `Air` slices are skipped. Used to seed a region's per-chunk physics path.
    pub fn materialized_sub_chunks(&self) -> Vec<(i32, Chunk)> {
        self.sub_chunks
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                SubChunk::Dense(c) => Some((self.base_sub_y + i as i32, (**c).clone())),
                SubChunk::Air => None,
            })
            .collect()
    }
}

impl ReadVol for Column {
    fn get(&self, pos: BlockPos) -> Block {
        let sub_y = pos.y.div_euclid(Self::SUB_BLOCKS_Y);
        let local_y = pos.y.rem_euclid(Self::SUB_BLOCKS_Y);
        match self.sub_chunk_at(sub_y) {
            Some(s) => s.get(BlockPos::new(pos.x, local_y, pos.z)),
            None => self.sentinel_at(sub_y),
        }
    }
}

impl WriteVol for Column {
    fn set(&mut self, pos: BlockPos, block: Block) -> Block {
        let prev = self.get(pos);
        let sub_y = pos.y.div_euclid(Self::SUB_BLOCKS_Y);
        let local_y = pos.y.rem_euclid(Self::SUB_BLOCKS_Y);
        // Grow via the below/above loops even from a fully empty band: both
        // loops key off the current (always-meaningful) `base_sub_y`, so they
        // fill exactly the gap between the old boundary and `sub_y` with the
        // correct sentinel — no separate empty-band special case is needed
        // (a prior version jumped `base_sub_y` straight to `sub_y`, which
        // opened a hole of un-materialized cells that silently flipped from
        // `below` to `above` reads; see
        // `column_tests::downward_growth_into_stone_preserves_cells_and_prev`).
        while sub_y < self.base_sub_y {
            self.sub_chunks.insert(0, SubChunk::uniform(self.below));
            self.base_sub_y -= 1;
        }
        let top = self.base_sub_y + self.sub_chunks.len() as i32;
        for _ in top..=sub_y {
            self.sub_chunks.push(SubChunk::uniform(self.above));
        }
        let idx = (sub_y - self.base_sub_y) as usize;
        self.sub_chunks[idx].set(BlockPos::new(pos.x, local_y, pos.z), block);
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
        // Above the band (block Y 20 → sub_y 10): air sentinel.
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
        // Full-canonical support (see `Column::normalize`): the trailing Air
        // slices trim against the `above` sentinel as before, and — since
        // `below` is also stone — the remaining `dense_stone` slice is now
        // *also* redundant with `below` and trims away too, leaving the band
        // fully empty. Reads are unaffected: `base_sub_y` absorbs the trim.
        let col = Column::new(0, stone(), air(), vec![dense_stone(), SubChunk::Air, SubChunk::Air]);
        assert!(col.sub_chunks.is_empty());
        assert_eq!(col.get(BlockPos::new(0, 0, 0)), stone());
        assert_eq!(col.get(BlockPos::new(0, 20, 0)), air());
    }

    #[test]
    fn grounded_columns_keep_their_stone_bottom() {
        let ground = crate::ground::ColumnGround::plain([4; 4], -1, BlockId(2), BlockId(6));
        let col = Column::grounded(-1, stone(), air(), vec![dense_stone(), dense_stone(), SubChunk::Air], ground);
        assert_eq!(col.base_sub_y(), -1, "uniform stone at the bottom stays materialised");
        assert_eq!(col.sub_chunks.len(), 2, "the air top still trims");
        assert_eq!(col.ground().unwrap().band_base_sub_y, col.base_sub_y());
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
        // Full-canonical support (see `Column::normalize`): `dense_stone`
        // equals the `below` sentinel, so `Column::new` already collapses it
        // away — the band starts (and, after this round-trip write, ends)
        // fully empty. Reads stay correct via `base_sub_y` throughout.
        let mut col = Column::new(0, stone(), air(), vec![dense_stone()]);
        // Write a stone block well above the band.
        let far = BlockPos::new(0, 40, 0);
        assert_eq!(col.get(far), air()); // sentinel before
        col.set(far, stone());
        assert_eq!(col.get(far), stone()); // present after
        // Writing air back at the same spot returns to canonical (collapse/trim).
        col.set(far, air());
        assert_eq!(col.get(far), air());
        // Re-normalized: band is back to fully empty (canonical form).
        assert!(col.sub_chunks.is_empty());
    }

    #[test]
    fn downward_growth_into_stone_preserves_cells_and_prev() {
        let mut col = Column::new(0, stone(), air(), vec![dense_stone()]);
        assert_eq!(col.get(BlockPos::new(0, -3, 0)), stone()); // below sentinel
        let dirt = Block::new(BlockId(3));
        let prev = col.set(BlockPos::new(0, -9, 0), dirt);
        assert_eq!(prev, stone());                              // true previous = below sentinel
        assert_eq!(col.get(BlockPos::new(0, -9, 0)), dirt);     // write landed
        assert_eq!(col.get(BlockPos::new(0, -3, 0)), stone());  // untouched cell NOT corrupted to air
    }

    #[test]
    fn writing_sentinel_value_stays_canonical() {
        let a = Column::new(0, stone(), air(), vec![dense_stone()]);
        let mut b = Column::new(0, stone(), air(), vec![dense_stone()]);
        b.set(BlockPos::new(0, -9, 0), stone()); // stone below a stone sentinel = logical no-op
        assert_eq!(bincode::serialize(&a).unwrap(), bincode::serialize(&b).unwrap());
    }

    #[test]
    fn empty_bands_are_canonical_regardless_of_history() {
        let x = Column::new(0, air(), air(), vec![SubChunk::Air]);   // collapses to empty
        let mut y = Column::new(0, air(), air(), vec![SubChunk::Air]);
        y.set(BlockPos::new(0, 10, 0), stone()); // grow band up
        y.set(BlockPos::new(0, 10, 0), air());   // erase -> band collapses empty again
        assert_eq!(bincode::serialize(&x).unwrap(), bincode::serialize(&y).unwrap());
        assert_eq!(y.get(BlockPos::new(0, 10, 0)), air());
    }
}
