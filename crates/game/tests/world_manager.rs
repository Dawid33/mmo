use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, GameEvent, GameEventKind,
    InlineLodBuilder, InlineSpawner, LodBuilder, RegionCoords, RegionOutput, ServerEvent,
    ServerPacket, SubChunk, WorldManager, SPAWN_REGION, UNLOAD_GRACE_MS,
};

struct Harness {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    /// The runners' output channel, for tests that inject an output.
    region_out_send: crossbeam::channel::Sender<(RegionCoords, RegionOutput)>,
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
        water: Box::new(game::lod::LodWater::dry),
        out: lod_result_send,
    });
    Harness {
        manager: WorldManager::new(
            InlineSpawner::default(),
            generator,
            out_send,
            region_out_send.clone(),
            lod_builder,
            lod_result_recv,
        ),
        region_out,
        region_out_send,
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
        GameEventKind::CreateClientAt(0, [1, 1], None),
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

/// A region generated afresh after its thread died must not hand out the
/// `EntityId`s of its earlier incarnation's entities (which may live on in
/// other regions, the directory or a parked record): its serials start past
/// `world tick << ENTITY_SERIAL_TICK_SHIFT`.
#[test]
fn respawned_region_allocates_fresh_entity_ids() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let rc = RegionCoords::new(2, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    for _ in 0..5 {
        h.manager.spawner_mut().tick_all();
        h.settle(0);
    }
    let serial = |h: &mut Harness| {
        let mut out = None;
        h.manager.spawner_mut().with_region(rc, |r| out = Some(*r.data().next_entity_serial));
        out.expect("running")
    };
    let first = serial(&mut h);
    h.manager.spawner_mut().kill(rc);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 500);
    let world_tick = game::RegionSpawner::world_tick(h.manager.spawner_mut());
    assert!(world_tick > 0);
    let second = serial(&mut h);
    assert!(
        second >= world_tick << game::ENTITY_SERIAL_TICK_SHIFT && second > first,
        "respawned serials {second} overlap the dead incarnation's (< {first}, world tick {world_tick})"
    );
}

/// Outputs a dead runner left in the channel (it died after sending them,
/// and was respawned at once) are not the new incarnation's: dropped, not
/// fanned out to clients.
#[test]
fn dead_incarnations_late_outputs_are_dropped() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    let rc = RegionCoords::new(2, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.drain_packets();
    h.manager.spawner_mut().kill(rc);
    // What the dead runner sent just before it died, still unread.
    let stale_id = 987_654;
    let stale = GameEvent::new(GameEventKind::Tick, stale_id, rc);
    h.region_out_send.send((rc, RegionOutput::EventProcessed(stale))).unwrap();
    // Death detected and the region respawned before the channel is read.
    assert!(h.manager.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        500
    ));
    h.settle(500);
    assert!(h.manager.running_regions().contains(&rc));
    let packets = h.drain_packets();
    assert!(
        !packets.iter().any(|(_, p)| matches!(p, ServerPacket::GameEvent(e) if e.id == stale_id)),
        "a dead runner's late event reached the client"
    );
    assert!(
        packets.iter().any(|(_, p)| matches!(p, ServerPacket::Region(id, _) if *id == rc)),
        "the new incarnation's snapshot is delivered"
    );
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
        *target == Some(0) && matches!(p, ServerPacket::LodData(k, ..) if *k == key)
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
    fn world_tick(&self) -> game::Tick {
        0
    }
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
        water: Box::new(game::lod::LodWater::dry),
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

use game::{body_on_tile, feet_of, feet_tile, standable, PlayMode, PlayerId, PlayerRecord, TilePos};

