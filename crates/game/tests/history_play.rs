//! Plan 3: the player's side of the world history — embark, retire, return, the sites /
//! calendar / notice broadcasts and person info — driven through a real `WorldManager`
//! over `InlineSpawner` regions with a bootstrapped history.

use crate::history_handover::{assert_one_place, bodies, floored_region, history_mut_of, history_of, install_history, land_map, years_before};
use crossbeam::channel::{unbounded, Receiver};
use game::history::bootstrap::{bootstrap, BootstrapParams};
use game::history::log::LogKind;
use game::history::persist;
use game::history::rules::EMBARK_PLAYER_PARTY_MAX;
use game::history::*;
use game::{
    Biome, ClientId, ClientPacket, EmbarkRequest, GameEvent, GameEventKind, InlineLodBuilder, InlineSpawner,
    LodBuilder, PersonSpawn, PlayMode, RegionCoords, RegionOutput, ServerEvent, ServerPacket, SiteMarker, SpawnAt, WorldManager,
    WorldMap, SPAWN_REGION, TICK_RATE, UNLOAD_GRACE_MS,
};

fn rc(x: i32, z: i32) -> RegionCoords {
    RegionCoords::new(x, z)
}

/// A 16 × 1 strip of grassland whose first tile — the spawn region (0, 0) — is ocean,
/// so no site is ever there and a connecting player's home never runs one.
fn strip() -> WorldMap {
    let mut m = land_map(16, 1);
    m.tiles[0].biome = Biome::Ocean;
    m
}

/// One civ of ten founders (three couples, four children), no years run.
fn history() -> History {
    let p = BootstrapParams { seed: 7, civs: 1, founders_per_civ: 10, years: 0, ..BootstrapParams::default() };
    bootstrap(&strip(), &p).expect("the strip holds a civ")
}

/// Regions a new site may go in, at least the embark distance apart from each other.
fn free_regions(h: &History) -> Vec<RegionCoords> {
    let mut out: Vec<RegionCoords> = Vec::new();
    for x in 0..16 {
        let r = rc(x, 0);
        if h.embark_allowed(r).is_ok() && out.iter().all(|o| (o.x - x).unsigned_abs() >= 4) {
            out.push(r);
        }
    }
    out
}

/// The world's one bootstrapped site, its civ and its people (by id).
fn home_site(h: &History) -> (SiteId, CivId, Vec<PersonId>) {
    let s = h.sites().next().expect("a site");
    (s.id, s.civ, s.members.iter().copied().collect())
}

const A: ClientId = 0;
const B: ClientId = 1;
/// Player `A` again after a reconnect: client ids are per connection.
const A2: ClientId = 2;

/// A `WorldManager` over `InlineSpawner` regions, keeping every packet except the big
/// region snapshots and the per-tick stream.
struct World {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    inbox: Vec<(Option<ClientId>, ServerPacket)>,
    now_ms: u64,
}

impl World {
    fn new() -> Self {
        Self::over(history(), InlineSpawner::default())
    }

    /// A world over `history` whose regions `spawner` runs (a restarted server:
    /// the saved history and region states).
    fn over(history: History, spawner: InlineSpawner) -> Self {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        let mut manager = WorldManager::new(
            spawner,
            Box::new(floored_region),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        manager.set_world_map(strip());
        install_history(&mut manager, history, 0);
        World { manager, region_out, packets, inbox: Vec::new(), now_ms: 0 }
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, self.now_ms);
            }
        }
        for (to, p) in self.packets.try_iter() {
            if !matches!(
                p,
                ServerPacket::Region(..) | ServerPacket::GameEvent(_) | ServerPacket::SyncClock(..) | ServerPacket::LodData(..)
            ) {
                self.inbox.push((to, p));
            }
        }
    }

    fn send(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms));
        self.settle();
    }

    fn connect(&mut self, c: ClientId) {
        self.send(ServerEvent::connected(c));
    }

    fn packet(&mut self, c: ClientId, packet: ClientPacket) {
        self.send(ServerEvent::ClientPacket(packet, c));
    }

    /// Every packet addressed to `c` since the last call.
    fn take(&mut self, c: ClientId) -> Vec<ServerPacket> {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.inbox).into_iter().partition(|(to, _)| *to == Some(c));
        self.inbox = rest;
        mine.into_iter().map(|(_, p)| p).collect()
    }

    fn embark(&mut self, c: ClientId, req: EmbarkRequest) -> Result<SiteId, EmbarkRefusal> {
        self.packet(c, ClientPacket::Embark(req));
        self.result(c)
    }

    fn return_to(&mut self, c: ClientId, site: SiteId) -> Result<SiteId, EmbarkRefusal> {
        self.packet(c, ClientPacket::ReturnTo(site));
        self.result(c)
    }

    fn adventure(&mut self, c: ClientId, site: SiteId) -> Result<SiteId, AdventureRefusal> {
        self.packet(c, ClientPacket::StartAdventure(site));
        let mut results = Vec::new();
        self.inbox.retain(|(to, p)| match p {
            ServerPacket::AdventureResult(r) if *to == Some(c) => {
                results.push(*r);
                false
            }
            _ => true,
        });
        assert_eq!(results.len(), 1, "one AdventureResult: {results:?}");
        results[0]
    }

    /// The play mode of `c`'s player in `r`, if `r` runs and holds it.
    fn mode_in(&mut self, r: RegionCoords, c: ClientId) -> Option<PlayMode> {
        let mut out = None;
        if self.running(r) {
            self.manager.spawner_mut().with_region(r, |region| out = region.data().clients.get(&c).map(|c| c.mode));
        }
        out
    }

    /// The block column (`[x, z]`) `c`'s body stands over in `r`, if `r` runs and holds it.
    fn body_column(&mut self, r: RegionCoords, c: ClientId) -> Option<[i32; 2]> {
        let mut out = None;
        if self.running(r) {
            self.manager.spawner_mut().with_region(r, |region| {
                let d = region.data();
                let handle = d.player_entites.get(&c).and_then(|&k| *d.ecs.rigidbody.try_get(k));
                if let Some(t) = handle.and_then(|h| d.physics.bodies.get(h)).map(|b| b.position().translation) {
                    let bv = game::BLOCK_VOXELS as f32;
                    out = Some([(t.x / bv).floor() as i32, (t.z / bv).floor() as i32]);
                }
            });
        }
        out
    }

    /// The one `EmbarkResult` sent to `c` since the last call (other packets stay).
    fn result(&mut self, c: ClientId) -> Result<SiteId, EmbarkRefusal> {
        let mut results = Vec::new();
        self.inbox.retain(|(to, p)| match p {
            ServerPacket::EmbarkResult(r) if *to == Some(c) => {
                results.push(*r);
                false
            }
            _ => true,
        });
        assert_eq!(results.len(), 1, "one EmbarkResult: {results:?}");
        results[0]
    }

    fn tick(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.settle();
        self.manager.maintain(self.now_ms);
        self.settle();
    }

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

    /// `c`'s `Client` in `r`, if `r` runs and holds it.
    fn client_in(&mut self, r: RegionCoords, c: ClientId) -> Option<game::Client> {
        let mut out = None;
        if self.running(r) {
            self.manager.spawner_mut().with_region(r, |region| out = region.data().clients.get(&c).cloned());
        }
        out
    }

    /// The player bodies (clients) in `r`, if it runs.
    fn players_in(&mut self, r: RegionCoords) -> Vec<ClientId> {
        let mut out = Vec::new();
        if self.running(r) {
            self.manager.spawner_mut().with_region(r, |region| out.extend(region.data().clients.keys().copied()));
        }
        out
    }

    fn check(&mut self, when: &str) {
        let census = bodies(&mut self.manager);
        assert_one_place(history_of(&self.manager), &census, when);
    }
}

