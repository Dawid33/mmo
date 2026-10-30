//! Felling standing trees (spec `2026-10-29-trees-investigation.md` §6): the
//! blocks a chop at one log or branch block brings down, and the spin that
//! tips them over. Pure functions over a [`TileSource`], so the sim and the
//! client's previews agree; the `Chop` order (`orders.rs`) carries them out
//! with `detach_blocks_safe`.
//!
//! A tree is its blocks with one tree id (`Block::tree_id`, 0 = not a tree):
//! logs, branches and leaves, 26-connected.

use std::collections::BTreeSet;

use rapier3d::math::Vector;

use crate::orders::{TilePos, TileSource};
use crate::slime::roll;
use crate::REGION_BLOCKS;

/// Most blocks one tree flood visits. The largest species (oak: crown radius
/// 8 blocks + 1 block of leaf reach, 20 blocks tall) fits in a 19 × 20 × 19 =
/// 7220-block box, so this bounds any generated tree with room to spare
/// (pinned by worldgen's `mature_trees_fit_the_flood_bound`). A flood that
/// hits it fells nothing.
pub const MAX_TREE_BLOCKS: usize = 8192;
/// Spin a felled part starts with, rad/s, about a horizontal axis.
pub const TIP_ANGVEL: f32 = 0.6;
/// A felled part's landing reach must stay this many blocks inside the region
/// (interim seam rule, spec §5.7: bodies cannot cross seams yet).
pub const TIP_EDGE_MARGIN_BLOCKS: i32 = 4;
/// `roll` salt of the tip direction when the part has no lean.
pub const TIP_SALT: u64 = 0x7119_FE11;
/// A lean under this many blocks (horizontal centroid offset from the cut)
/// counts as none: the direction is rolled.
pub const TIP_MIN_LEAN_BLOCKS: f32 = 0.5;

/// The eight horizontal tip directions `[x, z]`, in order of angle.
const DIRECTIONS: [[i32; 2]; 8] = [[1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1], [0, -1], [1, -1]];

fn neighbours(p: TilePos) -> impl Iterator<Item = TilePos> {
    (-1..=1).flat_map(move |dy| {
        (-1..=1).flat_map(move |dz| {
            (-1..=1).filter(move |&dx| (dx, dy, dz) != (0, 0, 0)).map(move |dx| [p[0] + dx, p[1] + dy, p[2] + dz])
        })
    })
}

/// Lowest `y`, then `x`, then `z`.
fn base_key(p: TilePos) -> (i32, i32, i32) {
    (p[1], p[0], p[2])
}

/// The parts of the tree at `cut` that fall when `cut` is chopped: flood the
/// 26-connected blocks with `cut`'s tree id (at most [`MAX_TREE_BLOCKS`]),
/// take `cut` out, and return every connected piece that does not hold the
/// tree's base (the lowest block, ties by lowest `x`, then `z`), each sorted.
/// Pieces are ordered by their first block. Empty when `cut` is not a tree
/// block or the flood overflows; when `cut` is the base, everything else
/// falls.
pub fn falling_sets(src: &impl TileSource, cut: TilePos) -> Vec<Vec<TilePos>> {
    let tree = src.tree_id(cut);
    if tree == 0 {
        return Vec::new();
    }
    let mut tree_blocks = BTreeSet::from([cut]);
    let mut stack = vec![cut];
    while let Some(p) = stack.pop() {
        for q in neighbours(p) {
            if !tree_blocks.contains(&q) && src.tree_id(q) == tree {
                if tree_blocks.len() == MAX_TREE_BLOCKS {
                    return Vec::new();
                }
                tree_blocks.insert(q);
                stack.push(q);
            }
        }
    }
    let base = *tree_blocks.iter().min_by_key(|p| base_key(**p)).expect("holds the cut");
    tree_blocks.remove(&cut);
    let mut sets = Vec::new();
    let mut unseen = tree_blocks;
    while let Some(start) = unseen.pop_first() {
        let mut piece = vec![start];
        let mut stack = vec![start];
        while let Some(p) = stack.pop() {
            for q in neighbours(p) {
                if unseen.remove(&q) {
                    piece.push(q);
                    stack.push(q);
                }
            }
        }
        if !piece.contains(&base) {
            piece.sort_unstable();
            sets.push(piece);
        }
    }
    sets.sort_unstable_by_key(|s| s[0]);
    sets
}

/// Spin for the part `set` felled at `cut` of tree `tree` on region tick
/// `tick` (rad/s, world axes): [`TIP_ANGVEL`] about the horizontal axis that
/// tips the part's top toward a direction. That direction is the nearest of
/// eight to the horizontal offset of the part's centroid from the cut, or
/// `roll(tick, tree, TIP_SALT)`'s pick under [`TIP_MIN_LEAN_BLOCKS`] of lean;
/// it is the first of it and the other seven, nearest angle first, whose
/// landing reach (cut + direction × the part's height) stays inside the
/// region with [`TIP_EDGE_MARGIN_BLOCKS`] to spare — the first choice when
/// none does. Zero for an empty set.
pub fn tip_angvel(tree: u32, cut: TilePos, set: &[TilePos], tick: u64) -> Vector {
    if set.is_empty() {
        return Vector::ZERO;
    }
    let n = set.len() as f32;
    let centre = |a: usize| set.iter().map(|p| p[a] as f32).sum::<f32>() / n - cut[a] as f32;
    let (lean_x, lean_z) = (centre(0), centre(2));
    let unit = |d: [i32; 2]| {
        let len = ((d[0] * d[0] + d[1] * d[1]) as f32).sqrt();
        (d[0] as f32 / len, d[1] as f32 / len)
    };
    let first = if (lean_x * lean_x + lean_z * lean_z).sqrt() < TIP_MIN_LEAN_BLOCKS {
        (roll(tick, u64::from(tree), TIP_SALT) % 8) as usize
    } else {
        let dot = |i: usize| {
            let (x, z) = unit(DIRECTIONS[i]);
            x * lean_x + z * lean_z
        };
        // Highest dot product, ties to the lowest index.
        (0..8).fold(0, |best, i| if dot(i) > dot(best) { i } else { best })
    };
    let (lo, hi) = set.iter().fold((i32::MAX, i32::MIN), |(lo, hi), p| (lo.min(p[1]), hi.max(p[1])));
    let height = (hi - lo + 1) as f32;
    let inside = |v: f32| v >= TIP_EDGE_MARGIN_BLOCKS as f32 && v <= (REGION_BLOCKS as i32 - TIP_EDGE_MARGIN_BLOCKS) as f32;
    let lands_inside = |i: usize| {
        let (x, z) = unit(DIRECTIONS[i]);
        inside(cut[0] as f32 + 0.5 + x * height) && inside(cut[2] as f32 + 0.5 + z * height)
    };
    // first, first+1, first-1, first+2, …, first+4.
    let order = std::iter::once(0).chain((1..4).flat_map(|k| [k, 8 - k])).chain(std::iter::once(4));
    let pick = order.map(|k| (first + k) % 8).find(|&i| lands_inside(i)).unwrap_or(first);
    let (x, z) = unit(DIRECTIONS[pick]);
    // Spinning about up × d moves the top along d.
    Vector::new(z, 0.0, -x) * TIP_ANGVEL
}
