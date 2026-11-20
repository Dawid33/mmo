//! The commander view (RTS Phase 2): the player has no body to look through.
//! A client-local camera looks down at the island, pans with the screen
//! edges, WASD / the arrow keys and a middle-drag, turns (yaw and pitch) with
//! a right-drag about the ground at the centre of the screen (marked while
//! the drag lasts). With the cut on ("Hide above") it rides the working
//! level's floor: Space / Shift step the level (see `level`), the wheel
//! zooms and pans scale with the view depth ([`pan_depth`]). With the cut off
//! it is a free camera with no zoom: Space / Shift raise and lower it at a
//! constant speed, PgUp / PgDn by one level, with no ceiling
//! ([`move_vertically`]). It follows nothing — not the terrain, trees or the
//! cut: its height is the player's own, and it passes through the ground
//! into the rock — the terrain mesh only has faces where rock meets air, so
//! from inside the rock every dug space shows from the outside in. Home puts
//! it back over the surface. The free camera moves at constant speeds ([`PAN_SPEED_M_PER_S`],
//! [`VERTICAL_SPEED_M_PER_S`]); its wheel does nothing. None of it is sim state —
//! nothing in the sim depends on where a commander looks — so it lives here,
//! with the minimap (`minimap`), the client-local selection and the ground
//! pick (`selection`), all of which read the client height grid (`height`),
//! and the fortress designation tools (`designate`), which read the tile
//! mirror (`tiles`) at the working level (`level`); units above the
//! working level's cut are hidden (`cut_entities`).
//!
//! V switches to the DF-style top-down view (`top_down`): straight down,
//! north up, orthographic, the cut always on, the wheel stepping levels.
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
pub mod first_person;
pub mod follow;
pub mod height;
pub mod level;
pub mod minimap;
pub mod move_order;
pub mod selection;
pub mod tiles;
pub mod top_down;
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
    /// The render benchmark flies the body's camera, so it keeps first person.
    /// The autopilot probes (`just fly-probe` / `shuttle-probe`) need the
    /// commander: they ask for the overview (`ClientPlayMode`,
    /// `ToggleModeRequest`, both registered by [`CommanderPlugin`]).
    pub fn from_env() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        if super::render_bench::enabled() {
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
/// Lowest pitch a right-drag reaches: negative is above the horizon, so the
/// camera can look up (just short of straight up, where yaw degenerates).
pub const PITCH_MIN_DEG: f32 = -85.0;
/// Below this pitch a right-drag turns the view about the eye (looks around)
/// instead of swinging the rig about the ground at the screen centre, which
/// would drive the eye under the ground as the view tips up.
pub const ORBIT_GROUND_MIN_DEG: f32 = 15.0;
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
/// Sprint (Minecraft-style): keyboard / screen-edge pan speed multiplier
/// while Ctrl is held or after W is double-tapped ([`Sprint`]).
pub const SPRINT_FACTOR: f32 = 2.0;
/// Held: sprint.
pub const SPRINT_KEYS: [KeyCode; 2] = [KeyCode::ControlLeft, KeyCode::ControlRight];
/// Double-tapped (within the sim's [`game::movement::DOUBLE_TAP_WINDOW_MS`],
/// as for flight): sprint until it is released.
pub const SPRINT_TAP_KEY: KeyCode = KeyCode::KeyW;
/// Space / Shift: straight up / down, metres per second.
pub const VERTICAL_SPEED_M_PER_S: f32 = 15.0;
/// Keys that raise / lower the camera while held.
pub const RISE_KEYS: [KeyCode; 1] = [KeyCode::Space];
pub const SINK_KEYS: [KeyCode; 2] = [KeyCode::ShiftLeft, KeyCode::ShiftRight];
/// Width of the screen-edge band that pans.
pub const EDGE_BAND_PX: f32 = 8.0;
/// Cut on: time constant the focus height follows the working level's floor with.
pub const HEIGHT_SMOOTHING_S: f32 = 0.15;
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
    /// off it stays at [`ZOOM_START_M`] (the free camera has no zoom).
    pub distance: f32,
    pub target_distance: f32,
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
    /// Whether the camera has been put over the local body inside `rc` (the
    /// home's copy in the `WrapFrame`), i.e. `rc` is the home and its region
    /// is loaded far enough to show the body.
    pub fn placed_in(&self, rc: game::RegionCoords) -> bool {
        self.placed && self.bounds == Some(region_rect(rc))
    }
}

