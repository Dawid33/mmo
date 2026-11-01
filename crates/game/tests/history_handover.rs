//! Plan 2 hand-over: what a region spawns when it starts, what happens to people when it
//! parks or someone crosses a seam, and which outcomes become region directives. Pure
//! `History` — the world manager is played by calling the API directly.

use game::history::log::LogKind;
use game::history::rules::*;
use game::history::*;
use game::{Biome, GameEventKind, Goal, PersonSpawn, RegionCoords, SpawnAt, TileSummary, WorldMap, REGION_BLOCKS};

const LANDING: [u32; 2] = [20, 30];

fn rc(x: i32, z: i32) -> RegionCoords {
    RegionCoords::new(x, z)
}

/// `width` × `height` grassland regions, landing at [`LANDING`].
fn land_map(width: u32, height: u32) -> WorldMap {
    let tile = TileSummary {
        ground_min_m: 0.0,
        ground_mean_m: 10.0,
        ground_max_m: 50.0,
        relief_m: 50.0,
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

fn start() -> CalendarTick {
    CalendarTick::from_days(HISTORY_START_YEAR * DAYS_PER_YEAR)
}

fn years_before(t: CalendarTick, y: u64) -> CalendarTick {
    CalendarTick(t.0 - y * DAYS_PER_YEAR * TICKS_PER_DAY)
}

fn leg() -> u64 {
    walk_ticks_per_region()
}

fn day() -> u64 {
    TICKS_PER_DAY
}

/// Site 0 at (0, 0) with 3 women, site 1 at (6, 0) with 2 men, on an 8 × 3 land map.
fn two_sites() -> (History, Vec<PersonId>, Vec<PersonId>) {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&land_map(8, 3)));
    let civ = h.add_civ(Race::Dwarf);
    let a: Vec<_> = (0..3).map(|_| h.add_founder(Sex::Female, years_before(s, 20))).collect();
    let b: Vec<_> = (0..2).map(|_| h.add_founder(Sex::Male, years_before(s, 20))).collect();
    h.found_site(civ, rc(0, 0), &a, None, false);
    h.found_site(civ, rc(6, 0), &b, None, false);
    (h, a, b)
}

/// A 170-year-old (dies on the next `advance`) made a member of `site`.
fn old_member_of(h: &mut History, site: SiteId) -> PersonId {
    let old = h.add_founder(Sex::Male, years_before(start(), 170));
    h.sites.get_mut(&site).unwrap().members.insert(old);
    let p = h.people.get_mut(&old).unwrap();
    p.site = Some(site);
    p.location = Location::AtSite(site);
    old
}

fn spawn_of(events: &[GameEventKind], person: PersonId) -> Option<PersonSpawn> {
    events.iter().find_map(|e| match e {
        GameEventKind::SpawnPerson(s) if s.person == person => Some(*s),
        _ => None,
    })
}

fn centre(region: RegionCoords) -> Goal {
    let c = (REGION_BLOCKS / 2) as i32;
    Goal { region, column: [c, c], y: None }
}

