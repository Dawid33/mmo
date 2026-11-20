//! Adventure move orders: a right click on the ground walks the local body
//! there, and the hover shows the path it would take first.
//!
//! Active only in Adventure, with the local body selected and the pointer
//! not on the UI. The hover's tile is the one block selection picks: with
//! the cut off the air in front of the block face under the cursor, at
//! whatever level (`designate::pick_visible`), with it on the working
//! level's cell; off the home region's blocks, the ground pick's column
//! ([`goal_from_hover`]). It is pathed from the
//! body's tile over the home region's [`TileMirror`] and its neighbours' halo
//! band (`TileMirror::with_halo`, what the sim's region reads) by the sim's
//! own planner, `game::PathPlan` — through the halo to a hover there, and for
//! a hover farther away into the next region of the route, with a dashed
//! line on to the hover. The search runs on the main
//! thread, only when the (body tile, hover tile) pair changes
//! ([`PathPreview::update`]), and under [`PREVIEW_MAX_PATH_NODES`] rather
//! than the sim's `MAX_PATH_NODES`, so a hover never costs a frame more than
//! a few ms. A capped search that fails cannot tell "unreachable" from "out
//! of budget", so it shows as "not previewed" (an amber cross); a click there
//! runs one search at the sim's full budget ([`click_path`]) and sends the
//! order if it finds a path — else, for a hover in the home region, the
//! hover turns to "no path" (a red cross: the sim, with the same inputs and
//! budget, would refuse it too). A hover outside the home region is always
//! sent ([`click_order`]): with no path known it stays amber ("not
//! previewed") and the sim decides.
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
use game::{Goal, Order, PathPlan, RouteMap, RegionCoords, RegionId, TilePos, TileSource, BLOCK_VOXELS, REGION_BLOCKS};

use super::adventure::ClientPlayMode;
use super::designate::{self, HiddenEdgeGizmos};
use super::level::WorkingLevel;
use super::CommanderView;
use bevy::window::PrimaryWindow;
use super::selection::{self, GroundPick, Selection};
use super::tiles::TileMirror;
use super::RightPress;
use crate::renderer::bridge::{LocalBody, SimEntity};
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::{GameEvents, LocalPlayer};
use crate::wrap_frame::WrapFrame;

/// Expansion budget of one preview search, in nodes. The sim searches up to
/// `game::MAX_PATH_NODES` (32 768), which measured ~87 ms on the main thread
/// (release, 512 m region, worst case); 2048 measured ~4.4 ms.
pub const PREVIEW_MAX_PATH_NODES: u32 = 2048;
/// The previewed path.
pub const PATH_COLOR_OK: Color = Color::srgb(0.35, 0.95, 0.45);
/// The cross at a hover with no path (none within the sim's budget either).
pub const PATH_COLOR_NONE: Color = Color::srgb(0.95, 0.25, 0.2);
/// The cross at a hover the capped preview search could not reach: a click
/// still tries at the sim's budget.
pub const PATH_COLOR_UNKNOWN: Color = Color::srgb(1.0, 0.7, 0.15);
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
/// standable tile of the column). `region` may be any copy of a region in
/// `frame`; the goal names its canonical form (the sims' and the server's).
pub fn goal_from_hover(frame: &WrapFrame, region: RegionId, local: Vec3, exact: bool, home: RegionId) -> Goal {
    let region = frame.canonical(region);
    let t = game::feet_tile(Vector::new(local.x, local.y, local.z));
    Goal::local(region, [t[0], t[2]], (exact && region == frame.canonical(home)).then_some(t[1]))
}

/// The `MoveTo` goal for a picked block-selection tile in `home`: that
/// tile's column, at its level.
pub fn goal_from_tile(tile: TilePos, home: RegionId) -> Goal {
    Goal::local(home, [tile[0], tile[2]], Some(tile[1]))
}

