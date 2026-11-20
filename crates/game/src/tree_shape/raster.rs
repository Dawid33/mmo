//! Rasterise a `TreeSkeleton` into blocks (spec §4.5).
//!
//! Trees are whole blocks, like Minecraft's: no voxel masks. Each skeleton
//! segment (node → parent) is a tapered capsule; a block whose centre lies in
//! it is wood, and so is every block of the segment's axis walk
//! ([`block_line`], face steps from the parent node's block to the node's), so
//! a trunk or limb thinner than a block is still an unbroken column or
//! staircase. Wood is a `Log` (levels 0–1) or a `Branch` (level ≥ 2).
//! Segments thinner than `MIN_WOOD_RADIUS` dissolve into the leaf mass. Leaf clusters fill whole
//! `Leaves` blocks: the block holding the cluster centre, plus a sphere test on
//! block centres dithered by a hash within `LEAF_DITHER_VOXELS` of the radius.
//! Leaves not face-connected to wood (through leaves) are dropped, so nothing
//! floats. Wood wins over leaves. Nothing goes below the base block (y < 0 is
//! the ground).
//!
//! Only `+ - * /`, `sqrt`, `floor` and integer math (the `noise.rs` policy).

use super::skeleton::TreeSkeleton;
use super::tree_hash64;
use crate::BLOCK_VOXELS;

/// Segments thinner than this radius (voxels) at either end are not wood.
pub const MIN_WOOD_RADIUS: i32 = 2;
/// Leaf blocks are dithered within this many voxels of a cluster's radius.
/// Half a block: with block centres on the trunk-axis lattice, a block within
/// `r + 8` of a fitted cluster stays inside the species limits.
pub const LEAF_DITHER_VOXELS: i32 = 8;
/// Skeleton level from which wood is `Branch` rather than `Log`.
pub const BRANCH_LEVEL: u8 = 2;

