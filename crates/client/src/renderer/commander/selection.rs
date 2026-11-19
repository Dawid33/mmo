//! Client-local selection and the ground pick.
//!
//! Left click selects the entity whose body on screen — its feet-to-head
//! axis, as wide as the body — is nearest the cursor (within
//! [`CLICK_SELECT_RADIUS_PX`] of it, so a click anywhere on a dwarf hits it at
//! any zoom), left-drag selects everything whose projected centre falls in
//! the box, Ctrl adds to the selection (Shift is the level-down key), and a click on
//! empty ground clears it. Nothing here reaches the sim: orders (a right
//! click, designations) will be sim events aimed at [`GroundPick`].

use std::collections::BTreeSet;

use bevy::color::palettes::css;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::{EntityKey, EntityKind, RegionId};

use super::cut_entities::CutHidden;
use super::height::HeightGrid;
use super::level::WorkingLevel;
use super::minimap::{self, MinimapPointer};
use super::tiles::TileMirror;
use super::CommanderView;
use super::adventure::ClientPlayMode;
use crate::renderer::bridge::{LocalBody, SimEntity, SimEntityMap, SimKind};
use crate::renderer::cursor::CursorPolicy;

/// A click selects the nearest entity whose body on screen is this close
/// (slack beyond its projected width, so a far, tiny one is still hittable).
pub const CLICK_SELECT_RADIUS_PX: f32 = 12.0;
/// A left press that moves farther than this becomes a box drag.
pub const DRAG_THRESHOLD_PX: f32 = 4.0;
/// Ground-pick march step along the cursor ray, metres.
pub const PICK_STEP_M: f32 = 0.5;
/// How far a ground pick (the cursor ray, the orbit pivot) looks, metres.
/// Picks only meet the height grid of loaded near terrain, and the march is
/// linear in it, so it is NOT the view distance (3 km at the play sizes):
/// the old 1 km horizon.
pub const PICK_REACH_M: f32 = 1024.0;
/// Bisection rounds refining a ground-pick hit.
pub const PICK_REFINE_STEPS: u32 = 8;
/// Selection ring radius as a share of the entity's footprint edge.
pub const RING_RADIUS_FACTOR: f32 = 0.8;

/// The selected entities, as stable sim keys.
#[derive(Resource, Default, Debug)]
pub struct Selection(pub BTreeSet<(RegionId, EntityKey)>);

impl Selection {
    pub fn contains(&self, region: RegionId, key: EntityKey) -> bool {
        self.0.contains(&(region, key))
    }
}

/// The terrain point under the cursor: the region it lies in and the point in
/// that region's local frame (sim units, as sim events use) — `None` off the
/// loaded terrain. Cut-away rock is not in the way
/// ([`super::visible::visible_ground`]). Orders and designations aim at it.
///
/// The flag is `true` for an exact hit on the loaded terrain. Where the ray
/// meets none, [`fallback_ground_pick`] puts the pick on the plane `y =
/// cam.ground_y` and marks it inexact (`false`): good for aiming a move order
/// at unloaded ground, never for a designation.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct GroundPick(pub Option<(RegionId, Vec3)>, pub bool);

/// The rubber-band box while a box drag is under way, window logical
/// pixels; drawn by the HUD layer (`HudUi`, green fill + border).
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct SelectionBand(pub Option<Rect>);

/// The band for a left press at `press` (`None` when it didn't start on the
/// ground) with the pointer now at `cursor`: shown only while the button is
/// held and the drag has passed [`DRAG_THRESHOLD_PX`].
pub fn band_rect(press: Option<Vec2>, cursor: Option<Vec2>, held: bool) -> Option<Rect> {
    match (press, cursor) {
        (Some(start), Some(now)) if held && start.distance(now) > DRAG_THRESHOLD_PX => {
            Some(Rect::from_corners(start, now))
        }
        _ => None,
    }
}

/// What can be selected: everything but players (commanders have no body),
/// except the local player's own body in Adventure — `local_player` is true
/// only for that body while the client plays it.
pub fn is_selectable(kind: EntityKind, local_player: bool) -> bool {
    !matches!(kind, EntityKind::Player) || local_player // people and slimes always
}

/// A selectable as the screen sees it: its centre, its vertical axis from
/// feet to head, and its half-width, all in viewport pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenBody {
    pub centre: Vec2,
    pub feet: Vec2,
    pub head: Vec2,
    pub half_width: f32,
}

impl ScreenBody {
    /// A point-sized body (no extent).
    pub fn point(at: Vec2) -> Self {
        Self { centre: at, feet: at, head: at, half_width: 0.0 }
    }

