use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, GameEvent, GameEventKind,
    InlineLodBuilder, InlineSpawner, LodBuilder, RegionCoords, RegionOutput, ServerEvent,
    ServerPacket, SubChunk, WorldManager, SPAWN_REGION, UNLOAD_GRACE_MS,
};

struct Harness {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

fn harness() -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let generator = Box::new(|_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        let below = Block::new(BlockId::AIR);
        let above = Block::new(BlockId::AIR);
        let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
        vec![(ColumnCoords::new(0, 0), Column::new(0, below, above, vec![sub]))]
    });
    let (lod_result_send, lod_result_recv) = unbounded();
    let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        out: lod_result_send,
    });
    Harness {
        manager: WorldManager::new(
            InlineSpawner::default(),
            generator,
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        ),
        region_out,
        packets,
    }
}

impl Harness {
    /// Route → pump runners → route outputs → pump LOD builds, twice
    /// (outputs can trigger respawns/resubscribes that need one more pump).
    /// Mirrors LocalServer.
    fn settle(&mut self, now_ms: u64) {
        for _ in 0..2 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now_ms);
            }
            self.manager.pump_lod_results();
        }
    }
    fn event(&mut self, ev: ServerEvent, now_ms: u64) -> bool {
        let alive = self.manager.handle_server_event(ev, now_ms);
        self.settle(now_ms);
        alive
    }
    fn drain_packets(&mut self) -> Vec<(Option<ClientId>, ServerPacket)> {
        self.packets.try_iter().collect()
    }
}

#[test]
fn connect_spawns_home_and_creates_player() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    assert_eq!(h.manager.running_regions(), vec![SPAWN_REGION]);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestPlayerRegion, 0), 0);
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == SPAWN_REGION)
    }));
}

#[test]
fn subscribe_spawns_region_and_delivers_snapshot() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.drain_packets();
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    assert!(h.manager.running_regions().contains(&rc));
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::Region(id, _) if *id == rc)
    }));
}

#[test]
fn client_sent_session_events_are_dropped() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.drain_packets();

    // Sessions are created and moved by the manager only: a client-sent
    // CreateClient / CreateClientAt / ClientLeft never reaches a region,
    // even a subscribed one.
    for kind in [
        GameEventKind::CreateClient(0),
        GameEventKind::CreateClientAt(0, [1, 1]),
        GameEventKind::ClientLeft(0),
    ] {
        let ev = GameEvent::new(kind, 0, rc);
        h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0);
        h.settle(0);
        assert!(!h.drain_packets().iter().any(|(_, p)| matches!(p, ServerPacket::GameEvent(_))));
    }

    // Unsubscribed region: dropped, and not woken.
    let far = RegionCoords::new(9, 9);
    let ev = GameEvent::new(GameEventKind::CreateClient(0), 0, far);
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0);
    assert!(!h.manager.running_regions().contains(&far));
}

#[test]
fn release_then_grace_parks_region_and_resubscribe_restores_it() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0), 1000);

    // Before the grace period: still running.
    h.manager.maintain(1000 + UNLOAD_GRACE_MS - 1);
    h.settle(1000 + UNLOAD_GRACE_MS - 1);
    assert!(h.manager.running_regions().contains(&rc));

    // After: shut down and parked.
    h.manager.maintain(1000 + UNLOAD_GRACE_MS);
    h.settle(1000 + UNLOAD_GRACE_MS);
    assert!(!h.manager.running_regions().contains(&rc));
    assert!(h.manager.parked_regions().contains(&rc));

    // Resubscribe restores from the parking lot.
    h.drain_packets();
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 20_000);
    assert!(h.manager.running_regions().contains(&rc));
    assert!(!h.manager.parked_regions().contains(&rc));
    assert!(h.drain_packets().iter().any(|(_, p)| matches!(p, ServerPacket::Region(id, _) if *id == rc)));
}

