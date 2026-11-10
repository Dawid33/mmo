//! Trees on stud terrain: the voxel generator's trees (same placement, species, seeds
//! and skeletons — `trees::trees_in`, `trees::skeleton`) laid as feature bricks instead
//! of blocks. Only the shape → blocks step (`trees::rasterize`) is replaced.
//!
//! A tree is rasterised onto the stud cell grid (a stud wide, a plate tall) from its
//! skeleton with integer arithmetic only: wood along the trunk and limb segments, leaves
//! as brick-course cells around the leaf clusters. The cells are merged greedily into
//! boxes, cut at chunk edges (bricks never cross one) and tiled with catalog parts
//! (`game::fill_box`). Every brick carries the tree's id in `StudBrick::data`
//! ([`stud_tree_id`]): felling needs it.
//!
//! Like voxel trees, a tree belongs to the region of its base and never writes outside
//! it, so a region's features are a pure function of the region.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use game::{
    fill_box, StudBrick, StudChunkCoords, StudColumnGround, MAT_LEAVES, MAT_WOOD, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS,
};

use super::*;
use crate::stud_gen::STUDS_PER_M;

/// Voxels (1/16 m, the skeleton's unit) per stud.
const STUD_VOXELS: i32 = BLOCK_VOXELS as i32 / STUDS_PER_M;
/// Fifths of a voxel per stud and per plate: a plate is 3.2 voxels, so cell centres
/// and distances are whole numbers in fifths.
const STUD_FIFTHS: i64 = STUD_VOXELS as i64 * 5;
const PLATE_FIFTHS: i64 = 16;
const _: () = assert!(BLOCK_VOXELS == 16 && STUD_VOXELS == 8);
/// Wood is laid in courses this many plates tall (one brick), counted from the tree's
/// base plate: per-plate cells made a leaning limb a stair of 1 x 1 plates.
pub const WOOD_COURSE_PLATES: i32 = 3;
/// Leaves are laid in courses this many plates tall (one brick).
pub const LEAF_COURSE_PLATES: i32 = 3;
/// Highest skeleton level laid as wood (0 trunk, 1 limb, 2 branch, 3 twig). Thinner
/// wood would be strings of 1 x 1 bricks inside the leaves.
pub const STUD_TREE_WOOD_LEVEL: u8 = 1;
/// [`STUD_TREE_WOOD_LEVEL`] for a species without leaves: its branches are all it has.
pub const STUD_TREE_BARE_WOOD_LEVEL: u8 = 2;
/// Leaves are laid in cells this many studs wide (on the tree's own grid, even studs
/// from its base block): a canopy of 1-stud cells was ~400 bricks a tree.
pub const LEAF_CELL_STUDS: i32 = 2;
/// Voxels between the points a wood segment is sampled at: under a plate (3.2), so a
/// thin trunk never skips one.
const WOOD_STEP_VOXELS: i64 = 2;
/// Bits per axis of a stud tree id.
const STUD_ID_AXIS_BITS: u32 = 8;
/// The id of the trees whose lattice cell is `(0, 0)` in both low bytes: 0 is "not a
/// tree", so they take the id of the cell half a wrap away on both axes.
const STUD_ZERO_TREE_ID: u16 = 0x8080;

/// The tree id a stud tree's bricks carry in `StudBrick::data`: the low
/// [`STUD_ID_AXIS_BITS`] bits of each axis of the voxel tree id (a lattice cell,
/// `trees::placement::tree_id`), so non-zero and shared only by trees a multiple of
/// 128 lattice cells (1024 m) apart on both axes — never by two trees of one region or
/// of two neighbouring ones (regions are at most 512 m wide).
pub fn stud_tree_id(id: u32) -> u16 {
    let axis_bits = game::TREE_ID_BITS / 2;
    let mask = (1u32 << STUD_ID_AXIS_BITS) - 1;
    let packed = ((id & mask) | ((id >> axis_bits) & mask) << STUD_ID_AXIS_BITS) as u16;
    if packed == 0 { STUD_ZERO_TREE_ID } else { packed }
}
const _: () = assert!(
    trees::TREE_LATTICE_BLOCKS as i64 * (1i64 << (STUD_ID_AXIS_BITS - 1)) >= 2 * REGION_BLOCKS as i64
        || REGION_BLOCKS > 512
);

/// A tree's cells `[stud, plate, stud]` relative to its base block's minimum stud and
/// its base plate. Disjoint.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StudTreeCells {
    pub wood: BTreeSet<[i32; 3]>,
    pub leaves: BTreeSet<[i32; 3]>,
}

