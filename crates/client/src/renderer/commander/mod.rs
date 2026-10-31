//! The commander view (RTS Phase 2): the player has no body to look through.
//! A client-local camera looks down at the island, pans with the screen
//! edges, WASD / the arrow keys and a middle-drag, turns (yaw and pitch) with
//! a right-drag about the ground at the centre of the screen (marked while
//! the drag lasts). With the cut on ("Hide above") it rides the working
//! level's floor: Space / Shift step the level (see `level`), the wheel
//! zooms and pans scale with the view depth ([`pan_depth`]). With the cut off
//! it is a free camera with no zoom: Space / Shift raise and lower it at a
//! constant speed, PgUp / PgDn by one level. Above ground it
//! follows the terrain lazily ([`hold_baseline`]); held below its lowest
//! height it sinks through the surface into a free underground camera
//! ([`move_vertically`]) — the terrain mesh only has faces where rock meets
//! air, so from inside the rock every dug space shows from the outside in —
//! and rising back past the surface re-attaches it (Home does at once). The
//! free camera moves at constant speeds ([`PAN_SPEED_M_PER_S`],
//! [`VERTICAL_SPEED_M_PER_S`]); its wheel does nothing. None of it is sim state —
//! nothing in the sim depends on where a commander looks — so it lives here,
//! with the minimap (`minimap`), the client-local selection and the ground
//! pick (`selection`), all of which read the client height grid (`height`),
//! and the fortress designation tools (`designate`), which read the tile
//! mirror (`tiles`) at the working level (`level`); units above the
//! working level's cut are hidden (`cut_entities`).
//!
//! The math is pure functions over plain values (tested without a window);
//! the systems only gather input and write the results.
//!
//! The world is many regions; the commander plays in one — the region its
//! (inert) body is in, the home region. The camera starts over that region's
//! rectangle and the minimap shows it; the camera itself collides with
//! nothing and may fly past the region's edge. Travelling between regions moves the
//! body, which moves the home region and the near window with it.
//!
//! The sim player body still exists (it goes away in a later phase) but is
//! inert: the commander never sends it keys, mouse motion or clicks, so it
//! never turns on fps-cam mode, never walks and never edits blocks. The bridge
//! hides it and gives it no camera ([`CameraMode::Commander`]).

pub mod adventure;
pub mod bounds;
pub mod cut_entities;
pub mod designate;
pub mod follow;
pub mod height;
pub mod level;
pub mod minimap;
pub mod move_order;
pub mod selection;
pub mod tiles;
pub mod visible;

use crate::renderer::voxel_pull::xray::xray_on;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use game::movement::UNITS_PER_METER;

use super::cursor::CursorPolicy;
use crate::session::{world_systems_enabled, SessionResetExt};
use height::HeightGrid;

/// Which view the client renders from.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraMode {
    /// The legacy first-person view through the local player's sim camera.
    /// The default only for headless harnesses that build the bridge without
    /// `SimBridgePlugin` (they test the body's input path); the game itself
    /// picks with [`CameraMode::from_env`].
    #[default]
    FirstPerson,
    /// The RTS commander camera ([`CommanderPlugin`]).
    Commander,
}

impl CameraMode {
    /// `VOXEL_CAMERA=first-person` brings back the walking body's view (a dev
    /// tool until the body leaves the sim); anything else is the commander.
    /// The render benchmark and the autopilot probes (`just fly-probe` /
    /// `shuttle-probe`) fly the body's camera, so they keep first person.
    pub fn from_env() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        if super::render_bench::enabled() {
            return Self::FirstPerson;
        }
        if std::env::var("VOXEL_AUTOPILOT").is_ok_and(|v| !v.is_empty() && v != "0") {
            return Self::FirstPerson;
        }
        match std::env::var("VOXEL_CAMERA").as_deref() {
            Ok("first-person") | Ok("fps") => Self::FirstPerson,
            _ => Self::Commander,
        }
    }
}

// Tunables: starting points to tune by feel, in metres / degrees / seconds /
// logical pixels; converted to sim units (`UNITS_PER_METER`) at the boundary.

/// Camera pitch below the horizon the game opens at. A right-drag changes the
/// pitch within the limits below.
pub const PITCH_DEG: f32 = 55.0;
/// Shallowest pitch a right-drag reaches.
pub const PITCH_MIN_DEG: f32 = 15.0;
/// Steepest pitch (just short of straight down, where yaw degenerates).
pub const PITCH_MAX_DEG: f32 = 89.0;
/// Right-drag turn rate, per logical pixel of mouse motion.
pub const ORBIT_DEG_PER_PX: f32 = 0.25;
/// Highest the camera rises above the terrain-following height.
pub const RISE_MAX_M: f32 = 200.0;
/// Vertical field of view.
pub const FOV_Y_DEG: f32 = 40.0;
/// Lowest camera-to-focus distance above ground; Shift held past it sinks
/// the camera through the surface ([`move_vertically`]).
pub const ZOOM_NEAR_M: f32 = 5.0;
/// Highest camera-to-focus distance. The sun's shadows must reach twice this
/// far (`atmosphere::SHADOW_MAX_DISTANCE_M`, pinned by a test there): raise
/// that with it.
pub const ZOOM_FAR_M: f32 = 150.0;
/// Camera-to-focus distance the game opens at (and Home returns to).
pub const ZOOM_START_M: f32 = 45.0;
/// Cut on: distance factor per wheel notch toward the ground (its inverse away).
pub const ZOOM_STEP: f32 = 0.85;
/// Cut on: time constant the distance eases toward its wheel target with.
pub const ZOOM_SMOOTHING_S: f32 = 0.08;
/// Cut on: edge/keyboard pan speed, in view depths per second ([`pan_depth`]).
pub const PAN_ZOOMS_PER_S: f32 = 1.0;
/// Cut off: keyboard / screen-edge pan speed over the ground, metres per second.
pub const PAN_SPEED_M_PER_S: f32 = 30.0;
/// Space / Shift: straight up / down, metres per second.
pub const VERTICAL_SPEED_M_PER_S: f32 = 15.0;
/// Keys that raise / lower the camera while held.
pub const RISE_KEYS: [KeyCode; 1] = [KeyCode::Space];
pub const SINK_KEYS: [KeyCode; 2] = [KeyCode::ShiftLeft, KeyCode::ShiftRight];
/// Surface mode holds the camera's height while the ground under the focus
/// stays within this share of the eye's height above the focus, above or
/// below it ([`hold_baseline`]): trees and bumps pass under a high camera,
/// and only ground that comes this close (or drops this far) moves it.
pub const FOLLOW_BAND_FRAC: f32 = 0.5;
/// The dead band never shrinks below this, metres (zoomed right in).
pub const FOLLOW_BAND_MIN_M: f32 = 1.0;
/// The eye keeps at least this much air over the tallest column near it,
/// metres, whatever the band says ([`clearance_floor`]).
pub const MIN_CLEARANCE_M: f32 = 3.0;
/// Radius around the point under the eye searched for that column, metres.
pub const CLEARANCE_RADIUS_M: f32 = 4.0;
/// Width of the screen-edge band that pans.
pub const EDGE_BAND_PX: f32 = 8.0;
/// Time constant the focus height follows the terrain with.
pub const HEIGHT_SMOOTHING_S: f32 = 0.15;
/// Half-width of the square the focus height averages the terrain over.
pub const HEIGHT_SAMPLE_RADIUS_M: f32 = 6.0;
/// Spacing of the samples inside that square.
pub const HEIGHT_SAMPLE_STEP_M: f32 = 2.0;
/// Near clip plane.
pub const NEAR_CLIP_M: f32 = 0.5;

