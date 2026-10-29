//! The commander view (RTS Phase 2): the player has no body to look through.
//! A client-local camera looks down at the island, pans with the screen
//! edges, WASD / the arrow keys and a middle-drag, turns (yaw and pitch) with
//! a right-drag, rises and sinks with Space / Shift, zooms with the wheel, and
//! rides a smoothed terrain height. None of it is sim state —
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
//! (inert) body is in, the home region. The camera is kept over that region's
//! rectangle and the minimap shows it. Travelling between regions moves the
//! body, which moves the home region and the near window with it.
//!
//! The sim player body still exists (it goes away in a later phase) but is
//! inert: the commander never sends it keys, mouse motion or clicks, so it
//! never turns on fps-cam mode, never walks and never edits blocks. The bridge
//! hides it and gives it no camera ([`CameraMode::Commander`]).

pub mod cut_entities;
pub mod designate;
pub mod height;
pub mod level;
pub mod minimap;
pub mod selection;
pub mod tiles;

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use game::movement::UNITS_PER_METER;

use super::cursor::CursorPolicy;
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

/// Camera pitch below the horizon the game opens at. Zooming only changes the
/// distance; a right-drag changes the pitch within the limits below.
pub const PITCH_DEG: f32 = 55.0;
/// Shallowest pitch a right-drag reaches.
pub const PITCH_MIN_DEG: f32 = 15.0;
/// Steepest pitch (just short of straight down, where yaw degenerates).
pub const PITCH_MAX_DEG: f32 = 89.0;
/// Right-drag turn rate, per logical pixel of mouse motion.
pub const ORBIT_DEG_PER_PX: f32 = 0.25;
/// Space / Shift vertical speed, in zoom distances per second (like panning).
pub const RISE_ZOOMS_PER_S: f32 = 1.0;
/// Highest the camera rises above the terrain-following height.
pub const RISE_MAX_M: f32 = 200.0;
/// Lowest the eye sinks to, above the terrain-following height.
pub const MIN_CLEARANCE_M: f32 = 2.0;
/// Vertical field of view.
pub const FOV_Y_DEG: f32 = 40.0;
/// Closest zoom: camera-to-focus distance.
pub const ZOOM_NEAR_M: f32 = 20.0;
/// Farthest zoom.
pub const ZOOM_FAR_M: f32 = 80.0;
/// Zoom the game opens at.
pub const ZOOM_START_M: f32 = 45.0;
/// Distance factor per wheel notch toward the ground (its inverse away).
pub const ZOOM_STEP: f32 = 0.85;
/// Time constant the distance eases toward its wheel target with.
pub const ZOOM_SMOOTHING_S: f32 = 0.08;
/// Edge/keyboard pan speed, in zoom distances per second (zoomed out pans
/// faster over the ground, at the same speed across the screen).
pub const PAN_ZOOMS_PER_S: f32 = 1.0;
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
    /// Camera-to-focus distance in sim units, easing toward `target_distance`.
    pub distance: f32,
    pub target_distance: f32,
    /// Heading around the vertical axis, radians; 0 = north-up, positive
    /// turns counter-clockwise seen from above (Bevy's yaw).
    pub yaw: f32,
    /// Angle below the horizon, radians, within the pitch limits.
    pub pitch: f32,
    /// Space / Shift offset of the focus above `ground_y`, in sim units.
    pub rise: f32,
    /// Set once the camera has been put over the local body.
    pub placed: bool,
    /// The home region's rectangle on the ground (world x, z); `None` until
    /// the first status arrives. The focus is clamped to it.
    pub bounds: Option<Rect>,
}

