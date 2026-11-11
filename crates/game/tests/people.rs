use crate::common;
use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientId, ClientPacket, InlineSpawner, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager,
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
    // Flat stud ground under chunk column (0, 0) of every region.
    let spawner = common::stud_spawner(|_rc| common::floor_seed(0.0..1.0, 0.0..1.0, common::FLOOR_PLATES));
    Harness {
        manager: common::stud_manager(spawner, out_send, region_out_send),
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
    #[allow(dead_code)]
    fn teleport_player(&mut self, rc: RegionCoords, client: ClientId, x: f32, z: f32) {
        self.manager.spawner_mut().with_region(rc, |region| {
            let key = *region.data().player_entites.get(&client).unwrap();
            region.with_data(|d| {
                d.set_body_pose_safe(
                    key,
                    game::Pose::from_translation(game::rapier3d::math::Vector::new(x, common::above_floor_y(), z)),
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


use game::history::PersonId;
use game::rapier3d::math::Vector;

fn flat_region() -> game::Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    let seed = common::floor_seed(0.0..2.0 * common::CHUNK_EDGE, 0.0..2.0 * common::CHUNK_EDGE, common::FLOOR_PLATES);
    let mut region = game::Region::new(game::Rollback::new(Some(send.clone())), Some(send), common::r0(), None);
    region.seed_stud_world(seed);
    region
}

const P: PersonId = PersonId(7);

#[test]
fn spawn_person_is_idempotent() {
    let mut region = flat_region();
    let mut first = None;
    let mut second = Some(game::EntityKey::default());
    region.with_data(|d| {
        first = d.spawn_person_safe(P, (100.0, 100.0));
        second = d.spawn_person_safe(P, (120.0, 100.0));
    });
    assert!(first.is_some());
    assert!(second.is_none(), "a second spawn changes nothing");
    let d = region.data();
    assert_eq!(d.person_entity(P), first);
    assert_eq!(d.people_here().len(), 1);
    assert!(matches!(*d.ecs.kind.try_get(first.unwrap()), Some(game::EntityKind::Person { person }) if person == P));
    assert!(d.ecs.animator.try_get(first.unwrap()).is_some(), "dwarf rig attached");
    assert!(d.ecs.camera.try_get(first.unwrap()).is_none(), "no camera");
    assert!(d.clients.is_empty(), "no client");
}

#[test]
fn people_here_lists_bodies_in_id_order() {
    let mut region = flat_region();
    region.with_data(|d| {
        d.spawn_person_safe(PersonId(9), (100.0, 100.0));
        d.spawn_person_safe(PersonId(2), (50.0, 60.0));
    });
    let here = region.data().people_here();
    assert_eq!(here.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), vec![2, 9]);
    assert_eq!((here[0].1[0], here[0].1[2]), (50.0, 60.0));
}

fn flat_rollback() -> game::Rollback {
    let (send, recv) = crossbeam::channel::unbounded();
    let seed = common::floor_seed(0.0..2.0 * common::CHUNK_EDGE, 0.0..2.0 * common::CHUNK_EDGE, common::FLOOR_PLATES);
    let mut r = game::Rollback::new(Some(send));
    common::seed_studs(&mut r, seed);
    while recv.try_recv().is_ok() {}
    std::mem::forget(recv);
    r
}

#[test]
fn despawn_person_undo_restores_hash() {
    let mut d = flat_rollback();
    d.new_transaction();
    d.spawn_person_safe(P, (100.0, 100.0));
    d.forget();
    let before = game::state_hash(&d);
    d.new_transaction();
    assert!(d.despawn_person_safe(P));
    assert!(d.person_entity(P).is_none());
    assert!(!d.despawn_person_safe(P), "nothing left to despawn");
    d.rollback();
    assert_eq!(before, game::state_hash(&d), "undo restores the exact state");
    assert!(d.person_entity(P).is_some());
}

#[test]
fn spawn_person_undo_restores_hash() {
    let mut d = flat_rollback();
    let before = game::state_hash(&d);
    d.new_transaction();
    d.spawn_person_safe(P, (100.0, 100.0));
    d.rollback();
    assert_eq!(before, game::state_hash(&d));
    assert!(d.person_entity(P).is_none());
}

#[test]
fn person_crosses_seam_keeps_identity() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let target = RegionCoords::new(1, 0);
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        region.with_data(|d| {
            let e = d.spawn_person_safe(P, (100.0, 128.0)).unwrap();
            d.set_body_pose_safe(
                e,
                game::Pose::from_translation(Vector::new(REGION_SIZE + FLIP_HYSTERESIS + 2.0, 26.0, 128.0)),
            );
        });
    });
    h.tick(100);
    let mut here = Some(game::EntityKey::default());
    let mut there = None;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| here = r.data().person_entity(P));
    h.manager.spawner_mut().with_region(target, |r| there = r.data().person_entity(P));
    assert!(here.is_none(), "source forgot the person");
    assert!(there.is_some(), "target has the person");
}

#[test]
fn arrival_of_a_person_with_a_body_is_ignored() {
    let mut region = flat_region();
    region.with_data(|d| {
        let e = d.spawn_person_safe(P, (100.0, 100.0)).unwrap();
        let bundle = d.extract_entity_safe(e);
        // Same person arrives twice under different identities.
        let mut dup = bundle.clone();
        d.apply_arrival(bundle);
        dup.id.serial += 1;
        assert!(d.apply_arrival(dup).is_none(), "duplicate person ignored");
        assert_eq!(d.people_here().len(), 1);
        assert_eq!(d.ecs.kind.iter().filter(|(_, k)| matches!(k, Some(game::EntityKind::Person { .. }))).count(), 1);
    });
}

// ---- walking ----

use game::{Goal, GameEventKind, Region, REGION_BLOCKS};

fn floored_region() -> Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(game::Rollback::new(Some(send.clone())), Some(send), common::r0(), None);
    let size = REGION_SIZE;
    region.seed_stud_world(common::floor_seed(0.0..size, 0.0..size, common::FLOOR_PLATES));
    region
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

fn column_xz(c: [i32; 2]) -> (f32, f32) {
    let bv = common::TILE_W;
    ((c[0] as f32 + 0.5) * bv, (c[1] as f32 + 0.5) * bv)
}

fn person_xz(region: &Region) -> (f32, f32) {
    let here = region.data().people_here();
    (here[0].1[0], here[0].1[2])
}

fn goal_here(c: [i32; 2]) -> Goal {
    Goal::local(common::r0(), c, None)
}

#[test]
fn person_walks_to_goal() {
    let mut region = floored_region();
    let start = column_xz([10, 12]);
    region.with_data(|d| {
        d.spawn_person_safe(P, start).unwrap();
    });
    for _ in 0..10 {
        tick(&mut region);
    }
    let e = region.data().person_entity(P).unwrap();
    let mut ok = false;
    region.with_data(|d| ok = d.walk_entity_safe(e, goal_here([20, 12]), common::r0(), None));
    assert!(ok, "a path exists");
    // (The person may wander off afterwards: take the closest approach.)
    let (gx, gz) = column_xz([20, 12]);
    let mut best = f32::MAX;
    for _ in 0..400 {
        tick(&mut region);
        let (x, z) = person_xz(&region);
        best = best.min(((x - gx).powi(2) + (z - gz).powi(2)).sqrt());
    }
    let reach = game::WAYPOINT_REACH_M * game::movement::UNITS_PER_METER;
    assert!(best <= reach + 1.0, "reached the goal, closest {best}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn person_wanders_near_spawn() {
    let mut region = floored_region();
    let start = column_xz([30, 30]);
    region.with_data(|d| {
        d.spawn_person_safe(P, start).unwrap();
    });
    let limit = (game::people::WANDER_RADIUS_BLOCKS as f32 + 2.0) * common::TILE_W;
    let mut moved = false;
    for _ in 0..2000 {
        tick(&mut region);
        let (x, z) = person_xz(&region);
        let d = (x - start.0).abs().max((z - start.1).abs());
        assert!(d <= limit, "wandered {d} units from spawn");
        moved |= d > common::TILE_W;
    }
    assert!(moved, "the person wandered at least once");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walk_out_departs() {
    let mut region = floored_region();
    // A walk out runs over the halo into the neighbour.
    common::seed_floor_halo(&mut region, common::r0());
    let n = REGION_BLOCKS as i32;
    region.with_data(|d| {
        d.spawn_person_safe(P, column_xz([n - 6, 12])).unwrap();
    });
    for _ in 0..10 {
        tick(&mut region);
    }
    let mut ok = false;
    region.with_data(|d| ok = d.walk_out_safe(P, RegionCoords::new(1, 0), common::r0(), None));
    assert!(ok);
    let mut got = None;
    for _ in 0..2000 {
        tick(&mut region);
        let (deps, _) = region.take_transfers();
        if let Some((b, to)) = deps.into_iter().next() {
            got = Some((b.kind, to));
            break;
        }
    }
    let (kind, to) = got.expect("a departure");
    assert!(matches!(kind, game::EntityKind::Person { person } if person == P));
    assert_eq!(to, RegionCoords::new(1, 0));
}

#[test]
fn replayed_person_arrival_corrects_pose() {
    let mut a = flat_region();
    let mut b = flat_region();
    let mut bundle = None;
    a.with_data(|d| {
        let e = d.spawn_person_safe(P, (100.0, 100.0)).unwrap();
        bundle = Some(d.extract_entity_safe(e));
    });
    let bundle = bundle.unwrap();
    b.with_data(|d| {
        let e = d.apply_arrival(bundle.clone()).unwrap();
        d.set_body_pose_safe(e, game::Pose::from_translation(Vector::new(10.0, 30.0, 10.0)));
        assert!(d.apply_arrival(bundle.clone()).is_none(), "replay creates nothing");
        let t = d.people_here()[0].1;
        assert_eq!([t[0], t[2]], [bundle.body.pose.translation.x, bundle.body.pose.translation.z], "pose corrected");
    });
}

#[test]
fn walk_out_heads_for_the_nearest_edge_tile() {
    let mut region = floored_region();
    common::seed_floor_halo(&mut region, common::r0());
    let n = REGION_BLOCKS as i64;
    region.with_data(|d| {
        d.spawn_person_safe(P, column_xz([10, 12])).unwrap();
    });
    for _ in 0..10 {
        tick(&mut region);
    }
    let mut ok = false;
    region.with_data(|d| ok = d.walk_out_safe(P, RegionCoords::new(1, 0), common::r0(), None));
    assert!(ok);
    let e = region.data().person_entity(P).unwrap();
    let w = region.data().ecs.walker.try_get(e).clone().expect("walking");
    let last = *w.path.last().unwrap();
    // Straight across the +x seam, not toward the far region's centre.
    assert_eq!([last.x, last.z], [n, 12], "path ends at {last:?}");
    assert!(w.to_goal, "the column across the seam is in the halo");
    // Diagonal: the corner column across from the person.
    region.with_data(|d| ok = d.walk_out_safe(P, RegionCoords::new(-1, -1), common::r0(), None));
    assert!(ok);
    let w = region.data().ecs.walker.try_get(e).clone().expect("walking");
    let last = *w.path.last().unwrap();
    assert_eq!(last.to_local().0, RegionCoords::new(-1, -1), "path ends past the corner: {last:?}");
}

#[test]
fn idle_person_falls_when_the_ground_is_dug() {
    let mut region = floored_region();
    region.with_data(|d| {
        d.spawn_person_safe(P, column_xz([30, 30])).unwrap();
    });
    for _ in 0..10 {
        tick(&mut region);
    }
    let e = region.data().person_entity(P).unwrap();
    assert!(region.data().ecs.motion.try_get(e).unwrap().grounded, "standing on the floor");
    assert!(region.data().ecs.walker.try_get(e).is_none());
    let y0 = region.data().people_here()[0].1[1];
    region.with_data(|d| {
        // The floor's top tile, five tiles each way around the person.
        for x in 28..=32 {
            for z in 28..=32 {
                assert!(common::carve_tile(d, [x, common::STAND - 1, z]));
            }
        }
    });
    for _ in 0..2 * game::people::IDLE_RESETTLE_TICKS {
        tick(&mut region);
    }
    assert!(region.data().ecs.walker.try_get(e).is_none(), "still idle");
    let y = region.data().people_here()[0].1[1];
    assert!(y < y0 - common::tile_h() / 2.0, "fell from {y0} to {y}");
}

// ---- undo: every new mutator rolls back bit-exact (HashMode::Full default) ----

fn region_with_person(xz: (f32, f32)) -> (Region, game::EntityKey) {
    let mut region = floored_region();
    common::seed_floor_halo(&mut region, common::r0());
    region.with_data(|d| {
        d.spawn_person_safe(P, xz).unwrap();
    });
    for _ in 0..10 {
        tick(&mut region);
    }
    let e = region.data().person_entity(P).unwrap();
    (region, e)
}

/// Rolls back one Tick and checks the state hash is the one before it.
fn tick_and_undo(region: &mut Region) {
    let before = game::state_hash(region.data());
    region.handle_event(GameEventKind::Tick).unwrap();
    region.rollback_last_event();
    assert_eq!(before, game::state_hash(region.data()), "tick undo restores the exact state");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn wander_tick_undo_restores_hash() {
    let (mut region, e) = region_with_person(column_xz([30, 30]));
    let mut checked = false;
    for _ in 0..600 {
        let before = game::state_hash(region.data());
        region.handle_event(GameEventKind::Tick).unwrap();
        if region.data().ecs.walker.try_get(e).is_some() {
            // This tick planned the wander and steered the body.
            region.rollback_last_event();
            assert_eq!(before, game::state_hash(region.data()), "wander tick undo restores the exact state");
            assert!(region.data().ecs.walker.try_get(e).is_none());
            checked = true;
            break;
        }
        region.forget_last_event();
    }
    assert!(checked, "a wander started");
}

#[test]
fn walk_entity_undo_restores_hash() {
    let (mut region, e) = region_with_person(column_xz([10, 12]));
    let before = game::state_hash(region.data());
    // Mutator alone, in its own transaction.
    region.with_data(|d| {
        d.new_transaction();
        assert!(d.walk_entity_safe(e, goal_here([20, 12]), common::r0(), None));
        d.rollback();
    });
    assert_eq!(before, game::state_hash(region.data()), "walk_entity undo restores the exact state");
    // Then a steering tick under an active walk.
    region.with_data(|d| {
        assert!(d.walk_entity_safe(e, goal_here([20, 12]), common::r0(), None));
    });
    tick_and_undo(&mut region);
    tick(&mut region);
    tick_and_undo(&mut region);
}

#[test]
fn walk_out_undo_restores_hash() {
    let n = REGION_BLOCKS as i32;
    let (mut region, e) = region_with_person(column_xz([n - 6, 12]));
    let before = game::state_hash(region.data());
    region.with_data(|d| {
        d.new_transaction();
        assert!(d.walk_out_safe(P, RegionCoords::new(1, 0), common::r0(), None));
        d.rollback();
    });
    assert_eq!(before, game::state_hash(region.data()), "walk_out undo restores the exact state");
    assert!(region.data().ecs.walker.try_get(e).is_none());
    region.with_data(|d| {
        assert!(d.walk_out_safe(P, RegionCoords::new(1, 0), common::r0(), None));
    });
    tick_and_undo(&mut region);
}

// ---- server-authored person events ----

use game::{PersonSpawn, RegionInput, RegionRunner, SpawnAt};

fn person_runner() -> (RegionRunner, Receiver<(RegionCoords, RegionOutput)>) {
    let (out_send, out_recv) = unbounded();
    (RegionRunner::new(common::r0(), floored_region(), out_send), out_recv)
}

fn spawn_ev(person: PersonId, at: SpawnAt) -> RegionInput {
    RegionInput::Event(GameEventKind::SpawnPerson(PersonSpawn { person, at, walk_to: None }))
}

#[test]
fn spawn_person_event_spawns() {
    let (mut r, _out) = person_runner();
    r.handle_input(spawn_ev(P, SpawnAt::Column([10, 12])));
    let here = r.region_mut().data().people_here();
    assert_eq!(here.len(), 1);
    assert_eq!(here[0].0, P);
    let bv = common::TILE_W;
    assert_eq!((here[0].1[0], here[0].1[2]), (10.5 * bv, 12.5 * bv));
    r.handle_input(RegionInput::Event(GameEventKind::DespawnPerson(P)));
    assert!(r.region_mut().data().people_here().is_empty());
}

#[test]
fn spawn_near_person() {
    let (mut r, _out) = person_runner();
    r.handle_input(spawn_ev(P, SpawnAt::Column([20, 20])));
    r.handle_input(spawn_ev(PersonId(8), SpawnAt::NearPerson(P)));
    let here = r.region_mut().data().people_here();
    assert_eq!(here.len(), 2);
    let bv = common::TILE_W;
    let (a, b) = (here[0].1, here[1].1);
    let (dx, dz) = (((b[0] - a[0]) / bv).abs(), ((b[2] - a[2]) / bv).abs());
    assert!(dx <= 2.0 && dz <= 2.0 && (dx > 0.0 || dz > 0.0), "dx={dx} dz={dz}");
    // No body here: falls back to the region landing.
    r.handle_input(spawn_ev(PersonId(9), SpawnAt::NearPerson(PersonId(99))));
    assert_eq!(r.region_mut().data().people_here().len(), 3);
}

#[test]
fn spawn_at_edge_is_on_edge() {
    let (mut r, _out) = person_runner();
    r.handle_input(spawn_ev(P, SpawnAt::Edge { from: RegionCoords::new(-1, 0), along_per_mille: 500 }));
    let bv = common::TILE_W;
    let here = r.region_mut().data().people_here();
    assert!(here[0].1[0] < bv, "x block column 0, got {}", here[0].1[0]);
    let mid = REGION_BLOCKS as f32 * bv / 2.0;
    assert!((here[0].1[2] - mid).abs() < 2.0 * bv);
    r.handle_input(spawn_ev(PersonId(8), SpawnAt::Edge { from: RegionCoords::new(0, 1), along_per_mille: 0 }));
    let here = r.region_mut().data().people_here();
    let p8 = here.iter().find(|(p, _)| p.0 == 8).unwrap().1;
    assert!(p8[2] >= (REGION_BLOCKS as f32 - 1.0) * bv && p8[0] < bv);
}

#[test]
fn shutdown_reports_and_strips_people() {
    let (mut r, out) = person_runner();
    r.handle_input(spawn_ev(PersonId(2), SpawnAt::Column([10, 10])));
    r.handle_input(spawn_ev(PersonId(5), SpawnAt::Column([30, 30])));
    let expected = r.region_mut().data().people_here();
    assert!(!r.handle_input(RegionInput::Shutdown));
    let outs: Vec<RegionOutput> = out.try_iter().map(|(_, o)| o).collect();
    let parked_at = outs.iter().position(|o| matches!(o, RegionOutput::PeopleParked(_))).expect("PeopleParked");
    let stopped_at = outs.iter().position(|o| matches!(o, RegionOutput::Stopped(..))).expect("Stopped");
    assert!(parked_at < stopped_at);
    let RegionOutput::PeopleParked(list) = &outs[parked_at] else { unreachable!() };
    assert_eq!(*list, expected);
    assert_eq!(list.len(), 2);
    let RegionOutput::Stopped(blob, _, _, portable) = &outs[stopped_at] else { unreachable!() };
    assert!(portable.entities.iter().all(|b| !matches!(b.kind, game::EntityKind::Person { .. })), "no people in the stop snapshot");
    let restored = blob.to_rollback().unwrap();
    assert!(restored.people_here().is_empty());
    assert!(restored.data.ecs.kind.iter().all(|(_, k)| !matches!(k, Some(game::EntityKind::Person { .. }))));
}

#[test]
fn client_cannot_send_spawn_person() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let ev = game::GameEvent::new(
        GameEventKind::SpawnPerson(PersonSpawn { person: P, at: SpawnAt::Column([5, 5]), walk_to: None }),
        0,
        SPAWN_REGION,
    );
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 1);
    h.tick(2);
    let mut n = 0;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |region| n = region.data().people_here().len());
    assert_eq!(n, 0);
}

// ---- client convergence: a replica fed the server's events ends bit-identical ----

fn world_hash(w: &game::World) -> u32 {
    use game::StateHash;
    let mut hasher = crc32fast::Hasher::new();
    w.data(&common::r0()).state_hash(&mut hasher);
    hasher.finalize()
}

/// The server's region and a client's replica of it (joined from its snapshot, predicting
/// ticks and reconciling the server's stream, as the client does) stay bit-identical
/// through every person event: a spawn with a walk, a spawn that wanders, a walk out and
/// a despawn, with idle wander ticks in between.
///
/// Both regions get the same map extent BEFORE the stream starts. That ordering matters:
/// the client sets its extent when the `WorldMap` packet arrives, and a walk planned while
/// a replica has no extent (unbounded) gets an exit over the outer seam where the server's
/// walk stops at the edge tile — the replica would diverge until its next snapshot. The
/// server sends the `WorldMap` on connect, before any region snapshot, so in practice the
/// extent is set first.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn client_replica_converges_on_person_events() {
    let extent = Some(game::MapExtent { origin: common::r0(), width: 1, height: 1 });
    let (send, _server_updates) = crossbeam::channel::unbounded();
    let mut server_region =
        Region::new(game::Rollback::new(Some(send.clone())), Some(send), common::r0(), None);
    server_region.seed_stud_world(common::floor_seed(0.0..REGION_SIZE, 0.0..REGION_SIZE, common::FLOOR_PLATES));
    server_region.set_map_extent(extent);
    let mut server = game::World::new();
    server.load(&common::r0(), server_region);

    let (send, _client_updates) = crossbeam::channel::unbounded();
    let mut client_region = Region::new(server.get_region_data(&common::r0()), Some(send), common::r0(), Some(0));
    client_region.set_map_extent(extent);
    let mut client = game::World::new();
    client.load(&common::r0(), client_region);
    assert_eq!(world_hash(&client), world_hash(&server));

    let q = PersonId(8);
    let n = REGION_BLOCKS as i32;
    let script: Vec<(usize, GameEventKind)> = vec![
        (
            2,
            GameEventKind::SpawnPerson(PersonSpawn {
                person: P,
                at: SpawnAt::Column([10, 12]),
                walk_to: Some(goal_here([20, 12])),
            }),
        ),
        (3, GameEventKind::SpawnPerson(PersonSpawn { person: q, at: SpawnAt::Column([n as u32 / 2, n as u32 / 2]), walk_to: None })),
        // Toward the off-map +x neighbour: the walk ends on the edge tile.
        (120, GameEventKind::WalkOut(P, RegionCoords::new(1, 0))),
        (420, GameEventKind::DespawnPerson(q)),
    ];
    let mut wandered = false;
    let mut furthest_x = 0.0f32;
    for round in 0..460 {
        let mut predicted = std::collections::BTreeMap::new();
        client.progress_world_one_tick(&mut predicted);
        for (_, kind) in script.iter().filter(|(at, _)| *at == round) {
            let ev = server.handle_region_event(kind.clone(), common::r0()).unwrap();
            server.forget_last_event(&common::r0());
            client.reconcile_event(ev).unwrap();
        }
        let mut results = std::collections::BTreeMap::new();
        server.progress_world_one_tick(&mut results);
        server.forget_last_event(&common::r0());
        let tick = results.remove(&common::r0()).unwrap().unwrap();
        client.reconcile_event(tick).unwrap();
        assert_eq!(world_hash(&client), world_hash(&server), "round {round}");
        if let Some(e) = server.data(&common::r0()).person_entity(q) {
            wandered |= server.data(&common::r0()).ecs.walker.try_get(e).is_some();
        }
        if let Some((_, t)) = server.data(&common::r0()).people_here().into_iter().find(|(p, _)| *p == P) {
            furthest_x = furthest_x.max(t[0]);
        }
    }
    assert!(wandered, "the idle person wandered");
    assert!(furthest_x > (n - 2) as f32 * common::TILE_W, "walked out to the +x edge: x {furthest_x}");
    let here = server.data(&common::r0()).people_here();
    assert_eq!(here.iter().map(|(p, _)| *p).collect::<Vec<_>>(), vec![P], "on the map's edge, not over it");
    assert_eq!(client.data(&common::r0()).people_here(), here);
}

