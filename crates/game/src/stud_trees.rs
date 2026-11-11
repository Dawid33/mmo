//! Felling trees on stud terrain — `trees.rs`' twin (stud world plan P6b).
//!
//! A stud tree is the bricks carrying one non-zero tree id in `StudBrick::data`
//! (worldgen's `stud_trees.rs`): wood (`MAT_WOOD`) and leaves (`MAT_LEAVES`). Unlike a
//! voxel tree block, a brick records no limb, so felling is by height and by support:
//!
//! - the **cut** is the tree's wood bricks meeting the chopped tile's level within
//!   [`STUD_TRUNK_CUT_RADIUS_TILES`] of it (the trunk's cross-section there);
//! - everything of the tree that starts at or above the lowest cut brick **falls**,
//!   and so does whatever is then no longer joined (bricks touching by a face, an
//!   edge or a corner) to the tree's standing wood at its lowest level — nothing is
//!   left floating. A chop at the tree's foot fells it all; a chop on a high branch
//!   fells the whole top from that height (there is no limb to follow).
//!
//! The bricks are found by scanning the chunks around the chopped tile
//! ([`STUD_TREE_SCAN_TILES_H`] / [`STUD_TREE_SCAN_TILES_V`]) for the id, not by a
//! flood: leaves need not touch their tree. Only this region's chunks: a generated
//! tree never crosses a seam (worldgen keeps crowns inside the region), as on voxels.
//!
//! [`Rollback::fell_stud_safe`] carries it out: the cut bricks leave (one write per
//! chunk) and yield their drops at the stump at once; the falling bricks become one
//! stud body (`stud_body.rs`) turning about the cut, which settles by its policy.

use std::collections::{BTreeMap, BTreeSet};

use rapier3d::math::Vector;

use crate::orders::TilePos;
use crate::slime::roll;
use crate::stud::{StudBrick, StudChunkCoords, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, TILE_PLATES, TILE_STUDS};
use crate::stud_material::MAT_WOOD;
use crate::tiles::{STUD_CHUNK_TILES, STUD_LAYER_TILES};
use crate::{EntityKey, Rollback, SettlePolicy, REGION_CHUNKS};

/// How far (tiles, on x and on z) from the chopped tile a tree's bricks are looked
/// for: the widest generated tree (crown radius 8 m and a metre of leaf reach either
/// side of the trunk: 19 m) and a leaf cell. Pinned against the species table by
/// worldgen's `stud_trees::trees_fit_the_felling_scan`.
pub const STUD_TREE_SCAN_TILES_H: i32 = 20;
/// The same reach up and down (tiles of 1.2 m): the tallest tree (32 m) with its foot.
pub const STUD_TREE_SCAN_TILES_V: i32 = 29;
/// A trunk cut takes the tree's wood of the chopped level out to this many tiles (on
/// x and on z) from the chopped tile .
pub const STUD_TRUNK_CUT_RADIUS_TILES: i32 = 4;
/// Most bricks of one tree a felling handles; a bigger one cannot be chopped.
pub const MAX_STUD_TREE_BRICKS: usize = crate::stud_body::MAX_STUD_BODY_BRICKS;
/// Spin a felled part starts with, rad/s, about a horizontal axis through
/// its pivot (the top of the chopped tile). Only enough to start the topple: gravity does
/// the rest.
pub const TIP_ANGVEL: f32 = 0.6;
/// `roll` salt of the tip direction when the part has no lean.
pub const TIP_SALT: u64 = 0x7119_FE11;
/// A lean under this many blocks (horizontal centroid offset from the cut)
/// counts as none: the direction is rolled.
pub const TIP_MIN_LEAN_BLOCKS: f32 = 0.5;

/// The eight horizontal tip directions `[x, z]`, in order of angle.
const DIRECTIONS: [[i32; 2]; 8] = [[1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1], [0, -1], [1, -1]];

/// Spin for the part `set` felled at `cut` of tree `tree` on region tick
/// `tick` (rad/s, world axes): [`TIP_ANGVEL`] about the horizontal axis that
/// tips the part's top toward a direction. That direction is the nearest of
/// eight to the horizontal offset of the part's centroid from the cut, or
/// `roll(tick, tree, TIP_SALT)`'s pick under [`TIP_MIN_LEAN_BLOCKS`] of lean.
/// A region edge does not matter: the felled part crosses the seam as any
/// body does. Zero for an empty set.
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
    let pick = if (lean_x * lean_x + lean_z * lean_z).sqrt() < TIP_MIN_LEAN_BLOCKS {
        (roll(tick, u64::from(tree), TIP_SALT) % 8) as usize
    } else {
        let dot = |i: usize| {
            let (x, z) = unit(DIRECTIONS[i]);
            x * lean_x + z * lean_z
        };
        // Highest dot product, ties to the lowest index.
        (0..8).fold(0, |best, i| if dot(i) > dot(best) { i } else { best })
    };
    let (x, z) = unit(DIRECTIONS[pick]);
    // Spinning about up × d moves the top along d.
    Vector::new(z, 0.0, -x) * TIP_ANGVEL
}