fn player_region(packets: &[ServerPacket]) -> Option<Option<RegionCoords>> {
    packets.iter().rev().find_map(|p| match p {
        ServerPacket::PlayerRegion(r, _) => Some(*r),
        _ => None,
    })
}

fn sites_in(packets: &[ServerPacket]) -> Option<Vec<SiteMarker>> {
    packets.iter().rev().find_map(|p| match p {
        ServerPacket::Sites(s) => Some(s.clone()),
        _ => None,
    })
}

fn notices_in(packets: &[ServerPacket]) -> Vec<String> {
    packets
        .iter()
        .filter_map(|p| match p {
            ServerPacket::Notice(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

/// A site of `civ` the history founds in `region` (an NPC founding, from the bootstrapped
/// site), with `n` new grown founders: the site and its founders.
fn npc_site(w: &mut World, civ: CivId, region: RegionCoords, n: usize) -> (SiteId, Vec<PersonId>) {
    let from = w.history().sites().next().map(|s| s.id);
    let h = history_mut_of(&mut w.manager);
    let born = years_before(h.now(), 20);
    let founders: Vec<PersonId> = (0..n).map(|i| h.add_founder(if i % 2 == 0 { Sex::Female } else { Sex::Male }, born)).collect();
    (h.found_site(civ, region, &founders, from, false), founders)
}

/// A connected world with player `A`; the bootstrapped site, its civ and people, and the
/// free regions.
fn embark_world() -> (World, SiteId, CivId, Vec<PersonId>, Vec<RegionCoords>) {
    let mut w = World::new();
    w.connect(A);
    let (site, civ, people) = home_site(w.history());
    let free = free_regions(w.history());
    assert!(free.len() >= 2, "room for two embarks: {free:?}");
    w.take(A);
    (w, site, civ, people, free)
}

#[test]
fn embark_founds_site_with_chosen_people() {
    let (mut w, home, civ, people, free) = embark_world();
    // The options list the civ's site and every one of its people.
    w.packet(A, ClientPacket::RequestEmbarkOptions);
    let options = w
        .take(A)
        .into_iter()
        .find_map(|p| match p {
            ServerPacket::EmbarkOptions(o) => Some(o),
            _ => None,
        })
        .expect("options sent");
    assert_eq!(options.civs.len(), 1);
    assert_eq!(options.civs[0].civ, civ);
    assert_eq!(options.civs[0].sites.len(), 1);
    let listed: Vec<PersonId> = options.civs[0].sites[0].people.iter().map(|p| p.person).collect();
    assert_eq!(listed, people);

    let chosen = people[..3].to_vec();
    let target = free[0];
    let log_before = w.history().log().len();
    let site = w.embark(A, EmbarkRequest { civ, people: chosen.clone(), region: target }).expect("embark accepted");
    let h = w.history();
    let s = h.site(site).unwrap();
    assert!(s.player_founded);
    assert_eq!((s.region, s.civ, s.from), (target, civ, Some(home)));
    assert_eq!(s.members.iter().copied().collect::<Vec<_>>(), chosen);
    for p in &chosen {
        assert!(!h.site(home).unwrap().members.contains(p), "{p:?} left the old site");
        assert_eq!(h.person(*p).unwrap().site, Some(site));
    }
    let log = &h.log()[log_before..];
    assert!(log.iter().any(|e| matches!(&e.kind, LogKind::Departed { from, people, purpose: Purpose::Embark { .. } }
        if *from == home && *people == chosen)));
    assert!(log.iter().any(|e| matches!(&e.kind, LogKind::SiteFounded { site: s, .. } if *s == site)));
    // The fortress runs, the player and the party are in it.
    assert!(w.running(target));
    let census = bodies(&mut w.manager);
    for p in &chosen {
        assert_eq!(census.get(p), Some(&vec![target]), "{p:?} spawned in the fortress");
    }
    let to_a = w.take(A);
    assert_eq!(player_region(&to_a), Some(Some(target)));
    assert!(notices_in(&to_a).iter().any(|n| n.contains("was founded")), "{to_a:?}");
    w.check("embarked");
    w.tick();
    w.check("a tick later");
}

#[test]
fn embark_refuses_unavailable_people() {
    let (mut w, home, civ, people, free) = embark_world();
    w.connect(B);
    let target = free[0];
    let req = |people: Vec<PersonId>, region| EmbarkRequest { civ, people, region };
    // Region rules first: off the map, uninhabitable, taken.
    assert_eq!(w.embark(A, req(vec![people[0]], rc(40, 0))), Err(EmbarkRefusal::OffMap));
    assert_eq!(w.embark(A, req(vec![people[0]], rc(0, 0))), Err(EmbarkRefusal::NotHabitable));
    let home_region = w.history().site(home).unwrap().region;
    assert_eq!(w.embark(A, req(vec![people[0]], home_region)), Err(EmbarkRefusal::Occupied));
    // Party size.
    assert_eq!(w.embark(A, req(vec![], target)), Err(EmbarkRefusal::PartySize));
    let eight = people[..EMBARK_PLAYER_PARTY_MAX as usize + 1].to_vec();
    assert_eq!(w.embark(A, req(eight, target)), Err(EmbarkRefusal::PartySize));
    // A wrong civ, someone unknown, a duplicate.
    let other_civ = EmbarkRequest { civ: CivId(9), people: vec![people[0]], region: target };
    assert_eq!(w.embark(A, other_civ), Err(EmbarkRefusal::UnavailablePerson(people[0])));
    assert_eq!(w.embark(A, req(vec![people[0], PersonId(999)], target)), Err(EmbarkRefusal::UnavailablePerson(PersonId(999))));
    assert_eq!(w.embark(A, req(vec![people[0], people[0]], target)), Err(EmbarkRefusal::UnavailablePerson(people[0])));
    // People who stopped being available since the options were sent: died, left on a
    // journey, or now embodied in a running region.
    let dead = people[1];
    let traveller = people[2];
    {
        let h = history_mut_of(&mut w.manager);
        h.record_death(dead, DeathCause::OldAge);
        h.start_journey(vec![traveller], home, free[1], Purpose::Embark { civ, from: home }).expect("a route");
    }
    assert_eq!(w.embark(A, req(vec![people[0], dead], target)), Err(EmbarkRefusal::UnavailablePerson(dead)));
    assert_eq!(w.embark(A, req(vec![people[0], traveller], target)), Err(EmbarkRefusal::UnavailablePerson(traveller)));
    let sites_before = w.history().sites().count();
    let log_before = w.history().log().len();
    w.packet(B, ClientPacket::RequestRegionConnection(home_region));
    assert!(w.running(home_region));
    assert_eq!(w.embark(A, req(vec![people[0]], target)), Err(EmbarkRefusal::UnavailablePerson(people[0])));
    // No partial embark: nothing founded, nobody moved, nothing logged.
    let h = w.history();
    assert_eq!(h.sites().count(), sites_before);
    assert_eq!(h.log().len(), log_before);
    assert!(h.site(home).unwrap().members.contains(&people[0]));
    assert_eq!(h.person(people[0]).unwrap().abstract_location(), Location::AtSite(home));
    assert!(!w.running(target));
    // Once the site region parks again its people may embark.
    w.packet(B, ClientPacket::ReleaseRegionConnection(home_region));
    w.wait(UNLOAD_GRACE_MS);
    assert!(!w.running(home_region));
    assert!(w.embark(A, req(vec![people[0]], target)).is_ok());
    w.check("embarked after all");
}

#[test]
fn second_embark_too_close() {
    let (mut w, home, civ, people, free) = embark_world();
    w.connect(B);
    // A free region with a neighbour on the map at least the embark distance from the
    // bootstrapped site, so only the new fortress is too close to it.
    let home_x = w.history().site(home).unwrap().region.x;
    let (target, near) = free
        .iter()
        .find_map(|r| {
            [r.x - 1, r.x + 1].into_iter().find(|&n| (1..16).contains(&n) && (n - home_x).abs() >= 4).map(|n| (*r, rc(n, 0)))
        })
        .expect("a free region with such a neighbour");
    let first = w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: target }).unwrap();
    // The other player, on the same tile or next to it.
    let req = |region| EmbarkRequest { civ, people: people[2..4].to_vec(), region };
    assert_eq!(w.embark(B, req(target)), Err(EmbarkRefusal::Occupied));
    assert_eq!(w.embark(B, req(near)), Err(EmbarkRefusal::TooClose { site: first }));
    let far = *free_regions(w.history()).first().expect("a region far from both sites");
    let second = w.embark(B, req(far)).expect("far enough");
    assert_ne!(first, second);
    w.check("two fortresses");
}

#[test]
fn one_fortress_per_player() {
    let (mut w, _, civ, people, free) = embark_world();
    let site = w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: free[0] }).unwrap();
    let again = EmbarkRequest { civ, people: people[2..4].to_vec(), region: free[1] };
    assert_eq!(w.embark(A, again.clone()), Err(EmbarkRefusal::AlreadyHasFortress));
    assert_eq!(w.return_to(A, site), Err(EmbarkRefusal::AlreadyHasFortress));
    assert!(!w.running(free[1]));
    w.packet(A, ClientPacket::Retire);
    assert!(w.embark(A, again).is_ok(), "a retired player may embark again");
    w.check("second fortress");
}

