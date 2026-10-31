//! Adventure move orders: a right click on the ground walks the local body
//! there, and the hover shows the path it would take first.
//!
//! Active only in Adventure, with the local body selected and the pointer
//! not on the UI. The hover's tile ([`goal_from_hover`]) is pathed from the
//! body's tile over the home region's [`TileMirror`] with the sim's own rules
//! (`game::find_path`, `game::nearest_standable`, and `game::edge_toward` for
//! a hover in a neighbouring region — the path then runs to the facing edge
//! tile and a dashed line goes on to the hover). The search runs on the main
//! thread, only when the (body tile, hover tile) pair changes
//! ([`PathPreview::update`]), and under [`PREVIEW_MAX_PATH_NODES`] rather
//! than the sim's `MAX_PATH_NODES`, so a hover never costs a frame more than
//! a few ms; a hover that is out of the preview's reach shows as "no path"
//! (a red cross) and a click on it sends nothing.
//!
//! A right press starts an orbit drag (`hold_pointer_while_orbiting`); a
//! press released with less than [`selection::DRAG_THRESHOLD_PX`] of mouse
//! motion ([`is_click`]) is a click, and sends `Order::MoveTo` for the tile
//! hovered at the press. The last sent path stays drawn, dimmed, until the
//! body reaches its end, leaves the home region or a new order replaces it —
//! cosmetic only: the sim plans its own path.

use bevy::prelude::*;
use game::movement::UNITS_PER_METER;
use game::rapier3d::math::Vector;
use game::{Goal, Order, RegionCoords, RegionId, TilePos, TileSource, BLOCK_VOXELS, REGION_BLOCKS};

use super::adventure::ClientPlayMode;
use super::designate::HiddenEdgeGizmos;
use super::selection::{self, GroundPick, Selection};
use super::tiles::TileMirror;
use super::RightPress;
use crate::renderer::bridge::{LocalBody, SimEntity};
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::{GameEvents, LocalPlayer};

/// Expansion budget of one preview search, in nodes. The sim searches up to
/// `game::MAX_PATH_NODES` (32 768), which measured ~87 ms on the main thread
/// (release, 512 m region, worst case); 2048 measured ~4.4 ms.
pub const PREVIEW_MAX_PATH_NODES: u32 = 2048;
/// The previewed path.
pub const PATH_COLOR_OK: Color = Color::srgb(0.35, 0.95, 0.45);
/// The cross at a hover with no path.
pub const PATH_COLOR_NONE: Color = Color::srgb(0.95, 0.25, 0.2);
/// Alpha of the last sent path (dimmed under the live preview).
pub const SENT_PATH_ALPHA: f32 = 0.35;
/// Alpha of the path's part hidden behind terrain (drawn in front of it).
pub const HIDDEN_PATH_ALPHA: f32 = 0.3;
/// Height of the path line above each tile's floor, metres.
pub const PATH_LIFT_M: f32 = 0.1;
/// Half the arm of the no-path cross, metres.
pub const NO_PATH_CROSS_M: f32 = 0.4;
/// Dash length (and gap) of the segment on to a hover in another region,
/// metres.
pub const DASH_M: f32 = 0.5;

/// Whether a press whose mouse moved `accumulated_motion_px` (raw motion,
/// pixels) before its release was a click rather than an orbit drag.
pub fn is_click(accumulated_motion_px: f32) -> bool {
    accumulated_motion_px < selection::DRAG_THRESHOLD_PX
}

/// The `MoveTo` goal for a hover at `local` (sim units, local to `region`):
/// its block column, at the hovered level when the pick is `exact` and in
/// `home`; elsewhere the level is left to the sim (`y: None`, the topmost
/// standable tile of the column).
pub fn goal_from_hover(region: RegionId, local: Vec3, exact: bool, home: RegionId) -> Goal {
    let t = game::feet_tile(Vector::new(local.x, local.y, local.z));
    Goal { region, column: [t[0], t[2]], y: (exact && region == home).then_some(t[1]) }
}

/// The hovered tile in `home`'s block coordinates (outside `0..REGION_BLOCKS`
/// for a hover in another region): the preview's key.
pub fn hover_tile(region: RegionId, local: Vec3, home: RegionId) -> TilePos {
    let n = REGION_BLOCKS as i32;
    let t = game::feet_tile(Vector::new(local.x, local.y, local.z));
    [t[0] + (region.x - home.x) * n, t[1], t[2] + (region.z - home.z) * n]
}

