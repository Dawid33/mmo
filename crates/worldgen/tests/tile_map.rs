//! World-map tile summaries (`worldgen::tile_map`): one summary per engine
//! region tile laid over the forged artifact, computed from its rasters and
//! point samples only, on the synthetic world of `common`.

use game::{RegionCoords, TileSummary, WorldMap};
use worldgen::forged::{ForgedWorld, OUTSIDE_HEIGHT_M};
use worldgen::tile_map::*;

use crate::common;
use common::*;

/// One tile's edge in engine metres (= `REGION_BLOCKS` blocks).
fn tile_m() -> f64 {
    f64::from(game::REGION_SIZE / game::movement::UNITS_PER_METER)
}

/// Engine metres per block column (the map's 1 m sampling columns).
fn block_m() -> f64 {
    1.0
}

/// The region tile holding artifact point `(ax, az)`.
fn tile_at(ax: f64, az: f64) -> RegionCoords {
    let (x, z) = engine(ax, az);
    RegionCoords::new((x / tile_m()).floor() as i32, (z / tile_m()).floor() as i32)
}

fn summary(map: &WorldMap, rc: RegionCoords) -> &TileSummary {
    map.tile(rc).unwrap_or_else(|| panic!("tile {rc:?} missing from the map"))
}

/// Engine metres of the centre of block column `landing` of tile `rc`.
fn landing_m(rc: RegionCoords, landing: [u32; 2]) -> (f64, f64) {
    let at = |r: i32, b: u32| f64::from(r) * tile_m() + (f64::from(b) + 0.5) * block_m();
    (at(rc.x, landing[0]), at(rc.z, landing[1]))
}

fn assert_lands_dry(world: &ForgedWorld, rc: RegionCoords, tile: &TileSummary) -> [u32; 2] {
    let landing = tile.landing.unwrap_or_else(|| panic!("tile {rc:?} has no landing: {tile:?}"));
    assert!(landing.iter().all(|&b| (b as usize) < game::REGION_BLOCKS), "landing {landing:?} off the region");
    let (x, z) = landing_m(rc, landing);
    let sample = world.sample(x, z);
    assert!(is_dry(&sample), "landing {landing:?} of {rc:?} is wet: {sample:?}");
    let above_sea = surface_block(sample.ground_y) - worldgen::pipeline::SEA_TOP_BLOCK;
    assert!(
        above_sea >= LANDING_MIN_BLOCKS_ABOVE_SEA,
        "landing {landing:?} of {rc:?} only {above_sea} blocks above the sea: {sample:?}"
    );
    landing
}

#[test]
fn a_waterline_column_is_dry_but_no_landing() {
    use worldgen::forged::PointSample;
    use worldgen::pipeline::SEA_TOP_BLOCK;
    // The ground height whose surface block is `block` (its centre just under it).
    let ground = |block: i32| f64::from(block) + 1.0;
    let at = |block: i32| PointSample { ground_y: ground(block), water_y: None };
    assert_eq!(surface_block(ground(SEA_TOP_BLOCK)), SEA_TOP_BLOCK);
    assert!(is_dry(&at(SEA_TOP_BLOCK)), "level with the sea is dry");
    assert!(!is_landing(&at(SEA_TOP_BLOCK)), "but too marginal to land on");
    assert!(is_landing(&at(SEA_TOP_BLOCK + LANDING_MIN_BLOCKS_ABOVE_SEA)));
    let lake = PointSample { water_y: Some(ground(SEA_TOP_BLOCK + 5) + 1.0), ..at(SEA_TOP_BLOCK + 5) };
    assert!(!is_landing(&lake), "high but under a lake");
}