/// A harness whose regions are a whole floor (block 0 solid, block 1 air),
/// built over `spawner`.
fn floored_harness_with(spawner: InlineSpawner) -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let (lod_result_send, lod_result_recv) = unbounded();
    let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        water: Box::new(game::lod::LodWater::dry),
        out: lod_result_send,
    });
    Harness {
        manager: WorldManager::new(
            spawner,
            Box::new(crate::history_handover::floored_region),
            out_send,
            region_out_send.clone(),
            lod_builder,
            lod_result_recv,
        ),
        region_out,
        region_out_send,
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
/// to Adventure: state no fresh player has (a parked record resets input /
/// first person / the open flag, and puts the cursor stack into the
/// inventory).
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

/// `rearrange_inventory`'s state as a park keeps it: the cursor stack is in
/// the inventory now.
fn rearranged(record: &game::Client) -> bool {
    record.mode == game::PlayMode::Adventure
        && record.inventory.slots[MARK_SLOT] == Some(MARK_STACK)
        && record.cursor.is_none()
        && record.inventory.slots.iter().any(|s| *s == Some(MARK_CURSOR))
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
    let record = h.manager.parked_record(PlayerId(0)).expect("the park is kept").clone();
    assert_eq!(record.region, SPAWN_REGION);
    assert!(rearranged(&record.client), "the record keeps the inventory: {:?}", record.client.inventory);
    // Records reach the store only in cuts (Decision 12): once, the parked one.
    let cut = cut_now(&mut h);
    let mine: Vec<_> = cut.players.iter().filter(|(p, _)| *p == PlayerId(0)).collect();
    assert_eq!(mine.len(), 1, "{:?}", cut.players);
    assert_eq!(mine[0].1, record);
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
    assert!(h.manager.parked_record(PlayerId(0)).is_none(), "the takeover's park went into the new session");
}

#[test]
fn park_in_dead_region_does_not_lock_out() {
    let mut h = floored_harness();
    h.event(session(0, 0, None), 0);
    rearrange_inventory(&mut h, 0);
    // What the store holds for the player: the last save cut's record.
    let stored = cut_now(&mut h).players.pop().expect("the player is in the cut").1;
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

/// A save cut at the next tick (lockstep), taken.
fn cut_now(h: &mut Harness) -> game::WorldCut {
    use game::RegionSpawner;
    let world = h.manager.spawner_mut().world_tick();
    h.manager.begin_cut((world + 2).saturating_sub(game::SAVE_CUT_LEAD_TICKS)).expect("no cut pending");
    for _ in 0..game::SAVE_CUT_LEAD_TICKS + 4 {
        if let Some(cut) = h.manager.take_cut() {
            return cut;
        }
        h.manager.spawner_mut().tick_all();
        h.settle(0);
    }
    panic!("the cut never completed; waiting for {:?}", h.manager.awaiting_cut());
}

#[test]
fn cut_carries_online_player_records() {
    let mut h = floored_harness();
    h.event(session(0, 3, None), 0);
    rearrange_inventory(&mut h, 0);
    let cut = cut_now(&mut h);
    assert_eq!(cut.regions.len(), 1);
    assert_eq!(cut.players.len(), 1, "{:?}", cut.players);
    assert_eq!(cut.players[0].0, PlayerId(3));
    assert_eq!(cut.players[0].1.region, SPAWN_REGION);
    assert!(rearranged(&cut.players[0].1.client));
    assert_eq!(bodies_in(&mut h, SPAWN_REGION), 1, "a cut parks nobody");
    assert!(h.manager.parked_record(PlayerId(3)).is_none(), "an online player's record goes only into cuts");
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
    // Nothing runs: the save cut is complete at once.
    h.manager.begin_cut(0).expect("no cut pending");
    let mut store = crate::common::cut_store::CutStore::default();
    store.apply(&h.manager.take_cut().expect("complete"));
    drop(h);

    // A new process: client ids start at 0 again; the edge resolves the token.
    let mut h = floored_harness_with(InlineSpawner::with_portable(store.regions));
    h.event(session(0, 0, store.players.get(&PlayerId(0)).cloned()), 0);
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
    let record = h.manager.parked_record(PlayerId(0)).expect("parked where it went");
    assert_eq!(record.region, east);
    assert!(rearranged(&record.client));
    // And it comes back there.
    h.event(session(1, 0, None), 20);
    assert_eq!(bodies_in(&mut h, east), 1);
    assert!(rearranged(&client_in(&mut h, east, 1).unwrap()));
}

#[test]
fn starting_kit_only_for_new_player() {
    let reg = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    game::install_block_props(reg.props());
    assert!(game::Inventory::starting_kit().slots[0].is_some(), "the kit holds chests");
    let mut h = floored_harness();
    // A player the world has never seen gets the kit.
    h.event(session(0, 0, None), 0);
    let fresh = client_in(&mut h, SPAWN_REGION, 0).expect("a new player");
    assert_eq!(fresh.inventory, game::Inventory::starting_kit());
    // A known player who used it up comes back empty-handed, not with a new kit.
    let record = PlayerRecord {
        client: game::Client { inventory: game::Inventory::new(), ..game::Client::default() },
        pose: game::Pose::from_translation(body_on_tile([3, 1, 3])),
        region: SPAWN_REGION,
        fortress: None,
        adventure: None,
    };
    h.event(session(1, 1, Some(record)), 0);
    let back = client_in(&mut h, SPAWN_REGION, 1).expect("restored");
    assert_eq!(back.inventory, game::Inventory::new());
}

// ---- A region whose stored state cannot be read (world store Task 7) ----

/// An inline spawner whose `fail` region answers `LoadFailed` (its store
/// read failed) on its first `failures` starts; it keeps the dead start's
/// input channel open until the manager reaps it, as the server's region
/// thread does.
struct FailingSpawner {
    inner: InlineSpawner,
    fail: RegionCoords,
    failures: u32,
    starts: Vec<RegionCoords>,
    dead: std::collections::BTreeMap<RegionCoords, Receiver<game::RegionInput>>,
    /// Test support: the failure is reported only by [`Self::report_failures`]
    /// (a store read slower than the region's other traffic).
    deferred: bool,
    unreported: Vec<(RegionCoords, crossbeam::channel::Sender<(RegionCoords, RegionOutput)>)>,
}

impl FailingSpawner {
    fn new(fail: RegionCoords) -> Self {
        FailingSpawner {
            inner: InlineSpawner::default(),
            fail,
            failures: 1,
            starts: Vec::new(),
            dead: Default::default(),
            deferred: false,
            unreported: Vec::new(),
        }
    }

    fn report_failures(&mut self) {
        for (id, out) in self.unreported.drain(..) {
            let _ = out.send((id, RegionOutput::LoadFailed("test: the world store refused the read".into())));
        }
    }
}

impl game::RegionSpawner for FailingSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: game::RegionSeed,
        out: crossbeam::channel::Sender<(RegionCoords, RegionOutput)>,
    ) -> crossbeam::channel::Sender<game::RegionInput> {
        self.starts.push(id);
        if id == self.fail && self.failures > 0 {
            self.failures -= 1;
            let (send, recv) = unbounded();
            self.dead.insert(id, recv);
            self.unreported.push((id, out));
            if !self.deferred {
                self.report_failures();
            }
            return send;
        }
        self.inner.spawn(id, seed, out)
    }

    fn reap(&mut self, id: RegionCoords) {
        self.dead.remove(&id);
        self.inner.reap(id);
    }

    fn world_tick(&self) -> game::Tick {
        self.inner.world_tick()
    }
}

