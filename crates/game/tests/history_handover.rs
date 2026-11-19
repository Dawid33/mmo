//! Plan 2 hand-over: what a region spawns when it starts, what happens to people when it
//! parks or someone crosses a seam, and which outcomes become region directives. Pure
//! `History` — the world manager is played by calling the API directly — then the same
//! hand-over driven by a real `WorldManager` over `InlineSpawner` regions.

use game::history::rules::*;
use game::history::*;
use game::{Biome, RegionCoords, TileSummary, WorldMap};

const LANDING: [u32; 2] = [20, 30];

fn rc(x: i32, z: i32) -> RegionCoords {
    RegionCoords::new(x, z)
}

/// `width` × `height` grassland regions, landing at [`LANDING`].
pub fn land_map(width: u32, height: u32) -> WorldMap {
    let tile = TileSummary {
        ground_min_m: 0.0,
        ground_mean_m: 10.0,
        ground_max_m: 10.0,
        // Gentle: under `game::EMBARK_MAX_RELIEF_PER_REGION_M` at every region size.
        relief_m: 10.0,
        slope: [0.0, 0.0],
        sea_fraction: 0.0,
        lake_fraction: 0.0,
        lake_level_m: None,
        rivers: 1,
        river_max_discharge_m2: 0.0,
        river_max_width_m: 0.0,
        surface_rock: None,
        landing: Some(LANDING),
        biome: Biome::Grassland,
        region: 0,
        temperature_c: 15.0,
        precip_mm: 800.0,
        savagery: 0,
        alignment: 0,
        volcano: false,
    };
    WorldMap {
        origin: rc(0, 0),
        width,
        height,
        tiles: vec![tile; (width * height) as usize],
        rock_names: vec![],
        region_types: vec![],
    }
}

pub fn start() -> CalendarTick {
    CalendarTick::from_days(HISTORY_START_YEAR * DAYS_PER_YEAR)
}

// ---- the world manager runs the history -------------------------------------


