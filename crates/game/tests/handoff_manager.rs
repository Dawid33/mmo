use crossbeam::channel::{unbounded, Receiver};
use game::{
    Chunk, ChunkCoords, ClientId, ClientPacket, GameEvent, GameEventKind, InlineSpawner,
    InputEvent, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager,
    FLIP_HYSTERESIS, REGION_SIZE, SPAWN_REGION,
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
    fn settle(&mut self, now_ms: u64) {
        for _ in 0..3 {
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
    fn tick(&mut self, now_ms: u64) {
        self.manager.spawner_mut().tick_all();
        self.settle(now_ms);
    }
    fn drain_packets(&mut self) -> Vec<(Option<ClientId>, ServerPacket)> {
        self.packets.try_iter().collect()
    }
    /// Teleport client 0's player inside its home region (test surgery via
    /// InlineSpawner::with_region — add alongside kill()).
    fn teleport_player(&mut self, rc: RegionCoords, client: ClientId, x: f32, z: f32) {
        self.manager.spawner_mut().with_region(rc, |region| {
            let key = *region.data().player_entites.get(&client).unwrap();
            region.with_data(|d| {
                d.set_body_pose_safe(
                    key,
                    game::IsometryReal::from_parts(
                        game::na::Translation3::new(x.into(), 26.0f32.into(), z.into()),
                        game::na::Unit::<game::na::Quaternion<game::parry::math::Real>>::identity(),
                    ),
                )
            });
        });
    }
}

fn connect_and_subscribe(h: &mut Harness) {
    h.event(ServerEvent::ClientConnected(0), 0);
    for rc in SPAWN_REGION.window_3x3() {
        h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    }
    h.drain_packets();
}

#[test]
fn crossing_flips_ownership_home_and_pushes_player_region() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let target = RegionCoords::new(1, 0);

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(100); // scan extracts; relay injects

    // Player owned by the target region now.
    let mut found_home = None;
    h.manager.spawner_mut().with_region(target, |region| {
        found_home = region.data().player_entites.get(&0).copied();
    });
    assert!(found_home.is_some(), "player entity arrived in {:?}", target);

    let packets = h.drain_packets();
    assert!(
        packets.iter().any(|(to, p)| *to == Some(0)
            && matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == target)),
        "authoritative home push after flip"
    );
    // The arrival also fanned out to subscribers as an EventProcessed.
    assert!(packets.iter().any(|(_, p)| matches!(
        p,
        ServerPacket::GameEvent(ev) if matches!(ev.kind, GameEventKind::EntityArrived(_))
    )));
}

#[test]
fn arrival_wakes_a_cold_region() {
    let mut h = harness();
    // Connect but subscribe to nothing beyond home: target region is cold.
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let target = RegionCoords::new(1, 0);
    assert!(!h.manager.running_regions().contains(&target));

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(100);
    assert!(h.manager.running_regions().contains(&target), "arrival woke the cold target");
}

#[test]
fn ghost_updates_never_wake_a_cold_region() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let neighbour = RegionCoords::new(1, 0);

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE - 5.0, 128.0); // margin, not past
    h.tick(100);
    assert!(
        !h.manager.running_regions().contains(&neighbour),
        "ghost updates to cold regions are dropped"
    );
}

#[test]
fn ghost_updates_reach_running_neighbours() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let neighbour = RegionCoords::new(1, 0);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE - 5.0, 128.0);
    h.tick(100);
    let mut ghost_count = 0;
    h.manager.spawner_mut().with_region(neighbour, |region| {
        ghost_count = region.data().ghosts.len();
    });
    assert_eq!(ghost_count, 1, "margin player mirrored into the running neighbour");
}

#[test]
fn input_routes_by_home_ignoring_the_stamp() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let target = RegionCoords::new(1, 0);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(100);
    h.drain_packets();

    // Client still stamps the OLD home (its prediction hasn't confirmed).
    let ev = GameEvent::new(
        GameEventKind::PlayerInput(0, InputEvent::Key { key: game::Key::KeyE, pressed: true }),
        0,
        SPAWN_REGION,
    );
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 200);
    // The input reached the NEW home: its EventProcessed comes from `target`.
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(_, p)| matches!(
        p,
        ServerPacket::GameEvent(ev)
            if ev.region_id == target && matches!(ev.kind, GameEventKind::PlayerInput(0, _))
    )));
}
