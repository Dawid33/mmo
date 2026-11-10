//! The tip spin that topples a felled part toward its lean
//! (`game::trees::tip_angvel`): what a chopped stud tree's falling body starts
//! with (`stud_trees.rs`). Which bricks a cut brings down, the order carried
//! out and the logs it leaves are `stud_gameplay::` (felling, a chopped tree
//! falling, a mined trunk) and `handoff_schedule::stud_tree_*` (across a seam).
use game::rapier3d::math::Vector;
use game::{tip_angvel, TilePos, REGION_BLOCKS, TIP_ANGVEL};

const TREE: u32 = 5;

/// The horizontal direction a spin `w` tips a part's top toward.
fn tip_direction(w: Vector) -> Vector {
    w.cross(Vector::Y) / TIP_ANGVEL
}

/// A 6-high part straight above `cut`, leaning `lean` tiles in x.
fn part(cut: TilePos, lean: i32) -> Vec<TilePos> {
    (1..=6).map(|dy| [cut[0] + lean, cut[1] + dy, cut[2]]).collect()
}

#[test]
fn tip_follows_the_lean() {
    let mid = REGION_BLOCKS as i32 / 2;
    let cut = [mid, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, -1), 0));
    assert!((d - Vector::new(-1.0, 0.0, 0.0)).length() < 1e-5, "{d:?}");
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, 1), 0));
    assert!((d - Vector::new(1.0, 0.0, 0.0)).length() < 1e-5, "{d:?}");
    // No lean: a rolled direction, the same on every replay.
    let a = tip_angvel(TREE, cut, &part(cut, 0), 7);
    assert_eq!(a, tip_angvel(TREE, cut, &part(cut, 0), 7));
    assert!((a.length() - TIP_ANGVEL).abs() < 1e-5 && a.y == 0.0);
}

/// Bodies cross seams (Task 7.5), so the tip does not turn inward at a
/// region edge: a part leaning out over an edge falls out over it.
#[test]
fn tip_near_edge_follows_the_lean_across_the_seam() {
    let mid = REGION_BLOCKS as i32 / 2;
    let cut = [2, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, -1), 0));
    assert!((d - Vector::new(-1.0, 0.0, 0.0)).length() < 1e-5, "over the −x seam: {d:?}");
    let cut = [REGION_BLOCKS as i32 - 3, 1, mid];
    let d = tip_direction(tip_angvel(TREE, cut, &part(cut, 1), 0));
    assert!((d - Vector::new(1.0, 0.0, 0.0)).length() < 1e-5, "over the +x seam: {d:?}");
}