impl Default for CommanderCamera {
    fn default() -> Self {
        let d = start_angle_deg("VOXEL_CAMERA_DISTANCE_M").unwrap_or(ZOOM_START_M) * UNITS_PER_METER;
        Self {
            focus: region_rect(game::RegionCoords { x: 0, z: 0 }).center(),
            ground_y: sea_level_units(),
            distance: d,
            target_distance: d,
            yaw: start_angle_deg("VOXEL_CAMERA_YAW_DEG").unwrap_or(0.0).to_radians(),
            pitch: start_angle_deg("VOXEL_CAMERA_PITCH_DEG")
                .unwrap_or(PITCH_DEG)
                .clamp(PITCH_MIN_DEG, PITCH_MAX_DEG)
                .to_radians(),
            rise: 0.0,
            pivot: None,
            placed: false,
            bounds: None,
        }
    }
}

/// `VOXEL_CAMERA_PITCH_DEG` / `VOXEL_CAMERA_YAW_DEG` / `VOXEL_CAMERA_DISTANCE_M`
/// (native): the commander camera's starting pitch below the horizon and
/// heading (degrees) and distance from its focus (metres; the wheel later
/// clamps to `ZOOM_NEAR_M..=ZOOM_FAR_M`) — e.g. a horizon-facing benchmark view
/// above the canopy (`…_PITCH_DEG=15 …_DISTANCE_M=150`).
fn start_angle_deg(name: &str) -> Option<f32> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var(name).ok()?.trim().parse().ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        None
    }
}

/// Marker: the commander's render camera.
#[derive(Component)]
pub struct CommanderView;

/// A region's rectangle on the ground plane (world x, z); `rc` is the copy in
/// the `WrapFrame` (render space).
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
/// and view together) rigidly, by the same angles as [`orbit`]: about
/// `pivot` while the pitch stays at or above [`ORBIT_GROUND_MIN_DEG`] (the
/// pivot keeps its place on the screen, whatever the zoom), about the eye
/// below it (looking around, up to the sky). A drag crossing that pitch
/// splits at it. Returns the new (focus, yaw, pitch); the distance is
/// unchanged.
pub fn orbit_about(focus: Vec3, distance: f32, yaw: f32, pitch: f32, pivot: Vec3, delta: Vec2) -> (Vec3, f32, f32) {
    let (new_yaw, new_pitch) = orbit(yaw, pitch, delta);
    let band = ORBIT_GROUND_MIN_DEG.to_radians();
    let turn = |focus: Vec3, from: (f32, f32), to: (f32, f32), about: Option<Vec3>| {
        if from == to {
            return focus;
        }
        let about = about.unwrap_or_else(|| camera_transform(focus, distance, from.0, from.1).translation);
        let r = view_rotation(to.0, to.1) * view_rotation(from.0, from.1).inverse();
        about + r * (focus - about)
    };
    let focus = if pitch >= band {
        let mid = (new_yaw, new_pitch.max(band));
        let focus = turn(focus, (yaw, pitch), mid, Some(pivot));
        turn(focus, mid, (new_yaw, new_pitch), None)
    } else {
        let mid = (new_yaw, new_pitch.min(band));
        let focus = turn(focus, (yaw, pitch), mid, None);
        turn(focus, mid, (new_yaw, new_pitch), Some(pivot))
    };
    (focus, new_yaw, new_pitch)
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

/// Cut off: the free camera turns about `ground` (the visible ground at the
/// centre of the screen) only while it is no farther from the `eye` than
/// the focus is, else about the focus — so the turning point never moves
/// away from the camera as it rises: high up it is the focus, a fixed
/// [`ZOOM_START_M`] ahead.
pub fn free_orbit_pivot(ground: Option<Vec3>, eye: Vec3, focus: Vec3) -> Vec3 {
    let reach = eye.distance(focus);
    ground.filter(|g| g.distance(eye) <= reach).unwrap_or(focus)
}

/// Keep the rise under [`RISE_MAX_M`]. There is no floor: the camera never
/// collides, so a turn near the ground passes through terrain and trees
/// instead of being pushed up over them.
pub fn clamp_rise(rise: f32) -> f32 {
    rise.min(RISE_MAX_M * UNITS_PER_METER)
}

/// Minecraft-style sprint latch: a double tap of [`SPRINT_TAP_KEY`] sprints
/// until that key is released. (Ctrl sprints only while held, outside it.)
#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub struct Sprint {
    /// When the key was last tapped (seconds), awaiting a second tap.
    last_tap: Option<f32>,
    pub latched: bool,
}

