//! Water as the sim reads it: units per cell, the DF depth levels over them,
//! and the rules pathing and swimming derive from them. Static water is the
//! ground recipe's (`ColumnGround::recipe_water`) over air cells, labelled
//! into bodies at genesis ([`label_water_bodies`]); chunks hold no water
//! blocks (a liquid block — `BlockKind::Liquid`, no collider — is not water
//! by itself). [`Rollback::water_units_at`] is the single water read in the sim.

use std::collections::BTreeMap;

use rapier3d::math::Vector;

use crate::movement::{Immersion, PLAYER_EYE_HEIGHT, PLAYER_HEIGHT};
use crate::{
    BLOCK_VOXELS, BlockId, BlockPos, CHUNK_BLOCKS, ColumnCoords, ColumnGround, EntityKey,
    REGION_BLOCKS, Rollback, WorldBlockPos,
};

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
/// `ColumnGround::water_top` of a block column without recipe water.
pub const NO_WATER: i16 = i16::MIN;
/// `ColumnGround::water_body` of a block column in no water body.
pub const NO_BODY: u32 = u32::MAX;

/// A water body of one region: its region-local lexicographically smallest
/// member block column, packed as `x * REGION_BLOCKS + z`. (`Default` only
/// because `#[undo(map)]` keys need it.)
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterBodyId(pub u32);

/// A body of static water: the 4-connected block columns of one region
/// sharing one recipe `water_top`, labelled once at genesis.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct WaterBody {
    /// The surface, absolute: `(water_top + 1) * WATER_FULL`.
    pub level: i64,
    /// `WATER_FULL` per recipe cell that was not ice at genesis.
    pub volume: u64,
}

/// Water units of a cell holding `block`, at block Y `y` of a column whose
/// recipe water is `recipe` (`ColumnGround::recipe_water`): full for air
/// inside the recipe, else none — water is the recipe's alone, chunks hold
/// no water blocks. The rule the sim and the client's tile mirror share.
pub fn static_water_units(block: BlockId, recipe: Option<(i32, i32)>, y: i32) -> u16 {
    let in_recipe = block == BlockId::AIR && recipe.is_some_and(|(lo, hi)| (lo..=hi).contains(&y));
    if in_recipe { WATER_FULL } else { 0 }
}

