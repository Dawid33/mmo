use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientId, ClientPacket, GameEvent, GameEventKind, InlineSpawner, InputEvent, RegionCoords, RegionOutput,
    ServerEvent, ServerPacket, Tick, WorldManager, FLIP_HYSTERESIS, REGION_SIZE, SPAWN_REGION,
};

/// A body-centre height just above the floor (`common::above_floor_y`).
fn above_floor() -> f32 {
    crate::common::above_floor_y()
}

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
    // Flat stud ground under chunk column (0, 0) of every region.
    let spawner = crate::common::stud_spawner(|_rc| {
        crate::common::floor_seed(0.0..1.0, 0.0..1.0, crate::common::FLOOR_PLATES)
    });
    Harness {
        manager: crate::common::stud_manager(spawner, out_send, region_out_send),
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
                    game::Pose::from_translation(game::rapier3d::math::Vector::new(x, above_floor(), z)),
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
#[ignore = "slow: >100ms — run via just test-full"]
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
#[ignore = "slow: >100ms — run via just test-full"]
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
#[ignore = "slow: >100ms — run via just test-full"]
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
#[ignore = "slow: >100ms — run via just test-full"]
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
#[ignore = "slow: >100ms — run via just test-full"]
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
#[ignore = "slow: >100ms — run via just test-full"]
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

/// One whole save cut at the next tick (lockstep: every region reaches it
/// in one round): asked, answered, taken, and committed into `store`.
fn cut_into(h: &mut Harness, store: &mut crate::common::cut_store::CutStore, now_ms: u64) -> game::WorldCut {
    use game::RegionSpawner;
    let world = h.manager.spawner_mut().world_tick();
    h.manager.begin_cut((world + 2).saturating_sub(game::SAVE_CUT_LEAD_TICKS)).expect("no cut pending");
    for _ in 0..game::SAVE_CUT_LEAD_TICKS + 4 {
        if let Some(cut) = h.manager.take_cut() {
            store.apply(&cut);
            h.manager.cut_committed(cut.id, Vec::new());
            return cut;
        }
        h.tick(now_ms);
    }
    panic!("the cut never completed; waiting for {:?}", h.manager.awaiting_cut());
}

/// The in-flight row of `id` in `store`, if any.
fn row_of(store: &crate::common::cut_store::CutStore, id: game::EntityId) -> Option<game::EntityBundle> {
    store.in_flight.iter().find_map(|row| match row {
        game::InFlightRow::Transfer { bundle, .. } if bundle.id == id => Some(bundle.clone()),
        _ => None,
    })
}

/// Decision 3: a slime crossing into a region that is not running does not
/// wake it. It waits in the manager's mailbox — in every save cut exactly
/// once, as an in-flight row to its target (in the target's frame) — and is
/// delivered when the region next starts.
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
        let e = crate::common::spawn_slime(region, [128.0, above_floor(), -(FLIP_HYSTERESIS + 2.0)], 1, 0.0, 0);
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

    let mut store = crate::common::cut_store::CutStore::default();
    for round in 0..3 {
        h.tick(150 + round * 50);
        cut_into(&mut h, &mut store, 150 + round * 50);
        let at = store.places(id);
        assert_eq!(at, [(north, "row")], "cut {round}: the waiting slime is saved once, on its way to its target");
        let bundle = row_of(&store, id).unwrap();
        assert!(matches!(bundle.kind, game::EntityKind::Slime { size: 1 }));
        assert!(bundle.body.pose.translation.z > REGION_SIZE / 2.0, "in its target's frame, just past the seam");
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
        cut_into(&mut h, &mut store, 450 + round * 50);
        assert_eq!(store.places(id), [(north, "state")], "cut {round} after delivery");
    }
}