    /// Pixels from `p` to the body's outline (0 inside it).
    pub fn distance(&self, p: Vec2) -> f32 {
        let axis = self.head - self.feet;
        let t = if axis.length_squared() > 0.0 { ((p - self.feet).dot(axis) / axis.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
        (p.distance(self.feet + axis * t) - self.half_width).max(0.0)
    }
}

/// The candidate whose body is nearest `cursor`, within `slack` pixels of
/// its outline; inside several, the one whose axis is nearest.
pub fn click_pick<K: Copy>(
    cursor: Vec2,
    candidates: impl IntoIterator<Item = (K, ScreenBody)>,
    slack: f32,
) -> Option<K> {
    candidates
        .into_iter()
        .map(|(k, b)| (k, b.distance(cursor), ScreenBody { half_width: 0.0, ..b }.distance(cursor)))
        .filter(|&(_, d, _)| d <= slack)
        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)))
        .map(|(k, _, _)| k)
}

/// Every candidate inside the box spanned by two corners.
pub fn box_pick<K: Copy>(a: Vec2, b: Vec2, candidates: impl IntoIterator<Item = (K, Vec2)>) -> Vec<K> {
    let rect = Rect::from_corners(a, b);
    candidates.into_iter().filter(|(_, p)| rect.contains(*p)).map(|(k, _)| k).collect()
}

/// March a ray over a height field: the first point where it dips to the
/// surface, refined by bisection. `height(x, z)` is `None` where unknown
/// (those stretches are passed over). `dir` need not be normalised.
pub fn march_height(
    origin: Vec3,
    dir: Vec3,
    height: impl Fn(f32, f32) -> Option<f32>,
    step: f32,
    max_dist: f32,
) -> Option<Vec3> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO || step <= 0.0 {
        return None;
    }
    let below = |t: f32| {
        let p = origin + dir * t;
        height(p.x, p.z).is_some_and(|h| p.y <= h)
    };
    let mut prev = 0.0;
    let mut t = 0.0;
    while t <= max_dist {
        if below(t) {
            let (mut lo, mut hi) = (prev, t);
            for _ in 0..PICK_REFINE_STEPS {
                let mid = (lo + hi) / 2.0;
                if below(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let p = origin + dir * hi;
            return Some(Vec3::new(p.x, height(p.x, p.z).unwrap_or(p.y), p.z));
        }
        prev = t;
        t += step;
    }
    None
}

/// Cursor ray → terrain point.
pub fn update_ground_pick(
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    grid: Res<HeightGrid>,
    working: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    xray: Option<Res<crate::renderer::voxel_pull::xray::XraySettings>>,
    mut pick: ResMut<GroundPick>,
) {
    pick.0 = (|| {
        let cursor = window.single().ok()?.cursor_position()?;
        let (camera, tf) = view.single().ok()?;
        // The camera has no parent, so its Transform is its global pose —
        // this frame's, not last frame's propagated one.
        let ray = camera.viewport_to_world(&GlobalTransform::from(*tf), cursor).ok()?;
        let hit = super::visible::visible_ground(
            ray.origin,
            *ray.direction,
            PICK_REACH_M * game::movement::UNITS_PER_METER,
            &grid,
            &working,
            &tiles,
            crate::renderer::voxel_pull::xray::xray_on(xray.as_deref(), working.cut),
        )?;
        let region = RegionId::from_world(hit.x, hit.z);
        Some((region, hit - Vec3::from_array(region.world_offset())))
    })();
}

/// Where a ray from `origin` along `dir` crosses the plane at height `y`,
/// in front of the origin.
pub fn plane_hit(origin: Vec3, dir: Vec3, y: f32) -> Option<Vec3> {
    let t = (y - origin.y) / dir.y;
    (dir.y != 0.0 && t.is_finite() && t > 0.0).then(|| origin + dir * t)
}

/// After [`update_ground_pick`]: an exact pick is marked so; a miss falls
/// back to the cursor ray's crossing of the camera's ground plane, inexact.
pub fn fallback_ground_pick(
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    cam: Res<super::CommanderCamera>,
    mut pick: ResMut<GroundPick>,
) {
    if pick.0.is_some() {
        if !pick.1 {
            pick.1 = true;
        }
        return;
    }
    pick.1 = false;
    pick.0 = (|| {
        let cursor = window.single().ok()?.cursor_position()?;
        let (camera, tf) = view.single().ok()?;
        let ray = camera.viewport_to_world(&GlobalTransform::from(*tf), cursor).ok()?;
        let hit = plane_hit(ray.origin, *ray.direction, cam.ground_y)?;
        let region = RegionId::from_world(hit.x, hit.z);
        Some((region, hit - Vec3::from_array(region.world_offset())))
    })();
}

/// Half the body's height and its half-width, world units: feet and head
/// are the pose's centre ∓ the first.
fn body_extent(kind: EntityKind) -> (f32, f32) {
    let half_height = super::cut_entities::feet_offset(kind);
    let half_width = match kind {
        EntityKind::Player | EntityKind::Person { .. } => game::movement::PLAYER_CAPSULE_RADIUS,
        EntityKind::Slime { size } => game::slime::slime_edge_units(size) / 2.0,
        // Placeholder: the 1 m dev cube.
        EntityKind::Body => game::BLOCK_VOXELS as f32 / 2.0,
    };
    (half_height, half_width)
}

/// The selectables' bodies on screen this frame (units hidden by the cut
/// are not selectable). A body whose centre is off-screen or behind the
/// camera is left out.
fn projected(
    camera: &Camera,
    gt: &GlobalTransform,
    entities: &Query<(&SimEntity, &SimKind, &GlobalTransform, Has<LocalBody>), Without<CutHidden>>,
    adventure: bool,
) -> Vec<((RegionId, EntityKey), ScreenBody)> {
    let right = gt.right();
    entities
        .iter()
        .filter(|(_, kind, _, local)| is_selectable(kind.0, *local && adventure))
        .filter_map(|(sim, kind, e, _)| {
            let at = e.translation();
            let to_screen = |p: Vec3| camera.world_to_viewport(gt, p).ok();
            let centre = to_screen(at)?;
            let (half_height, half_width) = body_extent(kind.0);
            let feet = to_screen(at - Vec3::Y * half_height).unwrap_or(centre);
            let head = to_screen(at + Vec3::Y * half_height).unwrap_or(centre);
            let half_width = to_screen(at + right * half_width).map_or(0.0, |p| p.distance(centre));
            Some(((sim.region, sim.key), ScreenBody { centre, feet, head, half_width }))
        })
        .collect()
}

/// Left click / drag / Ctrl → the selection, and the rubber-band box.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn select_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    entities: Query<(&SimEntity, &SimKind, &GlobalTransform, Has<LocalBody>), Without<CutHidden>>,
    mode: Res<ClientPlayMode>,
    policy: Res<CursorPolicy>,
    minimap_pointer: Res<MinimapPointer>,
    mut selection: ResMut<Selection>,
    mut press: crate::session::SessionLocal<Option<Vec2>>,
    mut band: ResMut<SelectionBand>,
) {
    let press = press.get();
    let Ok(window) = window.single() else { return };
    let cursor = window.cursor_position();
    if buttons.just_pressed(MouseButton::Left) {
        // A press on the minimap or the game UI (or under an overlay) is not
        // a selection.
        *press = cursor.filter(|c| {
            !policy.blocks_game()
                && !policy.ui_pointer
                && !minimap_pointer.dragging
                && !minimap::screen_rect(window.size()).contains(*c)
        });
    }
    let dragging = |start: Vec2, now: Vec2| start.distance(now) > DRAG_THRESHOLD_PX;

    band.set_if_neq(SelectionBand(band_rect(*press, cursor, buttons.pressed(MouseButton::Left))));

    if !buttons.just_released(MouseButton::Left) {
        return;
    }
    let Some(start) = press.take() else { return };
    let Some(end) = cursor else { return };
    let Ok((camera, tf)) = view.single() else { return };
    let gt = GlobalTransform::from(*tf);
    let candidates = projected(camera, &gt, &entities, mode.adventure());
    let picked: Vec<_> = if dragging(start, end) {
        box_pick(start, end, candidates.into_iter().map(|(k, b)| (k, b.centre)))
    } else {
        click_pick(end, candidates, CLICK_SELECT_RADIUS_PX).into_iter().collect()
    };
    let add = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    if !add {
        selection.0.clear();
    }
    selection.0.extend(picked);
}

