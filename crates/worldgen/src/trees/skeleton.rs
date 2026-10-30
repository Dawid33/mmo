//! Parametric tree skeletons (spec §4.4): a simplified Weber & Penn recursive
//! skeleton — a trunk, limbs on it, branches on the limbs — driven by per-species
//! `ShapeParams` and a per-tree seed.
//!
//! Determinism: only `+ - * /`, `sqrt`, `floor` and integer math (the `noise.rs`
//! policy); angles are indices into `SIN_TABLE`, never `f64::sin`.
//!
//! Age: the topology is that of the full-grown tree. Node `i` exists once
//! `age >= birth(i)`; nodes are ordered by birth so a younger skeleton is a
//! prefix of an older one (same indices, same parents). Lengths and radii scale
//! with `growth(age)`, so a growing tree keeps its branches.

use super::placement::{MAX_AGE, hash64};
use super::species::Species;
use game::BLOCK_VOXELS;

/// Fixed-point scale of `SIN_TABLE`.
pub const SIN_SCALE: i32 = 1 << 14;
/// Angle units per turn: angles are `SIN_TABLE` indices, 64 = a right angle.
pub const ANGLE_TURN: i32 = 256;
/// `sin(2π i / 256) · 2^14`, rounded.
#[rustfmt::skip]
pub const SIN_TABLE: [i32; 256] = [
    0, 402, 804, 1205, 1606, 2006, 2404, 2801, 3196, 3590, 3981, 4370, 4756, 5139, 5520, 5897,
    6270, 6639, 7005, 7366, 7723, 8076, 8423, 8765, 9102, 9434, 9760, 10080, 10394, 10702, 11003, 11297,
    11585, 11866, 12140, 12406, 12665, 12916, 13160, 13395, 13623, 13842, 14053, 14256, 14449, 14635, 14811, 14978,
    15137, 15286, 15426, 15557, 15679, 15791, 15893, 15986, 16069, 16143, 16207, 16261, 16305, 16340, 16364, 16379,
    16384, 16379, 16364, 16340, 16305, 16261, 16207, 16143, 16069, 15986, 15893, 15791, 15679, 15557, 15426, 15286,
    15137, 14978, 14811, 14635, 14449, 14256, 14053, 13842, 13623, 13395, 13160, 12916, 12665, 12406, 12140, 11866,
    11585, 11297, 11003, 10702, 10394, 10080, 9760, 9434, 9102, 8765, 8423, 8076, 7723, 7366, 7005, 6639,
    6270, 5897, 5520, 5139, 4756, 4370, 3981, 3590, 3196, 2801, 2404, 2006, 1606, 1205, 804, 402,
    0, -402, -804, -1205, -1606, -2006, -2404, -2801, -3196, -3590, -3981, -4370, -4756, -5139, -5520, -5897,
    -6270, -6639, -7005, -7366, -7723, -8076, -8423, -8765, -9102, -9434, -9760, -10080, -10394, -10702, -11003, -11297,
    -11585, -11866, -12140, -12406, -12665, -12916, -13160, -13395, -13623, -13842, -14053, -14256, -14449, -14635, -14811, -14978,
    -15137, -15286, -15426, -15557, -15679, -15791, -15893, -15986, -16069, -16143, -16207, -16261, -16305, -16340, -16364, -16379,
    -16384, -16379, -16364, -16340, -16305, -16261, -16207, -16143, -16069, -15986, -15893, -15791, -15679, -15557, -15426, -15286,
    -15137, -14978, -14811, -14635, -14449, -14256, -14053, -13842, -13623, -13395, -13160, -12916, -12665, -12406, -12140, -11866,
    -11585, -11297, -11003, -10702, -10394, -10080, -9760, -9434, -9102, -8765, -8423, -8076, -7723, -7366, -7005, -6639,
    -6270, -5897, -5520, -5139, -4756, -4370, -3981, -3590, -3196, -2801, -2404, -2006, -1606, -1205, -804, -402,
];

