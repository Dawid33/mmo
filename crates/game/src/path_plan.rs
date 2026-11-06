//! `PathPlan`: how a walker gets to a WORLD goal any number of regions away
//! (spec Part 3, row 6.5 of the seam-transparent plan). Owned by the
//! substrate, used the same way by the sim and the client's move-order
//! preview, so both plan the same path from the same tiles:
//!
//! - the COARSE route is over the region graph: [`next_region`] is its step,
//!   a pure integer function of the two regions (8-connected, along the
//!   straight line), identical in every region and on the client. A goal off
//!   the world's map (`MapExtent`) is first moved onto its nearest column
//!   inside it; the map is a rectangle, so the route never leaves it.
//! - the FINE path is block-scale A* (`pathfind.rs`) over the region's own
//!   tiles ∪ its terrain halo (`halo.rs`), tiles off the map excluded: to the
//!   goal when the halo reaches it, else to the first tile at least
//!   [`PORTAL_DEPTH_BLOCKS`] into the route's next region.
//! - the path is in world blocks and travels with the walker across a seam:
//!   the region it arrives in walks on along it when it still leads to the
//!   goal over its own ∪ halo tiles ([`Rollback::continue_walk_safe`]) — no
//!   stop at the edge and no re-plan, so a walk split by a seam takes the
//!   path a walk inside one region would — and plans the next stretch
//!   otherwise.
use crate::{
    find_path, find_path_toward, nearest_standable, standable, Goal, MapExtent, RegionCoords, Rollback, TilePos,
    TileSource, WorldBlockPos, BLOCK_VOXELS, FLIP_HYSTERESIS, HALO_BLOCKS, REGION_BLOCKS,
};

/// How far (blocks) past the seam into the route's next region a stretch
/// that does not reach the goal ends: the body is handed off well before it
/// gets there (`FLIP_HYSTERESIS`), and the tile is still inside the halo.
pub const PORTAL_DEPTH_BLOCKS: i32 = 2;
const _: () = assert!(
    (PORTAL_DEPTH_BLOCKS * BLOCK_VOXELS as i32) as f32 > FLIP_HYSTERESIS && PORTAL_DEPTH_BLOCKS < HALO_BLOCKS,
    "a stretch's end must be past the hand-off and inside the halo"
);
/// Farthest (blocks, per axis) the A* heuristic's target lies from the start:
/// a goal farther away is aimed at along the same line, this far.
pub const HINT_REACH_BLOCKS: i64 = 4 * REGION_BLOCKS as i64;
/// Farthest (blocks) the heuristic's target level lies from the start's.
pub const HINT_LEVEL_REACH: i32 = 4096;

/// A planned stretch of a walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPlan {
    /// World tiles, the start and the end inclusive.
    pub path: Vec<WorldBlockPos>,
    /// The path ends on the goal's tile; else a few blocks into the next
    /// region of the route toward it.
    pub reaches_goal: bool,
}

impl PathPlan {
    /// The stretch a walker standing on `from` (in `here`'s block frame)
    /// walks toward `goal`, searched under `max_nodes` over `src` (region ∪
    /// halo; tiles off `map` excluded): to the goal's standable tile nearest
    /// its level when it can be reached, else into the next region of the
    /// coarse route. `None` when `from` is not standable, the goal is in
    /// `here` (or nothing of the next region can be reached) without a
    /// path, or its column lies past the `i32` region grid.
    pub fn plan(
        src: &impl TileSource,
        from: TilePos,
        goal: &Goal,
        here: RegionCoords,
        map: Option<MapExtent>,
        max_nodes: u32,
    ) -> Option<PathPlan> {
        let goal = onto_map(goal, map)?;
        let goal_region = goal.region()?;
        let src = OnMap { src, here, map };
        let n = REGION_BLOCKS as i32;
        let gl = goal.local_column(here);
        let world = |path: Vec<TilePos>| {
            path.into_iter()
                .map(|p| WorldBlockPos::from_local(here, crate::BlockPos::new(p[0], p[1], p[2])))
                .collect::<Vec<_>>()
        };
        // The goal itself, when the tiles around this region reach it.
        if let Some(column) = gl.filter(|c| c.iter().all(|&v| (-n..2 * n).contains(&v))) {
            let to = nearest_standable(&src, column, goal.y);
            if let Some(path) = to.and_then(|to| find_path(&src, from, to, max_nodes)) {
                return Some(PathPlan { path: world(path), reaches_goal: true });
            }
            if goal_region == here {
                return None;
            }
        }
        // Else into the next region of the route.
        let next = next_region(here, goal_region);
        let toward = hint(from, &goal, here);
        let accept = |p: TilePos| past_seam(p, here, next);
        let path = find_path_toward(&src, from, toward, accept, max_nodes).filter(|p| p.len() > 1)?;
        Some(PathPlan { path: world(path), reaches_goal: false })
    }

