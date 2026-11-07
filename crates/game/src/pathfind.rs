//! Deterministic block-scale A* over region-local tiles (`TilePos`), shared by
//! the sim and the client. The sim's `TileSource` covers the region and its
//! terrain halo (`halo.rs`), so a path may run past the region's edge as far
//! as the source answers (unknown tiles read as air: never standable).
//! Walks to goals beyond that are planned by `path_plan.rs`. Moves
//! are DF-like: 8 horizontal neighbours at dy in {-1, 0, +1}, landing on a
//! standable tile; no swimming.
//!
//! Solid means `block_props().is_solid` (leaves are not: no collider) and not
//! air and not water; passable means air, or a non-solid non-water block.
//! Water is neither floor nor passable.
//!
//! Determinism: ordered containers only (`BinaryHeap` with a total order,
//! `BTreeMap`), never `HashMap` iteration.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use crate::orders::{TilePos, TileSource};
use crate::{block_props, BlockId};

/// Default expansion budget of one search, in nodes.
pub const MAX_PATH_NODES: u32 = 32_768;
/// Cost of one orthogonal step.
pub const STEP_COST: u32 = 10;
/// Cost of one diagonal step.
pub const DIAG_COST: u32 = 14;
/// Extra cost of a step that changes level — except a walk-up a slope
/// ([`step_is_walk_up`]), charged like a level step.
pub const CLIMB_COST: u32 = 5;
/// Lowest block `y` `nearest_standable` scans.
pub const COLUMN_SCAN_Y_MIN: i32 = -256;
/// Highest block `y` `nearest_standable` scans.
pub const COLUMN_SCAN_Y_MAX: i32 = 512;

fn passable_block(b: BlockId) -> bool {
    b == BlockId::AIR || (!block_props().is_liquid(b) && !block_props().is_solid(b))
}

fn solid_block(b: BlockId) -> bool {
    b != BlockId::AIR && !block_props().is_liquid(b) && block_props().is_solid(b)
}

fn passable(src: &impl TileSource, p: TilePos) -> bool {
    passable_block(src.block(p))
}

/// Whether a body can stand at `p`: `p` and the tile above are passable and
/// the tile below is solid. Anywhere the source answers — the sim's covers
/// region ∪ halo; an unknown floor reads as air, so it is never standable.
pub fn standable(src: &impl TileSource, p: TilePos) -> bool {
    passable_block(src.block(p))
        && passable_block(src.block([p[0], p[1] + 1, p[2]]))
        && solid_block(src.block([p[0], p[1] - 1, p[2]]))
}

/// Octile distance plus `CLIMB_COST` per level `a` must go DOWN to reach `b`:
/// every descent pays it, so that part never overestimates. Levels UP are not
/// charged — a slope walk-up climbs for free. Admissible and consistent (one
/// step moves at most one level, and a step down pays the `CLIMB_COST` the
/// heuristic drops).
fn heuristic(a: TilePos, b: TilePos) -> u32 {
    let dx = a[0].abs_diff(b[0]);
    let dz = a[2].abs_diff(b[2]);
    let (lo, hi) = (dx.min(dz), dx.max(dz));
    let down = (a[1] - b[1]).max(0) as u32;
    DIAG_COST * lo + STEP_COST * (hi - lo) + CLIMB_COST * down
}

/// Whether the `dy = +1` step `from -> to` walks up a slope: `to`'s floor is
/// a slope entered from its low side ([`crate::walk_up`]). Every other climb
/// is a jump.
pub fn step_is_walk_up(src: &impl TileSource, from: TilePos, to: TilePos) -> bool {
    to[1] == from[1] + 1
        && src.slope([to[0], to[1] - 1, to[2]]).is_some_and(|s| crate::walk_up(s, [from[0] - to[0], from[2] - to[2]]))
}

/// Per tile of `path`: whether the step onto it is a walk-up
/// ([`step_is_walk_up`]); the first tile is `false`.
pub fn walk_ups(src: &impl TileSource, path: &[TilePos]) -> Vec<bool> {
    let steps = path.windows(2).map(|w| step_is_walk_up(src, w[0], w[1]));
    path.first().map(|_| false).into_iter().chain(steps).collect()
}

/// Neighbour offsets in a fixed order (orthogonals first).
const DIRS: [[i32; 2]; 8] =
    [[1, 0], [-1, 0], [0, 1], [0, -1], [1, 1], [1, -1], [-1, 1], [-1, -1]];

