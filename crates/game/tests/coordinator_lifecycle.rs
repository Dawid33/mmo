//! R2b Task 5: the host's region lifecycle over a lagging world link. A `WorldManager`
//! over `InlineSpawner` regions talks to its history only through an inline coordinator
//! link whose every output is held back 0..=3 pumps (`InlineSchedule::coordinator_delay`),
//! so starts, parks, crossings and directives race the coordinator's answers. Each test
//! sweeps [`SEEDS`] seeds; a failure names the seed to replay.

use crate::history_handover::{
    assert_one_place, bodies, floored_region, history_mut_of, history_of, install_history_with, land_map, start,
    years_before,
};
use crossbeam::channel::{unbounded, Receiver};
use game::history::*;
use game::{
    ClientId, ClientPacket, InlineLodBuilder, InlineSchedule, InlineSpawner, LodBuilder, RegionCoords, RegionOutput,
    ServerEvent, ServerPacket, WorldManager, WorldMap, REGION_BLOCKS, TICK_RATE, UNLOAD_GRACE_MS,
};

/// Seeds swept per test.
const SEEDS: u64 = 32;
/// Most pumps the link holds each coordinator output back.
const LINK_DELAY: u32 = 3;
/// Rounds that drain the link and let the regions settle.
const DRAIN_ROUNDS: usize = 8;
/// Where people and players land: a few blocks short of the +x seam.
const NEAR_EAST_SEAM: [u32; 2] = [REGION_BLOCKS as u32 - 4, 30];
/// Rounds the walkers of [`person_crossing_under_delay_conserves_people`] walk (they
/// reach the +x seam in about ten).
const WALK_ROUNDS: usize = 40;
/// Rounds of [`crossing_while_the_attach_gathers_keeps_one_body`]: its residents, landed
/// near the +x seam, wander over it in about 260.
const WANDER_ROUNDS: usize = 300;
/// (2, 0) stops within this many rounds of the walk's start, by the seed.
const STOP_WITHIN_ROUNDS: u64 = 16;

const CLIENT: ClientId = 0;

fn rc(x: i32, z: i32) -> RegionCoords {
    RegionCoords::new(x, z)
}

/// Run `f(seed)` for every seed; a failure names the seed.
fn each_seed(test: &str, f: impl Fn(u64) + std::panic::RefUnwindSafe) {
    for seed in 0..SEEDS {
        if let Err(e) = std::panic::catch_unwind(|| f(seed)) {
            let why = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            panic!("{test}: seed {seed} failed (replay with that seed): {why}");
        }
    }
}

/// A 3 × 1 strip landing near each region's +x seam. Site 0 at (1, 0) holds `a` (four
/// women, four men); site 1 at (2, 0) holds `b` (two men). The player's home, (0, 0),
/// holds no site.
fn strip_history() -> (History, Vec<PersonId>, Vec<PersonId>) {
    let s = start();
    let mut h = History::new(11, s, SocietyGrid::from_world_map(&strip()));
    let civ = h.add_civ(Race::Dwarf);
    let a: Vec<_> = (0..8)
        .map(|i| h.add_founder(if i % 2 == 0 { Sex::Female } else { Sex::Male }, years_before(s, 20)))
        .collect();
    let b: Vec<_> = (0..2).map(|_| h.add_founder(Sex::Male, years_before(s, 20))).collect();
    h.found_site(civ, rc(1, 0), &a, None, false);
    h.found_site(civ, rc(2, 0), &b, None, false);
    (h, a, b)
}

fn strip() -> WorldMap {
    let mut m = land_map(3, 1);
    for t in &mut m.tiles {
        t.landing = Some(NEAR_EAST_SEAM);
    }
    m
}

/// A `WorldManager` whose world link lags by the seed's schedule.
struct World {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    now_ms: u64,
    seed: u64,
}