// ---- world save cuts ----

#[test]
fn cut_snapshot_restores_without_players_or_people() {
    let (mut r, out) = person_runner();
    r.handle_input(RegionInput::Event(GameEventKind::CreateClient(4)));
    r.handle_input(spawn_ev(PersonId(2), SpawnAt::Column([10, 10])));
    for _ in 0..3 {
        r.tick();
    }
    let at = r.current_tick() + 1;
    assert!(r.handle_input(RegionInput::SaveAt(game::CutId(1), at)), "SaveAt does not stop the region");
    r.tick();
    let outs: Vec<RegionOutput> = out.try_iter().map(|(_, o)| o).collect();
    let people = outs.iter().position(|o| matches!(o, RegionOutput::PeoplePositions(_))).expect("PeoplePositions");
    let snap = outs.iter().position(|o| matches!(o, RegionOutput::CutSnapshot(..))).expect("CutSnapshot");
    assert!(people < snap, "the positions come first");
    let RegionOutput::PeoplePositions(list) = &outs[people] else { unreachable!() };
    assert_eq!(list.iter().map(|(p, _)| *p).collect::<Vec<_>>(), vec![PersonId(2)]);
    let RegionOutput::CutSnapshot(id, tick, portable) = &outs[snap] else { unreachable!() };
    assert_eq!((*id, *tick, portable.meta.tick), (game::CutId(1), at, at));
    assert_eq!(portable.players.iter().map(|(c, _)| *c).collect::<Vec<_>>(), vec![4], "the player as a record");
    assert!(portable.entities.iter().all(|b| b.client.is_none() && !matches!(b.kind, game::EntityKind::Person { .. })));
    // The terrain as the save would write it from nothing: the recipes and the
    // rows of chunks that differ from them (none on this untouched ground).
    let terrain = |d: &game::Rollback| {
        let (p, _) = d.portable_snapshot(&Default::default());
        (p.stud_ground, p.stud_chunks, d.stud_index.len())
    };
    let saved = terrain(r.region_mut().data());
    assert!(saved.0.as_ref().is_some_and(|g| !g.is_empty()) && saved.2 > 0);
    let (restored, _) = game::restore_portable(common::r0(), game::StoredRegion::from((**portable).clone()));
    let data = restored.data();
    assert!(data.people_here().is_empty(), "the history respawns its people");
    assert!(data.player_entites.is_empty() && data.clients.is_empty(), "no stale sessions");
    assert_eq!(restored.current_tick(), at);
    assert!(terrain(restored.data()) == saved, "the restored region has the saved terrain");
    // A cut whose tick the region has passed is missed.
    assert!(r.handle_input(RegionInput::SaveAt(game::CutId(2), at)));
    assert!(out.try_iter().any(|(_, o)| matches!(o, RegionOutput::CutMissed(game::CutId(2)))));
}

