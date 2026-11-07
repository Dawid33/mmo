//! Recipe water (water plan Tasks 5 and 9): worldgen writes each block
//! column's water top into its `ColumnGround`, the sim's water query reads
//! it, and the chunks themselves hold no water blocks — a recipe cell is
//! air (or ice at a frozen top).
use game::{
    Block, BlockId, BlockPos, Column, ColumnCoords, ReadVol, RegionCoords, WorldBlockPos, CHUNK_BLOCKS, REGION_BLOCKS,
    WATER_FULL,
};
use worldgen::forged::ForgedWorld;

use crate::common::*;

fn install_real_block_props() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
        game::install_block_props(reg.props());
    });
}

/// The coastal region of the synthetic forged world: the western sea and
/// the river's mouth.
fn coastal_region() -> RegionCoords {
    let (x, z) = engine(4.0 * CELL_M, RIVER_Z);
    let rb = REGION_BLOCKS as f64;
    RegionCoords::new((x / rb).floor() as i32, (z / rb).floor() as i32)
}

/// Over `columns` (region `rc`): no materialised block is liquid, and every
/// cell of every column's recipe water range is either ice or an air cell
/// the region reads as full water. Returns (water cells, of them sea).
fn assert_recipe_water_only(rc: RegionCoords, columns: &[(ColumnCoords, Column)]) -> (usize, usize) {
    let props = game::block_props();
    let region = game::Region::from_columns(rc, columns.to_vec());
    let cb = CHUNK_BLOCKS as i32;
    let (mut wet, mut sea) = (0usize, 0usize);
    for (cc, col) in columns {
        let ground = col.ground().expect("every generated column carries a recipe");
        let lo = col.base_sub_y() * cb;
        let hi = lo + col.sub_chunks().len() as i32 * cb;
        for lx in 0..cb {
            for lz in 0..cb {
                for y in lo..hi {
                    let id = col.get(BlockPos::new(lx, y, lz)).id;
                    assert!(!props.is_liquid(id), "column {cc:?} ({lx}, {lz}) y {y}: a liquid block {id:?}");
                }
                let Some((a, b)) = ground.recipe_water(lx as usize, lz as usize) else { continue };
                for y in a..=b {
                    let id = col.get(BlockPos::new(lx, y, lz)).id;
                    let p = BlockPos::new(cc.x as i32 * cb + lx, y, cc.z as i32 * cb + lz);
                    let units = region.data().water_units_at(WorldBlockPos::from_local(rc, p));
                    if id == BlockId::AIR {
                        assert_eq!(units, WATER_FULL, "column {cc:?} ({lx}, {lz}) y {y}: recipe water");
                        wet += 1;
                        sea += (y <= SEA_TOP_BLOCK) as usize;
                    } else {
                        assert!(props.is_solid(id), "column {cc:?} ({lx}, {lz}) y {y}: {id:?} in the recipe range");
                        assert_eq!(units, 0, "column {cc:?} ({lx}, {lz}) y {y}: a solid cell is dry");
                    }
                }
            }
        }
    }
    (wet, sea)
}

/// Generated chunks hold no liquid blocks (water plan Task 9): over a forged
/// coastal region and a procedural sea region, the water is the recipe's
/// alone, and the region reads it as full on every air cell of every
/// recipe range.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_liquid_blocks_in_generated_chunks() {
    install_real_block_props();
    let world = ForgedWorld::new(synthetic());
    let rc = coastal_region();
    let columns = worldgen::generate_region_forged(&world, rc, mats());
    let (wet, sea) = assert_recipe_water_only(rc, &columns);
    eprintln!("forged region {rc:?}: {wet} water cells, {sea} of them sea");
    assert!(sea > 0 && wet > sea, "the region holds sea and fresh water: {wet} water cells, {sea} sea");
    assert!(!game::Region::from_columns(rc, columns).data().water_bodies().is_empty(), "labelled at genesis");

    // Far from the procedural spawn island: open sea.
    let rc = RegionCoords::new((4096 / REGION_BLOCKS) as i32, 0);
    let columns = worldgen::generate_region(rc, worldgen::WORLD_SEED, mats());
    let (wet, _) = assert_recipe_water_only(rc, &columns);
    eprintln!("procedural region {rc:?}: {wet} water cells");
    assert!(wet > 0, "the procedural region holds sea");
}

/// The static-water edit semantics (Task 5's `edits_follow_static_water_semantics`)
/// on a generated region: a block placed into the sea wins, breaking it
/// gives the water back, and digging the seabed leaves a dry cell.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn generated_seabed_edits_follow_static_water_semantics() {
    install_real_block_props();
    let world = ForgedWorld::new(synthetic());
    let rc = coastal_region();
    let columns = worldgen::generate_region_forged(&world, rc, mats());
    let cb = CHUNK_BLOCKS as i32;
    // The first sea column (water at least two deep) in column order.
    let (x, z, bed) = columns
        .iter()
        .find_map(|(cc, col)| {
            let g = col.ground()?;
            (0..cb).flat_map(|lx| (0..cb).map(move |lz| (lx, lz))).find_map(|(lx, lz)| {
                let (a, b) = g.recipe_water(lx as usize, lz as usize)?;
                (b <= SEA_TOP_BLOCK && b > a).then_some((cc.x as i32 * cb + lx, cc.z as i32 * cb + lz, a - 1))
            })
        })
        .expect("the coastal region holds sea");
    let mut region = game::Region::from_columns(rc, columns);
    let units = |r: &game::Region, y: i32| {
        r.data().water_units_at(WorldBlockPos::from_local(rc, BlockPos::new(x, y, z)))
    };
    let bodies = region.data().water_bodies().clone();
    let stone = Block::new(mats().stone);
    let air = Block::new(BlockId::AIR);
    assert_eq!(units(&region, bed + 1), WATER_FULL, "the sea over its bed");
    region.with_data(|d| assert!(d.set_block_safe([x, bed + 1, z], stone), "place"));
    assert_eq!(units(&region, bed + 1), 0, "a placed block wins");
    region.with_data(|d| assert!(d.set_block_safe([x, bed + 1, z], air), "break it"));
    assert_eq!(units(&region, bed + 1), WATER_FULL, "inside the recipe: water again");
    region.with_data(|d| assert!(d.set_block_safe([x, bed, z], air), "dig the seabed"));
    assert_eq!(units(&region, bed), 0, "below the recipe: dry");
    assert_eq!(units(&region, bed + 1), WATER_FULL, "the sea above is unchanged");
    assert_eq!(region.data().water_bodies(), &bodies, "edits never change water state");
}
