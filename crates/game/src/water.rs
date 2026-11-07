//! Water as the sim reads it: units per cell, the DF depth levels over them,
//! and the rules pathing and swimming derive from them. Water is a liquid
//! block with no collider (`BlockKind::Liquid`); [`Rollback::water_units_at`]
//! is the single water read in the sim.

use rapier3d::math::Vector;

use crate::movement::{Immersion, PLAYER_EYE_HEIGHT, PLAYER_HEIGHT};
use crate::{block_props, BlockPos, EntityKey, Rollback, WorldBlockPos, BLOCK_VOXELS};

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

/// Height above the feet, in units, of the feet sample: just inside the
/// cell the feet stand in.
const FEET_SAMPLE_UNITS: f32 = 1.0;

/// How deep a body of `height_units` with its feet at `feet` (region-local
/// units) is in water: the deepest of its feet (+1 unit), waist (half the
/// height) and eye (`PLAYER_EYE_HEIGHT / PLAYER_HEIGHT` of the height)
/// whose cell is wet (`water_units_at > 0`). Reads own ∪ halo, so a body at
/// a seam feels its neighbour's water.
pub fn immersion_at(rb: &Rollback, feet: Vector, height_units: f32) -> Immersion {
    let here = *rb.data.region;
    let bv = BLOCK_VOXELS as f32;
    let (x, z) = ((feet.x / bv).floor() as i32, (feet.z / bv).floor() as i32);
    let wet = |y: f32| {
        let p = WorldBlockPos::from_local(here, BlockPos::new(x, (y / bv).floor() as i32, z));
        rb.water_units_at(p) > 0
    };
    if wet(feet.y + height_units * (PLAYER_EYE_HEIGHT / PLAYER_HEIGHT)) {
        Immersion::Head
    } else if wet(feet.y + height_units * 0.5) {
        Immersion::Waist
    } else if wet(feet.y + FEET_SAMPLE_UNITS) {
        Immersion::Feet
    } else {
        Immersion::Dry
    }
}

impl Rollback {
    /// Every mover's (entity with a `Motion`) immersion for this tick, from
    /// its body pose and the height of its movement collider (the body's
    /// first). Written only when it changed. Runs at the opening of the
    /// tick, after the intents, so every mover plans with it.
    pub(crate) fn tick_immersion_safe(&mut self) {
        let movers: Vec<EntityKey> =
            self.data.ecs.motion.iter().filter_map(|(k, m)| m.map(|_| k)).collect();
        for e in movers {
            let Some((feet, height)) = self.mover_extent(e) else { continue };
            let Some(mut motion) = *self.data.ecs.motion.try_get(e) else { continue };
            let immersion = immersion_at(self, feet, height);
            if motion.immersion != immersion {
                motion.immersion = immersion;
                self.data.ecs.motion.set_safe(e, Some(motion));
            }
        }
    }

    /// The feet (bottom of the movement collider, region-local units) and
    /// height of `e`'s body, upright.
    fn mover_extent(&self, e: EntityKey) -> Option<(Vector, f32)> {
        let handle = (*self.data.ecs.rigidbody.try_get(e))?;
        let body = self.data.physics.bodies.get(handle)?;
        let collider = self.data.physics.colliders.get(*body.colliders().first()?)?;
        let aabb = collider.shape().compute_local_aabb();
        let lift = collider.position_wrt_parent().map_or(0.0, |p| p.translation.y);
        let t = body.translation();
        Some((Vector::new(t.x, t.y + lift + aabb.mins.y, t.z), aabb.maxs.y - aabb.mins.y))
    }

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
