//! Forged worlds (`worldgen::forged`): the world-file format, and what the
//! engine builds from one — carved river channels, water above sea level that
//! its banks actually hold, lakes at one flat level — on a small synthetic
//! world whose right answers are known.

use game::{BlockId, BlockPos, Column, ColumnCoords, ReadVol, RegionCoords};
use worldgen::forged::{ForgedWorld, Lake, RiverPoint, RiverReach, WorldArtifact, ARTIFACT_VERSION};

const N: usize = 64;
const CELL_M: f64 = 16.0;
const SEA_LEVEL: f64 = 64.0;
const SEA_TOP_BLOCK: i32 = 63;
/// River along this artifact z, flowing west to the sea.
const RIVER_Z: f64 = 512.0;
const RIVER_WIDTH_M: f32 = 8.0;
/// Lake cells (artifact cell indices) and its level (m above sea).
const LAKE_CELLS: (std::ops::Range<usize>, std::ops::Range<usize>) = (30..36, 10..16);
const LAKE_LEVEL: f32 = 16.6;
const SPAWN: [f64; 2] = [512.0, 600.0];

/// A plain rising eastward out of a western sea: h = 2 + 0.03 x.
fn plain(ax: f64) -> f64 {
    2.0 + 0.03 * ax
}

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
    }
}

fn synthetic() -> WorldArtifact {
    let mut height = vec![0f32; N * N];
    let mut lake_level = vec![f32::NAN; N * N];
    for iz in 0..N {
        for ix in 0..N {
            let ax = (ix as f64 + 0.5) * CELL_M;
            let i = iz * N + ix;
            height[i] = if ix < 4 { -5.0 } else { plain(ax) as f32 };
            if LAKE_CELLS.0.contains(&ix) && LAKE_CELLS.1.contains(&iz) {
                height[i] -= 5.0; // the basin
                lake_level[i] = LAKE_LEVEL;
            }
        }
    }
    // Source in the east, mouth in the western sea; water just under the plain.
    let points = (4..=56)
        .rev()
        .map(|k| {
            let x = k as f64 * CELL_M;
            let water = (plain(x) - 0.2) as f32;
            RiverPoint { x: x as f32, z: RIVER_Z as f32, bed: water - 1.0, water, in_lake: false }
        })
        .collect();
    WorldArtifact {
        version: ARTIFACT_VERSION,
        seed: 7,
        size_m: N as f64 * CELL_M,
        cell_m: CELL_M,
        n: N as u32,
        height,
        lake_level,
        uplifted: vec![0.0; N * N],
        bedding: vec![0.0; N * N],
        column: vec![(700.0, 0)],
        rock_names: vec!["sandstone".into()],
        rivers: vec![RiverReach { order: 1, discharge_m2: 2.0e6, width_m: RIVER_WIDTH_M, points }],
        lakes: vec![Lake { level: LAKE_LEVEL, area_m2: 36.0 * 256.0, max_depth: 5.0, outlet: [480.0, 200.0] }],
        spawn: SPAWN,
    }
}

/// Artifact meters -> engine world meters (spawn lands on engine (8.5, 8.5)).
fn engine(ax: f64, az: f64) -> (f64, f64) {
    (ax - SPAWN[0] + 8.5, az - SPAWN[1] + 8.5)
}

#[test]
fn world_file_round_trips_and_rejects_foreign_bytes() {
    let artifact = synthetic();
    let bytes = artifact.to_bytes();
    // Byte-level comparison: the lake raster is full of NaN, which != itself.
    assert_eq!(WorldArtifact::from_bytes(&bytes).unwrap().to_bytes(), bytes);

    assert!(WorldArtifact::from_bytes(b"not a world").is_err());
    let mut wrong_version = bytes.clone();
    wrong_version[4] = 99;
    let err = WorldArtifact::from_bytes(&wrong_version).unwrap_err().to_string();
    assert!(err.contains("re-forge"), "{err}");
    assert!(WorldArtifact::from_bytes(&bytes[..bytes.len() / 2]).is_err(), "truncated file");
}

#[test]
fn spawn_maps_to_the_engine_spawn_column_and_the_rim_is_ocean() {
    let world = ForgedWorld::new(synthetic());
    let at_spawn = world.sample(8.5, 8.5).ground_y;
    let expected = SEA_LEVEL + plain(SPAWN[0]);
    assert!((at_spawn - expected).abs() < 1.0, "spawn ground {at_spawn}, plain says {expected}");
    // Far outside the artifact: deep open sea, no fresh water.
    let outside = world.sample(50_000.0, -50_000.0);
    assert!(outside.ground_y < SEA_LEVEL - 50.0);
    assert_eq!(outside.water_y, None);
}

#[test]
fn river_channel_is_carved_holds_water_and_has_banks() {
    let world = ForgedWorld::new(synthetic());
    for ax in [200.0, 333.0, 620.0, 850.0] {
        let plain_y = SEA_LEVEL + plain(ax);
        let (x, z) = engine(ax, RIVER_Z);
        let mid = world.sample(x, z);
        let water = mid.water_y.expect("water on the centerline");
        assert_eq!(water, water.floor(), "water surfaces sit on whole blocks");
        assert!((water - plain_y).abs() <= 1.0, "river rides the plain: {water} vs {plain_y}");
        assert!(mid.ground_y <= water - 0.7, "bed under water: ground {} water {water}", mid.ground_y);

        // Just outside the channel the bank stands above the water it holds.
        let half = RIVER_WIDTH_M as f64 * 0.5;
        for side in [-1.0, 1.0] {
            let bank = world.sample(x, z + side * (half + 1.0));
            assert_eq!(bank.water_y, None, "no water on the bank");
            assert!(bank.ground_y >= water + 0.29, "bank {} under water {water} at ax={ax}", bank.ground_y);
        }
        // Well away from the river the plain is untouched (detail noise aside).
        let away = world.sample(x, z + 200.0);
        assert!((away.ground_y - plain_y).abs() < 2.5 && away.water_y.is_none());
    }
}