#[test]
fn grid_covers_the_artifact() {
    let world = ForgedWorld::new(synthetic());
    let map = build_world_map(&world);
    assert_eq!(map.tiles.len(), (map.width * map.height) as usize);
    assert_eq!(map.origin, tile_at(0.0, 0.0), "origin is the region of the artifact's min corner");
    let size = N as f64 * CELL_M;
    // The tile holding the artifact's last metre closes the grid.
    let last = tile_at(size - 1e-6, size - 1e-6);
    assert_eq!((map.width, map.height), ((last.x - map.origin.x + 1) as u32, (last.z - map.origin.z + 1) as u32));
    // Every tile intersecting the artifact is present, and nothing beyond it.
    let (x0, z0) = engine(0.0, 0.0);
    let (x1, z1) = engine(size, size);
    for (i, _) in map.tiles.iter().enumerate() {
        let rc = map.coords(i);
        let (tx, tz) = (f64::from(rc.x) * tile_m(), f64::from(rc.z) * tile_m());
        assert!(tx < x1 && tx + tile_m() > x0 && tz < z1 && tz + tile_m() > z0, "{rc:?} misses the artifact");
    }
    assert!(map.tile(RegionCoords::new(map.origin.x - 1, map.origin.z)).is_none());
    assert_eq!(map.rock_names, world.globals().rock_names);
}

#[test]
fn sea_tile_has_no_landing() {
    let world = ForgedWorld::new(synthetic());
    let map = build_world_map(&world);
    // Artifact x < 4 cells is sea; this tile holds nothing east of it.
    let rc = tile_at(CELL_M, 0.5 * N as f64 * CELL_M);
    assert!(f64::from(rc.x + 1) * tile_m() <= engine(4.0 * CELL_M, 0.0).0, "the tile must lie in the sea strip");
    let tile = summary(&map, rc);
    assert!(tile.sea_fraction > 0.95, "{tile:?}");
    assert_eq!(tile.landing, None);
}

#[test]
fn land_tile_lands_near_its_centre_on_dry_ground() {
    let world = ForgedWorld::new(synthetic());
    let map = build_world_map(&world);
    // On the plain, far from the river (z 512) and the lake.
    let rc = tile_at(790.0, 880.0);
    let tile = summary(&map, rc);
    assert_eq!(tile.sea_fraction, 0.0, "{tile:?}");
    assert_eq!(tile.rivers, 0);
    let landing = assert_lands_dry(&world, rc, tile);
    let centre = game::REGION_BLOCKS as f64 * 0.5;
    for b in landing {
        assert!((f64::from(b) + 0.5 - centre).abs() <= 2.0, "landing {landing:?} far from the centre {centre}");
    }
    // Ground follows the plain over the tile.
    assert!(tile.ground_min_m <= tile.ground_mean_m && tile.ground_mean_m <= tile.ground_max_m);
    let (x, _) = engine(790.0, 0.0);
    let ax_mid = (x / tile_m()).floor() * tile_m() + tile_m() * 0.5 - engine(0.0, 0.0).0;
    assert!((f64::from(tile.ground_mean_m) - plain(ax_mid)).abs() < 1.0, "{tile:?}");
    assert!((f64::from(tile.slope[0]) - 0.03).abs() < 1e-3 && f64::from(tile.slope[1]).abs() < 1e-3, "{tile:?}");
}

#[test]
fn river_tile_counts_the_river_and_never_lands_in_it() {
    let world = ForgedWorld::new(synthetic());
    let map = build_world_map(&world);
    let rc = tile_at(400.0, RIVER_Z);
    let tile = summary(&map, rc);
    assert!(tile.rivers >= 1, "{tile:?}");
    assert_eq!(tile.river_max_width_m, RIVER_WIDTH_M);
    assert_eq!(tile.river_max_discharge_m2, 2.0e6);
    assert_lands_dry(&world, rc, tile);
    // A tile well away from the river counts none.
    assert_eq!(summary(&map, tile_at(400.0, RIVER_Z + 3.0 * tile_m())).rivers, 0);
}

#[test]
fn lake_tile_reports_lake() {
    let world = ForgedWorld::new(synthetic());
    let map = build_world_map(&world);
    let rc = tile_at(33.0 * CELL_M, 13.0 * CELL_M); // middle of the lake
    let tile = summary(&map, rc);
    assert!(tile.lake_fraction > 0.0, "{tile:?}");
    assert_eq!(tile.lake_level_m, Some(LAKE_LEVEL));
    let dry = summary(&map, tile_at(790.0, 880.0));
    assert_eq!((dry.lake_fraction, dry.lake_level_m), (0.0, None));
}

