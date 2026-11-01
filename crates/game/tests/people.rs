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
