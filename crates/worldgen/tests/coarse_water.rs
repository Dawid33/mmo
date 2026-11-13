//! LOD water surface layer (water plan Task 8): coarse LOD terrain carries
//! no liquid (the seabed shows), and the water surface rides beside it as a
//! `LodWater` taken from the recipe (`worldgen::coarse_water`).
use game::lod::{cells_per_region, LodKey, LodWater};
use game::{build_lod_job, LodBuildJob, RegionCoords, NO_WATER, REGION_BLOCKS};
use worldgen::forged::ForgedWorld;

use crate::common::*;

fn install_real_block_props() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
        game::install_block_props(reg.props());
    });
}

fn side_of(key: &LodKey) -> u16 {
    (key.region_span() as usize * cells_per_region(key.factor)) as u16
}

/// Over the procedural world's open sea every water column's top is the sea
/// surface, the layer covers the key's LOD columns exactly, and a sub-block
/// key (sampled per block) agrees with it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coarse_water_matches_sea_level() {
    // Far from the spawn island: open sea (with maybe an island).
    let rc = RegionCoords::new((4096 / REGION_BLOCKS) as i32, 0);
    for factor in [4, 16, 64] {
        let key = LodKey::for_region(factor, rc);
        let water = worldgen::coarse_water(&key, worldgen::WORLD_SEED, mats());
        assert_eq!(water.side, side_of(&key), "factor {factor}");
        let tops = water.decode().expect("well-formed");
        let wet: Vec<_> = tops.iter().filter(|&&(top, _)| top != NO_WATER).collect();
        assert!(!wet.is_empty(), "factor {factor}: {key:?} holds sea");
        for &&(top, depth) in &wet {
            assert_eq!(i32::from(top), worldgen::pipeline::SEA_TOP_BLOCK, "factor {factor}");
            assert!(depth > 0, "factor {factor}: water over its floor");
        }
    }
}

/// No decoded grid voxel of a coastal key is liquid, however it is built:
/// pristine coarse sampling, a generated (sub-block) region, or a generated
/// region overlaid on the coarse tile. The water is in the key's `LodWater`.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coarse_lod_terrain_has_no_liquid() {
    install_real_block_props();
    let world = ForgedWorld::new(synthetic());
    let (x, z) = engine(4.0 * CELL_M, RIVER_Z); // the western sea and the river's mouth
    let rb = REGION_BLOCKS as f64;
    let rc = RegionCoords::new((x / rb).floor() as i32, (z / rb).floor() as i32);
    let coarse = |k: &LodKey| worldgen::coarse_tile_forged(&world, k, mats());
    let water = |k: &LodKey| worldgen::coarse_water_forged(&world, k, mats());
    let generate = |r: RegionCoords| worldgen::generate_region_forged(&world, r, mats());
    let props = game::block_props();
    let generated = game::TerrainSnapshot::from_columns(generate(rc)).chunks;
    assert!(
        !generated.iter().any(|(_, c)| {
            let n = game::CHUNK_BLOCKS;
            (0..n * n * n).any(|i| props.is_liquid(c.block_at(i % n, i / n % n, i / (n * n)).id))
        }),
        "the region's chunks hold no water blocks (water plan Task 9)"
    );
    let jobs = [
        ("pristine factor 4", LodBuildJob::Pristine { key: LodKey::for_region(4, rc), region: rc }),
        ("coarse factor 16", LodBuildJob::Tile { key: LodKey::for_region(16, rc), overlays: vec![], stored: vec![] }),
        (
            "overlaid factor 16",
            LodBuildJob::Tile { key: LodKey::for_region(16, rc), overlays: vec![(rc, generated.clone())], stored: vec![] },
        ),
        ("overlaid factor 64", LodBuildJob::Tile { key: LodKey::for_region(64, rc), overlays: vec![(rc, generated)], stored: vec![] }),
    ];
    for (what, job) in jobs {
        let (key, payload, lod_water) = build_lod_job(job, &coarse, &water, &generate, &game::NoStoredTerrain);
        assert!(!payload.is_empty(), "{what}: terrain");
        for (group, packed) in &payload {
            let grid = packed.decode().expect("well-formed");
            assert!(!grid.iter().any(|v| props.is_liquid(v.block)), "{what}: liquid in grid {group:?}");
        }
        assert_eq!(lod_water, water(&key), "{what}: every job kind takes the recipe's water");
        let tops = lod_water.decode().expect("well-formed");
        assert!(
            tops.iter().any(|&(top, _)| i32::from(top) == SEA_TOP_BLOCK),
            "{what}: the sea is in the water layer"
        );
    }
}

/// `LodWater` RLE round trip.
#[test]
fn coarse_water_rle_round_trips() {
    let tops = [(NO_WATER, 0), (NO_WATER, 0), (63, 4), (63, 4), (63, 5), (NO_WATER, 0), (70, 255), (70, 255), (70, 255)];
    let w = LodWater::encode(&tops, 3);
    assert_eq!(w.runs.len(), 5);
    assert_eq!(w.decode().as_deref(), Some(&tops[..]));
}
