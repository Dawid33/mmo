//! Deterministic block-scale A* over region-local tiles (`TilePos`), shared by
//! the sim and the client. The sim's `TileSource` covers the region and its
//! terrain halo (`halo.rs`), so a path may run past the region's edge as far
//! as the source answers (unknown tiles read as air: never standable).
//! Walks to goals beyond that are planned by `path_plan.rs`. Moves
//! are DF-like: 8 horizontal neighbours at dy in {-1, 0, +1}, landing on a
//! standable tile; no swimming.
//!
//! Solid means `block_props().is_solid` (leaves and water are not: no
//! collider) and not air; passable means not solid. Water is passable but
//! never a floor: a tile is standable only in water up to
//! [`WADE_MAX_DEPTH`] (DF depth), and stepping onto wading-depth water costs
//! [`WADE_COST`] more.
//!
//! Determinism: ordered containers only (`BinaryHeap` with a total order,
//! `BTreeMap`), never `HashMap` iteration.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use crate::orders::{TilePos, TileSource};
use crate::stud::TILE_PLATES;
use crate::tiles::{TerrainKind, TileFill, STUD_BODY_PLATES, STUD_DROP_PLATES, STUD_STEP_PLATES};
use crate::{block_props, df_depth, BlockId, WADE_COST, WADE_MAX_DEPTH};

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
    b == BlockId::AIR || !block_props().is_solid(b)
}

fn solid_block(b: BlockId) -> bool {
    b != BlockId::AIR && block_props().is_solid(b)
}

fn passable(src: &impl TileSource, p: TilePos) -> bool {
    passable_block(src.block(p))
}

/// Whether a body can stand at `p`: `p` and the tile above are passable and
/// the tile below is solid. Anywhere the source answers — the sim's covers
/// region ∪ halo; an unknown floor reads as air, so it is never standable.
pub fn standable(src: &impl TileSource, p: TilePos) -> bool {
    if src.terrain() == TerrainKind::Stud {
        return stud_standable(src, p);
    }
    passable_block(src.block(p))
        && passable_block(src.block([p[0], p[1] + 1, p[2]]))
        && df_depth(src.water_units(p)) <= WADE_MAX_DEPTH
        && solid_block(src.block([p[0], p[1] - 1, p[2]]))
}

/// Octile distance plus `CLIMB_COST` per level `a` must go DOWN to reach `b`:
/// every descent pays it, so that part never overestimates. Levels UP are not
/// charged — a slope walk-up climbs for free. Admissible and consistent (one
/// step moves at most one level, and a step down pays the `CLIMB_COST` the
/// heuristic drops).
///
/// On stud terrain a level can be descended in steps that cost nothing extra, so
/// the descent is not charged there (plain octile distance).
fn heuristic(kind: TerrainKind, a: TilePos, b: TilePos) -> u32 {
    let dx = a[0].abs_diff(b[0]);
    let dz = a[2].abs_diff(b[2]);
    let (lo, hi) = (dx.min(dz), dx.max(dz));
    let down = match kind {
        TerrainKind::Voxel => (a[1] - b[1]).max(0) as u32,
        TerrainKind::Stud => 0,
    };
    DIAG_COST * lo + STEP_COST * (hi - lo) + CLIMB_COST * down
}

// --- Stud terrain (`tiles.rs`) -------------------------------------------------
//
// Heights below are in plates. A body standing in tile `p` rests on a FLOOR `o`
// plates above the tile's bottom, `STUD_FLOOR_MIN_PLATES..=STUD_STEP_PLATES`
// (-2..=3): every floor height belongs to exactly one tile, the one whose bottom is
// nearest below-or-just-above it — the stud form of "the level nearest the feet".

/// The floor a body standing in stud tile `p` rests on, plates above the tile's
/// bottom: the tile's own partial fill when that is no higher than a step
/// (`Floor(1..=3)`), else — the tile open — the top of the tile below when that is
/// solid (0) or a partial fill higher than a step (`Floor(4..=6)`: -2..=0). `None`
/// when the tile holds no such floor: it is solid or filled higher than a step, or
/// open over nothing (or over a low floor, which belongs to the tile below).
fn stud_floor(src: &impl TileSource, p: TilePos) -> Option<i32> {
    match src.tile_fill(p) {
        TileFill::Solid => None,
        TileFill::Floor(k) => (i32::from(k) <= STUD_STEP_PLATES).then_some(i32::from(k)),
        TileFill::Open => match src.tile_fill([p[0], p[1] - 1, p[2]]) {
            TileFill::Solid => Some(0),
            TileFill::Floor(k) if i32::from(k) > STUD_STEP_PLATES => Some(i32::from(k) - TILE_PLATES as i32),
            _ => None,
        },
    }
}

/// The absolute plate of tile level `y`'s bottom.
fn level_plate(y: i32) -> i64 {
    i64::from(y) * TILE_PLATES as i64
}

/// Whether stud tile column `[x, z]` has room for a body whose feet are at absolute
/// plate `from`: every tile meeting the [`STUD_BODY_PLATES`] above it is open —
/// except the one holding `from`, which may be filled up to it. Tile-grained, so
/// conservative: anything in a tile the body reaches into blocks it.
fn stud_clear(src: &impl TileSource, column: [i32; 2], from: i64) -> bool {
    let tp = TILE_PLATES as i64;
    let (first, last) = (from.div_euclid(tp), (from + i64::from(STUD_BODY_PLATES) - 1).div_euclid(tp));
    (first..=last).all(|ty| match src.tile_fill([column[0], ty as i32, column[1]]) {
        TileFill::Open => true,
        TileFill::Floor(k) => ty * tp + i64::from(k) <= from,
        TileFill::Solid => false,
    })
}