#[test]
fn home_region_survives_zero_subscribers() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    // Client subscribes to home then wanders off and releases it.
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(SPAWN_REGION), 0), 100);
    h.manager.maintain(100 + UNLOAD_GRACE_MS * 2);
    h.settle(100 + UNLOAD_GRACE_MS * 2);
    // Still running: it hosts a connected client's player entity.
    assert!(h.manager.running_regions().contains(&SPAWN_REGION));

    // Once the client disconnects, the home region may park.
    h.event(ServerEvent::ClientDisconnected(0), 200 + UNLOAD_GRACE_MS * 2);
    h.manager.maintain(200 + UNLOAD_GRACE_MS * 4);
    h.settle(200 + UNLOAD_GRACE_MS * 4);
    assert!(!h.manager.running_regions().contains(&SPAWN_REGION));
    assert!(h.manager.parked_regions().contains(&SPAWN_REGION));
}

#[test]
fn reconnect_does_not_create_second_player() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    h.event(ServerEvent::ClientDisconnected(0), 100);
    h.event(ServerEvent::connected(0), 200);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 200);
    let packets = h.drain_packets();
    let snapshot = packets.iter().find_map(|(_, p)| match p {
        ServerPacket::Region(_, rollback) => Some(rollback),
        _ => None,
    }).expect("snapshot after resubscribe");
    assert_eq!(snapshot.player_entites.len(), 1, "reconnect must not duplicate the player");
}

#[test]
fn dead_region_respawns_and_resnapshots_subscribers() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let rc = RegionCoords::new(2, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.drain_packets();

    // Kill the runner behind the manager's back (thread-death stand-in:
    // the input channel's receiver is dropped, so the next send fails).
    h.manager.spawner_mut().kill(rc);

    // Next send to the region detects the death, respawns, resnapshots. (A
    // repeated subscribe, whose snapshot request is routed to `rc` itself;
    // PlayerInput is home-routed and client-sent session events are dropped.)
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 500);
    assert!(h.manager.running_regions().contains(&rc));
    assert!(h.drain_packets().iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::Region(id, _) if *id == rc)
    }));
}

#[test]
fn disconnect_during_stopping_window_leaves_no_phantom_subscriber() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let rc = RegionCoords::new(3, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0), 1000);
    h.drain_packets();

    // Past grace: Shutdown is sent but NOT yet processed (no settle) — the
    // region sits in the stopping window.
    h.manager.maintain(1000 + UNLOAD_GRACE_MS);

    // Re-subscribe lands in resubscribe_pending, then the client
    // disconnects, all before Stopped arrives. (Drive the manager directly:
    // the harness's event() auto-settles, which would close the window.)
    h.manager.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        2000 + UNLOAD_GRACE_MS,
    );
    h.manager.handle_server_event(ServerEvent::ClientDisconnected(0), 2000 + UNLOAD_GRACE_MS);
    h.drain_packets();

    // Now let the Shutdown → Stopped handshake (and any pending replays)
    // complete.
    h.settle(2000 + UNLOAD_GRACE_MS);

    // The dead client must not pin the region as a phantom subscriber:
    // after another grace period it must be parked (or never respawned).
    h.manager.maintain(3000 + UNLOAD_GRACE_MS * 2);
    h.settle(3000 + UNLOAD_GRACE_MS * 2);
    assert!(
        !h.manager.running_regions().contains(&rc),
        "phantom subscriber kept the region running"
    );
    assert!(h.manager.parked_regions().contains(&rc));
    // No packets to the disconnected client after the disconnect.
    assert!(h.drain_packets().is_empty(), "packets sent to a dead client");
}

#[test]
fn quit_event_stops_the_manager() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let ev = GameEvent::new(GameEventKind::Quit, 0, SPAWN_REGION);
    assert!(!h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0));
}

/// Connect client 0 and drain the join packets, returning the harness ready
/// for LOD-serving assertions.
fn connected_harness() -> Harness {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.drain_packets();
    h
}

/// Does the packet list carry a `LodData` for `key` addressed to client 0?
fn has_lod_data(packets: &[(Option<ClientId>, ServerPacket)], key: game::lod::LodKey) -> bool {
    packets.iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::LodData(k, _) if *k == key)
    })
}

#[test]
fn lod_request_for_never_run_region_serves_without_waking() {
    let mut h = connected_harness();
    let rc = RegionCoords::new(5, 5);
    let key = game::lod::LodKey::for_region(2, rc);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 0), 0);
    // Never spun up a sim thread for the region: generate-on-miss.
    assert!(!h.manager.running_regions().contains(&rc));
    assert!(
        has_lod_data(&h.drain_packets(), key),
        "LOD request must deliver a LodData packet"
    );
}