#[test]
fn cut_carries_running_then_parked_regions_until_committed() {
    use game::RegionSpawner;
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let mut running = h.manager.running_regions();
    running.sort();
    assert!(!running.is_empty());
    let world = h.manager.spawner_mut().world_tick();
    h.manager.begin_cut((world + 2).saturating_sub(game::SAVE_CUT_LEAD_TICKS)).expect("no cut pending");
    let mut cut = None;
    for _ in 0..game::SAVE_CUT_LEAD_TICKS + 4 {
        h.tick(0);
        cut = h.manager.take_cut();
        if cut.is_some() {
            break;
        }
    }
    let cut = cut.expect("every running region answered");
    let mut answered: Vec<_> = cut.regions.iter().map(|(rc, _)| *rc).collect();
    answered.sort();
    assert_eq!(answered, running, "every running region answers");
    h.manager.cut_committed(cut.id, Vec::new());
    assert!(h.manager.uncommitted_regions().is_empty(), "committed");
    h.manager.shutdown_all();
    h.settle(0);
    assert!(h.manager.running_regions().is_empty());
    // Nothing runs: the next cut is complete at once and carries every stop
    // snapshot (each since the last commit).
    h.manager.begin_cut(0).expect("no cut pending");
    let cut = h.manager.take_cut().expect("complete at once");
    let mut parked: Vec<_> = cut.regions.iter().map(|(rc, _)| *rc).collect();
    parked.sort();
    assert_eq!(parked, running, "each parked region once");
    assert!(cut.regions.iter().all(|(_, p)| p.stud_chunks.is_empty()), "nothing changed since the committed cut");
    h.manager.cut_failed(cut.id);
    h.manager.begin_cut(0).expect("no cut pending");
    let again = h.manager.take_cut().expect("complete at once");
    assert_eq!(again.regions.len(), running.len(), "a failed cut's regions are carried again");
    h.manager.cut_committed(again.id, Vec::new());
    h.manager.begin_cut(0).expect("no cut pending");
    assert!(h.manager.take_cut().expect("complete at once").regions.is_empty(), "committed parked regions are not carried twice");
}