/// The tile block selection picks under the cursor ray in the mirrored
/// home region: with the cut off the air in front of the first block face
/// the ray meets, at any level; with it on the working level's cell. `None`
/// when the ray meets none of the region's blocks. `xray`: the cut-off view
/// looks through to dug spaces (`designate::pick_visible_xray`).
pub fn pick_move_tile(ray: (Vector, Vector), working: &WorkingLevel, mirror: &TileMirror, xray: bool) -> Option<TilePos> {
    let p = if working.cut {
        game::pick_level_cell(ray.0, ray.1, working.level?, |p| game::block_props().blocks_ray(mirror.block(p)))?
    } else {
        designate::pick_visible_xray(ray, true, mirror, xray)?
    };
    let n = REGION_BLOCKS as i32;
    ((0..n).contains(&p[0]) && (0..n).contains(&p[2])).then_some(p)
}

/// The hovered tile in `home`'s block coordinates (outside `0..REGION_BLOCKS`
/// for a hover in another region; across a wrapping map's seam by the nearest
/// copy, `WrapFrame::delta`): the preview's key.
pub fn hover_tile(frame: &WrapFrame, region: RegionId, local: Vec3, home: RegionId) -> TilePos {
    let n = REGION_BLOCKS as i32;
    let t = game::feet_tile(Vector::new(local.x, local.y, local.z));
    let (dx, dz) = frame.delta(home, region);
    [t[0] + dx * n, t[1], t[2] + dz * n]
}

/// The stretch the sim would plan from `from` toward `goal` (`here` is the
/// home region, `map` the world's extent), searched under `max_nodes`: the
/// sim's own `game::PathPlan::plan`, over the same tiles when `src` is the
/// mirror with its halo.
pub fn plan_path(
    src: &impl TileSource,
    from: TilePos,
    goal: &Goal,
    here: RegionCoords,
    map: Option<&RouteMap>,
    max_nodes: u32,
) -> Option<PathPlan> {
    PathPlan::plan(src, from, goal, here, map, max_nodes)
}

/// [`plan_path`] under [`PREVIEW_MAX_PATH_NODES`].
pub fn preview_path(
    src: &impl TileSource,
    from: TilePos,
    goal: &Goal,
    here: RegionCoords,
    map: Option<&RouteMap>,
) -> Option<PathPlan> {
    plan_path(src, from, goal, here, map, PREVIEW_MAX_PATH_NODES)
}

/// The path a click sends: the preview's (`previewed`, searched under the
/// capped budget) when it found one, else one search at the sim's
/// `game::MAX_PATH_NODES` with the same inputs. `None` = the sim would find
/// no path either: send nothing.
pub fn click_path(
    src: &impl TileSource,
    from: TilePos,
    goal: &Goal,
    here: RegionCoords,
    map: Option<&RouteMap>,
    previewed: Option<PathPlan>,
) -> Option<PathPlan> {
    previewed.or_else(|| plan_path(src, from, goal, here, map, game::MAX_PATH_NODES))
}