#[test]
fn retire_then_return() {
    let (mut w, _, civ, people, free) = embark_world();
    let chosen = people[..3].to_vec();
    let target = free[0];
    let site = w.embark(A, EmbarkRequest { civ, people: chosen.clone(), region: target }).unwrap();
    w.tick();
    w.take(A);
    w.packet(A, ClientPacket::Retire);
    let to_a = w.take(A);
    assert_eq!(player_region(&to_a), Some(Some(SPAWN_REGION)), "back where a connection starts");
    let marker = sites_in(&to_a).expect("fresh sites").into_iter().find(|m| m.site == site).unwrap();
    assert!(marker.returnable && !marker.abandoned);
    assert_eq!((marker.region, marker.population, marker.civ), (target, 3, civ));
    // The fortress parks by the usual grace rule; its people carry on abstractly.
    w.tick();
    w.wait(UNLOAD_GRACE_MS);
    assert!(!w.running(target), "the retired fortress parked");
    for p in &chosen {
        assert_eq!(w.history().person(*p).unwrap().location, Location::AtSite(site));
    }
    w.check("parked");
    // Reconnect (a new connection, a new client id): the site is listed as returnable.
    w.send(ServerEvent::ClientDisconnected(A));
    w.connect(A2);
    let to_a2 = w.take(A2);
    assert_eq!(fortress_in(&to_a2), Some(None), "a new connection plays no fortress");
    let marker = sites_in(&to_a2).expect("sites on connect").into_iter().find(|m| m.site == site).unwrap();
    assert!(marker.returnable);
    // Return: the fortress starts again with its people, the player in it.
    assert_eq!(w.return_to(A2, site), Ok(site));
    assert!(w.running(target));
    let census = bodies(&mut w.manager);
    for p in &chosen {
        assert_eq!(census.get(p), Some(&vec![target]));
    }
    w.packet(A2, ClientPacket::RequestPlayerRegion);
    assert_eq!(player_region(&w.take(A2)), Some(Some(target)));
    w.check("returned");
}

