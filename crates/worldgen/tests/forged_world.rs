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
