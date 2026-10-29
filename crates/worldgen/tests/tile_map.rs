//! World-map tile summaries (`worldgen::tile_map`): one summary per engine
//! region tile laid over the forged artifact, computed from its rasters and
//! point samples only, on the synthetic world of `common`.

use game::{RegionCoords, TileSummary, WorldMap};
use worldgen::forged::{ForgedWorld, OUTSIDE_HEIGHT_M};
use worldgen::tile_map::*;

mod common;
use common::*;

/// One tile's edge in engine metres (= `REGION_BLOCKS` blocks).
fn tile_m() -> f64 {
    f64::from(game::REGION_SIZE / game::movement::UNITS_PER_METER)
}

/// Engine metres per block column.
fn block_m() -> f64 {
    game::BLOCK_VOXELS as f64 / f64::from(game::movement::UNITS_PER_METER)
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
    assert!(is_dry(&world.sample(x, z)), "landing {landing:?} of {rc:?} is wet: {:?}", world.sample(x, z));
    landing
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
    assert_eq!(map.rock_names, world.artifact().rock_names);
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
    }
}

#[test]
fn rasterize_size_and_grid() {
    let (w, h, px) = rasterize(&two_tile_map());
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
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0], "Region (4, -2)");
    assert_eq!(lines[1], "Terrain: Lowland (provisional)");
    assert_eq!(lines[2], "Elevation: 0\u{2013}20 m (mean 10 m)");
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
    assert_eq!(lines[3..], ["Rivers: 2 (largest 6 m wide)", "Lake: level 12 m", "Rock: granite"]);
    let bad = WorldMap { tiles: vec![tile(|t| t.surface_rock = Some(9))], ..full };
    assert_eq!(tile_info_lines(&bad, RegionCoords::new(3, -2)).len(), 3, "out-of-range rock omitted");
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
