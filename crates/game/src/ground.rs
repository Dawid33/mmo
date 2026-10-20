//! The ground under one worldgen column footprint, as a recipe: enough to
//! compute rock and bedrock below the surface band on demand, so deep
//! sub-chunks need not be stored. Integer-only and deterministic.

use crate::block::{Block, CHUNK_BLOCKS};
use crate::registry::BlockId;

/// Blocks of rock below a column's surface before bedrock takes over.
pub const UNDERGROUND_DEPTH: i32 = 60;

const FOOTPRINT: usize = CHUNK_BLOCKS * CHUNK_BLOCKS;

/// Ground recipe for one column footprint. Per-footprint arrays are indexed
/// `lx * CHUNK_BLOCKS + lz`.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, state_hash::StateHash,
)]
pub struct ColumnGround {
    /// Surface block Y per block column.
    pub surface: [i16; FOOTPRINT],
    /// Stratigraphic offset per block column: `s = y - geo`.
    pub geo: [i16; FOOTPRINT],
    /// Formations top-down by `top` (in `s`); the last entry extends downward.
    pub strata: Vec<(i16, BlockId)>,
    pub bedrock: BlockId,
    /// The column's lowest materialised sub-chunk Y at genesis.
    pub band_base_sub_y: i32,
}

impl ColumnGround {
    /// One stone formation everywhere, bedrock `UNDERGROUND_DEPTH` below the surface.
    pub fn plain(
        surface: [i16; FOOTPRINT],
        band_base_sub_y: i32,
        stone: BlockId,
        bedrock: BlockId,
    ) -> Self {
        Self {
            surface,
            geo: [0; FOOTPRINT],
            strata: vec![(0, stone)],
            bedrock,
            band_base_sub_y,
        }
    }

    /// The implied block at block coords `(lx, lz)` of the footprint and block Y `y`.
    /// Mirrors the forge's `Geology::rock_at_s`: the last stratum whose `top > s`,
    /// else the first (the top formation extends upward).
    pub fn block_at(&self, lx: usize, lz: usize, y: i32) -> Block {
        let i = lx * CHUNK_BLOCKS + lz;
        if i32::from(self.surface[i]) - y >= UNDERGROUND_DEPTH {
            return Block::new(self.bedrock);
        }
        let s = y - i32::from(self.geo[i]);
        let id = self
            .strata
            .iter()
            .rev()
            .find(|(top, _)| i32::from(*top) > s)
            .or_else(|| self.strata.first())
            .map_or(self.bedrock, |&(_, id)| id);
        Block::new(id)
    }

    /// Whether `sub_y` lies below the materialised band, i.e. is computed from this recipe.
    pub fn is_implied(&self, sub_y: i32) -> bool {
        sub_y < self.band_base_sub_y
    }
}