/// The commander camera's state. `focus` is the ground point the camera
/// looks at (world x, z in sim units); `ground_y` its smoothed height.
#[derive(Resource, Debug, Clone, Copy)]
pub struct CommanderCamera {
    pub focus: Vec2,
    pub ground_y: f32,
    /// Camera-to-focus distance in sim units, within the zoom limits; with
    /// the cut on it eases toward `target_distance` (the wheel's), with it
    /// off Space / Shift set it (and the target follows).
    pub distance: f32,
    pub target_distance: f32,
    /// The camera sank through the surface: `ground_y` is an absolute height
    /// the keys move, no longer the terrain under the focus.
    pub underground: bool,
    /// Heading around the vertical axis, radians; 0 = north-up, positive
    /// turns counter-clockwise seen from above (Bevy's yaw).
    pub yaw: f32,
    /// Angle below the horizon, radians, within the pitch limits.
    pub pitch: f32,
    /// Offset of the focus above `ground_y`, in sim units: nonzero only while
    /// a right-drag turns the rig about an off-centre pivot (released, it is
    /// re-anchored to 0).
    pub rise: f32,
    /// While a right-drag turns the view: the world point it turns about —
    /// the ground at the centre of the screen, re-picked every frame (so it
    /// follows panning). `None` otherwise.
    pub pivot: Option<Vec3>,
    /// Set once the camera has been put over the local body (cleared again
    /// when the home region changes, until it is put over the body there).
    pub placed: bool,
    /// The home region's rectangle on the ground (world x, z); `None` until
    /// the first status arrives. The camera is placed inside it (first
    /// placement, home changes, minimap jumps) but not held there.
    pub bounds: Option<Rect>,
}

impl CommanderCamera {
    /// Whether the camera has been put over the local body inside `rc`, i.e.
    /// `rc` is the home and its region is loaded far enough to show the body.
    pub fn placed_in(&self, rc: game::RegionCoords) -> bool {
        self.placed && self.bounds == Some(region_rect(rc))
    }
}

impl Default for CommanderCamera {
    fn default() -> Self {
        let d = ZOOM_START_M * UNITS_PER_METER;
        Self {
            focus: region_rect(game::RegionCoords { x: 0, z: 0 }).center(),
            ground_y: sea_level_units(),
            distance: d,
            target_distance: d,
            underground: false,
            yaw: 0.0,
            pitch: PITCH_DEG.to_radians(),
            rise: 0.0,
            pivot: None,
            placed: false,
            bounds: None,
        }
    }
}

/// Marker: the commander's render camera.
#[derive(Component)]
pub struct CommanderView;

/// A region's rectangle on the ground plane (world x, z).
pub fn region_rect(rc: game::RegionCoords) -> Rect {
    let o = rc.world_offset();
    Rect::new(o[0], o[2], o[0] + game::REGION_SIZE, o[2] + game::REGION_SIZE)
}

/// The sea surface's height in sim units.
pub fn sea_level_units() -> f32 {
    worldgen::pipeline::SEA_LEVEL as f32 * UNITS_PER_METER
}

/// The camera's pose for a focus point, distance, heading and pitch: behind
/// the focus along the heading (south of it at yaw 0), looking down at it.
pub fn camera_transform(focus: Vec3, distance: f32, yaw: f32, pitch: f32) -> Transform {
    let rotation = view_rotation(yaw, pitch);
    Transform {
        translation: focus + rotation * Vec3::Z * distance,
        rotation,
        scale: Vec3::ONE,
    }
}

/// The camera's orientation for a heading and a pitch below the horizon.
pub fn view_rotation(yaw: f32, pitch: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, yaw, -pitch, 0.0)
}

/// A screen-space ground direction (x to the right, y down the screen) as a
/// world (x, z) direction for a camera with heading `yaw`.
pub fn ground_dir(dir: Vec2, yaw: f32) -> Vec2 {
    let (sin, cos) = yaw.sin_cos();
    let right = Vec2::new(cos, -sin);
    let back = Vec2::new(sin, cos);
    right * dir.x + back * dir.y
}

/// A right-drag of `delta` pixels turns the view like first-person mouse
/// look: right turns right, down looks further down. Returns (yaw, pitch).
pub fn orbit(yaw: f32, pitch: f32, delta: Vec2) -> (f32, f32) {
    let rate = ORBIT_DEG_PER_PX.to_radians();
    let yaw = (yaw - delta.x * rate).rem_euclid(std::f32::consts::TAU);
    let pitch = (pitch + delta.y * rate).clamp(PITCH_MIN_DEG.to_radians(), PITCH_MAX_DEG.to_radians());
    (yaw, pitch)
}

/// A right-drag of `delta` pixels turning the whole camera rig (focus, eye
/// and view together) rigidly about `pivot`, by the same angles as [`orbit`]:
/// the pivot keeps its place on the screen, whatever the zoom. Returns the
/// new (focus, yaw, pitch); the distance is unchanged.
pub fn orbit_about(focus: Vec3, yaw: f32, pitch: f32, pivot: Vec3, delta: Vec2) -> (Vec3, f32, f32) {
    let (new_yaw, new_pitch) = orbit(yaw, pitch, delta);
    let turn = view_rotation(new_yaw, new_pitch) * view_rotation(yaw, pitch).inverse();
    (pivot + turn * (focus - pivot), new_yaw, new_pitch)
}

/// The focus moved along the view ray onto the plane `y = baseline`, with
/// the distance that leaves the camera where it is, so a turn about an
/// off-centre pivot hands back an ordinary rig with no rise. `None` when the
/// ray does not reach the plane.
pub fn reanchor(focus: Vec3, distance: f32, yaw: f32, pitch: f32, baseline: f32) -> Option<(Vec3, f32)> {
    let tf = camera_transform(focus, distance, yaw, pitch);
    let forward = *tf.forward();
    let t = (baseline - tf.translation.y) / forward.y;
    (t.is_finite() && t > 0.0).then(|| (tf.translation + forward * t, t))
}

/// The point a right-drag turns about: `ground`, the visible ground at the
/// centre of the screen ([`visible::visible_ground`] — with the cut on, the
/// level's floor or a pit's bottom, never cut-away rock); the focus when the
/// centre is off the terrain.
pub fn orbit_pivot(ground: Option<Vec3>, focus: Vec3) -> Vec3 {
    ground.unwrap_or(focus)
}

/// Keep the rise under [`RISE_MAX_M`]. There is no floor: the camera never
/// collides, so a turn near the ground passes through terrain and trees
/// instead of being pushed up over them.
pub fn clamp_rise(rise: f32) -> f32 {
    rise.min(RISE_MAX_M * UNITS_PER_METER)
}

/// Frame-rate-independent exponential approach of `current` to `target`
/// with time constant `tau` (seconds); `tau <= 0` snaps.
pub fn smooth(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    if tau <= 0.0 {
        return target;
    }
    current + (target - current) * (1.0 - (-dt / tau).exp())
}

/// Keep the focus on the home region's rectangle (unclamped while unknown).
pub fn clamp_focus(focus: Vec2, rect: Option<Rect>) -> Vec2 {
    match rect {
        Some(rect) => focus.clamp(rect.min, rect.max),
        None => focus,
    }
}

/// Pan direction from the cursor touching a screen edge: x to the right,
/// y down the screen (= +z, south). Zero inside the band-free middle.
pub fn edge_pan_dir(cursor: Vec2, window: Vec2, band: f32) -> Vec2 {
    let axis = |c: f32, len: f32| {
        if c < band {
            -1.0
        } else if c > len - band {
            1.0
        } else {
            0.0
        }
    };
    Vec2::new(axis(cursor.x, window.x), axis(cursor.y, window.y))
}

/// How far the ground at the centre of the screen is, which sets the
/// middle-drag scale so the ground under the cursor follows it: the centre
/// ray's hit on the visible ground (`hit`, sim units — the floor or a pit's
/// bottom with the cut on, never cut-away rock), else the camera `distance`.
/// Clamped to the distance limits.
pub fn pan_depth(hit: Option<f32>, distance: f32) -> f32 {
    hit.unwrap_or(distance)
        .clamp(ZOOM_NEAR_M * UNITS_PER_METER, ZOOM_FAR_M * UNITS_PER_METER)
}

/// Ground displacement (sim units) of one free-camera pan frame in screen
/// direction `dir`, at [`PAN_SPEED_M_PER_S`].
pub fn pan_step(dir: Vec2, dt: f32) -> Vec2 {
    dir.normalize_or_zero() * PAN_SPEED_M_PER_S * UNITS_PER_METER * dt
}