/// The cell holding skeleton point `v` (voxels).
fn cell_of(v: [i64; 3]) -> [i32; 3] {
    [
        v[0].div_euclid(STUD_VOXELS as i64) as i32,
        (v[1] * 5).div_euclid(PLATE_FIFTHS) as i32,
        v[2].div_euclid(STUD_VOXELS as i64) as i32,
    ]
}

/// Marks the cell holding point `p` (voxels) and every cell whose centre is within `r`
/// voxels of it, on the grid of cells `width` studs wide and `height` plates tall.
fn mark_ball(out: &mut BTreeSet<[i32; 3]>, p: [i64; 3], r: i64, width: i32, height: i32) {
    let (w, h) = (i64::from(width), i64::from(height));
    // A coarse cell's index along each axis, and the fine cells of coarse cell `c`.
    let coarse = |c: [i32; 3]| [c[0].div_euclid(width), c[1].div_euclid(height), c[2].div_euclid(width)];
    let mut fill = |c: [i32; 3]| {
        for dx in 0..width {
            for dy in 0..height {
                for dz in 0..width {
                    out.insert([c[0] * width + dx, c[1] * height + dy, c[2] * width + dz]);
                }
            }
        }
    };
    fill(coarse(cell_of(p)));
    let (p5, r5) = (p.map(|v| v * 5), r * 5);
    let lo = coarse(cell_of([p[0] - r, p[1] - r, p[2] - r]));
    let hi = coarse(cell_of([p[0] + r, p[1] + r, p[2] + r]));
    for cy in lo[1]..=hi[1] {
        let y5 = i64::from(cy) * h * PLATE_FIFTHS + h * PLATE_FIFTHS / 2;
        for cz in lo[2]..=hi[2] {
            let z5 = i64::from(cz) * w * STUD_FIFTHS + w * STUD_FIFTHS / 2;
            for cx in lo[0]..=hi[0] {
                let x5 = i64::from(cx) * w * STUD_FIFTHS + w * STUD_FIFTHS / 2;
                let d = [x5 - p5[0], y5 - p5[1], z5 - p5[2]];
                if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= r5 * r5 {
                    fill([cx, cy, cz]);
                }
            }
        }
    }
}

/// The stud cells of a tree with skeleton `sk`: wood along every segment up to
/// skeleton level `wood_level`, leaves around every leaf cluster where no wood is.
/// Integer arithmetic only.
pub fn stud_tree_cells(sk: &trees::TreeSkeleton, wood_level: u8) -> StudTreeCells {
    let mut wood = BTreeSet::new();
    for n in sk.nodes.iter().filter(|n| n.level <= wood_level) {
        let b = n.pos.map(i64::from);
        let (a, ra) = match n.parent.and_then(|p| sk.nodes.get(p as usize)) {
            Some(p) => (p.pos.map(i64::from), i64::from(p.radius)),
            None => (b, i64::from(n.radius)),
        };
        let rb = i64::from(n.radius);
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let steps = d.iter().map(|v| v.abs()).max().unwrap_or(0) / WOOD_STEP_VOXELS + 1;
        let course = |p: [i64; 3]| {
            let c = cell_of(p);
            [c[0], c[1].div_euclid(WOOD_COURSE_PLATES), c[2]]
        };
        let mut prev = course(a);
        for i in 0..=steps {
            let p = [a[0] + d[0] * i / steps, a[1] + d[1] * i / steps, a[2] + d[2] * i / steps];
            mark_ball(&mut wood, p, ra + (rb - ra) * i / steps, 1, WOOD_COURSE_PLATES);
            // A step that changes two axes at once would leave the two cells touching
            // by an edge only: bridge it, up first, so wood is face-connected.
            let cur = course(p);
            for bridge in [[prev[0], cur[1], prev[2]], [cur[0], cur[1], prev[2]]] {
                if bridge != prev && bridge != cur {
                    for dy in 0..WOOD_COURSE_PLATES {
                        wood.insert([bridge[0], bridge[1] * WOOD_COURSE_PLATES + dy, bridge[2]]);
                    }
                }
            }
            prev = cur;
        }
    }
    let mut leaves = BTreeSet::new();
    for &(centre, radius) in &sk.leaves {
        mark_ball(&mut leaves, centre.map(i64::from), i64::from(radius), LEAF_CELL_STUDS, LEAF_COURSE_PLATES);
    }
    // Nothing under the base plate: the ground is there.
    wood.retain(|c| c[1] >= 0);
    leaves.retain(|c| c[1] >= 0 && !wood.contains(c));
    StudTreeCells { wood, leaves }
}