    /// The path in `here`'s block frame.
    pub fn local_path(&self, here: RegionCoords) -> Vec<TilePos> {
        self.path.iter().map(|&p| crate::local_tile(p, here)).collect()
    }
}

/// The coarse route's step: the neighbour of `from` (8-connected) to walk
/// into toward `to` — one whose Chebyshev distance to `to` is one less,
/// moving along each axis only toward `to`, nearest the straight line from
/// `from` to `to` (ties: the lower `(x, z)`). `to` itself when `from == to`.
/// Pure integer: every region and the client step the same way.
pub fn next_region(from: RegionCoords, to: RegionCoords) -> RegionCoords {
    let (lx, lz) = (i128::from(to.x) - i128::from(from.x), i128::from(to.z) - i128::from(from.z));
    if (lx, lz) == (0, 0) {
        return to;
    }
    let cheb = |dx: i128, dz: i128| (lx - dx).abs().max((lz - dz).abs());
    let best = cheb(0, 0) - 1;
    let mut steps = Vec::with_capacity(3);
    for dx in [0, lx.signum()] {
        for dz in [0, lz.signum()] {
            if (dx, dz) != (0, 0) && cheb(dx, dz) == best && !steps.contains(&(dx, dz)) {
                steps.push((dx, dz));
            }
        }
    }
    let off_line = |&(dx, dz): &(i128, i128)| ((lx * dz - lz * dx).abs(), dx, dz);
    let (dx, dz) = steps.into_iter().min_by_key(off_line).expect("a step toward a different region");
    RegionCoords::new(from.x + dx as i32, from.z + dz as i32)
}

/// Whether tile `p` (in `here`'s frame) lies in region `next` at least
/// [`PORTAL_DEPTH_BLOCKS`] past each seam between `here` and `next`.
fn past_seam(p: TilePos, here: RegionCoords, next: RegionCoords) -> bool {
    let n = REGION_BLOCKS as i32;
    let d = PORTAL_DEPTH_BLOCKS;
    let axis = |v: i32, step: i32| match step.signum() {
        1 => v >= n + d,
        -1 => v < -d,
        _ => (0..n).contains(&v),
    };
    axis(p[0], next.x - here.x) && axis(p[2], next.z - here.z)
}

/// The A* heuristic's target for a walk from `from` toward `goal`: the
/// goal's tile in `here`'s frame, or the point [`HINT_REACH_BLOCKS`] along
/// the line to it; at the goal's level, or `from`'s when it has none.
fn hint(from: TilePos, goal: &Goal, here: RegionCoords) -> TilePos {
    let n = REGION_BLOCKS as i128;
    let d = [
        i128::from(goal.column[0]) - i128::from(here.x) * n - i128::from(from[0]),
        i128::from(goal.column[1]) - i128::from(here.z) * n - i128::from(from[2]),
    ];
    let far = d[0].abs().max(d[1].abs());
    let reach = i128::from(HINT_REACH_BLOCKS);
    let d = if far > reach { d.map(|v| v * reach / far) } else { d };
    let y = goal.y.unwrap_or(from[1]).clamp(from[1].saturating_sub(HINT_LEVEL_REACH), from[1].saturating_add(HINT_LEVEL_REACH));
    [from[0] + d[0] as i32, y, from[2] + d[1] as i32]
}

