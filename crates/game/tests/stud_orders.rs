//! `Order::PlaceStud` / `Order::RemoveStud` through a real `Region`.
use crossbeam::channel::Receiver;
use game::stud_catalog::STUD_PARTS;
use game::*;

fn brick(n: &str, min: [u8; 3]) -> StudBrick {
    let part = STUD_PARTS.iter().position(|p| p.number == n).unwrap() as u16;
    StudBrick { min, shape: StudShape::Part { part, rot: 0 }, material: 4, data: 0 }
}

const C: StudChunkCoords = StudChunkCoords { x: 1, y: 0, z: 2 };
const EMPTY: StudChunkCoords = StudChunkCoords { x: 2, y: 0, z: 2 };

fn region() -> (Region, Receiver<GameDataUpdate>) {
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Region::new(Rollback::new(None), Some(send), RegionCoords::new(0, 0), None);
    r.seed_studs(vec![(C, StudChunk::new([brick("3001", [10, 0, 10])]).unwrap())]);
    (r, recv)
}

fn hash(r: &Region) -> u32 {
    state_hash(r.data())
}

fn order(r: &mut Region, o: Order) {
    r.handle_event(GameEventKind::Order(0, o)).unwrap();
}

fn is_stud_update(k: &GameDataUpdateKind) -> bool {
    matches!(k, GameDataUpdateKind::AddStudChunk(..) | GameDataUpdateKind::SetStudChunk(..))
}

/// Stud updates only: other per-event updates are not what the no-op tests probe.
fn drain(rx: &Receiver<GameDataUpdate>) -> Vec<GameDataUpdateKind> {
    rx.try_iter()
        .map(|u| u.update_kind)
        .filter(is_stud_update)
        .collect()
}

fn place(chunk: StudChunkCoords, b: StudBrick) -> Order {
    Order::PlaceStud { chunk, brick: b }
}

#[test]
fn place_order_round_trip() {
    let (mut r, rx) = region();
    let before = hash(&r);
    drain(&rx);
    order(&mut r, place(C, brick("3001", [0, 0, 0])));
    assert!(r.data().stud_chunk_at(C).unwrap().brick_at([0, 0, 0]).is_some());
    assert!(drain(&rx).iter().any(|k| matches!(k, GameDataUpdateKind::SetStudChunk(..))));
    assert_ne!(before, hash(&r));
    r.rollback_last_event();
    assert!(r.data().stud_chunk_at(C).unwrap().brick_at([0, 0, 0]).is_none());
    assert_eq!(before, hash(&r));
}

#[test]
fn remove_order_round_trip() {
    let (mut r, _rx) = region();
    let before = hash(&r);
    order(&mut r, Order::RemoveStud { chunk: C, cell: [11, 0, 11] });
    assert!(r.data().stud_chunk_at(C).unwrap().is_empty());
    assert_ne!(before, hash(&r));
    r.rollback_last_event();
    assert_eq!(r.data().stud_chunk_at(C).unwrap().len(), 1);
    assert_eq!(before, hash(&r));
}

/// Handling an event bumps the (hashed) event id, so "touched nothing" is
/// measured against a reference event that is a no-op by construction (a
/// removal from a chunk that does not exist): equal hashes, no stud update.
fn assert_noop(bad: &[Order]) {
    let reference = Order::RemoveStud { chunk: EMPTY, cell: [0, 0, 0] };
    let (mut want, _w) = region();
    let (mut got, rx) = region();
    drain(&rx);
    for &o in bad {
        order(&mut want, reference);
        order(&mut got, o);
    }
    assert_eq!(hash(&want), hash(&got), "state untouched");
    assert!(drain(&rx).is_empty(), "no update emitted");
}

#[test]
fn place_order_across_chunk_edge_is_noop() {
    assert_noop(&[
        place(C, brick("3001", [30, 0, 0])),
        // Overlap, and removals of nothing.
        place(C, brick("3001", [10, 0, 10])),
        Order::RemoveStud { chunk: C, cell: [0, 0, 0] },
        Order::RemoveStud { chunk: EMPTY, cell: [0, 0, 0] },
        // Filler is worldgen-only terrain: clients may place catalog parts only.
        place(C, StudBrick { min: [0, 0, 0], shape: StudShape::Filler { size: [1, 1, 1] }, material: 4, data: 0 }),
        place(EMPTY, StudBrick { min: [0, 0, 0], shape: StudShape::Filler { size: [2, 3, 2] }, material: 4, data: 0 }),
    ]);
}

