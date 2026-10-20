//! Client-local selection and the ground pick.
//!
//! Left click selects the entity nearest the cursor (within
//! [`CLICK_SELECT_RADIUS_PX`]), left-drag selects everything whose projected
//! position falls in the box, Shift adds to the selection, and a click on
//! empty ground clears it. Nothing here reaches the sim: orders (a right
//! click, designations) will be sim events aimed at [`GroundPick`].

use std::collections::BTreeSet;

use bevy::color::palettes::css;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::{EntityKey, EntityKind, RegionId};

use super::height::HeightGrid;
use super::minimap::{self, MinimapPointer};
use super::CommanderView;
use crate::renderer::bridge::{SimEntity, SimEntityMap, SimKind};
use crate::renderer::cursor::CursorPolicy;

/// A click selects the nearest entity whose screen position is this close.
pub const CLICK_SELECT_RADIUS_PX: f32 = 12.0;
/// A left press that moves farther than this becomes a box drag.
pub const DRAG_THRESHOLD_PX: f32 = 4.0;
/// Ground-pick march step along the cursor ray, metres.
pub const PICK_STEP_M: f32 = 0.5;
/// Bisection rounds refining a ground-pick hit.
pub const PICK_REFINE_STEPS: u32 = 8;
/// Selection ring radius as a share of the entity's footprint edge.
pub const RING_RADIUS_FACTOR: f32 = 0.8;

const BOX_FILL: Color = Color::srgba(0.4, 1.0, 0.4, 0.12);
const BOX_BORDER: Color = Color::srgba(0.4, 1.0, 0.4, 0.9);

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
/// loaded terrain. Orders and designations aim at it.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct GroundPick(pub Option<(RegionId, Vec3)>);

/// Marker: the rubber-band box node.
#[derive(Component)]
pub struct SelectionBox;

/// What can be selected: everything but players (commanders have no body).
pub fn is_selectable(kind: EntityKind) -> bool {
    !matches!(kind, EntityKind::Player)
}