/// `goal` moved onto its nearest block column inside `map` (unchanged when
/// unbounded or already inside); `None` for an empty map.
fn onto_map(goal: &Goal, map: Option<MapExtent>) -> Option<Goal> {
    let Some(m) = map else { return Some(*goal) };
    if m.width == 0 || m.height == 0 {
        return None;
    }
    let n = REGION_BLOCKS as i64;
    let lo = [i64::from(m.origin.x) * n, i64::from(m.origin.z) * n];
    let hi = [lo[0] + i64::from(m.width) * n - 1, lo[1] + i64::from(m.height) * n - 1];
    Some(Goal { column: [goal.column[0].clamp(lo[0], hi[0]), goal.column[1].clamp(lo[1], hi[1])], ..*goal })
}

/// A [`TileSource`] with every tile in a region off `map` read as air
/// (nothing to stand on): no walk is planned across the world's outer seam.
struct OnMap<'a, S> {
    src: &'a S,
    here: RegionCoords,
    map: Option<MapExtent>,
}

impl<S: TileSource> OnMap<'_, S> {
    fn on_map(&self, x: i32, z: i32) -> bool {
        let Some(m) = self.map else { return true };
        let n = REGION_BLOCKS as i32;
        m.contains(RegionCoords::new(self.here.x + x.div_euclid(n), self.here.z + z.div_euclid(n)))
    }
}

impl<S: TileSource> TileSource for OnMap<'_, S> {
    fn block(&self, p: TilePos) -> crate::BlockId {
        if self.on_map(p[0], p[2]) { self.src.block(p) } else { crate::BlockId::AIR }
    }
    fn is_bedrock(&self, p: TilePos) -> bool {
        self.on_map(p[0], p[2]) && self.src.is_bedrock(p)
    }
    fn is_constructed(&self, p: TilePos) -> bool {
        self.on_map(p[0], p[2]) && self.src.is_constructed(p)
    }
    fn tree_id(&self, p: TilePos) -> u32 {
        if self.on_map(p[0], p[2]) { self.src.tree_id(p) } else { 0 }
    }
    fn tree_limb(&self, p: TilePos) -> u8 {
        if self.on_map(p[0], p[2]) { self.src.tree_limb(p) } else { 0 }
    }
    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        self.on_map(column[0], column[1]).then(|| self.src.column_band(column)).flatten()
    }
    fn slope(&self, p: TilePos) -> Option<crate::SlopeShape> {
        self.on_map(p[0], p[2]).then(|| self.src.slope(p)).flatten()
    }
}

impl Rollback {
    /// A walk that just arrived across a seam into `here` walks on, undo-
    /// safely: along its carried path when that still leads to the goal over
    /// this region's own ∪ halo tiles (every tile still to walk standable
    /// here), else along a stretch planned afresh from the body's tile
    /// ([`PathPlan::plan`]); dropped when there is none. No walker: no change.
    pub fn continue_walk_safe(&mut self, e: crate::EntityKey, here: RegionCoords, map: Option<MapExtent>) {
        let Some(old) = self.data.ecs.walker.try_get(e).clone() else { return };
        let src = OnMap { src: &*self, here, map };
        let rest = old.path.get(old.next as usize..).unwrap_or_default();
        let walkable = rest.iter().all(|&p| standable(&src, crate::local_tile(p, here)));
        if old.to_goal && !rest.is_empty() && walkable {
            return;
        }
        let new = self.plan_walker(e, old.goal, here, map);
        self.data.ecs.walker.set_safe(e, new);
    }
}
