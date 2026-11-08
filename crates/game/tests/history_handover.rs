//! Plan 2 hand-over: what a region spawns when it starts, what happens to people when it
//! parks or someone crosses a seam, and which outcomes become region directives. Pure
//! `History` — the world manager is played by calling the API directly — then the same
//! hand-over driven by a real `WorldManager` over `InlineSpawner` regions.

use game::history::log::LogKind;
use game::history::rules::*;
use game::history::*;
use game::{Biome, GameEventKind, Goal, PersonSpawn, RegionCoords, SpawnAt, TileSummary, WorldMap, REGION_BLOCKS};

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

pub fn years_before(t: CalendarTick, y: u64) -> CalendarTick {
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
    Goal::local(region, [c, c], None)
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
        Some(Goal::local(rc(6, 0), [LANDING[0] as i32, LANDING[1] as i32], None))
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
    // (Within HELD_TIMEOUT_LEGS of its leg: a longer hold is released.)
    h.advance(CalendarTick(h.now().0 + (HELD_TIMEOUT_LEGS - 1) * leg()));
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
    assert_eq!(persist::HISTORY_FILE_VERSION, 3);
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
fn stuck_walk_out_is_released_after_timeout() {
    let (mut h, a, j) = walking_out_of_running_site();
    h.take_directives();
    let held_at = h.now();
    let timeout = HELD_TIMEOUT_LEGS * leg();
    // The walk out never crosses (no path, the walker gave up): held until the timeout.
    h.advance(CalendarTick(held_at.0 + timeout - 1));
    assert_eq!(h.journey(j).unwrap().held_in, Some(rc(0, 0)));
    assert!(h.take_directives().is_empty());
    h.advance(CalendarTick(held_at.0 + timeout));
    let jr = h.journey(j).unwrap();
    assert_eq!((jr.held_in, jr.leg), (None, 0));
    assert_eq!(h.pending_journey_legs(), 1);
    assert_eq!(
        h.take_directives(),
        vec![Directive { region: rc(0, 0), event: GameEventKind::DespawnPerson(a[0]) }]
    );
    let p = h.person(a[0]).unwrap();
    assert_eq!((p.location, p.home_location), (Location::Journey(j), Location::Unplaced));
    // It goes on abstractly from (0, 0), half a leg left, and arrives.
    let released = h.now();
    h.advance(CalendarTick(released.0 + leg() / 2));
    assert_eq!(h.journey(j).unwrap().leg, 1);
    h.advance(CalendarTick(released.0 + 8 * leg()));
    assert!(h.journey(j).is_none());
    assert_eq!(h.person(a[0]).unwrap().location, Location::AtSite(SiteId(1)));
}

#[test]
fn held_journey_whose_bodies_died_resumes_without_park() {
    let (mut h, a, _) = two_sites();
    h.region_started(rc(0, 0));
    h.region_started(rc(1, 0));
    let j = h.start_journey(vec![a[0], a[1]], SiteId(0), rc(6, 0), Purpose::Migrate { to: SiteId(1) }).unwrap();
    assert!(h.person_crossed(a[0], rc(1, 0)));
    assert_eq!(h.journey(j).unwrap().held_in, Some(rc(1, 0)));
    h.record_death(a[0], DeathCause::OldAge);
    h.take_directives();
    // Nobody of the party is left in (1, 0), which never parks: released at once.
    h.advance(CalendarTick(h.now().0 + 1));
    let jr = h.journey(j).unwrap();
    assert_eq!(jr.held_in, None);
    assert_eq!(h.pending_journey_legs(), 1);
    assert!(h.take_directives().is_empty(), "no body to despawn");
    // a[1] still walks in (0, 0); the journey reaches site 1 without a park.
    h.region_stopping(rc(0, 0));
    h.region_parked(rc(0, 0), vec![(a[1], [0.0; 3])]);
    h.advance(CalendarTick(h.now().0 + 8 * leg()));
    assert!(h.journey(j).is_none());
    assert_eq!(h.person(a[1]).unwrap().location, Location::AtSite(SiteId(1)));
}

#[test]
fn sole_citizen_walking_home_keeps_their_site() {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&land_map(8, 3)));
    let civ = h.add_civ(Race::Dwarf);
    let a = h.add_founder(Sex::Female, years_before(s, 20));
    h.found_site(civ, rc(0, 0), &[a], None, false);
    h.region_started(rc(0, 0));
    // Their body wanders over the seam into unloaded (1, 0): they walk home.
    assert!(!h.person_crossed(a, rc(1, 0)));
    assert!(matches!(h.person(a).unwrap().location, Location::Journey(_)));
    assert_eq!(h.site(SiteId(0)).unwrap().abandoned, None, "a site its citizen walks home to is not abandoned");
    h.advance(CalendarTick(h.now().0 + 4 * leg()));
    // Walked back into the running site region: a body there, at home.
    let p = h.person(a).unwrap();
    assert_eq!((p.location, p.home_location), (Location::InRegion(rc(0, 0)), Location::AtSite(SiteId(0))));
    assert!(h.site(SiteId(0)).unwrap().members.contains(&a));
    assert!(!h.log().iter().any(|e| matches!(e.kind, LogKind::SiteAbandoned { .. })));
}

