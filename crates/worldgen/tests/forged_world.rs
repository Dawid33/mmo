//! Forged worlds (`worldgen::forged`): the world-file format, and what the
//! stud generator builds from one — carved river channels, water above sea
//! level that its banks actually hold, lakes at one flat level, biome ground
//! cover, glaciers, trees — on a small synthetic world whose right answers
//! are known.

use std::collections::{BTreeMap, BTreeSet};

use game::{classify, stud_material, Biome, ColumnCoords, RegionCoords, StudColumnGround, StudRegionSeed, STUD_CHUNK_STUDS};
use worldgen::stud_gen::{generate_stud_region_forged, stud_lod_tile_forged, PLATES_PER_M, STUDS_PER_M, STUD_BEDROCK_PLATE};
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

// ---- What the stud generator builds from a forged world ----

/// Stud columns along a chunk column's edge.
const CS: usize = STUD_CHUNK_STUDS;
/// The sea's surface, plates (one past the top water plate).
const SEA_PLATE: i32 = (SEA_TOP_BLOCK + 1) * PLATES_PER_M;

/// Region `rc` of `world`: ground recipes and tree bricks.
fn region(world: &ForgedWorld, rc: RegionCoords) -> StudRegionSeed {
    generate_stud_region_forged(world, rc)
}

/// Every stud column of a region's recipes: `(recipe, stud x, stud z)` (chunk-local).
fn stud_columns(ground: &[(ColumnCoords, StudColumnGround)]) -> impl Iterator<Item = (&StudColumnGround, usize, usize)> {
    ground.iter().flat_map(|(_, g)| (0..CS).flat_map(move |sz| (0..CS).map(move |sx| (g, sx, sz))))
}

/// Name of the material the recipe puts at `plate` of a stud column (it must be solid).
fn name(g: &StudColumnGround, sx: usize, plate: i32, sz: usize) -> &'static str {
    let id = g.material_at(sx, plate, sz).unwrap_or_else(|| panic!("air at plate {plate} (surface {})", g.surface(sx, sz)));
    stud_material(id).unwrap_or_else(|| panic!("material {id:#x} is not a stud material")).name
}

/// The forge's rock names (what a stratum can be).
fn is_rock(name: &str) -> bool {
    worldgen::forge::geology::ROCKS.iter().any(|r| r.name == name)
}

