//! Water as the sim reads it: units per tile, the DF depth levels over them,
//! and the rules pathing and swimming derive from them. Static water is the
//! stud ground recipe's (`StudColumnGround::water_top` above its `surface`);
//! chunks hold no water bricks, and digging beside or under it moves none of
//! it. [`Rollback::water_units_at`] is the single per-tile water read in the
//! sim; bodies compare themselves with the exact surface
//! ([`Rollback::stud_water_surface_units`]).

use rapier3d::math::Vector;

/// Region-local tile `[x, level, z]` (`orders::TilePos`).
type BlockPos3 = [i32; 3];

use crate::movement::{Immersion, PLAYER_EYE_HEIGHT, PLAYER_HEIGHT};
use crate::{ColumnCoords, EntityKey, Rollback, WorldBlockPos, WorldChunkPos};

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
/// height) and eye (`PLAYER_EYE_HEIGHT / PLAYER_HEIGHT` of the height) under
/// the water surface of the stud column under it — compared exactly (a tile
/// is 1.2 m: too coarse to tell feet from waist). Reads own ∪ halo, so a body
/// at a seam feels its neighbour's water.
pub fn immersion_at(rb: &Rollback, feet: Vector, height_units: f32) -> Immersion {
    let Some(surface) = rb.stud_water_surface_units(feet.x, feet.z) else { return Immersion::Dry };
    let wet = |y: f32| y < surface;
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

    /// Water units in world TILE `p`, own region ∪ stud halo; 0 beyond them.
    /// Graded: [`Self::stud_water_units`].
    pub fn water_units_at(&self, p: WorldBlockPos) -> u16 {
        self.stud_water_units(crate::item_piles::local_cell(*self.data.region, p))
    }

    /// One past the top water plate of region stud column `[x, z]` (plates; it may
    /// lie past the region's edge, in the stud halo), when the column's recipe holds
    /// water over its ground (`StudColumnGround::water_top` above its `surface`).
    /// Static water: it is the recipe's alone — digging beside or under it moves none
    /// of it.
    pub fn stud_water_top(&self, column: [i64; 2]) -> Option<i32> {
        self.stud_water_span(column).map(|(_, top)| top)
    }

    /// The plates of recipe water of region stud column `[x, z]`: from the recipe's
    /// ground top up to (not including) the water top. `None` for a dry column.
    fn stud_water_span(&self, column: [i64; 2]) -> Option<(i32, i32)> {
        let n = crate::STUD_CHUNK_STUDS as i64;
        let (cx, cz) = (column[0].div_euclid(n), column[1].div_euclid(n));
        let (sx, sz) = (column[0].rem_euclid(n) as usize, column[1].rem_euclid(n) as usize);
        let rn = crate::REGION_CHUNKS as i64;
        let g = if (0..rn).contains(&cx) && (0..rn).contains(&cz) {
            self.data.stud_ground.get(&ColumnCoords::new(cx as usize, cz as usize))?
        } else {
            let key = WorldChunkPos::from_local(*self.data.region, [cx as i32, 0, cz as i32]);
            self.data.stud_halo_ground.get(&key)?
        };
        let bed = g.surface(sx, sz);
        g.water_top(sx, sz).filter(|&top| top > bed).map(|top| (bed, top))
    }

    /// The water surface (region-local sim units) over the stud column holding
    /// `(x, z)`, if it holds recipe water.
    pub fn stud_water_surface_units(&self, x: f32, z: f32) -> Option<f32> {
        let s = crate::STUD_UNITS as f32;
        let top = self.stud_water_top([(x / s).floor() as i64, (z / s).floor() as i64])?;
        Some(crate::stud::plate_units(top))
    }

    /// Water units in stud tile `p` (region-local; own ∪ stud halo): `WATER_FULL` x
    /// the plates of water standing over the tile's own solid top (the highest water
    /// top of its four stud columns, less the highest filled plate in the tile), out
    /// of a tile's [`crate::TILE_PLATES`]. 0 for a solid tile, a dry one, or one
    /// whose bricks reach the surface. So a tile's DF depth is how deep a body
    /// standing on the tile's floor wades: 2 plates (0.4 m) is the deepest wading.
    pub fn stud_water_units(&self, p: BlockPos3) -> u16 {
        let (ts, tp) = (crate::TILE_STUDS as i64, crate::TILE_PLATES as i64);
        let bottom = i64::from(p[1]) * tp;
        let mut level = 0;
        for dz in 0..ts {
            for dx in 0..ts {
                let column = [i64::from(p[0]) * ts + dx, i64::from(p[2]) * ts + dz];
                // A tile wholly under the recipe's lake bed holds none, dug out or not.
                if let Some((bed, top)) = self.stud_water_span(column).filter(|&(bed, _)| i64::from(bed) < bottom + tp) {
                    let _ = bed;
                    level = level.max((i64::from(top) - bottom).clamp(0, tp));
                }
            }
        }
        if level == 0 {
            return 0;
        }
        // Only now the bricks: dry tiles (most) never scan a chunk.
        let solid = i64::from(self.stud_tile(p).floor_plates());
        if level <= solid {
            return 0;
        }
        (i64::from(WATER_FULL) * (level - solid) / tp) as u16
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
