use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, GameEvent, GameEventKind,
    InlineLodBuilder, InlineSpawner, InputEvent, LodBuilder, RegionCoords, RegionOutput,
    ServerEvent, ServerPacket, SubChunk, Tick, WorldManager, FLIP_HYSTERESIS, REGION_SIZE, SPAWN_REGION,
};

struct Harness {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    /// Every region output the manager was handed, reduced to what the
    /// slot tests read, in the order each region emitted it.
    trace: Vec<(RegionCoords, Traced)>,
}

/// One region output, as the transfer-slot tests see it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Traced {
    Tick,
    Arrived,
    /// A `Departures` output and its stamp.
    Departed(Tick),
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
        trace: Vec::new(),
    }
}

impl Harness {
    fn settle(&mut self, now_ms: u64) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                let traced = match &out {
                    RegionOutput::EventProcessed(ev) | RegionOutput::EventProcessedHashed(ev, ..) => {
                        match ev.kind {
                            GameEventKind::Tick => Some(Traced::Tick),
                            GameEventKind::EntityArrived(_) => Some(Traced::Arrived),
                            _ => None,
                        }
                    }
                    RegionOutput::Departures(_, stamp) => Some(Traced::Departed(*stamp)),
                    _ => None,
                };
                if let Some(t) = traced {
                    self.trace.push((rc, t));
                }
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
                    game::Pose::from_translation(game::rapier3d::math::Vector::new(x, 26.0, z)),
                )
            });
        });
    }
}

fn connect_and_subscribe(h: &mut Harness) {
    h.event(ServerEvent::connected(0), 0);
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
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let target = RegionCoords::new(1, 0);
    assert!(!h.manager.running_regions().contains(&target));

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(100);
    assert!(h.manager.running_regions().contains(&target), "arrival woke the cold target");
}

#[test]
fn halo_entity_updates_never_wake_a_cold_region() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let neighbour = RegionCoords::new(1, 0);

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE - 5.0, 128.0); // halo band, not past
    h.tick(100);
    assert!(
        !h.manager.running_regions().contains(&neighbour),
        "halo entity updates to cold regions are dropped"
    );
}

#[test]
fn halo_entity_updates_reach_running_neighbours() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let neighbour = RegionCoords::new(1, 0);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE - 5.0, 128.0);
    h.tick(100);
    let mut mirrors = 0;
    h.manager.spawner_mut().with_region(neighbour, |region| {
        mirrors = region.data().halo_entities.len();
    });
    assert_eq!(mirrors, 1, "band player mirrored into the running neighbour");
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

#[test]
fn late_started_region_starts_at_world_tick() {
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    for i in 0..30 {
        h.tick(i * 50);
    }
    let neighbour = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(neighbour), 0), 1500);
    h.tick(1550);
    let tick_of = |h: &mut Harness, rc: RegionCoords| {
        let mut t = None;
        h.manager.spawner_mut().with_region(rc, |r| t = Some(r.current_tick()));
        t.unwrap()
    };
    let a = tick_of(&mut h, SPAWN_REGION);
    let b = tick_of(&mut h, neighbour);
    assert!(a >= 31, "spawn region ran its rounds: {a}");
    assert_eq!(a, b, "late region joined the world tick numbering");
}

fn tick_of(h: &mut Harness, rc: RegionCoords) -> Tick {
    let mut t = None;
    h.manager.spawner_mut().with_region(rc, |r| t = Some(r.current_tick()));
    t.expect("region running")
}

/// From trace position `from` on, where `target` had completed `start` ticks:
/// its tick count each time it applied an `EntityArrived`.
fn arrival_ticks(h: &Harness, from: usize, target: RegionCoords, start: Tick) -> Vec<Tick> {
    let mut now = start;
    let mut arrivals = Vec::new();
    for (rc, t) in &h.trace[from..] {
        if *rc != target {
            continue;
        }
        match t {
            Traced::Tick => now += 1,
            Traced::Arrived => arrivals.push(now),
            Traced::Departed(_) => {}
        }
    }
    arrivals
}

fn departure_stamps(h: &Harness, from: usize, source: RegionCoords) -> Vec<Tick> {
    h.trace[from..]
        .iter()
        .filter_map(|(rc, t)| match t {
            Traced::Departed(stamp) if *rc == source => Some(*stamp),
            _ => None,
        })
        .collect()
}

/// Walks a few lockstep rounds first, so tick numbers are not all zero.
fn subscribed_and_warm() -> Harness {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    for i in 0..3 {
        h.tick(i * 50);
    }
    h
}

#[test]
fn arrival_lands_in_slot_after_departure_tick() {
    let mut h = subscribed_and_warm();
    let target = RegionCoords::new(1, 0);
    let mark = h.trace.len();
    let start = tick_of(&mut h, target);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(200);

    let stamps = departure_stamps(&h, mark, SPAWN_REGION);
    assert_eq!(stamps, vec![start + 1], "the departure is stamped with the source tick after it ran");
    // Slot `stamp`: after the target's tick that took it to `stamp`, before its next Tick.
    assert_eq!(arrival_ticks(&h, mark, target, start), vec![stamps[0]]);
}

#[test]
fn transfer_to_lagging_target_waits() {
    let mut h = subscribed_and_warm();
    let target = RegionCoords::new(1, 0);
    let mark = h.trace.len();
    let start = tick_of(&mut h, target);
    h.manager.spawner_mut().hold(target, 3);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    for i in 0..3 {
        h.tick(200 + i * 50);
    }
    assert_eq!(tick_of(&mut h, target), start, "held: the target has not ticked");
    assert!(arrival_ticks(&h, mark, target, start).is_empty(), "nothing applied before its slot");
    h.tick(400); // catch-up

    let stamps = departure_stamps(&h, mark, SPAWN_REGION);
    assert_eq!(stamps, vec![start + 1]);
    assert_eq!(
        arrival_ticks(&h, mark, target, start),
        vec![stamps[0]],
        "the arrival waited for the target to run the stamped tick"
    );
    assert_eq!(tick_of(&mut h, target), tick_of(&mut h, SPAWN_REGION), "the target caught up");
}

