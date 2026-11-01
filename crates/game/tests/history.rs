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

// ---- Task 3: queue, log, people, sites, births, deaths, marriages ----
use game::history::log::LogKind;

fn start() -> CalendarTick {
    CalendarTick::from_days(HISTORY_START_YEAR * DAYS_PER_YEAR)
}

fn years_before(t: CalendarTick, y: u64) -> CalendarTick {
    CalendarTick(t.0 - y * DAYS_PER_YEAR * TICKS_PER_DAY)
}

fn land_grid() -> SocietyGrid {
    SocietyGrid::from_world_map(&test_map(8, 8, |_, _| false))
}

fn growing_world() -> History {
    let s = start();
    let mut h = History::new(7, s, land_grid());
    let civ = h.add_civ(Race::Dwarf);
    let mut founders = Vec::new();
    for _ in 0..5 {
        founders.push(h.add_founder(Sex::Female, years_before(s, 20)));
        founders.push(h.add_founder(Sex::Male, years_before(s, 20)));
    }
    h.found_site(civ, RegionCoords::new(3, 3), &founders, None, false);
    h
}

#[test]
fn site_grows_by_births() {
    let mut h = growing_world();
    h.advance(start().plus_days(10 * DAYS_PER_YEAR));
    let site = h.site(SiteId(0)).unwrap();
    assert!(h.living_members(SiteId(0)).len() > 10, "{}", site.members.len());
    assert!(h.log().iter().any(|e| matches!(e.kind, LogKind::Married { .. })));
    let mut born = 0;
    for e in h.log() {
        if let LogKind::Born { mother: Some(m), father: Some(f), .. } = &e.kind {
            born += 1;
            assert_eq!(h.person(*m).unwrap().spouse, Some(*f));
        }
    }
    assert!(born >= 1);
}

#[test]
fn old_people_die() {
    let s = start();
    let mut h = History::new(1, s, land_grid());
    let civ = h.add_civ(Race::Dwarf);
    let p = h.add_founder(Sex::Female, years_before(s, 170));
    h.found_site(civ, RegionCoords::new(1, 1), &[p], None, false);
    h.advance(s.plus_days(1));
    assert_eq!(h.person(p).unwrap().died.map(|d| d.1), Some(DeathCause::OldAge));
    assert!(h.log().iter().any(|e| matches!(e.kind, LogKind::Died { person, .. } if person == p)));
    assert!(h.log().iter().any(|e| matches!(e.kind, LogKind::SiteAbandoned { .. })));
    assert!(h.site(SiteId(0)).unwrap().abandoned.is_some());
}

#[test]
fn history_is_deterministic() {
    let mut a = growing_world();
    let mut b = growing_world();
    a.advance(start().plus_days(20 * DAYS_PER_YEAR));
    b.advance(start().plus_days(20 * DAYS_PER_YEAR));
    assert_eq!(bincode::serialize(&a).unwrap(), bincode::serialize(&b).unwrap());
}

#[test]
fn advance_is_idempotent_and_monotonic() {
    let mut h = growing_world();
    let t = start().plus_days(5 * DAYS_PER_YEAR);
    h.advance(t);
    let snap = bincode::serialize(&h).unwrap();
    h.advance(t);
    h.advance(CalendarTick(t.0 - 100));
    assert_eq!(bincode::serialize(&h).unwrap(), snap);
    assert_eq!(h.now(), t);
}

#[test]
fn no_marriage_between_siblings() {
    let s = start();
    let mut h = History::new(3, s, land_grid());
    let civ = h.add_civ(Race::Dwarf);
    let mother = h.add_founder(Sex::Female, years_before(s, 40));
    let father = h.add_founder(Sex::Male, years_before(s, 40));
    let son = h.add_founder(Sex::Male, years_before(s, 20));
    let daughter = h.add_founder(Sex::Female, years_before(s, 20));
    for c in [son, daughter] {
        let p = h.people.get_mut(&c).unwrap();
        p.mother = Some(mother);
        p.father = Some(father);
    }
    h.found_site(civ, RegionCoords::new(1, 1), &[son, daughter], None, false);
    h.advance(s.plus_days(10 * DAYS_PER_YEAR));
    assert_eq!(h.person(son).unwrap().spouse, None);
    assert_eq!(h.person(daughter).unwrap().spouse, None);
    assert!(!h.log().iter().any(|e| matches!(e.kind, LogKind::Married { .. })));
}

#[test]
fn unrelated_adults_do_marry() {
    let s = start();
    let mut h = History::new(3, s, land_grid());
    let civ = h.add_civ(Race::Dwarf);
    let a = h.add_founder(Sex::Female, years_before(s, 20));
    let b = h.add_founder(Sex::Male, years_before(s, 20));
    h.found_site(civ, RegionCoords::new(1, 1), &[a, b], None, false);
    h.advance(s.plus_days(30 * DAYS_PER_YEAR));
    assert_eq!(h.person(a).unwrap().spouse, Some(b));
}

#[test]
fn describe_born_line() {
    let mut h = growing_world();
    h.advance(start().plus_days(10 * DAYS_PER_YEAR));
    let e = h.log().iter().find(|e| matches!(e.kind, LogKind::Born { site: Some(_), .. })).unwrap();
    let line = e.describe(&h);
    assert!(line.contains("was born at Site 0"), "{line}");
}
