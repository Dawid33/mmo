//! Water as the sim reads it: units per cell, the DF depth levels over them,
//! and the rules pathing and swimming derive from them. Water is a liquid
//! block with no collider (`BlockKind::Liquid`); [`Rollback::water_units_at`]
//! is the single water read in the sim.

use crate::{block_props, Rollback, WorldBlockPos};

/// Water units in one full cell.
pub const WATER_FULL: u16 = 4096;
/// DF's depth levels: a cell shows 1..=`DF_DEPTH_LEVELS` sevenths of full.
pub const DF_DEPTH_LEVELS: u8 = 7;
/// Deepest water a body wades through, standing on the floor below.
pub const WADE_MAX_DEPTH: u8 = 3;
/// Shallowest water a body swims in.
pub const SWIM_MIN_DEPTH: u8 = 4;
/// Depth at which a head under water is drowning.
pub const DROWN_DEPTH: u8 = 7;
/// Extra path cost of stepping onto a tile with wading-depth water (1..=3).
pub const WADE_COST: u32 = 10;

/// The DF depth (0..=7) of `units` of water: 0 for none, else
/// `ceil(units * 7 / WATER_FULL)` clamped to 1..=7.
pub fn df_depth(units: u16) -> u8 {
    if units == 0 {
        return 0;
    }
    let levels = u32::from(DF_DEPTH_LEVELS);
    let full = u32::from(WATER_FULL);
    (u32::from(units) * levels).div_ceil(full).clamp(1, levels) as u8
}

impl Rollback {
    /// Water units in world block `p`, own region ∪ halo; 0 beyond them.
    /// Static water: a liquid block is [`WATER_FULL`].
    pub fn water_units_at(&self, p: WorldBlockPos) -> u16 {
        match self.halo_block(p) {
            Some(b) if block_props().is_liquid(b.id) => WATER_FULL,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_depth_maps_units_to_sevenths() {
        assert_eq!(df_depth(0), 0);
        assert_eq!(df_depth(1), 1);
        assert_eq!(df_depth(WATER_FULL / 7), 1);
        assert_eq!(df_depth(WATER_FULL * 3 / 7), 3);
        assert_eq!(df_depth(WATER_FULL * 3 / 7 + 1), 4);
        assert_eq!(df_depth(WATER_FULL), 7);
    }
}