#[test]
fn lod_request_for_running_region_pulls_live_terrain() {
    let mut h = connected_harness();
    let rc = RegionCoords::new(1, 0);
    // Make it running via a normal (near) subscribe first.
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        0,
    );
    h.drain_packets();
    assert!(h.manager.running_regions().contains(&rc));
    let key = game::lod::LodKey::for_region(2, rc);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 0), 0);
    assert!(has_lod_data(&h.drain_packets(), key));
}

#[test]
fn lod_subscribers_do_not_keep_a_region_alive() {
    let mut h = connected_harness();
    let rc = RegionCoords::new(9, 9);
    let key = game::lod::LodKey::for_region(2, rc);
    // Wake it (arrival-style) by near-subscribing, then release near but keep LOD.
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        0,
    );
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 0), 0);
    h.drain_packets();
    h.event(
        ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0),
        0,
    );
    // Grace timer starts; after the window, maintain must park it despite
    // the LOD subscription.
    h.manager.maintain(UNLOAD_GRACE_MS + 1);
    h.settle(UNLOAD_GRACE_MS + 1);
    assert!(
        !h.manager.running_regions().contains(&rc),
        "a region with only LOD subscribers must still park"
    );
    assert!(h.manager.parked_regions().contains(&rc));
}

#[test]
fn parked_lod_request_still_gets_served_from_cache() {
    // After the region parks (previous scenario), a fresh LOD request is
    // served from terrain_cache without waking it.
    let mut h = connected_harness();
    let rc = RegionCoords::new(2, 3);
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        0,
    );
    h.event(
        ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0),
        0,
    );
    h.manager.maintain(UNLOAD_GRACE_MS + 1);
    h.settle(UNLOAD_GRACE_MS + 1);
    assert!(h.manager.parked_regions().contains(&rc));
    h.drain_packets();
    let key = game::lod::LodKey::for_region(2, rc);
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 0),
        UNLOAD_GRACE_MS + 2,
    );
    assert!(!h.manager.running_regions().contains(&rc), "LOD serve must not wake a parked region");
    assert!(has_lod_data(&h.drain_packets(), key));
}

/// Stand-in for the native server's thread spawner: it generates a region's
/// columns on the region's own thread (`generates_columns`), and records the
/// seeds it is handed instead of running anything.
struct DeferringSpawner {
    seeds: std::sync::Arc<std::sync::Mutex<Vec<(RegionCoords, &'static str)>>>,
}

impl game::RegionSpawner for DeferringSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: game::RegionSeed,
        _out: crossbeam::channel::Sender<(RegionCoords, RegionOutput)>,
    ) -> crossbeam::channel::Sender<game::RegionInput> {
        let kind = match seed {
            game::RegionSeed::Fresh(_) => "fresh",
            game::RegionSeed::Parked(..) => "parked",
            game::RegionSeed::Deferred(None) => "deferred-fresh",
            game::RegionSeed::Deferred(Some(_)) => "deferred-parked",
        };
        self.seeds.lock().unwrap().push((id, kind));
        let (send, recv) = unbounded();
        // Keep the channel alive so the manager does not see a dead region.
        std::mem::forget(recv);
        send
    }

    fn generates_columns(&self) -> bool {
        true
    }
}

/// Worldgen for a region start must not run on the manager thread (it measured
/// 0.1-0.9 s per region, sequentially, 6-7 s for a 3x3 join — all of it
/// routing latency): a spawner that generates columns itself gets a deferred
/// seed and the manager's generator is never called.
#[test]
fn region_start_defers_worldgen_to_a_spawner_that_generates_columns() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    let (out_send, _packets) = unbounded();
    let (region_out_send, _region_out) = unbounded();
    let generated = Arc::new(AtomicUsize::new(0));
    let counter = generated.clone();
    let generator = Box::new(move |_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        counter.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    });
    let (lod_result_send, lod_result_recv) = unbounded();
    let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        out: lod_result_send,
    });
    let seeds = Arc::new(Mutex::new(Vec::new()));
    let mut manager = WorldManager::new(
        DeferringSpawner { seeds: seeds.clone() },
        generator,
        out_send,
        region_out_send,
        lod_builder,
        lod_result_recv,
    );

    manager.handle_server_event(ServerEvent::connected(1), 0);
    manager.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(RegionCoords::new(5, 5)), 1),
        0,
    );

    assert_eq!(generated.load(Ordering::SeqCst), 0, "the manager must not run worldgen");
    let seeds = seeds.lock().unwrap();
    assert_eq!(
        *seeds,
        vec![(SPAWN_REGION, "deferred-fresh"), (RegionCoords::new(5, 5), "deferred-fresh")]
    );
}

