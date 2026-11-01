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

#[test]
fn unplaced_founder_until_found_site() {
    let s = start();
    let mut h = History::new(1, s, land_grid());
    let civ = h.add_civ(Race::Dwarf);
    let p = h.add_founder(Sex::Female, years_before(s, 20));
    assert_eq!(h.person(p).unwrap().location, Location::Unplaced);
    let site = h.found_site(civ, RegionCoords::new(1, 1), &[p], None, false);
    assert_eq!(h.person(p).unwrap().location, Location::AtSite(site));
}

// ---- Task 4: journeys ----

fn leg() -> u64 {
    walk_ticks_per_region()
}

fn two_site_world(sea: impl Fn(i32, i32) -> bool) -> (History, Vec<PersonId>, Vec<PersonId>) {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&test_map(8, 3, sea)));
    let civ = h.add_civ(Race::Dwarf);
    let a: Vec<_> = (0..3).map(|_| h.add_founder(Sex::Female, years_before(s, 20))).collect();
    let b: Vec<_> = (0..2).map(|_| h.add_founder(Sex::Male, years_before(s, 20))).collect();
    h.found_site(civ, RegionCoords::new(0, 0), &a, None, false);
    h.found_site(civ, RegionCoords::new(6, 0), &b, None, false);
    (h, a, b)
}

#[test]
fn migrants_arrive_after_travel_time() {
    let (mut h, a, _) = two_site_world(|_, _| false);
    let departed = h.now();
    let party = vec![a[0], a[1]];
    let j = h
        .start_journey(party.clone(), SiteId(0), RegionCoords::new(6, 0), Purpose::Migrate { to: SiteId(1) })
        .unwrap();
    assert!(!h.site(SiteId(0)).unwrap().members.contains(&a[0]));
    assert_eq!(h.person(a[0]).unwrap().location, Location::Journey(j));
    let route = h.journey(j).unwrap().route.clone();
    assert_eq!(h.journey_region_at(j, departed), Some(route[0]));
    assert_eq!(h.journey_region_at(j, CalendarTick(departed.0 + 3 * leg())), Some(route[3]));
    assert_eq!(h.journey_progress_in_leg(j, CalendarTick(departed.0 + 3 * leg() + leg() / 2)), 500);
    h.advance(CalendarTick(departed.0 + 7 * leg()));
    for p in party {
        assert!(h.site(SiteId(1)).unwrap().members.contains(&p));
        assert_eq!(h.person(p).unwrap().site, Some(SiteId(1)));
        assert_eq!(h.person(p).unwrap().location, Location::AtSite(SiteId(1)));
    }
    assert!(h.journey(j).is_none());
    let dep = h.log().iter().position(|e| matches!(e.kind, LogKind::Departed { .. })).unwrap();
    let arr = h.log().iter().position(|e| matches!(e.kind, LogKind::Arrived { .. })).unwrap();
    assert!(dep < arr);
}

#[test]
fn journey_without_route_is_refused() {
    let (mut h, a, _) = two_site_world(|x, _| x == 3);
    let before = bincode::serialize(&h).unwrap();
    let r = h.start_journey(vec![a[0]], SiteId(0), RegionCoords::new(6, 0), Purpose::Migrate { to: SiteId(1) });
    assert!(r.is_none());
    assert_eq!(bincode::serialize(&h).unwrap(), before);
    assert!(h.site(SiteId(0)).unwrap().members.contains(&a[0]));
}

#[test]
fn death_on_journey() {
    let s = start();
    let (mut h, a, _) = two_site_world(|_, _| false);
    let old = h.add_founder(Sex::Male, years_before(s, 170));
    let departed = h.now();
    let j = h
        .start_journey(vec![old, a[0]], SiteId(0), RegionCoords::new(6, 0), Purpose::Migrate { to: SiteId(1) })
        .unwrap();
    h.advance(CalendarTick(departed.0 + 1));
    assert!(!h.person(old).unwrap().alive());
    assert_eq!(h.journey(j).unwrap().people, vec![a[0]]);
    h.advance(CalendarTick(departed.0 + 7 * leg()));
    assert!(h.site(SiteId(1)).unwrap().members.contains(&a[0]));
    assert!(!h.site(SiteId(1)).unwrap().members.contains(&old));
    assert!(h.journey(j).is_none());
}

#[test]
fn emptied_journey_is_removed_and_leg_is_noop() {
    let s = start();
    let (mut h, _, _) = two_site_world(|_, _| false);
    let old = h.add_founder(Sex::Male, years_before(s, 170));
    let departed = h.now();
    let j = h
        .start_journey(vec![old], SiteId(0), RegionCoords::new(6, 0), Purpose::Migrate { to: SiteId(1) })
        .unwrap();
    h.advance(CalendarTick(departed.0 + 10 * leg()));
    assert!(h.journey(j).is_none());
    assert!(h.pending_journey_legs() == 0);
}

