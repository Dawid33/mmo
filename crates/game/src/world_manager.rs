//! World-level management: which regions run, who is subscribed to what,
//! where players' home regions are, and the parking lot for cycled-out
//! regions. Threadless: a `RegionSpawner` decides how regions actually run
//! (OS threads on the server, inline for wasm/tests), and time arrives as
//! an explicit `now_ms` so the core stays deterministic and wasm-safe.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::{
    Chunk, ChunkCoords, ClientId, ClientPacket, Column, ColumnCoords, FarSnapshot, GameEventKind,
    LodBuildJob, LodBuilder, LodKey, LodPayload, LodResult, Region, RegionCoords, RegionInput,
    RegionOutput, RegionRunner, RegionSeed, SerializedRegion, ServerPacket,
};

pub const SPAWN_REGION: RegionCoords = RegionCoords { x: 0, z: 0 };
pub const UNLOAD_GRACE_MS: u64 = 5000;
/// LRU cap on `lod_cache`; subscribed keys are pinned past this (see
/// `evict_lod_cache`).
pub const LOD_CACHE_MAX_ENTRIES: usize = 8192;

/// Internal event handled by the world manager. (Moved from
/// crates/server/src/main.rs; ServerTickTimer is gone — regions self-tick.)
#[derive(Debug)]
pub enum ServerEvent {
    ClientPacket(ClientPacket, ClientId),
    ClientConnected(ClientId),
    ClientDisconnected(ClientId),
}

pub type RegionGenerator = Box<dyn FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send>;

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
    far_subscribed: BTreeSet<RegionCoords>,
    lod_subscribed: BTreeSet<LodKey>,
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
    /// Far-viewers per region, INDEPENDENT of running/parked state.
    far_subscribers: BTreeMap<RegionCoords, BTreeSet<ClientId>>,
    /// Frozen terrain for parked / never-run regions, so far serving never
    /// wakes a region. Refreshed on FarSnapshotChanged and on park (Stopped).
    far_cache: BTreeMap<RegionCoords, FarSnapshot>,
    /// Connected clients and their subscriptions.
    sessions: BTreeMap<ClientId, Session>,
    /// Survives disconnects: which region holds this client's player entity.
    /// (The player parks with its region; reconnects create nothing.)
    homes: BTreeMap<ClientId, RegionCoords>,
    out: Sender<(Option<ClientId>, ServerPacket)>,
    region_out_send: Sender<(RegionCoords, RegionOutput)>,
    /// LOD subscriptions per key, and the reverse map for disconnects.
    lod_subscribers: BTreeMap<LodKey, BTreeSet<ClientId>>,
    /// key -> RLE payload; LRU-evicted past LOD_CACHE_MAX_ENTRIES,
    /// subscribed keys pinned. `u64` is a monotonically increasing access
    /// stamp (not wall time — determinism).
    lod_cache: BTreeMap<LodKey, (LodPayload, u64)>,
    /// LRU cap on `lod_cache`; defaults to `LOD_CACHE_MAX_ENTRIES`, overridable
    /// via `set_lod_cache_max` (test hook — mirrors `WorldgenRings.table`).
    lod_cache_max: usize,
    lod_access_counter: u64,
    /// Builds submitted, result not yet received — dedups double-requests.
    lod_in_flight: BTreeSet<LodKey>,
    /// Region-keyed builds waiting on a running region's terrain reply.
    lod_pending_terrain: BTreeMap<RegionCoords, BTreeSet<LodKey>>,
    /// Tile-keyed builds waiting on a subset of `key.regions()` whose terrain
    /// reply hasn't landed yet. Settled incrementally by `lod_terrain_ready`
    /// as each awaited region's reply seeds `far_cache`.
    lod_tile_waiting: BTreeMap<LodKey, BTreeSet<RegionCoords>>,
    lod_builder: Box<dyn LodBuilder>,
    lod_results: Receiver<LodResult>,
}