impl World {
    fn new(seed: u64, history: History) -> Self {
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
        manager.set_world_map(strip());
        let schedule = InlineSchedule::shuffled(seed).coordinator_delay(LINK_DELAY);
        install_history_with(&mut manager, history, 0, Some(&schedule));
        World { manager, region_out, packets, now_ms: 0, seed }
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, self.now_ms);
            }
        }
        self.packets.try_iter().for_each(drop);
    }

    /// A client event, without settling.
    fn event(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms));
    }

    fn send(&mut self, ev: ServerEvent) {
        self.event(ev);
        self.settle();
    }

    fn packet(&mut self, packet: ClientPacket) {
        self.send(ServerEvent::ClientPacket(packet, CLIENT));
    }

    fn tick(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.settle();
        self.manager.maintain(self.now_ms);
        self.settle();
    }

    /// [`Self::tick`] with the upkeep before the tick's outputs are handled: a region
    /// the upkeep stops is stopping (its `Shutdown` sent, its `Stopped` not seen) while
    /// the departures into it are routed.
    fn tick_upkeep_first(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.manager.maintain(self.now_ms);
        self.settle();
    }

    fn history(&self) -> &History {
        history_of(&self.manager)
    }

    fn running(&self, r: RegionCoords) -> bool {
        self.manager.running_regions().contains(&r)
    }

    /// Coordinator outputs not delivered yet.
    fn in_transit(&self) -> usize {
        self.manager.world_link().and_then(|l| l.as_inline()).expect("an inline link").in_transit()
    }

    /// Ticks until the link is empty and the regions have taken what it brought.
    fn drain(&mut self) {
        for _ in 0..DRAIN_ROUNDS {
            self.tick();
        }
        assert_eq!(self.in_transit(), 0, "seed {}: the link drained", self.seed);
    }

    /// What holds at every moment, however late the coordinator's answers: at most one
    /// body per person, only for the living, and only where the history embodies them.
    /// (A person the history has embodied may still wait for their body: their
    /// `SpawnPeople` is on the link.)
    fn check_lagging(&mut self, when: &str) {
        let census = bodies(&mut self.manager);
        let h = history_of(&self.manager);
        for (p, at) in &census {
            assert_eq!(at.len(), 1, "seed {} {when}: {p:?} has bodies in {at:?}", self.seed);
            let q = h.person(*p).unwrap_or_else(|| panic!("seed {} {when}: a body for unknown {p:?}", self.seed));
            assert_eq!(q.location, Location::InRegion(at[0]), "seed {} {when}: {p:?}'s body", self.seed);
        }
    }

    /// Once the link is drained: the history and the bodies agree exactly.
    fn check_settled(&mut self, when: &str) {
        let census = bodies(&mut self.manager);
        assert_one_place(history_of(&self.manager), &census, &format!("seed {} {when}", self.seed));
    }

    /// A connected player at home in (0, 0), the link drained.
    fn with_player(seed: u64, history: History) -> Self {
        let mut w = World::new(seed, history);
        w.send(ServerEvent::connected(CLIENT));
        w.drain();
        assert!(w.running(rc(0, 0)));
        w
    }
}