/// One brick of a tree: its chunk, and its cell box in region stud cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TreeBrick {
    pub lo: [i32; 3],
    pub hi: [i32; 3],
    pub chunk: StudChunkCoords,
    pub min: [u8; 3],
    pub wood: bool,
}

/// What one chop of a stud tree does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StudFelling {
    pub tree: u16,
    /// Cleared by the chop (wood), sorted.
    pub cut: Vec<TreeBrick>,
    /// What then falls, as one piece, sorted. May be empty.
    pub falling: Vec<TreeBrick>,
}

fn brick_box(c: StudChunkCoords, b: &StudBrick) -> ([i32; 3], [i32; 3]) {
    let base = [(c.x * STUD_CHUNK_STUDS) as i32, c.y * STUD_CHUNK_PLATES as i32, (c.z * STUD_CHUNK_STUDS) as i32];
    let size = b.size();
    let lo = [0, 1, 2].map(|a| base[a] + i32::from(b.min[a]));
    (lo, [0, 1, 2].map(|a| lo[a] + i32::from(size[a])))
}

/// Whether two cell boxes touch or overlap (by a face, an edge or a corner).
fn touching(a: &TreeBrick, b: &TreeBrick) -> bool {
    (0..3).all(|i| a.lo[i] <= b.hi[i] && b.lo[i] <= a.hi[i])
}

fn picks(bricks: &[TreeBrick]) -> BTreeMap<StudChunkCoords, BTreeSet<[u8; 3]>> {
    let mut out: BTreeMap<StudChunkCoords, BTreeSet<[u8; 3]>> = BTreeMap::new();
    for b in bricks {
        out.entry(b.chunk).or_default().insert(b.min);
    }
    out
}

impl Rollback {
    /// The tree whose WOOD is in stud tile `p` of this region: the tree id of the
    /// first wood brick (in chunk order) with one that meets the tile. `None` for a
    /// tile with no tree wood (leaves alone are not a tree to chop), or outside the
    /// region.
    pub fn stud_tree_at(&self, p: TilePos) -> Option<u16> {
        let (n, l) = (STUD_CHUNK_TILES as i32, STUD_LAYER_TILES as i32);
        let (cx, cz) = (p[0].div_euclid(n), p[2].div_euclid(n));
        if !(0..REGION_CHUNKS as i32).contains(&cx) || !(0..REGION_CHUNKS as i32).contains(&cz) {
            return None;
        }
        let c = StudChunkCoords { x: cx as usize, y: p[1].div_euclid(l), z: cz as usize };
        let (lo, hi) = crate::stud_tile_box(p)?;
        self.stud_chunk_at(c)?.bricks().find_map(|b| {
            let (blo, bhi) = brick_box(c, b);
            let meets = (0..3).all(|i| blo[i] < hi[i] && lo[i] < bhi[i]);
            (meets && b.data != 0 && b.material == MAT_WOOD).then_some(b.data)
        })
    }

    /// Every brick of tree `tree` in this region's chunks within the scan reach of
    /// tile `p`, sorted; `None` over [`MAX_STUD_TREE_BRICKS`].
    pub(crate) fn stud_tree_bricks(&self, tree: u16, p: TilePos) -> Option<Vec<TreeBrick>> {
        let (n, l) = (STUD_CHUNK_TILES as i32, STUD_LAYER_TILES as i32);
        let (h, v) = (STUD_TREE_SCAN_TILES_H, STUD_TREE_SCAN_TILES_V);
        let last = REGION_CHUNKS as i32 - 1;
        let chunks = |c: i32| ((c - h).div_euclid(n).max(0), (c + h).div_euclid(n).min(last));
        let ((x0, x1), (z0, z1)) = (chunks(p[0]), chunks(p[2]));
        let (y0, y1) = (p[1].saturating_sub(v).div_euclid(l), p[1].saturating_add(v).div_euclid(l));
        let mut out = Vec::new();
        for x in x0..=x1 {
            let range = StudChunkCoords { x: x as usize, y: y0, z: 0 }..=StudChunkCoords { x: x as usize, y: y1, z: usize::MAX };
            for (c, _) in self.data.stud_index.range(range).filter(|(c, _)| (z0..=z1).contains(&(c.z as i32))) {
                let Some(chunk) = self.stud_chunk_at(*c) else { continue };
                for b in chunk.bricks().filter(|b| b.data == tree) {
                    let (lo, hi) = brick_box(*c, b);
                    out.push(TreeBrick { lo, hi, chunk: *c, min: b.min, wood: b.material == MAT_WOOD });
                    if out.len() > MAX_STUD_TREE_BRICKS {
                        return None;
                    }
                }
            }
        }
        out.sort_unstable();
        Some(out)
    }