#[test]
fn rim_tile_counts_outside_as_sea() {
    let world = ForgedWorld::new(synthetic());
    let map = build_world_map(&world);
    let size = N as f64 * CELL_M;
    // East rim: the plain runs right up to the artifact edge.
    let rc = tile_at(size - 1.0, 600.0);
    let (edge_x, _) = engine(size, 0.0);
    let tile_x1 = f64::from(rc.x + 1) * tile_m();
    assert!(tile_x1 > edge_x, "the tile must hang off the artifact");
    let outside = (tile_x1 - edge_x) / tile_m();
    let tile = summary(&map, rc);
    assert!(tile.sea_fraction as f64 >= outside - CELL_M / tile_m(), "outside share {outside}: {tile:?}");
    assert!(tile.sea_fraction < 1.0, "{tile:?}");
    assert_eq!(f64::from(tile.ground_min_m), OUTSIDE_HEIGHT_M, "outside is ocean at OUTSIDE_HEIGHT_M");
    if let Some(landing) = tile.landing {
        let (x, _) = landing_m(rc, landing);
        assert!(x < edge_x, "landing {landing:?} outside the artifact");
        assert_lands_dry(&world, rc, tile);
    }
}

#[test]
fn surface_rock_is_the_top_formation() {
    let world = ForgedWorld::new(synthetic());
    let (x, z) = engine(790.0, 880.0);
    assert_eq!(world.surface_rock(x, z), Some(SANDSTONE));
    let map = build_world_map(&world);
    let tile = summary(&map, tile_at(790.0, 880.0));
    assert_eq!(tile.surface_rock, Some(SANDSTONE));
    assert_eq!(map.rock_names[SANDSTONE as usize], worldgen::forge::geology::ROCKS[SANDSTONE as usize].name);
}

fn tile(f: impl FnOnce(&mut TileSummary)) -> TileSummary {
    let mut t = TileSummary {
        ground_min_m: 0.0,
        ground_mean_m: 10.0,
        ground_max_m: 20.0,
        relief_m: 5.0,
        slope: [0.0, 0.0],
        sea_fraction: 0.0,
        lake_fraction: 0.0,
        lake_level_m: None,
        rivers: 0,
        river_max_discharge_m2: 0.0,
        river_max_width_m: 0.0,
        surface_rock: None,
        landing: Some([1, 1]),
        biome: game::Biome::Grassland,
        region: 0,
        temperature_c: 14.0,
        precip_mm: 900.0,
        savagery: 50,
        alignment: 0,
        volcano: false,
    };
    f(&mut t);
    t
}

#[test]
fn terrain_class_thresholds() {
    use TerrainClass::*;
    assert_eq!(terrain_class(&tile(|_| {})), Lowland);
    assert_eq!(terrain_class(&tile(|t| t.landing = None)), Ocean);
    assert_eq!(terrain_class(&tile(|t| t.sea_fraction = OCEAN_SEA_FRACTION)), Ocean);
    assert_eq!(terrain_class(&tile(|t| t.sea_fraction = OCEAN_SEA_FRACTION - 0.01)), Coast);
    assert_eq!(terrain_class(&tile(|t| t.sea_fraction = COAST_SEA_FRACTION)), Lowland);
    assert_eq!(terrain_class(&tile(|t| t.sea_fraction = COAST_SEA_FRACTION + 0.01)), Coast);
    assert_eq!(terrain_class(&tile(|t| t.relief_m = HILLS_RELIEF_M)), Hills);
    assert_eq!(terrain_class(&tile(|t| t.relief_m = HILLS_RELIEF_M - 0.1)), Lowland);
    assert_eq!(terrain_class(&tile(|t| t.relief_m = MOUNTAINS_RELIEF_M)), Mountains);
    assert_eq!(terrain_class(&tile(|t| t.relief_m = MOUNTAINS_RELIEF_M - 0.1)), Hills);
    assert_eq!(terrain_class(&tile(|t| t.ground_max_m = MOUNTAINS_MIN_M)), Mountains);
    assert_eq!(terrain_class(&tile(|t| t.ground_max_m = MOUNTAINS_MIN_M - 0.1)), Lowland);
    assert_eq!(Mountains.label(), "Mountains");
}

fn two_tile_map() -> WorldMap {
    WorldMap {
        origin: RegionCoords::new(3, -2),
        width: 2,
        height: 1,
        tiles: vec![tile(|t| t.rivers = 1), tile(|_| {})],
        rock_names: vec!["granite".into()],
        region_types: vec![],
    }
}