/// The path the sim would plan from `from` toward `goal` (`here` is the
/// home region), searched under [`PREVIEW_MAX_PATH_NODES`]: to the goal's
/// tile in `here`, else to `game::edge_toward`'s edge tile.
pub fn preview_path(src: &impl TileSource, from: TilePos, goal: &Goal, here: RegionCoords) -> Option<Vec<TilePos>> {
    let to = if goal.region == here {
        game::nearest_standable(src, goal.column, goal.y)?
    } else {
        game::edge_toward(src, from, goal, here)?
    };
    game::find_path(src, from, to, PREVIEW_MAX_PATH_NODES)
}

/// The live path preview.
#[derive(Resource, Default, Debug)]
pub struct PathPreview {
    /// (body tile, hovered tile) the preview was computed for.
    pub key: Option<(TilePos, TilePos)>,
    /// The path, home-local tiles; `None` = no path.
    pub path: Option<Vec<TilePos>>,
    /// The hover's world point when it lies in another region (the path
    /// then ends at the facing edge).
    pub beyond: Option<Vec3>,
    /// The goal a click would send.
    pub goal: Option<Goal>,
    /// The hover's world point.
    pub hover: Option<Vec3>,
}

impl PathPreview {
    /// Re-plan for the body on `from` hovering `hover` (its tile key, goal and
    /// world point), but only when the key changed; `true` when it re-planned.
    pub fn update(
        &mut self,
        src: &impl TileSource,
        from: TilePos,
        hover: TilePos,
        goal: Goal,
        here: RegionCoords,
        world: Vec3,
    ) -> bool {
        self.hover = Some(world);
        if self.key == Some((from, hover)) && self.goal == Some(goal) {
            return false;
        }
        self.key = Some((from, hover));
        self.goal = Some(goal);
        self.path = preview_path(src, from, &goal, here);
        self.beyond = (goal.region != here).then_some(world);
        true
    }

    pub fn clear(&mut self) {
        *self = PathPreview::default();
    }
}

/// The last sent path, drawn dimmed until the body gets there.
#[derive(Resource, Default, Debug)]
pub struct SentPath {
    pub region: Option<RegionId>,
    pub path: Vec<TilePos>,
    pub beyond: Option<Vec3>,
}

/// The local body's home-local feet tile as the sim would start a walk from
/// it (`game::current_tile`), when it stands in `home`.
fn body_tile(
    bodies: &Query<(&SimEntity, &GlobalTransform), With<LocalBody>>,
    mirror: &TileMirror,
    home: RegionId,
) -> Option<(TilePos, (RegionId, game::EntityKey))> {
    let (sim, gt) = bodies.iter().find(|(s, _)| s.region == home)?;
    let o = home.world_offset();
    let t = gt.translation();
    let feet = game::feet_of(Vector::new(t.x - o[0], t.y - o[1], t.z - o[2]));
    Some((game::current_tile(mirror, feet)?, (sim.region, sim.key)))
}

/// Keep the preview on the hover: cleared while move orders are off.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn update_path_preview(
    mode: Res<ClientPlayMode>,
    selection: Res<Selection>,
    policy: Res<CursorPolicy>,
    pick: Res<GroundPick>,
    mirror: Res<TileMirror>,
    bodies: Query<(&SimEntity, &GlobalTransform), With<LocalBody>>,
    mut preview: ResMut<PathPreview>,
    mut sent: ResMut<SentPath>,
) {
    let home = mirror.region();
    let body = home.and_then(|home| body_tile(&bodies, &mirror, home).map(|b| (home, b)));
    // The sent path is done once the body stands on its end, or left with it.
    if !sent.path.is_empty() {
        let arrived = body.is_some_and(|(_, (tile, _))| sent.beyond.is_none() && sent.path.last() == Some(&tile));
        if arrived || sent.region != home {
            *sent = SentPath::default();
        }
    }
    // A right-drag turning the view keeps the preview of its press (the
    // click, if it is one, sends that).
    if policy.orbit {
        return;
    }
    let active = mode.adventure() && !policy.ui_pointer && !policy.blocks_game();
    let hover = match (active, body, pick.0) {
        (true, Some((home, (from, key))), Some((region, local))) if selection.0.contains(&key) => {
            Some((home, from, region, local))
        }
        _ => None,
    };
    let Some((home, from, region, local)) = hover else {
        if preview.key.is_some() || preview.hover.is_some() {
            preview.clear();
        }
        return;
    };
    let goal = goal_from_hover(region, local, pick.1, home);
    let world = local + Vec3::from_array(region.world_offset());
    preview.update(&*mirror, from, hover_tile(region, local, home), goal, home, world);
}

