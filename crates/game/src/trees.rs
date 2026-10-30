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
use crate::{block_props, REGION_BLOCKS};

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
/// block (`Block::limb`: 0 = the trunk, `L ≥ 1` = one limb with everything
/// growing on it). The tree is `cut`'s 26-connected same-id blocks (at most
/// [`MAX_TREE_BLOCKS`]).
/// - A trunk cut (a trunk log of limb 0) clears the [`cut_blocks`]
///   cross-section; the trunk above it falls (limb-0 blocks with a higher
///   `y`) with every limb whose lowest wood (lowest leaf, for a limb with
///   no wood) is at or above the cut. Limbs rooted below stay. Cutting the tree's lowest wood level fells it all.
/// - A cut on a block of limb `L ≥ 1` clears that block and fells the rest
///   of limb `L`, wood and leaves.
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
    let wood = |p: &TilePos| block_props().is_solid(src.block(*p));
    let falls: Vec<TilePos> = if is_trunk_cut {
        let floor = blocks.iter().filter(|p| wood(p)).map(|p| p[1]).min().expect("the cut is wood");
        // Lowest wood level of each limb; a limb with no wood (all of it
        // thinner than the rasteriser's wood) by its lowest leaf.
        let mut limb_floor: BTreeMap<u8, (bool, i32)> = BTreeMap::new();
        for p in &blocks {
            let l = src.tree_limb(*p);
            if l != 0 {
                let key = (!wood(p), p[1]);
                let f = limb_floor.entry(l).or_insert(key);
                *f = (*f).min(key);
            }
        }
        let y = cut[1];
        blocks
            .iter()
            .copied()
            .filter(|p| !cuts.contains(p))
            .filter(|&p| {
                if y == floor {
                    return true;
                }
                match src.tree_limb(p) {
                    0 => p[1] > y,
                    l => limb_floor.get(&l).is_some_and(|&(_, f)| f >= y),
                }
            })
            .collect()
    } else if limb != 0 {
        blocks.iter().copied().filter(|&p| p != cut && src.tree_limb(p) == limb).collect()
    } else {
        Vec::new()
    };
    let falling = if falls.is_empty() { Vec::new() } else { vec![falls] };
    Some(Felling { cut: cuts, falling })
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