impl Sprint {
    /// The latch after this frame: `tapped` = the key went down at `now`
    /// (seconds), `held` = it is down.
    pub fn update(self, tapped: bool, held: bool, now: f32) -> Self {
        let mut next = self;
        if tapped {
            let window = game::movement::DOUBLE_TAP_WINDOW_MS as f32 / 1000.0;
            if next.last_tap.is_some_and(|t| now - t <= window) {
                (next.latched, next.last_tap) = (true, None);
            } else {
                next.last_tap = Some(now);
            }
        }
        if !held {
            next.latched = false;
        }
        next
    }
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

/// Cut off: move the free camera straight up / down by `dy` sim units
/// (+ up). Its height is absolute — it follows neither the terrain nor
/// trees — with no ceiling, and no lower than `floor` (just below bedrock).
pub fn move_vertically(cam: &mut CommanderCamera, dy: f32, floor: Option<f32>) {
    cam.ground_y = (cam.ground_y + dy).max(floor.unwrap_or(f32::NEG_INFINITY));
}

/// Home: back above the surface at the opening distance, straight up.
pub fn surface_camera(cam: &mut CommanderCamera, surface: Option<f32>) {
    let Some(surf) = surface else { return };
    let from = cam.distance;
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
            .add_session_reset::<first_person::FirstPerson>()
            .add_session_reset::<top_down::TopDown>()
            .add_session_reset::<RightPress>()
            .add_session_reset::<move_order::PathPreview>()
            .add_session_reset::<move_order::SentPath>()
            .add_message::<top_down::ToggleTopDownRequest>()
            .add_session_message_clear::<top_down::ToggleTopDownRequest>()
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
                    hold_pointer_while_orbiting.run_if(first_person::inactive),
                    grab_orbit_pivot.run_if(first_person::inactive).run_if(top_down::inactive),
                    // After `level_input`: a C that turns the cut off leaves top-down
                    // in the same frame, before `track_camera_level` sees it.
                    (level::init_level, level::level_input.run_if(first_person::inactive), top_down::toggle_top_down).chain(),
                    (
                        pan_and_zoom.run_if(first_person::inactive).run_if(top_down::inactive),
                        top_down::top_down_input.run_if(first_person::inactive).run_if(top_down::active),
                    )
                        .chain(),
                    (follow_terrain, follow::follow_camera, bounds::clamp_to_window, level::track_camera_level)
                        .chain(),
                    (underground_view, first_person_water_fog),
                    (top_down::sync_top_down_projection, apply_camera_transform).chain(),
                    (
                        designate::designate_keys,
                        designate::update_hover,
                        designate::drag_input,
                        designate::update_drag_preview,
                        designate::draw_designate_gizmos,
                    )
                        .chain()
                        .run_if(first_person::inactive),
                    (
                        selection::update_ground_pick,
                        selection::fallback_ground_pick,
                        move_order::update_path_preview,
                        move_order::send_move_order,
                        move_order::draw_paths,
                    )
                        .chain()
                        .run_if(first_person::inactive),
                    selection::select_input.run_if(designate::no_tool_active).run_if(first_person::inactive),
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
                (
                    adventure::ask_for_adventure,
                    adventure::send_mode_toggle,
                    adventure::on_mode_change,
                    first_person::ensure_first_person,
                    first_person::cycle_body_view,
                    // Before the eyes are moved toward their new target.
                    first_person::sync_first_person_camera.before(super::interpolate::interpolate_transforms),
                    // The glide works on the eye: last frame's third-person
                    // pull comes off first.
                    first_person::restore_eye_point.before(super::interpolate::interpolate_transforms),
                    adventure::sync_body_visibility,
                )
                    .chain()
                    .before(designate::designate_keys)
                    .before(cut_entities::hide_cut_entities)
                    .run_if(world_systems_enabled()),
            )
            .add_systems(
                Update,
                // After the look rotation is written, which it keeps.
                first_person::place_third_person_camera
                    .after(super::look::apply_local_look)
                    .after(super::interpolate::interpolate_transforms)
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
                (
                    super::fortress_view::update_overlay_grid,
                    (super::fortress_view::update_fortress_view, super::voxel_material::sync_chunk_material_cut).chain(),
                )
                    .after(level::track_camera_level)
                    .after(tiles::update_tile_mirror)
                    .after(designate::update_drag_preview)
                    .run_if(world_systems_enabled()),
            );
    }
}