#[test]
fn rasterize_size_and_grid() {
    let (w, h, px) = rasterize(&two_tile_map(), MapLayer::Relief);
    assert_eq!((w, h), (2 * MAP_TILE_PX, MAP_TILE_PX));
    assert_eq!(px.len(), (w * h) as usize);
    let at = |x: u32, z: u32| px[(z * w + x) as usize];
    assert!(at(7, 0)[0] < at(3, 3)[0] || at(7, 0)[1] < at(3, 3)[1], "grid pixel darker");
    assert!(px.iter().all(|p| p[3] == 255));
    let river = |x: u32, z: u32| at(x, z)[..3] == RIVER_RGB;
    assert!(river(3, 3) && river(4, 4));
    assert!(!river(MAP_TILE_PX + 3, 3) && !river(MAP_TILE_PX + 4, 4), "no river mark on the dry tile");
}

#[test]
fn info_lines_omit_absent_facts() {
    let map = two_tile_map();
    let lines = tile_info_lines(&map, RegionCoords::new(4, -2));
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert_eq!(lines[0], "Region (4, -2)");
    assert_eq!(lines[1], "Biome: Grassland");
    assert_eq!(lines[2], "Terrain: Lowland (provisional)");
    assert_eq!(lines[3], "Elevation: 0\u{2013}20 m (mean 10 m)");
    assert_eq!(lines[4], "Climate: 14 \u{b0}C, 900 mm rain a year");
    assert_eq!(lines[5], "Savagery: 50");
    assert!(tile_info_lines(&map, RegionCoords::new(0, 0)).is_empty());

    let full = WorldMap {
        tiles: vec![tile(|t| {
            t.rivers = 2;
            t.river_max_width_m = 6.0;
            t.lake_level_m = Some(12.0);
            t.surface_rock = Some(0);
        })],
        width: 1,
        ..two_tile_map()
    };
    let lines = tile_info_lines(&full, RegionCoords::new(3, -2));
    assert_eq!(lines[6..], ["Rivers: 2 (largest 6 m wide)", "Lake: level 12 m", "Rock: granite"]);
    let bad = WorldMap { tiles: vec![tile(|t| t.surface_rock = Some(9))], ..full };
    assert_eq!(tile_info_lines(&bad, RegionCoords::new(3, -2)).len(), 6, "out-of-range rock omitted");

    // A forge region (type + number, alignment when not neutral) and a volcano.
    let regions = WorldMap {
        tiles: vec![tile(|t| {
            t.region = 1;
            t.alignment = -1;
            t.volcano = true;
        })],
        region_types: vec![game::RegionType::Ocean, game::RegionType::Forest],
        ..bad
    };
    let lines = tile_info_lines(&regions, RegionCoords::new(3, -2));
    assert_eq!(lines[2], "Biome region: Forest #1 (evil)", "{lines:?}");
    assert!(lines.contains(&"Volcano".to_string()), "{lines:?}");
    let neutral = WorldMap { tiles: vec![tile(|t| t.region = 1)], ..regions };
    assert_eq!(tile_info_lines(&neutral, RegionCoords::new(3, -2))[2], "Biome region: Forest #1");
}

#[test]
fn play_blocker_rules() {
    let mut map = two_tile_map();
    map.tiles[1].landing = None;
    let (a, b) = (RegionCoords::new(3, -2), RegionCoords::new(4, -2));
    assert_eq!(play_blocker(&map, a, None), None);
    assert_eq!(play_blocker(&map, a, Some(a)), Some("You are here"));
    assert_eq!(play_blocker(&map, a, Some(b)), None);
    assert_eq!(play_blocker(&map, b, None), Some("No dry land"));
    assert_eq!(play_blocker(&map, b, Some(b)), Some("You are here"));
    assert_eq!(play_blocker(&map, RegionCoords::new(9, 9), None), Some("No dry land"));
}