/// A deferred seed generates exactly when it has to: a fresh region once, a
/// parked region never (the old eager path regenerated the columns on every
/// resubscribe just to have a fallback for a corrupt blob).
#[test]
fn deferred_seed_generates_only_for_fresh_regions() {
    use std::cell::Cell;
    let id = RegionCoords::new(2, -3);
    let calls = Cell::new(0);
    let generate = |_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        calls.set(calls.get() + 1);
        let air = Block::new(BlockId::AIR);
        let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
        vec![(ColumnCoords::new(0, 0), Column::new(0, air, air, vec![sub]))]
    };

    let fresh = game::RegionSeed::Deferred(None).into_region_with(id, &generate);
    assert_eq!(calls.get(), 1);
    assert_eq!(fresh.terrain_snapshot().chunks.len(), 1);

    let parked = game::SerializedRegion::from_rollback(fresh.data());
    let restored = game::RegionSeed::Deferred(Some(parked)).into_region_with(id, &generate);
    assert_eq!(calls.get(), 1, "a valid parked region restores without worldgen");
    assert_eq!(restored.terrain_fingerprint(), fresh.terrain_fingerprint());
}

#[test]
fn order_event_routes_to_the_home_region() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.drain_packets();
    // Air above the floor takes a Build designation.
    let order = game::Order::Designate {
        op: game::OpId { client: 0, seq: 1 },
        kind: game::DesignationKind::Build(game::edit::PLACE_BLOCK),
        priority: game::DEFAULT_PRIORITY,
        area: game::TileBox::new([0, 5, 0], [1, 5, 1]),
    };
    // Stamped with a region it is not in: the manager routes by home.
    let ev = GameEvent::new(GameEventKind::Order(0, order), 0, RegionCoords::new(7, 7));
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0);
    h.manager.spawner_mut().tick_all();
    h.settle(0);
    let mut designations = 0;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| designations = r.data().designations().len());
    assert_eq!(designations, 4);
}

#[test]
fn leave_packet_removes_the_session() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let before = h.manager.depths().sessions;
    assert_eq!(before, 1);
    h.event(ServerEvent::ClientPacket(ClientPacket::Leave, 0), 10);
    assert_eq!(h.manager.depths().sessions, before - 1);
    // The transport closing afterwards is a no-op.
    h.event(ServerEvent::ClientDisconnected(0), 20);
    assert_eq!(h.manager.depths().sessions, before - 1);
}

// ---- Players park and restore across disconnects and restarts (Task 2) ----

use game::{body_on_tile, feet_of, feet_tile, standable, PlayMode, PlayerId, PlayerRecord, Rollback, TilePos};
use std::collections::BTreeMap;

/// A harness whose regions are a whole floor (block 0 solid, block 1 air),
/// built over `spawner`.
fn floored_harness_with(spawner: InlineSpawner) -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let (lod_result_send, lod_result_recv) = unbounded();
    let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        out: lod_result_send,
    });
    Harness {
        manager: WorldManager::new(
            spawner,
            Box::new(crate::history_handover::floored_region),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        ),
        region_out,
        packets,
    }
}

fn floored_harness() -> Harness {
    floored_harness_with(InlineSpawner::default())
}

/// A session of `player` on connection `client`, with the record the store
/// returned for it.
fn session(client: ClientId, player: u64, record: Option<PlayerRecord>) -> ServerEvent {
    ServerEvent::ClientConnected { client, player: PlayerId(player), record }
}

fn client_event(h: &mut Harness, client: ClientId, kind: GameEventKind) {
    let ev = GameEvent::new(kind, 0, SPAWN_REGION);
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), client), 0);
}