#[test]
fn retire_moves_player_to_spawn() {
    let (mut w, _, civ, people, free) = embark_world();
    let target = free[0];
    w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: target }).unwrap();
    w.tick();
    assert_eq!(w.players_in(target), vec![A]);
    w.take(A);
    w.packet(A, ClientPacket::Retire);
    let to_a = w.take(A);
    assert!(
        to_a.iter().any(|p| matches!(p, ServerPacket::PlayerRegion(Some(r), _) if *r == SPAWN_REGION)),
        "{to_a:?}"
    );
    assert_eq!(fortress_in(&to_a), Some(None));
    w.packet(A, ClientPacket::RequestPlayerRegion);
    assert_eq!(player_region(&w.take(A)), Some(Some(SPAWN_REGION)), "home is the spawn region");
    w.tick();
    assert!(w.players_in(target).is_empty(), "the player left the fortress");
    assert_eq!(w.players_in(SPAWN_REGION), vec![A]);
    w.wait(UNLOAD_GRACE_MS);
    assert!(!w.running(target), "the retired fortress parked");
    w.check("retired");
    // Retiring again, with no fortress, changes nothing.
    w.packet(A, ClientPacket::Retire);
    assert!(w.take(A).is_empty(), "no-op retire sends nothing");
    w.tick();
    assert_eq!(w.players_in(SPAWN_REGION), vec![A]);
}

#[test]
fn return_after_reconnect_leaves_one_player_body() {
    let (mut w, _, civ, people, free) = embark_world();
    let target = free[0];
    let site = w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: target }).unwrap();
    w.tick();
    w.send(ServerEvent::ClientDisconnected(A));
    w.connect(A2);
    assert_eq!(fortress_in(&w.take(A2)), Some(None));
    assert_eq!(w.return_to(A2, site), Ok(site));
    w.tick();
    assert_eq!(w.players_in(target), vec![A2], "one player body: the returning player's");
    w.check("returned after reconnect");
}

#[test]
fn return_refuses_other_players_fortress() {
    let (mut w, home, civ, people, free) = embark_world();
    w.connect(B);
    let site = w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: free[0] }).unwrap();
    assert_eq!(w.return_to(B, site), Err(EmbarkRefusal::InUse));
    assert_eq!(w.return_to(B, home), Err(EmbarkRefusal::NotPlayerFounded), "not founded by a player");
    assert_eq!(w.return_to(B, SiteId(999)), Err(EmbarkRefusal::NotPlayerFounded));
    // Its player gone (no accounts: identity is the connection), it may be taken up.
    w.send(ServerEvent::ClientDisconnected(A));
    assert_eq!(w.return_to(B, site), Ok(site));
    w.connect(A);
    assert_eq!(w.return_to(A, site), Err(EmbarkRefusal::InUse));
}

#[test]
fn notices_only_to_home_region() {
    let (mut w, home, civ, people, free) = embark_world();
    w.connect(B);
    // Two adventurers at two sites, and a third site nobody plays.
    let (b_site, b_people) = npc_site(&mut w, civ, free[0], 2);
    let (far_site, far_people) = npc_site(&mut w, civ, free[1], 2);
    assert_eq!(w.adventure(A, home), Ok(home));
    assert_eq!(w.adventure(B, b_site), Ok(b_site));
    w.tick();
    w.take(A);
    w.take(B);
    // A death at A's site: only A hears of it.
    let victim = people[0];
    history_mut_of(&mut w.manager).record_death(victim, DeathCause::OldAge);
    w.tick();
    let (to_a, to_b) = (notices_in(&w.take(A)), notices_in(&w.take(B)));
    assert_eq!(to_a.len(), 1, "{to_a:?}");
    assert!(to_a[0].contains("died"), "{to_a:?}");
    assert!(to_b.is_empty(), "B is not at A's site: {to_b:?}");
    // One at the unplayed site: nobody hears.
    let far_away = far_people[0];
    assert_eq!(w.history().person(far_away).unwrap().location, Location::AtSite(far_site));
    history_mut_of(&mut w.manager).record_death(far_away, DeathCause::OldAge);
    w.tick();
    assert!(notices_in(&w.take(A)).is_empty());
    assert!(notices_in(&w.take(B)).is_empty());
    // One at B's: only B.
    history_mut_of(&mut w.manager).record_death(b_people[0], DeathCause::OldAge);
    w.tick();
    assert!(notices_in(&w.take(A)).is_empty());
    assert_eq!(notices_in(&w.take(B)).len(), 1);
}