/// R29: the mailbox is bounded. A running region next to a parked one
/// keeps pushing slimes across the seam; the mailbox never holds more than
/// `PENDING_ARRIVALS_PER_REGION_MAX` for its target — the oldest waiting
/// arrival is despawned (and counted) to make room — so the slime count of
/// running regions + mailbox stays bounded, and so does what is saved.
#[test]
fn mailbox_is_capped_and_despawns_the_oldest() {
    use game::{PENDING_ARRIVALS_PER_REGION_MAX as CAP, UNLOAD_GRACE_MS};
    crate::common::install_real_block_props();
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    let north = RegionCoords::new(0, -1);
    // North runs, then parks.
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north), 0), 0);
    h.tick(50);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(north), 0), 100);
    let mut now = 100 + UNLOAD_GRACE_MS;
    h.manager.maintain(now);
    h.settle(now);
    assert!(h.manager.parked_regions().contains(&north), "north parked");
    h.drain_packets();

    let walked = 3 * CAP;
    let mut ids = Vec::new();
    for i in 0..walked {
        h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
            let tick = region.current_tick();
            let e = crate::common::spawn_slime(region, [128.0 + i as f32, above_floor(), -(FLIP_HYSTERESIS + 2.0)], 1, 0.0, tick);
            ids.push(region.data().ecs.id.try_get(e).expect("an EntityId"));
        });
        now += 50;
        h.tick(now);
        assert!(!h.manager.running_regions().contains(&north), "slimes never wake north");
        assert!(h.manager.pending_arrival_count(north) <= CAP, "mailbox over its cap");
        let mut running_slimes = 0;
        h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
            running_slimes = region.data().ecs.slime.iter().filter(|(_, s)| s.is_some()).count();
        });
        assert!(
            running_slimes + h.manager.pending_arrival_total() <= game::slime::SLIME_CAP_PER_REGION + CAP,
            "slimes unbounded: {running_slimes} running + {} waiting",
            h.manager.pending_arrival_total()
        );
    }
    assert_eq!(h.manager.pending_arrival_count(north), CAP);
    assert_eq!(h.manager.arrivals_despawned(), (walked - CAP) as u64, "each overflow despawned one");
    assert_eq!(h.manager.depths().arrivals_despawned, (walked - CAP) as u64);
    // The NEWEST waited; the oldest were despawned.
    let waiting: Vec<_> = h.manager.pending_arrival_ids().into_iter().map(|(id, _)| id).collect();
    assert_eq!(waiting, ids[walked - CAP..].to_vec(), "the oldest went first");
    // A cut holds each waiting slime once and no despawned one.
    let mut store = crate::common::cut_store::CutStore::default();
    cut_into(&mut h, &mut store, now);
    for (i, id) in ids.iter().enumerate() {
        let want = if i >= walked - CAP { 1 } else { 0 };
        let n = store.places(*id).len();
        assert_eq!(n, want, "slime {i} saved {n} times");
    }
}

// --- Dynamic bodies and the mailbox (Task 7.4) --------------------------------

/// A 1 m cube body.
fn cube_collider() -> game::ColliderSpec {
    let b = crate::common::TILE_W;
    game::ColliderSpec::Cuboid { half_extents: game::rapier3d::math::Vector::splat(b / 2.0) }
}

/// Spawns a dynamic body in the spawn region just past its north seam (so it
/// departs on the next tick), resting still. Returns its id.
fn spawn_body_past_north_seam(h: &mut Harness) -> game::EntityId {
    use game::rapier3d::math::Vector;
    let mut id = None;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        let pose = game::Pose::from_translation(Vector::new(128.0, above_floor(), -(FLIP_HYSTERESIS + 2.0)));
        region.with_data(|d| {
            let e = d.spawn_body_safe(pose, cube_collider(), Vector::ZERO, Vector::ZERO);
            id = *d.ecs.id.try_get(e);
        });
    });
    id.expect("the body has an EntityId")
}

/// The kind of `id` in `rc` and whether its rigid body is dynamic, if `rc` owns it.
fn owned_body(h: &mut Harness, rc: RegionCoords, id: game::EntityId) -> Option<(Option<game::EntityKind>, bool)> {
    let mut out = None;
    h.manager.spawner_mut().with_region(rc, |region| {
        let data = region.data();
        if let Some(&key) = data.id_index.get(&id) {
            let dynamic = data.ecs.rigidbody.try_get(key).and_then(|h| data.physics.bodies.get(h)).is_some_and(|b| b.is_dynamic());
            out = Some((*data.ecs.kind.try_get(key), dynamic));
        }
    });
    out
}

/// Only the spawn region runs; a body departs north, into a region that
/// never ran.
fn body_waiting_north(h: &mut Harness) -> game::EntityId {
    crate::common::install_real_block_props();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let id = spawn_body_past_north_seam(h);
    h.tick(100);
    id
}

/// Decision 3 for dynamic bodies: a body crossing into a region that is not
/// running does not wake it; it waits in the mailbox.
#[test]
fn body_arrival_does_not_wake_unloaded_region() {
    let mut h = harness();
    let id = body_waiting_north(&mut h);
    let north = RegionCoords::new(0, -1);
    assert!(!h.manager.running_regions().contains(&north), "a body's arrival must not wake its target");
    assert_eq!(h.manager.pending_arrival_count(north), 1, "the body waits in the mailbox");
    assert_eq!(h.manager.pending_arrival_ids(), [(id, north)]);
    assert_eq!(owned_body(&mut h, SPAWN_REGION, id), None, "the body left its source");
    assert_eq!(h.manager.location_of(id), Some(north), "the directory points at its target");
}

