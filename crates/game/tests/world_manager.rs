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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    h.event(ServerEvent::ClientDisconnected(0), 100);
    h.event(ServerEvent::ClientConnected(0), 200);
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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
    let ev = GameEvent::new(GameEventKind::Quit, 0, SPAWN_REGION);
    assert!(!h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0));
}

/// Connect client 0 and drain the join packets, returning the harness ready
/// for LOD-serving assertions.
fn connected_harness() -> Harness {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
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

    manager.handle_server_event(ServerEvent::ClientConnected(1), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
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
    h.event(ServerEvent::ClientConnected(0), 0);
    let before = h.manager.depths().sessions;
    assert_eq!(before, 1);
    h.event(ServerEvent::ClientPacket(ClientPacket::Leave, 0), 10);
    assert_eq!(h.manager.depths().sessions, before - 1);
    // The transport closing afterwards is a no-op.
    h.event(ServerEvent::ClientDisconnected(0), 20);
    assert_eq!(h.manager.depths().sessions, before - 1);
}
