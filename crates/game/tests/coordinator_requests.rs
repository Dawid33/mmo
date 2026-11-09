//! R2b Task 6: a player's world requests (embark, return, adventure, legends, …) are
//! coordinator requests with ids, queued in the host's outbox while the world link is
//! down and replayed after its `Attach`, answered once, or `WorldUnavailable` after the
//! timeout. Driven through a `WorldManager` over `InlineSpawner` regions and an inline
//! coordinator link whose every output is held back 0..=3 pumps by the seed
//! (`InlineSchedule::coordinator_delay`); the link is taken down and up with
//! `InlineWorldLink::set_up`. Each test sweeps [`SEEDS`] seeds; a failure names the seed
//! to replay.

use std::sync::{Arc, Mutex};

use crate::history_handover::{assert_one_place, bodies, floored_region, history_mut_of, history_of, land_map, years_before};
use crossbeam::channel::{unbounded, Receiver};
use game::history::bootstrap::{bootstrap, BootstrapParams};
use game::history::*;
use game::{
    Biome, ClientId, ClientPacket, Coordinator, EmbarkRequest, HostMsg, InlineLodBuilder, InlineSchedule, InlineSpawner,
    InlineWorldLink, LodBuilder, PlayerId, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldLink,
    WorldManager, WorldMap, WorldRequest, TICK_RATE, UNLOAD_GRACE_MS, WORLD_REQUEST_TIMEOUT_SECS,
};

/// Seeds swept per test.
const SEEDS: u64 = 32;
/// Most pumps the link holds each coordinator output back.
const LINK_DELAY: u32 = 3;
/// Rounds that drain the link and let the regions settle.
const DRAIN_ROUNDS: usize = 10;

const A: ClientId = 0;
const B: ClientId = 1;
/// Player `A` again after a reconnect: client ids are per connection.
const A2: ClientId = 2;

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

/// A roll in `0..n` from the seed and a salt.
fn roll(seed: u64, salt: u64, n: u64) -> u64 {
    let mut z = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 31)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 29)) % n
}

/// A 16 × 1 strip of grassland whose first tile — the spawn region (0, 0) — is ocean.
fn strip() -> WorldMap {
    let mut m = land_map(16, 1);
    m.tiles[0].biome = Biome::Ocean;
    m
}

/// One civ of ten founders, no years run.
fn history() -> History {
    let p = BootstrapParams { seed: 7, civs: 1, founders_per_civ: 10, years: 0, ..BootstrapParams::default() };
    bootstrap(&strip(), &p).expect("the strip holds a civ")
}

/// The inline link, recording every message the coordinator was handed (sent while up).
struct Recording {
    inner: InlineWorldLink,
    sent: Arc<Mutex<Vec<HostMsg>>>,
    /// The connection generation the link reports ([`World::reconnect_between_looks`]).
    generation: Arc<std::sync::atomic::AtomicU64>,
    /// `Some` while the connection is dead but the host still believes it up
    /// ([`World::begin_stale`]): what the host sends waits here, nothing is readable.
    stale: Arc<Mutex<Option<Vec<HostMsg>>>>,
}

impl WorldLink for Recording {
    fn send(&mut self, msg: HostMsg) {
        if let Some(held) = self.stale.lock().unwrap().as_mut() {
            held.push(msg);
            return;
        }
        if self.inner.is_up() {
            self.sent.lock().unwrap().push(msg.clone());
        }
        self.inner.send(msg);
    }
    fn recv(&mut self) -> Option<game::CoordMsg> {
        if self.stale.lock().unwrap().is_some() {
            return None;
        }
        self.inner.recv()
    }
    fn is_up(&self) -> bool {
        self.inner.is_up()
    }
    fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn pump(&mut self, now_ms: u64) {
        self.inner.pump(now_ms);
    }
    fn as_inline_mut(&mut self) -> Option<&mut InlineWorldLink> {
        Some(&mut self.inner)
    }
    fn as_inline(&self) -> Option<&InlineWorldLink> {
        Some(&self.inner)
    }
}

/// A `WorldManager` over a bootstrapped history whose world link lags by the seed.
struct World {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    inbox: Vec<(Option<ClientId>, ServerPacket)>,
    now_ms: u64,
    seed: u64,
    sent: Arc<Mutex<Vec<HostMsg>>>,
    generation: Arc<std::sync::atomic::AtomicU64>,
    stale: Arc<Mutex<Option<Vec<HostMsg>>>>,
}

