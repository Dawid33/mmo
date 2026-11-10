//! Stud chunk data: units, rotation, validation, canonical form.
use game::stud_catalog::STUD_PARTS;
use game::*;
use rapier3d::math::Vector;
use state_hash::StateHash;
use std::hash::Hasher;

fn part(n: &str) -> u16 {
    STUD_PARTS.iter().position(|p| p.number == n).unwrap() as u16
}
fn brick(n: &str, min: [u8; 3], rot: u8) -> StudBrick {
    StudBrick { min, shape: StudShape::Part { part: part(n), rot }, material: 4, data: 0 }
}
fn hash<T: StateHash>(v: &T) -> u32 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finish() as u32
}

#[test]
fn units_are_exact() {
    assert_eq!(plate_units(90), 288.0);
    assert_eq!(plate_units(15), 48.0);
    assert_eq!(plate_units(5), 16.0);
}

#[test]
fn coords_round_trip_origin() {
    let c = StudChunkCoords { x: 3, y: -2, z: 7 };
    assert_eq!(StudChunkCoords::from_origin(c.origin()), c);
    assert_eq!(c.origin(), Vector::new(768.0, -576.0, 1792.0));
}

#[test]
fn rotation_four_times_is_identity() {
    let size = STUD_PARTS[part("3001") as usize].size;
    for rot in 0..4u8 {
        let mut seen = std::collections::BTreeSet::new();
        for x in 0..size[0] {
            for y in 0..size[1] {
                for z in 0..size[2] {
                    let c = [x, y, z];
                    let r = rotate_cell(rot, c, size);
                    let rsz = brick("3001", [0; 3], rot).size();
                    assert!(r[0] < rsz[0] && r[1] < rsz[1] && r[2] < rsz[2]);
                    assert!(seen.insert(r), "rotation must be a bijection");
                }
            }
        }
    }
    // rot 1 four times via the composed (rotated-size) frame is identity.
    let c = [1, 2, 3];
    let sz = [2, 3, 4];
    let mut cur = c;
    let mut cs = sz;
    for _ in 0..4 {
        cur = rotate_cell(1, cur, cs);
        cs = [cs[2], cs[1], cs[0]];
    }
    assert_eq!(cur, c);
}

#[test]
fn rotated_2x4_footprint_swaps() {
    assert_eq!(brick("3001", [0, 0, 0], 1).size(), [2, 3, 4]);
}

#[test]
fn brick_2x4_occupies_24_cells_and_8_studs() {
    for rot in 0..4 {
        let b = brick("3001", [0, 0, 0], rot);
        assert_eq!(b.cells().count(), 24);
        let studs = b.stud_cells();
        assert_eq!(studs.len(), 8);
        assert!(studs.iter().all(|c| c[1] == 3));
        let sz = b.size();
        assert!(studs.iter().all(|c| c[0] < sz[0] && c[2] < sz[2]));
    }
}

#[test]
fn place_refuses_overlap() {
    let mut c = StudChunk::default();
    c.place(brick("3001", [0, 0, 0], 0)).unwrap();
    let before = c.clone();
    assert_eq!(c.place(brick("3003", [1, 1, 0], 0)), Err(StudError::Overlap));
    assert_eq!(c, before);
}

#[test]
fn place_refuses_out_of_chunk() {
    let mut c = StudChunk::default();
    assert_eq!(c.place(brick("3001", [30, 0, 0], 0)), Err(StudError::OutOfChunk));
    assert_eq!(c.place(brick("3005", [0, 88, 0], 0)), Err(StudError::OutOfChunk));
    assert!(c.is_empty());
}

#[test]
fn place_refuses_unknown_part_colour_rotation() {
    let mut c = StudChunk::default();
    let mut b = brick("3001", [0; 3], 0);
    b.shape = StudShape::Part { part: 9999, rot: 0 };
    assert_eq!(c.place(b), Err(StudError::UnknownPart));
    let mut b = brick("3001", [0; 3], 0);
    b.material = 9999;
    assert_eq!(c.place(b), Err(StudError::UnknownMaterial));
    assert_eq!(c.place(brick("3001", [0; 3], 4)), Err(StudError::BadRotation));
    let f = StudBrick { min: [0; 3], shape: StudShape::Filler { size: [1, 0, 1] }, material: 4, data: 0 };
    assert_eq!(c.place(f), Err(StudError::EmptyFiller));
}

#[test]
fn canonical_form_is_order_independent() {
    let a = brick("3001", [0, 0, 0], 0);
    let b = brick("3003", [10, 0, 5], 1);
    let c1 = StudChunk::new([a, b]).unwrap();
    let c2 = StudChunk::new([b, a]).unwrap();
    assert_eq!(c1, c2);
    assert_eq!(bincode::serialize(&c1).unwrap(), bincode::serialize(&c2).unwrap());
    assert_eq!(hash(&c1), hash(&c2));
}

#[test]
fn remove_at_any_cell_removes_whole_brick() {
    let mut c = StudChunk::default();
    let b = brick("3001", [0, 0, 0], 0);
    c.place(b).unwrap();
    assert_eq!(c.remove_at([3, 2, 1]), Some(b));
    assert!(c.occupancy().is_empty());
    assert_eq!(c.remove_at([3, 2, 1]), None);
}