#[test]
fn sole_citizen_dying_on_the_walk_home_abandons_their_site() {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&land_map(8, 3)));
    let civ = h.add_civ(Race::Dwarf);
    let a = h.add_founder(Sex::Female, years_before(s, 20));
    h.found_site(civ, rc(0, 0), &[a], None, false);
    h.region_started(rc(0, 0));
    assert!(!h.person_crossed(a, rc(1, 0)));
    h.record_death(a, DeathCause::OldAge);
    assert!(h.site(SiteId(0)).unwrap().abandoned.is_some(), "nobody is left to come home");
}

#[test]
fn catch_up_spread_grows_with_the_crowd() {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&land_map(8, 3)));
    let civ = h.add_civ(Race::Dwarf);
    let people: Vec<_> = (0..100).map(|_| h.add_founder(Sex::Female, years_before(s, 20))).collect();
    h.found_site(civ, rc(0, 0), &people, None, false);
    let events = h.region_started(rc(0, 0));
    // ceil(sqrt(100)) * 2 = 20 blocks either way.
    let spread = 20i64;
    let mut widest = 0i64;
    for p in &people {
        let SpawnAt::Column([x, z]) = spawn_of(&events, *p).unwrap().at else { panic!("a column spawn") };
        let d = (i64::from(x) - i64::from(LANDING[0])).abs().max((i64::from(z) - i64::from(LANDING[1])).abs());
        assert!(d <= spread, "{p:?} {d} blocks from the landing");
        widest = widest.max(d);
    }
    assert!(widest > i64::from(SPAWN_SPREAD_BLOCKS), "a crowd spreads wider than {SPAWN_SPREAD_BLOCKS}: {widest}");
}

#[test]
fn site_founded_in_running_region_is_embodied() {
    let s = start();
    let mut h = History::new(5, s, SocietyGrid::from_world_map(&land_map(8, 3)));
    let civ = h.add_civ(Race::Dwarf);
    let founders: Vec<_> = (0..3).map(|_| h.add_founder(Sex::Male, years_before(s, 20))).collect();
    assert!(h.region_started(rc(3, 0)).is_empty());
    let site = h.found_site(civ, rc(3, 0), &founders, None, true);
    let d = h.take_directives();
    assert_eq!(d.len(), founders.len());
    for (f, d) in founders.iter().zip(&d) {
        assert_eq!(d.region, rc(3, 0));
        assert!(matches!(&d.event, GameEventKind::SpawnPerson(sp) if sp.person == *f && sp.walk_to.is_none()));
        let p = h.person(*f).unwrap();
        assert_eq!((p.location, p.home_location), (Location::InRegion(rc(3, 0)), Location::AtSite(site)));
    }
    // Again: nobody is spawned twice.
    h.embody_site(site);
    assert!(h.take_directives().is_empty());
    // Not running: no change.
    let other: Vec<_> = (0..2).map(|_| h.add_founder(Sex::Male, years_before(s, 20))).collect();
    let far = h.found_site(civ, rc(6, 0), &other, None, false);
    h.embody_site(far);
    assert!(h.take_directives().is_empty());
    assert_eq!(h.person(other[0]).unwrap().location, Location::AtSite(far));
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

// ---- the world manager runs the history -------------------------------------

use crate::common;
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, InlineLodBuilder, InlineSpawner, LodBuilder,
    RegionOutput, ServerEvent, ServerPacket, SubChunk, WorldManager, BLOCK_VOXELS, FLIP_HYSTERESIS, REGION_SIZE,
    TICK_RATE, UNLOAD_GRACE_MS,
};
use std::collections::BTreeMap;

