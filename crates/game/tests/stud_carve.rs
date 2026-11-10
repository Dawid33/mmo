//! `StudChunk::carve`, `fill_box`, `Rollback::carve_stud_safe` and `Order::CarveStud`.
use game::stud_catalog::STUD_PARTS;
use game::*;

fn part(n: &str) -> u16 {
    STUD_PARTS.iter().position(|p| p.number == n).unwrap() as u16
}
fn brick(n: &str, min: [u8; 3]) -> StudBrick {
    StudBrick::part_at(min, part(n), 0, 4)
}
fn cells(c: &StudChunk) -> u32 {
    c.bricks().map(|b| b.cells().count() as u32).sum()
}
fn hash(r: &Region) -> u32 {
    state_hash(r.data())
}

#[test]
fn materials_resolve() {
    assert_eq!(stud_material(4).unwrap().name, "plastic");
    assert_eq!(stud_material(MAT_STONE).unwrap().name, "stone");
    assert!(stud_material(9999).is_none());
    assert!(stud_material(NATURAL_BASE + STUD_NATURALS.len() as u16).is_none());
    // Every natural draws in a baked colour.
    for m in STUD_NATURALS {
        assert_eq!(stud_material(m.color).map(|p| p.color), Some(m.color), "{}", m.name);
    }
}

#[test]
fn fill_box_tiles_exactly_with_valid_parts() {
    for size in [[1, 1, 1], [2, 3, 2], [8, 3, 2], [7, 5, 3], [3, 4, 9], [16, 6, 16], [5, 2, 1]] {
        let mut out = Vec::new();
        fill_box([3, 4, 5], size, MAT_DIRT, 7, &mut out);
        // `StudChunk::new` rejects overlaps and out-of-chunk bricks.
        let c = StudChunk::new(out.iter().copied()).unwrap();
        assert_eq!(cells(&c), size.iter().map(|&v| v as u32).product::<u32>(), "{size:?}");
        assert!(out.iter().all(|b| matches!(b.shape, StudShape::Part { .. }) && b.material == MAT_DIRT && b.data == 7));
        let occ = c.occupancy();
        assert!(occ.get([3, 4, 5]) && occ.get([3 + size[0] as i32 - 1, 4 + size[1] as i32 - 1, 5 + size[2] as i32 - 1]));
        assert!(!occ.get([2, 4, 5]) && !occ.get([3, 4 + size[1] as i32, 5]));
    }
    // A 2 x 8 footprint is one part per course.
    let mut out = Vec::new();
    fill_box([0, 0, 0], [8, 3, 2], 4, 0, &mut out);
    assert_eq!(out.len(), 1);
}

#[test]
fn carve_splits_a_part_and_keeps_the_rest() {
    // A 2 x 8 brick along x at y 0..3; cut studs 3..5 right through it.
    let mut c = StudChunk::new([brick("3007", [4, 0, 4])]).unwrap();
    let removed = c.carve([7, 0, 0], [9, 90, 32]);
    assert_eq!(removed, vec![(4, 2 * 3 * 2)]);
    assert_eq!(cells(&c), (8 - 2) * 3 * 2);
    let occ = c.occupancy();
    assert!(occ.get([6, 0, 4]) && !occ.get([7, 1, 5]) && !occ.get([8, 2, 4]) && occ.get([9, 0, 5]));
    // The result is a valid canonical chunk.
    assert_eq!(StudChunk::new(c.bricks().copied()).unwrap(), c);
}

#[test]
fn carve_cuts_a_plate_layer_off_a_brick() {
    let mut c = StudChunk::new([brick("3001", [0, 0, 0])]).unwrap();
    assert_eq!(c.carve([0, 2, 0], [32, 3, 32]), vec![(4, 8)]);
    assert_eq!(cells(&c), 16);
    assert_eq!(c.top_plate(0, 0), Some(2));
}

#[test]
fn carve_splits_a_filler_into_fillers() {
    let mut c = StudChunk::new([StudBrick::filler([0, 0, 0], [32, 30, 32], MAT_STONE)]).unwrap();
    assert_eq!(c.carve([10, 12, 10], [12, 18, 12]), vec![(MAT_STONE, 2 * 6 * 2)]);
    assert_eq!(c.len(), 6);
    assert!(c.bricks().all(|b| matches!(b.shape, StudShape::Filler { .. })));
    assert_eq!(cells(&c), 32 * 30 * 32 - 24);
    assert_eq!(StudChunk::new(c.bricks().copied()).unwrap(), c);
}

