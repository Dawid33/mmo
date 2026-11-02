//! Plan 3: the player's side of the world history — embark, retire, return, the sites /
//! calendar / notice broadcasts and person info — driven through a real `WorldManager`
//! over `InlineSpawner` regions with a bootstrapped history.

use crate::history_handover::{assert_one_place, bodies, floored_region, land_map};
use crossbeam::channel::{unbounded, Receiver};
use game::history::bootstrap::{bootstrap, BootstrapParams};
use game::history::log::LogKind;
use game::history::rules::EMBARK_PLAYER_PARTY_MAX;
use game::history::*;
use game::{
    Biome, ClientId, ClientPacket, EmbarkRequest, GameEvent, GameEventKind, InlineLodBuilder, InlineSpawner,
    LodBuilder, PersonSpawn, RegionCoords, RegionOutput, ServerEvent, ServerPacket, SiteMarker, SpawnAt, WorldManager,
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
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
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
        manager.set_history(history(), 0);
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
        self.send(ServerEvent::ClientConnected(c));
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
        self.manager.history().expect("a history is installed")
    }

    fn running(&self, r: RegionCoords) -> bool {
        self.manager.running_regions().contains(&r)
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
        assert_one_place(self.manager.history().unwrap(), &census, when);
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
        let h = w.manager.history_mut().unwrap();
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
    let a_site = w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: free[0] }).unwrap();
    let b_site = w.embark(B, EmbarkRequest { civ, people: people[2..4].to_vec(), region: free[1] }).unwrap();
    w.tick();
    w.take(A);
    w.take(B);
    // A death in A's fortress: only A hears of it.
    let victim = people[0];
    w.manager.history_mut().unwrap().record_death(victim, DeathCause::OldAge);
    w.tick();
    let (to_a, to_b) = (notices_in(&w.take(A)), notices_in(&w.take(B)));
    assert_eq!(to_a.len(), 1, "{to_a:?}");
    assert!(to_a[0].contains("died"), "{to_a:?}");
    assert!(to_b.is_empty(), "B is not in A's fortress: {to_b:?}");
    // One at the unplayed bootstrap site: nobody hears.
    let far_away = people[5];
    assert_eq!(w.history().person(far_away).unwrap().location, Location::AtSite(home));
    w.manager.history_mut().unwrap().record_death(far_away, DeathCause::OldAge);
    w.tick();
    assert!(notices_in(&w.take(A)).is_empty());
    assert!(notices_in(&w.take(B)).is_empty());
    // One in B's: only B.
    w.manager.history_mut().unwrap().record_death(people[2], DeathCause::OldAge);
    w.tick();
    assert!(notices_in(&w.take(A)).is_empty());
    assert_eq!(notices_in(&w.take(B)).len(), 1);
    assert_ne!(a_site, b_site);
}

#[test]
fn client_cannot_spoof_server_events() {
    let (mut w, home, civ, people, free) = embark_world();
    let target = free[0];
    w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: target }).unwrap();
    w.tick();
    let before = bodies(&mut w.manager);
    let outsider = people[5];
    assert_eq!(w.history().person(outsider).unwrap().location, Location::AtSite(home));
    let spoofs = [
        GameEventKind::SpawnPerson(PersonSpawn { person: outsider, at: SpawnAt::Column([20, 30]), walk_to: None }),
        GameEventKind::DespawnPerson(people[0]),
        GameEventKind::WalkOut(people[1], rc(target.x + 1, 0)),
        GameEventKind::CreateClientAt(A, [5, 5]),
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
    let (mut w, _, civ, people, free) = embark_world();
    w.connect(B);
    let on_connect = w.take(B);
    assert!(on_connect.iter().any(|p| matches!(p, ServerPacket::Calendar(_))), "calendar on connect");
    assert_eq!(sites_in(&on_connect).expect("sites on connect").len(), 1);
    let site = w.embark(A, EmbarkRequest { civ, people: people[..2].to_vec(), region: free[0] }).unwrap();
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
    assert!(sites.iter().any(|m| m.site == site && m.returnable));
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
    let (home, civ, people) = home_site(&h);
    // Two embarks take every member, so the bootstrapped site is abandoned.
    let max = EMBARK_PLAYER_PARTY_MAX as usize;
    let first = free_regions(&h)[0];
    h.player_embark(&EmbarkRequest { civ, people: people[..max].to_vec(), region: first }).unwrap();
    let second = (0..16).map(|x| rc(x, 0)).find(|r| h.embark_allowed(*r).is_ok()).expect("room for a second embark");
    h.player_embark(&EmbarkRequest { civ, people: people[max..].to_vec(), region: second }).unwrap();
    assert!(h.site(home).unwrap().abandoned.is_some(), "the emptied site is abandoned");
    let sites: Vec<_> = h.sites().map(|s| (s.id, s.region, s.abandoned.is_some())).collect();
    for x in -1..17 {
        let r = rc(x, 0);
        assert_eq!(embark_allowed_in(h.grid(), sites.iter().copied(), r), h.embark_allowed(r), "region {x}");
    }
}

fn fortress_in(packets: &[ServerPacket]) -> Option<Option<SiteId>> {
    packets.iter().rev().find_map(|p| match p {
        ServerPacket::Fortress(f) => Some(*f),
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