/// Install `h` behind an inline coordinator link: lockstep (today's synchronous
/// ordering) unless `schedule` delays the link (`InlineSchedule::coordinator_delay`).
pub fn install_history<S: game::RegionSpawner>(m: &mut WorldManager<S>, h: History, now_ms: u64) {
    install_history_with(m, h, now_ms, None);
}

/// [`install_history`] with the link's delay taken from `schedule`.
pub fn install_history_with<S: game::RegionSpawner>(
    m: &mut WorldManager<S>,
    h: History,
    now_ms: u64,
    schedule: Option<&game::InlineSchedule>,
) {
    let link = game::InlineWorldLink::new(game::Coordinator::new(h, now_ms));
    let link = match schedule {
        Some(s) => link.with_schedule(s),
        None => link,
    };
    m.set_world_link(Box::new(link), now_ms);
}

/// The history of the manager's inline coordinator.
pub fn history_of<S: game::RegionSpawner>(m: &WorldManager<S>) -> &History {
    m.world_link().and_then(|l| l.as_inline()).expect("an inline world link is installed").coordinator().history()
}

/// [`history_of`], to change directly (what it decides goes out at the next pump).
pub fn history_mut_of<S: game::RegionSpawner>(m: &mut WorldManager<S>) -> &mut History {
    m.world_link_mut()
        .and_then(|l| l.as_inline_mut())
        .expect("an inline world link is installed")
        .coordinator_mut()
        .history_mut()
}

/// A one-block floor under the whole of every region.
pub fn floored_region(_rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let air = Block::new(BlockId::AIR);
    common::chunks_over(0.0..REGION_SIZE, 0.0..REGION_SIZE)
        .into_iter()
        .map(|(x, z)| {
            let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(BLOCK_VOXELS as u32)));
            (ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub]))
        })
        .collect()
}

/// Every person with a body, and the running regions holding them.
pub fn bodies(manager: &mut WorldManager<InlineSpawner>) -> BTreeMap<PersonId, Vec<RegionCoords>> {
    let mut out: BTreeMap<PersonId, Vec<RegionCoords>> = BTreeMap::new();
    for r in manager.running_regions() {
        manager.spawner_mut().with_region(r, |region| {
            for (p, _) in region.data().people_here() {
                out.entry(p).or_default().push(r);
            }
        });
    }
    out
}

/// At most one body per person, and the history agrees on where it is: a living person
/// `InRegion(r)` has their one body in `r` and an abstract home; anyone else has none.
pub fn assert_one_place(h: &History, bodies: &BTreeMap<PersonId, Vec<RegionCoords>>, when: &str) {
    for (p, at) in bodies {
        assert!(at.len() <= 1, "{when}: {p:?} has bodies in {at:?}");
        assert!(h.person(*p).is_some_and(|q| q.alive()), "{when}: a body for {p:?}, who is not alive");
    }
    for q in h.people().filter(|q| q.alive()) {
        let at = bodies.get(&q.id).map_or(&[][..], Vec::as_slice);
        match q.location {
            Location::InRegion(r) => {
                assert_eq!(at, [r], "{when}: {:?} is in {r:?}", q.id);
                assert!(
                    !matches!(q.home_location, Location::InRegion(_) | Location::Unplaced),
                    "{when}: embodied {:?} has no abstract place: {:?}",
                    q.id,
                    q.home_location
                );
            }
            loc => {
                assert!(at.is_empty(), "{when}: {:?} is abstract ({loc:?}) but has a body in {at:?}", q.id);
                assert_eq!(q.home_location, Location::Unplaced, "{when}: abstract {:?} keeps a home", q.id);
            }
        }
    }
}

const CLIENT: ClientId = 0;

/// A `WorldManager` over `InlineSpawner` regions with [`floored_region`] ground.
struct World {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    now_ms: u64,
}