#[test]
fn lake_is_flat_and_its_shore_stands_above_it() {
    let world = ForgedWorld::new(synthetic());
    let level = SEA_LEVEL + (LAKE_LEVEL as f64 + 0.5).floor();
    let mut wet = 0;
    // Walk a line through the lake and out both sides.
    for k in 0..200 {
        let ax = 420.0 + k as f64 * 1.0;
        let (x, z) = engine(ax, 13.0 * CELL_M);
        let s = world.sample(x, z);
        if let Some(w) = s.water_y {
            wet += 1;
            assert_eq!(w, level, "one flat surface");
            assert!(s.ground_y < w);
        } else {
            // Dry ground right next to the lake must not sit below its surface.
            let in_ring = (ax - 480.0).abs() < CELL_M || (ax - 576.0).abs() < CELL_M;
            if in_ring && ax < 480.0 {
                assert!(s.ground_y >= level + 0.29, "low shore at ax={ax}: {} < {level}", s.ground_y);
            }
        }
    }
    assert!(wet > 60, "the lake is ~96 m across on this line, {wet} wet samples");
}

/// (top solid-or-water block, is water) for every block column of a region,
/// keyed by region-local block (x, z).
fn tops(columns: &[(ColumnCoords, Column)]) -> std::collections::BTreeMap<(i32, i32), (i32, bool)> {
    let per = game::CHUNK_BLOCKS as i32;
    let mut out = std::collections::BTreeMap::new();
    for (cc, col) in columns {
        for lx in 0..per {
            for lz in 0..per {
                let top = (0..200)
                    .rev()
                    .map(|y| (y, col.get(BlockPos::new(lx, y, lz)).id))
                    .find(|(_, id)| *id != BlockId::AIR);
                if let Some((y, id)) = top {
                    out.insert((cc.x as i32 * per + lx, cc.z as i32 * per + lz), (y, id == mats().water));
                }
            }
        }
    }
    out
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn river_region_is_deterministic_and_its_water_is_contained() {
    let world = ForgedWorld::new(synthetic());
    // Engine region holding the river around artifact x = 400.
    let (x, z) = engine(400.0, RIVER_Z);
    let rc = RegionCoords::new((x / 64.0).floor() as i32, (z / 64.0).floor() as i32);
    let a = worldgen::generate_region_forged(&world, rc, mats());
    let b = worldgen::generate_region_forged(&world, rc, mats());
    assert_eq!(bincode::serialize(&a).unwrap(), bincode::serialize(&b).unwrap());

    let tops = tops(&a);
    let (mut fresh, mut leaks) = (0, 0);
    for (&(bx, bz), &(top, is_water)) in &tops {
        if !is_water || top <= SEA_TOP_BLOCK {
            continue;
        }
        fresh += 1;
        for (ox, oz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            // A neighbor holds this water if it is water no more than a step
            // lower (rivers descend in 1-block steps), or ground at least as high.
            if let Some(&(ntop, nwater)) = tops.get(&(bx + ox, bz + oz)) {
                let holds = if nwater { ntop >= top - 1 } else { ntop >= top };
                leaks += !holds as usize;
            }
        }
    }
    println!("river region {rc:?}: {fresh} fresh-water columns, {leaks} open sides");
    assert!(fresh > 300, "an 8 m river crosses a 64 m region: {fresh} columns");
    assert_eq!(leaks, 0, "water with an open side");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lake_shows_in_regions_and_in_coarse_lod() {
    let world = ForgedWorld::new(synthetic());
    let (x, z) = engine(33.0 * CELL_M, 13.0 * CELL_M); // middle of the lake
    let rc = RegionCoords::new((x / 64.0).floor() as i32, (z / 64.0).floor() as i32);
    let level_top = (SEA_LEVEL + (LAKE_LEVEL as f64 + 0.5).floor()) as i32 - 1;

    let tops = tops(&worldgen::generate_region_forged(&world, rc, mats()));
    let lake_columns = tops.values().filter(|&&(top, water)| water && top == level_top).count();
    assert!(lake_columns > 1000, "{lake_columns} lake columns at block {level_top}");
    assert!(
        tops.values().all(|&(top, water)| !water || top == level_top || top <= SEA_TOP_BLOCK),
        "every fresh-water column of the lake region stands at the one level"
    );

    // Far LOD, factor 16 (1-block cells): the same surface block is water.
    let key = game::lod::LodKey::for_region(16, rc);
    let water = mats().water;
    let lod_water = worldgen::coarse_tile_forged(&world, &key, mats())
        .iter()
        .filter(|cell| cell.group[1] * 32 <= level_top && level_top < cell.group[1] * 32 + 32)
        .flat_map(|cell| {
            let ly = (level_top - cell.group[1] * 32) as usize;
            (0..32 * 32).map(move |k| cell.grid[(k % 32) + 32 * ly + 32 * 32 * (k / 32)].block)
        })
        .filter(|&block| block == water)
        .count();
    assert!(lod_water > 1000, "{lod_water} water cells at the lake level in the factor-16 LOD");
}