#[test]
fn client_cannot_spoof_server_events() {
    let (mut w, home, civ, people, free) = embark_world();
    // An unplayed site's founder: the outsider.
    let (far, far_people) = npc_site(&mut w, civ, free[0], 2);
    let target = w.history().site(home).unwrap().region;
    assert_eq!(w.adventure(A, home), Ok(home));
    w.tick();
    let before = bodies(&mut w.manager);
    let outsider = far_people[0];
    assert_eq!(w.history().person(outsider).unwrap().location, Location::AtSite(far));
    assert_eq!(w.history().person(people[0]).unwrap().location, Location::InRegion(target), "A's site has bodies");
    let spoofs = [
        GameEventKind::SpawnPerson(PersonSpawn { person: outsider, at: SpawnAt::Column([20, 30]), walk_to: None }),
        GameEventKind::DespawnPerson(people[0]),
        GameEventKind::WalkOut(people[1], rc(target.x + 1, 0)),
        GameEventKind::CreateClientAt(A, [5, 5], None),
        GameEventKind::ClientLeft(A),
    ];
    for kind in spoofs {
        w.packet(A, ClientPacket::GameEvent(GameEvent::new(kind, 0, target)));
    }
    w.tick();
    w.tick();
    assert_eq!(bodies(&mut w.manager), before, "the region took no client-sent server event");
    w.check("spoofed");
}

#[test]
fn person_info_lists_family() {
    let (mut w, home, _, _, _) = embark_world();
    let h = w.history();
    let kid = h.people().find(|p| p.mother.is_some() && p.father.is_some()).expect("a founder's child");
    let (kid, mother, father) = (kid.id, kid.mother.unwrap(), kid.father.unwrap());
    let name = |p: PersonId| h.person(p).unwrap().name.clone();
    let (kid_name, mother_name, father_name) = (name(kid), name(mother), name(father));
    let site_name = h.site(home).unwrap().name.clone();
    let info = |w: &mut World, p: PersonId| {
        w.packet(A, ClientPacket::RequestPersonInfo(p));
        w.take(A).into_iter().find_map(|p| match p {
            ServerPacket::PersonInfo(i) => Some(i),
            _ => None,
        })
    };
    let k = info(&mut w, kid).expect("info for the child");
    assert_eq!((k.person, k.alive, k.site.as_deref()), (kid, true, Some(site_name.as_str())));
    assert_eq!(k.name, kid_name);
    assert_eq!(k.parents, vec![mother_name, father_name.clone()]);
    assert!(k.spouse.is_none());
    let m = info(&mut w, mother).expect("info for the mother");
    assert_eq!(m.spouse, Some(father_name));
    assert!(m.children.contains(&kid_name));
    assert!(m.parents.is_empty());
    assert!(info(&mut w, PersonId(999)).is_none(), "an unknown person is ignored");
}

#[test]
fn calendar_and_sites_broadcast_daily() {
    let (mut w, _, civ, _, free) = embark_world();
    w.connect(B);
    let on_connect = w.take(B);
    assert!(on_connect.iter().any(|p| matches!(p, ServerPacket::Calendar(_))), "calendar on connect");
    assert_eq!(sites_in(&on_connect).expect("sites on connect").len(), 1);
    // The history founds a site (an NPC founding): the sites changed.
    let (site, _) = npc_site(&mut w, civ, free[0], 2);
    w.take(B);
    // Within the day nothing more is broadcast; at the day's turn the calendar and the
    // changed sites go to everyone.
    w.wait(DAY_MS);
    let to_b = w.take(B);
    let calendars: Vec<CalendarTick> = to_b
        .iter()
        .filter_map(|p| match p {
            ServerPacket::Calendar(t) => Some(*t),
            _ => None,
        })
        .collect();
    assert_eq!(calendars.len(), 1, "{to_b:?}");
    let sites = sites_in(&to_b).expect("the new site is broadcast");
    assert!(sites.iter().any(|m| m.site == site), "{sites:?}");
    // Nothing changed: the next day sends the calendar only.
    w.wait(DAY_MS);
    let to_b = w.take(B);
    assert!(sites_in(&to_b).is_none(), "{to_b:?}");
    assert!(to_b.iter().any(|p| matches!(p, ServerPacket::Calendar(_))));
}

#[test]
fn embark_allowed_in_matches_history() {
    let h = history();
    let sites: Vec<_> = h.sites().map(|s| (s.id, s.region, s.abandoned.is_some())).collect();
    for x in -1..17 {
        let r = rc(x, 0);
        assert_eq!(embark_allowed_in(h.grid(), sites.iter().copied(), r), h.embark_allowed(r), "region {x}");
    }
}

#[test]
fn embark_allowed_in_matches_history_with_an_abandoned_site() {
    let mut h = history();
    let (home, _, people) = home_site(&h);
    // Every member dies, so the bootstrapped site is abandoned.
    for p in people {
        h.record_death(p, DeathCause::OldAge);
    }
    assert!(h.site(home).unwrap().abandoned.is_some(), "the emptied site is abandoned");
    let sites: Vec<_> = h.sites().map(|s| (s.id, s.region, s.abandoned.is_some())).collect();
    for x in -1..17 {
        let r = rc(x, 0);
        assert_eq!(embark_allowed_in(h.grid(), sites.iter().copied(), r), h.embark_allowed(r), "region {x}");
    }
}

/// A mountainside refuses the player's embark (with no state change) while the civs'
/// own rule still allows it; the client's tile check agrees.
#[test]
fn embark_refuses_a_too_steep_region() {
    let mut map = strip();
    let steep_relief = game::EMBARK_MAX_RELIEF_PER_REGION_M * game::REGION_BLOCKS as f32 + 1.0;
    for t in &mut map.tiles[8..] {
        t.relief_m = steep_relief;
        assert!(game::too_steep_to_embark(t));
    }
    let p = BootstrapParams { seed: 7, civs: 1, founders_per_civ: 10, years: 0, ..BootstrapParams::default() };
    let mut h = bootstrap(&map, &p).expect("the strip holds a civ");
    let (_, civ, people) = home_site(&h);
    let steep = (8..16).map(|x| rc(x, 0)).find(|r| h.embark_allowed(*r).is_ok()).expect("a steep region civs may settle");
    let sites: Vec<_> = h.sites().map(|s| (s.id, s.region, s.abandoned.is_some())).collect();
    assert_eq!(player_embark_allowed_in(h.grid(), sites.iter().copied(), steep), Err(EmbarkRefusal::TooSteep));
    let log_before = h.log().len();
    let req = EmbarkRequest { civ, people: people[..2].to_vec(), region: steep };
    assert_eq!(h.player_embark(&req), Err(EmbarkRefusal::TooSteep));
    assert_eq!(h.log().len(), log_before, "a refusal changes nothing");
    let gentle = (1..8).map(|x| rc(x, 0)).find(|r| h.player_embark_allowed(*r).is_ok()).expect("a gentle free region");
    h.player_embark(&EmbarkRequest { region: gentle, ..req }).expect("gentle ground is fine");
}