fn successors(src: &impl TileSource, p: TilePos, out: &mut Vec<(TilePos, u32)>) {
    out.clear();
    for [dx, dz] in DIRS {
        for dy in [0, 1, -1] {
            let q = [p[0] + dx, p[1] + dy, p[2] + dz];
            if !standable(src, q) {
                continue;
            }
            if dy == 1 && !passable(src, [p[0], p[1] + 2, p[2]]) {
                continue;
            }
            let diagonal = dx != 0 && dz != 0;
            if diagonal {
                let top = p[1].max(q[1]);
                if !passable(src, [p[0] + dx, top, p[2]]) || !passable(src, [p[0], top, p[2] + dz]) {
                    continue;
                }
            }
            let mut cost = if diagonal { DIAG_COST } else { STEP_COST };
            if dy == -1 || (dy == 1 && !step_is_walk_up(src, p, q)) {
                cost += CLIMB_COST;
            }
            out.push((q, cost));
        }
    }
}

/// The cheapest path `from..=to` inclusive, or `None` when unreachable, an
/// end is not standable, or more than `max_nodes` nodes would be expanded.
pub fn find_path(src: &impl TileSource, from: TilePos, to: TilePos, max_nodes: u32) -> Option<Vec<TilePos>> {
    if !standable(src, to) {
        return None;
    }
    find_path_toward(src, from, to, |p| p == to, max_nodes)
}

/// A* from `from` guided toward `toward` (the heuristic's target), ending at
/// the first tile `accept` takes as it is expanded: the cheapest such tile
/// when `toward` is accepted (as [`find_path`]), else the first one the
/// search toward `toward` reaches (`path_plan.rs` uses it to cross into the
/// next region of a route). `None` when `from` is not standable, no accepted
/// tile is reachable, or more than `max_nodes` nodes would be expanded.
pub fn find_path_toward(
    src: &impl TileSource,
    from: TilePos,
    toward: TilePos,
    accept: impl Fn(TilePos) -> bool,
    max_nodes: u32,
) -> Option<Vec<TilePos>> {
    if !standable(src, from) {
        return None;
    }
    let to = toward;
    if accept(from) {
        return Some(vec![from]);
    }
    let mut g: BTreeMap<TilePos, u32> = BTreeMap::new();
    let mut came: BTreeMap<TilePos, TilePos> = BTreeMap::new();
    let mut open = BinaryHeap::new();
    let h0 = heuristic(from, to);
    g.insert(from, 0);
    open.push((Reverse(h0), Reverse(h0), Reverse(from)));
    let mut expanded = 0u32;
    let mut next = Vec::with_capacity(16);
    while let Some((Reverse(f), Reverse(h), Reverse(p))) = open.pop() {
        let gp = f - h;
        if g.get(&p).is_some_and(|&best| gp > best) {
            continue;
        }
        if accept(p) {
            let mut path = vec![p];
            let mut cur = p;
            while let Some(&prev) = came.get(&cur) {
                path.push(prev);
                cur = prev;
            }
            path.reverse();
            return Some(path);
        }
        if expanded >= max_nodes {
            return None;
        }
        expanded += 1;
        successors(src, p, &mut next);
        for &(q, cost) in &next {
            let gq = gp + cost;
            if g.get(&q).is_none_or(|&best| gq < best) {
                g.insert(q, gq);
                came.insert(q, p);
                let hq = heuristic(q, to);
                open.push((Reverse(gq + hq), Reverse(hq), Reverse(q)));
            }
        }
    }
    None
}

/// Margin, in blocks, `nearest_standable` scans beyond a column's band.
pub const COLUMN_BAND_MARGIN: i32 = 2;

/// The block-`y` range scanned for `column`: its [`TileSource::column_band`]
/// widened by [`COLUMN_BAND_MARGIN`], else the fixed fallback constants.
pub fn column_scan_range(src: &impl TileSource, column: [i32; 2]) -> (i32, i32) {
    src.column_band(column)
        .map_or((COLUMN_SCAN_Y_MIN, COLUMN_SCAN_Y_MAX), |(lo, hi)| (lo - COLUMN_BAND_MARGIN, hi + COLUMN_BAND_MARGIN))
}

/// A standable tile in `column` (`[x, z]`): the one closest to `y_hint`
/// (lower first on a tie), or the topmost when `None`. Scans the column's
/// band ([`column_scan_range`]).
pub fn nearest_standable(src: &impl TileSource, column: [i32; 2], y_hint: Option<i32>) -> Option<TilePos> {
    let at = |y: i32| [column[0], y, column[1]];
    let (y_min, y_max) = column_scan_range(src, column);
    match y_hint {
        None => (y_min..=y_max).rev().map(at).find(|&p| standable(src, p)),
        Some(hint) => {
            let hint = hint.clamp(y_min, y_max);
            let span = (hint - y_min).max(y_max - hint).max(0);
            (0..=span).find_map(|d| {
                [hint - d, hint + d]
                    .into_iter()
                    .filter(|y| (y_min..=y_max).contains(y))
                    .map(at)
                    .find(|&p| standable(src, p))
            })
        }
    }
}