/// The absolute plate of the floor a body standing in stud tile `p` rests on, when
/// it can stand there: the tile has a floor ([`stud_floor`]) with room for the body
/// over it ([`stud_clear`]). In tiles: over a floor up to 1 plate above the tile's
/// bottom the tile above must be open; over a higher one (2 or 3 plates) the tile
/// two above as well — a 2.2 m body is 11 plates, two tiles are 12.
///
/// Water is never a floor, as on voxels: the tile is standable only in water up to
/// [`WADE_MAX_DEPTH`] (`Rollback::stud_water_units`: 2 plates over the tile's floor).
fn stud_stand(src: &impl TileSource, p: TilePos) -> Option<i64> {
    let floor = level_plate(p[1]) + i64::from(stud_floor(src, p)?);
    (stud_clear(src, [p[0], p[2]], floor) && df_depth(src.water_units(p)) <= WADE_MAX_DEPTH).then_some(floor)
}

fn stud_standable(src: &impl TileSource, p: TilePos) -> bool {
    stud_stand(src, p).is_some()
}

/// How far above tile `p`'s bottom (sim units) the feet of a body standing in it
/// rest: 0 on voxel terrain (the top of the block below); on stud terrain the height
/// of its floor (negative when the floor is in the tile below), 0 where it has none.
pub fn stand_offset_units(src: &impl TileSource, p: TilePos) -> f32 {
    match src.terrain() {
        TerrainKind::Voxel => 0.0,
        TerrainKind::Stud => crate::stud::plate_units(stud_floor(src, p).unwrap_or(0)),
    }
}

/// Whether a body with feet at `feet` (region-local sim units) is clear of terrain
/// where it is: on voxel terrain its feet tile and the tile above hold no solid
/// block (water is clear); on stud terrain its column has room from the plate
/// its feet stand on ([`stud_clear`]).
pub fn body_fits(src: &impl TileSource, feet: rapier3d::math::Vector) -> bool {
    let kind = src.terrain();
    let t = kind.feet_tile(feet);
    match kind {
        TerrainKind::Voxel => passable(src, t) && passable(src, [t[0], t[1] + 1, t[2]]),
        TerrainKind::Stud => stud_clear(src, [t[0], t[2]], TerrainKind::feet_plate(feet.y)),
    }
}

/// [`successors`] on stud terrain. From a standable tile to each of the 8
/// neighbouring columns, landing on a standable tile one level up, level or down,
/// when the floors differ by no more than a step UP ([`STUD_STEP_PLATES`]: walked,
/// never jumped) or a tile DOWN ([`STUD_DROP_PLATES`]: a drop, charged `CLIMB_COST`
/// when deeper than a step). The lower column must have room for the body at the
/// higher floor; a diagonal step needs that room in both columns beside it too.
fn stud_successors(src: &impl TileSource, p: TilePos, out: &mut Vec<(TilePos, u32)>) {
    let Some(sp) = stud_stand(src, p) else { return };
    for [dx, dz] in DIRS {
        for dy in [0, 1, -1] {
            let q = [p[0] + dx, p[1] + dy, p[2] + dz];
            let Some(sq) = stud_stand(src, q) else { continue };
            let rise = sq - sp;
            if rise > i64::from(STUD_STEP_PLATES) || rise < -i64::from(STUD_DROP_PLATES) {
                continue;
            }
            let high = sp.max(sq);
            if rise > 0 && !stud_clear(src, [p[0], p[2]], high) {
                continue;
            }
            if rise < 0 && !stud_clear(src, [q[0], q[2]], high) {
                continue;
            }
            let diagonal = dx != 0 && dz != 0;
            if diagonal && !(stud_clear(src, [p[0] + dx, p[2]], high) && stud_clear(src, [p[0], p[2] + dz], high)) {
                continue;
            }
            let mut cost = if diagonal { DIAG_COST } else { STEP_COST };
            if rise < -i64::from(STUD_STEP_PLATES) {
                cost += CLIMB_COST;
            }
            if (1..=WADE_MAX_DEPTH).contains(&df_depth(src.water_units(q))) {
                cost += WADE_COST;
            }
            out.push((q, cost));
        }
    }
}

/// Whether the `dy = +1` step `from -> to` walks up a slope: `to`'s floor is
/// a slope entered from its low side ([`crate::walk_up`]). Every other climb
/// is a jump. On stud terrain every step of a path is walked (a path never rises
/// more than the step height at once), so every step is a walk-up.
pub fn step_is_walk_up(src: &impl TileSource, from: TilePos, to: TilePos) -> bool {
    if src.terrain() == TerrainKind::Stud {
        return true;
    }
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
    if src.terrain() == TerrainKind::Stud {
        return stud_successors(src, p, out);
    }
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
            if (1..=WADE_MAX_DEPTH).contains(&df_depth(src.water_units(q))) {
                cost += WADE_COST;
            }
            out.push((q, cost));
        }
    }
}

/// The cost the search charges for walking `path` (consecutive tiles must be
/// successors of each other); `None` when a step is not a legal move.
pub fn path_cost(src: &impl TileSource, path: &[TilePos]) -> Option<u32> {
    let mut next = Vec::with_capacity(16);
    path.windows(2).try_fold(0u32, |sum, w| {
        successors(src, w[0], &mut next);
        next.iter().find(|(q, _)| *q == w[1]).map(|&(_, c)| sum + c)
    })
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
    let kind = src.terrain();
    let heuristic = |a: TilePos, b: TilePos| heuristic(kind, a, b);
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