#[test]
fn place_order_into_missing_chunk_creates_it() {
    let (mut r, rx) = region();
    let before = hash(&r);
    drain(&rx);
    order(&mut r, place(EMPTY, brick("3001", [0, 0, 0])));
    assert_eq!(r.data().stud_chunk_at(EMPTY).unwrap().len(), 1);
    assert!(drain(&rx).iter().any(|k| matches!(k, GameDataUpdateKind::AddStudChunk(..))));
    r.rollback_last_event();
    assert!(r.data().stud_chunk_at(EMPTY).is_none());
    assert_eq!(before, hash(&r));
}

#[test]
fn place_order_outside_region_is_noop() {
    assert_noop(&[
        place(StudChunkCoords { x: REGION_CHUNKS, y: 0, z: 0 }, brick("3001", [0, 0, 0])),
        place(StudChunkCoords { x: 0, y: 0, z: REGION_CHUNKS }, brick("3001", [0, 0, 0])),
        place(StudChunkCoords { x: 0, y: STUD_LAYER_LIMIT + 1, z: 0 }, brick("3001", [0, 0, 0])),
        place(StudChunkCoords { x: 0, y: -STUD_LAYER_LIMIT - 1, z: 0 }, brick("3001", [0, 0, 0])),
        place(StudChunkCoords { x: 0, y: i32::MAX, z: 0 }, brick("3001", [0, 0, 0])),
        place(StudChunkCoords { x: 0, y: i32::MIN, z: 0 }, brick("3001", [0, 0, 0])),
    ]);
}

#[test]
fn replayed_orders_match_server() {
    let script = [
        place(C, brick("3001", [0, 0, 0])),
        place(EMPTY, brick("3001", [4, 0, 4])),
        Order::RemoveStud { chunk: C, cell: [10, 0, 10] },
        place(C, brick("3001", [10, 0, 10])),
    ];
    let (mut server, _s) = region();
    for o in script {
        order(&mut server, o);
        server.commit_server_event();
    }
    // The predicting copy applies them, rolls everything back, replays.
    let (mut client, _c) = region();
    let base = hash(&client);
    for o in script {
        order(&mut client, o);
    }
    for _ in script {
        client.rollback_last_event();
    }
    assert_eq!(base, hash(&client), "full rollback restores the base state");
    for o in script {
        order(&mut client, o);
        client.forget_last_event();
    }
    assert_eq!(hash(&server), hash(&client));
}

/// The reference no-op used by `assert_noop` really is one: against a region
/// that handled a different no-op (a removal in a present chunk, no brick
/// there) it differs in nothing, and it emits no stud update or entity.
#[test]
fn reference_noop_changes_only_the_event_id() {
    let (mut a, rx) = region();
    let (mut b, _b) = region();
    let entities = a.data().stud_key_at(C).is_some() && a.data().stud_key_at(EMPTY).is_none();
    assert!(entities);
    drain(&rx);
    order(&mut a, Order::RemoveStud { chunk: EMPTY, cell: [0, 0, 0] });
    order(&mut b, Order::RemoveStud { chunk: C, cell: [0, 0, 0] });
    assert_eq!(hash(&a), hash(&b));
    assert!(drain(&rx).is_empty());
    assert!(a.data().stud_key_at(EMPTY).is_none());
}

/// A second client joins an EDITED stud region from a snapshot: the
/// serialized `Rollback` (the `ServerPacket::Region` / parking payload, with
/// the stud colliders' `Voxels` patched in place by the edits) must decode to
/// the same state hash, and keep matching the server under a further order.
#[test]
fn edited_stud_region_snapshot_round_trips() {
    let script = [
        place(C, brick("3001", [0, 0, 0])),
        place(C, brick("3001", [4, 3, 4])),
        place(EMPTY, brick("3001", [4, 0, 4])),
        Order::RemoveStud { chunk: C, cell: [10, 0, 10] },
        place(C, brick("3001", [20, 0, 20])),
    ];
    let (mut server, _s) = region();
    for o in script {
        order(&mut server, o);
        server.commit_server_event();
    }
    let snapshot = SerializedRegion::from_rollback(server.data());
    let decoded = snapshot.to_rollback().expect("snapshot decodes");
    assert_eq!(state_hash(server.data()), state_hash(&decoded), "decoded snapshot hashes equal");

    let mut joined = Region::new(decoded, None, RegionCoords::new(0, 0), None);
    let next = place(C, brick("3001", [10, 0, 10]));
    order(&mut server, next);
    order(&mut joined, next);
    assert_eq!(hash(&server), hash(&joined), "the joined copy keeps matching after a further edit");
}