/// The marker stacks: a main-inventory slot and the cursor.
const MARK_SLOT: usize = 20;
const MARK_STACK: game::ItemStack = game::ItemStack { item: game::ItemId(5), count: 33 };
const MARK_CURSOR: game::ItemStack = game::ItemStack { item: game::ItemId(6), count: 2 };

/// Give `client` a stack in a main slot and one on the cursor, and switch it
/// to Adventure: state no fresh player has (the kit is empty, and a parked
/// record resets only input / first person / the open flag).
fn rearrange_inventory(h: &mut Harness, client: ClientId) {
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        region.with_data(|d| {
            let c = d.clients.get_mut(&client).expect("client in the spawn region");
            c.inventory.slots[MARK_SLOT] = Some(MARK_STACK);
            c.cursor = Some(MARK_CURSOR);
        });
    });
    client_event(h, client, GameEventKind::Order(client, game::Order::SetMode { mode: game::PlayMode::Adventure, at: None }));
}

fn rearranged(record: &game::Client) -> bool {
    record.mode == game::PlayMode::Adventure
        && record.inventory.slots[MARK_SLOT] == Some(MARK_STACK)
        && record.cursor == Some(MARK_CURSOR)
}

/// Put `client`'s body on tile `at` of `rc`; returns its translation.
fn place_body(h: &mut Harness, rc: RegionCoords, client: ClientId, at: TilePos) -> game::rapier3d::math::Vector {
    let t = body_on_tile(at);
    h.manager.spawner_mut().with_region(rc, |region| {
        let key = *region.data().player_entites.get(&client).expect("a body");
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(t)));
    });
    t
}

/// The translation of `client`'s body in `rc`, if `rc` runs and holds it.
fn body_of(h: &mut Harness, rc: RegionCoords, client: ClientId) -> Option<game::rapier3d::math::Vector> {
    let mut out = None;
    h.manager.spawner_mut().with_region(rc, |region| {
        let d = region.data();
        let handle = d.player_entites.get(&client).and_then(|&k| *d.ecs.rigidbody.try_get(k));
        out = handle.and_then(|h| d.physics.bodies.get(h)).map(|b| b.position().translation);
    });
    out
}

fn client_in(h: &mut Harness, rc: RegionCoords, client: ClientId) -> Option<game::Client> {
    let mut out = None;
    h.manager.spawner_mut().with_region(rc, |region| out = region.data().clients.get(&client).cloned());
    out
}

fn bodies_in(h: &mut Harness, rc: RegionCoords) -> usize {
    let mut n = 0;
    h.manager.spawner_mut().with_region(rc, |region| n = region.data().player_entites.len());
    n
}

#[test]
fn disconnect_parks_player_no_orphan_body() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    h.event(ServerEvent::ClientDisconnected(0), 10);
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 0, "no orphan body");
    assert!(client_in(&mut h, SPAWN_REGION, 0).is_none(), "no orphan client entry");
    let saves = h.manager.take_player_saves();
    assert_eq!(saves.len(), 1, "{saves:?}");
    let (player, record) = &saves[0];
    assert_eq!(*player, PlayerId(0));
    assert_eq!(record.region, SPAWN_REGION);
    assert!(rearranged(&record.client), "the record keeps the inventory: {:?}", record.client.inventory);
    assert!(h.manager.take_player_saves().is_empty(), "taken once");
}

#[test]
fn reconnect_same_token_restores_body_and_inventory() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    client_event(&mut h, 0, GameEventKind::Order(0, game::Order::SetMode { mode: PlayMode::Adventure, at: None }));
    let at = place_body(&mut h, SPAWN_REGION, 0, [5, 1, 7]);
    h.event(ServerEvent::ClientDisconnected(0), 10);
    h.drain_packets();
    // A new connection (a new client id) of the same player.
    h.event(session(1, 0, None), 20);
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 1);
    assert_eq!(body_of(&mut h, SPAWN_REGION, 1), Some(at), "the body is back where it stood");
    let client = client_in(&mut h, SPAWN_REGION, 1).expect("the client is restored");
    assert!(rearranged(&client), "{:?}", client.inventory);
    assert_eq!(client.mode, PlayMode::Adventure);
    assert!(h.drain_packets().iter().any(|(to, p)| {
        *to == Some(1) && matches!(p, ServerPacket::PlayerRegion(Some(rc), 1) if *rc == SPAWN_REGION)
    }));
}

