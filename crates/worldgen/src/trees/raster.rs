//! Rasterise a `TreeSkeleton` into blocks (spec §4.5).
//!
//! Each skeleton segment (node → parent) is a tapered capsule, filled as voxel
//! row spans. A block wholly inside the wood is a plain `Log` (levels 0–1) or
//! `Branch` (level ≥ 2); a block the surface cuts through carries a 16³ voxel mask. Segments thinner than
//! `MIN_WOOD_RADIUS` dissolve into the leaf mass. Leaf clusters fill whole
//! `Leaves` blocks: the block holding the cluster centre, plus a sphere test on
//! block centres dithered by a hash within `LEAF_DITHER_VOXELS` of the radius.
//! Leaves not face-connected to wood (through leaves) are dropped, so nothing
//! floats. Wood wins over leaves. Nothing goes below the base block (y < 0 is
//! the ground).
//!
//! Only `+ - * /`, `sqrt`, `floor` and integer math (the `noise.rs` policy).

use super::placement::hash64;
use super::skeleton::TreeSkeleton;
use game::BLOCK_VOXELS;

/// Segments thinner than this radius (voxels) at either end are not wood.
pub const MIN_WOOD_RADIUS: i32 = 2;
/// Leaf blocks are dithered within this many voxels of a cluster's radius.
/// Half a block: with block centres on the trunk-axis lattice, a block within
/// `r + 8` of a fitted cluster stays inside the species limits.
pub const LEAF_DITHER_VOXELS: i32 = 8;
/// Skeleton level from which wood is `Branch` rather than `Log`.
pub const BRANCH_LEVEL: u8 = 2;
/// Voxels in one block's mask.
pub const BLOCK_MASK_LEN: usize = BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS;

const BV: i32 = BLOCK_VOXELS as i32;
/// Sphere spacing along a wood segment, as a fraction of its thinner radius (at
/// least 1 voxel): the scallop between spheres is 3% of the radius, under half a
/// voxel on the thickest trunk.
const SWEEP_STEP_FRAC: f64 = 0.5;
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
    /// Solid voxels of a partial wood block, `BLOCK_MASK_LEN` entries, x fastest,
    /// then y, then z (`ChiselData::from_fn`'s order); `None` = the whole block.
    pub chisel: Option<Vec<bool>>,
    /// The limb it belongs to (`TreeSkeleton::limbs`; 0 = the trunk): wood
    /// takes the lowest limb of the segments filling it (the trunk wins at a
    /// junction), leaves the lowest limb of the clusters covering them.
    pub limb: u8,
}

struct Wood {
    /// One bit per voxel: `rows[vy + vz·16]` bit `vx`.
    rows: [u16; BLOCK_VOXELS * BLOCK_VOXELS],
    log: bool,
    limb: u8,
}

impl Wood {
    fn solid(&self) -> usize {
        self.rows.iter().map(|r| r.count_ones() as usize).sum()
    }