/// `a[0]` leaves site 0 (running) for site 1: the journey and its id.
fn walking_out_of_running_site() -> (History, Vec<PersonId>, JourneyId) {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(0, 0));
    let j = h.start_journey(vec![a[0]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    (h, a, j)
}

#[test]
fn catch_up_spawns_site_people() {
    let (mut h, a, b) = two_sites();
    let events = h.region_started(rc(0, 0));
    assert_eq!(events.len(), a.len());
    for p in &a {
        let s = spawn_of(&events, *p).expect("every member spawns");
        assert_eq!(s.walk_to, None);
        let SpawnAt::Column([x, z]) = s.at else { panic!("site people land on a column: {:?}", s.at) };
        assert!(x.abs_diff(LANDING[0]) <= 4 && z.abs_diff(LANDING[1]) <= 4, "{x},{z}");
        let pr = h.person(*p).unwrap();
        assert_eq!(pr.location, Location::InRegion(rc(0, 0)));
        assert_eq!(pr.home_location, Location::AtSite(SiteId(0)));
        // Ruling P2-C: an embodied citizen at their own site is still a member.
        assert!(h.site(SiteId(0)).unwrap().members.contains(p));
    }
    for p in &b {
        assert_eq!(h.person(*p).unwrap().location, Location::AtSite(SiteId(1)));
    }
    // Starting it again spawns nobody twice.
    assert!(h.region_started(rc(0, 0)).is_empty());
}

#[test]
fn catch_up_spawns_traveller_on_route() {
    let (mut h, a, _) = two_sites();
    let departed = h.now();
    let j = h.start_journey(vec![a[0], a[1]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    h.advance(CalendarTick(departed.0 + 3 * leg() + leg() / 2));
    assert_eq!(h.journey_region_at(j, h.now()), Some(rc(3, 0)));
    let events = h.region_started(rc(3, 0));
    assert_eq!(events.len(), 2);
    for p in [a[0], a[1]] {
        let s = spawn_of(&events, p).unwrap();
        let SpawnAt::Edge { from, along_per_mille } = s.at else { panic!("{:?}", s.at) };
        assert_eq!(from, rc(2, 0));
        assert!(along_per_mille <= 1000);
        assert_eq!(s.walk_to, Some(centre(rc(4, 0))));
        assert_eq!(h.person(p).unwrap().location, Location::InRegion(rc(3, 0)));
        assert_eq!(h.person(p).unwrap().home_location, Location::Journey(j));
    }
    assert_eq!(h.journey(j).unwrap().held_in, Some(rc(3, 0)));
    assert_eq!(h.journey(j).unwrap().leg, 3);
    // Site 0's region is not running: its member stays abstract.
    assert_eq!(h.person(a[2]).unwrap().location, Location::AtSite(SiteId(0)));
}

#[test]
fn catch_up_at_destination_walks_to_landing() {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(6, 0));
    let departed = h.now();
    let j = h.start_journey(vec![a[0]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    h.advance(CalendarTick(departed.0 + 6 * leg()));
    // The leg into the running destination spawns the party at the edge and arrives at once.
    let d = h.take_directives();
    let spawn = d
        .iter()
        .find_map(|d| match &d.event {
            GameEventKind::SpawnPerson(s) if d.region == rc(6, 0) => Some(*s),
            _ => None,
        })
        .expect("the arriving migrant spawns");
    assert!(matches!(spawn.at, SpawnAt::Edge { from, .. } if from == rc(5, 0)), "{:?}", spawn.at);
    assert_eq!(
        spawn.walk_to,
        Some(Goal { region: rc(6, 0), column: [LANDING[0] as i32, LANDING[1] as i32], y: None })
    );
    assert!(h.journey(j).is_none());
    let p = h.person(a[0]).unwrap();
    assert_eq!(p.location, Location::InRegion(rc(6, 0)));
    assert_eq!(p.home_location, Location::AtSite(SiteId(1)));
    assert!(h.site(SiteId(1)).unwrap().members.contains(&a[0]));
}

#[test]
fn death_in_running_region_despawns() {
    let (mut h, _, _) = two_sites();
    let old = old_member_of(&mut h, SiteId(0));
    h.region_started(rc(0, 0));
    h.advance(CalendarTick(h.now().0 + 1));
    assert!(!h.person(old).unwrap().alive());
    assert!(!h.site(SiteId(0)).unwrap().members.contains(&old));
    let d = h.take_directives();
    assert_eq!(d, vec![Directive { region: rc(0, 0), event: GameEventKind::DespawnPerson(old) }]);
    assert!(h.take_directives().is_empty());
}

#[test]
fn birth_in_running_region_spawns_near_mother() {
    let s = start();
    let mut h = History::new(3, s, SocietyGrid::from_world_map(&land_map(4, 4)));
    let civ = h.add_civ(Race::Dwarf);
    let m = h.add_founder(Sex::Female, years_before(s, 20));
    let f = h.add_founder(Sex::Male, years_before(s, 20));
    h.found_site(civ, rc(1, 1), &[m, f], None, false);
    h.people.get_mut(&m).unwrap().spouse = Some(f);
    h.people.get_mut(&f).unwrap().spouse = Some(m);
    h.region_started(rc(1, 1));
    h.advance(s.plus_days(20 * DAYS_PER_YEAR));
    let child = h
        .log()
        .iter()
        .find_map(|e| match e.kind {
            LogKind::Born { person, .. } => Some(person),
            _ => None,
        })
        .expect("a child in 20 years");
    let d = h.take_directives();
    let spawn = d
        .iter()
        .find(|d| matches!(&d.event, GameEventKind::SpawnPerson(s) if s.person == child))
        .expect("the child spawns");
    assert_eq!(spawn.region, rc(1, 1));
    assert_eq!(
        spawn.event,
        GameEventKind::SpawnPerson(PersonSpawn { person: child, at: SpawnAt::NearPerson(m), walk_to: None })
    );
    let c = h.person(child).unwrap();
    assert_eq!(c.location, Location::InRegion(rc(1, 1)));
    assert_eq!(c.home_location, Location::AtSite(SiteId(0)));
    assert!(h.site(SiteId(0)).unwrap().members.contains(&child));
}

#[test]
fn departure_in_running_region_walks_out() {
    let (mut h, a, j) = walking_out_of_running_site();
    let d = h.take_directives();
    assert_eq!(d, vec![Directive { region: rc(0, 0), event: GameEventKind::WalkOut(a[0], rc(1, 0)) }]);
    let jr = h.journey(j).unwrap();
    assert_eq!(jr.held_in, Some(rc(0, 0)));
    assert_eq!(h.pending_journey_legs(), 0);
    let p = h.person(a[0]).unwrap();
    assert_eq!(p.location, Location::InRegion(rc(0, 0)));
    assert_eq!(p.home_location, Location::Journey(j));
    assert!(!h.site(SiteId(0)).unwrap().members.contains(&a[0]));
    assert_eq!(h.civ_population(CivId(0)), 5);
}

#[test]
fn crossing_into_unloaded_region_is_absorbed() {
    let (mut h, a, j) = walking_out_of_running_site();
    h.take_directives();
    let crossed = h.now();
    assert!(!h.person_crossed(a[0], rc(1, 0)));
    let jr = h.journey(j).unwrap();
    assert_eq!(jr.held_in, None);
    assert_eq!(jr.leg, 1);
    assert_eq!(h.person(a[0]).unwrap().location, Location::Journey(j));
    assert_eq!(h.pending_journey_legs(), 1);
    // A leg to cross (1, 0), then four more to site 1.
    h.advance(CalendarTick(crossed.0 + leg() - 1));
    assert_eq!(h.journey(j).unwrap().leg, 1);
    h.advance(CalendarTick(crossed.0 + 5 * leg()));
    assert!(h.journey(j).is_none());
    assert_eq!(h.person(a[0]).unwrap().location, Location::AtSite(SiteId(1)));
    assert!(h.take_directives().is_empty());
}

#[test]
fn crossing_into_running_region_is_delivered() {
    let (mut h, a, j) = walking_out_of_running_site();
    h.region_started(rc(1, 0));
    h.take_directives();
    assert!(h.person_crossed(a[0], rc(1, 0)));
    let p = h.person(a[0]).unwrap();
    assert_eq!(p.location, Location::InRegion(rc(1, 0)));
    assert_eq!(p.home_location, Location::Journey(j));
    let jr = h.journey(j).unwrap();
    assert_eq!((jr.leg, jr.held_in), (1, Some(rc(1, 0))));
    assert_eq!(h.pending_journey_legs(), 0);
    assert_eq!(h.take_directives(), vec![Directive { region: rc(1, 0), event: GameEventKind::WalkOut(a[0], rc(2, 0)) }]);
}

#[test]
fn step_for_stopping_region_is_deferred() {
    let (mut h, a, _) = two_sites();
    let old = old_member_of(&mut h, SiteId(0));
    h.region_started(rc(0, 0));
    h.region_stopping(rc(0, 0));
    let t = h.now();
    h.advance(CalendarTick(t.0 + 1));
    assert!(h.person(old).unwrap().alive(), "a death in a stopping region waits");
    assert!(h.take_directives().is_empty());
    let parked: Vec<(PersonId, [f32; 3])> = a.iter().chain([&old]).map(|p| (*p, [0.0; 3])).collect();
    h.region_parked(rc(0, 0), parked);
    assert_eq!(h.person(old).unwrap().location, Location::AtSite(SiteId(0)));
    h.advance(CalendarTick(t.0 + day() + 1));
    assert!(!h.person(old).unwrap().alive());
    assert!(h.take_directives().is_empty(), "nobody is embodied any more");
}

#[test]
fn parked_people_return_to_site() {
    let (mut h, a, _) = two_sites();
    let departed = h.now();
    let j = h.start_journey(vec![a[0]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    h.advance(CalendarTick(departed.0 + leg() / 2));
    h.region_started(rc(0, 0));
    assert_eq!(h.journey(j).unwrap().held_in, Some(rc(0, 0)));
    h.region_stopping(rc(0, 0));
    // a[2] is "lost" (not listed): restored all the same.
    h.region_parked(rc(0, 0), vec![(a[0], [1.0, 2.0, 3.0]), (a[1], [0.0; 3])]);
    for p in [a[1], a[2]] {
        let pr = h.person(p).unwrap();
        assert_eq!(pr.location, Location::AtSite(SiteId(0)));
        assert_eq!(pr.home_location, Location::Unplaced);
    }
    assert_eq!(h.person(a[0]).unwrap().location, Location::Journey(j));
    assert_eq!(h.journey(j).unwrap().held_in, None);
    // The walk out of (0, 0) resumes, half a leg left.
    let parked = h.now();
    h.advance(CalendarTick(parked.0 + leg() / 2 - 1));
    assert_eq!(h.journey(j).unwrap().leg, 0);
    h.advance(CalendarTick(parked.0 + leg() / 2));
    assert_eq!(h.journey(j).unwrap().leg, 1);
    assert!(h.take_directives().is_empty());
}

#[test]
fn off_route_crossing_reroutes() {
    let (mut h, a, j) = walking_out_of_running_site();
    h.take_directives();
    assert!(!h.journey(j).unwrap().route.contains(&rc(0, 1)));
    assert!(!h.person_crossed(a[0], rc(0, 1)));
    let jr = h.journey(j).unwrap();
    assert_eq!(jr.route[0], rc(0, 1));
    assert_eq!(*jr.route.last().unwrap(), rc(6, 0));
    assert_eq!((jr.leg, jr.held_in), (0, None));
    assert_eq!(h.person(a[0]).unwrap().location, Location::Journey(j));
    h.advance(CalendarTick(h.now().0 + 8 * leg()));
    assert_eq!(h.person(a[0]).unwrap().location, Location::AtSite(SiteId(1)));
}

#[test]
fn wandering_citizen_is_walked_home() {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(0, 0));
    assert!(!h.person_crossed(a[0], rc(1, 0)));
    let Location::Journey(j) = h.person(a[0]).unwrap().location else { panic!() };
    let jr = h.journey(j).unwrap();
    assert_eq!(jr.purpose, Purpose::Migrate { to: SiteId(0) });
    assert_eq!(jr.route, vec![rc(1, 0), rc(0, 0)]);
    assert!(!h.site(SiteId(0)).unwrap().members.contains(&a[0]));
    // Home is running: the leg back spawns them at its edge.
    h.advance(CalendarTick(h.now().0 + leg()));
    assert!(h.journey(j).is_none());
    let d = h.take_directives();
    assert!(d.iter().any(|d| d.region == rc(0, 0)
        && matches!(&d.event, GameEventKind::SpawnPerson(s) if s.person == a[0]
            && matches!(s.at, SpawnAt::Edge { from, .. } if from == rc(1, 0)))));
    let p = h.person(a[0]).unwrap();
    assert_eq!((p.location, p.home_location), (Location::InRegion(rc(0, 0)), Location::AtSite(SiteId(0))));
    assert!(h.site(SiteId(0)).unwrap().members.contains(&a[0]));
}

#[test]
fn held_journey_does_not_advance_abstractly() {
    let (mut h, a, _) = two_sites();
    let departed = h.now();
    let j = h.start_journey(vec![a[0]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    h.advance(CalendarTick(departed.0 + 2 * leg() + 1));
    h.region_started(rc(2, 0));
    let before = h.journey(j).unwrap().clone();
    h.advance(CalendarTick(h.now().0 + 20 * leg()));
    assert_eq!(h.journey(j).unwrap(), &before);
    assert_eq!(h.person(a[0]).unwrap().location, Location::InRegion(rc(2, 0)));
    assert_eq!(h.pending_journey_legs(), 0);
}

#[test]
fn diagonal_entry_spawns_from_orthogonal_neighbour() {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&land_map(6, 6)));
    let civ = h.add_civ(Race::Dwarf);
    let a = h.add_founder(Sex::Female, years_before(s, 20));
    let b = h.add_founder(Sex::Male, years_before(s, 20));
    h.found_site(civ, rc(0, 0), &[a], None, false);
    h.found_site(civ, rc(5, 5), &[b], None, false);
    let j = h.start_journey(vec![a], SiteId(0), rc(5, 5), Purpose::Migrate { to: SiteId(1) }).unwrap();
    assert_eq!(h.journey(j).unwrap().route[1], rc(1, 1));
    h.region_started(rc(1, 1));
    h.advance(CalendarTick(h.now().0 + 2 * leg()));
    let d = h.take_directives();
    let spawn = d
        .iter()
        .find_map(|d| match &d.event {
            GameEventKind::SpawnPerson(s) => Some(*s),
            _ => None,
        })
        .unwrap();
    // Entered (1, 1) from (0, 0): spawned on the x-side edge, facing (0, 1).
    assert!(matches!(spawn.at, SpawnAt::Edge { from, .. } if from == rc(0, 1)));
}

#[test]
fn load_normalises_embodied_state() {
    assert_eq!(persist::HISTORY_FILE_VERSION, 2);
    let (h, a, j) = walking_out_of_running_site();
    assert_eq!(h.journey(j).unwrap().held_in, Some(rc(0, 0)));
    assert_eq!(h.pending_journey_legs(), 0);
    let back = persist::decode(&persist::encode(&h), SocietyGrid::from_world_map(&land_map(8, 3))).unwrap();
    assert!(back.people().all(|p| !matches!(p.location, Location::InRegion(_))));
    assert!(back.people().all(|p| p.home_location == Location::Unplaced));
    assert_eq!(back.journey(j).unwrap().held_in, None);
    assert_eq!(back.pending_journey_legs(), 1);
    assert_eq!(back.person(a[0]).unwrap().location, Location::Journey(j));
    assert_eq!(back.person(a[1]).unwrap().location, Location::AtSite(SiteId(0)));
}

#[test]
fn held_journey_without_bodies_resumes_on_park() {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(0, 0));
    let j = h.start_journey(vec![a[0], a[1]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    // a[0] walks into unloaded (1, 0) and dies there; a[1] is still a body in (0, 0).
    assert!(!h.person_crossed(a[0], rc(1, 0)));
    h.record_death(a[0], DeathCause::OldAge);
    // The leg into running (2, 0) has nobody to walk in: the journey is not held there.
    h.region_started(rc(2, 0));
    h.advance(CalendarTick(h.now().0 + leg()));
    let jr = h.journey(j).unwrap();
    assert_eq!((jr.leg, jr.held_in), (2, None));
    h.region_stopping(rc(0, 0));
    h.region_parked(rc(0, 0), vec![(a[1], [0.0; 3])]);
    h.region_stopping(rc(2, 0));
    h.region_parked(rc(2, 0), vec![]);
    h.advance(CalendarTick(h.now().0 + 6 * leg()));
    assert!(h.journey(j).is_none());
    assert_eq!(h.person(a[1]).unwrap().location, Location::AtSite(SiteId(1)));
}

#[test]
fn journey_held_where_its_bodies_died_resumes_on_park() {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(0, 0));
    h.region_started(rc(1, 0));
    let j = h.start_journey(vec![a[0], a[1]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    assert!(h.person_crossed(a[0], rc(1, 0)));
    assert_eq!(h.journey(j).unwrap().held_in, Some(rc(1, 0)));
    h.record_death(a[0], DeathCause::OldAge);
    h.region_stopping(rc(1, 0));
    h.region_parked(rc(1, 0), vec![]);
    assert_eq!(h.journey(j).unwrap().held_in, None);
    assert_eq!(h.pending_journey_legs(), 1);
    h.region_stopping(rc(0, 0));
    h.region_parked(rc(0, 0), vec![(a[1], [0.0; 3]), (a[2], [0.0; 3])]);
    h.advance(CalendarTick(h.now().0 + 6 * leg()));
    assert!(h.journey(j).is_none());
    assert_eq!(h.person(a[1]).unwrap().location, Location::AtSite(SiteId(1)));
}

#[test]
fn party_of_two_crossing_one_at_a_time() {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(0, 0));
    let j = h.start_journey(vec![a[0], a[1]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    let t1 = h.now();
    assert!(!h.person_crossed(a[0], rc(1, 0)));
    h.advance(CalendarTick(t1.0 + 5));
    assert!(!h.person_crossed(a[1], rc(1, 0)));
    let jr = h.journey(j).unwrap();
    assert_eq!((jr.leg, jr.held_in), (1, None));
    assert_eq!(jr.people, vec![a[0], a[1]]);
    assert_eq!(h.pending_journey_legs(), 1);
    for p in [a[0], a[1]] {
        assert_eq!(h.person(p).unwrap().location, Location::Journey(j));
    }
    // A crossing back to the (now parked) start does not rewind the journey.
    h.region_stopping(rc(0, 0));
    h.region_parked(rc(0, 0), vec![]);
    assert!(!h.person_crossed(a[1], rc(0, 0)));
    assert_eq!(h.journey(j).unwrap().leg, 1);
    // Timed from the first crossing, not re-timed by the later ones.
    h.advance(CalendarTick(t1.0 + leg() - 1));
    assert_eq!(h.journey(j).unwrap().leg, 1);
    h.advance(CalendarTick(t1.0 + leg()));
    assert_eq!(h.journey(j).unwrap().leg, 2);
    h.advance(CalendarTick(t1.0 + 5 * leg()));
    assert!(h.journey(j).is_none());
    for p in [a[0], a[1]] {
        assert_eq!(h.person(p).unwrap().location, Location::AtSite(SiteId(1)));
    }
    assert!(h.take_directives().iter().all(|d| d.region == rc(0, 0)));
}

#[test]
fn party_of_two_delivered_one_at_a_time() {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(0, 0));
    h.region_started(rc(1, 0));
    let j = h.start_journey(vec![a[0], a[1]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    h.take_directives();
    assert!(h.person_crossed(a[0], rc(1, 0)));
    assert!(h.person_crossed(a[1], rc(1, 0)));
    let jr = h.journey(j).unwrap();
    assert_eq!((jr.leg, jr.held_in), (1, Some(rc(1, 0))));
    assert_eq!(h.pending_journey_legs(), 0);
    assert_eq!(
        h.take_directives(),
        vec![
            Directive { region: rc(1, 0), event: GameEventKind::WalkOut(a[0], rc(2, 0)) },
            Directive { region: rc(1, 0), event: GameEventKind::WalkOut(a[1], rc(2, 0)) },
        ]
    );
    for p in [a[0], a[1]] {
        let pr = h.person(p).unwrap();
        assert_eq!((pr.location, pr.home_location), (Location::InRegion(rc(1, 0)), Location::Journey(j)));
    }
}

#[test]
fn civ_population_counts_strays() {
    // Sea along x = 7: a citizen who walks into it has no way home and becomes a stray.
    let mut map = land_map(8, 3);
    for (i, t) in map.tiles.iter_mut().enumerate() {
        if i % 8 == 7 {
            t.sea_fraction = 1.0;
            t.landing = None;
        }
    }
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&map));
    let civ = h.add_civ(Race::Dwarf);
    let a: Vec<_> = (0..3).map(|_| h.add_founder(Sex::Female, years_before(s, 20))).collect();
    let b: Vec<_> = (0..2).map(|_| h.add_founder(Sex::Male, years_before(s, 20))).collect();
    h.found_site(civ, rc(0, 0), &a, None, false);
    h.found_site(civ, rc(6, 0), &b, None, false);
    h.region_started(rc(6, 0));
    assert!(!h.person_crossed(b[1], rc(7, 0)));
    let p = h.person(b[1]).unwrap();
    assert_eq!((p.location, p.site), (Location::Wandering(rc(7, 0)), Some(SiteId(1))));
    assert!(!h.site(SiteId(1)).unwrap().members.contains(&b[1]));
    assert_eq!(h.civ_population(civ), 5);
    assert_eq!(h.stats().per_civ[0].1, 5);
    // Still counted after a reload (the stray index is rebuilt).
    let back = persist::decode(&persist::encode(&h), SocietyGrid::from_world_map(&map)).unwrap();
    assert_eq!(back.civ_population(civ), 5);
}
