//! LOD of real generated terrain read through brick views equals the
//! pre-brick downsampler's, byte for byte (see `game::lod::downsample_chunks`).

#[path = "../../game/tests/support/lod_reference.rs"]
mod lod_reference;

use game::{BlockId, RegionCoords, TerrainSnapshot};

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn far_lod_from_bricks_matches_reference_on_a_generated_region() {
    if !lod_reference::reference_applies() {
        return;
    }
    // A land region near spawn (slopes, soil, water) and an off-spawn one.
    for rc in [RegionCoords::new(0, 0), RegionCoords::new(1, -2)] {
        let chunks = TerrainSnapshot::from_columns(worldgen::generate_region(rc, worldgen::WORLD_SEED, mats())).chunks;
        let slopes = chunks.iter().flat_map(|(_, c)| c.chisel.values()).filter(|c| game::slope::classify(c).is_some());
        assert!(slopes.count() > 0, "{rc:?}: fixture has slopes");
        for factor in [2u32, 4, 16] {
            let got = game::lod::downsample_chunks(chunks.clone(), factor);
            let want = lod_reference::downsample_reference(chunks.clone(), factor);
            assert!(!got.is_empty());
            assert_eq!(
                bincode::serialize(&lod_reference::packed(&got)).unwrap(),
                bincode::serialize(&lod_reference::packed(&want)).unwrap(),
                "{rc:?} factor {factor}"
            );
        }
    }
}