const BV: i32 = BLOCK_VOXELS as i32;
const SALT_DITHER: u64 = 0x1EAF;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreePart {
    Log,
    Branch,
    Leaves,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeBlock {
    /// Block offset from the tree's base block.
    pub offset: [i32; 3],
    pub part: TreePart,
    /// The limb it belongs to (`TreeSkeleton::limbs`; 0 = the trunk): wood
    /// takes the lowest limb of the segments filling it (the trunk wins at a
    /// junction), leaves the lowest limb of the clusters covering them.
    pub limb: u8,
}

#[derive(Clone, Copy)]
struct Wood {
    log: bool,
    limb: u8,
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn f3(p: [i32; 3]) -> [f64; 3] {
    [p[0] as f64, p[1] as f64, p[2] as f64]
}

fn dither01(block: [i32; 3], centre: [i32; 3]) -> f64 {
    let b =
        (block[0] as u32 as u64) | (block[1] as u32 as u64) << 21 | (block[2] as u32 as u64) << 42;
    let c = (centre[0] as u32 as u64)
        | (centre[1] as u32 as u64) << 21
        | (centre[2] as u32 as u64) << 42;
    (tree_hash64(SALT_DITHER, b, c) >> 11) as f64 / (1u64 << 53) as f64
}

/// The block holding voxel point `p` (y clamped to the base layer: y < 0 is the ground).
fn block_of(p: [i32; 3]) -> [i32; 3] {
    [
        p[0].div_euclid(BV),
        p[1].max(0).div_euclid(BV),
        p[2].div_euclid(BV),
    ]
}

/// The tree's blocks in a dense box: wood and leaf flags.
struct Grid {
    /// Block coordinates of the box's minimum corner, and its size in blocks.
    lo: [i32; 3],
    dim: [i32; 3],
    wood: Vec<Option<Wood>>,
    leaf: Vec<bool>,
    /// Per cell, the lowest limb of the leaf clusters covering it.
    leaf_limb: Vec<u8>,
}

impl Grid {
    /// Room for every block within voxel bounds `lo..=hi` (y clipped at 0).
    fn new(lo: [i32; 3], hi: [i32; 3]) -> Grid {
        let lo = [
            lo[0].div_euclid(BV),
            lo[1].max(0).div_euclid(BV),
            lo[2].div_euclid(BV),
        ];
        let dim = [0, 1, 2].map(|i| (hi[i].max(0).div_euclid(BV) - lo[i] + 1).max(1));
        let n = (dim[0] * dim[1] * dim[2]) as usize;
        Grid {
            lo,
            dim,
            wood: vec![None; n],
            leaf: vec![false; n],
            leaf_limb: vec![u8::MAX; n],
        }
    }

    /// Cell of block `b`, ordered (y, z, x) so cell order is output order.
    fn cell(&self, b: [i32; 3]) -> Option<usize> {
        let r = [b[0] - self.lo[0], b[1] - self.lo[1], b[2] - self.lo[2]];
        if (0..3).all(|i| (0..self.dim[i]).contains(&r[i])) {
            Some((r[0] + self.dim[0] * (r[2] + self.dim[2] * r[1])) as usize)
        } else {
            None
        }
    }

    fn block(&self, cell: usize) -> [i32; 3] {
        let c = cell as i32;
        let (x, z, y) = (
            c % self.dim[0],
            c / self.dim[0] % self.dim[2],
            c / (self.dim[0] * self.dim[2]),
        );
        [self.lo[0] + x, self.lo[1] + y, self.lo[2] + z]
    }

    fn is_wood(&self, b: [i32; 3]) -> bool {
        self.cell(b).is_some_and(|c| self.wood[c].is_some())
    }

    /// Make block `b` wood: `log` if any segment filling it is, of the lowest limb.
    fn mark(&mut self, b: [i32; 3], log: bool, limb: u8) {
        let cell = self.cell(b).expect("wood inside the tree's bounds");
        let w = self.wood[cell].get_or_insert(Wood { log, limb });
        w.log |= log;
        w.limb = w.limb.min(limb);
    }
}

/// The face-connected block path from block `a` to block `b`, both included:
/// `|Δx| + |Δy| + |Δz|` single-axis steps, each on the axis that is furthest
/// behind its share of the way (ties: x, then y, then z). Integer only.
fn block_line(a: [i32; 3], b: [i32; 3]) -> Vec<[i32; 3]> {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let n = [d[0].abs(), d[1].abs(), d[2].abs()];
    let mut taken = [0i32; 3];
    let mut p = a;
    let mut out = Vec::with_capacity((n[0] + n[1] + n[2] + 1) as usize);
    out.push(p);
    loop {
        // Axis i is at fraction (taken + ½) / n of its way after its next step's midpoint;
        // the smallest goes next: compare (2·taken_i + 1)·n_j < (2·taken_j + 1)·n_i.
        let mut best: Option<usize> = None;
        for i in 0..3 {
            if taken[i] < n[i] {
                let better = match best {
                    None => true,
                    Some(j) => {
                        i64::from(2 * taken[i] + 1) * i64::from(n[j])
                            < i64::from(2 * taken[j] + 1) * i64::from(n[i])
                    }
                };
                if better {
                    best = Some(i);
                }
            }
        }
        let Some(i) = best else { break };
        taken[i] += 1;
        p[i] += d[i].signum();
        out.push(p);
    }
    out
}

/// The blocks of the tapered capsule from `a` (radius `ra`) to `b` (`rb`),
/// voxels: every block whose centre lies within `r(t)` of the segment's
/// closest point `t` (radius linear along it), plus the axis walk from `a`'s
/// block to `b`'s, so even a sub-block segment leaves a connected path.
fn wood_segment(grid: &mut Grid, a: [i32; 3], b: [i32; 3], ra: i32, rb: i32, level: u8, limb: u8) {
    if ra.min(rb) < MIN_WOOD_RADIUS {
        return;
    }
    let log = level < BRANCH_LEVEL;
    let (af, bf, raf, rbf) = (f3(a), f3(b), ra as f64, rb as f64);
    let d = sub(bf, af);
    let dd = dot(d, d);
    let rmax = ra.max(rb);
    let blo = |i: usize| (a[i].min(b[i]) - rmax).div_euclid(BV);
    let bhi = |i: usize| (a[i].max(b[i]) + rmax).div_euclid(BV);
    for by in blo(1).max(0)..=bhi(1) {
        for bz in blo(2)..=bhi(2) {
            for bx in blo(0)..=bhi(0) {
                let c = [
                    (bx * BV + BV / 2) as f64,
                    (by * BV + BV / 2) as f64,
                    (bz * BV + BV / 2) as f64,
                ];
                let t = if dd > 0.0 {
                    (dot(sub(c, af), d) / dd).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let q = [af[0] + d[0] * t, af[1] + d[1] * t, af[2] + d[2] * t];
                let r = raf + (rbf - raf) * t;
                let e = sub(c, q);
                if dot(e, e) <= r * r {
                    grid.mark([bx, by, bz], log, limb);
                }
            }
        }
    }
    for p in block_line(block_of(a), block_of(b)) {
        grid.mark(p, log, limb);
    }
}

/// The tree's blocks, sorted by `offset` in (y, z, x) order.
pub fn rasterize(sk: &TreeSkeleton) -> Vec<TreeBlock> {
    // Bounds: every wood voxel and every leaf block (a cluster's own block too).
    let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
    let mut grow = |p: [i32; 3], r: i32| {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i] - r);
            hi[i] = hi[i].max(p[i] + r);
        }
    };
    for n in &sk.nodes {
        grow(n.pos, n.radius + 1);
    }
    for &(c, r) in &sk.leaves {
        grow(c, r + LEAF_DITHER_VOXELS + 1);
    }
    if sk.nodes.is_empty() {
        return Vec::new();
    }
    let mut grid = Grid::new(lo, hi);
    let limbs = sk.limbs();
    for (i, n) in sk.nodes.iter().enumerate() {
        if let Some(p) = n.parent {
            let p = &sk.nodes[p as usize];
            wood_segment(
                &mut grid, p.pos, n.pos, p.radius, n.radius, n.level, limbs[i],
            );
        }
    }
    let rim = LEAF_DITHER_VOXELS as f64;
    let mut leaves = vec![false; grid.leaf.len()];
    for (&(c, r), &node) in sk.leaves.iter().zip(&sk.leaf_nodes) {
        let limb = limbs[node as usize];
        let (cf, rf) = (f3(c), r as f64);
        let reach = r + LEAF_DITHER_VOXELS;
        let own = [
            c[0].div_euclid(BV),
            c[1].div_euclid(BV),
            c[2].div_euclid(BV),
        ];
        for by in (c[1] - reach).max(0).div_euclid(BV)..=(c[1] + reach).div_euclid(BV) {
            for bz in (c[2] - reach).div_euclid(BV)..=(c[2] + reach).div_euclid(BV) {
                for bx in (c[0] - reach).div_euclid(BV)..=(c[0] + reach).div_euclid(BV) {
                    let centre = [
                        (bx * BV + BV / 2) as f64,
                        (by * BV + BV / 2) as f64,
                        (bz * BV + BV / 2) as f64,
                    ];
                    let e = sub(centre, cf);
                    let dist = dot(e, e).sqrt();
                    let inside = [bx, by, bz] == own
                        || dist <= rf - rim
                        || (dist <= rf + rim
                            && dither01([bx, by, bz], c) < (rf + rim - dist) / (2.0 * rim));
                    if inside {
                        if let Some(cell) = grid.cell([bx, by, bz]) {
                            leaves[cell] = grid.wood[cell].is_none();
                            grid.leaf_limb[cell] = grid.leaf_limb[cell].min(limb);
                        }
                    }
                }
            }
        }
    }

    // Keep only leaves face-connected (through leaves) to wood: nothing floats.
    let faces = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    let step = |k: [i32; 3], d: &[i32; 3]| [k[0] + d[0], k[1] + d[1], k[2] + d[2]];
    let mut stack: Vec<usize> = (0..leaves.len())
        .filter(|&c| leaves[c] && faces.iter().any(|d| grid.is_wood(step(grid.block(c), d))))
        .collect();
    while let Some(c) = stack.pop() {
        if !grid.leaf[c] {
            grid.leaf[c] = true;
            let b = grid.block(c);
            for d in &faces {
                if let Some(n) = grid.cell(step(b, d)) {
                    if leaves[n] && !grid.leaf[n] {
                        stack.push(n);
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    for cell in 0..grid.wood.len() {
        let offset = grid.block(cell);
        if let Some(w) = grid.wood[cell] {
            let part = if w.log {
                TreePart::Log
            } else {
                TreePart::Branch
            };
            out.push(TreeBlock {
                offset,
                part,
                limb: w.limb,
            });
        } else if grid.leaf[cell] {
            out.push(TreeBlock {
                offset,
                part: TreePart::Leaves,
                limb: grid.leaf_limb[cell],
            });
        }
    }
    out
}