/// Cut on: one pan frame at view depth `depth` ([`pan_depth`]): zoomed out
/// pans faster over the ground, at the same speed across the screen.
pub fn zoom_pan_step(dir: Vec2, depth: f32, dt: f32) -> Vec2 {
    dir.normalize_or_zero() * depth * PAN_ZOOMS_PER_S * dt
}

/// Cut on: the wheel's new target distance after `notches` (positive =
/// toward the ground), clamped to the zoom limits.
pub fn zoom_target(target: f32, notches: f32) -> f32 {
    (target * ZOOM_STEP.powf(notches))
        .clamp(ZOOM_NEAR_M * UNITS_PER_METER, ZOOM_FAR_M * UNITS_PER_METER)
}

/// The focus (x, z) that keeps the eye straight above / below where it was
/// when the camera-to-focus distance goes from `from` to `to` at heading
/// `yaw` and `pitch`: the eye sits behind the focus along the view, so a
/// distance change alone would slide it along the view ray (a zoom).
pub fn focus_keeping_eye(focus: Vec2, yaw: f32, pitch: f32, from: f32, to: f32) -> Vec2 {
    let back = view_rotation(yaw, pitch) * Vec3::Z;
    focus - Vec2::new(back.x, back.z) * (to - from)
}

/// Move the camera vertically by `dy` sim units (+ up). Above ground that is
/// its height over the terrain (the distance, within the limits; vertical
/// travel `dy` over the pitch); past the lowest height the focus itself sinks
/// through the surface and the camera is underground, where `dy` moves the
/// absolute height, down to `floor`. Rising back past `surface` (the terrain
/// under the focus) re-attaches it above ground, the rest of `dy` going into
/// the distance again. With no `surface` loaded it stays where it is.
pub fn vertical_move(
    (distance, ground_y, underground): (f32, f32, bool),
    pitch: f32,
    dy: f32,
    surface: Option<f32>,
    floor: Option<f32>,
) -> (f32, f32, bool) {
    let (near, far) = (ZOOM_NEAR_M * UNITS_PER_METER, ZOOM_FAR_M * UNITS_PER_METER);
    let s = pitch.sin().max(0.1);
    let floor = floor.unwrap_or(f32::NEG_INFINITY);
    if !underground {
        let d = distance + dy / s;
        if d >= near {
            return (d.min(far), ground_y, false);
        }
        if surface.is_none() {
            return (near, ground_y, false);
        }
        let sink = (near - d) * s;
        return (near, (ground_y - sink).max(floor), true);
    }
    let g = (ground_y + dy).max(floor);
    match surface {
        Some(surf) if g >= surf => ((near + (g - surf) / s).min(far), surf, false),
        _ => (distance, g, true),
    }
}

/// [`vertical_move`] applied to the camera, which moves straight up / down:
/// the focus slides to keep the eye's x, z ([`focus_keeping_eye`]).
pub fn move_vertically(cam: &mut CommanderCamera, dy: f32, surface: Option<f32>, floor: Option<f32>) {
    let from = cam.distance;
    (cam.distance, cam.ground_y, cam.underground) =
        vertical_move((cam.distance, cam.ground_y, cam.underground), cam.pitch, dy, surface, floor);
    cam.focus = focus_keeping_eye(cam.focus, cam.yaw, cam.pitch, from, cam.distance);
    cam.target_distance = cam.distance;
}

/// Home: back above the surface at the opening distance, straight up.
pub fn surface_camera(cam: &mut CommanderCamera, surface: Option<f32>) {
    let Some(surf) = surface else { return };
    let from = cam.distance;
    cam.underground = false;
    cam.ground_y = surf;
    cam.distance = ZOOM_START_M * UNITS_PER_METER;
    cam.target_distance = cam.distance;
    cam.focus = focus_keeping_eye(cam.focus, cam.yaw, cam.pitch, from, cam.distance);
}

/// Screen-space focus displacement (x right, y down the screen; turn it into
/// the world with [`ground_dir`]) for a middle-drag of `delta` pixels, so the
/// ground under the cursor (near the focus) follows it.
pub fn drag_pan(delta: Vec2, distance: f32, fov_y: f32, viewport_h: f32, pitch: f32) -> Vec2 {
    if viewport_h <= 0.0 {
        return Vec2::ZERO;
    }
    let per_px = 2.0 * distance * (fov_y / 2.0).tan() / viewport_h;
    // Screen-vertical pixels cover more ground: the view is foreshortened.
    Vec2::new(-delta.x * per_px, -delta.y * per_px / pitch.sin().max(0.1))
}

/// The terrain height the camera rides: the mean of the known surface over a
/// square around the focus. Averaging (then easing over time) is what keeps
/// a cliff from popping the camera. `None` when nothing there is loaded.
pub fn focus_ground_target(grid: &HeightGrid, focus: Vec2) -> Option<f32> {
    let r = HEIGHT_SAMPLE_RADIUS_M * UNITS_PER_METER;
    let step = HEIGHT_SAMPLE_STEP_M * UNITS_PER_METER;
    let n = (r / step).round() as i32;
    let (mut sum, mut count) = (0.0, 0u32);
    for iz in -n..=n {
        for ix in -n..=n {
            let p = focus + Vec2::new(ix as f32, iz as f32) * step;
            if let Some(h) = grid.surface_at(p.x, p.y) {
                sum += h;
                count += 1;
            }
        }
    }
    (count > 0).then(|| sum / count as f32)
}

/// Commander camera, height grid, minimap and selection.
pub struct CommanderPlugin;

impl Plugin for CommanderPlugin {
    fn build(&self, app: &mut App) {
        // Everything here but the gizmo group is the session's: it goes back
        // to its fresh value in `end_session`.
        app.add_session_reset::<CommanderCamera>()
            .insert_resource(fresh_height_grid())
            .add_session_reset_with(|world| {
                world.insert_resource(fresh_height_grid());
            })
            .add_session_reset::<minimap::MinimapPointer>()
            .add_session_reset::<minimap::MinimapPixels>()
            .add_session_reset::<selection::Selection>()
            .add_session_reset::<selection::GroundPick>()
            .add_session_reset::<selection::SelectionBand>()
            .add_session_reset::<tiles::TileMirror>()
            .add_session_reset::<level::WorkingLevel>()
            .add_session_reset::<designate::DesignateState>()
            .add_session_reset::<designate::DragReadout>()
            .add_session_reset::<adventure::ClientPlayMode>()
            .add_session_reset::<follow::Follow>()
            .add_session_reset::<RightPress>()
            .add_session_reset::<move_order::PathPreview>()
            .add_session_reset::<move_order::SentPath>()
            .add_message::<adventure::ToggleModeRequest>()
            .add_session_message_clear::<adventure::ToggleModeRequest>()
            .init_gizmo_group::<designate::HiddenEdgeGizmos>()
            .add_message::<tiles::TileMirrorMsg>()
            .add_session_message_clear::<tiles::TileMirrorMsg>()
            // Ungated: hands the screen to the frontend and back.
            .add_systems(PreUpdate, yield_commander_camera)
            .add_systems(
                Startup,
                (
                    spawn_commander_camera,
                    designate::setup_designate_gizmos,
                ),
            )
            .add_systems(
                Update,
                (
                    height::update_height_grid,
                    tiles::update_tile_mirror,
                    track_home_region,
                    place_on_body,
                    minimap::minimap_pointer,
                    hold_pointer_while_orbiting,
                    grab_orbit_pivot,
                    (level::init_level, level::level_input).chain(),
                    pan_and_zoom,
                    (follow_terrain, follow::follow_camera, bounds::clamp_to_window, level::track_camera_level)
                        .chain(),
                    underground_view,
                    apply_camera_transform,
                    (
                        designate::designate_keys,
                        designate::update_hover,
                        designate::drag_input,
                        designate::update_drag_preview,
                        designate::draw_designate_gizmos,
                    )
                        .chain(),
                    (
                        selection::update_ground_pick,
                        selection::fallback_ground_pick,
                        move_order::update_path_preview,
                        move_order::send_move_order,
                        move_order::draw_paths,
                    )
                        .chain(),
                    selection::select_input.run_if(designate::no_tool_active),
                    follow::rekey_local_player,
                    selection::prune_selection,
                    selection::draw_selection_rings,
                    (minimap::rebuild_minimap_base, minimap::draw_minimap_overlay).chain(),
                )
                    .chain()
                    .after(super::interpolate::interpolate_transforms)
                    .run_if(world_systems_enabled()),
            )
            .add_systems(
                Update,
                (adventure::send_mode_toggle, adventure::on_mode_change, adventure::sync_body_visibility)
                    .chain()
                    .before(designate::designate_keys)
                    .before(cut_entities::hide_cut_entities)
                    .run_if(world_systems_enabled()),
            )
            .add_systems(
                Update,
                cut_entities::hide_cut_entities
                    .after(super::interpolate::interpolate_transforms)
                    .after(level::track_camera_level)
                    .after(tiles::update_tile_mirror)
                    .run_if(world_systems_enabled()),
            )
            .add_systems(
                Update,
                (super::fortress_view::update_overlay_grid, super::fortress_view::update_fortress_view)
                    .after(level::track_camera_level)
                    .after(tiles::update_tile_mirror)
                    .after(designate::update_drag_preview)
                    .run_if(world_systems_enabled()),
            );
    }
}

