//! Rasterise a `TreeSkeleton` into blocks (spec §4.5).
//!
//! Each skeleton segment (node → parent) is a tapered capsule. A block wholly
//! inside the wood is a plain `Log` (levels 0–1) or `Branch` (level ≥ 2); a block
//! the surface cuts through carries a 16³ voxel mask. Segments thinner than
//! `MIN_WOOD_RADIUS` dissolve into the leaf mass. Leaf clusters fill whole
//! `Leaves` blocks: the block holding the cluster centre, plus a sphere test on
//! block centres dithered by a hash within `LEAF_DITHER_VOXELS` of the radius.
//! Leaves not face-connected to wood (through leaves) are dropped, so nothing
//! floats. Wood wins over leaves. Nothing goes below the base block (y < 0 is
//! the ground).
//!
//! Only `+ - * /`, `sqrt`, `floor` and integer math (the `noise.rs` policy).

use std::collections::{BTreeMap, BTreeSet};

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
/// Centre-to-corner distance of a block, voxels (8·√3, rounded up).
const HALF_DIAGONAL: f64 = 13.86;
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
}

struct Wood {
    full: bool,
    mask: Vec<bool>,
    log: bool,
}

fn mask_index(vx: i32, vy: i32, vz: i32) -> usize {
    (vx + vy * BV + vz * BV * BV) as usize
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

/// Squared distance from `q` to segment `a + d·t`, and the clamped `t`.
fn to_segment(q: [f64; 3], a: [f64; 3], d: [f64; 3], dd: f64) -> (f64, f64) {
    let aq = sub(q, a);
    let t = if dd > 0.0 { (dot(aq, d) / dd).clamp(0.0, 1.0) } else { 0.0 };
    let e = [aq[0] - d[0] * t, aq[1] - d[1] * t, aq[2] - d[2] * t];
    (dot(e, e), t)
}

/// Key ordering blocks by (y, z, x).
fn key(o: [i32; 3]) -> [i32; 3] {
    [o[1], o[2], o[0]]
}

fn unkey(k: [i32; 3]) -> [i32; 3] {
    [k[2], k[0], k[1]]
}

fn dither01(block: [i32; 3], centre: [i32; 3]) -> f64 {
    let b = (block[0] as u32 as u64) | (block[1] as u32 as u64) << 21 | (block[2] as u32 as u64) << 42;
    let c = (centre[0] as u32 as u64) | (centre[1] as u32 as u64) << 21 | (centre[2] as u32 as u64) << 42;
    (hash64(SALT_DITHER, b, c) >> 11) as f64 / (1u64 << 53) as f64
}

fn wood_segment(wood: &mut BTreeMap<[i32; 3], Wood>, a: [i32; 3], b: [i32; 3], ra: i32, rb: i32, level: u8) {
    if ra.min(rb) < MIN_WOOD_RADIUS {
        return;
    }
    let (ra, rb) = (ra as f64, rb as f64);
    let (rmin, rmax) = (ra.min(rb), ra.max(rb));
    let (af, d) = (f3(a), sub(f3(b), f3(a)));
    let dd = dot(d, d);
    let reach = rmax.ceil() as i32 + 1;
    let lo = [a[0].min(b[0]) - reach, (a[1].min(b[1]) - reach).max(0), a[2].min(b[2]) - reach];
    let hi = [a[0].max(b[0]) + reach, a[1].max(b[1]) + reach, a[2].max(b[2]) + reach];
    let log = level < BRANCH_LEVEL;
    for by in lo[1].div_euclid(BV)..=hi[1].div_euclid(BV) {
        for bz in lo[2].div_euclid(BV)..=hi[2].div_euclid(BV) {
            for bx in lo[0].div_euclid(BV)..=hi[0].div_euclid(BV) {
                let base = [bx * BV, by * BV, bz * BV];
                let centre = [(base[0] + BV / 2) as f64, (base[1] + BV / 2) as f64, (base[2] + BV / 2) as f64];
                let (d2, _) = to_segment(centre, af, d, dd);
                let dist = d2.sqrt();
                if dist > rmax + HALF_DIAGONAL {
                    continue;
                }
                let k = key([bx, by, bz]);
                if dist + HALF_DIAGONAL <= rmin {
                    let w = wood.entry(k).or_insert_with(|| Wood { full: false, mask: Vec::new(), log: false });
                    w.full = true;
                    w.log |= log;
                    continue;
                }
                if wood.get(&k).is_some_and(|w| w.full) {
                    wood.get_mut(&k).unwrap().log |= log;
                    continue;
                }
                let mut hits = Vec::new();
                for vz in 0..BV {
                    for vy in 0..BV {
                        for vx in 0..BV {
                            let q =
                                [(base[0] + vx) as f64 + 0.5, (base[1] + vy) as f64 + 0.5, (base[2] + vz) as f64 + 0.5];
                            let (d2, t) = to_segment(q, af, d, dd);
                            let r = ra + (rb - ra) * t;
                            if d2 <= r * r {
                                hits.push(mask_index(vx, vy, vz));
                            }
                        }
                    }
                }
                if hits.is_empty() {
                    continue;
                }
                let w = wood.entry(k).or_insert_with(|| Wood {
                    full: false,
                    mask: vec![false; BLOCK_MASK_LEN],
                    log: false,
                });
                w.log |= log;
                for i in hits {
                    w.mask[i] = true;
                }
            }
        }
    }
}

/// The tree's blocks, sorted by `offset` in (y, z, x) order.
pub fn rasterize(sk: &TreeSkeleton) -> Vec<TreeBlock> {
    let mut wood = BTreeMap::new();
    for n in &sk.nodes {
        if let Some(p) = n.parent {
            let p = &sk.nodes[p as usize];
            wood_segment(&mut wood, p.pos, n.pos, p.radius, n.radius, n.level);
        }
    }
    let rim = LEAF_DITHER_VOXELS as f64;
    let mut leaves = BTreeSet::new();
    for &(c, r) in &sk.leaves {
        let (cf, rf) = (f3(c), r as f64);
        let reach = r + LEAF_DITHER_VOXELS;
        let lo = [c[0] - reach, (c[1] - reach).max(0), c[2] - reach];
        let hi = [c[0] + reach, c[1] + reach, c[2] + reach];
        for by in lo[1].div_euclid(BV)..=hi[1].div_euclid(BV) {
            for bz in lo[2].div_euclid(BV)..=hi[2].div_euclid(BV) {
                for bx in lo[0].div_euclid(BV)..=hi[0].div_euclid(BV) {
                    let centre = [(bx * BV + BV / 2) as f64, (by * BV + BV / 2) as f64, (bz * BV + BV / 2) as f64];
                    let e = sub(centre, cf);
                    let dist = dot(e, e).sqrt();
                    let own = [bx, by, bz] == [c[0].div_euclid(BV), c[1].div_euclid(BV), c[2].div_euclid(BV)];
                    let inside = own
                        || dist <= rf - rim
                        || (dist <= rf + rim && dither01([bx, by, bz], c) < (rf + rim - dist) / (2.0 * rim));
                    if inside {
                        leaves.insert(key([bx, by, bz]));
                    }
                }
            }
        }
    }

    // Keep only leaves face-connected (through leaves) to wood: nothing floats.
    let faces = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];
    let step = |k: [i32; 3], d: &[i32; 3]| [k[0] + d[0], k[1] + d[1], k[2] + d[2]];
    let mut kept = BTreeSet::new();
    let mut stack: Vec<[i32; 3]> = leaves
        .iter()
        .copied()
        .filter(|&k| !wood.contains_key(&k) && faces.iter().any(|d| wood.contains_key(&step(k, d))))
        .collect();
    while let Some(k) = stack.pop() {
        if kept.insert(k) {
            for d in &faces {
                let n = step(k, d);
                if leaves.contains(&n) && !wood.contains_key(&n) && !kept.contains(&n) {
                    stack.push(n);
                }
            }
        }
    }
    let leaves = kept;

    let mut out = Vec::with_capacity(wood.len() + leaves.len());
    let mut wood = wood.into_iter().peekable();
    let mut leaves = leaves.into_iter().peekable();
    loop {
        let take_wood = match (wood.peek(), leaves.peek()) {
            (None, None) => break,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some((wk, _)), Some(lk)) => wk <= lk,
        };
        if take_wood {
            let (k, w) = wood.next().unwrap();
            if leaves.peek() == Some(&k) {
                leaves.next();
            }
            let part = if w.log { TreePart::Log } else { TreePart::Branch };
            let chisel = if w.full || w.mask.iter().all(|&v| v) { None } else { Some(w.mask) };
            out.push(TreeBlock { offset: unkey(k), part, chisel });
        } else {
            let k = leaves.next().unwrap();
            out.push(TreeBlock { offset: unkey(k), part: TreePart::Leaves, chisel: None });
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
    #[ignore = "slow: >100ms — run via just test-full"]
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
                assert!(top * 4 >= sp.max_height_blocks * 3, "{} seed {seed}: only {top} tall", sp.name);
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
    fn wood_wins_over_leaves() {
        let sk = TreeSkeleton {
            nodes: vec![
                super::super::skeleton::Node { parent: None, pos: [8, 0, 8], radius: 8, level: 0 },
                super::super::skeleton::Node { parent: Some(0), pos: [8, 64, 8], radius: 8, level: 0 },
            ],
            leaves: vec![([8, 40, 8], 40)],
        };
        let blocks = rasterize(&sk);
        let at = |o: [i32; 3]| blocks.iter().find(|b| b.offset == o).map(|b| b.part);
        assert_eq!(at([0, 2, 0]), Some(TreePart::Log));
        assert_eq!(at([1, 2, 0]), Some(TreePart::Leaves));
        assert!(at([0, 0, 0]).is_some() && at([0, -1, 0]).is_none());
    }
}
