//! Stud chunk collider derivation: fine/coarse cells, stud caps, hulls, shape order.
use game::stud_catalog::STUD_PARTS;
use game::*;
use parry3d::query::Ray;
use rapier3d::math::{IVector, Vector};

fn part(n: &str) -> u16 {
    STUD_PARTS.iter().position(|p| p.number == n).unwrap() as u16
}
fn brick(n: &str, min: [u8; 3], rot: u8) -> StudBrick {
    StudBrick { min, shape: StudShape::Part { part: part(n), rot }, material: 4, data: 0 }
}
fn filler(size: [u8; 3], min: [u8; 3]) -> StudBrick {
    StudBrick { min, shape: StudShape::Filler { size }, material: 4, data: 0 }
}
fn chunk(b: impl IntoIterator<Item = StudBrick>) -> StudChunk {
    StudChunk::new(b).unwrap()
}

#[test]
fn single_brick_fine_cells_and_caps() {
    let cells = stud_collider_cells(&chunk([brick("3001", [0, 0, 0], 0)]));
    assert!(cells.coarse.is_empty());
    assert_eq!(cells.fine.len(), 24 * 2 + 8);
    assert_eq!(cells.fine.iter().filter(|k| k.y == 6).count(), 8);
    assert!(cells.fine.iter().all(|k| k.y <= 6));
}

#[test]
fn stacked_brick_covers_caps() {
    let cells = stud_collider_cells(&chunk([brick("3001", [0, 0, 0], 0), brick("3001", [0, 3, 0], 0)]));
    // y == 6 is the upper brick's lower half-cells only: the lower brick's caps are covered.
    assert_eq!(cells.fine.iter().filter(|k| k.y == 6).count(), 8);
    assert_eq!(cells.fine.iter().filter(|k| k.y == 12).count(), 8);
    assert_eq!(cells.fine.len(), 48 * 2 + 8);
}

#[test]
fn full_filler_goes_coarse() {
    let cells = stud_collider_cells(&chunk([filler([32, 15, 32], [0, 0, 0])]));
    assert_eq!(cells.coarse.len(), 64);
    assert!(cells.coarse.iter().all(|k| k.y == 0));
    assert!(cells.fine.is_empty());
}

#[test]
fn partial_coarse_cell_stays_fine() {
    let cells = stud_collider_cells(&chunk([filler([32, 14, 32], [0, 0, 0])]));
    assert!(cells.coarse.is_empty());
    assert_eq!(cells.fine.len(), 32 * 14 * 32 * 2);
}

#[test]
fn slope_is_a_hull_not_cells() {
    let c = chunk([brick("3040", [2, 6, 3], 1)]);
    let cells = stud_collider_cells(&c);
    assert!(cells.coarse.is_empty());
    assert_eq!(cells.fine.len(), 1);
    let hulls = stud_hulls(&c);
    assert_eq!(hulls.len(), 1);
    let b = c.bricks().next().unwrap();
    let [sx, sy, sz] = b.size();
    let lo = Vector::new(2.0 * 8.0, plate_units(6), 3.0 * 8.0);
    let hi = lo + Vector::new(sx as f32 * 8.0, plate_units(sy as i32), sz as f32 * 8.0);
    for p in &hulls[0] {
        assert!(p.cmpge(lo - Vector::splat(1e-3)).all() && p.cmple(hi + Vector::splat(1e-3)).all(), "{p:?}");
    }
}

#[test]
fn collider_matches_occupancy() {
    let box_parts: Vec<u16> =
        (0..STUD_PARTS.len()).filter(|&i| STUD_PARTS[i].hull.is_empty()).map(|i| i as u16).collect();
    let mut rng = oorandom::Rand64::new(7);
    for _ in 0..3 {
        let mut c = StudChunk::default();
        for _ in 0..120 {
            let p = box_parts[rng.rand_range(0..box_parts.len() as u64) as usize];
            let b = StudBrick {
                min: [rng.rand_range(0..32) as u8, rng.rand_range(0..60) as u8, rng.rand_range(0..32) as u8],
                shape: StudShape::Part { part: p, rot: rng.rand_range(0..4) as u8 },
                material: 4,
                data: 0,
            };
            let _ = c.place(b);
        }
        let shapes = stud_collider_shapes(&c);
        for sx in 0..32u8 {
            for sz in 0..32u8 {
                let ray = Ray::new(
                    Vector::new(sx as f32 * 8.0 + 4.0, 400.0, sz as f32 * 8.0 + 4.0),
                    Vector::new(0.0, -1.0, 0.0),
                );
                let hit = shapes
                    .iter()
                    .filter_map(|s| s.cast_local_ray(&ray, 1000.0, true))
                    .reduce(f32::min)
                    .map(|t| 400.0 - t);
                let want = c.top_plate(sx, sz).map(|top| {
                    let capped = c.bricks().any(|b| {
                        b.min[1] + b.size()[1] == top
                            && b.stud_cells().contains(&[sx, top, sz])
                    });
                    plate_units(top as i32) + if capped { 1.6 } else { 0.0 }
                });
                match (hit, want) {
                    (None, None) => {}
                    (Some(h), Some(w)) => assert!((h - w).abs() < 1e-3, "col {sx},{sz}: {h} vs {w}"),
                    other => panic!("col {sx},{sz}: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn shapes_order_is_coarse_fine_hulls() {
    let c = chunk([filler([32, 15, 32], [0, 0, 0]), brick("3001", [0, 15, 0], 0), brick("3040", [10, 15, 10], 0)]);
    let shapes = stud_collider_shapes(&c);
    assert_eq!(shapes.len(), 3);
    let v0 = shapes[0].as_voxels().unwrap();
    assert_eq!(v0.voxel_size(), STUD_COARSE_CELL);
    let v1 = shapes[1].as_voxels().unwrap();
    assert_eq!(v1.voxel_size(), STUD_FINE_CELL);
    assert!(shapes[2].as_convex_polyhedron().is_some());
    let _ = IVector::ZERO;
    assert!(stud_collider_shapes(&StudChunk::default()).is_empty());
}