impl Default for CommanderCamera {
    fn default() -> Self {
        let d = ZOOM_START_M * UNITS_PER_METER;
        Self {
            focus: region_rect(game::RegionCoords { x: 0, z: 0 }).center(),
            ground_y: sea_level_units(),
            distance: d,
            target_distance: d,
            yaw: 0.0,
            pitch: PITCH_DEG.to_radians(),
            rise: 0.0,
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
    let rotation = Quat::from_euler(EulerRot::YXZ, yaw, -pitch, 0.0);
    Transform {
        translation: focus + rotation * Vec3::Z * distance,
        rotation,
        scale: Vec3::ONE,
    }
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

/// Keep the rise within its limits: at most [`RISE_MAX_M`], and low enough
/// only that the eye stays [`MIN_CLEARANCE_M`] above the followed terrain.
pub fn clamp_rise(rise: f32, distance: f32, pitch: f32) -> f32 {
    let lowest = MIN_CLEARANCE_M * UNITS_PER_METER - distance * pitch.sin();
    rise.clamp(lowest.min(0.0), RISE_MAX_M * UNITS_PER_METER)
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

/// Keep the camera itself (its ground position, behind the focus along the
/// heading) on the home region's rectangle, so it can fly right up to an edge
/// whichever way it faces. Clamping the focus instead stopped a camera facing
/// an edge `distance·cos(pitch)` short of it. Returns the adjusted focus.
pub fn clamp_eye(focus: Vec2, yaw: f32, pitch: f32, distance: f32, rect: Option<Rect>) -> Vec2 {
    let behind = ground_dir(Vec2::Y, yaw) * distance * pitch.cos();
    clamp_focus(focus + behind, rect) - behind
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

/// Ground displacement (sim units) of one pan frame in screen direction
/// `dir`, at the current zoom distance (sim units).
pub fn pan_step(dir: Vec2, distance: f32, dt: f32) -> Vec2 {
    dir.normalize_or_zero() * distance * PAN_ZOOMS_PER_S * dt
}

/// The wheel's new target distance after `notches` (positive = toward the
/// ground), clamped to the zoom limits.
pub fn zoom_target(target: f32, notches: f32) -> f32 {
    (target * ZOOM_STEP.powf(notches))
        .clamp(ZOOM_NEAR_M * UNITS_PER_METER, ZOOM_FAR_M * UNITS_PER_METER)
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
        app.init_resource::<CommanderCamera>()
            .insert_resource(HeightGrid::new(
                crate::blocks::load_registry().id_of("water").unwrap_or(game::BlockId::AIR),
            ))
            .init_resource::<minimap::MinimapPointer>()
            .init_resource::<selection::Selection>()
            .init_resource::<selection::GroundPick>()
            .init_resource::<tiles::TileMirror>()
            .init_resource::<level::WorkingLevel>()
            .init_resource::<designate::DesignateState>()
            .init_resource::<designate::DragReadout>()
            .init_gizmo_group::<designate::HiddenEdgeGizmos>()
            .add_message::<tiles::TileMirrorMsg>()
            .add_systems(
                Startup,
                (
                    spawn_commander_camera,
                    minimap::setup_minimap,
                    selection::setup_selection_box,
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
                    level::init_level,
                    level::step_level_input,
                    pan_and_zoom,
                    follow_terrain,
                    apply_camera_transform,
                    (
                        designate::designate_keys,
                        designate::update_hover,
                        designate::drag_input,
                        designate::update_drag_preview,
                        designate::draw_designate_gizmos,
                    )
                        .chain(),
                    selection::update_ground_pick,
                    selection::select_input.run_if(designate::no_tool_active),
                    selection::prune_selection,
                    selection::draw_selection_rings,
                    minimap::rebuild_minimap_base,
                    minimap::draw_minimap_overlay,
                )
                    .chain()
                    .after(super::interpolate::interpolate_transforms),
            )
            .add_systems(
                Update,
                cut_entities::hide_cut_entities
                    .after(super::interpolate::interpolate_transforms)
                    .after(level::step_level_input)
                    .after(tiles::update_tile_mirror),
            )
            .add_systems(
                Update,
                (super::fortress_view::update_overlay_grid, super::fortress_view::update_fortress_view)
                    .after(level::step_level_input)
                    .after(tiles::update_tile_mirror)
                    .after(designate::update_drag_preview),
            );
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
        bevy::ui::IsDefaultUiCamera,
        Name::new("commander camera"),
    ));
}

/// The home region bounds the focus. When it changes (the body travelled),
/// the focus is pulled into the new region.
fn track_home_region(status: Res<super::hud::HudStatus>, mut cam: ResMut<CommanderCamera>) {
    let bounds = status.home_region.map(region_rect);
    if cam.bounds != bounds {
        cam.bounds = bounds;
        cam.focus = clamp_focus(cam.focus, bounds);
    }
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
    let Some(at) = body.iter().map(|gt| gt.translation()).find(|p| *p != Vec3::ZERO) else {
        return;
    };
    cam.focus = clamp_focus(Vec2::new(at.x, at.z), cam.bounds);
    cam.ground_y = at.y;
    cam.placed = true;
}

/// A right press inside the window (not on the game UI or an overlay) starts
/// an orbit drag: the pointer is locked hidden ([`CursorPolicy::orbit`]) and
/// kept where the press was, and stays there when the button is released.
/// The turn itself reads raw mouse motion, which a warp does not produce.
/// The warp is what holds it on X11 and Windows, where `Locked` falls back to
/// `Confined` and the hidden pointer would otherwise wander.
fn hold_pointer_while_orbiting(
    buttons: Res<ButtonInput<MouseButton>>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    mut policy: ResMut<CursorPolicy>,
    mut anchor: Local<Option<Vec2>>,
) {
    let Ok(mut window) = window.single_mut() else { return };
    if buttons.just_pressed(MouseButton::Right)
        && window.focused
        && !policy.overlay_open
        && !policy.ui_pointer
    {
        *anchor = window.cursor_position();
    }
    let held = buttons.pressed(MouseButton::Right) && window.focused && !policy.overlay_open;
    let Some(at) = *anchor else { return };
    // Only a real displacement is written: a `Window` change re-syncs the
    // whole window to winit.
    if window.cursor_position().is_none_or(|c| c.distance(at) > 0.5) {
        window.set_cursor_position(Some(at));
    }
    if !held {
        *anchor = None;
    }
    if policy.orbit != held {
        policy.orbit = held;
    }
}

/// Space / Shift rise and sink direction (+1 / -1 / 0). `level_keys` = Alt
/// or PgUp/PgDn held: Shift there is the fast working-level step
/// (`level::step_level_input`), not a sink, so both keys are ignored.
fn rise_input(space: bool, shift: bool, level_keys: bool) -> f32 {
    if level_keys {
        return 0.0;
    }
    f32::from(u8::from(space)) - f32::from(u8::from(shift))
}

/// Keyboard, screen-edge, middle/right-drag and wheel input → focus, heading,
/// pitch, rise and zoom.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn pan_and_zoom(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    policy: Res<CursorPolicy>,
    minimap: Res<minimap::MinimapPointer>,
    mut cam: ResMut<CommanderCamera>,
) {
    let Ok(window) = window.single() else { return };
    // A client-local overlay (dev panel) owns the pointer and the keyboard.
    if policy.overlay_open {
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
    let mut step = pan_step(dir, cam.distance, dt);

    if buttons.pressed(MouseButton::Middle) && window.focused {
        step += drag_pan(motion.delta, cam.distance, FOV_Y_DEG.to_radians(), size.y, cam.pitch);
    }
    let pan = ground_dir(step, cam.yaw);
    cam.focus += pan;

    if policy.orbit {
        (cam.yaw, cam.pitch) = orbit(cam.yaw, cam.pitch, motion.delta);
    }

    let alt = keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]);
    let paging = keys.any_pressed([KeyCode::PageUp, KeyCode::PageDown]);
    let vertical = rise_input(keys.pressed(KeyCode::Space), keys.pressed(KeyCode::ShiftLeft), alt || paging);
    cam.rise += vertical * cam.distance * RISE_ZOOMS_PER_S * dt;

    let notches = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        // Touchpads: ~one notch per 40 px.
        MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
    };
    let over_minimap = cursor.is_some_and(|c| minimap::screen_rect(size).contains(c));
    if notches != 0.0
        && !over_minimap
        && !policy.ui_pointer
        && !level::wheel_routes_to_level(alt, window.focused)
    {
        cam.target_distance = zoom_target(cam.target_distance, notches);
    }
    cam.distance = smooth(cam.distance, cam.target_distance, dt, ZOOM_SMOOTHING_S);
    cam.rise = clamp_rise(cam.rise, cam.distance, cam.pitch);
    // Last, after every pan, turn, zoom and minimap jump this frame.
    cam.focus = clamp_eye(cam.focus, cam.yaw, cam.pitch, cam.distance, cam.bounds);
}