/// Review focus 3: a region whose store read fails does not start and is
/// never regenerated; what crosses into it waits in the mailbox, Play there
/// is refused `RegionUnavailable`, and after `STORE_LOAD_RETRY_SECS` of
/// world time it is started again (here by a Play), getting its arrivals.
#[test]
fn region_load_failure_never_regenerates() {
    crate::common::install_real_block_props();
    let east = RegionCoords::new(1, 0);
    let spawner = FailingSpawner::new(east);
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
        Box::new(crate::history_handover::floored_region),
        out_send,
        region_out_send,
        lod_builder,
        lod_result_recv,
    );
    // 3x3 tiles around the spawn region, every one with a dry landing.
    let tiles = (0..9).map(|_| play_tile()).collect();
    manager.set_world_map(game::WorldMap {
        origin: RegionCoords::new(-1, -1),
        width: 3,
        height: 3,
        tiles,
        rock_names: vec![],
        region_types: vec![],
    });
    let settle = |m: &mut WorldManager<FailingSpawner>, now: u64| {
        for _ in 0..3 {
            m.spawner_mut().inner.pump();
            while let Ok((rc, out)) = region_out.try_recv() {
                m.handle_region_output(rc, out, now);
            }
            m.pump_lod_results();
        }
    };
    manager.handle_server_event(session(0, 0, None), 0);
    settle(&mut manager, 0);

    // A slime departs the spawn region eastwards, and the east region is
    // started (subscribed) before the departure is routed: the crossing goes
    // to it, then its load fails.
    let edge = game::REGION_SIZE;
    let mut slime = None;
    manager.spawner_mut().inner.with_region(SPAWN_REGION, |region| {
        let y = game::BLOCK_VOXELS as f32 + 10.0;
        let e = crate::common::spawn_slime(region, [edge + game::FLIP_HYSTERESIS + 2.0, y, 128.0], 1, 0.0, 0);
        slime = *region.data().ecs.id.try_get(e);
    });
    let slime = slime.expect("the slime has an EntityId");
    manager.spawner_mut().inner.tick_all();
    manager.spawner_mut().inner.pump();
    manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(east), 0), 10);
    settle(&mut manager, 10);

    assert!(manager.is_unavailable(east), "the failed region is marked unavailable");
    assert!(!manager.running_regions().contains(&east), "and does not run");
    assert_eq!(manager.pending_arrival_count(east), 1, "the slime waits in the mailbox");
    assert_eq!(manager.location_of(slime), Some(east), "on its way to east");
    let mut in_spawn = true;
    manager.spawner_mut().inner.with_region(SPAWN_REGION, |r| in_spawn = r.data().id_index.contains_key(&slime));
    assert!(!in_spawn, "the slime left its source");

    // Play there is refused; asking for it again does not start it.
    packets.try_iter().count();
    manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(east), 0), 20);
    manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(east), 0), 20);
    settle(&mut manager, 20);
    let refused = packets.try_iter().any(|(to, p)| {
        to == Some(0) && matches!(p, ServerPacket::PlayRefused(rc, game::PlayRefusal::RegionUnavailable) if rc == east)
    });
    assert!(refused, "Play into an unavailable region is refused RegionUnavailable");
    assert_eq!(manager.spawner_mut().starts.iter().filter(|rc| **rc == east).count(), 1, "never started again (no regeneration)");

    // World time passes (the spawn region sits it out: no ticks to pay).
    let retry = game::STORE_LOAD_RETRY_TICKS;
    assert_eq!(retry, game::STORE_LOAD_RETRY_SECS * 1000 / game::TICK_RATE);
    manager.spawner_mut().inner.hold(SPAWN_REGION, retry as u32 + 2);
    for _ in 0..retry - 2 {
        manager.spawner_mut().inner.tick_all();
    }
    assert!(manager.is_unavailable(east), "still unavailable before the retry tick");
    for _ in 0..4 {
        manager.spawner_mut().inner.tick_all();
    }
    manager.spawner_mut().inner.hold(SPAWN_REGION, 0);
    assert!(!manager.is_unavailable(east));
    // The subscriber still asks for it: the retry starts it again, and the
    // slime is delivered before its first tick.
    manager.maintain(30);
    settle(&mut manager, 30);
    assert_eq!(manager.spawner_mut().starts.iter().filter(|rc| **rc == east).count(), 2, "started again after the retry tick");
    assert!(manager.running_regions().contains(&east));
    assert_eq!(manager.pending_arrival_count(east), 0, "the waiting slime was delivered");
    manager.spawner_mut().inner.tick_all();
    settle(&mut manager, 40);
    let mut arrived = false;
    manager.spawner_mut().inner.with_region(east, |r| arrived = r.data().id_index.contains_key(&slime));
    assert!(arrived, "the slime is in east, once");
    // And Play there is accepted now.
    packets.try_iter().count();
    manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(east), 0), 50);
    settle(&mut manager, 50);
    assert!(!packets.try_iter().any(|(_, p)| matches!(p, ServerPacket::PlayRefused(..))));
}