/// Length of one skeleton segment, voxels (branches bend between segments).
pub const SEGMENT_VOXELS: f64 = 20.0;
/// Most segments in one branch.
pub const MAX_SEGMENTS: usize = 16;
/// A branch's tip radius as a fraction of its base radius.
pub const TIP_RADIUS_FRAC: f64 = 0.35;
/// Spacing of sub-branches along their parent, voxels.
pub const CHILD_SPACING_VOXELS: f64 = 18.0;
/// Most sub-branches on one branch.
pub const MAX_CHILDREN: usize = 7;
/// Sub-branches sit on the outer part of their parent, from this fraction on.
pub const CHILD_START_FRAC: f64 = 0.3;
/// A sub-branch's base radius as a fraction of its parent's radius there.
pub const CHILD_RADIUS_FRAC: f64 = 0.6;
/// A tip cluster sits this fraction of its radius (at least `MIN_LEAF_RADIUS`)
/// beyond the branch end.
pub const TIP_LEAF_OFFSET: f64 = 0.5;
/// Golden angle in `SIN_TABLE` units (137.5°): spiral limb phyllotaxis.
pub const GOLDEN_ANGLE: i32 = 98;
/// Ages at which limbs (level 1) appear, and the extra delay per deeper level.
pub const LIMB_BIRTH: (i32, i32) = (4, 30);
pub const CHILD_BIRTH_DELAY: (i32, i32) = (4, 14);
/// `growth(age)` = age·(MAX_AGE + K) / (MAX_AGE·(age + K)), at least `MIN_GROWTH`:
/// fast early growth that saturates at 1 at `MAX_AGE`.
pub const GROWTH_K: f64 = 40.0;
pub const MIN_GROWTH: f64 = 0.08;
/// Floors so a sapling is still a visible stick with a tuft of leaves, voxels
/// (nothing is floored above its full-grown size).
pub const MIN_TRUNK_RADIUS: i32 = 2;
pub const MIN_LEAF_RADIUS: i32 = 12;

/// The crown envelope: limb length by height in the crown (Weber & Penn's shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrownShape {
    /// Longest at the bottom (spruce / fir).
    Conical,
    /// Longest at 60% up, short at both ends (birch).
    Flame,
    /// Nearly even, a little shorter towards the top: a broad dome whose upper
    /// limbs are steeper rather than shorter (oak).
    Even,
}