/// (top plate, is water) for every stud column of a region, keyed by region-local stud
/// `(x, z)`: the water's top where water stands over the ground, else the ground's.
fn tops(ground: &[(ColumnCoords, StudColumnGround)]) -> BTreeMap<(i32, i32), (i32, bool)> {
    let mut out = BTreeMap::new();
    for (cc, g) in ground {
        for sx in 0..CS {
            for sz in 0..CS {
                let top = g.water_top(sx, sz).map_or((g.surface(sx, sz), false), |w| (w, true));
                out.insert(((cc.x * CS + sx) as i32, (cc.z * CS + sz) as i32), top);
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
    let rc = region_at(400.0, RIVER_Z);
    let a = region(&world, rc);
    assert!(a == region(&world, rc), "deterministic");

    let tops = tops(&a.ground);
    let (mut fresh, mut leaks) = (0, 0);
    for (&(sx, sz), &(top, is_water)) in &tops {
        if !is_water || top <= SEA_PLATE {
            continue;
        }
        fresh += 1;
        for (ox, oz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            // A neighbour holds this water if it is water no more than a step
            // lower (rivers descend in 1 m steps), or ground at least as high.
            if let Some(&(ntop, nwater)) = tops.get(&(sx + ox, sz + oz)) {
                let holds = if nwater { ntop >= top - PLATES_PER_M } else { ntop >= top };
                leaks += !holds as usize;
            }
        }
    }
    println!("river region {rc:?}: {fresh} fresh-water stud columns, {leaks} open sides");
    assert!(fresh > 1200, "an 8 m river crosses a 64 m region: {fresh} stud columns");
    assert_eq!(leaks, 0, "water with an open side");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lake_shows_in_regions_and_in_far_terrain() {
    let world = ForgedWorld::new(synthetic());
    let rc = region_at(33.0 * CELL_M, 13.0 * CELL_M); // middle of the lake
    let level = (SEA_LEVEL + (LAKE_LEVEL as f64 + 0.5).floor()) as i32 * PLATES_PER_M;

    let tops = tops(&region(&world, rc).ground);
    let lake_columns = tops.values().filter(|&&(top, water)| water && top == level).count();
    assert!(lake_columns > 4000, "{lake_columns} lake stud columns at plate {level}");
    assert!(
        tops.values().all(|&(top, water)| !water || top == level || top <= SEA_PLATE),
        "every fresh-water column of the lake region stands at the one level"
    );

    // Far terrain, factor 16 (1 m cells): the lake's surface at its level over its bed.
    let key = game::lod::LodKey::for_region(16, rc);
    let samples = stud_lod_tile_forged(&world, &key).decode().expect("a generated tile decodes");
    let lod_water = samples.iter().filter(|s| s.has_water() && i32::from(s.water) == level && i32::from(s.top) < level).count();
    assert!(lod_water > 1000, "{lod_water} lake samples at the lake level in the factor-16 tile");
}

#[test]
fn shaft_crosses_soil_then_layers_then_bedrock() {
    // No uplift or bedding: geo is the sea-level block 64, so s = y - 64 and
    // sandstone holds s >= -25 (y >= 39 m), shale below.
    let world = ForgedWorld::new(synthetic());
    let seed = region(&world, RegionCoords::new(0, 0));
    // The spawn column (block (8, 8)): on the plain, well away from the river and the lake.
    let spawn_stud = (game::PLAYER_SPAWN_X / game::STUD_UNITS as f32) as usize;
    let (col, s) = (spawn_stud / CS, spawn_stud % CS);
    let g = &seed.ground.iter().find(|(c, _)| (c.x, c.z) == (col, col)).expect("spawn column").1;
    let surface = g.surface(s, s);
    let boundary = 39 * PLATES_PER_M;
    assert!(surface > boundary + 60 && g.water_top(s, s).is_none(), "a dry surface {surface} over both layers");
    assert_eq!(g.material_at(s, surface, s), None, "plate {surface} is over the top");

    // Cover and soil: a brick course of cover over three blocks of soil.
    let soil_plates = 4 * PLATES_PER_M;
    for plate in (boundary - 200..surface).rev() {
        let got = name(g, s, plate, s);
        if plate >= surface - soil_plates {
            assert!(["grass", "dirt", "sand", "sandstone"].contains(&got), "soil at plate {plate} (surface {surface}): {got}");
        } else if plate >= boundary + PLATES_PER_M {
            assert_eq!(got, "sandstone", "plate {plate} (surface {surface})");
        } else if plate < boundary - PLATES_PER_M {
            assert_eq!(got, "shale", "plate {plate} (surface {surface})");
        }
    }
    assert!(["grass", "sand"].contains(&name(g, s, surface - 1, s)), "the cover is on top");
    assert_eq!(name(g, s, STUD_BEDROCK_PLATE, s), "shale", "rock down to the bedrock plate");
    assert_eq!(name(g, s, STUD_BEDROCK_PLATE - 1, s), "bedrock");
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
    let a = world.artifact().expect("whole world");
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
    assert_eq!(after.artifact(), Some(&artifact));
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

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forged_ground_above_block_320_is_solid() {
    let world = ForgedWorld::new(cone());
    let seed = region(&world, RegionCoords::new(0, 0));
    let tops = tops(&seed.ground);
    assert!(tops.values().all(|&(_, water)| !water), "no water on the cone");
    // The summit column: engine block (8, 8).
    let expected = ((CONE_PEAK_M + SEA_LEVEL) * PLATES_PER_M as f64).round() as i32;
    let summit = tops[&(8 * STUDS_PER_M, 8 * STUDS_PER_M)].0;
    assert!((summit - expected).abs() <= PLATES_PER_M, "summit surface plate {summit}, expected {expected} ± a block");
    // No plateau cut at the old procedural band top (block 320): nothing in the
    // region stops there, and the flank inside the region keeps falling well above it.
    let band_top = 321 * PLATES_PER_M;
    assert!(tops.values().all(|&(top, _)| top != band_top), "a column topped out at the procedural band top");
    let lowest = tops.values().map(|t| t.0).min().unwrap();
    assert!(
        lowest > 1000 * PLATES_PER_M && lowest < summit - 50 * PLATES_PER_M,
        "flank inside the region: lowest surface {lowest}, summit {summit}"
    );
    // Solid all the way: rock under the summit's soil, far above the band top.
    let g = &seed.ground.iter().find(|(c, _)| (c.x, c.z) == (0, 0)).unwrap().1;
    let s = (8 * STUDS_PER_M) as usize;
    for plate in [summit - 1, summit - 200, 2000 * PLATES_PER_M, band_top, band_top - 1] {
        assert!(g.material_at(s, plate, s).is_some(), "air at plate {plate} under the summit");
    }
    assert!(is_rock(name(g, s, 2000 * PLATES_PER_M, s)));
}

// ---- Biomes at walking scale (ground cover, dithered borders, glaciers) ----

/// Ground of the two-biome world (m above sea).
const TWO_BIOME_GROUND_M: f32 = 20.0;
/// The desert / glacier border, artifact metres (x).
const BORDER_AX: f64 = 512.0;
/// Glacier temperature (deg C): -20 gives 20 m of glacier ice.
const GLACIER_T: f32 = -20.0;
/// A lake in the glacier half (artifact cells), level 1 m under the ground.
const FROZEN_LAKE_CELLS: (std::ops::Range<usize>, std::ops::Range<usize>) = (42..52, 8..20);
const FROZEN_LAKE_LEVEL: f32 = 19.0;
/// East of this cell column the glacier half is sea (against the east sea edge).
const EAST_SEA_FROM: usize = 60;

/// A 1 km world: a hot sand desert west of `BORDER_AX`, a glacier east of it
/// holding a lake, a sea strip along the east (sea) edge.
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

/// Tree ids (`StudBrick::data`) of a region's feature bricks, and how many are leaves.
fn tree_bricks(seed: &StudRegionSeed) -> (BTreeSet<u16>, usize) {
    let bricks = || seed.features.iter().flat_map(|(_, b)| b.iter());
    assert!(bricks().all(|b| b.data != 0 && [game::MAT_WOOD, game::MAT_LEAVES].contains(&b.material)), "feature bricks are trees");
    (bricks().map(|b| b.data).collect(), bricks().filter(|b| b.material == game::MAT_LEAVES).count())
}

/// The hot sand desert grows scattered cacti and no other tree (a cactus is
/// wood only — no leaves): `NATIVE_ONLY_BIOMES` + the 0.02 density.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn desert_grows_only_scattered_cacti() {
    let world = two_biome_world();
    let (mut cacti, mut regions) = (0, 0);
    for (ax, az) in [(100.0, 300.0), (100.0, 600.0), (300.0, 300.0), (300.0, 600.0), (200.0, 450.0), (100.0, 800.0)] {
        regions += 1;
        let (ids, leaves) = tree_bricks(&region(&world, region_at(ax, az)));
        assert_eq!(leaves, 0, "a leafy tree in the desert at ({ax}, {az})");
        cacti += ids.len();
    }
    let per_region = cacti as f64 / regions as f64;
    println!("{cacti} cacti in {regions} desert regions");
    // A 64-block region has 64 lattice cells: about one cactus each.
    assert!(cacti > 0 && per_region < 4.0, "{per_region} cacti per region");
}

/// Region (test size, 64 blocks) holding artifact point `(ax, az)`.
fn region_at(ax: f64, az: f64) -> RegionCoords {
    let (x, z) = engine(ax, az);
    let rb = game::REGION_BLOCKS as f64;
    RegionCoords::new((x / rb).floor() as i32, (z / rb).floor() as i32)
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
    let seed = region(&world, region_at(200.0, 600.0));
    let mut checked = 0;
    for (g, sx, sz) in stud_columns(&seed.ground) {
        let s = g.surface(sx, sz);
        assert_eq!(g.water_top(sx, sz), None);
        // Six metres of sand under the surface.
        for d in 1..=6 * PLATES_PER_M {
            assert_eq!(name(g, sx, s - d, sz), "sand", "desert sand {d} plates below the surface {s}");
        }
        checked += 1;
    }
    assert_eq!(checked, (game::REGION_BLOCKS * game::REGION_BLOCKS) as usize * (STUDS_PER_M * STUDS_PER_M) as usize);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn glacier_region_has_ice_over_rock() {
    let world = two_biome_world();
    let seed = region(&world, region_at(800.0, 600.0));
    let thickness = (-GLACIER_T).round() as i32 * PLATES_PER_M;
    let ground_top = (SEA_TOP_BLOCK + 1 + TWO_BIOME_GROUND_M as i32) * PLATES_PER_M;
    assert!(seed.features.is_empty(), "nothing grows on a glacier");
    for (g, sx, sz) in stud_columns(&seed.ground) {
        // The recipe's surface is the visible top: the ice.
        let top = g.surface(sx, sz);
        let mut ice = 0;
        while name(g, sx, top - 1 - ice, sz) == "glacier_ice" {
            ice += 1;
        }
        assert_eq!(ice, thickness, "glacier ice at ({sx}, {sz}), surface {top}");
        // Straight onto rock (no soil, no snow), and on the plain's ground.
        let ground = top - ice;
        assert!((ground - ground_top).abs() <= 2 * PLATES_PER_M, "ground {ground} vs plain {ground_top}");
        // Digging down through the ice reaches rock long before bedrock.
        for plate in ground - 10 * PLATES_PER_M..ground {
            assert!(is_rock(name(g, sx, plate, sz)), "{} at plate {plate} under a glacier", name(g, sx, plate, sz));
        }
    }
}

/// A lake in the glacier is the recipe's water at the lake's one level. (Frozen water
/// tops — ice on cold lakes and seas — were blocks of the voxel generator; stud recipes
/// hold the water only.)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cold_lake_is_recipe_water_at_its_level() {
    let world = two_biome_world();
    let seed = region(&world, region_at(752.0, 224.0));
    let level = (SEA_TOP_BLOCK + 1 + FROZEN_LAKE_LEVEL as i32) * PLATES_PER_M;
    let mut wet = 0;
    for (g, sx, sz) in stud_columns(&seed.ground) {
        assert_eq!(g.water_top(sx, sz), Some(level), "the whole region is lake");
        assert!(g.surface(sx, sz) < level);
        assert_eq!(name(g, sx, g.surface(sx, sz) - 1, sz), "sand", "the lake bed");
        wet += 1;
    }
    assert_eq!(wet, (game::REGION_BLOCKS * game::REGION_BLOCKS) as usize * (STUDS_PER_M * STUDS_PER_M) as usize);
}

#[test]
fn biome_border_is_dithered_not_straight() {
    let world = two_biome_world();
    let dither = worldgen::forged::BIOME_DITHER_M;
    assert_eq!(dither, 24.0);
    let (mut west_other, mut east_other, mut west, mut east) = (0, 0, 0, 0);
    let mut crossings = BTreeSet::new();
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

/// A sea reaching the map's sea edge runs on past it: the same flat water
/// surface on both sides, in regions and in far terrain.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn sea_continues_past_the_sea_edge_without_a_step() {
    let world = two_biome_world();
    let rc = region_at(N as f64 * CELL_M, 600.0); // straddles the east (sea) edge
    let tops = tops(&region(&world, rc).ground);
    assert!(tops.values().all(|&(top, water)| top == SEA_PLATE && water), "flat sea at the sea surface");
    let key = game::lod::LodKey::for_region(16, rc);
    let samples = stud_lod_tile_forged(&world, &key).decode().expect("a generated tile decodes");
    assert!(samples.iter().all(|s| s.has_water() && i32::from(s.water) == SEA_PLATE), "far terrain: flat sea across the edge");
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

/// Trees (distinct tree ids of the feature bricks) in the spawn region.
fn trees(world: &ForgedWorld) -> usize {
    tree_bricks(&region(world, RegionCoords::new(0, 0))).0.len()
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
    let (with, without) = (trees(&shrubland), trees(&marsh));
    println!("trees: shrubland {with}, marsh {without}");
    assert!(with > 0, "the shrubland control grows trees");
    assert_eq!(without, 0, "no trees on marsh mud");
    // Snow over alpine-cold grassland: no trees either; a snowy boreal
    // forest floor keeps its trees.
    let snowy_grass = one_climate_world(-3.0, 350.0, 60);
    assert_eq!(snowy_grass.biome_cell(x, z), Biome::Grassland);
    assert!(snowy_grass.snow_line_m(x, z) < TWO_BIOME_GROUND_M);
    assert_eq!(trees(&snowy_grass), 0, "no trees on snow-covered grassland");
}

/// The glacier's dithered edge (region (0, 0) straddles the desert / glacier
/// border): ice stands on rock and thins towards its edge; the columns the
/// dither gives the desert are sand, under snow where the glacier's cold reaches.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn glacier_edge_is_grounded() {
    let world = two_biome_world();
    let rc = region_at(BORDER_AX, 600.0);
    assert_eq!(rc, RegionCoords::new(0, 0));
    let seed = region(&world, rc);
    let (mut ice_columns, mut bare, mut sand, mut thicknesses) = (0, 0, 0, BTreeSet::new());
    for (g, sx, sz) in stud_columns(&seed.ground) {
        let top = g.surface(sx, sz);
        let mut ice = 0;
        while name(g, sx, top - 1 - ice, sz) == "glacier_ice" {
            ice += 1;
        }
        if ice > 0 {
            ice_columns += 1;
            thicknesses.insert(ice);
            let under = name(g, sx, top - 1 - ice, sz);
            assert!(is_rock(under), "glacier ice at ({sx}, {sz}) stands on {under}");
        } else {
            bare += 1;
            let cover = name(g, sx, top - 1, sz);
            assert!(["sand", "snow"].contains(&cover), "{cover} on the desert side");
            sand += (cover == "sand") as usize;
        }
    }
    println!("glacier edge: {ice_columns} ice columns ({} thicknesses {thicknesses:?}), {bare} desert", thicknesses.len());
    assert!(ice_columns > 2000 && bare > 2000 && sand > 1000, "the region holds both sides of the border ({sand} sand)");
    assert!(thicknesses.len() > 5, "the glacier thins towards its edge");
}

/// Water (plates) over a floor from which the cover rule calls the column
/// submerged: covers are decided on the 1 m block surface (a block at least
/// half full under the water's top block is dry ground), so a floor under one
/// or two plates of water still carries its land cover.
const SUBMERGED_PLATES: i32 = 3;

/// Top ground material of every submerged stud column (`SUBMERGED_PLATES` of
/// water or more) of the coast region at artifact x `ax`.
fn sea_floors(world: &ForgedWorld, ax: f64) -> Vec<&'static str> {
    let seed = region(world, region_at(ax, 600.0));
    stud_columns(&seed.ground)
        .filter(|(g, sx, sz)| g.water_top(*sx, *sz).is_some_and(|w| w - g.surface(*sx, *sz) >= SUBMERGED_PLATES))
        .map(|(g, sx, sz)| name(g, sx, g.surface(sx, sz) - 1, sz))
        .collect()
}

/// Cold sea floors keep sea-floor cover — sand — whatever biome the dither
/// gives them: no snow (the snow line lies far under a cold sea), no glacier
/// bare rock, no tundra frozen dirt or snow patches.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cold_sea_floors_are_sand() {
    // The glacier half meets the sea strip at artifact x 960.
    let glacier_coast = sea_floors(&two_biome_world(), EAST_SEA_FROM as f64 * CELL_M);
    // A tundra coast: the same sea strip next to -8 C tundra.
    let mut a = one_climate_world(-8.0, 300.0, 50).artifact().expect("whole world").clone();
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
        assert!(floors.len() > 2000, "{name}: the region holds sea ({} stud columns)", floors.len());
        let bad: Vec<_> = floors.iter().filter(|f| **f != "sand").collect();
        assert!(bad.is_empty(), "{name}: {} sea floors not sand, e.g. {:?}", bad.len(), bad.first());
    }
}

// ---- Sampling guard (forged-world-in-db plan, Task 1) ----

/// crc32 of every point sampler output [`forged_sampling_is_bit_exact`] hashes. Any
/// change to it is a change in what a forged world generates.
///
/// Recorded 2026-11-10 (stud world P10b). Its predecessor (`0x5AD6_2E91`, recorded
/// before raster reads moved behind `forged::Rasters`) also hashed five voxel regions
/// and one key's voxel LOD, which went with the voxel generator; the sampler code this
/// part hashes was not touched by that deletion.
const SAMPLERS_GUARD_CRC: u32 = 0xDC18_3DE2;
/// crc32 of what the stud generator builds from the guard world: five whole regions
/// (recipes and tree bricks) and one region's far terrain. Recorded 2026-11-10 (P10b).
const STUD_TERRAIN_GUARD_CRC: u32 = 0x8FAA_FF6B;

/// Every public sampler of a real forged world (4,096 m / 16 m, seed 4) on a
/// 7 m grid over the map and 200 m past it, then five whole regions and one
/// region's far terrain, hashed bit-exactly (`to_bits`, bincode of structs
/// without hash maps; fixed iteration order).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forged_sampling_is_bit_exact() {
    let p = worldgen::forge::ForgeParams { size_m: 4096.0, cell_m: 16.0, seed: 4, ..Default::default() };
    let world = ForgedWorld::new(worldgen::forge::forge(&p).artifact(&p));
    let mut h = crc32fast::Hasher::new();
    let f64b = |h: &mut crc32fast::Hasher, v: f64| h.update(&v.to_bits().to_le_bytes());
    let f32b = |h: &mut crc32fast::Hasher, v: f32| h.update(&v.to_bits().to_le_bytes());

    let (min, max) = world.artifact_bounds_engine_m();
    const OUTSIDE_M: f64 = 200.0;
    const STRIDE_M: f64 = 7.0;
    let steps = |lo: f64, hi: f64| ((hi - lo + 2.0 * OUTSIDE_M) / STRIDE_M).ceil() as i64;
    let (nx, nz) = (steps(min[0], max[0]), steps(min[1], max[1]));
    let (mut wet, mut points) = (0usize, 0usize);
    for kz in 0..=nz {
        let z = min[1] - OUTSIDE_M + kz as f64 * STRIDE_M;
        for kx in 0..=nx {
            let x = min[0] - OUTSIDE_M + kx as f64 * STRIDE_M;
            points += 1;
            let s = world.sample(x, z);
            f64b(&mut h, s.ground_y);
            match s.water_y {
                Some(w) => {
                    wet += 1;
                    h.update(&[1]);
                    f64b(&mut h, w);
                }
                None => h.update(&[0]),
            }
            let c = world.column_climate(x, z);
            h.update(&[c.biome as u8, c.salt_flat as u8, c.snow_patch as u8]);
            f32b(&mut h, c.snow_line_m);
            f32b(&mut h, c.temperature_c);
            f32b(&mut h, c.glacier_share);
            h.update(&[world.biome_at(x, z) as u8]);
            let (id, kind) = world.region_cell(x, z);
            h.update(&id.to_le_bytes());
            h.update(&[kind.map_or(u8::MAX, |k| k as u8)]);
            let cc = world.cell_climate(x, z);
            f32b(&mut h, cc.height_m);
            f32b(&mut h, cc.temperature_c);
            f32b(&mut h, cc.precip_mm);
            h.update(&[cc.drainage, cc.salinity, cc.is_lake as u8]);
            f32b(&mut h, world.snow_line_m(x, z));
            h.update(&world.geo_offset_blocks(x, z).to_le_bytes());
            h.update(&[world.surface_rock(x, z).map_or(u8::MAX, |r| r)]);
        }
    }
    // Raster cells, the map and 200 m past it.
    let n = i64::from(world.globals().n);
    let pad = (OUTSIDE_M / world.globals().cell_m).ceil() as i64;
    for iz in -pad..n + pad {
        for ix in -pad..n + pad {
            f64b(&mut h, world.extended_cell_height(ix, iz));
        }
    }
    let samplers_crc = h.finalize();

    // Whole regions: the centre and the four corners of the map.
    let mut h = crc32fast::Hasher::new();
    let rb = game::REGION_BLOCKS as f64;
    let rc = |x: f64, z: f64| RegionCoords::new((x / rb).floor() as i32, (z / rb).floor() as i32);
    let regions = [
        RegionCoords::new(0, 0),
        rc(min[0], min[1]),
        rc(max[0] - 1.0, min[1]),
        rc(min[0], max[1] - 1.0),
        rc(max[0] - 1.0, max[1] - 1.0),
    ];
    for &r in &regions {
        let seed = region(&world, r);
        h.update(&bincode::serialize(&seed.ground).unwrap());
        h.update(&bincode::serialize(&seed.features).unwrap());
    }
    let key = game::lod::LodKey::for_region(16, RegionCoords::new(0, 0));
    h.update(&bincode::serialize(&stud_lod_tile_forged(&world, &key)).unwrap());
    let terrain_crc = h.finalize();

    let a = world.artifact().expect("whole world");
    println!(
        "guard: {points} points ({wet} wet), {} rivers, {} lakes, edges {:?}, regions {regions:?}: \
         samplers crc {samplers_crc:#010X}, stud terrain crc {terrain_crc:#010X}",
        a.rivers.len(),
        a.lakes.len(),
        a.edges,
    );
    assert!(!a.rivers.is_empty() && !a.lakes.is_empty() && wet > 0, "the guard world must hold rivers and lakes");
    assert_eq!(samplers_crc, SAMPLERS_GUARD_CRC, "forged sampling changed: crc {samplers_crc:#010X}");
    assert_eq!(terrain_crc, STUD_TERRAIN_GUARD_CRC, "forged stud terrain changed: crc {terrain_crc:#010X}");
}