    /// The `TreeBlock::chisel` layout: x fastest, then y, then z — the order of
    /// `rows`, so it is a straight expansion of the bits.
    fn mask(&self) -> Vec<bool> {
        let mut m = Vec::with_capacity(BLOCK_MASK_LEN);
        for &row in &self.rows {
            m.extend((0..BV).map(|vx| row >> vx & 1 == 1));
        }
        m
    }
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

/// Key ordering blocks by (y, z, x).
#[cfg(test)]
fn key(o: [i32; 3]) -> [i32; 3] {
    [o[1], o[2], o[0]]
}

fn dither01(block: [i32; 3], centre: [i32; 3]) -> f64 {
    let b = (block[0] as u32 as u64) | (block[1] as u32 as u64) << 21 | (block[2] as u32 as u64) << 42;
    let c = (centre[0] as u32 as u64) | (centre[1] as u32 as u64) << 21 | (centre[2] as u32 as u64) << 42;
    (hash64(SALT_DITHER, b, c) >> 11) as f64 / (1u64 << 53) as f64
}

/// The tree's blocks in a dense box: wood (bit masks) and leaf flags.
struct Grid {
    /// Block coordinates of the box's minimum corner, and its size in blocks.
    lo: [i32; 3],
    dim: [i32; 3],
    /// Per cell: 0 = no wood, else 1 + index into `woods`.
    wood: Vec<u32>,
    woods: Vec<Wood>,
    leaf: Vec<bool>,
    /// Per cell, the lowest limb of the leaf clusters covering it.
    leaf_limb: Vec<u8>,
}

impl Grid {
    /// Room for every block within voxel bounds `lo..=hi` (y clipped at 0).
    fn new(lo: [i32; 3], hi: [i32; 3]) -> Grid {
        let lo = [lo[0].div_euclid(BV), lo[1].max(0).div_euclid(BV), lo[2].div_euclid(BV)];
        let dim = [0, 1, 2].map(|i| (hi[i].max(0).div_euclid(BV) - lo[i] + 1).max(1));
        let n = (dim[0] * dim[1] * dim[2]) as usize;
        Grid { lo, dim, wood: vec![0; n], woods: Vec::new(), leaf: vec![false; n], leaf_limb: vec![u8::MAX; n] }
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
        let (x, z, y) = (c % self.dim[0], c / self.dim[0] % self.dim[2], c / (self.dim[0] * self.dim[2]));
        [self.lo[0] + x, self.lo[1] + y, self.lo[2] + z]
    }

    fn is_wood(&self, b: [i32; 3]) -> bool {
        self.cell(b).is_some_and(|c| self.wood[c] != 0)
    }