#[test]
fn transfer_to_leading_target_applies_on_receipt() {
    let mut h = subscribed_and_warm();
    let target = RegionCoords::new(1, 0);
    let mark = h.trace.len();
    let start = tick_of(&mut h, target);
    h.manager.spawner_mut().hold(SPAWN_REGION, 3);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    for i in 0..4 {
        h.tick(200 + i * 50);
    }

    let stamps = departure_stamps(&h, mark, SPAWN_REGION);
    assert_eq!(stamps, vec![start + 1], "the lagging source departs in its first catch-up tick");
    let arrivals = arrival_ticks(&h, mark, target, start);
    assert_eq!(arrivals, vec![start + 4], "applied at once, exactly once, by the leading target");
    let mut in_source = true;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        in_source = r.data().player_entites.contains_key(&0);
    });
    assert!(!in_source, "the body left the source");
}

/// Where `id` is in one autosave round's saves: `(region, "state" | "transfer")`.
fn saved_places(saves: Vec<(RegionCoords, game::RegionSave)>, id: game::EntityId) -> Vec<(RegionCoords, &'static str, Option<game::EntityBundle>)> {
    let mut at = Vec::new();
    for (rc, save) in saves {
        let (state, transfers) = match save {
            game::RegionSave::Live(state, transfers) => (*state, transfers),
            parked => {
                let saved = parked.into_serialized().to_saved().expect("parked blob decodes");
                (saved.state, saved.transfers)
            }
        };
        if state.id_index.contains_key(&id) {
            at.push((rc, "state", None));
        }
        at.extend(transfers.into_iter().filter(|(b, _)| b.id == id).map(|(b, _)| (rc, "transfer", Some(b))));
    }
    at
}

/// One whole autosave round: asked, answered, collected.
fn autosave(h: &mut Harness, now_ms: u64) -> Vec<(RegionCoords, game::RegionSave)> {
    h.manager.request_save();
    h.settle(now_ms);
    assert!(h.manager.awaiting_save().is_empty(), "every region asked answered");
    h.manager.take_saves()
}

/// Decision 3: a slime crossing into a region that is not running does not
/// wake it. It waits in the manager's mailbox — saved exactly once per
/// autosave round, with the region it left (rebased back, so restored it
/// crosses again) — and is delivered when the region next starts.
#[test]
fn slime_arrival_does_not_wake_unloaded_region() {
    crate::common::install_real_block_props();
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let north = RegionCoords::new(0, -1);
    assert!(!h.manager.running_regions().contains(&north));

    let mut id = None;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        let e = crate::common::spawn_slime(region, [128.0, 26.0, -(FLIP_HYSTERESIS + 2.0)], 1, 0.0, 0);
        id = *region.data().ecs.id.try_get(e);
    });
    let id = id.expect("the slime has an EntityId");
    h.tick(100);
    assert!(!h.manager.running_regions().contains(&north), "a slime's arrival must not wake its target");
    assert_eq!(h.manager.pending_arrival_count(north), 1, "the slime waits in the mailbox");
    assert_eq!(h.manager.depths().pending_arrivals, 1);
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        assert!(!region.data().id_index.contains_key(&id), "the slime left its source");
    });

    for round in 0..3 {
        h.tick(150 + round * 50);
        let at = saved_places(autosave(&mut h, 150 + round * 50), id);
        assert_eq!(at.len(), 1, "save {round}: the waiting slime is saved in {:?}", at.iter().map(|a| (a.0, a.1)).collect::<Vec<_>>());
        let (rc, what, bundle) = &at[0];
        assert_eq!((*rc, *what), (SPAWN_REGION, "transfer"), "saved with the region it left");
        let bundle = bundle.as_ref().unwrap();
        assert!(matches!(bundle.kind, game::EntityKind::Slime { size: 1 }));
        assert!(bundle.body.pose.translation.z < 0.0, "in its source's frame, past the seam: restored, it crosses again");
    }
    assert!(!h.manager.running_regions().contains(&north), "saving did not wake it either");

    // The region starts (subscribed): the slime is delivered before its first tick.
    let mark = h.trace.len();
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north), 0), 400);
    assert_eq!(h.manager.pending_arrival_count(north), 0, "delivered on start");
    h.tick(420);
    let first = h.trace[mark..].iter().find(|(rc, _)| *rc == north).map(|(_, t)| *t);
    assert_eq!(first, Some(Traced::Arrived), "applied before the region's first tick");
    let mut slime = None;
    h.manager.spawner_mut().with_region(north, |region| {
        let data = region.data();
        let key = *data.id_index.get(&id).expect("north owns the slime");
        slime = Some((*data.ecs.kind.try_get(key), data.ecs.slime.try_get(key).is_some()));
    });
    assert_eq!(slime, Some((Some(game::EntityKind::Slime { size: 1 }), true)), "a whole slime arrived");
    assert_eq!(h.manager.in_flight_count(), 0, "its arrival is confirmed");

    for round in 0..2 {
        h.tick(450 + round * 50);
        let at = saved_places(autosave(&mut h, 450 + round * 50), id);
        let at: Vec<_> = at.into_iter().map(|a| (a.0, a.1)).collect();
        assert_eq!(at, [(north, "state")], "save {round} after delivery");
    }
}