/// A right click (a press released as [`is_click`]) sends `Order::MoveTo`
/// for the tile hovered at the press, when the preview found a path there.
pub fn send_move_order(
    buttons: Res<ButtonInput<MouseButton>>,
    press: Res<RightPress>,
    preview: Res<PathPreview>,
    mirror: Res<TileMirror>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
    mut pending: Local<Option<(Goal, Vec<TilePos>, Option<Vec3>)>>,
    mut sent: ResMut<SentPath>,
) {
    if buttons.just_pressed(MouseButton::Right) && press.at.is_some() {
        *pending = match (preview.goal, &preview.path) {
            (Some(goal), Some(path)) => Some((goal, path.clone(), preview.beyond)),
            _ => None,
        };
    }
    let Some(motion) = press.released else { return };
    let Some((goal, path, beyond)) = pending.take() else { return };
    let Some(client) = local.0 else { return };
    if !is_click(motion) {
        return;
    }
    let _ = events.0.send(game::GameEventKind::Order(client, Order::MoveTo(goal)));
    *sent = SentPath { region: mirror.region(), path, beyond };
}

/// World point of tile `p`'s floor centre in `home`, lifted
/// [`PATH_LIFT_M`].
fn tile_point(p: TilePos, home: RegionId) -> Vec3 {
    let bv = BLOCK_VOXELS as f32;
    let o = Vec3::from_array(home.world_offset());
    o + Vec3::new((p[0] as f32 + 0.5) * bv, p[1] as f32 * bv + PATH_LIFT_M * UNITS_PER_METER, (p[2] as f32 + 0.5) * bv)
}

/// A dashed line from `a` to `b`.
fn dashed(gizmos: &mut Gizmos, a: Vec3, b: Vec3, color: Color) {
    let len = a.distance(b);
    let dash = DASH_M * UNITS_PER_METER;
    if len <= f32::EPSILON {
        return;
    }
    let dir = (b - a) / len;
    let mut t = 0.0;
    while t < len {
        gizmos.line(a + dir * t, a + dir * (t + dash).min(len), color);
        t += 2.0 * dash;
    }
}

fn draw_path(
    gizmos: &mut Gizmos,
    hidden: &mut Gizmos<HiddenEdgeGizmos>,
    path: &[TilePos],
    beyond: Option<Vec3>,
    home: RegionId,
    color: Color,
) {
    let points: Vec<Vec3> = path.iter().map(|&p| tile_point(p, home)).collect();
    gizmos.linestrip(points.iter().copied(), color);
    hidden.linestrip(points.iter().copied(), color.with_alpha(color.alpha() * HIDDEN_PATH_ALPHA));
    if let (Some(end), Some(b)) = (points.last(), beyond) {
        dashed(gizmos, *end, b + Vec3::Y * PATH_LIFT_M * UNITS_PER_METER, color);
    }
}