    /// Mark voxels `x0..=x1` of voxel row (y, z) solid.
    fn fill_row(&mut self, x0: i32, x1: i32, y: i32, z: i32, log: bool, limb: u8) {
        let (by, bz, vy, vz) = (y.div_euclid(BV), z.div_euclid(BV), y.rem_euclid(BV), z.rem_euclid(BV));
        let mut x = x0;
        while x <= x1 {
            let bx = x.div_euclid(BV);
            let end = x1.min(bx * BV + BV - 1);
            let cell = self.cell([bx, by, bz]).expect("wood inside the tree's bounds");
            if self.wood[cell] == 0 {
                self.woods.push(Wood { rows: [0; BLOCK_VOXELS * BLOCK_VOXELS], log: false, limb });
                self.wood[cell] = self.woods.len() as u32;
            }
            let w = &mut self.woods[self.wood[cell] as usize - 1];
            w.log |= log;
            w.limb = w.limb.min(limb);
            let (a, b) = (x - bx * BV, end - bx * BV);
            w.rows[(vy + vz * BV) as usize] |= ((1u32 << (b + 1)) - (1u32 << a)) as u16;
            x = end + 1;
        }
    }
}

/// The voxels of the tapered capsule from `a` (radius `ra`) to `b` (`rb`): the
/// union of spheres swept along the segment with linearly varying radius,
/// sampled every `SWEEP_STEP_FRAC` of the thinner radius (at least 1 voxel).
/// Filled a row at a time: the shape is convex, so each x-row crosses it in one
/// span, the hull of the spheres' spans.
fn wood_segment(grid: &mut Grid, a: [i32; 3], b: [i32; 3], ra: i32, rb: i32, level: u8, limb: u8) {
    if ra.min(rb) < MIN_WOOD_RADIUS {
        return;
    }
    let (af, bf, ra, rb) = (f3(a), f3(b), ra as f64, rb as f64);
    let d = sub(bf, af);
    let len = dot(d, d).sqrt();
    let n = ((len / (ra.min(rb) * SWEEP_STEP_FRAC).max(1.0)).ceil() as usize).max(1);
    let spheres: Vec<([f64; 3], f64)> = (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            ([af[0] + d[0] * t, af[1] + d[1] * t, af[2] + d[2] * t], ra + (rb - ra) * t)
        })
        .collect();
    let rmax = ra.max(rb);
    let lo = |i: usize| (af[i].min(bf[i]) - rmax).floor() as i32;
    let hi = |i: usize| (af[i].max(bf[i]) + rmax).ceil() as i32;
    let log = level < BRANCH_LEVEL;
    // Per (y, z) row of the bounding box, the x-span the spheres cover.
    let (y0, z0) = (lo(1).max(0), lo(2));
    let (ny, nz) = ((hi(1) - y0 + 1).max(0) as usize, (hi(2) - z0 + 1) as usize);
    let mut rows = vec![(f64::INFINITY, f64::NEG_INFINITY); ny * nz];
    for &(c, r) in &spheres {
        let (zs, ze) = (((c[2] - r - 0.5).ceil() as i32).max(z0), (c[2] + r - 0.5).floor() as i32);
        let (ys, ye) = (((c[1] - r - 0.5).ceil() as i32).max(y0), (c[1] + r - 0.5).floor() as i32);
        for z in zs..=ze {
            let qz = z as f64 + 0.5 - c[2];
            for y in ys..=ye {
                let qy = y as f64 + 0.5 - c[1];
                let h = r * r - qy * qy - qz * qz;
                if h >= 0.0 {
                    let dx = h.sqrt();
                    let row = &mut rows[(y - y0) as usize + (z - z0) as usize * ny];
                    row.0 = row.0.min(c[0] - dx);
                    row.1 = row.1.max(c[0] + dx);
                }
            }
        }
    }
    for (k, &(x0, x1)) in rows.iter().enumerate() {
        // Voxel centres `x + 0.5` inside [x0, x1].
        if x0 <= x1 {
            let (vx0, vx1) = ((x0 - 0.5).ceil() as i32, (x1 - 0.5).floor() as i32);
            if vx0 <= vx1 {
                grid.fill_row(vx0, vx1, y0 + (k % ny) as i32, z0 + (k / ny) as i32, log, limb);
            }
        }
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
            wood_segment(&mut grid, p.pos, n.pos, p.radius, n.radius, n.level, limbs[i]);
        }
    }
    let rim = LEAF_DITHER_VOXELS as f64;
    let mut leaves = vec![false; grid.leaf.len()];
    for (&(c, r), &node) in sk.leaves.iter().zip(&sk.leaf_nodes) {
        let limb = limbs[node as usize];
        let (cf, rf) = (f3(c), r as f64);
        let reach = r + LEAF_DITHER_VOXELS;
        let own = [c[0].div_euclid(BV), c[1].div_euclid(BV), c[2].div_euclid(BV)];
        for by in (c[1] - reach).max(0).div_euclid(BV)..=(c[1] + reach).div_euclid(BV) {
            for bz in (c[2] - reach).div_euclid(BV)..=(c[2] + reach).div_euclid(BV) {
                for bx in (c[0] - reach).div_euclid(BV)..=(c[0] + reach).div_euclid(BV) {
                    let centre = [(bx * BV + BV / 2) as f64, (by * BV + BV / 2) as f64, (bz * BV + BV / 2) as f64];
                    let e = sub(centre, cf);
                    let dist = dot(e, e).sqrt();
                    let inside = [bx, by, bz] == own
                        || dist <= rf - rim
                        || (dist <= rf + rim && dither01([bx, by, bz], c) < (rf + rim - dist) / (2.0 * rim));
                    if inside && let Some(cell) = grid.cell([bx, by, bz]) {
                        leaves[cell] = grid.wood[cell] == 0;
                        grid.leaf_limb[cell] = grid.leaf_limb[cell].min(limb);
                    }
                }
            }
        }
    }

    // Keep only leaves face-connected (through leaves) to wood: nothing floats.
    let faces = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];
    let step = |k: [i32; 3], d: &[i32; 3]| [k[0] + d[0], k[1] + d[1], k[2] + d[2]];
    let mut stack: Vec<usize> = (0..leaves.len())
        .filter(|&c| leaves[c] && faces.iter().any(|d| grid.is_wood(step(grid.block(c), d))))
        .collect();
    while let Some(c) = stack.pop() {
        if !grid.leaf[c] {
            grid.leaf[c] = true;
            let b = grid.block(c);
            for d in &faces {
                if let Some(n) = grid.cell(step(b, d))
                    && leaves[n]
                    && !grid.leaf[n]
                {
                    stack.push(n);
                }
            }
        }
    }

    let mut out = Vec::with_capacity(grid.woods.len() * 3);
    for cell in 0..grid.wood.len() {
        let offset = grid.block(cell);
        if grid.wood[cell] != 0 {
            let w = &grid.woods[grid.wood[cell] as usize - 1];
            let part = if w.log { TreePart::Log } else { TreePart::Branch };
            let chisel = if w.solid() == BLOCK_MASK_LEN { None } else { Some(w.mask()) };
            out.push(TreeBlock { offset, part, chisel, limb: w.limb });
        } else if grid.leaf[cell] {
            out.push(TreeBlock { offset, part: TreePart::Leaves, chisel: None, limb: grid.leaf_limb[cell] });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trees::skeleton::skeleton;
    use crate::trees::{MAX_AGE, SPECIES};
    use std::collections::HashSet;

    fn trees() -> impl Iterator<Item = (usize, u64, u16, Vec<TreeBlock>)> {
        (0..SPECIES.len()).flat_map(|s| {
            [(1u64, 5u16), (2, 30), (3, 60), (4, MAX_AGE), (77, MAX_AGE)]
                .into_iter()
                .map(move |(seed, age)| (s, seed, age, rasterize(&skeleton(&SPECIES[s], seed, age))))
        })
    }

    #[test]
    fn trunk_is_connected_to_base() {
        for (s, seed, age, blocks) in trees() {
            let wood: HashSet<[i32; 3]> =
                blocks.iter().filter(|b| b.part != TreePart::Leaves).map(|b| b.offset).collect();
            assert!(wood.contains(&[0, 0, 0]), "{} seed {seed} age {age}: no base block", SPECIES[s].name);
            let mut seen = HashSet::from([[0, 0, 0]]);
            let mut stack = vec![[0, 0, 0]];
            while let Some(p) = stack.pop() {
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            let q = [p[0] + dx, p[1] + dy, p[2] + dz];
                            if wood.contains(&q) && seen.insert(q) {
                                stack.push(q);
                            }
                        }
                    }
                }
            }
            assert_eq!(seen.len(), wood.len(), "{} seed {seed} age {age}: floating wood", SPECIES[s].name);
        }
    }

    #[test]
    fn height_within_species_max() {
        for sp in &SPECIES {
            for seed in 0..12 {
                let blocks = rasterize(&skeleton(sp, seed, MAX_AGE));
                for b in &blocks {
                    let [x, y, z] = b.offset;
                    assert!((0..sp.max_height_blocks).contains(&y), "{} seed {seed}: {:?}", sp.name, b.offset);
                    // Leaf dither may reach one block past the crown radius; wood never.
                    let r = sp.crown_radius_blocks + if b.part == TreePart::Leaves { 1 } else { 0 };
                    assert!(x.abs() <= r && z.abs() <= r, "{} seed {seed}: {:?} {:?}", sp.name, b.offset, b.part);
                }
                let top = blocks.iter().map(|b| b.offset[1]).max().unwrap();
                // A mature tree uses most of its species' height (a broad oak less).
                assert!(top * 3 >= sp.max_height_blocks * 2, "{} seed {seed}: only {top} tall", sp.name);
            }
        }
    }

    /// Every block of a mature tree is reachable by the sim's chop flood
    /// (`game::MAX_TREE_BLOCKS`), with margin, and so is the whole box a
    /// species may fill.
    #[test]
    fn mature_trees_fit_the_flood_bound() {
        for sp in &SPECIES {
            let r = sp.crown_radius_blocks + crate::trees::LEAF_REACH_PAST_CROWN_BLOCKS;
            let bound = ((2 * r + 1) * (2 * r + 1) * sp.max_height_blocks) as usize;
            assert!(bound < game::MAX_TREE_BLOCKS, "{}: box of {bound} blocks", sp.name);
            for seed in 0..12 {
                let n = rasterize(&skeleton(sp, seed, MAX_AGE)).len();
                assert!(n * 2 <= game::MAX_TREE_BLOCKS, "{} seed {seed}: {n} blocks", sp.name);
            }
        }
    }

    #[test]
    fn pine_is_taller_than_wide() {
        let pine = SPECIES.iter().find(|s| s.name == "pine").unwrap();
        for seed in 0..3 {
            let blocks = rasterize(&skeleton(pine, seed, MAX_AGE));
            let ext = |i: usize| {
                let v = blocks.iter().map(|b| b.offset[i]);
                v.clone().max().unwrap() - v.min().unwrap() + 1
            };
            assert!(ext(1) > 2 * ext(0).max(ext(2)), "seed {seed}: {} tall, {}×{} wide", ext(1), ext(0), ext(2));
        }
    }

    #[test]
    fn blocks_are_sorted_unique_and_well_formed() {
        let blocks = rasterize(&skeleton(&SPECIES[0], 11, 80));
        assert!(blocks.windows(2).all(|w| key(w[0].offset) < key(w[1].offset)));
        assert_eq!(blocks.iter().find(|b| b.offset == [0, 0, 0]).map(|b| b.part), Some(TreePart::Log));
        assert!(blocks.iter().all(|b| b.offset[1] >= 0));
        let mut partial = 0;
        for b in &blocks {
            if let Some(m) = &b.chisel {
                assert_ne!(b.part, TreePart::Leaves);
                assert_eq!(m.len(), BLOCK_MASK_LEN);
                assert!(m.iter().any(|&v| v) && !m.iter().all(|&v| v));
                partial += 1;
            }
        }
        assert!(partial > 0 && blocks.iter().any(|b| b.part == TreePart::Leaves));
    }

    #[test]
    fn blocks_carry_their_limb() {
        for (s, seed, age, blocks) in trees() {
            let name = SPECIES[s].name;
            assert!(blocks.iter().all(|b| b.limb <= game::MAX_TREE_LIMB), "{name} seed {seed} age {age}");
            assert_eq!(blocks.iter().find(|b| b.offset == [0, 0, 0]).map(|b| b.limb), Some(0), "{name}: the base is trunk");
            if age == MAX_AGE {
                assert!(blocks.iter().any(|b| b.limb > 0 && b.part != TreePart::Leaves), "{name} seed {seed}: limb wood");
                assert!(blocks.iter().any(|b| b.limb > 0 && b.part == TreePart::Leaves), "{name} seed {seed}: limb leaves");
            }
        }
    }

    #[test]
    fn wood_wins_over_leaves() {
        let sk = TreeSkeleton {
            nodes: vec![
                super::super::skeleton::Node { parent: None, pos: [8, 0, 8], radius: 8, level: 0 },
                super::super::skeleton::Node { parent: Some(0), pos: [8, 64, 8], radius: 8, level: 0 },
            ],
            leaves: vec![([8, 40, 8], 40)],
            leaf_nodes: vec![1],
        };
        let blocks = rasterize(&sk);
        let at = |o: [i32; 3]| blocks.iter().find(|b| b.offset == o).map(|b| b.part);
        assert_eq!(at([0, 2, 0]), Some(TreePart::Log));
        assert_eq!(at([1, 2, 0]), Some(TreePart::Leaves));
        assert!(at([0, 0, 0]).is_some() && at([0, -1, 0]).is_none());
    }
}
