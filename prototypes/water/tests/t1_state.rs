use water_proto::body::Body;
use water_proto::grid::{Dims, Dir, horizontal_order};
use water_proto::state::World;
use water_proto::{FULL, Mode};

#[test]
fn index_roundtrip_and_closed_box() {
    let d = Dims::cube(8);
    for i in 0..d.len() as u32 {
        let (x, y, z) = d.xyz(i);
        assert_eq!(d.idx(x, y, z), i);
        assert_eq!(d.y_of(i), y);
    }
    let corner = d.idx(0, 0, 0);
    assert_eq!(d.step(corner, Dir::Down), None);
    assert_eq!(d.step(corner, Dir::NegX), None);
    assert_eq!(d.step(corner, Dir::NegZ), None);
    assert_eq!(d.step(corner, Dir::Up), Some(d.idx(0, 1, 0)));
    assert_eq!(d.step(d.idx(7, 7, 7), Dir::PosX), None);
}

#[test]
fn horizontal_order_rotates() {
    assert_ne!(horizontal_order(0), horizontal_order(1));
    assert_eq!(horizontal_order(0), horizontal_order(4));
}

#[test]
fn undo_restores_every_field() {
    let mut w = World::new(Dims::cube(8), Mode::Bodies);
    let h0 = w.hash();
    w.begin_log();
    w.set_step(3);
    w.set_water(5, 100);
    w.set_owner(6, 3);
    w.set_solid(7, true);
    w.set_active(8, true);
    w.touch(9);
    w.add_poured(7);
    let id = w.alloc_id();
    let mut b = Body::empty(id, w.dims);
    b.volume = 42;
    w.insert_body(b);
    w.body_mut(id).volume = 43;
    let log = w.end_log();
    assert!(!log.is_empty());
    assert_ne!(w.hash(), h0);
    w.undo(log);
    assert_eq!(w.hash(), h0);
    assert!(w.bodies.is_empty());
    assert_eq!(w.next_id, 1);
    assert_eq!(w.step, 0);
}

#[test]
fn body_level_math() {
    let mut b = Body::empty(1, Dims::cube(8));
    b.top = 2;
    b.area[1] = 4;
    b.area[2] = 4;
    b.below = 4;
    b.cells = 8;
    b.volume = 4 * FULL as u64 + 4 * 1000;
    assert_eq!(b.capacity(), 8 * FULL as u64);
    assert_eq!(b.frac(), 4000);
    assert_eq!(b.room(), 4 * FULL as u64 - 4000);
    assert_eq!(b.virtual_at(1), FULL);
    assert_eq!(b.virtual_at(2), 1000);
    assert_eq!(b.virtual_at(3), 0);
    assert_eq!(b.level(), 2 * FULL as u64 + 1000);
}

#[test]
fn hash_ignores_stats() {
    let mut w = World::new(Dims::cube(8), Mode::CellOnly);
    let h = w.hash();
    w.stats.transfers = 9;
    assert_eq!(w.hash(), h);
}
