//! Felling standing trees (spec `2026-10-29-trees-investigation.md` §6): the
//! blocks a chop at one log or branch block cuts and brings down, and the
//! spin that tips them over. Pure functions over a [`TileSource`], so the sim
//! and the client's previews agree; the `Chop` order (`orders.rs`) carries
//! them out with `clear_blocks_safe` + `detach_blocks_safe`.
//!
//! A tree is its blocks with one tree id (`Block::tree_id`, 0 = not a tree);
//! each block also carries the limb it grows on (`Block::limb`, 0 = the
//! trunk), recorded by worldgen, since touching twigs make geometry useless
//! for deciding what hangs on what.

use std::collections::{BTreeMap, BTreeSet};

use rapier3d::math::Vector;

use crate::orders::{TilePos, TileSource};
use crate::slime::roll;
use crate::{block_props, BLOCK_VOXELS, REGION_BLOCKS};

/// Most blocks one tree flood visits. The widest species box (oak: crown
/// radius 8 blocks + 1 block of leaf reach, 20 blocks tall) is 19 × 20 × 19 =
/// 7220 blocks (pine, the tallest at 24, is 11 × 24 × 11), so this bounds any
/// generated tree with room to spare (pinned by worldgen's
/// `mature_trees_fit_the_flood_bound`). A tree that overflows it cannot be
/// chopped.
pub const MAX_TREE_BLOCKS: usize = 8192;
/// A trunk cut takes the trunk logs of the cut block's layer out to this
/// many blocks (in x and in z) from it.
pub const TRUNK_CUT_RADIUS_BLOCKS: i32 = 4;
/// Spin a felled part starts with, rad/s, about a horizontal axis through
/// its pivot ([`tip_pivot`]). Only enough to start the topple: gravity does
/// the rest.
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

/// The 26 neighbours, in a fixed order.
fn neighbours(p: TilePos) -> impl Iterator<Item = TilePos> {
    (-1..=1).flat_map(move |dy| {
        (-1..=1).flat_map(move |dz| {
            (-1..=1).filter(move |&dx| (dx, dy, dz) != (0, 0, 0)).map(move |dx| [p[0] + dx, p[1] + dy, p[2] + dz])
        })
    })
}

/// What one chop does: the blocks it clears and the pieces that then fall.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Felling {
    /// Cleared by the chop, sorted.
    pub cut: Vec<TilePos>,
    /// Each falling piece, sorted (at most one).
    pub falling: Vec<Vec<TilePos>>,
}

/// Whether `p` is solid wood of tree `tree`.
fn is_wood(src: &impl TileSource, p: TilePos, tree: u32) -> bool {
    src.tree_id(p) == tree && block_props().is_solid(src.block(p))
}