/// Greedy boxes `(min, size)` covering `cells` exactly, disjoint: from the lowest
/// remaining cell (in `[x, y, z]` order) along x, then whole rows along z, then whole
/// slabs along y.
fn merge_boxes(mut cells: BTreeSet<[i32; 3]>) -> Vec<([i32; 3], [i32; 3])> {
    let mut out = Vec::new();
    while let Some(&s) = cells.iter().next() {
        let mut e = [1, 1, 1];
        while cells.contains(&[s[0] + e[0], s[1], s[2]]) {
            e[0] += 1;
        }
        let row = |y: i32, z: i32, e0: i32, set: &BTreeSet<[i32; 3]>| (0..e0).all(|x| set.contains(&[s[0] + x, y, z]));
        while row(s[1], s[2] + e[2], e[0], &cells) {
            e[2] += 1;
        }
        while (0..e[2]).all(|z| row(s[1] + e[1], s[2] + z, e[0], &cells)) {
            e[1] += 1;
        }
        for x in 0..e[0] {
            for y in 0..e[1] {
                for z in 0..e[2] {
                    cells.remove(&[s[0] + x, s[1] + y, s[2] + z]);
                }
            }
        }
        out.push((s, e));
    }
    out
}

/// Lays the region stud cells `cells` (`[stud x, plate y, stud z]`, plate 0 = the
/// bottom of layer 0) as bricks of `material` tagged `data` into `out`, by chunk.
fn lay_cells(
    cells: BTreeSet<[i32; 3]>,
    material: u16,
    data: u16,
    out: &mut BTreeMap<StudChunkCoords, Vec<StudBrick>>,
) {
    let chunk = [STUD_CHUNK_STUDS as i32, STUD_CHUNK_PLATES as i32, STUD_CHUNK_STUDS as i32];
    for (min, size) in merge_boxes(cells) {
        let first = [0, 1, 2].map(|a| min[a].div_euclid(chunk[a]));
        let last = [0, 1, 2].map(|a| (min[a] + size[a] - 1).div_euclid(chunk[a]));
        for cx in first[0]..=last[0] {
            for cy in first[1]..=last[1] {
                for cz in first[2]..=last[2] {
                    let c = [cx, cy, cz];
                    // The box's part inside this chunk, chunk-local.
                    let lo = [0, 1, 2].map(|a| (min[a] - c[a] * chunk[a]).max(0));
                    let hi = [0, 1, 2].map(|a| (min[a] + size[a] - c[a] * chunk[a]).min(chunk[a]));
                    let coords = StudChunkCoords { x: cx as usize, y: cy, z: cz as usize };
                    fill_box(
                        lo.map(|v| v as u8),
                        [0, 1, 2].map(|a| (hi[a] - lo[a]) as u8),
                        material,
                        data,
                        out.entry(coords).or_default(),
                    );
                }
            }
        }
    }
}