fn play_tile() -> game::TileSummary {
    game::TileSummary {
        ground_min_m: 0.0,
        ground_mean_m: 0.0,
        ground_max_m: 0.0,
        relief_m: 0.0,
        slope: [0.0, 0.0],
        sea_fraction: 0.0,
        lake_fraction: 0.0,
        lake_level_m: None,
        rivers: 0,
        river_max_discharge_m2: 0.0,
        river_max_width_m: 0.0,
        surface_rock: None,
        landing: Some([2, 2]),
        biome: game::Biome::Grassland,
        region: 0,
        temperature_c: 10.0,
        precip_mm: 700.0,
        savagery: 50,
        alignment: 0,
        volcano: false,
    }
}

/// A manager over a [`FailingSpawner`] on floored regions, with a 3x3 world
/// map around the spawn region.
struct FailHarness {
    manager: WorldManager<FailingSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

impl FailHarness {
    fn new(spawner: FailingSpawner) -> Self {
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
            Box::new(crate::history_handover::floored_region),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        let tiles = (0..9).map(|_| play_tile()).collect();
        manager.set_world_map(game::WorldMap {
            origin: RegionCoords::new(-1, -1),
            width: 3,
            height: 3,
            tiles,
            rock_names: vec![],
            region_types: vec![],
        });
        FailHarness { manager, region_out, packets }
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.manager.spawner_mut().inner.pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, 0);
            }
            self.manager.pump_lod_results();
        }
    }

    fn event(&mut self, ev: ServerEvent) {
        self.manager.handle_server_event(ev, 0);
        self.settle();
    }

    /// A save cut at the next tick, taken.
    fn cut(&mut self) -> game::WorldCut {
        use game::RegionSpawner;
        let world = self.manager.spawner_mut().world_tick();
        self.manager.begin_cut((world + 2).saturating_sub(game::SAVE_CUT_LEAD_TICKS)).expect("no cut pending");
        for _ in 0..game::SAVE_CUT_LEAD_TICKS + 4 {
            if let Some(cut) = self.manager.take_cut() {
                self.manager.cut_committed(cut.id, Vec::new());
                return cut;
            }
            self.manager.spawner_mut().inner.tick_all();
            self.settle();
        }
        panic!("the cut never completed; waiting for {:?}", self.manager.awaiting_cut());
    }

    /// World time passes beyond the retry of every failed region (the
    /// running regions sit it out).
    fn pass_retry(&mut self) {
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().inner.hold(rc, game::STORE_LOAD_RETRY_TICKS as u32 + 2);
        }
        for _ in 0..game::STORE_LOAD_RETRY_TICKS + 2 {
            self.manager.spawner_mut().inner.tick_all();
        }
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().inner.hold(rc, 0);
        }
    }

    fn client_in(&mut self, rc: RegionCoords, client: ClientId) -> Option<game::Client> {
        let mut out = None;
        self.manager.spawner_mut().inner.with_region(rc, |r| out = r.data().clients.get(&client).cloned());
        out
    }
}