/// The height grid a session starts with.
fn fresh_height_grid() -> HeightGrid {
    HeightGrid::new()
}

/// The commander camera gives the screen to the frontend (menu, starting
/// screen, arrival map) and takes it back: inactive and without `UiCamera`
/// while [`frontend_has_screen`](crate::session::frontend_has_screen), the
/// active `UiCamera` otherwise.
pub fn yield_commander_camera(
    mut commands: Commands,
    state: Option<Res<State<crate::session::AppState>>>,
    in_play: Option<Res<crate::session::InPlay>>,
    first_person: Option<Res<first_person::FirstPerson>>,
    mut cams: Query<(Entity, &mut Camera, Has<crate::renderer::UiCamera>), With<CommanderView>>,
) {
    // In first person the adventurer's eyes have the screen.
    let world_owns = !crate::session::frontend_has_screen(state.as_deref(), in_play.as_deref())
        && first_person.is_none_or(|fp| !fp.on);
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

/// The 3D commander camera's projection (the top-down view swaps in its
/// orthographic one, `top_down::sync_top_down_projection`).
pub fn commander_perspective() -> PerspectiveProjection {
    PerspectiveProjection {
        fov: FOV_Y_DEG.to_radians(),
        near: NEAR_CLIP_M * UNITS_PER_METER,
        far: game::lod::LOD_HORIZON_UNITS,
        ..default()
    }
}

fn spawn_commander_camera(mut commands: Commands, cam: Res<CommanderCamera>) {
    let focus = Vec3::new(cam.focus.x, cam.ground_y, cam.focus.y);
    commands.spawn((
        CommanderView,
        Camera3d::default(),
        Projection::Perspective(commander_perspective()),
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
    mut level: ResMut<level::WorkingLevel>,
    mut cam: ResMut<CommanderCamera>,
    frame: Option<Res<crate::wrap_frame::WrapFrame>>,
) {
    let frame = frame.as_deref().copied().unwrap_or_default();
    let bounds = status.home_region.map(|home| region_rect(frame.alias(home)));
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
                // The level is region-local: the old region's would put the
                // camera anywhere in the new one's ground. `init_level` /
                // `place_on_body` pick it again from the new surface.
                level.level = None;
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

/// Put the camera over the local (inert) body the first time it is placed
/// (and after every Play): above ground, on the surface of the body's column,
/// with the working level there too — not the level picked under the region
/// centre while the camera waited, nor the previous region's. Waits until the
/// height grid has that column, so the surface is the real one.
fn place_on_body(
    body: Query<&GlobalTransform, With<super::bridge::LocalBody>>,
    grid: Res<HeightGrid>,
    mut level: ResMut<level::WorkingLevel>,
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
    let focus = clamp_focus(Vec2::new(at.x, at.z), cam.bounds);
    let Some(surface) = level::focus_surface(&grid, focus) else { return };
    cam.focus = focus;
    cam.ground_y = surface;
    cam.rise = 0.0;
    cam.pivot = None;
    level.level = Some(level::surface_level(surface));
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
/// `Confined` and the hidden pointer would otherwise wander (native only —
/// winit cannot warp on the web, where pointer lock is real). The press and
/// its motion are recorded in [`RightPress`].
fn hold_pointer_while_orbiting(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    mut policy: ResMut<CursorPolicy>,
    mut press: ResMut<RightPress>,
    mut anchor: crate::session::SessionLocal<Option<Vec2>>,
    td: Option<Res<top_down::TopDown>>,
) {
    // Top-down never turns: the press is only recorded (a move order's
    // click vs drag), the pointer stays free.
    let turns = td.is_none_or(|t| !t.on);
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
    if turns {
        warp_pointer(&mut window, at);
    }
    if !held {
        *anchor = None;
        // Only a real release of the button (window focused, game not
        // blocked by a menu) is a click; focus loss or Esc just ends the drag.
        let clicked = buttons.just_released(MouseButton::Right) && window.focused && !policy.blocks_game();
        *press = RightPress { at: None, motion_px: 0.0, released: clicked.then_some(press.motion_px) };
    }
    let orbit = held && turns;
    if policy.orbit != orbit {
        policy.orbit = orbit;
    }
}

/// Puts the pointer back at `at`. Only a real displacement is written: a
/// `Window` change re-syncs the whole window to winit (hence `Mut`, not
/// `&mut Window` — dereferencing it mutably marks it changed). Not on the
/// web: winit cannot warp the pointer there (it logs "could not set cursor
/// position" every frame), and the browser's pointer lock already holds it.
#[cfg(not(target_arch = "wasm32"))]
fn warp_pointer(window: &mut Mut<Window>, at: Vec2) {
    if window.cursor_position().is_none_or(|c| c.distance(at) > 0.5) {
        window.set_cursor_position(Some(at));
    }
}

#[cfg(target_arch = "wasm32")]
fn warp_pointer(_window: &mut Mut<Window>, _at: Vec2) {}

/// The focus as a world point (x, height, z).
fn focus_point(cam: &CommanderCamera) -> Vec3 {
    Vec3::new(cam.focus.x, cam.ground_y + cam.rise, cam.focus.y)
}

/// While a right-drag turns the view, the point it turns about: the ground at
/// the centre of the screen, picked again every frame so it moves with WASD,
/// edge and minimap panning. While following, the followed entity itself
/// (the focus): turning about whatever the centre ray meets first — a tree
/// between the camera and the body — would swing the eye in close, and
/// nothing would ever move it back out. With the cut off, never farther
/// than the focus ([`free_orbit_pivot`]).
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
        visible::visible_ground(tf.translation, *tf.forward(), selection::PICK_REACH_M * UNITS_PER_METER, &grid, &working, &tiles, xray_on(xray.as_deref(), working.cut));
    cam.pivot = Some(if working.cut { orbit_pivot(ground, focus) } else { free_orbit_pivot(ground, tf.translation, focus) });
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
    mut sprint: crate::session::SessionLocal<Sprint>,
) {
    let Ok(window) = window.single() else { return };
    let sprint = sprint.get();
    // A client-local overlay (dev panel) or the menu / world map owns the
    // pointer and the keyboard.
    if policy.blocks_game() {
        *sprint = Sprint::default();
        return;
    }
    *sprint = sprint.update(keys.just_pressed(SPRINT_TAP_KEY), keys.pressed(SPRINT_TAP_KEY), time.elapsed_secs());
    let speed = if sprint.latched || keys.any_pressed(SPRINT_KEYS) { SPRINT_FACTOR } else { 1.0 };
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
        Some(depth) if working.cut => zoom_pan_step(dir, depth, dt) * speed,
        _ => pan_step(dir, dt) * speed,
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
        let (focus, yaw, pitch) = orbit_about(focus_point(&cam), cam.distance, cam.yaw, cam.pitch, pivot, motion.delta);
        cam.focus = Vec2::new(focus.x, focus.z);
        cam.rise = focus.y - cam.ground_y;
        (cam.yaw, cam.pitch) = (yaw, pitch);
    } else if !policy.orbit && cam.pivot.take().is_some() {
        if follow.0.is_some() {
            // Turned about the followed entity (the focus): nothing to
            // re-anchor, and the zoom distance stays as it was.
            cam.rise = 0.0;
        } else if !working.cut {
            // The free camera's height is absolute: the focus keeps its
            // distance and simply takes the height it was turned to, so
            // turns never stretch the distance to the turning point.
            cam.ground_y += cam.rise;
            cam.rise = 0.0;
        } else if cam.pitch < ORBIT_GROUND_MIN_DEG.to_radians() {
            // Looking along or above the horizon: the view ray meets the
            // floor far away or never, and re-anchoring onto it would throw
            // the zoom distance out — keep the rise instead.
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
    // the ground and trees alike.
    let vertical = keys.any_pressed(RISE_KEYS) as i32 - keys.any_pressed(SINK_KEYS) as i32;
    if vertical != 0 {
        let dy = vertical as f32 * VERTICAL_SPEED_M_PER_S * UNITS_PER_METER * dt;
        move_vertically(&mut cam, dy, grid.surface_range().map(level::sink_floor));
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

/// Cut on: ease the focus height toward the working level's floor. Cut off
/// the camera is free: its height is the player's own, whatever the terrain,
/// trees or cut below it ([`move_vertically`]).
fn follow_terrain(
    follow: Res<follow::Follow>,
    time: Res<Time>,
    working: Res<level::WorkingLevel>,
    tiles: Res<tiles::TileMirror>,
    mut cam: ResMut<CommanderCamera>,
) {
    // `follow::follow_camera` owns the height while following.
    if follow.0.is_some() || !working.cut {
        return;
    }
    if let Some(floor) = level::level_floor_y(&working, tiles.region()) {
        cam.ground_y = smooth(cam.ground_y, floor, time.delta_secs(), HEIGHT_SMOOTHING_S);
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
    // `is_solid` answers for blocks only (air is not one, and reads solid).
    let id = game::TileSource::block(mirror, tile);
    Some(id != game::BlockId::AIR && game::block_props().is_solid(id))
}

/// Whether world point `eye` lies in a water cell of the home region
/// (`TileSource::water_units` > 0: recipe water, `game::static_water_units`).
pub fn eye_in_water(mirror: &tiles::TileMirror, eye: Vec3) -> bool {
    let world = eye / game::BLOCK_VOXELS as f32;
    let (bx, bz) = (world.x.floor() as i32, world.z.floor() as i32);
    if !mirror.in_home(bx, bz) {
        return false;
    }
    let n = game::REGION_BLOCKS as i32;
    game::TileSource::water_units(mirror, [bx.rem_euclid(n), world.y.floor() as i32, bz.rem_euclid(n)]) > 0
}

/// Keeps the first-person (body) camera's fog in step with the water: the
/// underwater fog while its eye is in water, the distance fog otherwise.
/// (`underground_view` only drives the commander camera; this camera has the
/// sky and sun as they are and swaps nothing else.)
pub fn first_person_water_fog(
    mirror: Res<tiles::TileMirror>,
    view: Option<Res<crate::renderer::atmosphere::ViewDistance>>,
    mut cams: Query<(&GlobalTransform, &mut bevy::pbr::DistanceFog), (With<Camera3d>, With<first_person::Eyes>)>,
) {
    for (gt, mut fog) in &mut cams {
        let want = if eye_in_water(&mirror, gt.translation()) {
            crate::renderer::atmosphere::underwater_fog()
        } else {
            crate::renderer::atmosphere::distance_fog(&view.as_deref().copied().unwrap_or_default())
        };
        if fog.color != want.color {
            *fog = want;
        }
    }
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
    td: Option<Res<top_down::TopDown>>,
    working: Res<level::WorkingLevel>,
    mut fog_choice: crate::session::SessionLocal<Option<(bool, bool, bool)>>,
    view: Option<Res<crate::renderer::atmosphere::ViewDistance>>,
) {
    let Some(mut ambient) = ambient else { return };
    // Top-down: its eye is always in the air; underground is the working
    // level below the surface at the focus, and there is no fog.
    let top = td.is_some_and(|t| t.on);
    // Underground beats underwater beats open air.
    let (under, wet) = if top {
        (top_down::top_down_underground(working.level, top_down::lowest_surface_level(grid.surface_range())), false)
    } else {
        let eye = camera_transform(focus_point(&cam), cam.distance, cam.yaw, cam.pitch).translation;
        let under = eye_in_block(home_block_solid(&mirror, eye), eye, grid.surface_at(eye.x, eye.z));
        (under, !under && eye_in_water(&mirror, eye))
    };
    let fog_choice = fog_choice.get();
    let new_fog = *fog_choice != Some((top, under, wet));
    for (skybox, fog) in &mut views {
        if let (Some(mut skybox), Some(sky), Some(rock)) = (skybox, sky.as_deref(), rock.as_deref()) {
            let want = if under { &rock.0 } else { &sky.0 };
            if skybox.image.as_ref() != Some(want) {
                skybox.image = Some(want.clone());
            }
        }
        if let Some(mut fog) = fog {
            let want = if top {
                crate::renderer::atmosphere::top_down_fog()
            } else if under {
                crate::renderer::atmosphere::underground_fog()
            } else if wet {
                crate::renderer::atmosphere::underwater_fog()
            } else {
                crate::renderer::atmosphere::distance_fog(&view.as_deref().copied().unwrap_or_default())
            };
            // The top-down fog shares the surface fog's colour, so the
            // colour alone cannot tell them apart.
            if new_fog || fog.color != want.color {
                *fog = want;
            }
            // Recorded only once a fog was there to take it.
            *fog_choice = Some((top, under, wet));
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
    td: Option<Res<top_down::TopDown>>,
    working: Res<level::WorkingLevel>,
    tiles: Res<tiles::TileMirror>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut view: Query<&mut Transform, With<CommanderView>>,
) {
    let want = match td.as_deref() {
        Some(td) if td.on => {
            // Straight down over the focus, the block grid on whole pixels.
            let scale = window.single().map_or(1.0, |w| w.scale_factor());
            let focus = top_down::snap_to_pixel(cam.focus, top_down::units_per_px(td.px_per_block(), scale));
            let cut = level::camera_cut(&working, &tiles).unwrap_or(cam.ground_y);
            top_down::top_down_transform(focus, cut)
        }
        _ => camera_transform(focus_point(&cam), cam.distance, cam.yaw, cam.pitch),
    };
    for mut tf in &mut view {
        tf.set_if_neq(want);
    }
}