/// Per-species shape numbers (pinned in `SPECIES`). Lengths in voxels unless named
/// `_blocks`; angles in `SIN_TABLE` units (64 = 90°); percentages as integers.
#[derive(Clone, Copy, Debug)]
pub struct ShapeParams {
    /// Trunk length of a full-grown tree, blocks; the seed picks within the range.
    pub trunk_height_blocks: (i32, i32),
    /// Trunk radius at the ground of a full-grown tree.
    pub trunk_radius: i32,
    /// Trunk radius at the top, % of the base radius.
    pub taper_pct: i32,
    /// Lean of the trunk from vertical, at most.
    pub trunk_lean: i32,
    /// Level-1 limbs on the trunk (per whorl when `whorl_spacing > 0`).
    pub limbs: (i32, i32),
    /// Vertical spacing of limb whorls; 0 = limbs spiral at the golden angle.
    pub whorl_spacing: i32,
    /// The crown starts at this % of the trunk length.
    pub limb_start_pct: i32,
    /// Elevation of a limb at the trunk (negative droops): (lowest limb, top limb),
    /// interpolated by height in the crown.
    pub limb_angle: (i32, i32),
    /// Elevation change per segment along a branch (+ sweeps up, − droops).
    pub limb_bend: i32,
    /// Longest limb, % of the trunk length (scaled by `crown`).
    pub limb_length_pct: i32,
    /// Limb base radius, % of the trunk radius where it attaches.
    pub limb_radius_pct: i32,
    pub crown: CrownShape,
    /// Recursion depth below the trunk, 1..=3.
    pub branch_levels: u8,
    /// Azimuth offset of a sub-branch from its parent's heading.
    pub child_angle: i32,
    /// Sub-branch length, % of the parent's length.
    pub child_length_pct: i32,
    /// Leaf cluster radius of a full-grown tree.
    pub cluster_radius: i32,
    /// Leaf clusters along the trunk inside the crown too (dense conifer core).
    pub trunk_leaves: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// Index of the parent node; `None` for the trunk base.
    pub parent: Option<u16>,
    /// Voxels from the base block's minimum corner.
    pub pos: [i32; 3],
    pub radius: i32,
    /// 0 trunk, 1 limb, 2 branch, 3 twig.
    pub level: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSkeleton {
    pub nodes: Vec<Node>,
    /// Leaf cluster centre and radius, voxels.
    pub leaves: Vec<([i32; 3], i32)>,
}

const SALT_SHAPE: u64 = 0x7EE5_5EED;
const BV: f64 = BLOCK_VOXELS as f64;
/// The trunk stands on the centre of its base block.
const AXIS: f64 = BV / 2.0;

fn sin(a: i32) -> f64 {
    SIN_TABLE[(a & (ANGLE_TURN - 1)) as usize] as f64 / SIN_SCALE as f64
}

fn cos(a: i32) -> f64 {
    sin(a + ANGLE_TURN / 4)
}

/// Unit vector of azimuth `az` and elevation `el`.
fn dir(az: i32, el: i32) -> [f64; 3] {
    [cos(el) * cos(az), sin(el), cos(el) * sin(az)]
}

/// How much a tree of `age` has grown, `MIN_GROWTH..=1`.
pub fn growth(age: u16) -> f64 {
    let a = age.min(MAX_AGE) as f64;
    let m = MAX_AGE as f64;
    (a * (m + GROWTH_K) / (m * (a + GROWTH_K))).max(MIN_GROWTH)
}

/// Limb length factor at height `t` (0 crown base .. 1 top) of the crown.
fn crown_ratio(shape: CrownShape, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match shape {
        CrownShape::Conical => 0.05 + 0.95 * (1.0 - t),
        CrownShape::Even => 0.6 + 0.4 * (1.0 - t),
        CrownShape::Flame => {
            if t <= 0.6 {
                0.3 + 0.7 * t / 0.6
            } else {
                0.15 + 0.85 * (1.0 - t) / 0.4
            }
        }
    }
}

struct Rng {
    seed: u64,
    n: u64,
}

impl Rng {
    fn next(&mut self) -> u64 {
        self.n += 1;
        hash64(self.seed, self.n, SALT_SHAPE)
    }
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform integer in `lo..=hi`.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1).max(1) as u64) as i32
    }
    /// Uniform in [-amp, amp].
    fn jitter(&mut self, amp: f64) -> f64 {
        (self.unit() * 2.0 - 1.0) * amp
    }
    /// 1 ± `amp`.
    fn scale(&mut self, amp: f64) -> f64 {
        1.0 + self.jitter(amp)
    }
}

struct Proto {
    parent: Option<usize>,
    pos: [f64; 3],
    radius: f64,
    level: u8,
    birth: i32,
}

struct ProtoLeaf {
    /// The cluster exists while this node does.
    node: usize,
    centre: [f64; 3],
    radius: f64,
    /// Unit heading of a tip cluster (zero otherwise): the cluster sits
    /// `tip_offset(radius)` beyond the branch end along it.
    out: [f64; 3],
}

/// How far beyond its branch end a tip cluster of `radius` sits, voxels: enough
/// that a sapling's tuft lands in the block above its stick.
fn tip_offset(radius: f64) -> f64 {
    (radius * TIP_LEAF_OFFSET).max(MIN_LEAF_RADIUS as f64)
}

struct Builder<'a> {
    sp: &'a ShapeParams,
    rng: Rng,
    nodes: Vec<Proto>,
    leaves: Vec<ProtoLeaf>,
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