/// The height grid a session starts with.
fn fresh_height_grid() -> HeightGrid {
    HeightGrid::new(crate::blocks::load_registry().id_of("water").unwrap_or(game::BlockId::AIR))
}

/// The commander camera gives the screen to the frontend (menu, starting
/// screen, arrival map) and takes it back: inactive and without `UiCamera`
/// while [`frontend_has_screen`](crate::session::frontend_has_screen), the
/// active `UiCamera` otherwise.
pub fn yield_commander_camera(
    mut commands: Commands,
    state: Option<Res<State<crate::session::AppState>>>,
    embarked: Option<Res<crate::session::Embarked>>,
    mut cams: Query<(Entity, &mut Camera, Has<crate::renderer::UiCamera>), With<CommanderView>>,
) {
    let world_owns = !crate::session::frontend_has_screen(state.as_deref(), embarked.as_deref());
    for (e, mut cam, has_marker) in &mut cams {
        if cam.is_active != world_owns {
            cam.is_active = world_owns;
        }
        match (world_owns, has_marker) {
            (true, false) => {
                commands.entity(e).insert(crate::renderer::UiCamera);
            }
            (false, true) => {
                commands.entity(e).remove::<crate::renderer::UiCamera>();
            }
            _ => {}
        }
    }
}

fn spawn_commander_camera(mut commands: Commands, cam: Res<CommanderCamera>) {
    let focus = Vec3::new(cam.focus.x, cam.ground_y, cam.focus.y);
    commands.spawn((
        CommanderView,
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: FOV_Y_DEG.to_radians(),
            near: NEAR_CLIP_M * UNITS_PER_METER,
            far: game::lod::LOD_HORIZON_UNITS,
            ..default()
        }),
        camera_transform(focus, cam.distance, cam.yaw, cam.pitch),
        crate::renderer::UiCamera,
        Name::new("commander camera"),
    ));
}

/// The home region bounds the focus. When it changes (the body travelled),
/// the focus is pulled into the new region.
fn track_home_region(
    status: Res<super::hud::HudStatus>,
    mut pending_play: ResMut<crate::renderer::world_map::PendingPlay>,
    mut cam: ResMut<CommanderCamera>,
) {
    let bounds = status.home_region.map(region_rect);
    if cam.bounds != bounds {
        let change = cam.bounds.is_some() && bounds.is_some();
        // Only a Play re-places the camera over the new region's body; a
        // walking home change (a seam crossing) keeps the focus where it is
        // (Adventure's `bounds::clamp_to_window` confines it). The very first
        // home keeps the normal first placement.
        let walked = change && !pending_play.0;
        if change {
            if pending_play.0 {
                cam.placed = false;
            }
            pending_play.0 = false;
        }
        cam.bounds = bounds;
        if !walked {
            cam.focus = home_change_focus(cam.focus, bounds, cam.placed);
        }
    }
}

/// The focus right after the home rect changed to `bounds`: centred on the
/// new rect while the camera waits for `place_on_body` (a Play, or the first
/// home), so a travelled-to region is not entered at the edge nearest the old
/// place; kept (clamped) when the camera is already placed.
pub fn home_change_focus(focus: Vec2, bounds: Option<Rect>, placed: bool) -> Vec2 {
    match bounds {
        Some(rect) if !placed => rect.center(),
        _ => clamp_focus(focus, bounds),
    }
}

/// Whether the sim-unit point (x, z) lies inside `bounds` (none = anywhere).
fn inside_bounds(x: f32, z: f32, bounds: Option<Rect>) -> bool {
    bounds.is_none_or(|b| b.contains(Vec2::new(x, z)))
}

/// Put the camera over the local (inert) body the first time it is placed.
fn place_on_body(
    body: Query<&GlobalTransform, With<super::bridge::LocalBody>>,
    mut cam: ResMut<CommanderCamera>,
) {
    if cam.placed {
        return;
    }
    // A freshly spawned body reads the origin until transforms propagate.
    let Some(at) = body.iter().map(|gt| gt.translation()).find(|p| *p != Vec3::ZERO && inside_bounds(p.x, p.z, cam.bounds))
    else {
        return;
    };
    cam.focus = clamp_focus(Vec2::new(at.x, at.z), cam.bounds);
    cam.ground_y = at.y;
    cam.underground = false;
    cam.placed = true;
}

/// The right press that started an orbit drag, for telling a click (a move
/// order, `move_order`) from a turn. Written by
/// [`hold_pointer_while_orbiting`].
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct RightPress {
    /// Window position of the press while it is held.
    pub at: Option<Vec2>,
    /// Raw mouse motion since the press, pixels (summed lengths).
    pub motion_px: f32,
    /// On the frame the press ended only: its accumulated motion.
    pub released: Option<f32>,
}

/// A right press inside the window (not on the game UI or an overlay) starts
/// an orbit drag: the pointer is locked hidden ([`CursorPolicy::orbit`]) and
/// kept where the press was, and stays there when the button is released.
/// The turn itself reads raw mouse motion, which a warp does not produce.
/// The warp is what holds it on X11 and Windows, where `Locked` falls back to
/// `Confined` and the hidden pointer would otherwise wander. The press and
/// its motion are recorded in [`RightPress`].
fn hold_pointer_while_orbiting(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    mut policy: ResMut<CursorPolicy>,
    mut press: ResMut<RightPress>,
    mut anchor: crate::session::SessionLocal<Option<Vec2>>,
) {
    let anchor = anchor.get();
    if press.released.is_some() {
        press.released = None;
    }
    let Ok(mut window) = window.single_mut() else { return };
    if buttons.just_pressed(MouseButton::Right)
        && window.focused
        && !policy.blocks_game()
        && !policy.ui_pointer
    {
        *anchor = window.cursor_position();
        *press = RightPress { at: *anchor, motion_px: 0.0, released: None };
    }
    let held = buttons.pressed(MouseButton::Right) && window.focused && !policy.blocks_game();
    let Some(at) = *anchor else { return };
    if held && !buttons.just_pressed(MouseButton::Right) {
        press.motion_px += motion.delta.length();
    }
    // Only a real displacement is written: a `Window` change re-syncs the
    // whole window to winit.
    if window.cursor_position().is_none_or(|c| c.distance(at) > 0.5) {
        window.set_cursor_position(Some(at));
    }
    if !held {
        *anchor = None;
        // Only a real release of the button (window focused, game not
        // blocked by a menu) is a click; focus loss or Esc just ends the drag.
        let clicked = buttons.just_released(MouseButton::Right) && window.focused && !policy.blocks_game();
        *press = RightPress { at: None, motion_px: 0.0, released: clicked.then_some(press.motion_px) };
    }
    if policy.orbit != held {
        policy.orbit = held;
    }
}

/// The focus as a world point (x, height, z).
fn focus_point(cam: &CommanderCamera) -> Vec3 {
    Vec3::new(cam.focus.x, cam.ground_y + cam.rise, cam.focus.y)
}