// ---- wandering across a seam ----

/// Rounds a person standing on the north seam gets to pick a wander goal
/// across it and walk there (a wander is due every 100–300 ticks; a bit
/// under half of the goals lie north of the seam).
const WANDER_SEAM_ROUNDS: usize = 3000;

/// A person whose wander home is on the spawn region's north seam (world
/// block row z = 0) picks goals on both sides of it: one across the seam is
/// walked to like any other — the person crosses into north and reaches it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn wander_across_seam() {
    use crate::common::rig::{north, seam_floor_columns, Rig};
    common::install_real_block_props();
    let mut rig = Rig::with_floor(None, seam_floor_columns);
    rig.join_spawn();
    let home = [10i64, 0];
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        region.with_data(|d| {
            let e = d.spawn_person_safe(P, column_xz([10, 1])).unwrap();
            d.data.ecs.wander.set_safe(e, Some(game::people::Wander { next_tick: 0, home }));
        });
    });
    let reach = game::WAYPOINT_REACH_M * game::movement::UNITS_PER_METER + 1.0;
    let mut goals: Vec<Goal> = Vec::new();
    for _ in 0..WANDER_SEAM_ROUNDS {
        rig.round();
        for rc in rig.manager.running_regions() {
            let mut seen = None;
            rig.manager.spawner_mut().with_region(rc, |region| {
                let data = region.data();
                let Some(e) = data.person_entity(P) else { return };
                let walker = data.ecs.walker.try_get(e).clone();
                let t = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation();
                seen = Some((walker.map(|w| w.goal), t.x + rc.x as f32 * REGION_SIZE, t.z + rc.z as f32 * REGION_SIZE));
            });
            let Some((goal, x, z)) = seen else { continue };
            if let Some(goal) = goal.filter(|g| g.region() == Some(north())) {
                if !goals.contains(&goal) {
                    goals.push(goal);
                }
            }
            if rc != north() {
                continue;
            }
            let bv = common::TILE_W;
            let reached = goals.iter().any(|g| {
                let (gx, gz) = ((g.column[0] as f32 + 0.5) * bv, (g.column[1] as f32 + 0.5) * bv);
                ((x - gx).powi(2) + (z - gz).powi(2)).sqrt() <= reach
            });
            if reached {
                return;
            }
        }
    }
    panic!("the person never reached a wander goal across the seam (goals north: {goals:?})");
}