impl Builder<'_> {
    fn push(&mut self, parent: Option<usize>, pos: [f64; 3], radius: f64, level: u8, birth: i32) -> usize {
        self.nodes.push(Proto { parent, pos, radius, level, birth });
        self.nodes.len() - 1
    }

    fn leaf(&mut self, node: usize, radius_frac: f64) {
        self.leaf_at(node, self.nodes[node].pos, radius_frac, [0.0; 3]);
    }

    /// The cluster at the tip of the chain ending in `node`, out along its last
    /// segment, so the bud end is leaves rather than wood.
    fn tip_leaf(&mut self, node: usize, radius_frac: f64) {
        let p = self.nodes[node].pos;
        let q = self.nodes[node].parent.map_or(p, |i| self.nodes[i].pos);
        let d = [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let out = if len > 0.0 { [d[0] / len, d[1] / len, d[2] / len] } else { [0.0, 1.0, 0.0] };
        self.leaf_at(node, p, radius_frac, out);
    }

    /// A cluster at `p`, alive with `node`.
    fn leaf_at(&mut self, node: usize, p: [f64; 3], radius_frac: f64, out: [f64; 3]) {
        let radius = self.sp.cluster_radius as f64 * radius_frac * self.rng.scale(0.2);
        let centre = [p[0] + self.rng.jitter(3.0), p[1] + self.rng.jitter(2.0), p[2] + self.rng.jitter(3.0)];
        self.leaves.push(ProtoLeaf { node, centre, radius, out });
    }

    /// One branch of `level` from `start` (on `parent`'s segment), then its
    /// sub-branches. Headings are world-frame azimuth / elevation.
    #[allow(clippy::too_many_arguments)]
    fn branch(
        &mut self,
        parent: usize,
        start: [f64; 3],
        (az, el): (i32, i32),
        len: f64,
        r0: f64,
        level: u8,
        birth: i32,
        leaf_scale: f64,
    ) {
        let sp = self.sp;
        let n = ((len / SEGMENT_VOXELS).ceil() as usize).clamp(1, MAX_SEGMENTS);
        let step = len / n as f64;
        let r_tip = r0 * TIP_RADIUS_FRAC;
        let mut chain = vec![self.push(Some(parent), start, r0, level, birth)];
        let mut heads = Vec::with_capacity(n);
        let (mut az, mut el) = (az, el);
        let mut pos = start;
        for i in 1..=n {
            let d = dir(az, el);
            heads.push((az, el));
            pos = [pos[0] + d[0] * step, pos[1] + d[1] * step, pos[2] + d[2] * step];
            let r = r0 + (r_tip - r0) * i as f64 / n as f64;
            let prev = *chain.last().unwrap();
            chain.push(self.push(Some(prev), pos, r, level, birth));
            el = (el + sp.limb_bend + self.rng.range(-3, 3)).clamp(-40, 60);
            az += self.rng.range(-5, 5);
        }
        let last_level = level >= sp.branch_levels;
        // Leaves: at the tip, and all along the outermost branches.
        self.tip_leaf(*chain.last().unwrap(), leaf_scale);
        if last_level {
            for &node in &chain[1..chain.len() - 1] {
                self.leaf(node, 0.8 * leaf_scale);
            }
            return;
        }
        let count = ((len / CHILD_SPACING_VOXELS).floor() as usize).clamp(1, MAX_CHILDREN);
        let side0 = if self.rng.unit() < 0.5 { 1 } else { -1 };
        for k in 0..count {
            let f =
                CHILD_START_FRAC + (1.0 - CHILD_START_FRAC) * (k as f64 + 0.5 + self.rng.jitter(0.3)) / count as f64;
            let at = f * n as f64;
            let seg = (at.floor() as usize).min(n - 1);
            let u = at - seg as f64;
            let (a, b) = (chain[seg], chain[seg + 1]);
            let p = lerp3(self.nodes[a].pos, self.nodes[b].pos, u);
            let r_at = self.nodes[a].radius + (self.nodes[b].radius - self.nodes[a].radius) * u;
            let (haz, hel) = heads[seg];
            let side = if k % 2 == 0 { side0 } else { -side0 };
            let caz = haz + side * (sp.child_angle + self.rng.range(-8, 8));
            let cel = (hel + sp.limb_angle.0) / 2 + self.rng.range(-10, 10);
            let clen = len * sp.child_length_pct as f64 / 100.0 * (1.15 - 0.5 * f) * self.rng.scale(0.2);
            let cbirth = birth + self.rng.range(CHILD_BIRTH_DELAY.0, CHILD_BIRTH_DELAY.1);
            self.branch(a, p, (caz, cel), clen, r_at * CHILD_RADIUS_FRAC, level + 1, cbirth, leaf_scale);
        }
    }

    fn tree(&mut self) {
        let sp = self.sp;
        let h = self
            .rng
            .range(sp.trunk_height_blocks.0 * BLOCK_VOXELS as i32, sp.trunk_height_blocks.1 * BLOCK_VOXELS as i32)
            as f64;
        let r_base = sp.trunk_radius as f64;
        let r_top = r_base * sp.taper_pct as f64 / 100.0;
        let lean_az = self.rng.range(0, ANGLE_TURN - 1);
        let lean = self.rng.range(0, sp.trunk_lean);
        let n = ((h / SEGMENT_VOXELS).ceil() as usize).clamp(1, MAX_SEGMENTS * 2);
        let step = h / n as f64;
        let mut trunk = vec![self.push(None, [AXIS, 0.0, AXIS], r_base, 0, 0)];
        let mut pos = [AXIS, 0.0, AXIS];
        for i in 1..=n {
            // Lean grows with height a little, plus a wobble per segment.
            let el = ANGLE_TURN / 4 - lean - self.rng.range(0, sp.trunk_lean / 2 + 1) / 2;
            let az = lean_az + self.rng.range(-12, 12);
            let d = dir(az, el);
            pos = [pos[0] + d[0] * step, pos[1] + d[1] * step, pos[2] + d[2] * step];
            let r = r_base + (r_top - r_base) * i as f64 / n as f64;
            let prev = *trunk.last().unwrap();
            trunk.push(self.push(Some(prev), pos, r, 0, 0));
        }
        self.tip_leaf(*trunk.last().unwrap(), 0.4 + 0.7 * crown_ratio(sp.crown, 1.0));
        let start = sp.limb_start_pct as f64 / 100.0;
        if sp.trunk_leaves {
            // Every half segment, so the core has no gaps up to the tip.
            for i in 2..2 * n {
                let t = i as f64 / (2 * n) as f64;
                if t >= start {
                    let (a, b) = (trunk[i / 2], trunk[(i / 2 + 1).min(n)]);
                    let p = lerp3(self.nodes[a].pos, self.nodes[b].pos, (i % 2) as f64 / 2.0);
                    self.leaf_at(a, p, 0.3 + 0.7 * crown_ratio(sp.crown, (t - start) / (1.0 - start)), [0.0; 3]);
                }
            }
        }

        // Limb attachment heights (fractions of the trunk) and their azimuths.
        let mut limbs: Vec<(f64, i32)> = Vec::new();
        if sp.whorl_spacing > 0 {
            let mut y = start * h;
            let mut az0 = self.rng.range(0, ANGLE_TURN - 1);
            while y < h * 0.94 {
                let count = self.rng.range(sp.limbs.0, sp.limbs.1);
                for j in 0..count {
                    limbs.push((y / h, az0 + j * ANGLE_TURN / count + self.rng.range(-8, 8)));
                }
                az0 += self.rng.range(20, 44);
                y += sp.whorl_spacing as f64 * self.rng.scale(0.25);
            }
        } else {
            let count = self.rng.range(sp.limbs.0, sp.limbs.1);
            let az0 = self.rng.range(0, ANGLE_TURN - 1);
            for k in 0..count {
                let t = start + (0.92 - start) * (k as f64 + 0.5 + self.rng.jitter(0.35)) / count as f64;
                limbs.push((t, az0 + k * GOLDEN_ANGLE + self.rng.range(-10, 10)));
            }
        }
        for (t, az) in limbs {
            let at = t * n as f64;
            let seg = (at.floor() as usize).min(n - 1);
            let u = at - seg as f64;
            let (a, b) = (trunk[seg], trunk[seg + 1]);
            let p = lerp3(self.nodes[a].pos, self.nodes[b].pos, u);
            let r_at = self.nodes[a].radius + (self.nodes[b].radius - self.nodes[a].radius) * u;
            let tc = (t - start) / (1.0 - start);
            let len = h * sp.limb_length_pct as f64 / 100.0 * crown_ratio(sp.crown, tc) * self.rng.scale(0.15);
            let r0 = r_at * sp.limb_radius_pct as f64 / 100.0;
            let el = sp.limb_angle.0 + ((sp.limb_angle.1 - sp.limb_angle.0) as f64 * tc) as i32 + self.rng.range(-6, 6);
            let birth = self.rng.range(LIMB_BIRTH.0, LIMB_BIRTH.1);
            let leaf_scale = 0.4 + 0.6 * crown_ratio(sp.crown, tc);
            self.branch(a, p, (az, el), len, r0, 1, birth, leaf_scale);
        }
    }
}