/// While a right-drag turns the view, the point it turns about: the ground at
/// the centre of the screen, picked again every frame so it moves with WASD,
/// edge and minimap panning. While following, the followed entity itself
/// (the focus): turning about whatever the centre ray meets first — a tree
/// between the camera and the body — would swing the eye in close, and
/// nothing would ever move it back out.
fn grab_orbit_pivot(
    policy: Res<CursorPolicy>,
    follow: Res<follow::Follow>,
    grid: Res<HeightGrid>,
    working: Res<level::WorkingLevel>,
    tiles: Res<tiles::TileMirror>,
    xray: Option<Res<crate::renderer::voxel_pull::xray::XraySettings>>,
    mut cam: ResMut<CommanderCamera>,
) {
    if !policy.orbit {
        return;
    }
    let focus = focus_point(&cam);
    if follow.0.is_some() {
        cam.pivot = Some(focus);
        return;
    }
    let tf = camera_transform(focus, cam.distance, cam.yaw, cam.pitch);
    // The ground the player sees: with the cut on, never the cut-away rock.
    let ground =
        visible::visible_ground(tf.translation, *tf.forward(), game::lod::LOD_HORIZON_UNITS, &grid, &working, &tiles, xray_on(xray.as_deref(), working.cut));
    cam.pivot = Some(orbit_pivot(ground, focus));
}

/// Keyboard, screen-edge and middle/right-drag input → focus, heading,
/// pitch and zoom.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn pan_and_zoom(
    mut follow: ResMut<follow::Follow>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    policy: Res<CursorPolicy>,
    minimap: Res<minimap::MinimapPointer>,
    grid: Res<HeightGrid>,
    working: Res<level::WorkingLevel>,
    tiles: Res<tiles::TileMirror>,
    xray: Option<Res<crate::renderer::voxel_pull::xray::XraySettings>>,
    mut cam: ResMut<CommanderCamera>,
) {
    let Ok(window) = window.single() else { return };
    // A client-local overlay (dev panel) or the menu / world map owns the
    // pointer and the keyboard.
    if policy.blocks_game() {
        return;
    }
    let dt = time.delta_secs();
    let size = window.size();

    let mut dir = Vec2::ZERO;
    for (key, d) in [
        (KeyCode::KeyA, Vec2::NEG_X),
        (KeyCode::KeyD, Vec2::X),
        (KeyCode::KeyW, Vec2::NEG_Y),
        (KeyCode::KeyS, Vec2::Y),
        (KeyCode::ArrowLeft, Vec2::NEG_X),
        (KeyCode::ArrowRight, Vec2::X),
        (KeyCode::ArrowUp, Vec2::NEG_Y),
        (KeyCode::ArrowDown, Vec2::Y),
    ] {
        if keys.pressed(key) {
            dir += d;
        }
    }
    // A drag owns the mouse: the pointer crossing an edge band mid-turn
    // must not pan as well.
    let dragging = buttons.any_pressed([MouseButton::Middle, MouseButton::Right]);
    let cursor = window.cursor_position();
    if let Some(c) = cursor {
        let over_minimap = minimap::screen_rect(size).contains(c);
        if window.focused && !over_minimap && !minimap.dragging && !policy.ui_pointer && !dragging {
            dir += edge_pan_dir(c, size, EDGE_BAND_PX);
        }
    }
    let middle = buttons.pressed(MouseButton::Middle) && window.focused;
    let depth = (working.cut || middle).then(|| pan_depth(centre_ray_hit(&cam, &grid, &working, &tiles, xray_on(xray.as_deref(), working.cut)), cam.distance));
    let mut step = match depth {
        Some(depth) if working.cut => zoom_pan_step(dir, depth, dt),
        _ => pan_step(dir, dt),
    };
    if let (true, Some(depth)) = (middle, depth) {
        step += drag_pan(motion.delta, depth, FOV_Y_DEG.to_radians(), size.y, cam.pitch);
    }
    // Any pan ends following; zoom and orbit do not.
    if step != Vec2::ZERO && follow.0.is_some() {
        follow.0 = None;
    }
    let pan = ground_dir(step, cam.yaw);
    cam.focus += pan;
    // The turning point pans with the view, so it stays at the centre.
    if let Some(pivot) = cam.pivot.as_mut() {
        *pivot += Vec3::new(pan.x, 0.0, pan.y);
    }

    if let (true, Some(pivot)) = (policy.orbit, cam.pivot) {
        let (focus, yaw, pitch) = orbit_about(focus_point(&cam), cam.yaw, cam.pitch, pivot, motion.delta);
        cam.focus = Vec2::new(focus.x, focus.z);
        cam.rise = focus.y - cam.ground_y;
        (cam.yaw, cam.pitch) = (yaw, pitch);
    } else if !policy.orbit && cam.pivot.take().is_some() {
        if follow.0.is_some() {
            // Turned about the followed entity (the focus): nothing to
            // re-anchor, and the zoom distance stays as it was.
            cam.rise = 0.0;
        } else if let Some((focus, d)) = reanchor(focus_point(&cam), cam.distance, cam.yaw, cam.pitch, cam.ground_y) {
            cam.focus = Vec2::new(focus.x, focus.z);
            cam.rise = 0.0;
            (cam.distance, cam.target_distance) = (d, d);
        }
    }

    if working.cut {
        // "Hide above": the wheel zooms (Space / Shift step the level, `level`).
        let notches = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            // Touchpads: ~one notch per 40 px.
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        let over_minimap = cursor.is_some_and(|c| minimap::screen_rect(size).contains(c));
        if notches != 0.0 && !over_minimap && !policy.ui_pointer {
            cam.target_distance = zoom_target(cam.target_distance, notches);
        }
        cam.distance = smooth(cam.distance, cam.target_distance, dt, ZOOM_SMOOTHING_S);
        cam.rise = clamp_rise(cam.rise);
        return;
    }

    // Space / Shift held: straight up / down at a constant speed, through
    // the surface.
    let vertical = keys.any_pressed(RISE_KEYS) as i32 - keys.any_pressed(SINK_KEYS) as i32;
    if vertical != 0 {
        let dy = vertical as f32 * VERTICAL_SPEED_M_PER_S * UNITS_PER_METER * dt;
        let surface = focus_ground_target(&grid, cam.focus);
        let floor = grid.surface_range().map(level::sink_floor);
        move_vertically(&mut cam, dy, surface, floor);
    }
    cam.rise = clamp_rise(cam.rise);
    // The camera is free to leave the home region: nothing outside it depends
    // on the camera (designations, the cutaway and the minimap stay confined to
    // the home region on their own), and the terrain there renders as usual.
}

/// Distance (sim units) from the camera to the visible ground at the centre
/// of the screen (cut-away rock ignored), `None` when the centre ray meets
/// no loaded ground.
fn centre_ray_hit(
    cam: &CommanderCamera,
    grid: &HeightGrid,
    working: &level::WorkingLevel,
    tiles: &tiles::TileMirror,
    xray: bool,
) -> Option<f32> {
    let tf = camera_transform(focus_point(cam), cam.distance, cam.yaw, cam.pitch);
    let hit = visible::visible_ground(
        tf.translation,
        *tf.forward(),
        2.0 * ZOOM_FAR_M * UNITS_PER_METER,
        grid,
        working,
        tiles,
        xray,
    )?;
    Some(hit.distance(tf.translation))
}

/// The eye's height above the focus at `distance` and `pitch`.
pub fn eye_height(distance: f32, pitch: f32) -> f32 {
    distance * pitch.sin()
}

/// Surface mode's baseline target: `current` held while `ground` stays
/// within the dead band around it (a share of `eye_height`, see
/// [`FOLLOW_BAND_FRAC`]); ground beyond it drags the baseline only as far as
/// the band's edge, so the follow is continuous, never a jump.
pub fn hold_baseline(current: f32, ground: f32, eye_height: f32) -> f32 {
    let band = (eye_height * FOLLOW_BAND_FRAC).max(FOLLOW_BAND_MIN_M * UNITS_PER_METER);
    current.clamp(ground - band, ground + band)
}

/// The lowest baseline that keeps [`MIN_CLEARANCE_M`] of air between the
/// eye and `top`, the tallest column near the point under it.
pub fn clearance_floor(top: f32, eye_height: f32) -> f32 {
    top + MIN_CLEARANCE_M * UNITS_PER_METER - eye_height
}