#[test]
fn carve_removes_a_touched_slope_whole_and_skips_bedrock() {
    let slope = part("3040");
    let size = StudBrick::part_at([0; 3], slope, 0, 4).size();
    let mut c = StudChunk::new([
        StudBrick::part_at([0, 0, 0], slope, 0, 4),
        StudBrick::filler([10, 0, 10], [4, 3, 4], MAT_BEDROCK),
    ])
    .unwrap();
    let removed = c.carve([0, 0, 0], [32, 1, 32]);
    assert_eq!(removed, vec![(4, size.iter().map(|&v| v as u32).product())]);
    assert_eq!(c.len(), 1);
    assert_eq!(c.bricks().next().unwrap().material, MAT_BEDROCK);
}

#[test]
fn carve_of_nothing_changes_nothing() {
    let mut c = StudChunk::new([brick("3001", [0, 0, 0])]).unwrap();
    let before = c.clone();
    assert!(c.carve([10, 0, 10], [12, 6, 12]).is_empty());
    assert!(c.carve([0, 0, 0], [0, 6, 12]).is_empty());
    assert_eq!(c, before);
}

const A: StudChunkCoords = StudChunkCoords { x: 0, y: 0, z: 0 };
const B: StudChunkCoords = StudChunkCoords { x: 1, y: 0, z: 0 };
const UP: StudChunkCoords = StudChunkCoords { x: 0, y: 1, z: 0 };

fn seed() -> Vec<(StudChunkCoords, StudChunk)> {
    let slab = |m| StudChunk::new([StudBrick::filler([0, 0, 0], [32, 90, 32], m)]).unwrap();
    vec![(A, slab(MAT_STONE)), (B, slab(MAT_DIRT)), (UP, StudChunk::new([brick("3001", [28, 0, 0])]).unwrap())]
}
fn region() -> Region {
    // The receiver is dropped: updates are not read here.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Region::new(Rollback::new(None), Some(send), RegionCoords::new(0, 0), None);
    r.seed_studs(seed());
    r
}
fn rollback() -> Rollback {
    let mut r = Rollback::new(None);
    r.seed_stud_genesis(seed());
    r
}

#[test]
fn carve_order_spans_chunks_and_layers_and_rolls_back() {
    let mut r = region();
    let before = hash(&r);
    // A tile-sized hole (2 x 6 x 2) straddling A|B in x, plus a tall one into layer 1.
    r.handle_event(GameEventKind::Order(0, Order::CarveStud { lo: [31, 84, 0], hi: [33, 90, 2] })).unwrap();
    let a = r.data().stud_chunk_at(A).unwrap();
    let b = r.data().stud_chunk_at(B).unwrap();
    assert!(!a.occupancy().get([31, 84, 0]) && a.occupancy().get([30, 84, 0]) && a.occupancy().get([31, 83, 0]));
    assert!(!b.occupancy().get([0, 89, 1]) && b.occupancy().get([1, 89, 1]));
    r.rollback_last_event();
    assert_eq!(before, hash(&r));

    // Down through the 2 x 4 brick in layer 1 into the stone of layer 0.
    let mut r = rollback();
    let removed = r.carve_stud_safe([30, 88, 0], [32, 93, 1]);
    assert_eq!(removed, vec![(4, 2 * 3), (MAT_STONE, 2 * 2)]);
    // What is left of the brick: its 2 x 1 x 3 far row.
    let up = r.stud_chunk_at(UP).unwrap();
    assert_eq!(up.bricks().map(|b| b.cells().count()).sum::<usize>(), 2 * 3 * 4 - 6);
    assert!(!up.occupancy().get([30, 0, 0]) && up.occupancy().get([30, 0, 1]));
}

#[test]
fn carve_order_rejects_oversized_and_empty_boxes() {
    let mut r = rollback();
    assert!(r.carve_stud_safe([0, 0, 0], [33, 1, 1]).is_empty());
    assert!(r.carve_stud_safe([0, 0, 0], [1, 91, 1]).is_empty());
    assert!(r.carve_stud_safe([5, 5, 5], [5, 6, 6]).is_empty());
    assert!(r.carve_stud_safe([-10, 0, 0], [0, 1, 1]).is_empty());
    assert!(r.carve_stud_safe([0, i32::MIN, 0], [1, i32::MIN + 5, 1]).is_empty());
    assert_eq!(state_hash(&r), state_hash(&rollback()));
}