impl World {
    fn new(map: WorldMap, history: Option<History>) -> Self {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        let mut manager = WorldManager::new(
            InlineSpawner::default(),
            Box::new(floored_region),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        manager.set_world_map(map);
        if let Some(h) = history {
            install_history(&mut manager, h, 0);
        }
        World { manager, region_out, packets, now_ms: 0 }
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, self.now_ms);
            }
        }
        // Snapshots are big: never keep them.
        self.packets.try_iter().for_each(drop);
    }

    fn packet(&mut self, packet: ClientPacket) {
        self.send(ServerEvent::ClientPacket(packet, CLIENT));
    }

    fn send(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms));
        self.settle();
    }

    /// One round: every region ticks, then the manager's upkeep.
    fn tick(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.settle();
        self.manager.maintain(self.now_ms);
        self.settle();
    }

    /// `ms` of wall time pass with no region tick: the calendar and the unload grace move.
    fn wait(&mut self, ms: u64) {
        self.now_ms += ms;
        self.manager.maintain(self.now_ms);
        self.settle();
    }

    fn history(&self) -> &History {
        history_of(&self.manager)
    }

    fn running(&self, r: RegionCoords) -> bool {
        self.manager.running_regions().contains(&r)
    }

    fn check(&mut self, when: &str) {
        let census = bodies(&mut self.manager);
        assert_one_place(history_of(&self.manager), &census, when);
    }

    /// Move the player's body in `r` to region-local `x`, `z` (height kept).
    fn teleport_player(&mut self, r: RegionCoords, x: f32, z: f32) {
        self.manager.spawner_mut().with_region(r, |region| {
            let key = *region.data().player_entites.get(&CLIENT).expect("the player is here");
            region.with_data(|d| {
                let handle = *d.ecs.rigidbody.get(key);
                let y = d.physics.bodies.get(handle).unwrap().translation().y;
                d.set_body_pose_safe(key, game::Pose::from_translation(game::rapier3d::math::Vector::new(x, y, z)))
            });
        });
    }
}

/// Site 0 at (0, 0): four women and four men; site 1 at (2, 0): two men. On a 3 × 1 map.
fn manager_history() -> (History, Vec<PersonId>, Vec<PersonId>) {
    let s = start();
    let mut h = History::new(11, s, SocietyGrid::from_world_map(&land_map(3, 1)));
    let civ = h.add_civ(Race::Dwarf);
    let a: Vec<_> = (0..8)
        .map(|i| h.add_founder(if i % 2 == 0 { Sex::Female } else { Sex::Male }, years_before(s, 20)))
        .collect();
    let b: Vec<_> = (0..2).map(|_| h.add_founder(Sex::Male, years_before(s, 20))).collect();
    h.found_site(civ, rc(0, 0), &a, None, false);
    h.found_site(civ, rc(2, 0), &b, None, false);
    (h, a, b)
}

