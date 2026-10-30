//! The ground under one worldgen column footprint, as a recipe: enough to
//! compute rock and bedrock below the surface band on demand, so deep
//! sub-chunks need not be stored. Integer-only and deterministic.

use crate::block::{Block, CHUNK_BLOCKS};
use crate::registry::BlockId;

/// Blocks of rock below a column's surface before bedrock takes over.
pub const UNDERGROUND_DEPTH: i32 = 60;

/// Block columns in one column footprint (`CHUNK_BLOCKS²`): the length of
/// [`ColumnGround::surface`] and [`ColumnGround::geo`].
pub const COLUMN_FOOTPRINT: usize = CHUNK_BLOCKS * CHUNK_BLOCKS;
const FOOTPRINT: usize = COLUMN_FOOTPRINT;

/// Deserialize a per-footprint array, refusing any length but `FOOTPRINT`.
fn footprint<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Box<[i16]>, D::Error> {
    let v: Vec<i16> = serde::Deserialize::deserialize(d)?;
    if v.len() != FOOTPRINT {
        return Err(serde::de::Error::invalid_length(v.len(), &"one entry per block column of the footprint"));
    }
    Ok(v.into_boxed_slice())
}

/// Ground recipe for one column footprint. Per-footprint arrays hold
/// `FOOTPRINT` entries indexed `lx * CHUNK_BLOCKS + lz`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct ColumnGround {
    /// Surface block Y per block column.
    #[serde(deserialize_with = "footprint")]
    pub surface: Box<[i16]>,
    /// Stratigraphic offset per block column: `s = y - geo`.
    #[serde(deserialize_with = "footprint")]
    pub geo: Box<[i16]>,
    /// Formations top-down by `top` (in `s`); the last entry extends downward.
    pub strata: Vec<(i16, BlockId)>,
    pub bedrock: BlockId,
    /// The column's lowest materialised sub-chunk Y at genesis.
    pub band_base_sub_y: i32,
}

impl Default for ColumnGround {
    /// A zero surface and offset over every block column, no strata.
    fn default() -> Self {
        Self {
            surface: vec![0; FOOTPRINT].into_boxed_slice(),
            geo: vec![0; FOOTPRINT].into_boxed_slice(),
            strata: Vec::new(),
            bedrock: BlockId::default(),
            band_base_sub_y: 0,
        }
    }
}

impl ColumnGround {
    /// One stone formation everywhere, bedrock `UNDERGROUND_DEPTH` below the surface.
    /// `surface` holds `COLUMN_FOOTPRINT` entries.
    pub fn plain(surface: Box<[i16]>, band_base_sub_y: i32, stone: BlockId, bedrock: BlockId) -> Self {
        assert_eq!(surface.len(), FOOTPRINT, "one surface per block column of the footprint");
        Self {
            surface,
            geo: vec![0; FOOTPRINT].into_boxed_slice(),
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

/// The block a column's ground recipe implies at `local` of chunk `coords`:
/// `None` for a column without ground or a sub-chunk inside the band. Pure
/// core of `GameData::implied_block` (which handles real chunks).
pub fn implied_block(
    ground: Option<&ColumnGround>,
    coords: crate::ChunkCoords,
    local: (usize, usize, usize),
) -> Option<Block> {
    let ground = ground?;
    if !ground.is_implied(coords.y) {
        return None;
    }
    Some(ground.block_at(local.0, local.2, coords.y * CHUNK_BLOCKS as i32 + local.1 as i32))
}