/// The marker stacks of `rearrange_inventory`, kept as a park keeps them
/// (the cursor stack folded into the inventory).
fn holds_markers(client: &game::Client) -> bool {
    client.inventory.slots[MARK_SLOT] == Some(MARK_STACK)
        && client.cursor.is_none()
        && client.inventory.slots.iter().any(|s| *s == Some(MARK_CURSOR))
}

fn count_markers(cut: &game::WorldCut) -> (usize, usize) {
    let main = cut.players.iter().filter(|(_, r)| r.client.inventory.slots.contains(&Some(MARK_STACK))).count();
    let cursor = cut.players.iter().filter(|(_, r)| r.client.inventory.slots.contains(&Some(MARK_CURSOR))).count();
    (main, cursor)
}

/// A Play whose target's load fails AFTER the park answered: the move's
/// creation carried the only copy of the player's record. It is parked
/// again, so the next cut carries it once (items gained since the last cut
/// kept, none duplicated), and a reconnect restores it.
#[test]
fn record_of_a_move_into_a_failed_region_is_kept() {
    crate::common::install_real_block_props();
    let east = RegionCoords::new(1, 0);
    let mut spawner = FailingSpawner::new(east);
    spawner.deferred = true;
    let mut h = FailHarness::new(spawner);
    h.event(session(0, 0, None));
    let before = h.cut();
    assert_eq!(count_markers(&before), (0, 0), "the last cut predates the items");
    // Items gained since the last cut.
    h.manager.spawner_mut().inner.with_region(SPAWN_REGION, |region| {
        region.with_data(|d| {
            let c = d.clients.get_mut(&0).expect("the player");
            c.inventory.slots[MARK_SLOT] = Some(MARK_STACK);
            c.cursor = Some(MARK_CURSOR);
        });
    });
    // Play east: the park answers, the creation goes out, then east's load fails.
    h.event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(east), 0));
    h.manager.spawner_mut().report_failures();
    h.settle();
    assert!(h.manager.is_unavailable(east));
    let refused = h.packets.try_iter().any(|(to, p)| {
        to == Some(0) && matches!(&p, ServerPacket::Refused(why) if why == game::REGION_UNAVAILABLE_REFUSAL)
    });
    assert!(refused, "the session whose body was to be in east is ended");
    let parked = h.manager.parked_record(PlayerId(0)).expect("the carried record is parked again").clone();
    assert!(holds_markers(&parked.client), "{:?}", parked.client.inventory);
    let cut = h.cut();
    let mine: Vec<_> = cut.players.iter().filter(|(p, _)| *p == PlayerId(0)).collect();
    assert_eq!(mine.len(), 1, "{:?}", cut.players);
    assert_eq!(mine[0].1, parked, "the cut carries the carried record");
    assert_eq!(count_markers(&cut), (1, 1), "items conserved, none duplicated");
    // A reconnect restores it (in the region it left).
    h.event(session(1, 0, None));
    let back = h.client_in(SPAWN_REGION, 1).expect("restored where it stood");
    assert!(holds_markers(&back), "{:?}", back.inventory);
}