/// Ease the focus height toward its baseline: the working level's floor when
/// there is one (Space/Shift `rise` offsets from it), else the terrain.
fn follow_terrain(
    grid: Res<HeightGrid>,
    time: Res<Time>,
    working: Res<level::WorkingLevel>,
    tiles: Res<tiles::TileMirror>,
    mut cam: ResMut<CommanderCamera>,
) {
    let terrain = focus_ground_target(&grid, cam.focus);
    if let Some(target) = level::camera_baseline(terrain, level::level_floor_y(&working, tiles.region())) {
        cam.ground_y = smooth(cam.ground_y, target, time.delta_secs(), HEIGHT_SMOOTHING_S);
    }
}

fn apply_camera_transform(
    cam: Res<CommanderCamera>,
    mut view: Query<&mut Transform, With<CommanderView>>,
) {
    let focus = Vec3::new(cam.focus.x, cam.ground_y + cam.rise, cam.focus.y);
    for mut tf in &mut view {
        tf.set_if_neq(camera_transform(focus, cam.distance, cam.yaw, cam.pitch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn level_keys_suppress_rise_and_sink() {
        assert_eq!(rise_input(true, false, false), 1.0);
        assert_eq!(rise_input(false, true, false), -1.0);
        assert_eq!(rise_input(true, true, false), 0.0);
        assert_eq!(rise_input(false, true, true), 0.0, "Alt+Shift / Shift+PgDn is a fast level step");
        assert_eq!(rise_input(true, false, true), 0.0);
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
    fn rise_keeps_the_eye_above_the_terrain_and_under_the_ceiling() {
        let d = ZOOM_NEAR_M * UNITS_PER_METER;
        let pitch = PITCH_MIN_DEG.to_radians();
        let low = clamp_rise(-1e6, d, pitch);
        let eye_above = low + d * pitch.sin();
        assert!((eye_above - MIN_CLEARANCE_M * UNITS_PER_METER).abs() < 1e-3);
        assert_eq!(clamp_rise(1e6, d, pitch), RISE_MAX_M * UNITS_PER_METER);
        assert_eq!(clamp_rise(0.0, d, pitch), 0.0, "the default height is always legal");
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
    fn the_camera_reaches_the_region_edge_it_faces() {
        let rect = region_rect(game::RegionCoords { x: 0, z: 0 });
        let (d, pitch) = (640.0, PITCH_DEG.to_radians());
        let eye = |focus: Vec2, yaw: f32| {
            let tf = camera_transform(Vec3::new(focus.x, 0.0, focus.y), d, yaw, pitch);
            Vec2::new(tf.translation.x, tf.translation.z)
        };
        // Facing north (yaw 0) and pushed far past the north edge: the camera
        // stops ON the edge, the focus beyond it.
        let focus = clamp_eye(Vec2::new(rect.center().x, rect.min.y - 1e5), 0.0, pitch, d, Some(rect));
        assert!((eye(focus, 0.0).y - rect.min.y).abs() < 1e-2);
        assert!(focus.y < rect.min.y);
        // Any heading: the eye stays inside.
        for yaw in [0.4, 2.0, 3.5, 5.9] {
            let f = clamp_eye(Vec2::splat(-1e5), yaw, pitch, d, Some(rect));
            let e = eye(f, yaw);
            assert!(rect.contains(e) || (e - e.clamp(rect.min, rect.max)).length() < 1e-2, "{yaw}: {e:?}");
        }
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
    fn pan_speed_scales_with_zoom_and_is_direction_normalised() {
        let d = 40.0 * UNITS_PER_METER;
        let step = pan_step(Vec2::new(1.0, 1.0), d, 0.5);
        assert!((step.length() - d * PAN_ZOOMS_PER_S * 0.5).abs() < 1e-3);
        assert_eq!(pan_step(Vec2::ZERO, d, 0.5), Vec2::ZERO);
    }

    #[test]
    fn zoom_is_clamped_between_the_limits() {
        let near = ZOOM_NEAR_M * UNITS_PER_METER;
        let far = ZOOM_FAR_M * UNITS_PER_METER;
        let start = ZOOM_START_M * UNITS_PER_METER;
        assert!(zoom_target(start, 1.0) < start, "wheel up zooms in");
        assert_eq!(zoom_target(start, 100.0), near);
        assert_eq!(zoom_target(start, -100.0), far);
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