#[test]
fn second_session_takes_over() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    let at = place_body(&mut h, SPAWN_REGION, 0, [4, 1, 9]);
    h.drain_packets();
    // The same player connects again while its first session is still open
    // (a crashed client whose connection has not timed out yet).
    h.event(session(1, 0, None), 10);
    let packets = h.drain_packets();
    assert!(
        packets.iter().any(|(to, p)| *to == Some(0) && matches!(p, ServerPacket::Refused(why) if why == "logged in elsewhere")),
        "the old session is told why it ends: {packets:?}"
    );
    assert!(!packets.iter().any(|(to, p)| *to == Some(1) && matches!(p, ServerPacket::Refused(_))), "{packets:?}");
    assert_eq!(h.manager.depths().sessions, 1);
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 1, "exactly one body");
    assert!(body_of(&mut h, SPAWN_REGION, 0).is_none(), "the old session has no body");
    assert_eq!(body_of(&mut h, SPAWN_REGION, 1), Some(at), "the new session has the player's body");
    assert!(rearranged(&client_in(&mut h, SPAWN_REGION, 1).unwrap()));
    // The old connection closing afterwards changes nothing.
    h.event(ServerEvent::ClientDisconnected(0), 20);
    assert_eq!(h.manager.depths().sessions, 1);
    assert_eq!(body_of(&mut h, SPAWN_REGION, 1), Some(at));
    assert_eq!(h.manager.take_player_saves().len(), 1, "one park: the takeover's");
}

#[test]
fn park_in_dead_region_does_not_lock_out() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    // What the store holds for the player: the last autosave's record.
    h.manager.request_save();
    h.settle(0);
    let (_, stored) = h.manager.take_player_saves().pop().expect("autosaved");
    // The park goes out, then the region thread dies before answering it.
    h.manager.handle_server_event(ServerEvent::ClientDisconnected(0), 10);
    h.manager.spawner_mut().kill(SPAWN_REGION);
    // The player reconnects while the park is (as far as the manager knows)
    // still in flight: the restore waits.
    h.event(session(1, 0, Some(stored)), 20);
    // The death is noticed (here: the grace-period Shutdown finds the region
    // gone): the park is settled from the store's record and the player is in.
    let late = 20 + UNLOAD_GRACE_MS + 1;
    h.manager.maintain(late);
    h.settle(late);
    h.manager.maintain(late);
    h.settle(late);
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 1, "the player is back, once");
    assert!(rearranged(&client_in(&mut h, SPAWN_REGION, 1).expect("restored")));
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestPlayerRegion, 1), late);
    assert!(h.drain_packets().iter().any(|(to, p)| {
        *to == Some(1) && matches!(p, ServerPacket::PlayerRegion(Some(rc), 1) if *rc == SPAWN_REGION)
    }));
}

#[test]
fn restore_into_built_over_cell_lands_standable() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    place_body(&mut h, SPAWN_REGION, 0, [5, 1, 7]);
    h.event(ServerEvent::ClientDisconnected(0), 10);
    // Someone builds where the player stood: a two-block pillar.
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        region.with_data(|d| {
            assert!(d.set_block_safe([5, 1, 7], game::Block::new(BlockId(1))));
            assert!(d.set_block_safe([5, 2, 7], game::Block::new(BlockId(1))));
        });
    });
    // And the home region parks before the player is back.
    let late = 10 + UNLOAD_GRACE_MS + 1;
    h.manager.maintain(late);
    h.settle(late);
    assert!(h.manager.parked_regions().contains(&SPAWN_REGION));
    h.event(session(1, 0, None), late);
    let t = body_of(&mut h, SPAWN_REGION, 1).expect("restored");
    let tile = feet_tile(feet_of(t));
    let mut ok = false;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| ok = standable(region.data(), tile));
    assert!(ok, "the body stands on a standable tile, not inside the pillar: {tile:?}");
    assert_eq!(tile, [5, 3, 7], "on top of the pillar");
}

