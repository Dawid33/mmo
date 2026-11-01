//! Forged worlds (`worldgen::forged`): the world-file format, and what the
//! engine builds from one — carved river channels, water above sea level that
//! its banks actually hold, lakes at one flat level — on a small synthetic
//! world whose right answers are known.

use game::{classify, Biome, BlockId, BlockPos, Column, ColumnCoords, ReadVol, RegionCoords};
use worldgen::forged::{
    delta_rows, undelta_rows, ArtifactError, EdgeKind, ForgedWorld, QRaster, WorldArtifact, ARTIFACT_VERSION, HEIGHT_STEP_M,
};

mod common;
use common::*;

#[test]
fn world_file_round_trips_and_rejects_foreign_bytes() {
    let artifact = synthetic();
    let bytes = artifact.to_bytes();
    assert_eq!(WorldArtifact::from_bytes(&bytes).unwrap(), artifact);
    assert_eq!(WorldArtifact::from_bytes(&bytes).unwrap().to_bytes(), bytes);

    assert!(WorldArtifact::from_bytes(b"not a world").is_err());
    let mut wrong_version = bytes.clone();
    wrong_version[4] = 99;
    let err = WorldArtifact::from_bytes(&wrong_version).unwrap_err().to_string();
    assert!(err.contains("newer"), "{err}");
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

/// The synthetic world's east side is its high edge (the plain rising out
/// of the western sea): declared land there, the ground runs on past it.
fn east_land_edge() -> ForgedWorld {
    let mut artifact = synthetic();
    artifact.edges = [EdgeKind::Sea, EdgeKind::Land, EdgeKind::Sea, EdgeKind::Sea];
    ForgedWorld::new(artifact)
}

#[test]
fn outside_land_edge_continues_land() {
    let world = east_land_edge();
    let size = N as f64 * CELL_M;
    for az in [100.0, 333.0, RIVER_Z, 900.0] {
        let (ex, ez) = engine(size, az);
        let edge = world.sample(ex, ez);
        // The border's own ground: the plain's height at the map edge.
        assert!(
            (edge.ground_y - (SEA_LEVEL + plain(size))).abs() < 4.0,
            "edge ground {} vs plain {} at az={az}",
            edge.ground_y,
            SEA_LEVEL + plain(size)
        );
        for past in [1.0, 64.0, 500.0, 5_000.0] {
            let out = world.sample(ex + past, ez);
            assert_eq!(out.ground_y, edge.ground_y, "{past} m past the land edge at az={az}: the edge height");
            assert_eq!(out.water_y, None, "no rivers or lakes past the edge");
        }
        // Continuous across the edge: no cliff.
        let inside = world.sample(ex - 0.25, ez);
        assert!((inside.ground_y - edge.ground_y).abs() < 0.5, "step at the edge: {} vs {}", inside.ground_y, edge.ground_y);
    }
}

#[test]
fn outside_sea_edge_is_ocean() {
    let size = N as f64 * CELL_M;
    let ocean = SEA_LEVEL + worldgen::forged::OUTSIDE_HEIGHT_M;
    // All edges sea (the default): ocean past the east edge too.
    let world = ForgedWorld::new(synthetic());
    let (ex, ez) = engine(size + 500.0, 333.0);
    assert_eq!(world.sample(ex, ez), worldgen::forged::PointSample { ground_y: ocean, water_y: None });
    // A land east edge does not carry the land round a sea corner: past
    // both the east (land) and the north (sea) edge it is ocean, and so is
    // the west (sea) side.
    let world = east_land_edge();
    let (cx, cz) = engine(size + 500.0, -500.0);
    assert_eq!(world.sample(cx, cz).ground_y, ocean);
    let (wx, wz) = engine(-500.0, 333.0);
    assert_eq!(world.sample(wx, wz).ground_y, ocean);
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

/// Block at world block Y `y` of block column `(lx, lz)` of `col`: the band
/// where it is materialised, the ground recipe below it.
fn block_in(col: &Column, lx: usize, lz: usize, y: i32) -> BlockId {
    let ground = col.ground().expect("every generated column carries ground");
    if ground.is_implied(y.div_euclid(game::CHUNK_BLOCKS as i32)) {
        ground.block_at(lx, lz, y).id
    } else {
        col.get(BlockPos::new(lx as i32, y, lz as i32)).id
    }
}

#[test]
fn shaft_crosses_soil_then_layers_then_bedrock() {
    // No uplift or bedding: geo is the sea-level block 64, so s = y - 64 and
    // sandstone holds s >= -25 (y >= 39), shale below.
    let world = ForgedWorld::new(synthetic());
    let columns = worldgen::generate_region_forged(&world, RegionCoords::new(0, 0), mats());
    // Spawn block (8, 8): on the plain, well away from the river and the lake.
    // Its chunk column and block-in-chunk follow from the chunk size.
    let cb = game::CHUNK_BLOCKS;
    let spawn_block = (game::PLAYER_SPAWN_X / game::BLOCK_VOXELS as f32) as usize;
    let (col_xz, lx) = (spawn_block / cb, spawn_block % cb);
    let (cc, col) = columns.iter().find(|(c, _)| c.x == col_xz && c.z == col_xz).expect("spawn column");
    assert_eq!((cc.x, cc.z), (col_xz, col_xz));
    let ground = col.ground().unwrap();
    assert!(ground.geo.iter().all(|&g| g == 64), "geo {:?}", ground.geo);
    let surface = i32::from(ground.surface[lx * cb + lx]);
    assert!(surface > 50 && surface - 60 < 38, "surface {surface} must cross both layers");
    assert_eq!(col.get(BlockPos::new(lx as i32, surface + 1, lx as i32)).id, BlockId::AIR, "surface {surface} is the top");

    let m = mats();
    let (sandstone, shale) = (m.rocks[SANDSTONE as usize], m.rocks[SHALE as usize]);
    for y in (surface - 70..=surface).rev() {
        let id = block_in(col, lx, lx, y);
        let want = if y > surface - 4 {
            assert!([m.grass, m.dirt, m.sand].contains(&id), "soil at y={y} (surface {surface}): {id:?}");
            continue;
        } else if y <= surface - 60 {
            m.bedrock
        } else if y >= 39 {
            sandstone
        } else {
            shale
        };
        assert_eq!(id, want, "y={y} (surface {surface})");
    }
}

/// The stored biome of a cell is `classify` of what `cell_climate` reads
/// there: sea, lake and plain of the synthetic world.
#[test]
fn biome_cell_is_classify_of_cell_climate() {
    let world = ForgedWorld::new(synthetic());
    let cell = |ix: usize, iz: usize| engine((ix as f64 + 0.5) * CELL_M, (iz as f64 + 0.5) * CELL_M);
    let at = |(x, z): (f64, f64)| (world.biome_cell(x, z), world.cell_climate(x, z));
    let (sea, sea_climate) = at(cell(1, 40));
    assert_eq!(sea, Biome::Ocean, "{sea_climate:?}");
    let (lake, lake_climate) = at(cell(LAKE_CELLS.0.start + 1, LAKE_CELLS.1.start + 1));
    assert!(lake_climate.is_lake);
    assert_eq!(lake, Biome::Lake);
    // 14 C, 900 mm/yr (temperate forest rainfall), drainage 50: conifer forest.
    let (plain_biome, plain_climate) = at(cell(50, 40));
    assert_eq!(plain_biome, Biome::ConiferForest, "{plain_climate:?}");
    for iz in 0..N {
        for ix in 0..N {
            let (b, c) = at(cell(ix, iz));
            assert_eq!(b, classify(&c), "cell {ix},{iz}");
        }
    }
    // Clamped outside the map.
    assert_eq!(world.biome_cell(-1.0e6, -1.0e6), world.biome_cell(cell(0, 0).0, cell(0, 0).1));
    let a = world.artifact();
    assert_eq!(a.regions.iter().map(|r| r.cells as usize).sum::<usize>(), N * N);
    assert!(a.region.iter().all(|&r| (r as usize) < a.regions.len()));
}

#[test]
fn bad_region_or_biome_values_fail_loudly() {
    let mut artifact = synthetic();
    artifact.region[5] = artifact.regions.len() as u16;
    let err = WorldArtifact::from_bytes(&artifact.to_bytes()).unwrap_err().to_string();
    assert!(err.contains("region id out of range"), "{err}");
    let mut artifact = synthetic();
    artifact.biome[5] = 200;
    let err = WorldArtifact::from_bytes(&artifact.to_bytes()).unwrap_err().to_string();
    assert!(err.contains("unknown biome"), "{err}");
}

#[test]
fn unknown_rock_name_fails_loudly() {
    let mut artifact = synthetic();
    artifact.rock_names[0] = "marble".into();
    let err = WorldArtifact::from_bytes(&artifact.to_bytes()).unwrap_err().to_string();
    assert!(err.contains("unknown rock marble"), "{err}");
}

#[test]
fn old_artifact_version_fails_loudly() {
    let mut artifact = synthetic();
    artifact.version = ARTIFACT_VERSION - 1;
    let err = WorldArtifact::from_bytes(&artifact.to_bytes()).unwrap_err().to_string();
    assert!(err.contains("re-forge"), "{err}");
}

#[test]
fn v2_file_is_refused_with_reforge_message() {
    // A v2 file: header only matters, the body is never read.
    let mut bytes = b"VXWF".to_vec();
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(b"whatever the old body was");
    let err = WorldArtifact::from_bytes(&bytes).unwrap_err();
    assert!(matches!(err, ArtifactError::OldVersion { found: 2 }), "{err:?}");
    assert!(err.to_string().contains("re-forge"), "{err}");

    let path = std::env::temp_dir().join(format!("mmo-v2-{}.world", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let loaded = ForgedWorld::load(&path);
    let _ = std::fs::remove_file(&path);
    assert!(matches!(loaded, Err(ArtifactError::OldVersion { found: 2 })));
}

/// xorshift: deterministic pseudo-random floats without a dependency.
fn rng(state: &mut u64) -> f64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 11) as f64 / (1u64 << 53) as f64
}

#[test]
fn qraster_round_trip_within_half_step() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    // Fits at the preferred step: within half of 0.0625 m.
    let low: Vec<f32> = (0..20_000).map(|_| (-900.0 + rng(&mut state) * 4000.0) as f32).collect();
    let q = QRaster::fit(&low, HEIGHT_STEP_M);
    assert_eq!(q.step, HEIGHT_STEP_M);
    assert!(low.iter().enumerate().all(|(i, &v)| (q.get(i) - v).abs() <= HEIGHT_STEP_M / 2.0 + 1e-4));
    // Peaks to 3400 m overflow 65535 steps of 0.0625 m: the step widens to
    // span / 65535 and the error bound follows it.
    let high: Vec<f32> = (0..20_000).map(|_| (-900.0 + rng(&mut state) * 4300.0) as f32).collect();
    let q = QRaster::fit(&high, HEIGHT_STEP_M);
    assert!(q.step > HEIGHT_STEP_M && q.step < 0.07, "{}", q.step);
    assert!(high.iter().enumerate().all(|(i, &v)| (q.get(i) - v).abs() <= q.step / 2.0 + 1e-4));
    // Out-of-range values clamp instead of wrapping.
    let clamped = QRaster::from_f32(&[-5000.0, 5000.0], -1000.0, 0.0625);
    assert_eq!(clamped.data, vec![0, u16::MAX]);
}

#[test]
fn row_delta_inverts() {
    let mut state = 7u64;
    let original: Vec<u16> = (0..64 * 64).map(|_| (rng(&mut state) * 65535.0) as u16).collect();
    let mut data = original.clone();
    delta_rows(&mut data, 64);
    assert_ne!(data, original);
    undelta_rows(&mut data, 64);
    assert_eq!(data, original);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn artifact_v3_round_trip_preserves_samples() {
    // Seed 4: a small world with dry land for a spawn site.
    let p = worldgen::forge::ForgeParams { size_m: 4096.0, cell_m: 64.0, seed: 4, ..Default::default() };
    let artifact = worldgen::forge::forge(&p).artifact(&p);
    let before = ForgedWorld::new(artifact.clone());
    let path = std::env::temp_dir().join(format!("mmo-v3-{}.world", std::process::id()));
    artifact.save(&path).unwrap();
    let after = ForgedWorld::load(&path);
    let _ = std::fs::remove_file(&path);
    let after = after.unwrap();
    assert_eq!(after.artifact(), &artifact);
    let mut state = 42u64;
    for _ in 0..200 {
        // Engine meters of a random point inside the artifact.
        let (x, z) = (
            rng(&mut state) * 4096.0 - artifact.spawn[0] + 8.5,
            rng(&mut state) * 4096.0 - artifact.spawn[1] + 8.5,
        );
        let (a, b) = (before.sample(x, z), after.sample(x, z));
        assert!((a.ground_y - b.ground_y).abs() <= 0.07, "{x},{z}: {} vs {}", a.ground_y, b.ground_y);
        assert_eq!(a.water_y.is_some(), b.water_y.is_some(), "{x},{z}");
        assert_eq!(after.biome_cell(x, z), classify(&after.cell_climate(x, z)), "{x},{z}");
    }
}

#[test]
fn unsorted_rock_column_fails_loudly() {
    let mut artifact = synthetic();
    artifact.column.reverse(); // ascending tops
    let err = WorldArtifact::from_bytes(&artifact.to_bytes()).unwrap_err().to_string();
    assert!(err.contains("not sorted by descending top"), "{err}");
    artifact.column = vec![(-5.0, SANDSTONE), (f32::NAN, SHALE)];
    assert!(WorldArtifact::from_bytes(&artifact.to_bytes()).is_err(), "a NaN top is unsorted");
}

#[test]
#[should_panic(expected = "empty rock column")]
fn forged_world_new_rejects_an_empty_rock_column() {
    let mut artifact = synthetic();
    artifact.column.clear();
    ForgedWorld::new(artifact);
}

#[test]
#[should_panic(expected = "not sorted by descending top")]
fn forged_world_new_rejects_an_unsorted_rock_column() {
    let mut artifact = synthetic();
    artifact.column.reverse();
    ForgedWorld::new(artifact);
}

#[test]
fn save_is_atomic_and_leaves_no_temp_file() {
    let dir = std::env::temp_dir().join(format!("mmo-forged-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("w.world");
    let artifact = synthetic();
    artifact.save(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), artifact.to_bytes());
    assert!(!worldgen::forged::save_temp_path(&path).exists(), "the temp file is renamed away");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "only the world file remains");
    // A save that cannot land (target is a directory) cleans its temp file up.
    let blocked = dir.join("blocked.world");
    std::fs::create_dir(&blocked).unwrap();
    assert!(artifact.save(&blocked).is_err());
    assert!(!worldgen::forged::save_temp_path(&blocked).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn world_file_env_values() {
    use worldgen::forged::{from_repo_root, world_file_path, PROCEDURAL_KEYWORD};
    assert_eq!(world_file_path(None), None);
    assert_eq!(world_file_path(Some("")), None);
    assert_eq!(world_file_path(Some(PROCEDURAL_KEYWORD)), None, "procedural is no world file");
    let rel = world_file_path(Some("worlds/x.world")).unwrap();
    assert_eq!(rel, from_repo_root(std::path::Path::new("worlds/x.world")));
    assert!(rel.ends_with("worlds/x.world") && rel.is_absolute(), "relative paths resolve from the repo root");
    assert_eq!(world_file_path(Some("/abs/x.world")).unwrap(), std::path::PathBuf::from("/abs/x.world"));
}

/// `install` pins the process's world once; a second choice is a bug. (No
/// other test in this binary calls `forged::active()`, which would
/// initialise it from the environment first.)
#[test]
fn install_pins_the_world_once() {
    worldgen::forged::install(None);
    assert!(worldgen::forged::active().is_none());
    let again = std::panic::catch_unwind(|| worldgen::forged::install(None));
    assert!(again.is_err(), "a second install must panic");
}

/// Height of the synthetic cone: a flat 48 m summit disc at `CONE_PEAK_M`
/// over the artifact centre, flanks falling `CONE_SLOPE` m per m to the sea.
const CONE_PEAK_M: f64 = 2500.0;
const CONE_SLOPE: f64 = 6.0;
const CONE_CAP_M: f64 = 48.0;
const CONE_CENTRE: f64 = 512.0;

fn cone_height(ax: f64, az: f64) -> f64 {
    let r = ((ax - CONE_CENTRE).powi(2) + (az - CONE_CENTRE).powi(2)).sqrt();
    (CONE_PEAK_M - (r - CONE_CAP_M).max(0.0) * CONE_SLOPE).max(-5.0)
}

/// A 1 km world holding one 2,500 m cone, spawn on its summit (engine block
/// (8, 8) of region (0, 0)). No rivers, no lakes.
fn cone() -> WorldArtifact {
    let mut artifact = synthetic();
    let height: Vec<f32> = (0..N * N)
        .map(|i| cone_height((i % N) as f64 * CELL_M + CELL_M * 0.5, (i / N) as f64 * CELL_M + CELL_M * 0.5) as f32)
        .collect();
    artifact.height = QRaster::fit(&height, HEIGHT_STEP_M);
    artifact.rivers.clear();
    artifact.lakes.clear();
    artifact.spawn = [CONE_CENTRE, CONE_CENTRE];
    artifact
}

/// Highest non-air block of block column `(lx, lz)` of `col`, scanning the
/// whole forged band.
fn top_block(col: &Column, lx: i32, lz: i32) -> Option<i32> {
    (-1024..4096).rev().find(|&y| col.get(BlockPos::new(lx, y, lz)).id != BlockId::AIR)
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forged_ground_above_block_320_is_solid() {
    let world = ForgedWorld::new(cone());
    let columns = worldgen::generate_region_forged(&world, RegionCoords::new(0, 0), mats());
    let per = game::CHUNK_BLOCKS as i32;
    let mut tops = std::collections::BTreeMap::new();
    for (cc, col) in &columns {
        for lx in 0..per {
            for lz in 0..per {
                if let Some(y) = top_block(col, lx, lz) {
                    tops.insert((cc.x as i32 * per + lx, cc.z as i32 * per + lz), y);
                }
            }
        }
    }
    // The summit column: engine block (8, 8).
    let expected = (CONE_PEAK_M + SEA_TOP_BLOCK as f64).round() as i32;
    let summit = tops[&(8, 8)];
    assert!((summit - expected).abs() <= 1, "summit top block {summit}, expected {expected} ± 1");
    // No plateau cut at the old band top: nothing in the region stops at 320,
    // and the flank inside the region keeps falling well above it.
    assert!(tops.values().all(|&y| y != 320), "a column topped out at the old band top");
    let lowest = *tops.values().min().unwrap();
    assert!(lowest > 1000 && lowest < summit - 50, "flank inside the region: lowest top {lowest}, summit {summit}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coarse_lod_steep_face_has_no_gaps() {
    let world = ForgedWorld::new(cone());
    // A region on the flank, ~200 m east of the summit: a 6:1 face.
    let rc = RegionCoords::new(3, 0);
    let key = game::lod::LodKey::for_region(16, rc);
    let cells = worldgen::coarse_tile_forged(&world, &key, mats());
    // Factor 16: one cell per block; 64×64 cell columns over a 64 m region.
    let n = game::REGION_BLOCKS as i32;
    let mut solid: std::collections::BTreeMap<(i32, i32), std::collections::BTreeSet<i32>> = Default::default();
    for cell in &cells {
        for (k, v) in cell.grid.iter().enumerate() {
            if v.block == BlockId::AIR || v.block == mats().water {
                continue;
            }
            let (lx, ly, lz) = ((k % 32) as i32, ((k / 32) % 32) as i32, (k / (32 * 32)) as i32);
            let (cx, cy, cz) = (cell.group[0] * 32 + lx, cell.group[1] * 32 + ly, cell.group[2] * 32 + lz);
            solid.entry((cx, cz)).or_default().insert(cy);
        }
    }
    let top = |cx: i32, cz: i32| *solid[&(cx, cz)].last().unwrap();
    let (mut checked, mut steep) = (0, 0);
    for cx in 1..n - 1 {
        for cz in 1..n - 1 {
            let s = top(cx, cz);
            let low = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().map(|&(dx, dz)| top(cx + dx, cz + dz)).min().unwrap();
            steep += (s - low > 4) as usize;
            for y in low.min(s)..=s {
                assert!(solid[&(cx, cz)].contains(&y), "gap at cell ({cx}, {y}, {cz}): surface {s}, lowest neighbour {low}");
            }
            checked += 1;
        }
    }
    assert!(steep > 100, "the flank must be steep enough to open gaps: {steep} of {checked} steep cells");
    assert!(top(n / 2, n / 2) > 320, "the flank stands above the old band top");
}

// ---- Biomes at walking scale (ground cover, dithered borders, ice) ----

/// Real block ids, biome blocks on.
fn biome_mats() -> worldgen::material::Materials {
    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
    worldgen::material::Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")))
}

/// Ground of the two-biome world (m above sea).
const TWO_BIOME_GROUND_M: f32 = 20.0;
/// The desert / glacier border, artifact metres (x).
const BORDER_AX: f64 = 512.0;
/// Glacier temperature (deg C): -20 gives 20 blocks of glacier ice and 4 of lake ice.
const GLACIER_T: f32 = -20.0;
/// A frozen lake in the glacier half (artifact cells), level 1 m under the ground.
const FROZEN_LAKE_CELLS: (std::ops::Range<usize>, std::ops::Range<usize>) = (42..52, 8..20);
const FROZEN_LAKE_LEVEL: f32 = 19.0;
/// East of this cell column the glacier half is sea (against the east sea edge).
const EAST_SEA_FROM: usize = 60;

/// A 1 km world: a hot sand desert west of `BORDER_AX`, a glacier east of it
/// holding a frozen lake, a frozen sea strip along the east (sea) edge.
fn two_biome_world() -> ForgedWorld {
    let mut a = synthetic();
    a.rivers.clear();
    let (mut height, mut temp, mut precip, mut drainage) = (vec![0f32; N * N], vec![0f32; N * N], vec![0f32; N * N], vec![0u8; N * N]);
    let mut lake_cells = Vec::new();
    for iz in 0..N {
        for ix in 0..N {
            let i = iz * N + ix;
            let desert = ((ix as f64 + 0.5) * CELL_M) < BORDER_AX;
            height[i] = if ix >= EAST_SEA_FROM { -6.0 } else { TWO_BIOME_GROUND_M };
            (temp[i], precip[i], drainage[i]) = if desert { (25.0, 100.0, 20) } else { (GLACIER_T, 300.0, 50) };
            if FROZEN_LAKE_CELLS.0.contains(&ix) && FROZEN_LAKE_CELLS.1.contains(&iz) {
                height[i] = 10.0;
                lake_cells.push((i as u32, 1));
            }
        }
    }
    a.height = QRaster::fit(&height, HEIGHT_STEP_M);
    a.temperature_c = QRaster::fit(&temp, worldgen::forged::TEMPERATURE_STEP_C);
    a.precip_mm = QRaster::fit(&precip, worldgen::forged::PRECIP_STEP_MM);
    a.drainage = drainage;
    a.lakes = vec![worldgen::forged::Lake {
        level: FROZEN_LAKE_LEVEL,
        area_m2: 120.0 * 256.0,
        max_depth: 9.0,
        outlet: [672.0, 128.0],
        cells: lake_cells,
    }];
    ForgedWorld::new(a.with_regions())
}

/// The cactus block of `m` (the `single_block` species' wood).
fn cactus(m: &worldgen::material::Materials) -> BlockId {
    let i = worldgen::trees::SPECIES.iter().position(|s| s.name == "cactus").unwrap();
    m.trees.species[i].log
}

/// The hot sand desert grows scattered cacti and no other tree (no leaves,
/// no `_log` of another species): `NATIVE_ONLY_BIOMES` + the 0.02 density.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn desert_grows_only_scattered_cacti() {
    let world = two_biome_world();
    let m = biome_mats();
    let cactus = cactus(&m);
    let other_tree: std::collections::BTreeSet<BlockId> = m
        .trees
        .species
        .iter()
        .flat_map(|s| [s.log, s.branch, s.leaves])
        .filter(|&b| b != cactus && b != BlockId::AIR)
        .collect();
    let (mut ids, mut regions) = (std::collections::BTreeSet::new(), 0);
    for (ax, az) in [(100.0, 300.0), (100.0, 600.0), (300.0, 300.0), (300.0, 600.0), (200.0, 450.0), (100.0, 800.0)] {
        regions += 1;
        for (_, col) in worldgen::generate_region_forged(&world, region_at(ax, az), m) {
            for (_, sub) in (col.base_sub_y()..).zip(col.sub_chunks()) {
                let game::SubChunk::Dense(chunk) = sub else { continue };
                for (_, b) in chunk.blocks.iter() {
                    assert!(!other_tree.contains(&b.id), "a {:?} in the desert", b.id);
                    if b.id == cactus {
                        assert_ne!(b.tree_id(), 0, "cactus blocks carry their tree");
                        ids.insert(b.tree_id());
                    }
                }
            }
        }
    }
    let per_region = ids.len() as f64 / regions as f64;
    println!("{} cacti in {regions} desert regions", ids.len());
    // A 64-block region has 64 lattice cells: about one cactus each.
    assert!(!ids.is_empty() && per_region < 4.0, "{per_region} cacti per region");
}

/// Region (test size, 64 blocks) holding artifact point `(ax, az)`.
fn region_at(ax: f64, az: f64) -> RegionCoords {
    let (x, z) = engine(ax, az);
    let rb = game::REGION_BLOCKS as f64;
    RegionCoords::new((x / rb).floor() as i32, (z / rb).floor() as i32)
}

/// Block at world block Y `y` of block column `(lx, lz)` (band or recipe).
fn block_at_y(col: &Column, lx: usize, lz: usize, y: i32) -> BlockId {
    block_in(col, lx, lz, y)
}

#[test]
fn two_biome_world_classifies_as_desert_and_glacier() {
    let world = two_biome_world();
    let (dx, dz) = engine(200.0, 600.0);
    let (gx, gz) = engine(800.0, 600.0);
    assert_eq!(world.biome_cell(dx, dz), Biome::SandDesert, "{:?}", world.cell_climate(dx, dz));
    assert_eq!(world.biome_cell(gx, gz), Biome::Glacier, "{:?}", world.cell_climate(gx, gz));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn desert_region_surface_is_sand() {
    let world = two_biome_world();
    let m = biome_mats();
    let columns = worldgen::generate_region_forged(&world, region_at(200.0, 600.0), m);
    let per = game::CHUNK_BLOCKS;
    let mut checked = 0;
    for (_, col) in &columns {
        let ground = col.ground().unwrap();
        for lx in 0..per {
            for lz in 0..per {
                let s = i32::from(ground.surface[lx * per + lz]);
                for d in 0..6 {
                    assert_eq!(block_at_y(col, lx, lz, s - d), m.sand, "desert sand {d} below the surface {s}");
                }
                // Air, or a cactus (the desert's only tree).
                let above = block_at_y(col, lx, lz, s + 1);
                assert!(above == BlockId::AIR || above == cactus(&m), "{above:?} on the sand");
                checked += 1;
            }
        }
    }
    assert_eq!(checked, (game::REGION_BLOCKS * game::REGION_BLOCKS) as usize);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn glacier_region_has_ice_over_rock() {
    let world = two_biome_world();
    let m = biome_mats();
    let columns = worldgen::generate_region_forged(&world, region_at(800.0, 600.0), m);
    let per = game::CHUNK_BLOCKS;
    let thickness = (-GLACIER_T).round() as i32;
    let ground_top = SEA_TOP_BLOCK + TWO_BIOME_GROUND_M as i32;
    for (_, col) in &columns {
        let ground = col.ground().unwrap();
        for lx in 0..per {
            for lz in 0..per {
                // The recipe's surface is the visible top: the ice.
                let top = i32::from(ground.surface[lx * per + lz]);
                assert_eq!(block_at_y(col, lx, lz, top + 1), BlockId::AIR, "surface {top} is the top");
                let mut ice = 0;
                while block_at_y(col, lx, lz, top - ice) == m.biome.glacier_ice {
                    ice += 1;
                }
                assert_eq!(ice, thickness, "glacier ice at ({lx}, {lz}), top {top}");
                // Straight onto rock (no soil, no snow), and on the plain's ground.
                let rock = block_at_y(col, lx, lz, top - ice);
                assert!(m.rocks.contains(&rock), "rock under the ice: {rock:?}");
                assert!((top - ice - ground_top).abs() <= 2, "ground {} vs plain {ground_top}", top - ice);
                // Digging down through the ice reaches rock long before bedrock.
                for y in top - ice - 10..top - ice {
                    assert!(m.rocks.contains(&block_at_y(col, lx, lz, y)), "rock at y {y} under a glacier");
                }
            }
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn frozen_lake_has_ice_on_top() {
    let world = two_biome_world();
    let m = biome_mats();
    let rc = region_at(752.0, 224.0);
    let columns = worldgen::generate_region_forged(&world, rc, m);
    let level_top = SEA_TOP_BLOCK + FROZEN_LAKE_LEVEL as i32; // topmost water block
    let ice_blocks = 4; // clamp(round(20 / 4), 1, 4)
    let per = game::CHUNK_BLOCKS;
    let mut frozen = 0;
    for (_, col) in &columns {
        for lx in 0..per {
            for lz in 0..per {
                if block_at_y(col, lx, lz, level_top) != m.biome.ice {
                    continue;
                }
                frozen += 1;
                assert_eq!(block_at_y(col, lx, lz, level_top + 1), BlockId::AIR, "ice is the lake's top");
                for y in level_top - ice_blocks + 1..=level_top {
                    assert_eq!(block_at_y(col, lx, lz, y), m.biome.ice, "ice at y {y}");
                }
                assert_eq!(block_at_y(col, lx, lz, level_top - ice_blocks), m.water, "water under the ice");
            }
        }
    }
    assert_eq!(frozen, (game::REGION_BLOCKS * game::REGION_BLOCKS) as usize, "the whole region is frozen lake");
}

#[test]
fn biome_border_is_dithered_not_straight() {
    let world = two_biome_world();
    let dither = worldgen::forged::BIOME_DITHER_M;
    assert_eq!(dither, 24.0);
    let (mut west_other, mut east_other, mut west, mut east) = (0, 0, 0, 0);
    let mut crossings = std::collections::BTreeSet::new();
    for k in 0..600 {
        let az = 200.0 + k as f64;
        // Where the border falls on this row (first glacier column from the west).
        let mut first_glacier = None;
        for d in 0..2 * dither as i32 {
            let ax = BORDER_AX - dither + d as f64 + 0.5;
            let (x, z) = engine(ax, az);
            let b = world.biome_at(x, z);
            assert!(matches!(b, Biome::SandDesert | Biome::Glacier), "{b:?}");
            if ax < BORDER_AX {
                west += 1;
                west_other += (b == Biome::Glacier) as usize;
            } else {
                east += 1;
                east_other += (b == Biome::SandDesert) as usize;
            }
            if b == Biome::Glacier && first_glacier.is_none() {
                first_glacier = Some(d);
            }
        }
        crossings.insert(first_glacier);
    }
    let (w, e) = (west_other as f64 / west as f64, east_other as f64 / east as f64);
    println!("other biome within {dither} m: west {w:.3}, east {e:.3}; {} border positions", crossings.len());
    assert!(w >= 0.2 && e >= 0.2, "dithered on both sides: west {w:.3}, east {e:.3}");
    assert!(crossings.len() > 10, "the border wanders: {crossings:?}");
    // Deterministic and thread-independent: a pure function of the point.
    let (x, z) = engine(BORDER_AX + 3.5, 333.5);
    assert_eq!(world.biome_at(x, z), world.biome_at(x, z));
    // Far from any border the nearest cell's biome stands.
    let (dx, dz) = engine(200.0, 600.0);
    assert_eq!(world.biome_at(dx, dz), Biome::SandDesert);
}

#[test]
fn snow_line_follows_temperature() {
    let world = two_biome_world();
    // Warm desert: far above the ground; glacier at -20 C: below the ground.
    let (dx, dz) = engine(200.0, 600.0);
    let (gx, gz) = engine(800.0, 600.0);
    let lapse = worldgen::forge::Knobs::default().lapse_c_per_km / 1000.0;
    let want_desert = TWO_BIOME_GROUND_M + (25.0 - worldgen::forged::SNOW_LINE_C) / lapse as f32;
    assert!((world.snow_line_m(dx, dz) - want_desert).abs() < 1.0, "{} vs {want_desert}", world.snow_line_m(dx, dz));
    assert!(world.snow_line_m(gx, gz) < TWO_BIOME_GROUND_M);
}

/// A frozen sea reaching the map's sea edge freezes past it too: the same
/// flat ice top on both sides, in regions and in coarse LOD.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn frozen_sea_continues_past_the_sea_edge_without_a_step() {
    let world = two_biome_world();
    let m = biome_mats();
    let rc = region_at(N as f64 * CELL_M, 600.0); // straddles the east (sea) edge
    let tops = tops(&worldgen::generate_region_forged(&world, rc, m));
    // `tops` reports (top, is water): every column here is sea with ice on top.
    assert!(tops.values().all(|&(top, water)| top == SEA_TOP_BLOCK && !water), "flat ice at the sea top");
    let key = game::lod::LodKey::for_region(16, rc);
    let mut per_column: std::collections::BTreeMap<(i32, i32), (i32, BlockId)> = Default::default();
    for cell in worldgen::coarse_tile_forged(&world, &key, m) {
        for (k, v) in cell.grid.iter().enumerate() {
            if v.block == BlockId::AIR {
                continue;
            }
            let (lx, ly, lz) = ((k % 32) as i32, ((k / 32) % 32) as i32, (k / (32 * 32)) as i32);
            let col = (cell.group[0] * 32 + lx, cell.group[2] * 32 + lz);
            let y = cell.group[1] * 32 + ly;
            let e = per_column.entry(col).or_insert((y, v.block));
            if y > e.0 {
                *e = (y, v.block);
            }
        }
    }
    assert_eq!(per_column.len(), (game::REGION_BLOCKS * game::REGION_BLOCKS) as usize);
    assert!(per_column.values().all(|&(y, b)| y == SEA_TOP_BLOCK && b == m.biome.ice), "LOD: flat sea ice across the edge");
}

/// A flat 20 m world of one climate, no water but the sea past its edges.
fn one_climate_world(temperature_c: f32, precip_mm: f32, drainage: u8) -> ForgedWorld {
    let mut a = synthetic();
    a.rivers.clear();
    a.lakes.clear();
    a.height = QRaster::fit(&vec![TWO_BIOME_GROUND_M; N * N], HEIGHT_STEP_M);
    a.temperature_c = QRaster::fit(&vec![temperature_c; N * N], worldgen::forged::TEMPERATURE_STEP_C);
    a.precip_mm = QRaster::fit(&vec![precip_mm; N * N], worldgen::forged::PRECIP_STEP_MM);
    a.drainage = vec![drainage; N * N];
    ForgedWorld::new(a.with_regions())
}

/// Tree blocks (any block carrying a tree id) in the spawn region.
fn tree_blocks(world: &ForgedWorld) -> usize {
    let columns = worldgen::generate_region_forged(world, RegionCoords::new(0, 0), biome_mats());
    columns
        .iter()
        .flat_map(|(_, col)| col.sub_chunks().iter())
        .filter_map(|sub| match sub {
            game::SubChunk::Dense(c) => Some(c.blocks.iter().filter(|(_, b)| b.tree_id() != 0).count()),
            game::SubChunk::Air => None,
        })
        .sum()
}

/// Trees grow on soil, not on open mud flats: a marsh stays treeless
/// (swamps and mangrove swamps, DF's forested wetlands, keep their trees)
/// where the same rain on drained ground (shrubland) grows trees.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trees_keep_off_mud_and_snow() {
    let (x, z) = engine(SPAWN[0], SPAWN[1]);
    let shrubland = one_climate_world(12.0, 700.0, 60);
    assert_eq!(shrubland.biome_cell(x, z), Biome::Shrubland);
    let marsh = one_climate_world(12.0, 700.0, 20);
    assert_eq!(marsh.biome_cell(x, z), Biome::Marsh);
    let (with, without) = (tree_blocks(&shrubland), tree_blocks(&marsh));
    println!("tree blocks: shrubland {with}, marsh {without}");
    assert!(with > 0, "the shrubland control grows trees");
    assert_eq!(without, 0, "no trees on marsh mud");
    // Snow over alpine-cold grassland: no trees either; a snowy boreal
    // forest floor keeps its trees.
    let snowy_grass = one_climate_world(-3.0, 350.0, 60);
    assert_eq!(snowy_grass.biome_cell(x, z), Biome::Grassland);
    assert!(snowy_grass.snow_line_m(x, z) < TWO_BIOME_GROUND_M);
    assert_eq!(tree_blocks(&snowy_grass), 0, "no trees on snow-covered grassland");
}

/// The glacier's dithered edge (region (0, 0) straddles the desert / glacier
/// border): ice stands on ground (nothing floats), the band's deep solids are
/// the recipe's, and no open block faces implied ground — ice cliffs
/// included.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn glacier_edge_is_grounded_and_closed() {
    let world = two_biome_world();
    let m = biome_mats();
    let rc = region_at(BORDER_AX, 600.0);
    assert_eq!(rc, RegionCoords::new(0, 0));
    let cols = worldgen::generate_region_forged(&world, rc, m);
    let cb = game::CHUNK_BLOCKS as i32;
    let by_coords: std::collections::BTreeMap<(i32, i32), &Column> =
        cols.iter().map(|(c, col)| ((c.x as i32, c.z as i32), col)).collect();
    let block = |x: i32, y: i32, z: i32| -> Option<(BlockId, bool)> {
        let col = by_coords.get(&(x.div_euclid(cb), z.div_euclid(cb)))?;
        let (lx, lz) = (x.rem_euclid(cb) as usize, z.rem_euclid(cb) as usize);
        let g = col.ground().unwrap();
        Some(if g.is_implied(y.div_euclid(cb)) { (g.block_at(lx, lz, y).id, true) } else { (block_in(col, lx, lz, y), false) })
    };
    let (mut ice_columns, mut bare, mut glacier_tops) = (0, 0, std::collections::BTreeSet::new());
    for (c, col) in &cols {
        let g = col.ground().unwrap();
        for lx in 0..cb {
            for lz in 0..cb {
                let (x, z) = (c.x as i32 * cb + lx, c.z as i32 * cb + lz);
                let top = i32::from(g.surface[(lx * cb + lz) as usize]);
                assert_eq!(block(x, top + 1, z).unwrap().0, BlockId::AIR, "({x}, {z}) top {top}");
                let mut y = top;
                while block(x, y, z).unwrap().0 == m.biome.glacier_ice {
                    y -= 1;
                }
                if y < top {
                    ice_columns += 1;
                    glacier_tops.insert(top - y);
                    let under = block(x, y, z).unwrap().0;
                    assert!(m.rocks.contains(&under), "glacier ice at ({x}, {z}) stands on {under:?}");
                } else {
                    bare += 1;
                }
                // Band solids under the soil agree with the recipe.
                let ground = y;
                for y in (ground - 30)..(ground - 8) {
                    let (id, implied) = block(x, y, z).unwrap();
                    if !implied {
                        assert_eq!(id, g.block_at(lx as usize, lz as usize, y).id, "band vs recipe at ({x}, {y}, {z})");
                    }
                }
                // Open blocks (air above the top, down to the lowest
                // neighbour's top) never face an implied chunk.
                for y in top + 1..top + 45 {
                    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        if let Some((_, implied)) = block(x + dx, y, z + dz) {
                            assert!(!implied, "air at ({x}, {y}, {z}) faces implied ground");
                        }
                    }
                }
            }
        }
    }
    println!("glacier edge: {ice_columns} ice columns ({} thicknesses {:?}), {bare} desert", glacier_tops.len(), glacier_tops);
    assert!(ice_columns > 500 && bare > 500, "the region holds both sides of the border");
    assert!(glacier_tops.len() > 5, "the glacier thins towards its edge");
}

/// Every submerged column of the coast region at artifact x `ax` (sea floor
/// under water or ice): its top ground block, the block over it.
fn sea_floors(world: &ForgedWorld, ax: f64) -> Vec<(BlockId, BlockId)> {
    let m = biome_mats();
    let cols = worldgen::generate_region_forged(world, region_at(ax, 600.0), m);
    let per = game::CHUNK_BLOCKS;
    let mut out = Vec::new();
    for (_, col) in &cols {
        let g = col.ground().unwrap();
        for lx in 0..per {
            for lz in 0..per {
                let s = i32::from(g.surface[lx * per + lz]);
                let above = block_at_y(col, lx, lz, s + 1);
                if above == m.water || above == m.biome.ice {
                    out.push((block_at_y(col, lx, lz, s), above));
                }
            }
        }
    }
    out
}

/// Cold sea floors keep sea-floor cover — sand, under water and ice — whatever
/// biome the dither gives them: no snow (the snow line lies far under a cold
/// sea), no glacier bare rock, no tundra frozen dirt or snow patches.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cold_sea_floors_are_sand_under_the_ice() {
    let m = biome_mats();
    // The glacier half meets the frozen sea strip at artifact x 960.
    let glacier_coast = sea_floors(&two_biome_world(), EAST_SEA_FROM as f64 * CELL_M);
    // A tundra coast: the same sea strip next to -8 C tundra.
    let mut a = one_climate_world(-8.0, 300.0, 50).artifact().clone();
    let mut height = a.height.to_f32();
    for (i, h) in height.iter_mut().enumerate() {
        if i % N >= EAST_SEA_FROM {
            *h = -6.0;
        }
    }
    a.height = QRaster::fit(&height, HEIGHT_STEP_M);
    let tundra = ForgedWorld::new(a.with_regions());
    let (tx, tz) = engine(800.0, 600.0);
    assert_eq!(tundra.biome_cell(tx, tz), Biome::Tundra);
    let tundra_coast = sea_floors(&tundra, EAST_SEA_FROM as f64 * CELL_M);
    for (name, floors) in [("glacier", glacier_coast), ("tundra", tundra_coast)] {
        assert!(floors.len() > 500, "{name}: the region holds sea ({} columns)", floors.len());
        assert!(floors.iter().any(|f| f.1 == m.biome.ice), "{name}: the sea is frozen");
        let bad: Vec<_> = floors.iter().filter(|f| f.0 != m.sand).collect();
        assert!(bad.is_empty(), "{name}: {} sea floors not sand, e.g. {:?}", bad.len(), bad.first());
    }
}
