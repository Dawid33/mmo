//! The water query (`Rollback::water_units_at`).
use game::{BlockPos, WorldBlockPos, REGION_BLOCKS, SPAWN_REGION, WATER_FULL};

use crate::common::rig::Rig;
use crate::common::water::{floor_with_water, WaterBox};

/// Block column (8, 8) of the spawn region holds recipe water at levels
/// 1..=3 over the one-block floor: full there, dry on the floor, above it
/// and beside it; nothing beyond own region ∪ halo.
#[test]
fn water_units_at_reads_recipe_water() {
    crate::common::install_real_block_props();
    let n = REGION_BLOCKS as i64;
    let (x, z) = (SPAWN_REGION.x as i64 * n + 8, SPAWN_REGION.z as i64 * n + 8);
    let pond = WaterBox { xs: x..x + 1, zs: z..z + 1, surface: 0, water_top: 3 };
    let mut rig = Rig::with_floor(None, move |rc| floor_with_water(rc, std::slice::from_ref(&pond)));
    rig.join_spawn();
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let at = |x: i32, y: i32, z: i32| WorldBlockPos::from_local(SPAWN_REGION, BlockPos::new(x, y, z));
        let d = r.data();
        for y in 1..=3 {
            assert_eq!(d.water_units_at(at(8, y, 8)), WATER_FULL, "y {y}");
        }
        assert_eq!(d.water_units_at(at(8, 0, 8)), 0, "the floor");
        assert_eq!(d.water_units_at(at(8, 4, 8)), 0, "above the surface");
        assert_eq!(d.water_units_at(at(9, 2, 8)), 0, "beside the column");
        // Beyond own region ∪ halo: nothing.
        let far = at(REGION_BLOCKS as i32 + game::HALO_BLOCKS as i32 + 40, 3, 8);
        assert_eq!(d.water_units_at(far), 0);
    });
}

/// Water is the recipe's alone (water plan Task 9): a liquid block outside
/// any recipe range holds none, and a recipe cell holds water only while
/// it is air.
#[test]
fn only_recipe_air_is_water() {
    crate::common::install_real_block_props();
    let liquid = game::BlockId(5);
    assert!(game::block_props().is_liquid(liquid));
    assert_eq!(game::static_water_units(liquid, None, 2), 0, "a liquid block is not water by itself");
    assert_eq!(game::static_water_units(game::BlockId::AIR, Some((1, 3)), 2), WATER_FULL);
    assert_eq!(game::static_water_units(game::BlockId(1), Some((1, 3)), 2), 0, "a block in the recipe wins");
    assert_eq!(game::static_water_units(game::BlockId::AIR, Some((1, 3)), 4), 0, "above the recipe");
}

/// A parked player is never restored into recipe water: a record whose feet
/// stand in the pond is moved to a dry place, one on dry ground keeps its
/// position.
#[test]
fn restore_is_never_into_recipe_water() {
    crate::common::install_real_block_props();
    let rc = game::RegionCoords::new(0, 0);
    // Clear of the spawn column (`PLAYER_SPAWN_X/Z`, block 8, 8): the fallback.
    let pond = WaterBox { xs: 20..24, zs: 20..24, surface: 0, water_top: 3 };
    let region = game::Region::from_columns(rc, floor_with_water(rc, &[pond]));
    let d = region.data();
    let wet = |t: game::rapier3d::math::Vector| {
        let feet = game::feet_tile(game::feet_of(t));
        d.water_units_at(WorldBlockPos::from_local(rc, BlockPos::new(feet[0], feet[1], feet[2]))) > 0
    };
    let in_pond = game::body_on_tile([22, 1, 22]);
    assert!(wet(in_pond), "the record stands in the pond");
    let at = game::restore_translation(d, in_pond);
    assert!(!wet(at), "restored into the water at {at:?}");
    let n = REGION_BLOCKS as i32;
    let dry = game::body_on_tile([n - 4, 1, n - 4]);
    assert_eq!(game::restore_translation(d, dry), dry, "a dry record keeps its place");
}
