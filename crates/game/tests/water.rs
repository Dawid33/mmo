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