/// The tallest loaded surface (trees included) within
/// [`CLEARANCE_RADIUS_M`] of world (x, z).
fn tallest_near(grid: &HeightGrid, at: Vec2) -> Option<f32> {
    let r = CLEARANCE_RADIUS_M * UNITS_PER_METER;
    let step = game::BLOCK_VOXELS as f32;
    let n = (r / step).ceil() as i32;
    (-n..=n)
        .flat_map(|iz| (-n..=n).map(move |ix| Vec2::new(ix as f32, iz as f32) * step))
        .filter(|o| o.length() <= r)
        .filter_map(|o| grid.surface_at(at.x + o.x, at.y + o.y))
        .reduce(f32::max)
}

/// Ease the focus height toward its baseline: with the cut on, the working
/// level's floor; with it off, the terrain, held within a dead band
/// ([`hold_baseline`]) and kept clear of what is under the eye — except
/// underground, where the height is the player's own.
fn follow_terrain(
    follow: Res<follow::Follow>,
    grid: Res<HeightGrid>,
    time: Res<Time>,
    working: Res<level::WorkingLevel>,
    tiles: Res<tiles::TileMirror>,
    mut cam: ResMut<CommanderCamera>,
) {
    // `follow::follow_camera` owns the height while following.
    if follow.0.is_some() {
        return;
    }
    if working.cut {
        if let Some(floor) = level::level_floor_y(&working, tiles.region()) {
            cam.ground_y = smooth(cam.ground_y, floor, time.delta_secs(), HEIGHT_SMOOTHING_S);
        }
        return;
    }
    if cam.underground {
        return;
    }
    let target = focus_ground_target(&grid, cam.focus).map(|ground| {
        let eye_h = eye_height(cam.distance, cam.pitch);
        let held = hold_baseline(cam.ground_y, ground, eye_h);
        let tf = camera_transform(focus_point(&cam), cam.distance, cam.yaw, cam.pitch);
        let under_eye = Vec2::new(tf.translation.x, tf.translation.z);
        tallest_near(&grid, under_eye).map_or(held, |top| held.max(clearance_floor(top, eye_h)))
    });
    if let Some(target) = target {
        cam.ground_y = smooth(cam.ground_y, target, time.delta_secs(), HEIGHT_SMOOTHING_S);
    }
}

/// Ambient light while the eye is under the terrain, as a multiple of the
/// surface's: every cave is in the sun's shadow, so it gets the shadowless
/// sun and more fill instead, to read as shapes.
pub const UNDERGROUND_AMBIENT_GAIN: f32 = 2.0;

/// Whether the eye is inside a solid block — the only place the underground
/// view applies. `home_block`: the eye's block is solid, when the home
/// region's tile mirror answers for it (implied rock included, dug air,
/// leaves and water not); elsewhere the eye below the terrain under it.
/// Underground mode alone does not count: a camera sunk into a dug room is
/// in open air and sees the room as it is.
pub fn eye_in_block(home_block: Option<bool>, eye: Vec3, terrain_under_eye: Option<f32>) -> bool {
    home_block.unwrap_or_else(|| terrain_under_eye.is_some_and(|t| eye.y < t))
}

/// Whether world point `eye` lies in a solid home-region block (`None`
/// outside the home region).
fn home_block_solid(mirror: &tiles::TileMirror, eye: Vec3) -> Option<bool> {
    let size = game::BLOCK_VOXELS as f32;
    let world = eye / size;
    let (bx, bz) = (world.x.floor() as i32, world.z.floor() as i32);
    if !mirror.in_home(bx, bz) {
        return None;
    }
    let n = game::REGION_BLOCKS as i32;
    let tile = [bx.rem_euclid(n), world.y.floor() as i32, bz.rem_euclid(n)];
    Some(game::block_props().is_solid(game::TileSource::block(mirror, tile)))
}

/// Inside a block: no sun shadows (everything down there is shadowed), a
/// brighter ambient, the rock backdrop instead of the sky and the fog closed
/// in on it (`skybox::UndergroundCubemap`, `atmosphere::underground_fog`);
/// all restored once the eye is in open air again.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn underground_view(
    cam: Res<CommanderCamera>,
    grid: Res<HeightGrid>,
    mirror: Res<tiles::TileMirror>,
    mut suns: Query<&mut DirectionalLight>,
    ambient: Option<ResMut<GlobalAmbientLight>>,
    sky: Option<Res<crate::renderer::skybox::SkyboxCubemap>>,
    rock: Option<Res<crate::renderer::skybox::UndergroundCubemap>>,
    mut views: Query<(Option<&mut bevy::core_pipeline::Skybox>, Option<&mut bevy::pbr::DistanceFog>), With<CommanderView>>,
    mut surface_ambient: Local<Option<f32>>,
) {
    let Some(mut ambient) = ambient else { return };
    let eye = camera_transform(focus_point(&cam), cam.distance, cam.yaw, cam.pitch).translation;
    let under = eye_in_block(home_block_solid(&mirror, eye), eye, grid.surface_at(eye.x, eye.z));
    for (skybox, fog) in &mut views {
        if let (Some(mut skybox), Some(sky), Some(rock)) = (skybox, sky.as_deref(), rock.as_deref()) {
            let want = if under { &rock.0 } else { &sky.0 };
            if skybox.image.as_ref() != Some(want) {
                skybox.image = Some(want.clone());
            }
        }
        if let Some(mut fog) = fog {
            let want = if under {
                crate::renderer::atmosphere::underground_fog()
            } else {
                crate::renderer::atmosphere::distance_fog()
            };
            if fog.color != want.color {
                *fog = want;
            }
        }
    }
    let base = *surface_ambient.get_or_insert(ambient.brightness);
    let want = if under { base * UNDERGROUND_AMBIENT_GAIN } else { base };
    if ambient.brightness != want {
        ambient.brightness = want;
    }
    for mut sun in &mut suns {
        if sun.shadow_maps_enabled == under {
            sun.shadow_maps_enabled = !under;
        }
    }
}