fn fortress_in(packets: &[ServerPacket]) -> Option<Option<SiteId>> {
    packets.iter().rev().find_map(|p| match p {
        ServerPacket::Fortress(f) => Some(f.map(|f| f.site)),
        _ => None,
    })
}

#[test]
fn fortress_packet_follows_embark_retire_and_reconnect() {
    let (mut w, _, civ, people, free) = embark_world();
    let site = w.embark(A, EmbarkRequest { civ, people: people[..3].to_vec(), region: free[0] }).unwrap();
    assert_eq!(fortress_in(&w.take(A)), Some(Some(site)), "embark");
    w.packet(A, ClientPacket::Retire);
    assert_eq!(fortress_in(&w.take(A)), Some(None), "retire");
    assert_eq!(w.return_to(A, site), Ok(site));
    assert_eq!(fortress_in(&w.take(A)), Some(Some(site)), "return");
    // A reconnect is a new connection, so a new client id: it plays no fortress until
    // it returns to one.
    w.send(ServerEvent::ClientDisconnected(A));
    w.connect(A2);
    assert_eq!(fortress_in(&w.take(A2)), Some(None), "a reconnect starts with none");
    assert_eq!(w.return_to(A2, site), Ok(site));
    assert_eq!(fortress_in(&w.take(A2)), Some(Some(site)), "return after reconnect");
    w.connect(B);
    assert_eq!(fortress_in(&w.take(B)), Some(None), "a new player has none");
}

#[test]
fn connect_sends_civ_names() {
    let (mut w, _, _, _, _) = embark_world();
    w.connect(B);
    let civs: Vec<_> = w.take(B).into_iter().filter_map(|p| if let ServerPacket::Civs(c) = p { Some(c) } else { None }).collect();
    assert_eq!(civs.len(), 1, "one Civs packet");
    let want: Vec<_> = w.history().civs.values().map(|c| (c.id, c.name.clone())).collect();
    let got: Vec<_> = civs[0].iter().map(|m| (m.civ, m.name.clone())).collect();
    assert!(!want.is_empty());
    assert_eq!(got, want);
}

#[test]
fn legends_request_answered() {
    let (mut w, _, _, _, _) = embark_world();
    w.connect(B);
    w.take(B);
    let q = LegendsQuery { filter: LegendsFilter::All, before: None, limit: 5 };
    w.packet(B, ClientPacket::RequestLegends(q.clone()));
    let pages: Vec<_> = w.take(B).into_iter().filter_map(|p| if let ServerPacket::Legends(g) = p { Some(g) } else { None }).collect();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0], w.history().legends(&q));
}

#[test]
fn adventure_starts_at_populated_site() {
    let (mut w, home, _, _, _) = embark_world();
    let region = w.history().site(home).unwrap().region;
    assert_eq!(w.adventure(A, home), Ok(home));
    let to_a = w.take(A);
    assert_eq!(player_region(&to_a), Some(Some(region)));
    assert_eq!(fortress_in(&to_a), Some(None), "an adventurer holds no fortress");
    w.settle();
    assert_eq!(w.mode_in(region, A), Some(PlayMode::Adventure));
    w.tick();
    assert_eq!(w.players_in(region), vec![A]);
    assert_eq!(w.mode_in(region, A), Some(PlayMode::Adventure));
    w.check("adventuring");
}

#[test]
fn adventure_in_the_home_region_lands_the_body_on_the_site() {
    let (mut w, home, _, _, _) = embark_world();
    let region = w.history().site(home).unwrap().region;
    // Dev Play puts the player's body in the site's region first, off the landing.
    w.packet(A, ClientPacket::PlayRegion(region));
    w.take(A);
    assert_eq!(w.adventure(A, home), Ok(home));
    w.settle();
    assert_eq!(w.mode_in(region, A), Some(PlayMode::Adventure));
    w.tick();
    assert_eq!(w.body_column(region, A), Some([20, 30]), "the body is at the site's landing");
}

#[test]
fn adventure_refuses_unpopulated_and_abandoned() {
    let (mut w, home, civ, people, free) = embark_world();
    let (first, _) = npc_site(&mut w, civ, free[0], 2);
    // Every member of the bootstrapped site dies: it is abandoned.
    for p in people {
        history_mut_of(&mut w.manager).record_death(p, DeathCause::OldAge);
    }
    assert!(w.history().site(home).unwrap().abandoned.is_some(), "the emptied site is abandoned");
    w.take(A);
    assert_eq!(w.adventure(A, home), Err(AdventureRefusal::SiteAbandoned));
    // A site whose members are all dead but which is not yet abandoned.
    {
        let h = history_mut_of(&mut w.manager);
        let members: Vec<PersonId> = h.site(first).unwrap().members.iter().copied().collect();
        assert!(!members.is_empty());
        for m in members {
            h.people.get_mut(&m).unwrap().died = Some((CalendarTick(0), DeathCause::OldAge));
        }
    }
    assert_eq!(w.adventure(A, first), Err(AdventureRefusal::Unpopulated));
    assert_eq!(w.adventure(A, SiteId(9999)), Err(AdventureRefusal::UnknownSite));
    assert_eq!(player_region(&w.take(A)), None, "no move for a refusal");
}