/// The shipped default world summarises into a non-empty map with landings.
/// Skipped when `worlds/default.world` has not been forged yet (or is an older format).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn default_world_map_builds() {
    let path = worldgen::forged::from_repo_root(std::path::Path::new("worlds/default.world"));
    if !path.exists() {
        println!("{} missing: skipping", path.display());
        return;
    }
    let world = match ForgedWorld::load(&path) {
        Err(e @ worldgen::forged::ArtifactError::OldVersion { .. }) => {
            println!("{}: {e}; skipping", path.display());
            return;
        }
        loaded => loaded.expect("load default world"),
    };
    let started = std::time::Instant::now();
    let map = build_world_map(&world);
    println!("{}x{} tiles in {} ms", map.width, map.height, started.elapsed().as_millis());
    assert!(map.width > 0 && map.height > 0);
    assert!(map.tiles.iter().any(|t| t.landing.is_some()), "no tile has a landing");
}

/// The tile's biome is the stored biome raster's mode over its cells, its
/// region the region raster's mode (with that region's alignment), and its
/// climate the cell means.
#[test]
fn summary_carries_dominant_biome() {
    use game::Biome;
    let mut a = synthetic();
    // One cell in sixteen grassland, the rest savanna: savanna wins every
    // tile (no tile, edge strips included, holds as many grassland cells).
    for (i, b) in a.biome.iter_mut().enumerate() {
        let (ix, iz) = (i % N, i / N);
        *b = if ix % 4 == 1 && iz % 4 == 1 { Biome::Grassland } else { Biome::Savanna } as u8;
    }
    a.savagery = vec![40; N * N];
    // One volcano on the plain.
    let volcano = [790.0f32, 880.0];
    a.volcanoes = vec![volcano];
    let world = ForgedWorld::new(a);
    let map = build_world_map(&world);
    let a = world.artifact().expect("whole world");
    assert_eq!(map.region_types, a.regions.iter().map(|r| r.kind).collect::<Vec<_>>());
    for (i, t) in map.tiles.iter().enumerate() {
        let rc = map.coords(i);
        assert_eq!(t.biome, Biome::Savanna, "{rc:?}");
        let region = a.regions.get(usize::from(t.region)).unwrap_or_else(|| panic!("{rc:?}: region {}", t.region));
        assert_eq!(t.alignment, region.alignment, "{rc:?}");
        assert!((t.temperature_c - 14.0).abs() < 0.5, "{rc:?}: {t:?}");
        assert!((t.precip_mm - 900.0).abs() < 10.0, "{rc:?}: {t:?}");
        assert_eq!(t.savagery, 40, "{rc:?}");
        let here = rc == tile_at(f64::from(volcano[0]), f64::from(volcano[1]));
        assert_eq!(t.volcano, here, "{rc:?}");
    }
    // The mode, not the first cell: the region holding most of a land tile.
    let rc = tile_at(790.0, 880.0);
    let t = summary(&map, rc);
    let mut counts = std::collections::BTreeMap::<u16, u32>::new();
    let (x0, z0) = (f64::from(rc.x) * tile_m(), f64::from(rc.z) * tile_m());
    let (ox, oz) = engine(0.0, 0.0);
    for iz in 0..N {
        for ix in 0..N {
            let (x, z) = (ox + (ix as f64 + 0.5) * CELL_M, oz + (iz as f64 + 0.5) * CELL_M);
            if x >= x0 && x < x0 + tile_m() && z >= z0 && z < z0 + tile_m() {
                *counts.entry(a.region[iz * N + ix]).or_default() += 1;
            }
        }
    }
    let mode = counts.iter().max_by(|(i, x), (j, y)| x.cmp(y).then(j.cmp(i))).map(|(&id, _)| id);
    assert_eq!(Some(t.region), mode);
}