/// The waiting body is delivered when its region starts: it exists there
/// exactly once, still a dynamic `Body`, and the mailbox is empty.
#[test]
fn body_mailbox_delivers_when_region_starts() {
    let mut h = harness();
    let id = body_waiting_north(&mut h);
    let north = RegionCoords::new(0, -1);
    let mark = h.trace.len();
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north), 0), 400);
    assert_eq!(h.manager.pending_arrival_count(north), 0, "delivered on start");
    h.tick(420);
    let first = h.trace[mark..].iter().find(|(rc, _)| *rc == north).map(|(_, t)| *t);
    assert_eq!(first, Some(Traced::Arrived), "applied before the region's first tick");
    assert_eq!(owned_body(&mut h, north, id), Some((Some(game::EntityKind::Body), true)), "a dynamic body arrived");
    assert_eq!(owned_body(&mut h, SPAWN_REGION, id), None, "and only there");
    assert_eq!(h.manager.in_flight_count(), 0, "its arrival is confirmed");
    assert_eq!(h.manager.pending_arrival_total(), 0);
}

/// The target is stopping (Shutdown sent, `Stopped` not back) when the body
/// departs: the body waits in the mailbox, the region parks and stays parked
/// (a body never wakes it), and the resubscribe delivers the body once.
#[test]
fn body_mailbox_survives_stopping_region() {
    use game::UNLOAD_GRACE_MS;
    crate::common::install_real_block_props();
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    let north = RegionCoords::new(0, -1);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north), 0), 0);
    h.tick(50);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(north), 0), 100);
    let now = 100 + UNLOAD_GRACE_MS;
    // The Shutdown sits unread in north's queue until released.
    h.manager.spawner_mut().hold(north, u32::MAX);
    h.manager.maintain(now);
    h.settle(now);
    h.drain_packets();

    let id = spawn_body_past_north_seam(&mut h);
    h.tick(now + 50);
    assert_eq!(h.manager.pending_arrival_count(north), 1, "the body waits for the stopping region");
    assert_eq!(owned_body(&mut h, SPAWN_REGION, id), None, "the body left its source");

    h.manager.spawner_mut().hold(north, 0);
    h.tick(now + 100);
    h.tick(now + 150);
    assert!(h.manager.parked_regions().contains(&north), "north parked");
    assert!(!h.manager.running_regions().contains(&north), "the body did not wake north once it parked");
    assert_eq!(h.manager.pending_arrival_count(north), 1, "still waiting");
    assert_eq!(h.manager.location_of(id), Some(north), "the park kept its directory entry");

    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north), 0), now + 200);
    assert_eq!(h.manager.pending_arrival_count(north), 0, "delivered on the resubscribe");
    h.tick(now + 250);
    assert_eq!(owned_body(&mut h, north, id), Some((Some(game::EntityKind::Body), true)), "the body arrived");
    assert_eq!(owned_body(&mut h, SPAWN_REGION, id), None, "exactly once");
    assert_eq!(h.manager.in_flight_count(), 0);
}

/// Every item in `rc`'s piles: item -> count.
fn piled_items(h: &mut Harness, rc: RegionCoords) -> std::collections::BTreeMap<game::ItemId, u32> {
    let mut out = std::collections::BTreeMap::new();
    h.manager.spawner_mut().with_region(rc, |region| {
        for pile in region.data().item_piles.values() {
            for s in &pile.stacks {
                *out.entry(s.item).or_insert(0) += s.count;
            }
        }
    });
    out
}

/// A body departing `SPAWN_REGION` north at `x` (units), synthetic: it
/// carries `drops` (what a body leaves behind if destroyed — no body spawns
/// with drops yet; voxel bodies will).
fn departing_body(serial: u64, x: f32, drops: Vec<game::ItemStack>) -> game::EntityBundle {
    use game::rapier3d::math::Vector;
    let pose = game::Pose::from_translation(Vector::new(x, above_floor(), -(FLIP_HYSTERESIS + 2.0)));
    game::EntityBundle {
        id: game::EntityId { birth_region: SPAWN_REGION, serial: 1_000_000 + serial },
        kind: game::EntityKind::Body,
        body: game::body::body_state(pose, &cube_collider(), Vector::ZERO, Vector::ZERO),
        collider: cube_collider(),
        carried: game::Carried { drops, ..Default::default() },
        client: None,
        pending_inputs: Vec::new(),
    }
}