/// The blocks a chop at `cut` clears, sorted: on trunk wood (a log,
/// `BlockProps::is_trunk`, of limb 0), every log of the same tree in its
/// layer at most [`TRUNK_CUT_RADIUS_BLOCKS`] from it per axis, connected or
/// not (the trunk's cross-section, with any limb log beside it); on anything
/// else (a branch, a limb's log), just `cut`. Empty when `cut` is not solid
/// tree wood.
pub fn cut_blocks(src: &impl TileSource, cut: TilePos) -> Vec<TilePos> {
    let tree = src.tree_id(cut);
    if tree == 0 || !is_wood(src, cut, tree) {
        return Vec::new();
    }
    let trunk = |p: TilePos| src.tree_id(p) == tree && block_props().is_trunk(src.block(p));
    if !trunk(cut) || src.tree_limb(cut) != 0 {
        return vec![cut];
    }
    let r = TRUNK_CUT_RADIUS_BLOCKS;
    (-r..=r)
        .flat_map(|dx| (-r..=r).map(move |dz| [cut[0] + dx, cut[1], cut[2] + dz]))
        .filter(|&q| trunk(q))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What chopping `cut` does, from the structure worldgen recorded in each
/// block (`Block::limb`: 0 = the trunk, `L ≥ 1` = a limb with everything
/// growing on it). The tree is `cut`'s 26-connected same-id blocks (at most
/// [`MAX_TREE_BLOCKS`]); a limb piece is a 26-connected component of one limb
/// index's blocks (indices wrap, so one index may name several limbs).
/// - A trunk cut (a trunk log of limb 0) clears the [`cut_blocks`]
///   cross-section; the trunk above it falls (limb-0 blocks with a higher
///   `y`) with every limb piece (taken with the cut blocks removed, so a limb
///   crossing the cut layer splits there) whose lowest wood (lowest leaf, for
///   a piece with no wood) is at or above the cut. Pieces rooted below stay,
///   unless what is left of the tree no longer joins them (26-connected,
///   through any of its blocks) to its standing lowest-level wood: those fall
///   too, so nothing floats. Cutting
///   the tree's lowest wood level fells it all.
/// - A cut on a block of limb `L ≥ 1` clears that block and fells the rest
///   of its limb piece, wood and leaves, plus — by the same support rule —
///   anything left that the felled piece alone joined to the standing tree.
///
/// Everything falls as one piece. `None` when `cut` is not tree wood or the
/// tree overflows the bound; a limb-0 block that is not trunk wood is cut
/// alone and fells nothing.
pub fn fell(src: &impl TileSource, cut: TilePos) -> Option<Felling> {
    let cuts = cut_blocks(src, cut);
    if cuts.is_empty() {
        return None;
    }
    let tree = src.tree_id(cut);
    let mut blocks = BTreeSet::from([cut]);
    let mut stack = vec![cut];
    while let Some(p) = stack.pop() {
        for q in neighbours(p) {
            if !blocks.contains(&q) && src.tree_id(q) == tree {
                if blocks.len() == MAX_TREE_BLOCKS {
                    return None;
                }
                blocks.insert(q);
                stack.push(q);
            }
        }
    }
    let limb = src.tree_limb(cut);
    let is_trunk_cut = limb == 0 && block_props().is_trunk(src.block(cut));
    let limbs: BTreeMap<TilePos, u8> = blocks.iter().map(|&p| (p, src.tree_limb(p))).collect();
    let wood = |p: &TilePos| block_props().is_solid(src.block(*p));
    let floor = blocks.iter().filter(|p| wood(p)).map(|p| p[1]).min();
    let mut falls: Vec<TilePos> = if is_trunk_cut {
        let floor = floor.expect("the cut is wood");
        let y = cut[1];
        if y == floor {
            blocks.iter().copied().collect()
        } else {
            let mut v: Vec<TilePos> = limbs.iter().filter(|&(p, &l)| l == 0 && p[1] > y).map(|(p, _)| *p).collect();
            // Pieces of what is left once the cut is out: a limb rooted below
            // the cut whose wood crosses the cut layer splits there.
            let mut left = limbs.clone();
            left.retain(|p, _| !cuts.contains(p));
            for piece in limb_components(&left) {
                // Lowest wood, or lowest leaf for a piece with no wood.
                let root = piece.iter().map(|p| (!wood(p), p[1])).min().expect("non-empty").1;
                if root >= y {
                    v.extend(piece);
                }
            }
            v
        }
    } else if limb != 0 {
        let piece = limb_components(&limbs).into_iter().find(|c| c.contains(&cut)).expect("the cut is in a piece");
        piece.into_iter().filter(|&p| p != cut).collect()
    } else {
        Vec::new()
    };
    if is_trunk_cut || limb != 0 {
        falls.retain(|p| !cuts.contains(p));
        // Support (trunk and branch cuts alike): whatever is left that no
        // longer joins (26-connected, through any standing block of the
        // tree) the standing wood of the lowest level falls too, so nothing
        // is left floating — e.g. another limb's leaves held only by the
        // felled limb.
        let floor = floor.expect("the cut is wood");
        let gone: BTreeSet<TilePos> = falls.iter().chain(&cuts).copied().collect();
        let standing: BTreeSet<TilePos> = blocks.iter().copied().filter(|p| !gone.contains(p)).collect();
        let mut held: BTreeSet<TilePos> = standing.iter().copied().filter(|p| p[1] == floor && wood(p)).collect();
        let mut stack: Vec<TilePos> = held.iter().copied().collect();
        while let Some(p) = stack.pop() {
            for q in neighbours(p) {
                if standing.contains(&q) && held.insert(q) {
                    stack.push(q);
                }
            }
        }
        falls.extend(standing.into_iter().filter(|p| !held.contains(p)));
        falls.sort_unstable();
    }
    let falling = if falls.is_empty() { Vec::new() } else { vec![falls] };
    Some(Felling { cut: cuts, falling })
}

/// The limb pieces of a tree: for each limb index `L ≥ 1`, the 26-connected
/// components of its blocks (connected only through blocks of `L`), each
/// sorted, in order of (limb, first block). Limb indices wrap
/// (`MAX_TREE_LIMB`), so one index can name several far-apart limbs: the
/// component, not the index, is what hangs together.
fn limb_components(limbs: &BTreeMap<TilePos, u8>) -> Vec<Vec<TilePos>> {
    let mut by_limb: BTreeMap<u8, BTreeSet<TilePos>> = BTreeMap::new();
    for (&p, &l) in limbs {
        if l != 0 {
            by_limb.entry(l).or_default().insert(p);
        }
    }
    let mut out = Vec::new();
    for (_, mut unseen) in by_limb {
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
            piece.sort_unstable();
            out.push(piece);
        }
    }
    out
}

/// The pieces that fall when `cut` is chopped ([`fell`]); empty when none do
/// or `cut` cannot be chopped.
pub fn falling_sets(src: &impl TileSource, cut: TilePos) -> Vec<Vec<TilePos>> {
    fell(src, cut).map_or_else(Vec::new, |f| f.falling)
}

/// Spin for the part `set` felled at `cut` of tree `tree` on region tick
/// `tick` (rad/s, world axes): [`TIP_ANGVEL`] about the horizontal axis that
/// tips the part's top toward a direction. That direction is the nearest of
/// eight to the horizontal offset of the part's centroid from the cut, or
/// `roll(tick, tree, TIP_SALT)`'s pick under [`TIP_MIN_LEAN_BLOCKS`] of lean;
/// it is the first of it and the other seven, nearest angle first, whose
/// landing reach (cut + direction × the part's height) stays inside the
/// region with [`TIP_EDGE_MARGIN_BLOCKS`] to spare — the first choice when
/// none does (the part may then leave the region and vanish, as any voxel
/// body does). Zero for an empty set.
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

/// The point a part felled at `cut` starts turning about (region-local
/// units): the middle of the cut block's top face — the butt of the trunk
/// above a trunk cut — so the part topples about its base, its butt still,
/// rather than spinning about its centre of mass with the butt swinging up.
pub fn tip_pivot(cut: TilePos) -> Vector {
    let b = BLOCK_VOXELS as f32;
    Vector::new((cut[0] as f32 + 0.5) * b, (cut[1] + 1) as f32 * b, (cut[2] as f32 + 0.5) * b)
}
