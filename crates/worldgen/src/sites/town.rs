//! The first town layout: a plaza, up to four straight streets out of it, and houses on lots along both
//! sides of each street, every house turned to face its street and standing on its own pad. A pure
//! function of its arguments; integer math only. Nothing here samples terrain except through the
//! `ground` / `wet` closures it is given.

use std::sync::Arc;

use game::prefab::{placement_bounds, Facing, Placement, ResolvedPrefab};
use game::BlockId;

use super::kit::{turn_to_face, TownKit};
use crate::structures::{Pad, Paint, StructurePlacement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TownParams {
    pub seed: u64,
    /// 1..=4 streets out of the plaza, in the order east, west, south, north.
    pub arms: u8,
    /// Blocks from the plaza's edge.
    pub arm_len: i32,
    /// Odd.
    pub street_w: i32,
    /// Side of the square, odd.
    pub plaza: i32,
    /// Blocks between the street's edge and a house's front.
    pub setback: i32,
    /// Blocks between neighbouring houses along a street.
    pub gap_min: i32,
    pub gap_max: i32,
    /// A lot whose ground varies by more than this is skipped.
    pub max_relief: i32,
    pub pad_skirt: u32,
    pub road: BlockId,
    pub plaza_block: BlockId,
}

impl TownParams {
    pub fn small(seed: u64, road: BlockId, plaza_block: BlockId) -> Self {
        Self {
            seed,
            arms: 4,
            arm_len: 96,
            street_w: 5,
            plaza: 15,
            setback: 1,
            gap_min: 2,
            gap_max: 5,
            max_relief: 6,
            pad_skirt: 3,
            road,
            plaza_block,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lot {
    pub name: String,
    /// The house's world-block box: (min inclusive, max exclusive).
    pub bounds: ([i32; 3], [i32; 3]),
    /// The direction the house's front faces: toward its street.
    pub faces: Facing,
    pub arm: u8,
    pub left: bool,
}

#[derive(Debug, Clone)]
pub struct TownLayout {
    /// The centrepiece first (if any), then lots in the order laid.
    pub placements: Vec<StructurePlacement>,
    /// The plaza, then each arm's street.
    pub paints: Vec<Paint>,
    /// One per house placement, same order.
    pub lots: Vec<Lot>,
    /// Lot positions rejected for relief, water or overlap.
    pub skipped: u32,
}

/// The splitmix64 finaliser.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// The stateless roll used for every choice: a 64-bit mix of the seed and three small integers.
pub fn roll(seed: u64, a: u32, b: u32, c: u32) -> u64 {
    let mut h = mix(seed ^ GOLDEN);
    for v in [a, b, c] {
        h = mix(h ^ (u64::from(v) + 1).wrapping_mul(GOLDEN));
    }
    h
}

/// The arm's outward direction and its left-hand direction (looking outward from the plaza), as
/// (x, z) unit vectors. Arms in the order east, west, south, north.
fn arm_dirs(arm: u8) -> ([i32; 2], [i32; 2]) {
    match arm {
        0 => ([1, 0], [0, -1]),
        1 => ([-1, 0], [0, 1]),
        2 => ([0, 1], [1, 0]),
        _ => ([0, -1], [-1, 0]),
    }
}

fn facing_of(v: [i32; 2]) -> Facing {
    match v {
        [1, _] => Facing::East,
        [-1, _] => Facing::West,
        [_, 1] => Facing::South,
        _ => Facing::North,
    }
}

/// The world rectangle [x0, z0, x1, z1] (max exclusive) covering the half-open range `[a0, a1)` along
/// unit `along` and `[c0, c1)` along unit `across`, both measured from `origin`.
fn world_rect(origin: [i32; 2], along: [i32; 2], (a0, a1): (i32, i32), across: [i32; 2], (c0, c1): (i32, i32)) -> [i32; 4] {
    let mut r = [origin[0], origin[1], origin[0] + 1, origin[1] + 1];
    for (u, (lo, hi)) in [(along, (a0, a1)), (across, (c0, c1))] {
        let (axis, sign) = if u[0] != 0 { (0, u[0]) } else { (1, u[1]) };
        let o = origin[axis];
        let (min, max) = if sign > 0 { (o + lo, o + hi) } else { (o - hi + 1, o - lo + 1) };
        r[axis] = min;
        r[axis + 2] = max;
    }
    r
}

/// The extent of a world rectangle along unit `u`.
fn extent_along(r: [i32; 4], u: [i32; 2]) -> i32 {
    if u[0] != 0 { r[2] - r[0] } else { r[3] - r[1] }
}

fn meet(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// The placement putting `prefab` (turned `turn`) with its x/z box at (`x0`, `z0`) and its lowest layer
/// at `level + 1`.
fn place_at(prefab: &Arc<ResolvedPrefab>, turn: u8, x0: i32, z0: i32, level: i32) -> Placement {
    let trial = Placement { anchor: [0, 0, 0], turn, mirror: false };
    let (min, _) = placement_bounds(prefab, &trial);
    Placement { anchor: [x0 - min[0], level + 1 - min[1], z0 - min[2]], turn, mirror: false }
}

/// A pure function of its arguments. `ground(x, z)` is the natural ground height, `wet(x, z)` whether
/// the column is under water.
pub fn layout_town(
    kit: &TownKit,
    origin: [i32; 2],
    p: &TownParams,
    ground: &dyn Fn(i32, i32) -> i32,
    wet: &dyn Fn(i32, i32) -> bool,
) -> TownLayout {
    let [ox, oz] = origin;
    let (h, s) = (p.plaza / 2, p.street_w / 2);
    let arms = p.arms.min(4);
    let plaza = [ox - h, oz - h, ox + h + 1, oz + h + 1];
    let strips: Vec<[i32; 4]> = (0..arms)
        .map(|arm| {
            let (d, left) = arm_dirs(arm);
            world_rect(origin, d, (h + 1, h + 1 + p.arm_len), left, (-s, s + 1))
        })
        .collect();

    let mut out = TownLayout { placements: Vec::new(), paints: Vec::new(), lots: Vec::new(), skipped: 0 };
    out.paints.push(Paint { rect: plaza, block: p.plaza_block });
    // Every placed x/z box, the centrepiece too.
    let mut boxes: Vec<[i32; 4]> = Vec::new();

    if !kit.centrepieces.is_empty() {
        let c = &kit.centrepieces[(roll(p.seed, 0, 0, 0) % kit.centrepieces.len() as u64) as usize];
        let (sx, sz) = (c.size[0] as i32, c.size[2] as i32);
        let level = ground(ox, oz);
        let at = place_at(c, 0, ox - (sx - 1) / 2, oz - (sz - 1) / 2, level);
        let (min, max) = placement_bounds(c, &at);
        boxes.push([min[0], min[2], max[0], max[2]]);
        out.placements.push(StructurePlacement { prefab: c.clone(), at, pad: Some(Pad { level, skirt: 0 }) });
    }

    if kit.houses.is_empty() {
        return out;
    }
    let houses = kit.houses.len() as u64;
    let gap_span = (p.gap_max - p.gap_min + 1).max(1) as u64;
    let near = s + 1 + p.setback;

    for arm in 0..arms {
        let (d, left) = arm_dirs(arm);
        let mut far: Option<i32> = None;
        for side in 0..2u8 {
            let is_left = side == 0;
            let out_dir = if is_left { left } else { [-left[0], -left[1]] };
            let faces = facing_of([-out_dir[0], -out_dir[1]]);
            let stream = u32::from(arm) * 2 + u32::from(side) + 1;
            let mut t = h + 1 + p.gap_min;
            let mut n: u32 = 0;
            loop {
                let house = &kit.houses[(roll(p.seed, stream, n, 0) % houses) as usize];
                let turn = turn_to_face(house.front, faces);
                let trial = Placement { anchor: [0, 0, 0], turn, mirror: false };
                let (tmin, tmax) = placement_bounds(&house.prefab, &trial);
                let trial_rect = [tmin[0], tmin[2], tmax[0], tmax[2]];
                let (frontage, depth) = (extent_along(trial_rect, d), extent_along(trial_rect, out_dir));
                if t + frontage > h + 1 + p.arm_len {
                    break;
                }
                let r = world_rect(origin, d, (t, t + frontage), out_dir, (near, near + depth));
                let (cx, cz) = ((r[0] + r[2] - 1).div_euclid(2), (r[1] + r[3] - 1).div_euclid(2));
                let samples = [(r[0], r[1]), (r[2] - 1, r[1]), (r[0], r[3] - 1), (r[2] - 1, r[3] - 1), (cx, cz)];
                let heights = samples.map(|(x, z)| ground(x, z));
                let relief = heights.iter().max().unwrap() - heights.iter().min().unwrap();
                let grown = [r[0] - 1, r[1] - 1, r[2] + 1, r[3] + 1];
                let rejected = samples.iter().any(|&(x, z)| wet(x, z))
                    || relief > p.max_relief
                    || meet(grown, plaza)
                    || boxes.iter().any(|&b| meet(grown, b))
                    || strips.iter().any(|&b| meet(grown, b));
                if rejected {
                    out.skipped += 1;
                    t += 4;
                    n += 1;
                    continue;
                }
                // The middle column of the front face, rounding down along the arm.
                let mid = world_rect(origin, d, (t + (frontage - 1) / 2, t + (frontage - 1) / 2 + 1), out_dir, (near, near + 1));
                let level = ground(mid[0], mid[1]);
                let at = place_at(&house.prefab, turn, r[0], r[1], level);
                let bounds = placement_bounds(&house.prefab, &at);
                boxes.push(r);
                out.lots.push(Lot { name: house.prefab.name.clone(), bounds, faces, arm, left: is_left });
                out.placements.push(StructurePlacement {
                    prefab: house.prefab.clone(),
                    at,
                    pad: Some(Pad { level, skirt: p.pad_skirt }),
                });
                far = Some(far.map_or(t + frontage, |f: i32| f.max(t + frontage)));
                t += frontage + p.gap_min + (roll(p.seed, stream, n, 1) % gap_span) as i32;
                n += 1;
            }
        }
        if let Some(far) = far {
            out.paints.push(Paint { rect: world_rect(origin, d, (h + 1, far), left, (-s, s + 1)), block: p.road });
        }
    }
    out
}