    /// What chopping stud tile `p` does (module docs); `None` when the tile holds no
    /// tree wood or the tree is over [`MAX_STUD_TREE_BRICKS`]. Pure.
    pub fn stud_fell(&self, p: TilePos) -> Option<StudFelling> {
        let tree = self.stud_tree_at(p)?;
        let bricks = self.stud_tree_bricks(tree, p)?;
        let (ts, tp) = (TILE_STUDS as i32, TILE_PLATES as i32);
        let r = STUD_TRUNK_CUT_RADIUS_TILES;
        // The cut level's slab around the chopped tile, in stud cells.
        let slab_lo = [(p[0] - r) * ts, p[1] * tp, (p[2] - r) * ts];
        let slab_hi = [(p[0] + r + 1) * ts, (p[1] + 1) * tp, (p[2] + r + 1) * ts];
        let in_cut = |b: &TreeBrick| b.wood && (0..3).all(|i| b.lo[i] < slab_hi[i] && slab_lo[i] < b.hi[i]);
        let cut: Vec<TreeBrick> = bricks.iter().copied().filter(in_cut).collect();
        let from = cut.iter().map(|b| b.lo[1]).min()?;
        let floor = bricks.iter().filter(|b| b.wood).map(|b| b.lo[1]).min()?;
        let (mut falling, standing): (Vec<TreeBrick>, Vec<TreeBrick>) =
            bricks.iter().copied().filter(|b| !in_cut(b)).partition(|b| b.lo[1] >= from);
        // Support: what still joins the standing wood of the tree's lowest level.
        let mut held = vec![false; standing.len()];
        let mut stack: Vec<usize> = (0..standing.len()).filter(|&i| standing[i].wood && standing[i].lo[1] == floor).collect();
        for &i in &stack {
            held[i] = true;
        }
        while let Some(i) = stack.pop() {
            for j in 0..standing.len() {
                if !held[j] && touching(&standing[i], &standing[j]) {
                    held[j] = true;
                    stack.push(j);
                }
            }
        }
        falling.extend(standing.iter().zip(&held).filter(|(_, h)| !**h).map(|(b, _)| *b));
        falling.sort_unstable();
        Some(StudFelling { tree, cut, falling })
    }

    /// Fells the stud tree at tile `p` ([`Self::stud_fell`], computed first),
    /// undo-safely: the cut bricks are removed and their yield (`mining::stud_drops`)
    /// put down at the chopped tile at once; the falling bricks are detached into one
    /// stud body turning about the middle of the chopped tile's top (the tip spin of
    /// [`tip_angvel`], toward its lean), settling by `policy`.
    /// Returns the cells cut per material and the falling body (`None` when nothing
    /// fell). `None`, with nothing changed, where `stud_fell` refuses.
    pub fn fell_stud_safe(&mut self, p: TilePos, policy: SettlePolicy) -> Option<(Vec<(u16, u32)>, Option<EntityKey>)> {
        let felling = self.stud_fell(p)?;
        let tick = *self.data.tick;
        // The cut: out of its chunks, one write each.
        let mut removed: BTreeMap<u16, u32> = BTreeMap::new();
        for (c, mins) in picks(&felling.cut) {
            let Some(key) = self.stud_key_at(c) else { continue };
            let Some(mut chunk) = self.data.ecs.stud.try_get(key).clone() else { continue };
            let mut changed = false;
            for min in mins {
                if let Some(b) = chunk.bricks.remove(&min) {
                    *removed.entry(b.material).or_default() += b.size().iter().map(|&v| u32::from(v)).product::<u32>();
                    changed = true;
                }
            }
            if changed {
                self.set_stud_chunk_safe(key, chunk);
            }
        }
        let removed: Vec<(u16, u32)> = removed.into_iter().collect();
        if removed.is_empty() {
            return None;
        }
        // Piles resting on the cut wood settle again.
        let lo = felling.cut.iter().fold([i32::MAX; 3], |l, b| [0, 1, 2].map(|a| l[a].min(b.lo[a])));
        let hi = felling.cut.iter().fold([i32::MIN; 3], |h, b| [0, 1, 2].map(|a| h[a].max(b.hi[a])));
        let _destroyed = self.stud_piles_after_write_safe(lo, hi);
        // The fall, turning about the butt: the top of the chopped tile.
        let (cx, cz) = crate::column_centre([p[0], p[2]]);
        let pivot = rapier3d::math::Vector::new(cx, crate::level_bottom_units(p[1] + 1), cz);
        let centres: Vec<TilePos> = felling
            .falling
            .iter()
            .map(|b| {
                [
                    (b.lo[0] + b.hi[0]).div_euclid(2 * TILE_STUDS as i32),
                    (b.lo[1] + b.hi[1]).div_euclid(2 * TILE_PLATES as i32),
                    (b.lo[2] + b.hi[2]).div_euclid(2 * TILE_STUDS as i32),
                ]
            })
            .collect();
        let spin = tip_angvel(u32::from(felling.tree), p, &centres, tick);
        let body = self.detach_studs_safe(&picks(&felling.falling), policy, spin, Some(pivot));
        // The cut's yield, at the stump.
        self.deposit_drops_safe(p, &crate::mining::stud_drops(&removed), tick);
        Some((removed, body))
    }
}