#[test]
fn fortress_and_adventure_exclusive() {
    let (mut w, home, civ, people, free) = embark_world();
    w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: free[0] }).unwrap();
    assert_eq!(w.adventure(A, home), Err(AdventureRefusal::AlreadyPlaying));
    w.connect(B);
    assert_eq!(w.adventure(B, home), Ok(home));
    assert_eq!(w.adventure(B, home), Err(AdventureRefusal::AlreadyPlaying), "one adventure at a time");
    let req = EmbarkRequest { civ, people: people[2..4].to_vec(), region: free[1] };
    assert_eq!(w.embark(B, req), Err(EmbarkRefusal::AlreadyHasFortress));
    w.packet(A, ClientPacket::Retire);
    let site = w.history().sites().find(|s| s.player_founded).unwrap().id;
    assert_eq!(w.return_to(B, site), Err(EmbarkRefusal::AlreadyHasFortress));
}

#[test]
fn end_adventure_returns_to_spawn_and_frees_the_claim() {
    let (mut w, home, _, _, _) = embark_world();
    let region = w.history().site(home).unwrap().region;
    assert_eq!(w.adventure(A, home), Ok(home));
    w.tick();
    w.take(A);
    w.packet(A, ClientPacket::EndAdventure);
    let to_a = w.take(A);
    assert_eq!(player_region(&to_a), Some(Some(SPAWN_REGION)));
    assert!(sites_in(&to_a).is_some(), "fresh sites for the arrival map");
    w.tick();
    assert!(w.players_in(region).is_empty(), "the adventurer left the site");
    assert_eq!(w.players_in(SPAWN_REGION), vec![A]);
    assert_eq!(w.mode_in(SPAWN_REGION, A), Some(PlayMode::Adventure));
    assert_eq!(w.manager.client_claim(A), None, "the claim is freed on the host");
    // Ending again changes nothing.
    w.packet(A, ClientPacket::EndAdventure);
    assert!(w.take(A).is_empty(), "no-op end sends nothing");
    // Free again: a second adventure starts.
    w.wait(UNLOAD_GRACE_MS);
    assert_eq!(w.adventure(A, home), Ok(home), "a second adventure starts");
    assert_eq!(w.manager.client_claim(A), Some(game::Claim::Adventure(home)));
    w.check("adventuring again");
}

#[test]
fn adventure_kept_across_reconnect() {
    let (mut w, home, _, _, _) = embark_world();
    let region = w.history().site(home).unwrap().region;
    assert_eq!(w.adventure(A, home), Ok(home));
    w.send(ServerEvent::ClientDisconnected(A));
    // The same player on a new connection: the adventure is part of its record.
    w.send(ServerEvent::ClientConnected { client: A2, player: game::PlayerId(A as u64), record: None });
    let to_a2 = w.take(A2);
    assert!(to_a2.iter().any(|p| matches!(p, ServerPacket::AdventureResult(Ok(s)) if *s == home)), "{to_a2:?}");
    assert_eq!(player_region(&to_a2), Some(Some(region)));
    w.settle();
    assert_eq!(w.mode_in(region, A2), Some(PlayMode::Adventure));
    assert_eq!(w.adventure(A2, home), Err(AdventureRefusal::AlreadyPlaying), "still adventuring");
}

/// A player's fortress claim is part of its record: after an orderly exit and a
/// restart, the same player (a new client id) holds its fortress again without
/// embarking, and is told so on connect.
#[test]
fn fortress_claim_survives_restart() {
    let (mut w, _, civ, people, free) = embark_world();
    let target = free[0];
    let site = w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: target }).unwrap();
    w.tick();
    w.take(A);
    // The server's orderly exit: park every player, then every region; a
    // save cut (nothing runs, so it is complete at once) holds them all.
    w.manager.park_all_players(w.now_ms);
    w.manager.shutdown_all();
    w.settle();
    assert!(w.manager.running_regions().is_empty());
    w.manager.begin_cut(0).expect("no cut pending");
    let cut = w.manager.take_cut().expect("nothing runs: the cut is complete");
    let mut store = crate::common::cut_store::CutStore::default();
    store.apply(&cut);
    let record = store.players.get(&game::PlayerId(A as u64)).cloned().expect("A's record");
    assert_eq!(record.fortress, Some(site));
    let history = w.history().clone();
    drop(w);

    let mut w = World::over(history, InlineSpawner::with_portable(store.regions));
    w.send(ServerEvent::ClientConnected { client: 0, player: game::PlayerId(A as u64), record: Some(record) });
    let to_a = w.take(0);
    assert_eq!(fortress_in(&to_a), Some(Some(site)), "the fortress is held again: {to_a:?}");
    assert!(to_a.iter().any(|p| matches!(p, ServerPacket::EmbarkResult(Ok(s)) if *s == site)), "{to_a:?}");
    assert_eq!(player_region(&to_a), Some(Some(target)));
    assert_eq!(w.players_in(target), vec![0], "the body is back in the fortress");
    assert_eq!(w.return_to(0, site), Err(EmbarkRefusal::AlreadyHasFortress), "no second embark");
    // Nobody else may take it up.
    w.connect(B);
    assert_eq!(w.return_to(B, site), Err(EmbarkRefusal::InUse));
}

fn spawn_at(events: &[GameEventKind], who: PersonId) -> Option<SpawnAt> {
    events.iter().find_map(|e| match e {
        GameEventKind::SpawnPerson(s) if s.person == who => Some(s.at),
        _ => None,
    })
}

#[test]
fn autosave_positions_are_respawn_positions() {
    let mut h = history();
    let (site, _, people) = home_site(&h);
    let region = h.site(site).unwrap().region;
    let events = h.region_started(region);
    assert_eq!(events.len(), people.len());
    let who = people[0];
    let landing = spawn_at(&events, who).expect("spawns at the landing");
    // The autosave sees them far from the landing: block (3, 5), region-local units.
    let b = game::BLOCK_VOXELS as f32;
    h.note_positions(vec![(who, [3.5 * b, 40.0, 5.5 * b])]);
    // A server restart: save, load, the restarted host's attach lists no region (its
    // region parks empty: bodies go back to the abstract history), the region starts.
    let grid = h.grid().clone();
    let mut h = persist::decode(&persist::encode(&h), grid).expect("history reloads");
    h.region_parked(region, Vec::new());
    let events = h.region_started(region);
    assert_eq!(spawn_at(&events, who), Some(SpawnAt::Column([3, 5])));
    assert_eq!(h.noted_position(who), None, "the note is used up");
    // A park records positions too: the next start spawns them there.
    h.region_stopping(region);
    h.region_parked(region, vec![(who, [7.5 * b, 40.0, 9.5 * b])]);
    let events = h.region_started(region);
    assert_eq!(spawn_at(&events, who), Some(SpawnAt::Column([7, 9])));
    // Used up, so the next start is the plain landing again.
    h.region_stopping(region);
    h.region_parked(region, vec![]);
    let events = h.region_started(region);
    assert_eq!(spawn_at(&events, who), Some(landing));
}

