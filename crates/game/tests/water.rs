//! The water query (`Rollback::water_units_at`).
use game::{Block, BlockId, BlockPos, WorldBlockPos, REGION_BLOCKS, WATER_FULL};

use crate::common::rig::Rig;
use crate::common::seam_translate::uniform_floor;

const WATER: BlockId = BlockId(5);

#[test]
fn water_units_at_reads_liquid_blocks() {
    crate::common::install_real_block_props();
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let at = BlockPos::new(8, 3, 8);
    rig.manager.spawner_mut().with_region(game::SPAWN_REGION, |r| {
        r.with_data(|d| assert!(d.set_block_safe([at.x, at.y, at.z], Block::new(WATER))));
        let here = |dy: i32| WorldBlockPos::from_local(game::SPAWN_REGION, BlockPos::new(at.x, at.y + dy, at.z));
        let d = r.data();
        assert_eq!(d.water_units_at(here(0)), WATER_FULL);
        assert_eq!(d.water_units_at(here(1)), 0);
        assert_eq!(d.water_units_at(here(-1)), 0);
        // Beyond own region ∪ halo: nothing.
        let far = WorldBlockPos::from_local(
            game::SPAWN_REGION,
            BlockPos::new(REGION_BLOCKS as i32 + game::HALO_BLOCKS as i32 + 40, 3, 8),
        );
        assert_eq!(d.water_units_at(far), 0);
    });
}