/// The live path preview.
#[derive(Resource, Default, Debug)]
pub struct PathPreview {
    /// (body tile, hovered tile) the preview was computed for.
    pub key: Option<(TilePos, TilePos)>,
    /// The planned stretch; `None` = no path within the preview's budget
    /// (see `unreachable`).
    pub path: Option<PathPlan>,
    /// With no `path`: a full-budget search (a click) found none either.
    pub unreachable: bool,
    /// The hover's world point when the path stops short of it (it ends in
    /// the next region of the route toward a hover farther away).
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
        map: Option<&RouteMap>,
        world: Vec3,
    ) -> bool {
        self.hover = Some(world);
        if self.key == Some((from, hover)) && self.goal == Some(goal) {
            return false;
        }
        self.key = Some((from, hover));
        self.goal = Some(goal);
        self.path = preview_path(src, from, &goal, here, map);
        self.unreachable = false;
        self.beyond = self.path.as_ref().is_some_and(|p| !p.reaches_goal).then_some(world);
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
/// it (`game::current_tile`), when it stands in `home` (drawn at `origin`).
fn body_tile(
    bodies: &Query<(&SimEntity, &GlobalTransform), With<LocalBody>>,
    mirror: &impl TileSource,
    home: RegionId,
    origin: Vec3,
) -> Option<(TilePos, (RegionId, game::EntityKey))> {
    let (sim, gt) = bodies.iter().find(|(s, _)| s.region == home)?;
    let o = origin;
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
    working: Res<WorkingLevel>,
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    bodies: Query<(&SimEntity, &GlobalTransform), With<LocalBody>>,
    xray: Option<Res<crate::renderer::voxel_pull::xray::XraySettings>>,
    world_map: Option<Res<crate::renderer::world_map::WorldMapState>>,
    frame: Option<Res<WrapFrame>>,
    roots: Res<crate::renderer::bridge::RegionRoots>,
    mut preview: ResMut<PathPreview>,
    mut sent: ResMut<SentPath>,
) {
    let home = mirror.region();
    let tiles = mirror.with_halo();
    let route = route_of(world_map.as_deref());
    let map = route.as_deref();
    let frame = frame.as_deref().copied().unwrap_or_default();
    let origin = |home: RegionId| Vec3::from_array(frame.offset(home));
    // A home whose tree the frame moved away from has no body here yet.
    let body = home
        .filter(|home| roots.drawn(*home))
        .and_then(|home| body_tile(&bodies, &tiles, home, origin(home)).map(|b| (home, b)));
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
    // Block selection's tile first; the ground pick only off the region's blocks.
    let ray = window.single().ok().zip(view.single().ok()).and_then(|(w, v)| designate::cursor_ray(w, v, home, &frame));
    if let Some(tile) = ray.and_then(|ray| pick_move_tile(ray, &working, &mirror, crate::renderer::voxel_pull::xray::xray_on(xray.as_deref(), working.cut))) {
        preview.update(&tiles, from, tile, goal_from_tile(tile, home), home, map, tile_point(tile, origin(home)));
        return;
    }
    let goal = goal_from_hover(&frame, region, local, pick.1, home);
    let world = local + Vec3::from_array(frame.offset(region));
    preview.update(&tiles, from, hover_tile(&frame, region, local, home), goal, home, map, world);
}

/// The world's coarse-route map, as the sim's regions are bounded by it and
/// route over it (`Region::set_route_map`); `None` without a map
/// (procedural).
fn route_of(state: Option<&crate::renderer::world_map::WorldMapState>) -> Option<std::sync::Arc<RouteMap>> {
    state.and_then(|s| s.route.clone())
}

/// What a right click on a hover does.
#[derive(Debug, Clone, PartialEq)]
pub enum ClickOrder {
    /// Send `Order::MoveTo`: along `plan` (`None` when the client could not
    /// plan it), drawn with a dashed line on to `beyond`.
    Send { plan: Option<PathPlan>, beyond: Option<Vec3> },
    /// The goal is in the home region and the sim, planning over the same
    /// tiles at the same budget, would find no path: nothing is sent.
    NoPath,
}

/// The click's order for a hover at world point `hover` with goal `goal`:
/// along [`click_path`]'s path when there is one. Without one, a goal in the
/// home region is refused ([`ClickOrder::NoPath`]); a goal outside it is
/// sent anyway, unplanned — the mirror may not know the terrain on the way
/// (past the halo, or a halo not mirrored yet) and the sim decides.
pub fn click_order(
    src: &impl TileSource,
    from: TilePos,
    goal: &Goal,
    here: RegionCoords,
    map: Option<&RouteMap>,
    previewed: Option<PathPlan>,
    hover: Option<Vec3>,
) -> ClickOrder {
    match click_path(src, from, goal, here, map, previewed) {
        Some(plan) => {
            let beyond = (!plan.reaches_goal).then_some(hover).flatten();
            ClickOrder::Send { plan: Some(plan), beyond }
        }
        None if goal.region() != Some(here) => ClickOrder::Send { plan: None, beyond: hover },
        None => ClickOrder::NoPath,
    }
}

/// The hover a right press was made on, kept until its release.
#[derive(Debug, Clone)]
pub struct PressedHover {
    pub key: (TilePos, TilePos),
    pub goal: Goal,
    pub path: Option<PathPlan>,
    /// The hover's world point (the dashed line's end, if the sent path
    /// stops short of it).
    pub beyond: Option<Vec3>,
}

/// A right click (a press released as [`is_click`]) sends `Order::MoveTo`
/// for the tile hovered at the press, along the preview's path or, when the
/// capped preview found none, one found at the sim's budget
/// ([`click_order`]). A goal in the home region with no path at all sends
/// nothing and the hover shows "no path"; one outside it is always sent.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn send_move_order(
    buttons: Res<ButtonInput<MouseButton>>,
    press: Res<RightPress>,
    mut preview: ResMut<PathPreview>,
    mirror: Res<TileMirror>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
    world_map: Option<Res<crate::renderer::world_map::WorldMapState>>,
    mut pending: crate::session::SessionLocal<Option<PressedHover>>,
    mut sent: ResMut<SentPath>,
) {
    let pending = pending.get();
    if buttons.just_pressed(MouseButton::Right) && press.at.is_some() {
        *pending = match (preview.key, preview.goal) {
            (Some(key), Some(goal)) if !preview.unreachable => {
                Some(PressedHover { key, goal, path: preview.path.clone(), beyond: preview.hover })
            }
            _ => None,
        };
    }
    let Some(motion) = press.released else { return };
    let Some(hover) = pending.take() else { return };
    let (Some(client), Some(here)) = (local.0, mirror.region()) else { return };
    if !is_click(motion) {
        return;
    }
    let route = route_of(world_map.as_deref());
    let order = click_order(&mirror.with_halo(), hover.key.0, &hover.goal, here, route.as_deref(), hover.path, hover.beyond);
    let ClickOrder::Send { plan, beyond } = order else {
        // Truly unreachable: show it, for as long as the hover stays.
        if preview.key == Some(hover.key) && preview.path.is_none() {
            preview.unreachable = true;
        }
        return;
    };
    let path = plan.as_ref().map(|p| p.local_path(here)).unwrap_or_default();
    if preview.key == Some(hover.key) && preview.path.is_none() {
        preview.path = plan;
    }
    let _ = events.0.send(game::GameEventKind::Order(client, Order::MoveTo(hover.goal)));
    *sent = SentPath { region: Some(here), path, beyond };
}