fn apply_camera_transform(
    cam: Res<CommanderCamera>,
    mut view: Query<&mut Transform, With<CommanderView>>,
) {
    let focus = focus_point(&cam);
    for mut tf in &mut view {
        tf.set_if_neq(camera_transform(focus, cam.distance, cam.yaw, cam.pitch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_body_inside_the_home_bounds_places_the_camera() {
        let home = region_rect(game::RegionCoords::new(0, 0));
        let other = region_rect(game::RegionCoords::new(1, 0));
        let mid = home.center();
        assert!(inside_bounds(mid.x, mid.y, Some(home)));
        assert!(!inside_bounds(mid.x, mid.y, Some(other)));
        assert!(inside_bounds(mid.x, mid.y, None));
    }

    #[test]
    fn camera_looks_at_the_focus_from_the_south_at_the_pitch() {
        let focus = Vec3::new(100.0, 50.0, 200.0);
        let pitch = PITCH_DEG.to_radians();
        let tf = camera_transform(focus, 640.0, 0.0, pitch);
        assert!((tf.translation.distance(focus) - 640.0).abs() < 1e-3);
        assert!(tf.translation.z > focus.z, "camera sits south (+z) of the focus");
        let forward = tf.forward();
        let to_focus = (focus - tf.translation).normalize();
        assert!(forward.dot(to_focus) > 0.99999, "{forward:?} vs {to_focus:?}");
        let below = (-forward.y).asin();
        assert!((below - pitch).abs() < 1e-5, "pitch below horizon");
        assert!(tf.right().x > 0.999, "north-up: screen right is +x");
    }

    #[test]
    fn pitch_stays_fixed_across_zoom() {
        let focus = Vec3::ZERO;
        let a = camera_transform(focus, ZOOM_NEAR_M * UNITS_PER_METER, 0.3, PITCH_DEG.to_radians());
        let b = camera_transform(focus, ZOOM_FAR_M * UNITS_PER_METER, 0.3, PITCH_DEG.to_radians());
        assert_eq!(a.rotation, b.rotation);
    }

    #[test]
    fn a_turned_camera_still_looks_at_the_focus_and_pans_along_its_view() {
        let focus = Vec3::new(10.0, 5.0, -20.0);
        let yaw = 1.1;
        let tf = camera_transform(focus, 640.0, yaw, 0.7);
        let to_focus = (focus - tf.translation).normalize();
        assert!(tf.forward().dot(to_focus) > 0.99999);
        // Screen right / up map onto the camera's own right / ground-forward.
        let right = ground_dir(Vec2::X, yaw);
        assert!((right - Vec2::new(tf.right().x, tf.right().z)).length() < 1e-5);
        let up = ground_dir(Vec2::NEG_Y, yaw);
        let ahead = Vec2::new(tf.forward().x, tf.forward().z).normalize();
        assert!((up - ahead).length() < 1e-5, "{up:?} vs {ahead:?}");
        assert_eq!(ground_dir(Vec2::new(1.0, 1.0), 0.0), Vec2::new(1.0, 1.0), "yaw 0 is north-up");
    }

    #[test]
    fn right_drag_turns_like_mouse_look_within_the_pitch_limits() {
        let (yaw, pitch) = orbit(0.0, 0.8, Vec2::new(10.0, 10.0));
        assert!(yaw > std::f32::consts::PI, "dragging right turns clockwise (negative yaw, wrapped)");
        assert!(pitch > 0.8, "dragging down looks further down");
        assert_eq!(orbit(0.0, 0.8, Vec2::new(0.0, 1e5)).1, PITCH_MAX_DEG.to_radians());
        assert_eq!(orbit(0.0, 0.8, Vec2::new(0.0, -1e5)).1, PITCH_MIN_DEG.to_radians());
    }

    #[test]
    fn turning_about_a_pivot_keeps_it_in_place_on_the_screen() {
        let (focus, d) = (Vec3::new(300.0, 1600.0, -200.0), 640.0);
        let (yaw, pitch) = (0.4, 0.9);
        let pivot = Vec3::new(420.0, 1550.0, -350.0);
        let in_view = |f: Vec3, y: f32, p: f32| {
            let tf = camera_transform(f, d, y, p);
            tf.rotation.inverse() * (pivot - tf.translation)
        };
        let before = in_view(focus, yaw, pitch);
        for delta in [Vec2::new(40.0, 0.0), Vec2::new(-15.0, 30.0), Vec2::new(200.0, -60.0)] {
            let (f, y, p) = orbit_about(focus, yaw, pitch, pivot, delta);
            assert_eq!((y, p), orbit(yaw, pitch, delta), "same angles as a turn about the focus");
            assert!((in_view(f, y, p) - before).length() < 1e-2, "{delta:?}");
        }
        assert_eq!(orbit_about(focus, yaw, pitch, focus, Vec2::new(40.0, 0.0)).0, focus, "about the focus: focus stays");
    }

    #[test]
    fn reanchoring_moves_the_focus_onto_the_baseline_without_moving_the_camera() {
        let (focus, d, yaw, pitch) = (Vec3::new(0.0, 1700.0, 0.0), 640.0, 1.2, 0.8);
        let (f, nd) = reanchor(focus, d, yaw, pitch, 1600.0).unwrap();
        assert!((f.y - 1600.0).abs() < 1e-3);
        let (a, b) = (camera_transform(focus, d, yaw, pitch), camera_transform(f, nd, yaw, pitch));
        assert!(a.translation.distance(b.translation) < 1e-2);
        assert_eq!(a.rotation, b.rotation);
        assert!(reanchor(focus, d, yaw, pitch, 1e6).is_none(), "plane above the eye");
    }

    #[test]
    fn pivot_is_the_visible_ground_on_the_view_ray() {
        // What counts as visible (the cut) is `visible`'s, tested there.
        let focus = Vec3::new(0.0, 0.0, -500.0);
        let hit = Vec3::new(100.0, 200.0, -400.0);
        assert_eq!(orbit_pivot(None, focus), focus, "off the terrain: the focus");
        assert_eq!(orbit_pivot(Some(hit), focus), hit);
    }

    #[test]
    fn rise_has_a_ceiling_but_no_floor() {
        assert_eq!(clamp_rise(-1e6), -1e6, "the camera passes through the ground: no collision");
        assert_eq!(clamp_rise(1e6), RISE_MAX_M * UNITS_PER_METER);
        assert_eq!(clamp_rise(0.0), 0.0);
    }

    #[test]
    fn smoothing_converges_without_overshoot_and_is_frame_rate_independent() {
        let mut fast = 0.0;
        for _ in 0..60 {
            fast = smooth(fast, 10.0, 1.0 / 60.0, 0.15);
            assert!(fast <= 10.0);
        }
        let slow = smooth(smooth(0.0, 10.0, 0.5, 0.15), 10.0, 0.5, 0.15);
        assert!((fast - slow).abs() < 1e-3, "60 small steps ≈ 2 big ones: {fast} vs {slow}");
        assert_eq!(smooth(3.0, 7.0, 0.016, 0.0), 7.0, "tau 0 snaps");
    }

    #[test]
    fn a_home_change_centres_the_focus_until_the_camera_is_placed() {
        let old = region_rect(game::RegionCoords { x: 0, z: 0 });
        let new = region_rect(game::RegionCoords { x: 3, z: -2 });
        let near_old = old.max - Vec2::splat(1.0);
        assert_eq!(home_change_focus(near_old, Some(new), false), new.center());
        assert_eq!(home_change_focus(near_old, Some(new), true), clamp_focus(near_old, Some(new)));
        assert_eq!(home_change_focus(near_old, None, false), near_old);
    }

    #[test]
    fn placed_in_needs_the_camera_placed_over_that_home() {
        let a = game::RegionCoords { x: 0, z: 0 };
        let b = game::RegionCoords { x: 1, z: 0 };
        let mut cam = CommanderCamera { bounds: Some(region_rect(b)), placed: false, ..Default::default() };
        assert!(!cam.placed_in(b), "home known, body not shown yet");
        cam.placed = true;
        assert!(cam.placed_in(b));
        assert!(!cam.placed_in(a));
    }

    #[test]
    fn focus_is_clamped_to_the_home_region() {
        let rect = region_rect(game::RegionCoords { x: -1, z: 2 });
        assert_eq!(rect.size(), Vec2::splat(game::REGION_SIZE));
        assert_eq!(clamp_focus(rect.min - Vec2::splat(50.0), Some(rect)), rect.min);
        assert_eq!(clamp_focus(rect.max + Vec2::splat(50.0), Some(rect)), rect.max);
        assert_eq!(clamp_focus(rect.center(), Some(rect)), rect.center());
        let far = Vec2::splat(1e6);
        assert_eq!(clamp_focus(far, None), far, "unclamped until the home region is known");
    }

    #[test]
    fn edge_band_pans_toward_the_edge() {
        let w = Vec2::new(1920.0, 1080.0);
        assert_eq!(edge_pan_dir(Vec2::new(960.0, 540.0), w, 8.0), Vec2::ZERO);
        assert_eq!(edge_pan_dir(Vec2::new(2.0, 540.0), w, 8.0), Vec2::NEG_X);
        assert_eq!(edge_pan_dir(Vec2::new(1919.0, 1079.0), w, 8.0), Vec2::new(1.0, 1.0));
        assert_eq!(edge_pan_dir(Vec2::new(960.0, 0.0), w, 8.0), Vec2::NEG_Y, "top edge = north");
    }

    #[test]
    fn cut_on_pans_scale_with_the_view_depth_and_the_wheel_zooms() {
        let d = 40.0 * UNITS_PER_METER;
        let step = zoom_pan_step(Vec2::new(1.0, 1.0), d, 0.5);
        assert!((step.length() - d * PAN_ZOOMS_PER_S * 0.5).abs() < 1e-3);
        let (near, far) = (ZOOM_NEAR_M * UNITS_PER_METER, ZOOM_FAR_M * UNITS_PER_METER);
        let start = ZOOM_START_M * UNITS_PER_METER;
        assert!(zoom_target(start, 1.0) < start, "wheel up zooms in");
        assert_eq!(zoom_target(start, 100.0), near);
        assert_eq!(zoom_target(start, -100.0), far);
    }

    #[test]
    fn pan_speed_is_constant_and_direction_normalised() {
        let step = pan_step(Vec2::new(1.0, 1.0), 0.5);
        assert!((step.length() - PAN_SPEED_M_PER_S * UNITS_PER_METER * 0.5).abs() < 1e-3);
        assert_eq!(pan_step(Vec2::ZERO, 0.5), Vec2::ZERO);
    }

    #[test]
    fn height_keys_move_the_eye_straight_up_not_along_the_view() {
        let (yaw, pitch) = (0.7, 55f32.to_radians());
        let (focus, ground) = (Vec2::new(100.0, 200.0), 50.0);
        let (from, to) = (300.0, 420.0);
        let eye = |f: Vec2, d: f32| camera_transform(Vec3::new(f.x, ground, f.y), d, yaw, pitch).translation;
        let before = eye(focus, from);
        let after = eye(focus_keeping_eye(focus, yaw, pitch, from, to), to);
        assert!((after.x - before.x).abs() < 1e-3 && (after.z - before.z).abs() < 1e-3, "{before} -> {after}");
        assert!(after.y > before.y, "it rose");
    }

    #[test]
    fn surface_mode_holds_its_height_until_the_ground_comes_close() {
        let u = UNITS_PER_METER;
        let eye = 30.0 * u; // band 15 m
        assert_eq!(hold_baseline(100.0 * u, 110.0 * u, eye), 100.0 * u, "a 10 m tree line passes under");
        assert_eq!(hold_baseline(100.0 * u, 90.0 * u, eye), 100.0 * u, "a 10 m dip too");
        assert_eq!(hold_baseline(100.0 * u, 130.0 * u, eye), 115.0 * u, "a hill drags it to the band's edge");
        assert_eq!(hold_baseline(100.0 * u, 60.0 * u, eye), 75.0 * u, "a valley drags it down the same way");
        let close = 2.0 * u; // zoomed right in: band at its 1 m minimum
        assert_eq!(hold_baseline(100.0 * u, 103.0 * u, close), 102.0 * u);
    }

    #[test]
    fn the_eye_keeps_clear_of_the_tallest_column_under_it() {
        let u = UNITS_PER_METER;
        // Eye 4 m over the baseline, a 20 m crown under it: the baseline must
        // be at least 20 + 3 - 4 = 19 m.
        assert_eq!(clearance_floor(20.0 * u, 4.0 * u), 19.0 * u);
        // High up, the crown is far below and asks nothing of a 0 m baseline.
        assert!(clearance_floor(20.0 * u, 60.0 * u) < 0.0);
    }

    #[test]
    fn pan_depth_follows_the_visible_ground_within_the_zoom_limits() {
        let u = UNITS_PER_METER;
        assert_eq!(pan_depth(Some(20.0 * u), 60.0 * u), 20.0 * u, "a near hill slows the pan");
        assert_eq!(pan_depth(None, 60.0 * u), 60.0 * u, "no hit: the zoom distance");
        assert_eq!(pan_depth(Some(0.1 * u), 60.0 * u), ZOOM_NEAR_M * u);
        assert_eq!(pan_depth(Some(500.0 * u), 60.0 * u), ZOOM_FAR_M * u);
    }

    #[test]
    fn height_keys_move_the_camera_within_the_limits_above_ground() {
        let u = UNITS_PER_METER;
        let (near, far) = (ZOOM_NEAR_M * u, ZOOM_FAR_M * u);
        let pitch = std::f32::consts::FRAC_PI_2; // straight down: dy is the distance
        let surface = Some(100.0 * u);
        let (d, g, under) = vertical_move((20.0 * u, 100.0 * u, false), pitch, 10.0 * u, surface, None);
        assert_eq!((d, g, under), (30.0 * u, 100.0 * u, false), "Space rises");
        let (d, ..) = vertical_move((20.0 * u, 100.0 * u, false), pitch, 1e6, surface, None);
        assert_eq!(d, far, "capped at the top");
        let (d, g, under) = vertical_move((20.0 * u, 100.0 * u, false), pitch, -10.0 * u, surface, None);
        assert_eq!((d, g, under), (10.0 * u, 100.0 * u, false), "Shift lowers");
        assert!(near < 10.0 * u);
    }

    #[test]
    fn sinking_past_the_lowest_height_goes_through_the_surface_and_back() {
        let u = UNITS_PER_METER;
        let near = ZOOM_NEAR_M * u;
        let pitch = std::f32::consts::FRAC_PI_2;
        let surface = Some(100.0 * u);
        // 3 m above the lowest height, sink 10 m: 7 m under the surface.
        let (d, g, under) = vertical_move((near + 3.0 * u, 100.0 * u, false), pitch, -10.0 * u, surface, None);
        assert!(under, "sank through");
        assert_eq!(d, near);
        assert!((g - 93.0 * u).abs() < 1e-3, "{g}");
        // Underground the height is absolute, down to the floor.
        let (_, g2, _) = vertical_move((d, g, true), pitch, -1000.0 * u, surface, Some(50.0 * u));
        assert_eq!(g2, 50.0 * u, "no lower than the floor");
        // Rising 10 m from 93 m: 3 m past the surface, back above ground.
        let (d, g, under) = vertical_move((d, g, true), pitch, 10.0 * u, surface, None);
        assert!(!under);
        assert_eq!(g, 100.0 * u);
        assert!((d - (near + 3.0 * u)).abs() < 1e-3, "{d}");
        // No surface known: never sinks.
        let (d, _, under) = vertical_move((near, 100.0 * u, false), pitch, -10.0 * u, None, None);
        assert_eq!((d, under), (near, false));
    }

    #[test]
    fn the_underground_view_is_the_eye_inside_a_block() {
        let eye = Vec3::new(0.0, 50.0, 0.0);
        assert!(eye_in_block(Some(true), eye, Some(40.0)), "in rock, even above the height grid");
        assert!(!eye_in_block(Some(false), eye, Some(60.0)), "a dug room under the surface is open air");
        assert!(eye_in_block(None, eye, Some(60.0)), "outside home: dipped into a hill");
        assert!(!eye_in_block(None, eye, Some(40.0)));
        assert!(!eye_in_block(None, eye, None));
    }

    #[test]
    fn the_eye_tile_is_read_from_the_home_mirror() {
        let mut mirror = tiles::TileMirror::default();
        let home = game::RegionCoords::new(0, 0);
        assert_eq!(home_block_solid(&mirror, Vec3::splat(8.0)), None, "no home");
        mirror.set_home(Some(home));
        mirror.set_chunk([0, 0, 0], tiles::chunk_blocks(&game::Chunk::uniform(game::Block::new(game::BlockId(2)))));
        let s = game::BLOCK_VOXELS as f32;
        assert_eq!(home_block_solid(&mirror, Vec3::new(1.5 * s, 3.5 * s, 2.5 * s)), Some(true));
        let above = game::CHUNK_BLOCKS as f32 + 0.5;
        assert_eq!(home_block_solid(&mirror, Vec3::new(1.5 * s, above * s, 2.5 * s)), Some(false), "air above");
        assert_eq!(home_block_solid(&mirror, Vec3::new(-0.5 * s, 3.5 * s, 2.5 * s)), None, "outside home");
    }

    #[test]
    fn middle_drag_moves_the_focus_against_the_mouse() {
        let step = drag_pan(Vec2::new(10.0, 10.0), 640.0, FOV_Y_DEG.to_radians(), 1080.0, 1.0);
        assert!(step.x < 0.0 && step.y < 0.0);
        assert!(step.y.abs() > step.x.abs(), "vertical drag is foreshortened");
        assert_eq!(drag_pan(Vec2::ONE, 640.0, 0.7, 0.0, 1.0), Vec2::ZERO);
    }

    #[test]
    fn ground_target_averages_known_columns_only() {
        let mut grid = HeightGrid::new(game::BlockId(5));
        let at = Vec2::new(100.0, -300.0);
        assert_eq!(focus_ground_target(&grid, at), None);
        grid.set_column_for_test(at, 1600.0);
        assert_eq!(focus_ground_target(&grid, at), Some(1600.0));
    }
}