#[test]
fn top_plate_reads_highest_brick() {
    let filler = StudBrick { min: [0; 3], shape: StudShape::Filler { size: [32, 10, 32] }, material: 4, data: 0 };
    let c = StudChunk::new([filler, brick("3001", [0, 10, 0], 0)]).unwrap();
    assert_eq!(c.top_plate(0, 0), Some(13));
    assert_eq!(c.top_plate(31, 31), Some(10));
    assert_eq!(StudChunk::default().top_plate(0, 0), None);
}

fn chunk_of(bricks: Vec<StudBrick>) -> StudChunk {
    StudChunk::new(bricks).unwrap()
}

#[test]
fn ray_hits_top_of_brick() {
    let c0 = StudChunkCoords { x: 0, y: 0, z: 0 };
    let chunk = chunk_of(vec![brick("3001", [0, 0, 0], 0)]);
    let hit = raycast_studs(|c| (c == c0).then_some(&chunk), Vector::new(4.0, 100.0, 4.0), Vector::new(0.0, -1.0, 0.0), 500.0)
        .unwrap();
    assert_eq!((hit.chunk, hit.cell, hit.normal), (c0, [0, 2, 0], [0, 1, 0]));
}

#[test]
fn ray_hits_side_across_chunks() {
    let c1 = StudChunkCoords { x: 1, y: 0, z: 0 };
    let chunk = chunk_of(vec![brick("3001", [0, 0, 0], 0)]);
    let hit = raycast_studs(|c| (c == c1).then_some(&chunk), Vector::new(100.0, 4.0, 4.0), Vector::new(1.0, 0.0, 0.0), 500.0)
        .unwrap();
    assert_eq!((hit.chunk, hit.normal), (c1, [-1, 0, 0]));
}

#[test]
fn ray_misses_beyond_reach() {
    let c0 = StudChunkCoords { x: 0, y: 0, z: 0 };
    let chunk = chunk_of(vec![brick("3001", [0, 0, 0], 0)]);
    let r = raycast_studs(|c| (c == c0).then_some(&chunk), Vector::new(4.0, 100.0, 4.0), Vector::new(0.0, -1.0, 0.0), 50.0);
    assert!(r.is_none());
}

#[test]
fn placement_on_top_lands_on_next_plate() {
    let c0 = StudChunkCoords { x: 0, y: 0, z: 0 };
    let chunk = chunk_of(vec![brick("3001", [0, 0, 0], 0)]);
    let hit = raycast_studs(|c| (c == c0).then_some(&chunk), Vector::new(4.0, 100.0, 4.0), Vector::new(0.0, -1.0, 0.0), 500.0)
        .unwrap();
    let (coords, b) = placement_for(&hit, part("3001"), 0, 4).unwrap();
    assert_eq!((coords, b.min[1]), (c0, 3));
}

#[test]
fn placement_off_chunk_edge_wraps_to_neighbour() {
    let hit = StudHit { chunk: StudChunkCoords { x: 0, y: 0, z: 0 }, cell: [31, 5, 5], normal: [1, 0, 0], t: 0.0 };
    let (coords, b) = placement_for(&hit, part("3005"), 0, 4).unwrap();
    assert_eq!((coords.x, b.min), (1, [0, 5, 5]));
    let hit = StudHit { chunk: StudChunkCoords { x: 0, y: 0, z: 0 }, cell: [0, 5, 5], normal: [-1, 0, 0], t: 0.0 };
    assert!(placement_for(&hit, part("3005"), 0, 4).is_none());
}

#[test]
fn ray_hits_top_face_exactly_at_layer_boundary() {
    // A brick at the bottom of layer 1; the ray comes down onto its top
    // (y = 288 + 9.6), which must land in layer 1 cell 2, not in layer 0.
    let c1 = StudChunkCoords { x: 0, y: 1, z: 0 };
    let chunk = chunk_of(vec![brick("3001", [0, 0, 0], 0)]);
    let hit = raycast_studs(|c| (c == c1).then_some(&chunk), Vector::new(4.0, 400.0, 4.0), Vector::new(0.0, -1.0, 0.0), 500.0)
        .unwrap();
    assert_eq!((hit.chunk, hit.cell, hit.normal), (c1, [0, 2, 0], [0, 1, 0]));
    // Ray starting exactly on a 15-plate (48 unit) boundary inside layer 0 reads plate 15.
    let c0 = StudChunkCoords { x: 0, y: 0, z: 0 };
    let chunk = chunk_of(vec![brick("3001", [0, 15, 0], 0)]);
    let hit = raycast_studs(|c| (c == c0).then_some(&chunk), Vector::new(4.0, 48.0, 4.0), Vector::new(0.0, 1.0, 0.0), 10.0).unwrap();
    assert_eq!((hit.cell, hit.t), ([0, 15, 0], 0.0));
}

#[test]
fn placement_struck_from_below_hangs_down() {
    let c0 = StudChunkCoords { x: 0, y: 0, z: 0 };
    let hit = StudHit { chunk: c0, cell: [5, 10, 5], normal: [0, -1, 0], t: 0.0 };
    let (coords, b) = placement_for(&hit, part("3001"), 0, 4).unwrap();
    // target cell y 9; brick is 3 plates tall, so it spans 7..=9.
    assert_eq!((coords, b.min[1]), (c0, 7));
}
