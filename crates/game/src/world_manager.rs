//! World-level management: which regions run, who is subscribed to what,
//! where players' home regions are, and the parking lot for cycled-out
//! regions. Threadless: a `RegionSpawner` decides how regions actually run
//! (OS threads on the server, inline for wasm/tests), and time arrives as
//! an explicit `now_ms` so the core stays deterministic and wasm-safe.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::{
    Chunk, ChunkCoords, ClientId, ClientPacket, GameEventKind, Region, RegionCoords, RegionInput,
    RegionOutput, RegionRunner, RegionSeed, SerializedRegion, ServerPacket,
};

pub const SPAWN_REGION: RegionCoords = RegionCoords { x: 0, z: 0 };
pub const UNLOAD_GRACE_MS: u64 = 5000;

/// Internal event handled by the world manager. (Moved from
/// crates/server/src/main.rs; ServerTickTimer is gone — regions self-tick.)
#[derive(Debug)]
pub enum ServerEvent {
    ClientPacket(ClientPacket, ClientId),
    ClientConnected(ClientId),
    ClientDisconnected(ClientId),
}

pub type RegionGenerator = Box<dyn FnMut(RegionCoords) -> Vec<(ChunkCoords, Chunk)> + Send>;

pub trait RegionSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput>;

    /// Reclaim a stopped region's resources (join its thread). Called after
    /// `RegionOutput::Stopped` or when a dead region is detected.
    fn reap(&mut self, _id: RegionCoords) {}
}

/// Runs regions inline in the caller's thread: the wasm LocalServer and the
/// headless tests. `pump()`/`tick_all()` stand in for thread scheduling.
#[derive(Default)]
pub struct InlineSpawner {
    runners: BTreeMap<RegionCoords, (Receiver<RegionInput>, RegionRunner)>,
}

impl InlineSpawner {
    pub fn pump(&mut self) {
        let ids: Vec<RegionCoords> = self.runners.keys().copied().collect();
        for id in ids {
            let mut stopped = false;
            if let Some((recv, runner)) = self.runners.get_mut(&id) {
                while let Ok(input) = recv.try_recv() {
                    if !runner.handle_input(input) {
                        stopped = true;
                        break;
                    }
                }
            }
            if stopped {
                self.runners.remove(&id);
            }
        }
    }

    pub fn tick_all(&mut self) {
        for (_, (_, runner)) in self.runners.iter_mut() {
            runner.tick();
        }
    }

    pub fn running(&self) -> Vec<RegionCoords> {
        self.runners.keys().copied().collect()
    }

    /// Test support: make a region's channel dead without a Shutdown
    /// handshake, simulating a crashed region thread.
    pub fn kill(&mut self, id: RegionCoords) {
        self.runners.remove(&id);
    }

    /// Test support: run a closure against a live region (state assertions
    /// and teleports in headless harnesses).
    pub fn with_region(&mut self, id: RegionCoords, f: impl FnOnce(&mut Region)) {
        if let Some((_, runner)) = self.runners.get_mut(&id) {
            f(runner.region_mut());
        }
    }
}

impl RegionSpawner for InlineSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput> {
        let (send, recv) = unbounded();
        let runner = RegionRunner::new(id, seed.into_region(id), out);
        self.runners.insert(id, (recv, runner));
        send
    }
}

#[derive(Default)]
struct Session {
    subscribed: BTreeSet<RegionCoords>,
}

struct RegionLink {
    input: Sender<RegionInput>,
    subscribers: BTreeSet<ClientId>,
    /// Set when the region lost its last keep-alive reason; cleared on
    /// resubscribe. Grace-period timestamp base.
    empty_since_ms: Option<u64>,
    /// Shutdown sent, Stopped not yet received. Subscribes arriving in this
    /// window queue in `resubscribe_pending` and re-run after Stopped.
    stopping: bool,
    resubscribe_pending: Vec<ClientId>,
}