#[test]
fn same_region_journey_arrives_next_step() {
    let (mut h, a, _) = two_site_world(|_, _| false);
    let j = h
        .start_journey(vec![a[0]], SiteId(0), RegionCoords::new(0, 0), Purpose::Migrate { to: SiteId(0) })
        .unwrap();
    h.advance(h.now());
    assert!(h.journey(j).is_none());
    assert!(h.site(SiteId(0)).unwrap().members.contains(&a[0]));
}

// ---- Task 5: migration, embark and founding ----

fn cheb(a: RegionCoords, b: RegionCoords) -> u32 {
    (a.x - b.x).unsigned_abs().max((a.z - b.z).unsigned_abs())
}

/// `n` founders (half women, half men, 20 years old) founding a site at `region`.
fn add_site_with(h: &mut History, civ: CivId, region: RegionCoords, n: usize) -> SiteId {
    let s = start();
    let mut f = Vec::new();
    for i in 0..n {
        f.push(h.add_founder(if i % 2 == 0 { Sex::Female } else { Sex::Male }, years_before(s, 20)));
    }
    h.found_site(civ, region, &f, None, false)
}

#[test]
fn household_of_is_spouse_and_young_children() {
    let s = start();
    let mut h = History::new(2, s, land_grid());
    let civ = h.add_civ(Race::Dwarf);
    let m = h.add_founder(Sex::Female, years_before(s, 40));
    let f = h.add_founder(Sex::Male, years_before(s, 40));
    let kid = h.add_founder(Sex::Male, years_before(s, 5));
    let grown = h.add_founder(Sex::Female, years_before(s, 20));
    let other = h.add_founder(Sex::Female, years_before(s, 30));
    h.people.get_mut(&m).unwrap().spouse = Some(f);
    h.people.get_mut(&f).unwrap().spouse = Some(m);
    for c in [kid, grown] {
        h.people.get_mut(&c).unwrap().mother = Some(m);
        h.people.get_mut(&m).unwrap().children.push(c);
        h.people.get_mut(&f).unwrap().children.push(c);
    }
    h.found_site(civ, RegionCoords::new(1, 1), &[m, f, kid, grown, other], None, false);
    assert_eq!(household_of(&h, m), vec![m, f, kid]);
    assert_eq!(household_of(&h, f), vec![m, f, kid]);
    assert_eq!(household_of(&h, other), vec![other]);
}

#[test]
fn embark_allowed_rules() {
    let s = start();
    let mut h = History::new(2, s, SocietyGrid::from_world_map(&test_map(12, 3, |x, _| x == 11)));
    let civ = h.add_civ(Race::Dwarf);
    let p = h.add_founder(Sex::Female, years_before(s, 20));
    let site = h.found_site(civ, RegionCoords::new(0, 0), &[p], None, false);
    assert_eq!(h.embark_allowed(RegionCoords::new(-1, 0)), Err(EmbarkRefusal::OffMap));
    assert_eq!(h.embark_allowed(RegionCoords::new(11, 0)), Err(EmbarkRefusal::NotHabitable));
    assert_eq!(h.embark_allowed(RegionCoords::new(0, 0)), Err(EmbarkRefusal::Occupied));
    assert_eq!(h.embark_allowed(RegionCoords::new(3, 0)), Err(EmbarkRefusal::TooClose { site }));
    assert_eq!(h.embark_allowed(RegionCoords::new(4, 0)), Ok(()));
}

#[test]
fn crowded_site_sends_migrants() {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&test_map(8, 3, |_, _| false)));
    let civ = h.add_civ(Race::Dwarf);
    let a = add_site_with(&mut h, civ, RegionCoords::new(0, 0), 50);
    let b = add_site_with(&mut h, civ, RegionCoords::new(6, 0), 2);
    assert_eq!(h.site(a).unwrap().capacity, 50);
    h.advance(s.plus_days(2 * DAYS_PER_YEAR));
    assert!(h
        .log()
        .iter()
        .any(|e| matches!(&e.kind, LogKind::Departed { from, purpose: Purpose::Migrate { to }, .. } if *from == a && *to == b)));
    assert!(h.log().iter().any(|e| matches!(&e.kind, LogKind::Arrived { site, .. } if *site == b)));
}

#[test]
fn crowded_civ_embarks_and_founds() {
    let s = start();
    let mut h = History::new(9, s, SocietyGrid::from_world_map(&test_map(30, 30, |_, _| false)));
    let civ = h.add_civ(Race::Dwarf);
    let a = add_site_with(&mut h, civ, RegionCoords::new(15, 15), 50);
    h.advance(s.plus_days(5 * DAYS_PER_YEAR));
    let found = h.log().iter().find_map(|e| match &e.kind {
        LogKind::SiteFounded { site, founders, from: Some(f), .. } if *f == a => Some((*site, founders.len())),
        _ => None,
    });
    let (new, n) = found.expect("an embark party founded a site");
    assert!(n >= 7, "{n}");
    assert!(cheb(h.site(new).unwrap().region, RegionCoords::new(15, 15)) >= 4);
    assert_eq!(h.site(new).unwrap().from, Some(a));
}