#[test]
fn stale_position_not_used_after_leaving() {
    let mut h = history();
    let (site, _, people) = home_site(&h);
    let region = h.site(site).unwrap().region;
    h.region_started(region);
    let (left, died) = (people[0], people[1]);
    h.note_positions(vec![(left, [40.0, 0.0, 40.0]), (died, [40.0, 0.0, 40.0])]);
    assert!(h.noted_position(left).is_some() && h.noted_position(died).is_some());
    h.person_crossed(left, rc(region.x + 1, region.z));
    assert_eq!(h.noted_position(left), None);
    h.record_death(died, game::history::records::DeathCause::OldAge);
    assert_eq!(h.noted_position(died), None);
}

#[test]
fn noted_positions_survive_a_reload() {
    let mut h = history();
    let (site, _, people) = home_site(&h);
    let region = h.site(site).unwrap().region;
    h.region_started(region);
    h.note_positions(vec![(people[0], [40.0, 0.0, 40.0])]);
    let grid = h.grid().clone();
    let back = persist::decode(&persist::encode(&h), grid).expect("a current file loads");
    assert_eq!(back.noted_position(people[0]), Some((region, [40.0, 0.0, 40.0])));
    assert_eq!(persist::encode(&back), persist::encode(&h));
}

const MARK_STACK: game::ItemStack = game::ItemStack { item: game::ItemId(5), count: 33 };
const MARK_CURSOR: game::ItemStack = game::ItemStack { item: game::ItemId(6), count: 2 };

/// Puts marker stacks in `c`'s inventory (slot 20) and on its cursor in `r`;
/// returns the inventory a move must keep (the cursor stack folded into it).
fn mark_inventory(w: &mut World, r: RegionCoords, c: ClientId) -> game::Inventory {
    let mut expected = None;
    w.manager.spawner_mut().with_region(r, |region| {
        region.with_data(|d| {
            let client = d.clients.get_mut(&c).expect("the client is here");
            client.inventory.slots[20] = Some(MARK_STACK);
            client.cursor = Some(MARK_CURSOR);
            let mut inv = client.inventory.clone();
            assert_eq!(inv.add(MARK_CURSOR), 0);
            expected = Some(inv);
        });
    });
    expected.unwrap()
}

#[test]
fn inventory_survives_move_client() {
    let (mut w, home, _, _, _) = embark_world();
    let region = w.history().site(home).unwrap().region;
    assert_ne!(region, SPAWN_REGION);
    let expected = mark_inventory(&mut w, SPAWN_REGION, A);
    assert_eq!(w.adventure(A, home), Ok(home));
    w.tick();
    assert!(w.client_in(SPAWN_REGION, A).is_none(), "left the spawn region");
    let there = w.client_in(region, A).expect("moved to the site");
    assert_eq!(there.inventory, expected, "the inventory came along, no new kit");
    assert_eq!(there.cursor, None);
    assert_eq!(there.mode, PlayMode::Adventure);
    assert_eq!(w.players_in(region), vec![A]);
    w.packet(A, ClientPacket::EndAdventure);
    w.tick();
    let back = w.client_in(SPAWN_REGION, A).expect("back at spawn");
    assert_eq!(back.inventory, expected, "and back again");
    assert_eq!(back.mode, PlayMode::Adventure);
    assert!(w.players_in(region).is_empty());
    assert_eq!(w.players_in(SPAWN_REGION), vec![A]);
}

/// A second move while the first one's park is still on its way: the body
/// goes straight to the second target, once, with its inventory.
#[test]
fn move_retargeted_in_flight_keeps_one_body() {
    let (mut w, home, _, _, _) = embark_world();
    let region = w.history().site(home).unwrap().region;
    let expected = mark_inventory(&mut w, SPAWN_REGION, A);
    assert!(w.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::StartAdventure(home), A), w.now_ms));
    assert!(w.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::EndAdventure, A), w.now_ms));
    w.settle();
    w.tick();
    assert!(w.client_in(region, A).is_none(), "never created at the site");
    let back = w.client_in(SPAWN_REGION, A).expect("at spawn");
    assert_eq!(back.inventory, expected);
    assert_eq!(back.mode, PlayMode::Adventure, "a body at spawn after the adventure's start and end");
    assert_eq!(w.players_in(SPAWN_REGION), vec![A]);
}

#[test]
fn off_map_record_restores_at_spawn_with_its_client() {
    let mut w = World::new();
    let mut client = game::Client { inventory: game::Inventory::new(), ..game::Client::default() };
    client.inventory.slots[20] = Some(MARK_STACK);
    let record = game::PlayerRecord {
        client,
        pose: game::Pose::from_translation(game::body_on_tile([3, 1, 3])),
        region: rc(0, 5),
        fortress: None,
        adventure: None,
    };
    w.send(ServerEvent::ClientConnected { client: A, player: game::PlayerId(42), record: Some(record) });
    assert_eq!(player_region(&w.take(A)), Some(Some(SPAWN_REGION)));
    let placed = w.client_in(SPAWN_REGION, A).expect("placed at spawn");
    assert_eq!(placed.inventory.slots[20], Some(MARK_STACK), "with the record's Client, not a fresh one");
    assert_eq!(w.players_in(SPAWN_REGION), vec![A]);
}