/// Forget keys whose entity the sim removed.
pub fn prune_selection(map: Res<SimEntityMap>, mut selection: ResMut<Selection>) {
    if selection.0.is_empty() || !map.is_changed() {
        return;
    }
    selection.0.retain(|key| map.0.contains_key(key));
}

/// A ring on the ground under each selected entity.
pub fn draw_selection_rings(
    selection: Res<Selection>,
    map: Res<SimEntityMap>,
    entities: Query<(&SimKind, &GlobalTransform), Without<CutHidden>>,
    mut gizmos: Gizmos,
) {
    for key in &selection.0 {
        let Some(&e) = map.0.get(key) else { continue };
        let Ok((kind, gt)) = entities.get(e) else { continue };
        let (edge, centre_height) = match kind.0 {
            EntityKind::Slime { size } => {
                let edge = game::slime::slime_edge_units(size);
                (edge, edge / 2.0)
            }
            // Placeholder: the 1 m dev cube.
            EntityKind::Body => {
                let edge = game::BLOCK_VOXELS as f32;
                (edge, edge / 2.0)
            }
            // Only the adventurer's own body is ever selected.
            EntityKind::Player | EntityKind::Person { .. } => (
                2.0 * game::movement::PLAYER_CAPSULE_RADIUS,
                super::cut_entities::feet_offset(kind.0),
            ),
        };
        // The sim pose is the body centre; the ring sits at the feet.
        let feet = gt.translation() - Vec3::Y * (centre_height - 1.0);
        gizmos.circle(
            Isometry3d::new(feet, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
            edge * RING_RADIUS_FACTOR,
            css::LIME,
        );
    }
}