/// A roll in `0..n` from the seed and a salt.
fn roll(seed: u64, salt: u64, n: u64) -> u64 {
    let mut z = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 31)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 29)) % n
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn people_spawn_once_per_start_under_delay() {
    each_seed("people_spawn_once_per_start_under_delay", |seed| {
        let (h, a, _) = strip_history();
        let mut w = World::with_player(seed, h);
        for cycle in 0..4u64 {
            // Start the site region; stop it again after a seeded number of rounds — as
            // few as none, while its people are still on the link.
            w.packet(ClientPacket::RequestRegionConnection(rc(1, 0)));
            assert!(w.running(rc(1, 0)));
            w.check_lagging(&format!("cycle {cycle}: started"));
            for round in 0..roll(seed, cycle, 6) {
                w.tick();
                w.check_lagging(&format!("cycle {cycle}: round {round}"));
            }
            w.packet(ClientPacket::ReleaseRegionConnection(rc(1, 0)));
            w.now_ms += UNLOAD_GRACE_MS;
            for round in 0..DRAIN_ROUNDS {
                w.tick();
                w.check_lagging(&format!("cycle {cycle}: stopping, round {round}"));
            }
            assert!(!w.running(rc(1, 0)), "seed {seed} cycle {cycle}: parked");
            w.drain();
            w.check_settled(&format!("cycle {cycle}: parked"));
            let census = bodies(&mut w.manager);
            for p in &a {
                assert!(!census.contains_key(p), "seed {seed} cycle {cycle}: {p:?} kept a body");
                assert_eq!(w.history().person(*p).unwrap().location, Location::AtSite(SiteId(0)));
            }
        }
        // A last start, left running: everyone there once.
        w.packet(ClientPacket::RequestRegionConnection(rc(1, 0)));
        w.drain();
        w.check_settled("running");
        let census = bodies(&mut w.manager);
        for p in &a {
            assert_eq!(census.get(p), Some(&vec![rc(1, 0)]), "seed {seed}: {p:?}");
        }
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn spawn_for_stopped_incarnation_is_returned() {
    let dropped = std::sync::atomic::AtomicU64::new(0);
    each_seed("spawn_for_stopped_incarnation_is_returned", |seed| {
        let (h, a, _) = strip_history();
        let mut w = World::with_player(seed, h);
        // Start the site region and stop everything at once, no settling: its people
        // land before the stop or after it, by the seed.
        w.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc(1, 0)), CLIENT));
        let before = w.manager.world_link_counters().spawns_dropped;
        w.manager.shutdown_all();
        for _ in 0..DRAIN_ROUNDS {
            w.settle();
            w.manager.pump_world(w.now_ms);
        }
        if w.manager.world_link_counters().spawns_dropped > before {
            dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        assert_eq!(w.in_transit(), 0, "seed {seed}: the link drained");
        assert!(w.manager.running_regions().is_empty(), "seed {seed}: every region parked");
        assert!(bodies(&mut w.manager).is_empty(), "seed {seed}: no body");
        for p in &a {
            let q = w.history().person(*p).unwrap();
            assert_eq!(
                (q.location, q.home_location),
                (Location::AtSite(SiteId(0)), Location::Unplaced),
                "seed {seed}: {p:?} is abstract at its site"
            );
        }
    });
    let dropped = dropped.into_inner();
    assert!(dropped > 0, "no seed stopped the region before its people landed");
    assert!(dropped < SEEDS, "no seed landed the people before the stop");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn person_crossing_under_delay_conserves_people() {
    let delivered = std::sync::atomic::AtomicU64::new(0);
    let absorbed = std::sync::atomic::AtomicU64::new(0);
    each_seed("person_crossing_under_delay_conserves_people", |seed| {
        let (h, a, b) = strip_history();
        let population = a.len() + b.len();
        let mut w = World::with_player(seed, h);
        // Home in the site region (1, 0); (2, 0), past its +x seam, runs while subscribed.
        w.packet(ClientPacket::PlayRegion(rc(1, 0)));
        w.packet(ClientPacket::RequestRegionConnection(rc(2, 0)));
        w.drain();
        w.check_settled("start");
        // (2, 0) is released and stops a seeded number of rounds into the walk: walkers
        // reach it running, stopping or parked.
        w.packet(ClientPacket::ReleaseRegionConnection(rc(2, 0)));
        w.now_ms += UNLOAD_GRACE_MS - roll(seed, 1, STOP_WITHIN_ROUNDS) * TICK_RATE;
        let walkers: Vec<PersonId> = a[..4].to_vec();
        history_mut_of(&mut w.manager)
            .start_journey(walkers.clone(), SiteId(0), rc(2, 0), Purpose::Migrate { to: SiteId(1) })
            .expect("a route to site 1");
        // Per walker: seen embodied in (2, 0); seen abstract without that (absorbed at
        // the seam rather than parked with (2, 0)).
        let mut in_east: std::collections::BTreeSet<PersonId> = Default::default();
        let (mut seen_delivered, mut seen_absorbed) = (false, false);
        let east = w.manager.region_incarnation(rc(2, 0)).expect("(2, 0) runs");
        for round in 0..WALK_ROUNDS {
            w.tick_upkeep_first();
            w.check_lagging(&format!("round {round}"));
            // Delivered only into a running region that is not stopping: a walker reaching
            // (2, 0) while it stops is absorbed, never queued for it — a queued arrival
            // would wake it again right after its park (a second incarnation).
            if let Some(now) = w.manager.region_incarnation(rc(2, 0)) {
                assert_eq!(now, east, "seed {seed} round {round}: (2, 0) woke after its park");
            }
            assert_eq!(w.manager.pending_arrival_count(rc(2, 0)), 0, "seed {seed} round {round}: nothing waits for (2, 0)");
            let h = w.history();
            let alive = h.people().filter(|q| q.alive()).count();
            assert_eq!(alive, population, "seed {seed} round {round}: the population");
            for p in &walkers {
                match h.person(*p).unwrap().location {
                    Location::InRegion(r) if r == rc(2, 0) => {
                        in_east.insert(*p);
                        seen_delivered = true;
                    }
                    Location::InRegion(_) => {}
                    _ => seen_absorbed |= !in_east.contains(p),
                }
            }
        }
        if seen_delivered {
            delivered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if seen_absorbed {
            absorbed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        w.now_ms += UNLOAD_GRACE_MS;
        w.drain();
        assert!(!w.running(rc(2, 0)), "seed {seed}: (2, 0) parked");
        w.check_settled("after the walk");
        assert_eq!(w.history().people().filter(|q| q.alive()).count(), population, "seed {seed}: the population");
    });
    let (delivered, absorbed) = (delivered.into_inner(), absorbed.into_inner());
    assert!(delivered > 0, "no seed delivered a walker into running (2, 0)");
    assert!(absorbed > 0, "no seed had a walker absorbed by stopped (2, 0)");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn directive_for_stopping_region_bounces_and_lands_once() {
    let bounced = std::sync::atomic::AtomicU64::new(0);
    each_seed("directive_for_stopping_region_bounces_and_lands_once", |seed| {
        let (h, _, _) = strip_history();
        let mut w = World::with_player(seed, h);
        w.packet(ClientPacket::RequestRegionConnection(rc(1, 0)));
        w.drain();
        w.check_settled("running");
        // Released now: due to stop at the next upkeep past the grace.
        w.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc(1, 0)), CLIENT));
        // A site founded into the running region: its founders' spawns are directives,
        // decided at the next pump and held on the link by the seed.
        let history = history_mut_of(&mut w.manager);
        let born = years_before(history.now(), 20);
        let founders: Vec<PersonId> = vec![history.add_founder(Sex::Male, born)];
        let site = history.found_site(CivId(0), rc(1, 0), &founders, None, true);
        w.manager.pump_world(w.now_ms);
        // The stop: what is still on the link reaches a stopping region and bounces.
        w.now_ms += UNLOAD_GRACE_MS;
        let before = w.manager.world_link_counters().directives_bounced;
        w.manager.maintain(w.now_ms);
        for round in 0..DRAIN_ROUNDS {
            w.tick();
            w.check_lagging(&format!("stopping, round {round}"));
        }
        if w.manager.world_link_counters().directives_bounced > before {
            bounced.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        assert!(!w.running(rc(1, 0)), "seed {seed}: parked");
        w.drain();
        w.check_settled("parked");
        for f in &founders {
            assert_eq!(w.history().person(*f).unwrap().location, Location::AtSite(site), "seed {seed}: {f:?}");
        }
        // Restarted: each founder lands exactly once, bounced or not.
        w.packet(ClientPacket::RequestRegionConnection(rc(1, 0)));
        for round in 0..DRAIN_ROUNDS {
            w.tick();
            w.check_lagging(&format!("restarted, round {round}"));
        }
        w.drain();
        w.check_settled("restarted");
        let census = bodies(&mut w.manager);
        for f in &founders {
            assert_eq!(census.get(f), Some(&vec![rc(1, 0)]), "seed {seed}: {f:?} has one body");
        }
    });
    let bounced = bounced.into_inner();
    assert!(bounced > 0, "no seed held a directive past the stop");
    assert!(bounced < SEEDS, "no seed landed the directives before the stop");
}

/// Whether `out` carries a person's departure over a seam.
fn person_departs(out: &RegionOutput) -> bool {
    matches!(out, RegionOutput::Departures(list, _)
        if list.iter().any(|(b, _)| matches!(b.kind, game::EntityKind::Person { .. })))
}

/// People — residents wandering, or travellers walking out, by the seed — cross a seam
/// while the host gathers its `Attach`: the link comes back (by the seed) right when a
/// departure out of (1, 0) is queued but not yet handled, so
/// (1, 0) lists its people without the walker (its departure was handled first) and
/// (2, 0) may list before the walker's body lands — a crossing in flight is in neither
/// region's own list. (2, 0) runs (delivered) or not (absorbed at the seam) by the seed.
/// The `Attach` must still be exact: never two bodies of one person, and once drained
/// the history and the bodies agree (no spawned body beside a landing one, no body for a
/// person a held crossing absorbed).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn crossing_while_the_attach_gathers_keeps_one_body() {
    let raced = std::sync::atomic::AtomicU64::new(0);
    let raced_absorbed = std::sync::atomic::AtomicU64::new(0);
    each_seed("crossing_while_the_attach_gathers_keeps_one_body", |seed| {
        let (h, a, b) = strip_history();
        let population = a.len() + b.len();
        let mut w = World::with_player(seed, h);
        w.packet(ClientPacket::PlayRegion(rc(1, 0)));
        let east_runs = roll(seed, 2, 4) != 0;
        if east_runs {
            w.packet(ClientPacket::RequestRegionConnection(rc(2, 0)));
        }
        w.drain();
        w.check_settled("start");
        let travellers = roll(seed, 4, 2) == 0;
        if travellers {
            // Travellers walking out; else residents wandering over the seam.
            let walkers: Vec<PersonId> = a[..4].to_vec();
            history_mut_of(&mut w.manager)
                .start_journey(walkers, SiteId(0), rc(2, 0), Purpose::Migrate { to: SiteId(1) })
                .expect("a route to site 1");
        }
        // The walk-outs reach the bodies (the link lags at most `LINK_DELAY` pumps); then
        // the link drops, well before anyone reaches the seam.
        for _ in 0..3 {
            w.tick();
        }
        w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline").set_up(false);
        // The link comes back at the first queued departure (residents cross one by
        // one), or at a later one of the travellers'.
        let up_after = if travellers { roll(seed, 3, 4) } else { 0 };
        let (mut departures, mut up) = (0, false);
        for round in 0..WANDER_ROUNDS {
            w.now_ms += TICK_RATE;
            w.manager.spawner_mut().tick_all();
            // The regions' outputs, queued but not yet handled.
            let mut queued = Vec::new();
            for _ in 0..4 {
                w.manager.spawner_mut().pump();
                queued.extend(w.region_out.try_iter());
            }
            departures += queued.iter().filter(|(_, o)| person_departs(o)).count() as u64;
            if !up && departures > up_after {
                // Back now: the attach is gathered while those departures wait.
                w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline").set_up(true);
                w.manager.maintain(w.now_ms);
                up = true;
                raced.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if !east_runs {
                    raced_absorbed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            for (r, out) in queued {
                w.manager.handle_region_output(r, out, w.now_ms);
            }
            w.settle();
            w.manager.maintain(w.now_ms);
            w.settle();
            for (p, at) in bodies(&mut w.manager) {
                assert_eq!(at.len(), 1, "seed {seed} round {round}: {p:?} has bodies in {at:?}");
            }
        }
        w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline").set_up(true);
        w.drain();
        w.drain();
        w.check_settled("after the walk");
        assert_eq!(w.history().people().filter(|q| q.alive()).count(), population, "seed {seed}: the population");
    });
    assert!(raced.into_inner() > 0, "no seed brought the link back with a departure queued");
    assert!(raced_absorbed.into_inner() > 0, "no seed raced an absorbed crossing");
}

/// A resident of (1, 0) wandered into (2, 0); (by the seed) the coordinator restarts
/// (everyone abstract to it), and (2, 0) is released while the link is down and is still
/// stopping — its `Shutdown` unread for a seeded number of rounds, its
/// bodies standing — when the link comes back. A stopping region is not attached, so
/// the resident is in no list: an `Attach` sent now would let the history take them back
/// and its catch-up spawn them at home beside the body still in (2, 0). The host waits
/// for the park: the person never has two bodies, and once drained the history and the
/// bodies agree.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn resident_in_a_stopping_neighbour_keeps_one_body_across_the_attach() {
    each_seed("resident_in_a_stopping_neighbour_keeps_one_body_across_the_attach", |seed| {
        let (h, a, _) = strip_history();
        let mut w = World::with_player(seed, h);
        w.packet(ClientPacket::PlayRegion(rc(1, 0)));
        w.packet(ClientPacket::RequestRegionConnection(rc(2, 0)));
        w.drain();
        // A resident of site 0 wanders over the +x seam into (2, 0).
        let mut wanderer = None;
        for _ in 0..WANDER_ROUNDS {
            w.tick();
            let census = bodies(&mut w.manager);
            wanderer = a.iter().copied().find(|p| census.get(p).is_some_and(|at| at == &[rc(2, 0)]));
            if wanderer.is_some() {
                break;
            }
        }
        let p = wanderer.unwrap_or_else(|| panic!("seed {seed}: no resident wandered into (2, 0)"));
        w.drain();
        assert_eq!(w.history().person(p).unwrap().home_location, Location::AtSite(SiteId(0)), "seed {seed}");
        // Down; (2, 0) released and stopping, its Shutdown left unread for a while.
        w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline").set_up(false);
        if roll(seed, 6, 2) == 0 {
            // The coordinator restarts meanwhile (from its projection as written, as a new
            // process boot): what it holds of the host stands until the host's attach.
            let link = w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline");
            let (core, history) = link.coordinator().encode_projection();
            let grid = SocietyGrid::from_world_map(&strip());
            let restarted = game::Coordinator::load(&core, &history, grid, game::coordinator::ProcessBoot(1), w.now_ms);
            *link.coordinator_mut() = restarted.expect("the projection loads");
        }
        w.packet(ClientPacket::ReleaseRegionConnection(rc(2, 0)));
        let unread = 6 + roll(seed, 5, 6) as u32;
        w.manager.spawner_mut().hold_inputs(rc(2, 0), unread);
        w.now_ms += UNLOAD_GRACE_MS;
        w.tick();
        assert!(w.running(rc(2, 0)), "seed {seed}: (2, 0) is stopping, not parked");
        // Back while it stops.
        w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline").set_up(true);
        let mut waited = false;
        for round in 0..unread as usize + 3 * DRAIN_ROUNDS {
            w.tick();
            if w.running(rc(2, 0)) {
                // Still stopping: the attach waits for its park.
                assert!(!w.manager.world_attached(), "seed {seed} round {round}: attached while (2, 0) stops");
                waited = true;
            }
            for (q, at) in bodies(&mut w.manager) {
                assert_eq!(at.len(), 1, "seed {seed} round {round}: {q:?} has bodies in {at:?}");
            }
        }
        assert!(waited, "seed {seed}: (2, 0) was still stopping when the link came back");
        assert!(!w.running(rc(2, 0)), "seed {seed}: (2, 0) parked");
        assert!(w.manager.world_attached(), "seed {seed}: attached once it parked");
        w.drain();
        w.check_settled("after the attach");
    });
}

/// An inline coordinator link that records every `Attach` the host sends.
struct AttachTap {
    inner: game::InlineWorldLink,
    attaches: std::sync::Arc<std::sync::Mutex<Vec<game::coordinator::HostMsg>>>,
}

impl game::coordinator::WorldLink for AttachTap {
    fn send(&mut self, msg: game::coordinator::HostMsg) {
        if matches!(msg, game::coordinator::HostMsg::Attach { .. }) {
            self.attaches.lock().unwrap().push(msg.clone());
        }
        self.inner.send(msg)
    }
    fn recv(&mut self) -> Option<game::coordinator::CoordMsg> {
        self.inner.recv()
    }
    fn is_up(&self) -> bool {
        self.inner.is_up()
    }
    fn generation(&self) -> u64 {
        self.inner.generation()
    }
    fn pump(&mut self, now_ms: u64) {
        self.inner.pump(now_ms)
    }
    fn as_inline_mut(&mut self) -> Option<&mut game::InlineWorldLink> {
        Some(&mut self.inner)
    }
    fn as_inline(&self) -> Option<&game::InlineWorldLink> {
        Some(&self.inner)
    }
}

/// A person absorbed at a −x seam while the host gathers its `Attach` is listed in the
/// region they left at the position they left it at — the source-frame position, bit for
/// bit (a rebase into the target's frame and back is not exact in `f32`).
#[test]
fn absorbed_person_is_listed_at_its_source_frame_position() {
    use game::coordinator::HostMsg;
    let (h, a, _) = strip_history();
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let mut manager = crate::common::floor_manager(
        InlineSpawner::default(),
        floored_region,
        out_send,
        region_out_send,
    );
    manager.set_world_map(strip());
    let attaches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let tap = AttachTap { inner: game::InlineWorldLink::new(game::Coordinator::new(h, 0)), attaches: attaches.clone() };
    manager.set_world_link(Box::new(tap), 0, 0);
    let mut w = World { manager, region_out, packets, now_ms: 0, seed: 0 };
    w.send(ServerEvent::connected(CLIENT));
    // (2, 0) runs; (1, 0), west of it, does not.
    w.packet(ClientPacket::RequestRegionConnection(rc(2, 0)));
    w.drain();
    assert!(w.running(rc(2, 0)) && !w.running(rc(1, 0)));
    // The link drops and comes back: the host gathers its attach.
    w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline").set_up(false);
    w.tick();
    w.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("inline").set_up(true);
    attaches.lock().unwrap().clear();
    w.now_ms += TICK_RATE;
    w.manager.maintain(w.now_ms);
    assert!(!w.manager.world_attached(), "gathering");
    // Meanwhile a person (abstract to (2, 0)'s list) leaves (2, 0) over its −x seam.
    let left = [-2.3_f32, 40.0, 17.1];
    let person = a[0];
    let pose = game::Pose::from_translation(game::rapier3d::math::Vector::new(left[0], left[1], left[2]));
    let collider = game::ColliderSpec::Cuboid { half_extents: game::rapier3d::math::Vector::splat(1.0) };
    let bundle = game::EntityBundle {
        id: game::EntityId { birth_region: rc(2, 0), serial: 9_000_000 },
        kind: game::EntityKind::Person { person },
        body: game::body::body_state(pose, &collider, game::rapier3d::math::Vector::ZERO, game::rapier3d::math::Vector::ZERO),
        collider,
        carried: Default::default(),
        client: None,
        pending_inputs: Vec::new(),
    };
    let stamp = { use game::RegionSpawner; w.manager.spawner_mut().world_tick() };
    w.manager.handle_region_output(rc(2, 0), RegionOutput::Departures(vec![(bundle, rc(1, 0))], stamp), w.now_ms);
    w.settle();
    w.manager.maintain(w.now_ms);
    w.settle();
    let sent = attaches.lock().unwrap().clone();
    let listed = sent
        .iter()
        .find_map(|m| match m {
            HostMsg::Attach { running, .. } => running
                .iter()
                .find(|r| r.rc == rc(2, 0))
                .and_then(|r| r.people.iter().find(|(p, _)| *p == person).map(|(_, at)| *at)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the attach lists {person:?} in (2, 0): {sent:?}"));
    assert_eq!(
        listed.map(f32::to_bits),
        left.map(f32::to_bits),
        "listed at {listed:?}, left at {left:?}: the source-frame position, bit for bit"
    );
}