/// Draw the last sent path (dimmed), then the live preview: the path, or a
/// red cross at the hover when there is none.
pub fn draw_paths(
    preview: Res<PathPreview>,
    sent: Res<SentPath>,
    mirror: Res<TileMirror>,
    mut gizmos: Gizmos,
    mut hidden: Gizmos<HiddenEdgeGizmos>,
) {
    let Some(home) = mirror.region() else { return };
    if sent.region == Some(home) && !sent.path.is_empty() {
        let dim = PATH_COLOR_OK.with_alpha(SENT_PATH_ALPHA);
        draw_path(&mut gizmos, &mut hidden, &sent.path, sent.beyond, home, dim);
    }
    match (&preview.path, preview.hover) {
        (Some(path), _) => draw_path(&mut gizmos, &mut hidden, path, preview.beyond, home, PATH_COLOR_OK),
        (None, Some(at)) => {
            let r = NO_PATH_CROSS_M * UNITS_PER_METER;
            let at = at + Vec3::Y * PATH_LIFT_M * UNITS_PER_METER;
            for d in [Vec3::new(r, 0.0, r), Vec3::new(r, 0.0, -r)] {
                gizmos.line(at - d, at + d, PATH_COLOR_NONE);
                hidden.line(at - d, at + d, PATH_COLOR_NONE.with_alpha(HIDDEN_PATH_ALPHA));
            }
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use game::BlockId;

    #[test]
    fn is_click_under_threshold() {
        assert!(is_click(0.0));
        assert!(is_click(selection::DRAG_THRESHOLD_PX - 0.1));
        assert!(!is_click(selection::DRAG_THRESHOLD_PX));
        assert!(!is_click(40.0));
    }

    /// A flat one-block floor at y = 0 that counts its block reads.
    struct CountingFloor {
        reads: Cell<u32>,
    }

    impl TileSource for CountingFloor {
        fn block(&self, p: TilePos) -> BlockId {
            self.reads.set(self.reads.get() + 1);
            if p[1] == 0 { BlockId(1) } else { BlockId::AIR }
        }
        fn is_bedrock(&self, _: TilePos) -> bool {
            false
        }
        fn edge_locked(&self, _: TilePos) -> bool {
            false
        }
        fn is_constructed(&self, _: TilePos) -> bool {
            false
        }
        fn tree_id(&self, _: TilePos) -> u32 {
            0
        }
        fn tree_limb(&self, _: TilePos) -> u8 {
            0
        }
    }

    #[test]
    fn preview_recomputes_only_on_key_change() {
        let src = CountingFloor { reads: Cell::new(0) };
        let home = RegionCoords { x: 0, z: 0 };
        let goal = Goal { region: home, column: [8, 6], y: Some(1) };
        let mut preview = PathPreview::default();
        assert!(preview.update(&src, [2, 1, 2], [8, 1, 6], goal, home, Vec3::ZERO));
        assert_eq!(preview.path.as_ref().and_then(|p| p.last().copied()), Some([8, 1, 6]));
        let reads = src.reads.get();
        assert!(reads > 0);
        // Same body tile, same hover (the cursor moved within the tile).
        assert!(!preview.update(&src, [2, 1, 2], [8, 1, 6], goal, home, Vec3::ONE));
        assert_eq!(src.reads.get(), reads, "no search for an unchanged key");
        // The body stepped: re-planned.
        assert!(preview.update(&src, [3, 1, 2], [8, 1, 6], goal, home, Vec3::ONE));
        assert!(src.reads.get() > reads);
    }

    #[test]
    fn inexact_pick_meets_the_ground_plane() {
        let hit = selection::plane_hit(Vec3::new(0.0, 100.0, 0.0), Vec3::new(1.0, -1.0, 0.0), 40.0);
        assert_eq!(hit, Some(Vec3::new(60.0, 40.0, 0.0)));
        assert_eq!(selection::plane_hit(Vec3::Y * 100.0, Vec3::Y, 40.0), None, "behind the eye");
        assert_eq!(selection::plane_hit(Vec3::Y * 100.0, Vec3::X, 40.0), None, "parallel");
    }

    #[test]
    fn goal_from_hover_in_neighbour_region() {
        let home = RegionCoords { x: 0, z: 0 };
        let east = RegionCoords { x: 1, z: 0 };
        let bv = BLOCK_VOXELS as f32;
        let local = Vec3::new(5.5 * bv, 3.0 * bv, 12.25 * bv);
        assert_eq!(goal_from_hover(east, local, true, home), Goal { region: east, column: [5, 12], y: None });
        // At home an exact pick keeps its level; an inexact one does not.
        assert_eq!(goal_from_hover(home, local, true, home), Goal { region: home, column: [5, 12], y: Some(3) });
        assert_eq!(goal_from_hover(home, local, false, home).y, None);
        let n = REGION_BLOCKS as i32;
        assert_eq!(hover_tile(east, local, home), [5 + n, 3, 12]);
    }
}