/// A reconnect restored into a region whose load fails: the record it
/// carried (the store's) is parked again — in the next cut once, and
/// restored by a reconnect after the retry.
#[test]
fn record_of_a_restore_into_a_failed_region_is_kept() {
    crate::common::install_real_block_props();
    let east = RegionCoords::new(1, 0);
    let mut h = FailHarness::new(FailingSpawner::new(east));
    let mut client = game::Client::default();
    client.inventory.slots[MARK_SLOT] = Some(MARK_STACK);
    client.inventory.slots[MARK_SLOT + 1] = Some(MARK_CURSOR);
    let stored = PlayerRecord {
        client,
        pose: game::Pose::from_translation(body_on_tile([5, 1, 7])),
        region: east,
        fortress: None,
        adventure: None,
    };
    h.event(session(0, 4, Some(stored.clone())));
    assert!(h.manager.is_unavailable(east));
    assert_eq!(h.manager.depths().sessions, 0, "the session is ended");
    assert_eq!(h.manager.parked_record(PlayerId(4)), Some(&stored), "the record is parked again");
    // Refused again while east is unavailable, the record kept.
    h.event(session(1, 4, None));
    assert_eq!(h.manager.depths().sessions, 0);
    let cut = h.cut();
    let mine: Vec<_> = cut.players.iter().filter(|(p, _)| *p == PlayerId(4)).collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].1, stored);
    assert_eq!(count_markers(&cut), (1, 1));
    // After the retry east loads, and the player is restored there.
    h.pass_retry();
    h.event(session(2, 4, None));
    let back = h.client_in(east, 2).expect("restored in east");
    assert_eq!(back.inventory.slots[MARK_SLOT], Some(MARK_STACK));
}