impl<S: RegionSpawner> WorldManager<S> {
    pub fn new(
        spawner: S,
        generator: RegionGenerator,
        out: Sender<(Option<ClientId>, ServerPacket)>,
        region_out_send: Sender<(RegionCoords, RegionOutput)>,
        lod_builder: Box<dyn LodBuilder>,
        lod_results: Receiver<LodResult>,
    ) -> Self {
        Self {
            spawner,
            generator,
            regions: BTreeMap::new(),
            parked: BTreeMap::new(),
            far_subscribers: BTreeMap::new(),
            far_cache: BTreeMap::new(),
            sessions: BTreeMap::new(),
            homes: BTreeMap::new(),
            out,
            region_out_send,
            lod_subscribers: BTreeMap::new(),
            lod_cache: BTreeMap::new(),
            lod_cache_max: LOD_CACHE_MAX_ENTRIES,
            lod_access_counter: 0,
            lod_in_flight: BTreeSet::new(),
            lod_pending_terrain: BTreeMap::new(),
            lod_tile_waiting: BTreeMap::new(),
            lod_builder,
            lod_results,
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
                    for rc in session.far_subscribed {
                        if let Some(set) = self.far_subscribers.get_mut(&rc) {
                            set.remove(&id);
                            if set.is_empty() {
                                self.far_subscribers.remove(&rc);
                            }
                        }
                    }
                    for key in session.lod_subscribed {
                        if let Some(set) = self.lod_subscribers.get_mut(&key) {
                            set.remove(&id);
                            if set.is_empty() {
                                self.lod_subscribers.remove(&key);
                            }
                        }
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
                ClientPacket::RequestFarRegion(rc) => self.far_subscribe(id, rc),
                ClientPacket::ReleaseFarRegion(rc) => self.far_unsubscribe(id, rc),
                ClientPacket::RequestLod(key) => self.lod_subscribe(id, key, now_ms),
                ClientPacket::ReleaseLod(key) => self.lod_unsubscribe(id, key),
                ClientPacket::GameEvent(event) => match event.kind {
                    GameEventKind::Tick => {}
                    GameEventKind::Quit => return false,
                    kind @ (GameEventKind::PlayerInput(..) | GameEventKind::UiAction(..)) => {
                        // Manager-authoritative routing: the client's stamp
                        // lags its own predicted handoff; homes is truth.
                        // UiAction rides the same path as PlayerInput — both
                        // must follow the player across a region handoff.
                        let cid = kind.origin_client().expect("PlayerInput/UiAction carry a client");
                        let home = self.homes.get(&cid).copied();
                        match home {
                            Some(home) if self.regions.contains_key(&home) => {
                                log::info!(
                                    "[server] recv {kind:?} from cid={cid:?} -> route to home {home:?}"
                                );
                                self.send_to_region(home, RegionInput::Event(kind));
                            }
                            _ => log::info!(
                                "[server] DROP {kind:?} from cid={cid:?}: home={home:?} not running (loaded regions: {:?})",
                                self.regions.keys().collect::<Vec<_>>()
                            ),
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
                    if let GameEventKind::PlayerInput(_, ev) = &event.kind {
                        log::info!(
                            "[server] region {rc:?} processed PlayerInput id={} input={ev:?} -> echo to {} subscriber(s): {:?}",
                            event.id,
                            link.subscribers.len(),
                            link.subscribers
                        );
                    }
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
            RegionOutput::FarSnapshot(client, snap) => {
                self.far_cache.insert(rc, snap.clone());
                self.lod_terrain_ready(rc, client);
                let _ = self.out.send((Some(client), ServerPacket::FarRegion(rc, snap)));
            }
            RegionOutput::FarSnapshotChanged(snap) => {
                self.far_cache.insert(rc, snap.clone());
                self.invalidate_region_lod(rc);
                if let Some(subs) = self.far_subscribers.get(&rc) {
                    for client in subs {
                        let _ = self
                            .out
                            .send((Some(*client), ServerPacket::FarRegion(rc, snap.clone())));
                    }
                }
            }
            RegionOutput::Stopped(serialized, far) => {
                self.far_cache.insert(rc, far);
                self.invalidate_region_lod(rc);
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
        let columns = (self.generator)(rc); // now Vec<(ColumnCoords, Column)>
        let seed = match self.parked.remove(&rc) {
            Some(serialized) => RegionSeed::Parked(serialized, columns),
            None => RegionSeed::Fresh(columns),
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

    fn far_subscribe(&mut self, client: ClientId, rc: RegionCoords) {
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!("dropping far subscribe from sessionless client {client} for {:?}", rc);
            return;
        };
        session.far_subscribed.insert(rc);
        self.far_subscribers.entry(rc).or_default().insert(client);
        // Running (and not stopping) → pull live from the region.
        let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
        if running {
            self.send_to_region(rc, RegionInput::RequestFarSnapshot(client));
            return;
        }
        // Parked / never-run → serve from cache without waking; generate on miss.
        //
        // Edge case: if this far-subscribe lands during a region's `stopping`
        // window before any `FarSnapshotChanged`/`Stopped` has seeded
        // `far_cache` for it, the worldgen fallback below may serve
        // pre-edit terrain (a cache miss regenerates from scratch, ignoring
        // in-flight edits made before the region stopped ticking). This
        // self-heals on the next `FarSnapshotChanged` once the region runs
        // again and repopulates the cache.
        let snap = match self.far_cache.get(&rc) {
            Some(snap) => snap.clone(),
            None => {
                let columns = (self.generator)(rc);
                let snap = FarSnapshot::from_columns(columns);
                self.far_cache.insert(rc, snap.clone());
                snap
            }
        };
        let _ = self.out.send((Some(client), ServerPacket::FarRegion(rc, snap)));
    }

    fn far_unsubscribe(&mut self, client: ClientId, rc: RegionCoords) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.far_subscribed.remove(&rc);
        }
        if let Some(set) = self.far_subscribers.get_mut(&rc) {
            set.remove(&client);
            if set.is_empty() {
                self.far_subscribers.remove(&rc);
            }
        }
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

    fn lod_subscribe(&mut self, client: ClientId, key: LodKey, _now_ms: u64) {
        if !crate::lod::LOD_FACTORS.contains(&key.factor) {
            log::debug!("dropping RequestLod with invalid factor {}", key.factor);
            return;
        }
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!("dropping RequestLod from sessionless client {client}");
            return;
        };
        session.lod_subscribed.insert(key);
        self.lod_subscribers.entry(key).or_default().insert(client);
        self.lod_access_counter += 1;
        let stamp = self.lod_access_counter;
        if let Some((payload, access)) = self.lod_cache.get_mut(&key) {
            *access = stamp;
            let _ = self
                .out
                .send((Some(client), ServerPacket::LodData(key, payload.clone())));
            return;
        }
        if !self.lod_in_flight.contains(&key) {
            self.start_lod_build(key, client);
        }
        // else: result fans out to all subscribers when it lands.
    }

    fn lod_unsubscribe(&mut self, client: ClientId, key: LodKey) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.lod_subscribed.remove(&key);
        }
        if let Some(set) = self.lod_subscribers.get_mut(&key) {
            set.remove(&client);
            if set.is_empty() {
                self.lod_subscribers.remove(&key);
            }
        }
    }

    /// Kick off a build for `key`. Region keys: authoritative terrain if the
    /// region has state (running-and-seeded or parked-with-cache), request
    /// terrain if running-but-unseeded, generate otherwise. `client` is the
    /// requester used for terrain round-trips.
    fn start_lod_build(&mut self, key: LodKey, client: ClientId) {
        if key.region_span() == 1 {
            let rc = key.origin_region();
            let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
            if let Some(snap) = self.far_cache.get(&rc) {
                self.lod_in_flight.insert(key);
                self.lod_builder.submit(LodBuildJob::Region {
                    key,
                    chunks: snap.chunks.clone(),
                });
            } else if running {
                // Terrain reply (RegionOutput::FarSnapshot) seeds far_cache
                // and re-enters via lod_terrain_ready.
                self.lod_pending_terrain.entry(rc).or_default().insert(key);
                self.send_to_region(rc, RegionInput::RequestFarSnapshot(client));
            } else {
                let columns = (self.generator)(rc);
                let snap = FarSnapshot::from_columns(columns);
                self.far_cache.insert(rc, snap.clone());
                self.lod_in_flight.insert(key);
                self.lod_builder
                    .submit(LodBuildJob::Region { key, chunks: snap.chunks });
            }
        } else {
            self.start_tile_build(key, client);
        }
    }

    /// Tile keys (64/256): coarse base + one overlay per region with
    /// authoritative state. Regions running-but-unseeded are requested and
    /// the build deferred until every such reply has landed; pristine and
    /// parked-with-cache regions need nothing. Pristine regions are NOT
    /// generated here — coarse sampling covers them (the point of tiles).
    ///
    /// Caveat: `far_cache` reflects the LAST pushed/parked terrain of a
    /// running region — mid-cadence edits appear on the next
    /// `FarSnapshotChanged` (which Task 9 turns into invalidation +
    /// rebuild), matching the spec's freshness decision.
    fn start_tile_build(&mut self, key: LodKey, client: ClientId) {
        let mut waiting = BTreeSet::new();
        for rc in key.regions() {
            let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
            if running && !self.far_cache.contains_key(&rc) {
                waiting.insert(rc);
                self.lod_pending_terrain.entry(rc).or_default().insert(key);
                self.send_to_region(rc, RegionInput::RequestFarSnapshot(client));
            }
        }
        if !waiting.is_empty() {
            self.lod_tile_waiting.insert(key, waiting);
            return;
        }
        let overlays: Vec<(RegionCoords, Vec<(ChunkCoords, Chunk)>)> = key
            .regions()
            .into_iter()
            .filter_map(|rc| self.far_cache.get(&rc).map(|snap| (rc, snap.chunks.clone())))
            .collect();
        self.lod_in_flight.insert(key);
        self.lod_builder.submit(LodBuildJob::Tile { key, overlays });
    }

    /// A region's terrain just landed in far_cache: submit any LOD builds
    /// that were waiting on it, region- or tile-keyed.
    fn lod_terrain_ready(&mut self, rc: RegionCoords, client: ClientId) {
        let Some(keys) = self.lod_pending_terrain.remove(&rc) else { return };
        for key in keys {
            if let Some(waiting) = self.lod_tile_waiting.get_mut(&key) {
                waiting.remove(&rc);
                if !waiting.is_empty() {
                    continue;
                }
                self.lod_tile_waiting.remove(&key);
            }
            if !self.lod_in_flight.contains(&key) {
                self.start_lod_build(key, client);
            }
        }
    }

    pub fn handle_lod_result(&mut self, key: LodKey, payload: LodPayload) {
        self.lod_in_flight.remove(&key);
        self.lod_access_counter += 1;
        self.lod_cache.insert(key, (payload.clone(), self.lod_access_counter));
        self.evict_lod_cache();
        if let Some(subs) = self.lod_subscribers.get(&key) {
            for client in subs {
                let _ = self
                    .out
                    .send((Some(*client), ServerPacket::LodData(key, payload.clone())));
            }
        }
    }

    /// Drain the builder results channel (Inline path; the native server
    /// selects on the receiver instead and calls handle_lod_result directly).
    pub fn pump_lod_results(&mut self) {
        while let Ok((key, payload)) = self.lod_results.try_recv() {
            self.handle_lod_result(key, payload);
        }
    }

    /// A region's terrain changed (cadence push) or it parked: every LOD key
    /// depending on it is stale. Drop them from the cache; rebuild the ones
    /// with live subscribers (the fresh far_cache entry is the new source).
    fn invalidate_region_lod(&mut self, rc: RegionCoords) {
        for factor in crate::lod::LOD_FACTORS {
            let key = LodKey::for_region(factor, rc);
            self.lod_cache.remove(&key);
            let has_subs = self.lod_subscribers.get(&key).is_some_and(|s| !s.is_empty());
            if has_subs && !self.lod_in_flight.contains(&key) {
                // Any subscriber works as the terrain-request correlator.
                let client = *self.lod_subscribers[&key].iter().next().unwrap();
                self.start_lod_build(key, client);
            }
        }
    }

    /// Test hook: current cache size (`lod_cache_evicts_unsubscribed_lru_but_pins_subscribed`).
    pub fn lod_cache_len(&self) -> usize {
        self.lod_cache.len()
    }

    /// Test hook: shrink/grow the LRU cap below `LOD_CACHE_MAX_ENTRIES` so
    /// eviction is exercisable without filling thousands of real entries.
    pub fn set_lod_cache_max(&mut self, n: usize) {
        self.lod_cache_max = n;
        self.evict_lod_cache();
    }

    fn evict_lod_cache(&mut self) {
        while self.lod_cache.len() > self.lod_cache_max {
            let victim = self
                .lod_cache
                .iter()
                .filter(|(k, _)| !self.lod_subscribers.contains_key(k))
                .min_by_key(|(_, (_, access))| *access)
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.lod_cache.remove(&k);
                }
                None => break, // everything pinned by subscribers
            }
        }
    }
}