/// World point of tile `p`'s floor centre in the home region drawn at
/// `origin`, lifted [`PATH_LIFT_M`].
fn tile_point(p: TilePos, origin: Vec3) -> Vec3 {
    let bv = BLOCK_VOXELS as f32;
    origin + Vec3::new((p[0] as f32 + 0.5) * bv, p[1] as f32 * bv + PATH_LIFT_M * UNITS_PER_METER, (p[2] as f32 + 0.5) * bv)
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
    origin: Vec3,
    color: Color,
) {
    let points: Vec<Vec3> = path.iter().map(|&p| tile_point(p, origin)).collect();
    gizmos.linestrip(points.iter().copied(), color);
    hidden.linestrip(points.iter().copied(), color.with_alpha(color.alpha() * HIDDEN_PATH_ALPHA));
    if let (Some(end), Some(b)) = (points.last(), beyond) {
        dashed(gizmos, *end, b + Vec3::Y * PATH_LIFT_M * UNITS_PER_METER, color);
    }
}

/// Draw the last sent path (dimmed), then the live preview: the path, or a
/// cross at the hover when there is none — amber while only the capped
/// preview search failed, red once a full-budget search did too.
pub fn draw_paths(
    preview: Res<PathPreview>,
    sent: Res<SentPath>,
    mirror: Res<TileMirror>,
    mut gizmos: Gizmos,
    mut hidden: Gizmos<HiddenEdgeGizmos>,
) {
    let (Some(home), Some(origin)) = (mirror.region(), mirror.home_offset()) else { return };
    if sent.region == Some(home) && !sent.path.is_empty() {
        let dim = PATH_COLOR_OK.with_alpha(SENT_PATH_ALPHA);
        draw_path(&mut gizmos, &mut hidden, &sent.path, sent.beyond, origin, dim);
    }
    match (&preview.path, preview.hover) {
        (Some(plan), _) => draw_path(&mut gizmos, &mut hidden, &plan.local_path(home), preview.beyond, origin, PATH_COLOR_OK),
        (None, Some(at)) => {
            let color = if preview.unreachable { PATH_COLOR_NONE } else { PATH_COLOR_UNKNOWN };
            let r = NO_PATH_CROSS_M * UNITS_PER_METER;
            let at = at + Vec3::Y * PATH_LIFT_M * UNITS_PER_METER;
            for d in [Vec3::new(r, 0.0, r), Vec3::new(r, 0.0, -r)] {
                gizmos.line(at - d, at + d, color);
                hidden.line(at - d, at + d, color.with_alpha(HIDDEN_PATH_ALPHA));
            }
        }
        (None, None) => {}
    }
}

