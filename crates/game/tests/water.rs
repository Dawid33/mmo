//! The water query (`Rollback::water_units_at`).
use game::{BlockPos, WorldBlockPos, REGION_BLOCKS, SPAWN_REGION, WATER_FULL};

use crate::common::rig::Rig;
use crate::common::water::{floor_with_water, WaterBox};
use crate::common::STAND;

/// The level of the floor's top (solid) tile.
const G: i32 = STAND - 1;

/// Tile column (8, 8) of the spawn region holds recipe water at the three tile
/// levels over the floor: full there, dry on the floor, above it and beside
/// it; nothing beyond own region ∪ halo.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn water_units_at_reads_recipe_water() {
    crate::common::install_real_block_props();
    let n = REGION_BLOCKS as i64;
    let (x, z) = (SPAWN_REGION.x as i64 * n + 8, SPAWN_REGION.z as i64 * n + 8);
    let pond = WaterBox { xs: x..x + 1, zs: z..z + 1, surface: G, water_top: G + 3 };
    let mut rig = Rig::with_floor(None, move |rc| floor_with_water(rc, std::slice::from_ref(&pond)));
    rig.join_spawn();
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let at = |x: i32, y: i32, z: i32| WorldBlockPos::from_local(SPAWN_REGION, BlockPos::new(x, y, z));
        let d = r.data();
        for y in G + 1..=G + 3 {
            assert_eq!(d.water_units_at(at(8, y, 8)), WATER_FULL, "y {y}");
        }
        assert_eq!(d.water_units_at(at(8, G, 8)), 0, "the floor");
        assert_eq!(d.water_units_at(at(8, G + 4, 8)), 0, "above the surface");
        assert_eq!(d.water_units_at(at(9, G + 2, 8)), 0, "beside the column");
        // Beyond own region ∪ halo: nothing.
        let far = at(REGION_BLOCKS as i32 + game::HALO_BLOCKS as i32 + 40, G + 3, 8);
        assert_eq!(d.water_units_at(far), 0);
    });
}

/// A swimmer is restored in place: a record whose feet stand mid-pond in
/// deep recipe water keeps its position (water is not terrain), as does
/// one on dry ground.
#[test]
fn a_swimmer_is_restored_in_place() {
    crate::common::install_real_block_props();
    let rc = game::RegionCoords::new(0, 0);
    let pond = WaterBox { xs: 20..24, zs: 20..24, surface: G, water_top: G + 3 };
    let region = crate::common::region_of(rc, floor_with_water(rc, &[pond]));
    let d = region.data();
    let mid_pond = crate::common::body_on_tile([22, G + 2, 22]);
    let feet = crate::common::feet_tile(mid_pond);
    let cell = WorldBlockPos::from_local(rc, BlockPos::new(feet[0], feet[1], feet[2]));
    assert_eq!(d.water_units_at(cell), WATER_FULL, "the record swims in the pond");
    assert_eq!(game::restore_translation(d, mid_pond), mid_pond, "a swimmer keeps its place");
    let n = REGION_BLOCKS as i32;
    let dry = crate::common::body_on_tile([n - 4, G + 1, n - 4]);
    assert_eq!(game::restore_translation(d, dry), dry, "a dry record keeps its place");
}

/// A body created over recipe water starts on the water's top, not on the
/// bed under it (`player_spawn_y`); over dry ground, on the ground.
#[test]
fn spawn_height_over_water_is_the_water_top() {
    crate::common::install_real_block_props();
    let rc = game::RegionCoords::new(0, 0);
    let pond = WaterBox { xs: 20..24, zs: 20..24, surface: G, water_top: G + 3 };
    let region = crate::common::region_of(rc, floor_with_water(rc, &[pond]));
    let d = region.data();
    let bv = crate::common::TILE_W;
    let centre = |b: i32| (b as f32 + 0.5) * bv;
    let (wet, dry) = (d.player_spawn_y((centre(22), centre(22))), d.player_spawn_y((centre(40), centre(40))));
    // The floor's top is the bottom of tile level G + 1; the water's top the
    // bottom of level G + 4.
    let _ = bv;
    let water = game::plate_units(3 * game::TILE_PLATES as i32);
    assert!((wet - dry - water).abs() < 1e-3, "three water levels above the floor: wet {wet}, dry {dry}");
}