/// Labels the recipe water of a region's ground into bodies: a
/// deterministic 4-connected flood over the region's block columns in
/// ascending (x, z) order, joining columns with equal `water_top`. Writes
/// each column's `water_body`; a body's `volume` counts the cells of its
/// columns' recipe ranges for which `is_ice(column, (lx, lz), y)` is false.
pub fn label_water_bodies(
    ground: &mut [(ColumnCoords, ColumnGround)],
    is_ice: impl Fn(ColumnCoords, (usize, usize), i32) -> bool,
) -> BTreeMap<WaterBodyId, WaterBody> {
    let cb = CHUNK_BLOCKS;
    // Block column (x, z) -> (ground index, lx, lz), in ascending order.
    let mut at: BTreeMap<(usize, usize), (usize, usize, usize)> = BTreeMap::new();
    for (i, (c, g)) in ground.iter_mut().enumerate() {
        g.water_body.iter_mut().for_each(|b| *b = NO_BODY);
        for lx in 0..cb {
            for lz in 0..cb {
                if g.recipe_water(lx, lz).is_some() {
                    at.insert((c.x * cb + lx, c.z * cb + lz), (i, lx, lz));
                }
            }
        }
    }
    let mut bodies = BTreeMap::new();
    let keys: Vec<(usize, usize)> = at.keys().copied().collect();
    for start in keys {
        let (i, lx, lz) = at[&start];
        if ground[i].1.water_body[lx * cb + lz] != NO_BODY {
            continue;
        }
        let top = ground[i].1.water_top[lx * cb + lz];
        let id = WaterBodyId((start.0 * REGION_BLOCKS as usize + start.1) as u32);
        let mut body = WaterBody {
            level: (i64::from(top) + 1) * i64::from(WATER_FULL),
            volume: 0,
        };
        ground[i].1.water_body[lx * cb + lz] = id.0;
        let mut stack = vec![start];
        while let Some((x, z)) = stack.pop() {
            let (i, lx, lz) = at[&(x, z)];
            let (c, g) = &ground[i];
            let (lo, hi) = g
                .recipe_water(lx, lz)
                .expect("labelled columns hold recipe water");
            let cells = (lo..=hi).filter(|&y| !is_ice(*c, (lx, lz), y)).count() as u64;
            body.volume += cells * u64::from(WATER_FULL);
            for n in [
                (x + 1, z),
                (x, z + 1),
                (x.wrapping_sub(1), z),
                (x, z.wrapping_sub(1)),
            ] {
                let Some(&(j, nx, nz)) = at.get(&n) else {
                    continue;
                };
                let g = &mut ground[j].1;
                if g.water_body[nx * cb + nz] == NO_BODY && g.water_top[nx * cb + nz] == top {
                    g.water_body[nx * cb + nz] = id.0;
                    stack.push(n);
                }
            }
        }
        bodies.insert(id, body);
    }
    bodies
}

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
        let movers: Vec<EntityKey> = self
            .data
            .ecs
            .motion
            .iter()
            .filter_map(|(k, m)| m.map(|_| k))
            .collect();
        for e in movers {
            let Some((feet, height)) = self.mover_extent(e) else {
                continue;
            };
            let Some(mut motion) = *self.data.ecs.motion.try_get(e) else {
                continue;
            };
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
        let collider = self
            .data
            .physics
            .colliders
            .get(*body.colliders().first()?)?;
        let aabb = collider.shape().compute_local_aabb();
        let lift = collider
            .position_wrt_parent()
            .map_or(0.0, |p| p.translation.y);
        let t = body.translation();
        Some((
            Vector::new(t.x, t.y + lift + aabb.mins.y, t.z),
            aabb.maxs.y - aabb.mins.y,
        ))
    }

    /// Water units in world block `p`, own region ∪ halo; 0 beyond them.
    /// Static water ([`static_water_units`]): [`WATER_FULL`] for an air
    /// cell inside its column's recipe water — the
    /// region's own `ground` for its cells, the neighbour's (`halo_ground`)
    /// for halo cells.
    pub fn water_units_at(&self, p: WorldBlockPos) -> u16 {
        // Canonical (an unwrapped cell of this region's frame folds), so the
        // halo's ground key below is the neighbour's.
        let p = self.data.canonical_block(p);
        let Some(block) = self.halo_block(p) else {
            return 0;
        };
        let cb = CHUNK_BLOCKS as i64;
        let (lx, lz) = (p.x.rem_euclid(cb) as usize, p.z.rem_euclid(cb) as usize);
        let recipe = self
            .recipe_ground_at(p)
            .and_then(|g| g.recipe_water(lx, lz));
        static_water_units(block.id, recipe, p.y)
    }

    /// The ground recipe of world block `p`'s column: own (`ground`) inside
    /// the region, else the halo's (`halo_ground`).
    fn recipe_ground_at(&self, p: WorldBlockPos) -> Option<&ColumnGround> {
        let n = REGION_BLOCKS as i32;
        let cb = CHUNK_BLOCKS as i32;
        let [x, _, z] = self.data.local_block(p);
        if (0..n).contains(&x) && (0..n).contains(&z) {
            return self
                .data
                .ground
                .get(&ColumnCoords::new((x / cb) as usize, (z / cb) as usize));
        }
        self.data.halo_ground.get(
            &self
                .data
                .world_chunk([x.div_euclid(cb), 0, z.div_euclid(cb)]),
        )
    }

    /// The water body of region-local block `pos`'s column, when the cell
    /// holds water (`water_units_at > 0`). Own cells only: bodies are per
    /// region, so a halo cell has none here.
    pub fn water_body_at(&self, pos: BlockPos) -> Option<WaterBodyId> {
        let n = REGION_BLOCKS as i32;
        if !(0..n).contains(&pos.x) || !(0..n).contains(&pos.z) {
            return None;
        }
        if self.water_units_at(WorldBlockPos::from_local(*self.data.region, pos)) == 0 {
            return None;
        }
        let cb = CHUNK_BLOCKS as i32;
        let g = self.data.ground.get(&ColumnCoords::new(
            (pos.x / cb) as usize,
            (pos.z / cb) as usize,
        ))?;
        let b = g.water_body[(pos.x % cb) as usize * CHUNK_BLOCKS + (pos.z % cb) as usize];
        (b != NO_BODY).then_some(WaterBodyId(b))
    }
}