pub struct WorldManager<S: RegionSpawner> {
    spawner: S,
    generator: RegionGenerator,
    regions: BTreeMap<RegionCoords, RegionLink>,
    parked: BTreeMap<RegionCoords, SerializedRegion>,
    /// Connected clients and their subscriptions.
    sessions: BTreeMap<ClientId, Session>,
    /// Survives disconnects: which region holds this client's player entity.
    /// (The player parks with its region; reconnects create nothing.)
    homes: BTreeMap<ClientId, RegionCoords>,
    out: Sender<(Option<ClientId>, ServerPacket)>,
    region_out_send: Sender<(RegionCoords, RegionOutput)>,
}

impl<S: RegionSpawner> WorldManager<S> {
    pub fn new(
        spawner: S,
        generator: RegionGenerator,
        out: Sender<(Option<ClientId>, ServerPacket)>,
        region_out_send: Sender<(RegionCoords, RegionOutput)>,
    ) -> Self {
        Self {
            spawner,
            generator,
            regions: BTreeMap::new(),
            parked: BTreeMap::new(),
            sessions: BTreeMap::new(),
            homes: BTreeMap::new(),
            out,
            region_out_send,
        }
    }

    /// Returns false when the server should quit (a Quit game event).
    pub fn handle_server_event(&mut self, ev: ServerEvent, now_ms: u64) -> bool {
        match ev {
            ServerEvent::ClientConnected(id) => {
                self.sessions.insert(id, Session::default());
                if !self.homes.contains_key(&id) {
                    // Server-authoritative player creation, once per client.
                    self.ensure_running(SPAWN_REGION);
                    self.homes.insert(id, SPAWN_REGION);
                    self.send_to_region(
                        SPAWN_REGION,
                        RegionInput::Event(GameEventKind::CreateClient(id)),
                    );
                } else {
                    // Reconnect: the player already exists in its home
                    // region (running or parked); nothing to create.
                    log::info!("client {id} reconnected");
                }
            }
            ServerEvent::ClientDisconnected(id) => {
                if let Some(session) = self.sessions.remove(&id) {
                    for rc in session.subscribed {
                        self.unsubscribe_link(id, rc, now_ms);
                    }
                }
                // The home region may have just lost its keep-alive reason.
                if let Some(home) = self.homes.get(&id).copied() {
                    self.refresh_keepalive(home, now_ms);
                }
            }
            ServerEvent::ClientPacket(packet, id) => match packet {
                ClientPacket::RequestPlayerRegion => {
                    let home = self.homes.get(&id).copied();
                    let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(home, id)));
                }
                ClientPacket::RequestRegionConnection(rc) => self.subscribe(id, rc),
                ClientPacket::ReleaseRegionConnection(rc) => self.unsubscribe(id, rc, now_ms),
                ClientPacket::GameEvent(event) => match event.kind {
                    GameEventKind::Tick => {}
                    GameEventKind::Quit => return false,
                    kind @ GameEventKind::PlayerInput(..) => {
                        // Manager-authoritative routing: the client's stamp
                        // lags its own predicted handoff; homes is truth.
                        let GameEventKind::PlayerInput(cid, _) = &kind else { unreachable!() };
                        match self.homes.get(cid).copied() {
                            Some(home) if self.regions.contains_key(&home) => {
                                self.send_to_region(home, RegionInput::Event(kind));
                            }
                            _ => log::debug!("dropping input from {cid}: home not running"),
                        }
                    }
                    // Manager-internal event kinds: a client must never inject
                    // these directly. Drop rather than forward to a region.
                    GameEventKind::EntityArrived(_) | GameEventKind::GhostUpdate(_) => {
                        log::debug!(
                            "dropping internal event kind sent by client {id} for {:?}",
                            event.region_id
                        );
                    }
                    kind => {
                        let subscribed = self
                            .sessions
                            .get(&id)
                            .map_or(false, |s| s.subscribed.contains(&event.region_id));
                        if subscribed && self.regions.contains_key(&event.region_id) {
                            self.send_to_region(event.region_id, RegionInput::Event(kind));
                        } else {
                            log::debug!(
                                "dropping event from client {id} for unsubscribed region {:?}",
                                event.region_id
                            );
                        }
                    }
                },
            },
        }
        true
    }

    pub fn handle_region_output(&mut self, rc: RegionCoords, output: RegionOutput, now_ms: u64) {
        match output {
            RegionOutput::EventProcessed(event) => {
                if let Some(link) = self.regions.get(&rc) {
                    for client in &link.subscribers {
                        let _ = self
                            .out
                            .send((Some(*client), ServerPacket::GameEvent(event.clone())));
                    }
                }
            }
            RegionOutput::Snapshot(client, rollback) => {
                let _ = self.out.send((Some(client), ServerPacket::Region(rc, rollback)));
            }
            RegionOutput::SyncClock { tick_rate, tick } => {
                if let Some(link) = self.regions.get(&rc) {
                    for client in &link.subscribers {
                        let _ = self.out.send((
                            Some(*client),
                            ServerPacket::SyncClock(rc, tick_rate, tick, Duration::ZERO),
                        ));
                    }
                }
            }
            RegionOutput::Stopped(serialized) => {
                self.parked.insert(rc, serialized);
                let pending = self
                    .regions
                    .remove(&rc)
                    .map(|l| l.resubscribe_pending)
                    .unwrap_or_default();
                self.spawner.reap(rc);
                for client in pending {
                    // A client asked for this region while it was stopping:
                    // now that the state is parked, cycle it right back in.
                    self.subscribe(client, rc);
                }
                let _ = now_ms;
            }
            RegionOutput::Departures(list) => {
                for (mut bundle, target) in list {
                    let client = bundle.client.as_ref().map(|(c, _)| *c);
                    bundle.isometry = crate::rebase_isometry(&bundle.isometry, rc, target);
                    // Arrivals ALWAYS wake the target (parked blob or gen).
                    self.ensure_running(target);
                    self.send_to_region(
                        target,
                        RegionInput::Event(GameEventKind::EntityArrived(bundle)),
                    );
                    if let Some(c) = client {
                        self.homes.insert(c, target);
                        let _ = self
                            .out
                            .send((Some(c), ServerPacket::PlayerRegion(Some(target), c)));
                        // The old home may have just lost its keep-alive
                        // reason; the new one just gained it.
                        self.refresh_keepalive(rc, now_ms);
                        self.refresh_keepalive(target, now_ms);
                    }
                }
            }
            RegionOutput::GhostUpdates(list) => {
                for (mut data, target) in list {
                    // Ghost updates NEVER wake a region.
                    let running = self
                        .regions
                        .get(&target)
                        .map_or(false, |link| !link.stopping);
                    if !running {
                        continue;
                    }
                    data.isometry = crate::rebase_isometry(&data.isometry, rc, target);
                    self.send_to_region(target, RegionInput::Event(GameEventKind::GhostUpdate(data)));
                }
            }
        }
    }

    /// Grace-period unloads: a region with no subscribers and no connected
    /// client's player entity parks after UNLOAD_GRACE_MS.
    pub fn maintain(&mut self, now_ms: u64) {
        let connected_homes: BTreeSet<RegionCoords> = self
            .sessions
            .keys()
            .filter_map(|c| self.homes.get(c).copied())
            .collect();
        let expired: Vec<RegionCoords> = self
            .regions
            .iter()
            .filter(|(rc, link)| {
                !link.stopping
                    && link.subscribers.is_empty()
                    && !connected_homes.contains(*rc)
                    && link
                        .empty_since_ms
                        .map_or(false, |t| now_ms.saturating_sub(t) >= UNLOAD_GRACE_MS)
            })
            .map(|(rc, _)| *rc)
            .collect();
        for rc in expired {
            let link = self.regions.get_mut(&rc).unwrap();
            link.stopping = true;
            if link.input.send(RegionInput::Shutdown).is_err() {
                // Already dead: nothing to park (state lost), just clean up.
                log::error!("region {:?} died before shutdown", rc);
                self.regions.remove(&rc);
                self.spawner.reap(rc);
            }
        }
    }

    pub fn shutdown_all(&mut self) {
        for (rc, link) in self.regions.iter_mut() {
            if !link.stopping {
                link.stopping = true;
                if link.input.send(RegionInput::Shutdown).is_err() {
                    log::error!("region {:?} died before shutdown", rc);
                }
            }
        }
    }

    pub fn running_regions(&self) -> Vec<RegionCoords> {
        self.regions.keys().copied().collect()
    }

    pub fn parked_regions(&self) -> Vec<RegionCoords> {
        self.parked.keys().copied().collect()
    }

    pub fn spawner_mut(&mut self) -> &mut S {
        &mut self.spawner
    }

    fn ensure_running(&mut self, rc: RegionCoords) {
        if self.regions.contains_key(&rc) {
            return;
        }
        let chunks = (self.generator)(rc);
        let seed = match self.parked.remove(&rc) {
            Some(serialized) => RegionSeed::Parked(serialized, chunks),
            None => RegionSeed::Fresh(chunks),
        };
        let input = self.spawner.spawn(rc, seed, self.region_out_send.clone());
        self.regions.insert(
            rc,
            RegionLink {
                input,
                subscribers: BTreeSet::new(),
                empty_since_ms: None,
                stopping: false,
                resubscribe_pending: Vec::new(),
            },
        );
    }

    fn subscribe(&mut self, client: ClientId, rc: RegionCoords) {
        // Invariant: a client appears in `link.subscribers` (or in
        // `resubscribe_pending`) only while it has a live session whose
        // `subscribed` set contains `rc`. Sessionless subscribes — ghost
        // packets, or a Stopped-replay for a client that disconnected during
        // the stopping window — are dropped here; otherwise they'd pin the
        // region forever with a subscriber no disconnect can remove.
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!(
                "dropping subscribe from sessionless client {client} for {:?}",
                rc
            );
            return;
        };
        session.subscribed.insert(rc);
        self.ensure_running(rc);
        let link = self.regions.get_mut(&rc).unwrap();
        if link.stopping {
            // Recorded in session.subscribed above, so a disconnect (or
            // release) before Stopped arrives still reaches
            // unsubscribe_link and clears the pending intent.
            link.resubscribe_pending.push(client);
            return;
        }
        link.subscribers.insert(client);
        link.empty_since_ms = None;
        self.send_to_region(rc, RegionInput::RequestSnapshot(client));
    }

    fn unsubscribe(&mut self, client: ClientId, rc: RegionCoords, now_ms: u64) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.subscribed.remove(&rc);
        }
        self.unsubscribe_link(client, rc, now_ms);
    }

    fn unsubscribe_link(&mut self, client: ClientId, rc: RegionCoords, now_ms: u64) {
        let Some(link) = self.regions.get_mut(&rc) else { return };
        link.subscribers.remove(&client);
        // The client may only exist as a pending intent (subscribed during
        // the stopping window); clear that too so a Stopped-replay can't
        // resurrect the subscription.
        link.resubscribe_pending.retain(|c| c != &client);
        self.refresh_keepalive(rc, now_ms);
    }

    /// Re-evaluate whether `rc` still has a keep-alive reason; start the
    /// grace timer if not.
    fn refresh_keepalive(&mut self, rc: RegionCoords, now_ms: u64) {
        let is_home = self
            .sessions
            .keys()
            .any(|c| self.homes.get(c) == Some(&rc));
        let Some(link) = self.regions.get_mut(&rc) else { return };
        if link.subscribers.is_empty() && !is_home {
            link.empty_since_ms.get_or_insert(now_ms);
        } else {
            link.empty_since_ms = None;
        }
    }

    /// Route an input to a running region; a failed send means the region
    /// thread died — respawn it (parked state if any, else regenerated) and
    /// resnapshot every subscriber. The failed input itself is dropped; the
    /// snapshot resync covers the gap.
    fn send_to_region(&mut self, rc: RegionCoords, input: RegionInput) {
        let Some(link) = self.regions.get(&rc) else {
            log::debug!("send_to_region: {:?} not running", rc);
            return;
        };
        if link.input.send(input).is_ok() {
            return;
        }
        log::error!("region {:?} thread died; respawning", rc);
        let subscribers = self.regions.remove(&rc).unwrap().subscribers;
        self.spawner.reap(rc);
        self.ensure_running(rc);
        let link = self.regions.get_mut(&rc).unwrap();
        link.subscribers = subscribers.clone();
        for client in subscribers {
            let _ = link.input.send(RegionInput::RequestSnapshot(client));
        }
    }
}
