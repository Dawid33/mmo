use game::history::*;

#[test]
fn calendar_dates() {
    assert_eq!(CalendarTick(0).date(), Date { year: 0, month: 1, day: 1 });
    let one_year = CalendarTick::from_days(DAYS_PER_YEAR);
    assert_eq!(one_year.date(), Date { year: 1, month: 1, day: 1 });
    assert_eq!(CalendarTick::from_days(DAYS_PER_MONTH + 2).date(), Date { year: 0, month: 2, day: 3 });
    assert_eq!(one_year.years_since(CalendarTick(0)), 1);
    assert_eq!(TICKS_PER_DAY, 200);
    assert_eq!(CalendarTick::from_days(2).date().to_string(), "3 Granite 0");
}

#[test]
fn person_age_and_alive() {
    let p = Person::newborn(PersonId(1), Sex::Female, CalendarTick(0), None, None, None, Location::AtSite(SiteId(0)));
    assert!(p.alive());
    assert_eq!(p.age_years(CalendarTick::from_days(DAYS_PER_YEAR * 30 + 5)), 30);
}

// ---- Task 2: rolls, tunables, grid, routing ----
use game::history::rng::*;
use game::history::rules::*;
use game::history::grid::*;
use game::{Biome, RegionCoords, TileSummary, WorldMap};

fn test_map(width: u32, height: u32, sea: impl Fn(i32, i32) -> bool) -> WorldMap {
    let mut tiles = Vec::new();
    for z in 0..height as i32 {
        for x in 0..width as i32 {
            let is_sea = sea(x, z);
            tiles.push(TileSummary {
                ground_min_m: 0.0,
                ground_mean_m: 10.0,
                ground_max_m: 50.0,
                relief_m: 50.0,
                slope: [0.0, 0.0],
                sea_fraction: if is_sea { 1.0 } else { 0.0 },
                lake_fraction: 0.0,
                lake_level_m: None,
                rivers: 1,
                river_max_discharge_m2: 0.0,
                river_max_width_m: 0.0,
                surface_rock: None,
                landing: Some([256, 256]),
                biome: if is_sea { Biome::Ocean } else { Biome::Grassland },
                region: 0,
                temperature_c: 15.0,
                precip_mm: 800.0,
                savagery: 0,
                alignment: 0,
                volcano: false,
            });
        }
    }
    WorldMap {
        origin: RegionCoords::new(0, 0),
        width,
        height,
        tiles,
        rock_names: vec![],
        region_types: vec![],
    }
}

#[test]
fn roll_is_stateless() {
    let t = CalendarTick(77);
    assert_eq!(roll(t, 5, SALT_MARRY), roll(t, 5, SALT_MARRY));
    assert_ne!(roll(t, 5, SALT_MARRY), roll(t, 6, SALT_MARRY));
}

#[test]
fn death_age_in_range() {
    for id in 0..1000u64 {
        let y = death_age_days(PersonId(id as u32), CalendarTick(id * 31)) / DAYS_PER_YEAR;
        assert!((80..=160).contains(&y), "{y}");
    }
}

#[test]
fn birth_rate_falls_with_crowding() {
    assert!(birth_per_mille(10, 100) > birth_per_mille(80, 100));
    assert_eq!(birth_per_mille(100, 100), 0);
    assert_eq!(birth_per_mille(5, 0), 0);
}

#[test]
fn walk_ticks_matches_walk_speed() {
    let t = walk_ticks_per_region();
    let secs = t as f64 * game::TICK_RATE as f64 / 1000.0;
    let m = secs * 4.317;
    assert!((m - game::REGION_BLOCKS as f64).abs() < 1.0 * 4.317, "{m}");
}

#[test]
fn capacity_from_tiles() {
    let g = SocietyGrid::from_world_map(&test_map(3, 1, |x, _| x == 0));
    assert_eq!(g.capacity(RegionCoords::new(0, 0)), 0);
    assert!(!g.passable(RegionCoords::new(0, 0)));
    assert_eq!(g.capacity(RegionCoords::new(1, 0)), 50);
    assert!(g.passable(RegionCoords::new(1, 0)));
    assert!(!g.contains(RegionCoords::new(3, 0)));
    assert_eq!(g.regions().count(), 3);
}

#[test]
fn route_avoids_sea() {
    let g = SocietyGrid::from_world_map(&test_map(10, 3, |x, z| x == 5 && z != 2));
    let r = route(&g, RegionCoords::new(0, 0), RegionCoords::new(9, 0), ROUTE_MAX_NODES).unwrap();
    assert!(r.iter().all(|&rc| g.passable(rc)));
    assert!(r.contains(&RegionCoords::new(5, 2)));
    assert_eq!(r[0], RegionCoords::new(0, 0));
    assert_eq!(*r.last().unwrap(), RegionCoords::new(9, 0));
}

#[test]
fn route_unreachable() {
    let g = SocietyGrid::from_world_map(&test_map(10, 3, |x, _| x == 5));
    assert!(route(&g, RegionCoords::new(0, 0), RegionCoords::new(9, 0), ROUTE_MAX_NODES).is_none());
}

#[test]
fn route_deterministic() {
    let g = SocietyGrid::from_world_map(&test_map(10, 3, |x, z| x == 5 && z != 2));
    let a = route(&g, RegionCoords::new(0, 0), RegionCoords::new(9, 0), ROUTE_MAX_NODES);
    let b = route(&g, RegionCoords::new(0, 0), RegionCoords::new(9, 0), ROUTE_MAX_NODES);
    assert_eq!(a, b);
    assert_eq!(g.world_id(), g.world_id());
}