/// One pixel per tile, opaque; the biome layer is the shared palette and
/// every biome's colour differs clearly from every other's.
#[test]
fn rasterize_layer_sizes_and_colours() {
    use game::Biome;
    let mut map = two_tile_map();
    map.tiles[0].biome = Biome::Taiga;
    map.tiles[1].biome = Biome::SandDesert;
    for layer in MapLayer::ALL {
        let (w, h, px) = rasterize_layer(&map, layer);
        assert_eq!((w, h), (2, 1), "{layer:?}");
        assert_eq!(px.len(), 2);
        assert!(px.iter().all(|p| p[3] == 255), "{layer:?} opaque");
    }
    let (_, _, px) = rasterize_layer(&map, MapLayer::Biome);
    assert_eq!(px[0][..3], Biome::Taiga.map_rgb());
    assert_eq!(px[1][..3], Biome::SandDesert.map_rgb());
    for (i, a) in Biome::ALL.iter().enumerate() {
        for b in &Biome::ALL[i + 1..] {
            let d: i32 = (0..3).map(|k| (i32::from(a.map_rgb()[k]) - i32::from(b.map_rgb()[k])).abs()).sum();
            assert!(d >= 40, "{a:?} and {b:?} too alike ({d})");
        }
    }
    // Warmer is redder, wetter is bluer.
    map.tiles[1].temperature_c = 30.0;
    map.tiles[0].temperature_c = -20.0;
    let (_, _, px) = rasterize_layer(&map, MapLayer::Temperature);
    assert!(px[1][0] > px[0][0] && px[1][2] < px[0][2], "{px:?}");
    map.tiles[0].precip_mm = 100.0;
    map.tiles[1].precip_mm = 2500.0;
    let (_, _, px) = rasterize_layer(&map, MapLayer::Precipitation);
    assert!(px[1][2] > px[0][2], "{px:?}");
    // Two regions, two colours; one region, one.
    map.region_types = vec![game::RegionType::Forest, game::RegionType::Grassland];
    map.tiles[1].region = 1;
    let (_, _, px) = rasterize_layer(&map, MapLayer::Regions);
    assert_ne!(px[0], px[1]);
    map.tiles[1].region = 0;
    let (_, _, px) = rasterize_layer(&map, MapLayer::Regions);
    assert_eq!(px[0], px[1]);
    // The 8 px picture of a layer is its 1 px picture, grid-lined.
    let (w, _, big) = rasterize(&map, MapLayer::Biome);
    let (_, _, small) = rasterize_layer(&map, MapLayer::Biome);
    assert_eq!(big[(3 * w + MAP_TILE_PX + 3) as usize], small[1]);
}

/// A 100 km world's map (196 × 196 tiles at 512 m) goes out in one packet
/// well under 8 MB, even with no two tiles alike.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn world_map_packet_fits() {
    let side = 196u32;
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tiles = (0..side * side)
        .map(|_| {
            let r: Vec<u64> = (0..20).map(|_| next()).collect();
            let f = |k: usize| (r[k] % 1_000_000) as f32 / 7.0;
            TileSummary {
                ground_min_m: f(1),
                ground_mean_m: f(2),
                ground_max_m: f(3),
                relief_m: f(4),
                slope: [f(5), f(6)],
                sea_fraction: f(7),
                lake_fraction: f(8),
                lake_level_m: Some(f(9)),
                rivers: 3,
                river_max_discharge_m2: f(10),
                river_max_width_m: f(11),
                surface_rock: Some(2),
                landing: Some([17, 300]),
                biome: game::Biome::ALL[(r[18] % 20) as usize],
                region: (r[14] % 2750) as u16,
                temperature_c: f(12),
                precip_mm: f(13),
                savagery: (r[15] % 101) as u8,
                alignment: (r[16] % 3) as i8 - 1,
                volcano: r[17] % 2 == 0,
            }
        })
        .collect();
    let map = WorldMap {
        origin: RegionCoords::new(-98, -98),
        width: side,
        height: side,
        tiles,
        rock_names: worldgen::forge::geology::ROCKS.iter().map(|r| r.name.to_string()).collect(),
        region_types: vec![game::RegionType::Forest; 2750],
    };
    let bytes = game::encode_packet(&game::ServerPacket::WorldMap(map)).expect("encode");
    println!("196x196 world map packet: {} bytes", bytes.len());
    assert!(bytes.len() < 8 * 1024 * 1024, "{} bytes", bytes.len());
}

/// The whole-map build (rivers bucketed by tile in one pass) gives every
/// tile exactly what summarising it alone does.
#[test]
fn map_tiles_match_single_tile_summaries() {
    let world = ForgedWorld::new(synthetic());
    let map = build_world_map(&world);
    assert!(map.tiles.iter().any(|t| t.rivers > 0), "the synthetic river is on the map");
    for (i, t) in map.tiles.iter().enumerate() {
        assert_eq!(*t, summarize_tile(&world, map.coords(i)), "{:?}", map.coords(i));
    }
}
