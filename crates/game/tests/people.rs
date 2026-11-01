use crate::common;
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords,
    InlineLodBuilder, InlineSpawner, LodBuilder, RegionCoords, RegionOutput,
    ServerEvent, ServerPacket, SubChunk, WorldManager, FLIP_HYSTERESIS, REGION_SIZE, SPAWN_REGION,
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
                    game::Pose::from_translation(game::rapier3d::math::Vector::new(x, 26.0, z)),
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


use game::history::PersonId;
use game::rapier3d::math::Vector;

fn flat_region() -> game::Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    let chunks = common::floor_chunks(0.0..2.0 * common::CHUNK_EDGE, 0.0..2.0 * common::CHUNK_EDGE, 16);
    let mut region = game::Region::new(game::Rollback::new(Some(send.clone())), Some(send), common::r0(), None);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
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
    let chunks = common::floor_chunks(0.0..2.0 * common::CHUNK_EDGE, 0.0..2.0 * common::CHUNK_EDGE, 16);
    let mut r = game::Rollback::new(Some(send));
    r.seed_terrain_genesis(chunks);
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
        let bundle = d.extract_entity_safe(e, RegionCoords::new(1, 0));
        // Same person arrives twice under different identities.
        let mut dup = bundle.clone();
        d.apply_arrival(bundle);
        dup.source_key = game::EntityKey::default();
        assert!(d.apply_arrival(dup).is_none(), "duplicate person ignored");
        assert_eq!(d.people_here().len(), 1);
        assert_eq!(d.ecs.kind.iter().filter(|(_, k)| matches!(k, Some(game::EntityKind::Person { .. }))).count(), 1);
    });
}

// ---- walking ----

use game::{Goal, GameEventKind, Region, BLOCK_VOXELS, REGION_BLOCKS};

fn floored_region() -> Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(game::Rollback::new(Some(send.clone())), Some(send), common::r0(), None);
    let size = REGION_SIZE;
    region.with_data(|r| r.seed_terrain_genesis(common::floor_chunks(0.0..size, 0.0..size, 16)));
    region
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

fn column_xz(c: [i32; 2]) -> (f32, f32) {
    let bv = BLOCK_VOXELS as f32;
    ((c[0] as f32 + 0.5) * bv, (c[1] as f32 + 0.5) * bv)
}

fn person_xz(region: &Region) -> (f32, f32) {
    let here = region.data().people_here();
    (here[0].1[0], here[0].1[2])
}

fn goal_here(c: [i32; 2]) -> Goal {
    Goal { region: common::r0(), column: c, y: None }
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
    let limit = (game::people::WANDER_RADIUS_BLOCKS as f32 + 2.0) * BLOCK_VOXELS as f32;
    let mut moved = false;
    for _ in 0..2000 {
        tick(&mut region);
        let (x, z) = person_xz(&region);
        let d = (x - start.0).abs().max((z - start.1).abs());
        assert!(d <= limit, "wandered {d} units from spawn");
        moved |= d > BLOCK_VOXELS as f32;
    }
    assert!(moved, "the person wandered at least once");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walk_out_departs() {
    let mut region = floored_region();
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
        bundle = Some(d.extract_entity_safe(e, RegionCoords::new(1, 0)));
    });
    let bundle = bundle.unwrap();
    b.with_data(|d| {
        let e = d.apply_arrival(bundle.clone()).unwrap();
        d.set_body_pose_safe(e, game::Pose::from_translation(Vector::new(10.0, 30.0, 10.0)));
        assert!(d.apply_arrival(bundle.clone()).is_none(), "replay creates nothing");
        let t = d.people_here()[0].1;
        assert_eq!([t[0], t[2]], [bundle.isometry.translation.x, bundle.isometry.translation.z], "pose corrected");
    });
}

// ---- undo: every new mutator rolls back bit-exact (HashMode::Full default) ----

fn region_with_person(xz: (f32, f32)) -> (Region, game::EntityKey) {
    let mut region = floored_region();
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
    let bv = BLOCK_VOXELS as f32;
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
    let bv = BLOCK_VOXELS as f32;
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
    let bv = BLOCK_VOXELS as f32;
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
    let RegionOutput::Stopped(blob, _) = &outs[stopped_at] else { unreachable!() };
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
