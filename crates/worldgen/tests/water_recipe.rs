//! Recipe water (water plan Task 5): worldgen writes each block column's
//! water top into its `ColumnGround`, and the sim's water query reads it.
//! While worldgen still writes water blocks too, the recipe must cover
//! exactly those blocks.
use game::{BlockPos, ReadVol, RegionCoords, WorldBlockPos, CHUNK_BLOCKS, REGION_BLOCKS};
use worldgen::forged::ForgedWorld;

use crate::common::*;

fn install_real_block_props() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
        game::install_block_props(reg.props());
    });
}

/// Over a coastal region of the synthetic forged world (the western sea
/// and the river's mouth), every block holds recipe water (`water_units_at
/// > 0`) iff worldgen wrote a water block there, and every recipe range is
/// exactly its column's water blocks.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forged_water_recipe_matches_water_blocks() {
    install_real_block_props();
    let world = ForgedWorld::new(synthetic());
    let (x, z) = engine(4.0 * CELL_M, RIVER_Z);
    let rb = REGION_BLOCKS as f64;
    let rc = RegionCoords::new((x / rb).floor() as i32, (z / rb).floor() as i32);
    let columns = worldgen::generate_region_forged(&world, rc, mats());
    let region = game::Region::from_columns(rc, columns.clone());
    let water = mats().water;
    let cb = CHUNK_BLOCKS as i32;
    let (mut wet, mut sea) = (0usize, 0usize);
    for (cc, col) in &columns {
        let ground = col.ground().expect("every forged column carries a recipe");
        let lo = ground.band_base_sub_y * cb;
        let hi = lo + col.sub_chunks().len() as i32 * cb + cb;
        for lx in 0..cb {
            for lz in 0..cb {
                let recipe = ground.recipe_water(lx as usize, lz as usize);
                for y in lo..hi {
                    let is_water = col.get(BlockPos::new(lx, y, lz)).id == water;
                    let in_recipe = recipe.is_some_and(|(a, b)| (a..=b).contains(&y));
                    assert_eq!(in_recipe, is_water, "column {cc:?} ({lx}, {lz}) y {y}: recipe {recipe:?}");
                    let p = BlockPos::new(cc.x as i32 * cb + lx, y, cc.z as i32 * cb + lz);
                    let units = region.data().water_units_at(WorldBlockPos::from_local(rc, p));
                    assert_eq!(units > 0, is_water, "column {cc:?} ({lx}, {lz}) y {y}: {units} units");
                    wet += is_water as usize;
                    sea += (is_water && y <= SEA_TOP_BLOCK) as usize;
                }
            }
        }
    }
    eprintln!("region {rc:?}: {wet} water blocks, {sea} of them sea");
    assert!(sea > 0 && wet > sea, "the region holds sea and fresh water: {wet} water blocks, {sea} sea");
    assert!(!region.data().water_bodies().is_empty(), "labelled at genesis");
}