#[test]
fn autosave_extracts_online_player_records() {
    let mut h = floored_harness();
    h.event(session(0, 3, None), 0);
    rearrange_inventory(&mut h, 0);
    h.manager.request_save();
    h.settle(0);
    assert_eq!(h.manager.take_saves().len(), 1);
    let saves = h.manager.take_player_saves();
    assert_eq!(saves.len(), 1, "{saves:?}");
    assert_eq!(saves[0].0, PlayerId(3));
    assert_eq!(saves[0].1.region, SPAWN_REGION);
    assert!(rearranged(&saves[0].1.client));
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 1, "an autosave parks nobody");
}

#[test]
fn fast_reconnect_waits_for_park() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    let at = place_body(&mut h, SPAWN_REGION, 0, [9, 1, 4]);
    // Disconnect and reconnect before the region has parked the body (no
    // settle in between), with a stale record from the store.
    h.manager.handle_server_event(ServerEvent::ClientDisconnected(0), 10);
    let stale = PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(body_on_tile([1, 1, 1])),
        region: SPAWN_REGION,
        fortress: None,
        adventure: None,
    };
    h.manager.handle_server_event(session(1, 0, Some(stale)), 11);
    // A move request in the window waits for the restore too.
    h.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestPlayerRegion, 1), 11);
    h.settle(12);
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 1, "exactly one body");
    assert_eq!(body_of(&mut h, SPAWN_REGION, 1), Some(at), "restored from the park, not the stale record");
    assert!(rearranged(&client_in(&mut h, SPAWN_REGION, 1).unwrap()));
    let packets = h.drain_packets();
    assert!(
        !packets.iter().any(|(to, p)| *to == Some(1) && matches!(p, ServerPacket::PlayerRegion(None, _))),
        "never told it has no home: {packets:?}"
    );
}

/// Every region state a manager saved, as a restarted server's store would
/// hand them back.
fn saved_states(saves: Vec<(RegionCoords, game::RegionSave)>) -> BTreeMap<RegionCoords, Rollback> {
    saves
        .into_iter()
        .map(|(rc, save)| (rc, save.into_serialized().to_rollback().expect("decodes")))
        .collect()
}

#[test]
fn restart_restores_player() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    let at = place_body(&mut h, SPAWN_REGION, 0, [6, 1, 2]);
    h.manager.spawner_mut().tick_all();
    h.settle(0);
    let at = body_of(&mut h, SPAWN_REGION, 0).unwrap_or(at);
    // The server's orderly exit: park every player, then every region.
    h.manager.park_all_players(100);
    h.manager.shutdown_all();
    h.settle(100);
    assert!(h.manager.running_regions().is_empty());
    h.manager.request_save();
    let regions = saved_states(h.manager.take_saves());
    let mut store: BTreeMap<PlayerId, PlayerRecord> = BTreeMap::new();
    store.extend(h.manager.take_player_saves());
    drop(h);

    // A new process: client ids start at 0 again; the edge resolves the token.
    let mut h = floored_harness_with(InlineSpawner::with_saved(regions));
    h.event(session(0, 0, store.get(&PlayerId(0)).cloned()), 0);
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 1, "the saved body was stripped, the player restored once");
    assert_eq!(body_of(&mut h, SPAWN_REGION, 0), Some(at));
    assert!(rearranged(&client_in(&mut h, SPAWN_REGION, 0).unwrap()));
}

#[test]
fn disconnect_mid_crossing_parks_the_body_where_it_went() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    // Past the +x seam: the next tick hands the body to (1, 0).
    let edge = game::REGION_BLOCKS as i32;
    place_body(&mut h, SPAWN_REGION, 0, [edge + 2, 1, 7]);
    h.manager.spawner_mut().tick_all();
    // The disconnect reaches the manager before the departure does: the park
    // goes to the old home, which no longer has the body.
    h.manager.handle_server_event(ServerEvent::ClientDisconnected(0), 10);
    h.settle(10);
    h.settle(10);
    let east = RegionCoords::new(1, 0);
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 0);
    assert_eq!(bodies_in(&mut h, east), 0, "no orphan body where it crossed to");
    let saves = h.manager.take_player_saves();
    assert_eq!(saves.len(), 1, "{saves:?}");
    assert_eq!(saves[0].1.region, east);
    assert!(rearranged(&saves[0].1.client));
    // And it comes back there.
    h.event(session(1, 0, None), 20);
    assert_eq!(bodies_in(&mut h, east), 1);
    assert!(rearranged(&client_in(&mut h, east, 1).unwrap()));
}