#[test]
fn crowded_site_with_no_target_stays() {
    let s = start();
    let sea = |x: i32, z: i32| !(1..=5).contains(&x) || !(1..=5).contains(&z);
    let mut h = History::new(9, s, SocietyGrid::from_world_map(&test_map(7, 7, sea)));
    let civ = h.add_civ(Race::Dwarf);
    add_site_with(&mut h, civ, RegionCoords::new(3, 3), 50);
    h.advance(s.plus_days(5 * DAYS_PER_YEAR));
    assert!(!h.log().iter().any(|e| matches!(e.kind, LogKind::Departed { .. })));
    assert_eq!(h.sites().count(), 1);
}

#[test]
fn extinct_civ_leaves_no_live_steps() {
    let s = start();
    let mut h = History::new(4, s, land_grid());
    let civ = h.add_civ(Race::Dwarf);
    let a = h.add_founder(Sex::Female, years_before(s, 159));
    let b = h.add_founder(Sex::Male, years_before(s, 159));
    h.found_site(civ, RegionCoords::new(2, 2), &[a, b], None, false);
    h.advance(s.plus_days(3 * DAYS_PER_YEAR));
    assert!(h.people().all(|p| !p.alive()));
    assert!(h.sites().all(|s| s.abandoned.is_some()));
    assert_eq!(h.pending_journey_legs(), 0);
    assert_eq!(h.pending_site_months(SiteId(0)), 0);
    assert_eq!(h.pending_deaths_of_dead(), 0);
}

#[test]
fn last_household_leaving_abandons_site() {
    let (mut h, _, b) = two_site_world(|_, _| false);
    h.start_journey(b.clone(), SiteId(1), RegionCoords::new(0, 0), Purpose::Migrate { to: SiteId(0) }).unwrap();
    assert!(h.site(SiteId(1)).unwrap().abandoned.is_some());
    assert!(h.log().iter().any(|e| matches!(e.kind, LogKind::SiteAbandoned { site } if site == SiteId(1))));
    h.advance(h.now().plus_days(2 * DAYS_PER_MONTH));
    assert_eq!(h.pending_site_months(SiteId(1)), 0);
}

#[test]
fn migrants_revive_abandoned_site() {
    let (mut h, a, b) = two_site_world(|_, _| false);
    h.start_journey(b, SiteId(1), RegionCoords::new(0, 0), Purpose::Migrate { to: SiteId(0) }).unwrap();
    assert!(h.site(SiteId(1)).unwrap().abandoned.is_some());
    h.start_journey(vec![a[0]], SiteId(0), RegionCoords::new(6, 0), Purpose::Migrate { to: SiteId(1) })
        .unwrap();
    h.advance(CalendarTick(h.now().0 + 8 * leg()));
    let site = h.site(SiteId(1)).unwrap();
    assert!(site.abandoned.is_none());
    assert!(site.members.contains(&a[0]));
    h.advance(h.now().plus_days(2 * DAYS_PER_MONTH));
    assert_eq!(h.pending_site_months(SiteId(1)), 1);
}

#[test]
fn embark_party_with_nowhere_to_go_wanders() {
    let s = start();
    // A 1-tile-wide strip, 5 long: only x = 4 is far enough from the origin at x = 0.
    let mut h = History::new(11, s, SocietyGrid::from_world_map(&test_map(5, 1, |_, _| false)));
    let civ = h.add_civ(Race::Dwarf);
    let origin = add_site_with(&mut h, civ, RegionCoords::new(0, 0), 50);
    let mut guard = 0;
    while h.journeys.is_empty() {
        h.advance(CalendarTick(h.now().0 + 10));
        guard += 1;
        assert!(guard < 2_000_000, "no embark party left");
    }
    let (jid, party) = {
        let j = h.journeys.values().next().unwrap();
        assert!(matches!(j.purpose, Purpose::Embark { .. }));
        assert_eq!(*j.route.last().unwrap(), RegionCoords::new(4, 0));
        (j.id, j.people.clone())
    };
    // The target gets taken by another civ, and the origin dies out.
    let other = h.add_civ(Race::Dwarf);
    let o = h.add_founder(Sex::Female, years_before(s, 20));
    h.found_site(other, RegionCoords::new(4, 0), &[o], None, false);
    for p in h.living_members(origin) {
        h.record_death(p, DeathCause::OldAge);
    }
    assert!(h.site(origin).unwrap().abandoned.is_some());
    h.advance(CalendarTick(h.now().0 + 10 * leg()));
    assert!(h.journey(jid).is_none());
    for p in &party {
        let pr = h.person(*p).unwrap();
        assert_eq!(pr.location, Location::Wandering(RegionCoords::new(4, 0)));
        assert_eq!(pr.site, None);
    }
    assert_eq!(h.pending_journey_legs(), 0);
}