/// The candidate nearest `cursor` within `radius` pixels.
pub fn click_pick<K: Copy>(
    cursor: Vec2,
    candidates: impl IntoIterator<Item = (K, Vec2)>,
    radius: f32,
) -> Option<K> {
    candidates
        .into_iter()
        .map(|(k, p)| (k, p.distance(cursor)))
        .filter(|&(_, d)| d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
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

pub fn setup_selection_box(mut commands: Commands) {
    commands.spawn((
        SelectionBox,
        Pickable::IGNORE,
        Node {
            position_type: PositionType::Absolute,
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        },
        BackgroundColor(BOX_FILL),
        BorderColor::all(BOX_BORDER),
        Visibility::Hidden,
    ));
}

/// Cursor ray → terrain point.
pub fn update_ground_pick(
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    grid: Res<HeightGrid>,
    mut pick: ResMut<GroundPick>,
) {
    pick.0 = (|| {
        let cursor = window.single().ok()?.cursor_position()?;
        let (camera, tf) = view.single().ok()?;
        // The camera has no parent, so its Transform is its global pose —
        // this frame's, not last frame's propagated one.
        let ray = camera.viewport_to_world(&GlobalTransform::from(*tf), cursor).ok()?;
        let unit = game::movement::UNITS_PER_METER;
        let hit = march_height(
            ray.origin,
            *ray.direction,
            |x, z| grid.surface_at(x, z),
            PICK_STEP_M * unit,
            game::lod::LOD_HORIZON_UNITS,
        )?;
        let region = RegionId::from_world(hit.x, hit.z);
        Some((region, hit - Vec3::from_array(region.world_offset())))
    })();
}

/// The selectables' screen positions this frame.
fn projected(
    camera: &Camera,
    gt: &GlobalTransform,
    entities: &Query<(&SimEntity, &SimKind, &GlobalTransform)>,
) -> Vec<((RegionId, EntityKey), Vec2)> {
    entities
        .iter()
        .filter(|(_, kind, _)| is_selectable(kind.0))
        .filter_map(|(sim, _, e)| {
            let p = camera.world_to_viewport(gt, e.translation()).ok()?;
            Some(((sim.region, sim.key), p))
        })
        .collect()
}

/// Left click / drag / Shift → the selection, and the rubber-band box.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn select_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    entities: Query<(&SimEntity, &SimKind, &GlobalTransform)>,
    policy: Res<CursorPolicy>,
    minimap_pointer: Res<MinimapPointer>,
    mut selection: ResMut<Selection>,
    mut press: Local<Option<Vec2>>,
    mut band: Query<(&mut Node, &mut Visibility), With<SelectionBox>>,
) {
    let Ok(window) = window.single() else { return };
    let cursor = window.cursor_position();
    if buttons.just_pressed(MouseButton::Left) {
        // A press on the minimap or the game UI (or under an overlay) is not
        // a selection.
        *press = cursor.filter(|c| {
            !policy.overlay_open
                && !policy.ui_pointer
                && !minimap_pointer.dragging
                && !minimap::screen_rect(window.size()).contains(*c)
        });
    }
    let dragging = |start: Vec2, now: Vec2| start.distance(now) > DRAG_THRESHOLD_PX;

    if let Ok((mut node, mut vis)) = band.single_mut() {
        match (*press, cursor) {
            (Some(start), Some(now)) if buttons.pressed(MouseButton::Left) && dragging(start, now) => {
                let r = Rect::from_corners(start, now);
                node.left = Val::Px(r.min.x);
                node.top = Val::Px(r.min.y);
                node.width = Val::Px(r.width());
                node.height = Val::Px(r.height());
                vis.set_if_neq(Visibility::Visible);
            }
            _ => {
                vis.set_if_neq(Visibility::Hidden);
            }
        }
    }

    if !buttons.just_released(MouseButton::Left) {
        return;
    }
    let Some(start) = press.take() else { return };
    let Some(end) = cursor else { return };
    let Ok((camera, tf)) = view.single() else { return };
    let gt = GlobalTransform::from(*tf);
    let candidates = projected(camera, &gt, &entities);
    let picked: Vec<_> = if dragging(start, end) {
        box_pick(start, end, candidates)
    } else {
        click_pick(end, candidates, CLICK_SELECT_RADIUS_PX).into_iter().collect()
    };
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if !shift {
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
    entities: Query<(&SimKind, &GlobalTransform)>,
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
            EntityKind::Player => continue,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_takes_the_nearest_within_the_radius() {
        let c = [(1, Vec2::new(100.0, 100.0)), (2, Vec2::new(105.0, 100.0)), (3, Vec2::new(300.0, 0.0))];
        assert_eq!(click_pick(Vec2::new(104.0, 100.0), c, 12.0), Some(2));
        assert_eq!(click_pick(Vec2::new(200.0, 200.0), c, 12.0), None, "empty ground");
    }

    #[test]
    fn box_takes_everything_inside_whichever_way_it_was_dragged() {
        let c = [(1, Vec2::new(10.0, 10.0)), (2, Vec2::new(50.0, 50.0)), (3, Vec2::new(90.0, 10.0))];
        let mut got = box_pick(Vec2::new(60.0, 60.0), Vec2::new(0.0, 0.0), c);
        got.sort();
        assert_eq!(got, vec![1, 2]);
    }

    #[test]
    fn march_finds_the_surface_under_a_slanted_ray() {
        // A step: ground at y = 10 for x < 50, y = 40 beyond.
        let height = |x: f32, _z: f32| Some(if x < 50.0 { 10.0 } else { 40.0 });
        let hit = march_height(Vec3::new(0.0, 100.0, 0.0), Vec3::new(1.0, -1.0, 0.0), height, 0.5, 1000.0)
            .expect("hits");
        // Reaches y = 50 at x = 50: above the low ground, then dips into the
        // cliff face at x = 50 (height 40 < 50, so it continues to y = 40 at x = 60).
        assert!((hit.x - 60.0).abs() < 0.5, "{hit:?}");
        assert_eq!(hit.y, 40.0);
    }

    #[test]
    fn march_passes_over_unknown_columns_and_gives_up_past_max() {
        let height = |x: f32, _z: f32| (x > 80.0).then_some(0.0);
        let hit = march_height(Vec3::new(0.0, 50.0, 0.0), Vec3::new(1.0, -0.5, 0.0), height, 0.5, 1000.0);
        assert!(hit.is_some_and(|p| (p.x - 100.0).abs() < 0.5), "{hit:?}");
        assert_eq!(march_height(Vec3::Y, Vec3::Y, |_, _| Some(0.0), 0.5, 100.0), None, "looking up");
    }
}