/// The feature bricks of region `coords`' trees, by chunk (ascending), for the ground
/// recipes `ground` (the whole region's, `recipes_from_patch`'s). `patch` must cover
/// the region plus `TREE_PATCH_MARGIN`.
///
/// A tree stands on the highest of the four stud columns of its base block (its trunk
/// reaches down to the others'), keeps only cells inside the region and above the
/// ground, and where trees overlap wood wins over leaves, else the lower voxel tree id.
pub(crate) fn stud_tree_features(
    patch: &dyn Patch,
    coords: RegionCoords,
    mats: &Materials,
    trees_from: TreeSource,
    ground: &[(game::ColumnCoords, StudColumnGround)],
) -> Vec<(StudChunkCoords, Vec<StudBrick>)> {
    let (rx, rz) = (coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS);
    // Placement reads surfaces up to `m` past the region; each surface's slope reads
    // one block further (still inside the patch's margin) — as the LOD's trees do.
    let m = TREE_PATCH_MARGIN - PATCH_MARGIN;
    let sample_memo = std::cell::RefCell::new(std::collections::HashMap::new());
    let sample = |x: i32, z: i32| {
        let cached = sample_memo.borrow().get(&(x, z)).copied();
        cached.unwrap_or_else(|| {
            let c = column_sample(patch, mats, x, z);
            sample_memo.borrow_mut().insert((x, z), c);
            c
        })
    };
    let memo = std::cell::RefCell::new(std::collections::HashMap::new());
    let surface = |x: i32, z: i32| {
        assert!(
            (rx - m..rx + REGION_BLOCKS + m).contains(&x) && (rz - m..rz + REGION_BLOCKS + m).contains(&z),
            "tree placement read the surface at ({x}, {z}), outside the patch"
        );
        let cached = memo.borrow().get(&(x, z)).copied();
        cached.unwrap_or_else(|| {
            let c = sample(x, z);
            let slope = slopes::slope_for(&c, |dx, dz| sample(x + dx, z + dz)).is_some();
            let g = tree_ground(c, slope);
            memo.borrow_mut().insert((x, z), g);
            g
        })
    };
    let placed = trees_for_window(coords, [rx, rz, rx + REGION_BLOCKS, rz + REGION_BLOCKS], trees_from, &surface);

    let n = STUD_CHUNK_STUDS as i32;
    let region_studs = REGION_BLOCKS * STUDS_PER_M;
    let recipes: BTreeMap<(usize, usize), &StudColumnGround> = ground.iter().map(|(c, g)| ((c.x, c.z), g)).collect();
    // The ground's top plate (one past) at region stud column `(sx, sz)`; `None`
    // outside the region.
    let ground_top = |sx: i32, sz: i32| -> Option<i32> {
        if !(0..region_studs).contains(&sx) || !(0..region_studs).contains(&sz) {
            return None;
        }
        let g = recipes.get(&((sx / n) as usize, (sz / n) as usize))?;
        Some(g.surface((sx % n) as usize, (sz % n) as usize))
    };

    // Each tree's cells in region stud cells, in voxel tree id order.
    let mut trees_cells: Vec<(u16, BTreeSet<[i32; 3]>, BTreeSet<[i32; 3]>)> = Vec::with_capacity(placed.len());
    for tree in &placed {
        let species = &trees::SPECIES[tree.species as usize];
        let sk = trees::skeleton(species, tree.seed, tree.age);
        let bare = species.shape.leafless || species.single_block;
        let cells = stud_tree_cells(&sk, if bare { STUD_TREE_BARE_WOOD_LEVEL } else { STUD_TREE_WOOD_LEVEL });
        let base = [(tree.base[0] - rx) * STUDS_PER_M, (tree.base[2] - rz) * STUDS_PER_M];
        let tops: Vec<i32> = (0..STUDS_PER_M)
            .flat_map(|dz| (0..STUDS_PER_M).map(move |dx| (dx, dz)))
            .filter_map(|(dx, dz)| ground_top(base[0] + dx, base[1] + dz))
            .collect();
        let Some(&base_plate) = tops.iter().max() else { continue };
        let to_region = |c: &[i32; 3]| [base[0] + c[0], base_plate + c[1], base[1] + c[2]];
        let above_ground = |c: &[i32; 3]| ground_top(c[0], c[2]).is_some_and(|top| c[1] >= top);
        let mut wood: BTreeSet<[i32; 3]> = cells.wood.iter().map(to_region).filter(above_ground).collect();
        // The trunk's foot reaches down to the ground of every column it stands over.
        for c in cells.wood.iter().filter(|c| c[1] == 0) {
            let r = to_region(c);
            if let Some(top) = ground_top(r[0], r[2]) {
                wood.extend((top..r[1]).map(|p| [r[0], p, r[2]]));
            }
        }
        let leaves: BTreeSet<[i32; 3]> = cells.leaves.iter().map(to_region).filter(above_ground).collect();
        trees_cells.push((stud_tree_id(tree.id), wood, leaves));
    }

    // Overlaps: all wood first (the lower id keeps a shared cell), then leaves.
    let mut claimed: HashSet<[i32; 3]> = HashSet::new();
    for (_, wood, _) in &mut trees_cells {
        wood.retain(|c| claimed.insert(*c));
    }
    for (_, _, leaves) in &mut trees_cells {
        leaves.retain(|c| claimed.insert(*c));
    }
    let mut out: BTreeMap<StudChunkCoords, Vec<StudBrick>> = BTreeMap::new();
    for (id, wood, leaves) in trees_cells {
        lay_cells(wood, MAT_WOOD, id, &mut out);
        lay_cells(leaves, MAT_LEAVES, id, &mut out);
    }
    out.into_iter().collect()
}