/// Site 0's people have bodies in (0, 0), site 1's are abstract at their site.
fn assert_site_0_embodied(w: &mut World, a: &[PersonId], b: &[PersonId]) {
    let census = bodies(&mut w.manager);
    for p in a {
        assert_eq!(census.get(p), Some(&vec![rc(0, 0)]), "{p:?} spawned in the site region");
        let q = w.history().person(*p).unwrap();
        assert_eq!((q.location, q.home_location), (Location::InRegion(rc(0, 0)), Location::AtSite(SiteId(0))));
    }
    for p in b {
        assert!(!census.contains_key(p));
        assert_eq!(w.history().person(*p).unwrap().location, Location::AtSite(SiteId(1)));
    }
    w.check("site 0 embodied");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn site_region_start_spawns_its_people() {
    let (h, a, b) = manager_history();
    // Installed before the region starts: caught up as it starts.
    let mut w = World::new(land_map(3, 1), Some(h.clone()));
    w.send(ServerEvent::connected(CLIENT));
    assert!(w.running(rc(0, 0)));
    assert_site_0_embodied(&mut w, &a, &b);
    // Installed while it runs: caught up at once.
    let mut w = World::new(land_map(3, 1), None);
    w.send(ServerEvent::connected(CLIENT));
    assert!(bodies(&mut w.manager).is_empty());
    install_history(&mut w.manager, h, w.now_ms);
    w.settle();
    assert_site_0_embodied(&mut w, &a, &b);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn neighbour_wake_catches_up() {
    let (h, a, _) = manager_history();
    let mut w = World::new(land_map(3, 1), Some(h));
    w.send(ServerEvent::connected(CLIENT));
    w.packet(ClientPacket::PlayRegion(rc(1, 0)));
    w.tick();
    w.wait(UNLOAD_GRACE_MS);
    assert!(!w.running(rc(0, 0)), "the site region parked");
    for p in &a {
        assert_eq!(w.history().person(*p).unwrap().location, Location::AtSite(SiteId(0)));
    }
    w.check("parked");

    // The player walks over the seam into the site region: the arrival wakes it, and its
    // people are there.
    let z = (LANDING[1] as f32 + 0.5) * BLOCK_VOXELS as f32;
    w.teleport_player(rc(1, 0), -(FLIP_HYSTERESIS + 2.0), z);
    w.tick();
    assert!(w.running(rc(0, 0)), "the player's arrival woke the site region");
    let census = bodies(&mut w.manager);
    for p in &a {
        assert_eq!(census.get(p), Some(&vec![rc(0, 0)]));
    }
    w.check("woken by the player");

    // And back out: once the site region parks, its people are abstract at their site.
    w.teleport_player(rc(0, 0), REGION_SIZE + FLIP_HYSTERESIS + 2.0, z);
    w.tick();
    w.wait(UNLOAD_GRACE_MS);
    assert!(!w.running(rc(0, 0)), "the site region parked again");
    for p in &a {
        let q = w.history().person(*p).unwrap();
        assert_eq!((q.location, q.home_location), (Location::AtSite(SiteId(0)), Location::Unplaced));
    }
    assert!(bodies(&mut w.manager).is_empty());
    w.check("parked again");
}

/// Rounds for a body to walk from the landing out over the +x seam (~45 blocks).
const WALK_OUT_ROUNDS: usize = 800;

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walk_out_does_not_wake_neighbour() {
    let (h, a, _) = manager_history();
    let mut w = World::new(land_map(3, 1), Some(h));
    // The player's home keeps (0, 0) running; nobody asks for (1, 0).
    w.send(ServerEvent::connected(CLIENT));
    let j = history_mut_of(&mut w.manager)
        .start_journey(vec![a[0]], SiteId(0), rc(2, 0), Purpose::Migrate { to: SiteId(1) })
        .expect("a route to site 1");
    let mut absorbed = false;
    for round in 0..WALK_OUT_ROUNDS {
        w.tick();
        assert!(!w.running(rc(1, 0)), "round {round}: the walk out woke (1, 0)");
        w.check(&format!("round {round}"));
        if w.history().person(a[0]).unwrap().location == Location::Journey(j) {
            absorbed = true;
            break;
        }
    }
    assert!(absorbed, "the traveller never walked out of (0, 0)");
    let jr = w.history().journey(j).unwrap();
    assert_eq!((jr.held_in, jr.leg), (None, 1), "the journey goes on abstractly from (1, 0)");
    assert!(!bodies(&mut w.manager).contains_key(&a[0]));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_person_twice_across_park_cycles() {
    let (h, a, b) = manager_history();
    let half_year = DAYS_PER_YEAR * DAY_MS / 2;
    let mut w = World::new(land_map(3, 1), Some(h));
    w.send(ServerEvent::connected(CLIENT));
    // Home in site 1's region, so nothing but a subscription keeps (0, 0) running.
    w.packet(ClientPacket::PlayRegion(rc(2, 0)));
    w.tick();
    w.wait(UNLOAD_GRACE_MS);
    assert!(!w.running(rc(0, 0)));
    w.check("start");
    let mut journey = None;
    for cycle in 0..5 {
        w.packet(ClientPacket::RequestRegionConnection(rc(0, 0)));
        assert!(w.running(rc(0, 0)));
        w.check(&format!("cycle {cycle}: started"));
        // Half a year with the region running: births beside embodied mothers.
        w.wait(half_year);
        w.check(&format!("cycle {cycle}: half a year running"));
        if cycle == 1 {
            journey = history_mut_of(&mut w.manager).start_journey(
                vec![a[0]],
                SiteId(0),
                rc(2, 0),
                Purpose::Migrate { to: SiteId(1) },
            );
            assert!(journey.is_some(), "a route to site 1");
        }
        for round in 0..40 {
            w.tick();
            w.check(&format!("cycle {cycle}: round {round}"));
        }
        w.packet(ClientPacket::ReleaseRegionConnection(rc(0, 0)));
        w.wait(UNLOAD_GRACE_MS);
        assert!(!w.running(rc(0, 0)), "cycle {cycle}: parked");
        w.check(&format!("cycle {cycle}: parked"));
        // Half a year parked: births at the abstract site, the journey goes on.
        w.wait(half_year);
        w.check(&format!("cycle {cycle}: half a year parked"));
    }
    let born = w.history().log().iter().filter(|e| matches!(e.kind, LogKind::Born { .. })).count();
    assert!(born > 0, "children were born over the cycles");
    let j = journey.unwrap();
    assert!(w.history().journey(j).is_none(), "the journey ended");
    let q = w.history().person(a[0]).unwrap();
    assert_eq!(q.site, Some(SiteId(1)), "the traveller arrived at site 1");
    // Site 1's region runs (the player's home): its people, the newcomer too, are there.
    let census = bodies(&mut w.manager);
    for p in b.iter().chain([&a[0]]) {
        assert_eq!(census.get(p), Some(&vec![rc(2, 0)]), "{p:?}");
    }
}

#[test]
fn calendar_follows_now_ms() {
    let (h, _, _) = manager_history();
    let t0 = h.now();
    let mut w = World::new(land_map(3, 1), None);
    assert_eq!(w.manager.calendar_now(0), None);
    install_history(&mut w.manager, h, 5_000);
    assert_eq!(w.manager.calendar_now(5_000), Some(t0));
    assert_eq!(w.manager.calendar_now(5_000 + 7 * TICK_RATE + 3), Some(CalendarTick(t0.0 + 7)));
    w.now_ms = 5_000;
    w.wait(3 * DAY_MS);
    assert_eq!(w.history().now(), t0.plus_days(3));
}

/// The server's exit path: `shutdown_all` parks every region, THEN the history is saved. A
/// history saved that way reloads with nobody embodied and the whole population intact.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn history_saved_on_shutdown_reloads() {
    let (h, a, b) = manager_history();
    let people = h.people().filter(|q| q.alive()).count();
    assert_eq!(people, a.len() + b.len());
    let mut w = World::new(land_map(3, 1), Some(h));
    w.send(ServerEvent::connected(CLIENT));
    w.packet(ClientPacket::PlayRegion(rc(0, 0)));
    w.tick();
    assert!(
        w.history().people().any(|q| matches!(q.location, Location::InRegion(_))),
        "site 0's people are embodied while the region runs"
    );
    w.manager.shutdown_all();
    w.settle();
    assert!(w.manager.running_regions().is_empty(), "every region is parked");
    let bytes = persist::encode(w.history());
    let loaded = persist::decode(&bytes, SocietyGrid::from_world_map(&land_map(3, 1))).unwrap();
    assert!(
        loaded.people().all(|q| !matches!(q.location, Location::InRegion(_))),
        "nobody is left embodied in the saved file"
    );
    assert_eq!(loaded.people().filter(|q| q.alive()).count(), people);
}

/// A region's thread has already handled `Shutdown` (its `PeopleParked` / `Stopped` are on
/// their way) when a departure is routed into it: the arrival waits for the park and is
/// replayed into the woken region, which is never respawned in between — a respawn would
/// be parked by the old thread's outputs and leave bodies the history does not know.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn arrival_into_stopping_region_waits_for_its_park() {
    let (h, a, _) = manager_history();
    let mut w = World::new(land_map(3, 1), Some(h));
    w.send(ServerEvent::connected(CLIENT));
    w.packet(ClientPacket::PlayRegion(rc(1, 0)));
    w.tick();
    // (0, 0) loses its keep-alive: Shutdown is sent and its thread handles it, but the
    // manager has not seen its outputs yet.
    w.now_ms += UNLOAD_GRACE_MS;
    w.manager.maintain(w.now_ms);
    w.manager.spawner_mut().pump();
    assert!(!w.manager.spawner_mut().running().contains(&rc(0, 0)), "the old thread is gone");
    let stop_outputs: Vec<_> = w.region_out.try_iter().collect();
    assert!(stop_outputs.iter().any(|(r, o)| *r == rc(0, 0) && matches!(o, RegionOutput::Stopped(..))));
    // The player walks over the seam into (0, 0): its departure is handled first.
    let z = (LANDING[1] as f32 + 0.5) * BLOCK_VOXELS as f32;
    w.teleport_player(rc(1, 0), -(FLIP_HYSTERESIS + 2.0), z);
    w.now_ms += TICK_RATE;
    w.manager.spawner_mut().tick_all();
    let tick_outputs: Vec<_> = w.region_out.try_iter().collect();
    assert!(tick_outputs.iter().any(|(_, o)| matches!(o, RegionOutput::Departures(d, _) if !d.is_empty())));
    for (r, o) in tick_outputs.into_iter().chain(stop_outputs) {
        w.manager.handle_region_output(r, o, w.now_ms);
    }
    w.settle();
    assert!(w.running(rc(0, 0)), "the arrival woke (0, 0) once it parked");
    let mut player_here = false;
    w.manager.spawner_mut().with_region(rc(0, 0), |r| player_here = r.data().player_entites.contains_key(&CLIENT));
    assert!(player_here, "the player arrived in (0, 0)");
    let census = bodies(&mut w.manager);
    for p in &a {
        assert_eq!(census.get(p), Some(&vec![rc(0, 0)]), "{p:?} has one body in (0, 0)");
    }
    w.check("woken");
    for round in 0..5 {
        w.tick();
        w.check(&format!("round {round}"));
    }
}

/// `PeopleParked` alone hands a parking region's people back (the `Stopped` backstop is
/// for a region that sent none).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn people_parked_hands_people_back_before_stopped() {
    let (h, a, _) = manager_history();
    let mut w = World::new(land_map(3, 1), Some(h));
    w.send(ServerEvent::connected(CLIENT));
    w.packet(ClientPacket::PlayRegion(rc(1, 0)));
    w.tick();
    w.now_ms += UNLOAD_GRACE_MS;
    w.manager.maintain(w.now_ms);
    w.manager.spawner_mut().pump();
    let outputs: Vec<_> = w.region_out.try_iter().filter(|(r, _)| *r == rc(0, 0)).collect();
    let parked = outputs.iter().position(|(_, o)| matches!(o, RegionOutput::PeopleParked(_))).unwrap();
    let stopped = outputs.iter().position(|(_, o)| matches!(o, RegionOutput::Stopped(..))).unwrap();
    assert!(parked < stopped);
    let mut outputs = outputs.into_iter();
    for (r, o) in outputs.by_ref().take(parked + 1) {
        w.manager.handle_region_output(r, o, w.now_ms);
    }
    // Before `Stopped`: everyone is back at their site, nobody embodied.
    for p in &a {
        let q = w.history().person(*p).unwrap();
        assert_eq!((q.location, q.home_location), (Location::AtSite(SiteId(0)), Location::Unplaced), "{p:?}");
    }
    for (r, o) in outputs {
        w.manager.handle_region_output(r, o, w.now_ms);
    }
    w.settle();
    assert!(!w.running(rc(0, 0)));
    for p in &a {
        assert_eq!(w.history().person(*p).unwrap().location, Location::AtSite(SiteId(0)));
    }
    w.check("parked");
}

/// A site founded into a running region (an embark into a played region) has bodies.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn site_founded_in_running_region_has_bodies() {
    let (h, _, _) = manager_history();
    let mut w = World::new(land_map(3, 1), Some(h));
    w.send(ServerEvent::connected(CLIENT));
    w.packet(ClientPacket::PlayRegion(rc(1, 0)));
    w.tick();
    let history = history_mut_of(&mut w.manager);
    let born = years_before(history.now(), 20);
    let founders: Vec<_> = (0..4).map(|_| history.add_founder(Sex::Male, born)).collect();
    let site = history.found_site(CivId(0), rc(1, 0), &founders, None, true);
    w.tick();
    let census = bodies(&mut w.manager);
    for f in &founders {
        assert_eq!(census.get(f), Some(&vec![rc(1, 0)]), "{f:?}");
        let q = w.history().person(*f).unwrap();
        assert_eq!((q.location, q.home_location), (Location::InRegion(rc(1, 0)), Location::AtSite(site)));
    }
    w.check("founded");
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "world link is already installed")]
fn set_history_twice_is_a_bug() {
    let (h, _, _) = manager_history();
    let mut w = World::new(land_map(3, 1), Some(h.clone()));
    install_history(&mut w.manager, h, 0);
}
