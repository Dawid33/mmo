use crossbeam::channel::{unbounded, Receiver};
use game::{
    Chunk, ChunkCoords, ClientId, ClientPacket, GameEvent, GameEventKind, InlineSpawner,
    RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager, SPAWN_REGION,
    UNLOAD_GRACE_MS,
};

struct Harness {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

fn harness() -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let generator = Box::new(|_rc: RegionCoords| -> Vec<(ChunkCoords, Chunk)> {
        vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))]
    });
    Harness {
        manager: WorldManager::new(InlineSpawner::default(), generator, out_send, region_out_send),
        region_out,
        packets,
    }
}

impl Harness {
    /// Route → pump runners → route outputs, twice (outputs can trigger
    /// respawns/resubscribes that need one more pump). Mirrors LocalServer.
    fn settle(&mut self, now_ms: u64) {
        for _ in 0..2 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now_ms);
            }
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
fn events_route_only_to_subscribed_regions() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.drain_packets();

    // Subscribed: a generic (non-input) event comes back as an authoritative
    // GameEvent. (PlayerInput is now home-routed, so it can't probe the
    // subscription path — use CreateClient, which flows through it.)
    let ev = GameEvent::new(GameEventKind::CreateClient(0), 0, rc);
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0);
    assert!(h.drain_packets().iter().any(|(_, p)| matches!(p, ServerPacket::GameEvent(_))));

    // Not subscribed: dropped silently.
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

    // Next routed event detects the death, respawns, resnapshots. (A
    // subscription-routed event, not PlayerInput, which is now home-routed.)
    let ev = GameEvent::new(GameEventKind::CreateClient(0), 0, rc);
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 500);
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
