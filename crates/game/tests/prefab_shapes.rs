//! The prefab shape catalogue: occupancy, rotation, mirroring, canonical chisels.

use game::prefab::{turn_voxel, turn_xz, Facing, Half, Shape, Sides, StairCorner};
use game::{BlockId, ChiselData, BLOCK_VOXELS};

const FACINGS: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];
const HALVES: [Half; 2] = [Half::Bottom, Half::Top];
const CORNERS: [StairCorner; 5] = [
    StairCorner::Straight,
    StairCorner::InnerLeft,
    StairCorner::InnerRight,
    StairCorner::OuterLeft,
    StairCorner::OuterRight,
];

fn all_sides() -> Vec<Sides> {
    (0..16u8)
        .map(|m| Sides { north: m & 1 != 0, east: m & 2 != 0, south: m & 4 != 0, west: m & 8 != 0 })
        .collect()
}

fn all_shapes() -> Vec<Shape> {
    let mut v = Vec::new();
    for h in HALVES {
        v.push(Shape::Slab { half: h });
        for f in FACINGS {
            for c in CORNERS {
                v.push(Shape::Stairs { facing: f, half: h, corner: c });
            }
            for open in [false, true] {
                v.push(Shape::Trapdoor { facing: f, half: h, open });
            }
        }
    }
    for s in all_sides() {
        for post in [false, true] {
            v.push(Shape::Wall { sides: s, post });
        }
        v.push(Shape::Fence { sides: s });
        v.push(Shape::Pane { sides: s });
    }
    v.push(Shape::Carpet);
    for layers in 1..=8 {
        v.push(Shape::Layer { layers });
    }
    v
}

fn count(s: Shape) -> usize {
    let n = BLOCK_VOXELS;
    let mut c = 0;
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                c += s.filled(x, y, z) as usize;
            }
        }
    }
    c
}

#[test]
fn voxel_counts() {
    assert_eq!(count(Shape::Slab { half: Half::Bottom }), 2048);
    assert_eq!(count(Shape::Slab { half: Half::Top }), 2048);
    for h in HALVES {
        for f in FACINGS {
            for c in CORNERS {
                let want = match c {
                    StairCorner::Straight => 3072,
                    StairCorner::OuterLeft | StairCorner::OuterRight => 2560,
                    StairCorner::InnerLeft | StairCorner::InnerRight => 3584,
                };
                assert_eq!(count(Shape::Stairs { facing: f, half: h, corner: c }), want, "{f:?} {h:?} {c:?}");
            }
            for open in [false, true] {
                assert_eq!(count(Shape::Trapdoor { facing: f, half: h, open }), 768);
            }
        }
    }
    assert_eq!(count(Shape::Carpet), 256);
    for n in 1..=8u8 {
        assert_eq!(count(Shape::Layer { layers: n }), 512 * n as usize);
    }
    assert_eq!(count(Shape::Fence { sides: Sides::default() }), 256);
    assert_eq!(count(Shape::Pane { sides: Sides::default() }), 64);
    assert_eq!(count(Shape::Wall { sides: Sides::default(), post: true }), 1024);
    assert_eq!(count(Shape::Wall { sides: Sides::default(), post: false }), 0);
}

#[test]
fn stairs_facing_north_bottom_fills_the_low_z_top_half() {
    let s = Shape::Stairs { facing: Facing::North, half: Half::Bottom, corner: StairCorner::Straight };
    let last = BLOCK_VOXELS - 1;
    assert!(s.filled(0, last, 0));
    assert!(!s.filled(0, last, last));
    assert!(s.filled(0, 0, last));
}

#[test]
fn turned_matches_turn_voxel() {
    let n = BLOCK_VOXELS;
    for s in all_shapes() {
        let t = s.turned(1);
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    let [tx, ty, tz] = turn_voxel([x, y, z], 1);
                    assert_eq!(t.filled(tx, ty, tz), s.filled(x, y, z), "{s:?} at {x},{y},{z}");
                }
            }
        }
    }
}

#[test]
fn four_turns_and_two_mirrors_are_identity() {
    for s in all_shapes() {
        assert_eq!(s.turned(4), s);
        assert_eq!(s.turned(1).turned(1).turned(1).turned(1), s);
        assert_eq!(s.mirrored_x().mirrored_x(), s);
    }
}

#[test]
fn mirrored_x_matches_reflection() {
    let n = BLOCK_VOXELS;
    for s in all_shapes() {
        let m = s.mirrored_x();
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    assert_eq!(m.filled(n - 1 - x, y, z), s.filled(x, y, z), "{s:?} at {x},{y},{z}");
                }
            }
        }
    }
}

#[test]
fn north_turned_once_is_east() {
    let st = |facing| Shape::Stairs { facing, half: Half::Bottom, corner: StairCorner::OuterLeft };
    assert_eq!(st(Facing::North).turned(1), st(Facing::East));
    assert_eq!(turn_xz(0, -1, 1), (1, 0));
    assert_eq!(turn_xz(3, 5, 4), (3, 5));
}

#[test]
fn mirror_swaps_left_and_right_corners() {
    for h in HALVES {
        let st = |facing, corner| Shape::Stairs { facing, half: h, corner };
        assert_eq!(
            st(Facing::North, StairCorner::OuterLeft).mirrored_x(),
            st(Facing::North, StairCorner::OuterRight)
        );
        assert_eq!(
            st(Facing::West, StairCorner::InnerRight).mirrored_x(),
            st(Facing::East, StairCorner::InnerLeft)
        );
    }
}

#[test]
fn chisel_is_canonical() {
    let m = BlockId(7);
    for s in all_shapes() {
        assert_eq!(s.chisel(m), ChiselData::from_fn(vec![m], |x, y, z| s.filled(x, y, z).then_some(0)));
    }
    let n = BLOCK_VOXELS;
    assert_eq!(
        Shape::Slab { half: Half::Bottom }.chisel(m),
        ChiselData::cuboid(m, [0, 0, 0], [n, n / 2, n])
    );
}

#[test]
fn layer_validity() {
    assert!(!Shape::Layer { layers: 0 }.is_valid());
    assert!(!Shape::Layer { layers: 9 }.is_valid());
    for s in all_shapes() {
        assert!(s.is_valid(), "{s:?}");
    }
}