/// Overflow policy: a waiting body that carries drops is never despawned
/// silently. Pushed out of the full mailbox, it is counted as despawned and
/// its drops are put down as item piles at the seam cell on the side of the
/// region it left — every item conserved; a plain body is wildlife.
#[test]
fn mailbox_overflow_puts_a_bodys_drops_down() {
    use game::PENDING_ARRIVALS_PER_REGION_MAX as CAP;
    crate::common::install_real_block_props();
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let north = RegionCoords::new(0, -1);
    let item = game::ItemId::block(game::BlockId(1));
    let drops = vec![game::ItemStack { item, count: 5 }];
    let mut now = 50;
    h.tick(now);
    let mut stamp = 0;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| stamp = r.current_tick());
    // The oldest carries drops; CAP plain bodies follow it.
    let x = 128.0;
    let with_drops = departing_body(0, x, drops.clone());
    h.manager.handle_region_output(SPAWN_REGION, RegionOutput::Departures(vec![(with_drops.clone(), north)], stamp), now);
    for i in 1..=CAP as u64 {
        let plain = departing_body(i, x + i as f32, Vec::new());
        h.manager.handle_region_output(SPAWN_REGION, RegionOutput::Departures(vec![(plain, north)], stamp), now);
    }
    assert!(!h.manager.running_regions().contains(&north), "bodies never wake north");
    assert_eq!(h.manager.pending_arrival_count(north), CAP);
    assert_eq!(h.manager.arrivals_despawned(), 1, "the body with drops was pushed out, counted");
    assert_eq!(h.manager.recently_despawned_arrivals(), [with_drops.id]);
    for _ in 0..3 {
        now += 50;
        h.tick(now);
    }
    assert_eq!(piled_items(&mut h, SPAWN_REGION).get(&item), Some(&5), "every dropped item lies in a pile");
    assert_eq!(h.manager.items_destroyed(), 0);
    assert_eq!(h.manager.intent_replies_lost(), 0);
    // At the seam: the source's row of cells next to north.
    let mut cells = Vec::new();
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        cells = region.data().item_piles.keys().map(|c| c.to_local().1).collect();
    });
    let b = crate::common::TILE_W;
    let col = (x / b).floor() as i32;
    for c in &cells {
        assert!(c.z <= 1 && (c.x - col).abs() <= 1, "pile at {c:?}, not at the seam cell (x {col}, z 0)");
    }
}

/// A falling stud body (a felled tree, `DropItems`) that tips into a region
/// that is not running does not wait out the park in the mailbox, logs and
/// all: it settles at once, its drops put down at the seam cell on the side
/// of the region it left. North is not woken.
#[test]
fn stud_body_into_a_region_not_running_settles_at_the_seam() {
    crate::common::install_real_block_props();
    let mut h = harness();
    h.event(ServerEvent::connected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let north = RegionCoords::new(0, -1);
    let item = game::ItemId::block(game::BlockId(1));
    let mut now = 50;
    h.tick(now);
    let mut stamp = 0;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| stamp = r.current_tick());
    let mut body = departing_body(0, 128.0, vec![game::ItemStack { item, count: 4 }]);
    body.carried.stud_body = Some(game::StudBody { policy: game::SettlePolicy::DropItems, ..Default::default() });
    let id = body.id;
    h.manager.handle_region_output(SPAWN_REGION, RegionOutput::Departures(vec![(body, north)], stamp), now);
    assert!(!h.manager.running_regions().contains(&north), "a body never wakes north");
    assert_eq!(h.manager.pending_arrival_count(north), 0, "it does not wait in the mailbox");
    assert_eq!(h.manager.location_of(id), None, "it is gone from the directory");
    for _ in 0..3 {
        now += 50;
        h.tick(now);
    }
    assert_eq!(piled_items(&mut h, SPAWN_REGION).get(&item), Some(&4), "its drops lie at the seam");
    assert_eq!(h.manager.items_destroyed(), 0);
}

/// Crossings a restarted server restores from the store into a region that is
/// not running wait under the same cap as any other: past
/// `PENDING_ARRIVALS_PER_REGION_MAX` the oldest are despawned and counted.
#[test]
fn restored_in_flight_arrivals_respect_the_mailbox_cap() {
    use game::PENDING_ARRIVALS_PER_REGION_MAX as CAP;
    let mut h = harness();
    let north = RegionCoords::new(0, -1);
    let rows: Vec<game::InFlightRow> = (0..CAP as u64 + 3)
        .map(|i| game::InFlightRow::Transfer { target: north, bundle: departing_body(i, 128.0, Vec::new()), stamp: 5 })
        .collect();
    let first = match &rows[0] {
        game::InFlightRow::Transfer { bundle, .. } => bundle.id,
        _ => unreachable!(),
    };
    h.manager.restore_in_flight(rows);
    assert!(!h.manager.running_regions().contains(&north), "restored bodies never wake north");
    assert_eq!(h.manager.pending_arrival_count(north), CAP);
    assert_eq!(h.manager.arrivals_despawned(), 3);
    assert!(h.manager.recently_despawned_arrivals().contains(&first), "the oldest made room");
}