/// The skeleton of a `species` tree from `seed` at `age` (clamped to `MAX_AGE`).
pub fn skeleton(species: &Species, seed: u64, age: u16) -> TreeSkeleton {
    let sp = &species.shape;
    let mut b = Builder { sp, rng: Rng { seed, n: 0 }, nodes: Vec::new(), leaves: Vec::new() };
    b.tree();

    // Fit the full-grown tree inside the species limits (crown radius from the
    // trunk axis, height from the ground), horizontally and vertically apart.
    let lim_h = (species.crown_radius_blocks * BLOCK_VOXELS as i32) as f64 + AXIS - 2.0;
    let lim_v = (species.max_height_blocks * BLOCK_VOXELS as i32) as f64 - 2.0;
    let reach = |p: [f64; 3]| ((p[0] - AXIS) * (p[0] - AXIS) + (p[2] - AXIS) * (p[2] - AXIS)).sqrt();
    let (mut sh, mut sv) = (1.0f64, 1.0f64);
    // Wood and leaf clusters keep their size (only growth scales them): the fit
    // pulls the nodes in.
    for n in &b.nodes {
        sh = sh.min((lim_h - n.radius) / reach(n.pos).max(1.0));
        sv = sv.min((lim_v - n.radius) / n.pos[1].max(1.0));
    }
    for l in &b.leaves {
        // A leaf block counts if its centre is within `r + LEAF_DITHER_VOXELS`
        // of the cluster's (`raster.rs`); block centres sit on the trunk axis
        // lattice, so this keeps those blocks inside the limits too. Younger
        // trees have smaller radii and offsets, so the full-grown fit holds.
        let off = if l.out == [0.0; 3] { 0.0 } else { tip_offset(l.radius) };
        let out_h = (l.out[0] * l.out[0] + l.out[2] * l.out[2]).sqrt() * off;
        sh = sh.min((lim_h - l.radius - out_h) / reach(l.centre).max(1.0));
        sv = sv.min((lim_v - l.radius - l.out[1].max(0.0) * off) / l.centre[1].max(1.0));
    }
    let g = growth(age);
    let place = |p: [f64; 3]| {
        [
            (AXIS + (p[0] - AXIS) * sh * g + 0.5).floor() as i32,
            (p[1] * sv * g + 0.5).floor() as i32,
            (AXIS + (p[2] - AXIS) * sh * g + 0.5).floor() as i32,
        ]
    };

    // Birth order (stable, so parents — born no later, created first — precede
    // their children), then the prefix alive at `age`.
    let mut order: Vec<usize> = (0..b.nodes.len()).collect();
    order.sort_by_key(|&i| b.nodes[i].birth);
    let mut rank = vec![0usize; b.nodes.len()];
    for (k, &i) in order.iter().enumerate() {
        rank[i] = k;
    }
    let alive = order.iter().take_while(|&&i| b.nodes[i].birth <= age as i32).count();
    let nodes = order[..alive]
        .iter()
        .map(|&i| {
            let n = &b.nodes[i];
            let mut radius = (n.radius * g + 0.5).floor() as i32;
            if n.level == 0 {
                radius = radius.max(MIN_TRUNK_RADIUS.min((n.radius + 0.5).floor() as i32));
            }
            Node { parent: n.parent.map(|p| rank[p] as u16), pos: place(n.pos), radius, level: n.level }
        })
        .collect();
    let leaves = b
        .leaves
        .iter()
        .filter(|l| rank[l.node] < alive)
        .map(|l| {
            let r = ((l.radius * g + 0.5).floor() as i32).max(MIN_LEAF_RADIUS.min((l.radius + 0.5).floor() as i32));
            let off = if l.out == [0.0; 3] { 0.0 } else { tip_offset(r as f64) };
            let c = place(l.centre);
            let shift = |i: usize| (l.out[i] * off + 0.5).floor() as i32;
            ([c[0] + shift(0), c[1] + shift(1), c[2] + shift(2)], r)
        })
        .collect();
    TreeSkeleton { nodes, leaves }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trees::SPECIES;

    #[test]
    fn sin_table_is_symmetric() {
        for i in 0..64 {
            assert_eq!(SIN_TABLE[i], SIN_TABLE[128 - i]);
            assert_eq!(SIN_TABLE[i], -SIN_TABLE[128 + i]);
        }
        assert_eq!(SIN_TABLE[64], SIN_SCALE);
    }

    #[test]
    fn species_shapes_are_valid() {
        for sp in &SPECIES {
            let p = &sp.shape;
            assert!((1..=3).contains(&p.branch_levels), "{}", sp.name);
            assert!(p.trunk_height_blocks.0 <= p.trunk_height_blocks.1 && p.limbs.0 <= p.limbs.1);
            assert!(p.trunk_height_blocks.1 < sp.max_height_blocks, "{}", sp.name);
        }
    }

    #[test]
    fn skeleton_is_deterministic() {
        for sp in &SPECIES {
            let a = skeleton(sp, 42, 70);
            assert!(a.nodes.len() > 10 && !a.leaves.is_empty());
            assert_eq!(a, skeleton(sp, 42, 70));
            assert_ne!(a, skeleton(sp, 43, 70));
        }
    }

    #[test]
    fn older_tree_keeps_its_branches() {
        for sp in &SPECIES {
            for seed in 0..4 {
                for age in (0..=MAX_AGE - 10).step_by(15) {
                    let young = skeleton(sp, seed, age);
                    let old = skeleton(sp, seed, age + 10);
                    assert!(old.nodes.len() >= young.nodes.len());
                    for (i, n) in young.nodes.iter().enumerate() {
                        assert_eq!(n.parent, old.nodes[i].parent, "{} seed {seed} age {age} node {i}", sp.name);
                        assert_eq!(n.level, old.nodes[i].level);
                    }
                }
            }
        }
    }

    #[test]
    fn parents_precede_children() {
        for sp in &SPECIES {
            let s = skeleton(sp, 9, MAX_AGE);
            assert_eq!(s.nodes[0].parent, None);
            for (i, n) in s.nodes.iter().enumerate().skip(1) {
                assert!((n.parent.unwrap() as usize) < i);
            }
        }
    }

    #[test]
    fn sapling_is_a_stick_with_a_few_leaves() {
        for sp in &SPECIES {
            let young = skeleton(sp, 5, 5);
            let old = skeleton(sp, 5, MAX_AGE);
            let top = |s: &TreeSkeleton| s.nodes.iter().map(|n| n.pos[1]).max().unwrap();
            assert!(top(&young) * 3 < top(&old), "{}", sp.name);
            assert!(young.nodes.iter().filter(|n| n.level == 0).all(|n| n.radius <= MIN_TRUNK_RADIUS + 1));
            assert!(young.leaves.len() < old.leaves.len() / 4);
        }
    }
}