impl World {
    fn new(seed: u64) -> Self {
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
        let sent = Arc::new(Mutex::new(Vec::new()));
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let inner = InlineWorldLink::new(Coordinator::new(history(), 0)).with_schedule(&schedule);
        let stale = Arc::new(Mutex::new(None));
        let link = Recording { inner, sent: sent.clone(), generation: generation.clone(), stale: stale.clone() };
        manager.set_world_link(Box::new(link), seed, 0);
        World { manager, region_out, packets, inbox: Vec::new(), now_ms: 0, seed, sent, generation, stale }
    }

    /// Connected players `clients`, the link drained.
    fn with(seed: u64, clients: &[ClientId]) -> Self {
        let mut w = World::new(seed);
        for &c in clients {
            w.send(ServerEvent::connected(c));
        }
        w.drain();
        w.inbox.clear();
        w
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

    fn packet(&mut self, c: ClientId, packet: ClientPacket) {
        self.send(ServerEvent::ClientPacket(packet, c));
    }

    fn tick(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.settle();
        self.manager.maintain(self.now_ms);
        self.settle();
    }

    fn link(&mut self) -> &mut InlineWorldLink {
        self.manager.world_link_mut().and_then(|l| l.as_inline_mut()).expect("an inline link")
    }

    fn set_up(&mut self, up: bool) {
        self.link().set_up(up);
    }

    /// The link dropped and came back between two of the host's looks: up before and
    /// after, a new connection generation, and what was on the old connection lost.
    fn reconnect_between_looks(&mut self) {
        self.link().lose_in_transit();
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// The connection dies unnoticed: the host keeps sending as if up (held, see
    /// [`Recording::stale`]) and reads nothing.
    fn begin_stale(&mut self) {
        *self.stale.lock().unwrap() = Some(Vec::new());
    }

    /// The link reconnected: each message sent under the stale belief was lost with the
    /// old connection or (by the seed) lands on the new one — ahead of the `Attach`, which
    /// the host sends only once it sees the new generation. Returns those delivered.
    fn end_stale(&mut self) -> Vec<HostMsg> {
        let held = self.stale.lock().unwrap().take().expect("a stale window");
        self.link().lose_in_transit();
        let mut landed = Vec::new();
        for (i, msg) in held.into_iter().enumerate() {
            if roll(self.seed, 100 + i as u64, 3) > 0 {
                landed.push(msg.clone());
                self.sent.lock().unwrap().push(msg.clone());
                self.link().send(msg);
            }
        }
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        landed
    }

    /// Ticks until the link is empty and every request answered.
    fn drain(&mut self) {
        for _ in 0..DRAIN_ROUNDS {
            self.tick();
        }
        assert_eq!(self.link().in_transit(), 0, "seed {}: the link drained", self.seed);
        assert_eq!(self.manager.pending_world_requests(), 0, "seed {}: every request answered", self.seed);
        assert!(self.manager.world_attached(), "seed {}: attached", self.seed);
    }

    fn history(&self) -> &History {
        history_of(&self.manager)
    }

    fn coordinator(&mut self) -> &Coordinator {
        self.link().coordinator()
    }

    /// Every `EmbarkResult` sent to `c` since the last call.
    fn embark_results(&mut self, c: ClientId) -> Vec<Result<SiteId, EmbarkRefusal>> {
        let mut out = Vec::new();
        self.inbox.retain(|(to, p)| match p {
            ServerPacket::EmbarkResult(r) if *to == Some(c) => {
                out.push(*r);
                false
            }
            _ => true,
        });
        out
    }

    /// Every `Legends` page sent to `c` since the last call.
    fn legends(&mut self, c: ClientId) -> usize {
        let before = self.inbox.len();
        self.inbox.retain(|(to, p)| !(matches!(p, ServerPacket::Legends(_)) && *to == Some(c)));
        before - self.inbox.len()
    }

    /// `c` asks `packet`, then the world drains: its one `EmbarkResult`.
    fn ask(&mut self, c: ClientId, packet: ClientPacket) -> Result<SiteId, EmbarkRefusal> {
        self.packet(c, packet);
        self.drain();
        let results = self.embark_results(c);
        assert_eq!(results.len(), 1, "seed {}: one EmbarkResult: {results:?}", self.seed);
        results[0]
    }

    /// The bootstrapped site's civ and people, and a region a new site may go in.
    fn embark_request(&self) -> EmbarkRequest {
        let h = self.history();
        let s = h.sites().next().expect("a site");
        let region = (0..16).map(|x| rc(x, 0)).find(|r| h.embark_allowed(*r).is_ok()).expect("room to embark");
        EmbarkRequest { civ: s.civ, people: s.members.iter().copied().take(2).collect(), region }
    }

    fn player_sites(&self) -> Vec<SiteId> {
        self.history().sites().filter(|s| s.player_founded).map(|s| s.id).collect()
    }

    fn players_in(&mut self, r: RegionCoords) -> Vec<ClientId> {
        let mut out = Vec::new();
        if self.manager.running_regions().contains(&r) {
            self.manager.spawner_mut().with_region(r, |region| out.extend(region.data().clients.keys().copied()));
        }
        out
    }

    /// The messages the coordinator was handed since `mark`.
    fn sent_since(&self, mark: usize) -> Vec<HostMsg> {
        self.sent.lock().unwrap()[mark..].to_vec()
    }

    fn mark(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    /// At most one body per person, only for the living, only where the history has it.
    fn check_lagging(&mut self, when: &str) {
        let census = bodies(&mut self.manager);
        let h = history_of(&self.manager);
        for (p, at) in &census {
            assert_eq!(at.len(), 1, "seed {} {when}: {p:?} has bodies in {at:?}", self.seed);
            let q = h.person(*p).unwrap_or_else(|| panic!("seed {} {when}: a body for unknown {p:?}", self.seed));
            assert_eq!(q.location, Location::InRegion(at[0]), "seed {} {when}: {p:?}'s body", self.seed);
        }
    }

    fn check_settled(&mut self, when: &str) {
        let census = bodies(&mut self.manager);
        assert_one_place(history_of(&self.manager), &census, &format!("seed {} {when}", self.seed));
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn embark_round_trip_moves_player_once() {
    each_seed("embark_round_trip_moves_player_once", |seed| {
        let mut w = World::with(seed, &[A]);
        let req = w.embark_request();
        let site = w.ask(A, ClientPacket::Embark(req.clone())).expect("embarked");
        assert_eq!(w.player_sites(), vec![site], "seed {seed}: one site founded");
        assert_eq!(w.coordinator().claim_of(PlayerId(A as u64)), Some(game::Claim::Fortress(site)));
        assert_eq!(w.players_in(req.region), vec![A], "seed {seed}: the player moved in once");
        for p in &req.people {
            assert_eq!(bodies(&mut w.manager).get(p), Some(&vec![req.region]), "seed {seed}: {p:?} in the fortress");
        }
        w.check_settled("embarked");
        // A second embark of the same player: refused by the host, nothing sent.
        let again = w.ask(A, ClientPacket::Embark(req));
        assert_eq!(again, Err(EmbarkRefusal::AlreadyHasFortress));
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn embark_reply_after_disconnect_releases_claim() {
    each_seed("embark_reply_after_disconnect_releases_claim", |seed| {
        let mut w = World::with(seed, &[A, B]);
        let req = w.embark_request();
        // Down: the embark waits in the outbox; its player leaves before it is answered.
        w.set_up(false);
        w.packet(A, ClientPacket::Embark(req.clone()));
        w.send(ServerEvent::ClientDisconnected(A));
        assert_eq!(w.manager.pending_world_requests(), 1);
        w.set_up(true);
        w.drain();
        assert!(w.embark_results(A).is_empty(), "seed {seed}: nobody to tell");
        let sites = w.player_sites();
        assert_eq!(sites.len(), 1, "seed {seed}: the site is founded once");
        let site = sites[0];
        assert!(w.history().returnable(site), "seed {seed}: the site stays, returnable");
        assert_eq!(w.coordinator().claim_of(PlayerId(A as u64)), None, "seed {seed}: the claim was given back");
        assert!(w.players_in(req.region).is_empty(), "seed {seed}: nobody moved in");
        assert_eq!(w.ask(B, ClientPacket::ReturnTo(site)), Ok(site), "seed {seed}: free for another player");
        w.check_settled("returned");
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn requests_queue_while_link_down_and_replay_once() {
    each_seed("requests_queue_while_link_down_and_replay_once", |seed| {
        let mut w = World::with(seed, &[A]);
        let req = w.embark_request();
        let query = LegendsQuery { filter: LegendsFilter::All, before: None, limit: 5 };
        w.set_up(false);
        w.packet(A, ClientPacket::Embark(req.clone()));
        w.packet(A, ClientPacket::RequestLegends(query.clone()));
        assert_eq!(w.manager.world_outbox_len(), 2, "seed {seed}: both queued");
        for _ in 0..roll(seed, 1, 4) {
            w.tick();
        }
        assert!(w.embark_results(A).is_empty() && w.legends(A) == 0, "seed {seed}: unanswered while down");
        let mark = w.mark();
        w.set_up(true);
        w.drain();
        let sent = w.sent_since(mark);
        assert!(matches!(sent.first(), Some(HostMsg::Attach { .. })), "seed {seed}: the Attach first: {sent:?}");
        let requests: Vec<&WorldRequest> = sent
            .iter()
            .filter_map(|m| match m {
                HostMsg::Request { req, .. } => Some(req),
                _ => None,
            })
            .collect();
        assert!(
            matches!(requests[..], [WorldRequest::Embark(_), WorldRequest::Legends(_)]),
            "seed {seed}: each request once, in order: {requests:?}"
        );
        assert_eq!(w.embark_results(A).len(), 1, "seed {seed}: one EmbarkResult");
        assert_eq!(w.legends(A), 1, "seed {seed}: one Legends page");
        assert_eq!(w.player_sites().len(), 1, "seed {seed}: one site");
        assert_eq!(w.manager.world_outbox_len(), 0);
        assert_eq!(w.players_in(req.region), vec![A]);
    });
}

/// A reconnect the host never saw as down (a QUIC link that dropped and came back
/// between two pumps: `is_up` true both times, a new connection generation) still makes
/// it attach again — a fresh `Attach` epoch, then the unanswered request resent — and
/// the embark is answered once, one site.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn reconnect_between_looks_reattaches() {
    each_seed("reconnect_between_looks_reattaches", |seed| {
        let mut w = World::with(seed, &[A]);
        let req = w.embark_request();
        let mark = w.mark();
        w.packet(A, ClientPacket::Embark(req.clone()));
        for _ in 0..roll(seed, 2, 4) {
            w.tick();
        }
        w.reconnect_between_looks();
        w.drain();
        let epochs: Vec<u64> = w
            .sent_since(mark)
            .iter()
            .filter_map(|m| match m {
                HostMsg::Attach { epoch, .. } => Some(*epoch),
                _ => None,
            })
            .collect();
        assert_eq!(epochs, vec![2], "seed {seed}: one fresh Attach after the reconnect");
        let results = w.embark_results(A);
        assert_eq!(results.len(), 1, "seed {seed}: one EmbarkResult: {results:?}");
        assert!(results[0].is_ok(), "seed {seed}: embarked");
        assert_eq!(w.player_sites().len(), 1, "seed {seed}: one site");
        assert_eq!(w.players_in(req.region), vec![A], "seed {seed}: moved in once");
        w.check_settled("reattached");
    });
}

/// Lifecycle messages the host sent while it still believed a dead connection up (a
/// region starting, or stopping and parking with its bodies) land on the new connection
/// ahead of the `Attach` (svc-link is at-least-once around a reconnect), or are lost with
/// the old one. Either way each person keeps at most one body throughout and ends in
/// exactly one place.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lifecycle_on_stale_link_lands_before_attach() {
    let ahead = std::sync::atomic::AtomicU64::new(0);
    each_seed("lifecycle_on_stale_link_lands_before_attach", |seed| {
        let mut w = World::with(seed, &[A]);
        let site_region = w.history().sites().next().unwrap().region;
        let parking = roll(seed, 5, 2) == 1;
        if parking {
            w.packet(A, ClientPacket::RequestRegionConnection(site_region));
            w.drain();
            w.check_settled("running");
        }
        let mark = w.mark();
        w.begin_stale();
        if parking {
            w.packet(A, ClientPacket::ReleaseRegionConnection(site_region));
            w.now_ms += UNLOAD_GRACE_MS;
        } else {
            w.packet(A, ClientPacket::RequestRegionConnection(site_region));
        }
        for round in 0..2 + roll(seed, 6, 4) {
            w.tick();
            w.check_lagging(&format!("stale, round {round}"));
        }
        let landed = w.end_stale();
        for round in 0..DRAIN_ROUNDS {
            w.tick();
            w.check_lagging(&format!("reattached, round {round}"));
        }
        w.drain();
        w.check_settled("reattached");
        let sent = w.sent_since(mark);
        let attach_at = sent.iter().position(|m| matches!(m, HostMsg::Attach { .. })).expect("an Attach");
        let lifecycle = |m: &HostMsg| {
            matches!(m, HostMsg::RegionStarted { .. } | HostMsg::RegionStopping { .. } | HostMsg::RegionParked { .. })
        };
        if sent[..attach_at].iter().any(lifecycle) {
            assert!(landed.iter().any(lifecycle), "seed {seed}: only stale messages precede the Attach");
            ahead.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // The region runs (or restarts) afterwards with each resident once.
        w.packet(A, ClientPacket::RequestRegionConnection(site_region));
        w.drain();
        w.check_settled("after");
    });
    assert!(ahead.into_inner() > 0, "no seed landed a lifecycle message ahead of the Attach");
}

/// A request already sent when the link drops — its reply lost with the connection, or
/// still on its way — is sent again after the `Attach`: the coordinator answers it from
/// its memo, and the host takes the first reply only (a second one for the same id is
/// dropped).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn sent_request_replayed_after_reconnect_answers_once() {
    let replayed = std::sync::atomic::AtomicU64::new(0);
    each_seed("sent_request_replayed_after_reconnect_answers_once", |seed| {
        let mut w = World::with(seed, &[A]);
        let req = w.embark_request();
        let mark = w.mark();
        w.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::Embark(req.clone()), A), w.now_ms);
        for _ in 0..roll(seed, 2, 3) {
            w.manager.pump_world(w.now_ms);
        }
        // Down before anything else pumps the link (every region output handled does);
        // a dropped connection may lose the reply on it.
        w.set_up(false);
        if roll(seed, 5, 2) == 0 {
            w.link().lose_in_transit();
        }
        w.tick();
        w.set_up(true);
        w.drain();
        let embark_requests: Vec<HostMsg> = w
            .sent_since(mark)
            .into_iter()
            .filter(|m| matches!(m, HostMsg::Request { req: WorldRequest::Embark(_), .. }))
            .collect();
        if embark_requests.len() > 1 {
            replayed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let site = w.embark_results(A);
        assert!(matches!(site[..], [Ok(_)]), "seed {seed}: one EmbarkResult: {site:?}");
        assert_eq!(w.player_sites().len(), 1, "seed {seed}: one site");
        assert_eq!(w.players_in(req.region), vec![A], "seed {seed}: one body");
        // The same request once more (a replay racing its first reply): answered from
        // the memo, the second reply dropped by id.
        w.link().send(embark_requests[0].clone());
        w.drain();
        assert!(w.embark_results(A).is_empty(), "seed {seed}: the second reply is dropped");
        assert_eq!(w.player_sites().len(), 1, "seed {seed}: still one site");
        assert_eq!(w.players_in(req.region), vec![A], "seed {seed}: still one body");
    });
    assert!(replayed.into_inner() > 0, "no seed replayed a sent request");
}

#[test]
fn request_times_out_with_world_unavailable() {
    let mut w = World::with(0, &[A]);
    let req = w.embark_request();
    w.set_up(false);
    w.packet(A, ClientPacket::Embark(req.clone()));
    w.packet(A, ClientPacket::RequestLegends(LegendsQuery { filter: LegendsFilter::All, before: None, limit: 5 }));
    w.now_ms += WORLD_REQUEST_TIMEOUT_SECS * 1000 - TICK_RATE;
    w.tick();
    assert_eq!(w.embark_results(A), vec![Err(EmbarkRefusal::WorldUnavailable)]);
    assert_eq!(w.manager.pending_world_requests(), 0, "forgotten");
    assert_eq!(w.manager.world_outbox_len(), 0, "out of the outbox");
    w.set_up(true);
    w.drain();
    assert!(w.embark_results(A).is_empty(), "never answered again");
    assert_eq!(w.legends(A), 0, "a query just goes unanswered");
    assert!(w.player_sites().is_empty(), "never founded");
    // The world is back: the player may embark.
    assert!(w.ask(A, ClientPacket::Embark(req)).is_ok());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn return_in_use_across_reconnect() {
    each_seed("return_in_use_across_reconnect", |seed| {
        let mut w = World::with(seed, &[A, B]);
        let site = w.ask(A, ClientPacket::Embark(w.embark_request())).expect("embarked");
        assert_eq!(w.ask(B, ClientPacket::ReturnTo(site)), Err(EmbarkRefusal::InUse), "seed {seed}: A plays it");
        w.send(ServerEvent::ClientDisconnected(A));
        assert_eq!(w.ask(B, ClientPacket::ReturnTo(site)), Ok(site), "seed {seed}: A is offline");
        // A comes back with its record's claim: B plays the fortress now.
        w.send(ServerEvent::ClientConnected { client: A2, player: PlayerId(A as u64), record: None });
        w.drain();
        assert_eq!(w.coordinator().claim_of(PlayerId(A as u64)), None, "seed {seed}: A's claim is gone");
        assert_eq!(w.ask(A2, ClientPacket::ReturnTo(site)), Err(EmbarkRefusal::InUse), "seed {seed}");
    });
}

/// A history directive on the link when it drops (a spawn into a running region) reaches
/// the host after its `Attach`: the attach re-decided the region from its bodies, so the
/// host drops it rather than make a second body; restarted, the region spawns the person
/// exactly once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn directive_on_the_link_at_reconnect_is_dropped() {
    let stale = std::sync::atomic::AtomicU64::new(0);
    each_seed("directive_on_the_link_at_reconnect_is_dropped", |seed| {
        let mut w = World::with(seed, &[A]);
        let site_region = w.history().sites().next().unwrap().region;
        w.packet(A, ClientPacket::RequestRegionConnection(site_region));
        w.drain();
        w.check_settled("running");
        // A site founded into the running region: its founder's spawn is a directive,
        // decided at the next pump and held on the link by the seed.
        let history = history_mut_of(&mut w.manager);
        let born = years_before(history.now(), 20);
        let founder = history.add_founder(Sex::Male, born);
        let civ = history.sites().next().unwrap().civ;
        let new_site = history.found_site(civ, site_region, &[founder], None, true);
        for _ in 0..roll(seed, 3, 3) {
            w.manager.pump_world(w.now_ms);
        }
        let before = w.manager.world_link_counters().stale_dropped;
        w.set_up(false);
        for round in 0..roll(seed, 4, 3) {
            w.tick();
            w.check_lagging(&format!("down, round {round}"));
        }
        w.set_up(true);
        for round in 0..DRAIN_ROUNDS {
            w.tick();
            w.check_lagging(&format!("reattached, round {round}"));
        }
        w.drain();
        w.check_settled("reattached");
        if w.manager.world_link_counters().stale_dropped > before {
            stale.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // Restarted: the founder lands exactly once, whether its spawn landed before the
        // drop or was superseded by the attach.
        w.packet(A, ClientPacket::ReleaseRegionConnection(site_region));
        w.now_ms += UNLOAD_GRACE_MS;
        w.drain();
        assert!(!w.manager.running_regions().contains(&site_region), "seed {seed}: parked");
        assert_eq!(w.history().person(founder).unwrap().location, Location::AtSite(new_site), "seed {seed}");
        w.packet(A, ClientPacket::RequestRegionConnection(site_region));
        w.drain();
        w.check_settled("restarted");
        assert_eq!(bodies(&mut w.manager).get(&founder), Some(&vec![site_region]), "seed {seed}: one body");
    });
    let stale = stale.into_inner();
    assert!(stale > 0, "no seed had the spawn on the link at the reconnect");
    assert!(stale < SEEDS, "no seed landed the spawn before the drop");
}

/// The people each region was attached with, from the `Attach`es handed over since `mark`.
fn attached_people(w: &World, mark: usize) -> Vec<Vec<game::RunningRegion>> {
    w.sent_since(mark)
        .into_iter()
        .filter_map(|m| match m {
            HostMsg::Attach { running, .. } => Some(running),
            _ => None,
        })
        .collect()
}

/// The people with a body in `r`, sorted (what an exact list holds).
fn people_in(w: &mut World, r: RegionCoords) -> Vec<PersonId> {
    bodies(&mut w.manager).into_iter().filter(|(_, at)| at.contains(&r)).map(|(p, _)| p).collect()
}

/// A region's people list emitted for a save cut just before the link came back (its
/// `PeoplePositions` in flight at link-up) is not the region's answer to the attach:
/// only its `PeopleListed` of that epoch is.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_positions_in_flight_do_not_satisfy_the_attach() {
    use game::RegionSpawner;
    each_seed("cut_positions_in_flight_do_not_satisfy_the_attach", |seed| {
        let mut w = World::with(seed, &[A]);
        let site_region = w.history().sites().next().unwrap().region;
        w.packet(A, ClientPacket::RequestRegionConnection(site_region));
        w.drain();
        // A cut whose answers (each region's people, then its snapshot) are held back.
        let now = w.manager.spawner_mut().world_tick();
        w.manager.begin_cut(now).expect("no cut pending");
        let mut held = Vec::new();
        for _ in 0..64 {
            w.manager.spawner_mut().pump();
            w.manager.spawner_mut().tick_all();
            held.extend(w.region_out.try_iter());
            let positions = held.iter().any(|(r, o)| *r == site_region && matches!(o, RegionOutput::PeoplePositions(_)));
            if positions && held.iter().any(|(r, o)| *r == site_region && matches!(o, RegionOutput::CutSnapshot(..))) {
                break;
            }
        }
        assert!(
            held.iter().any(|(r, o)| *r == site_region && matches!(o, RegionOutput::PeoplePositions(_))),
            "seed {seed}: the cut listed the site's people"
        );
        // The link drops and comes back: the attach asks every region; the cut's lists
        // arrive first.
        w.set_up(false);
        w.manager.pump_world(w.now_ms);
        let mark = w.mark();
        w.set_up(true);
        w.manager.pump_world(w.now_ms);
        for (r, o) in held {
            w.manager.handle_region_output(r, o, w.now_ms);
        }
        // Every other region lists its people; the site region has not read its ask yet.
        w.manager.spawner_mut().hold_inputs(site_region, 1);
        w.manager.spawner_mut().pump();
        let answers: Vec<_> = w.region_out.try_iter().collect();
        for (r, o) in answers {
            w.manager.handle_region_output(r, o, w.now_ms);
        }
        assert!(attached_people(&w, mark).is_empty(), "seed {seed}: no Attach on the cut's lists");
        w.manager.spawner_mut().hold_inputs(site_region, 0);
        assert!(!w.manager.world_attached(), "seed {seed}");
        w.drain();
        let attaches = attached_people(&w, mark);
        assert_eq!(attaches.len(), 1, "seed {seed}: one Attach");
        let listed = attaches[0].iter().find(|r| r.rc == site_region).expect("the site region attached");
        let mut listed: Vec<PersonId> = listed.people.iter().map(|(p, _)| *p).collect();
        listed.sort();
        assert_eq!(listed, people_in(&mut w, site_region), "seed {seed}: attached with exactly its bodies");
        w.check_settled("reattached");
    });
}

/// A region that lists its people late (its inputs unread past `ATTACH_WAIT_MS`) is
/// waited for, never attached with a guessed list; once it answers it is attached with
/// exactly its bodies, and nobody gets a second one.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn late_listing_region_is_attached_exactly() {
    each_seed("late_listing_region_is_attached_exactly", |seed| {
        let mut w = World::with(seed, &[A]);
        let site_region = w.history().sites().next().unwrap().region;
        w.packet(A, ClientPacket::RequestRegionConnection(site_region));
        w.drain();
        let census = people_in(&mut w, site_region);
        assert!(!census.is_empty(), "seed {seed}: the site's people stand there");
        w.set_up(false);
        w.manager.pump_world(w.now_ms);
        let late_rounds = (game::ATTACH_WAIT_MS / TICK_RATE) as u32 + 3 + roll(seed, 6, 4) as u32;
        w.manager.spawner_mut().hold_inputs(site_region, late_rounds);
        let mark = w.mark();
        w.set_up(true);
        for round in 0..late_rounds.saturating_sub(1) {
            w.tick();
            w.check_lagging(&format!("waiting, round {round}"));
            assert!(attached_people(&w, mark).is_empty(), "seed {seed} round {round}: no Attach before the list");
        }
        assert!(!w.manager.world_attached(), "seed {seed}: still waiting past ATTACH_WAIT_MS");
        for round in 0..DRAIN_ROUNDS {
            w.tick();
            w.check_lagging(&format!("attaching, round {round}"));
        }
        w.drain();
        let attaches = attached_people(&w, mark);
        assert_eq!(attaches.len(), 1, "seed {seed}: one Attach");
        let listed = attaches[0].iter().find(|r| r.rc == site_region).expect("the site region attached");
        let mut listed: Vec<PersonId> = listed.people.iter().map(|(p, _)| *p).collect();
        listed.sort();
        assert_eq!(listed, census, "seed {seed}: attached with exactly its bodies");
        w.check_settled("reattached");
        assert_eq!(people_in(&mut w, site_region), census, "seed {seed}: the same bodies, once each");
    });
}

/// An embark the coordinator applied whose reply was lost, the link down past the
/// timeout: the player is told `WorldUnavailable`; after the link is back the request is
/// sent again (answered from the memo), its claim given back, and the player may embark
/// again. The first request founded exactly one site.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn expired_sent_embark_gives_back_its_claim() {
    let ran = std::sync::atomic::AtomicU64::new(0);
    each_seed("expired_sent_embark_gives_back_its_claim", |seed| {
        let mut w = World::with(seed, &[A]);
        let req = w.embark_request();
        w.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::Embark(req), A), w.now_ms);
        if w.manager.pending_world_requests() == 0 {
            return; // Answered at once (no lag by this seed): nothing in flight to lose.
        }
        ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        w.set_up(false);
        w.link().lose_in_transit();
        let site = w.player_sites();
        assert_eq!(site.len(), 1, "seed {seed}: the coordinator applied it");
        assert_eq!(w.coordinator().claim_of(PlayerId(A as u64)), Some(game::Claim::Fortress(site[0])));
        w.now_ms += WORLD_REQUEST_TIMEOUT_SECS * 1000;
        w.tick();
        assert_eq!(w.embark_results(A), vec![Err(EmbarkRefusal::WorldUnavailable)], "seed {seed}");
        assert_eq!(w.manager.pending_world_requests(), 0, "seed {seed}: forgotten toward the client");
        for _ in 0..roll(seed, 7, 4) {
            w.tick();
        }
        w.set_up(true);
        w.drain();
        assert_eq!(w.coordinator().claim_of(PlayerId(A as u64)), None, "seed {seed}: the claim was given back");
        assert!(w.embark_results(A).is_empty(), "seed {seed}: the late reply is not shown");
        assert_eq!(w.player_sites(), site, "seed {seed}: one site, the first request's");
        assert!(w.history().returnable(site[0]), "seed {seed}: the site stays");
        assert!(w.ask(A, ClientPacket::Embark(w.embark_request())).is_ok(), "seed {seed}: may embark again");
    });
    assert!(ran.into_inner() > 0, "no seed had the reply in flight");
}

/// The coordinator holds a claim the host does not (B-C1), with nothing of the player
/// unsettled on the host: (a) it restarts from a save taken before A retired (the
/// release lost); (b) an all-in-one crash after an embark: the coordinator saved the
/// embark's reply, the restarted host has no record of the player. Either way the host's
/// report is the truth: A is never locked out with `AlreadyHasFortress`, and the fortress
/// has at most one player.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coordinator_claim_older_than_the_host_never_locks_out() {
    each_seed("coordinator_claim_older_than_the_host_never_locks_out", |seed| {
        let a = PlayerId(A as u64);
        let mut w = World::with(seed, &[A, B]);
        let site = w.ask(A, ClientPacket::Embark(w.embark_request())).expect("embarked");
        let embarked = w.coordinator().state();
        // (a) A retires; the coordinator then restarts from the save before the release.
        w.packet(A, ClientPacket::Retire);
        w.drain();
        assert_eq!(w.coordinator().claim_of(a), None, "seed {seed}: released");
        let now_ms = w.now_ms;
        let older = Coordinator::from_state(embarked.clone(), SocietyGrid::from_world_map(&strip()), now_ms);
        *w.link().coordinator_mut() = older.expect("the state loads");
        assert_eq!(w.coordinator().claim_of(a), Some(game::Claim::Fortress(site)), "seed {seed}: the old claim");
        w.reconnect_between_looks();
        w.drain();
        assert_eq!(w.coordinator().claim_of(a), None, "seed {seed} (a): the stale claim is dropped");
        assert_eq!(w.ask(B, ClientPacket::ReturnTo(site)), Ok(site), "seed {seed} (a): the fortress is free");
        assert_eq!(w.ask(A, ClientPacket::ReturnTo(site)), Err(EmbarkRefusal::InUse), "seed {seed} (a): one player");
        assert!(w.ask(A, ClientPacket::Embark(w.embark_request())).is_ok(), "seed {seed} (a): A is not locked out");
        // (b) A new host (no record of A) over the coordinator as saved with the embark.
        let mut w = World::new(seed);
        let restarted = Coordinator::from_state(embarked, SocietyGrid::from_world_map(&strip()), 0);
        *w.link().coordinator_mut() = restarted.expect("the state loads");
        w.send(ServerEvent::connected(A));
        w.send(ServerEvent::connected(B));
        w.drain();
        w.inbox.clear();
        assert_eq!(w.coordinator().claim_of(a), None, "seed {seed} (b): the claim the host lost is dropped");
        assert_eq!(w.ask(B, ClientPacket::ReturnTo(site)), Ok(site), "seed {seed} (b): the fortress is free");
        assert!(w.ask(A, ClientPacket::Embark(w.embark_request())).is_ok(), "seed {seed} (b): A is not locked out");
    });
}

/// An embark sent on a connection that died unnoticed times out (its player told
/// `WorldUnavailable`); by the seed the old connection lost it or delivered it. After the
/// re-attach the host only settles it (B-I1): lost, it is refused unapplied and never
/// founds a site later; delivered, its memo answers and the claim is given back. Either
/// way the player holds nothing and may embark again.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn expired_embark_is_settled_never_executed() {
    let lost = std::sync::atomic::AtomicU64::new(0);
    each_seed("expired_embark_is_settled_never_executed", |seed| {
        let mut w = World::with(seed, &[A]);
        w.begin_stale();
        w.packet(A, ClientPacket::Embark(w.embark_request()));
        w.now_ms += WORLD_REQUEST_TIMEOUT_SECS * 1000;
        w.tick();
        assert_eq!(w.embark_results(A), vec![Err(EmbarkRefusal::WorldUnavailable)], "seed {seed}");
        let landed = w.end_stale();
        let delivered = landed.iter().any(|m| matches!(m, HostMsg::Request { req: WorldRequest::Embark(_), .. }));
        w.drain();
        assert!(w.embark_results(A).is_empty(), "seed {seed}: the settle's answer is not shown");
        assert_eq!(w.coordinator().claim_of(PlayerId(A as u64)), None, "seed {seed}: no claim");
        if delivered {
            assert_eq!(w.player_sites().len(), 1, "seed {seed}: delivered, it founded its site once");
        } else {
            lost.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert!(w.player_sites().is_empty(), "seed {seed}: lost, it never runs");
        }
        let sites = w.player_sites().len();
        assert!(w.ask(A, ClientPacket::Embark(w.embark_request())).is_ok(), "seed {seed}: may embark again");
        assert_eq!(w.player_sites().len(), sites + 1, "seed {seed}: the new embark founds one site");
    });
    assert!(lost.into_inner() > 0, "no seed lost the embark with the old connection");
}
