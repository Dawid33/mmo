//! The DF-style top-down view of the commander camera: straight down, north
//! up, orthographic, one working level at a time. V toggles it with the 3D
//! commander rig. The cut is always on here (turning it off leaves top-down):
//! everything above the working level is cut away and the levels below show
//! darkened (`fortress_view`), exactly as with the cut on in 3D. The wheel
//! steps the level, Ctrl+wheel and `[` / `]` zoom in whole pixels per block
//! ([`TOP_DOWN_ZOOM_PX_PER_BLOCK`]), WASD / arrows / the screen edge pan at
//! a constant speed across the screen and a middle-drag moves the ground with
//! the mouse; there is no turning.
//!
//! The camera's `Projection` and `Transform` are derived from [`TopDown`]
//! every frame (`sync_top_down_projection`, `super::apply_camera_transform`),
//! so a session reset (which resets [`TopDown`]) brings back the 3D view
//! with nothing else to undo. The eye sits [`TOP_DOWN_EYE_HEIGHT_M`] above
//! the cut plane, so neighbouring terrain (which the cut does not touch) up
//! to that high is drawn rather than near-clipped.

use bevy::camera::ScalingMode;
use bevy::light::CascadeShadowConfig;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use game::movement::UNITS_PER_METER;
use game::BLOCK_UNITS;

use super::first_person::FirstPerson;
use super::level::WorkingLevel;
use super::{CommanderCamera, CommanderView};
use crate::renderer::cursor::CursorPolicy;

/// Toggles the top-down view.
pub const TOP_DOWN_KEY: KeyCode = KeyCode::KeyV;
/// Whether a world opens in the top-down view.
pub const TOP_DOWN_DEFAULT: bool = false;
/// The zoom steps, in screen pixels per block (16 = one block texel per pixel).
pub const TOP_DOWN_ZOOM_PX_PER_BLOCK: [f32; 9] = [4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0];
/// The zoom step a world opens at (an index into [`TOP_DOWN_ZOOM_PX_PER_BLOCK`]).
pub const TOP_DOWN_ZOOM_START: usize = 4;
/// Keys that zoom in / out one step (as do Ctrl + the wheel).
pub const TOP_DOWN_ZOOM_IN_KEYS: [KeyCode; 1] = [KeyCode::BracketRight];
pub const TOP_DOWN_ZOOM_OUT_KEYS: [KeyCode; 1] = [KeyCode::BracketLeft];
/// Keyboard / screen-edge pan speed, in screen pixels per second (so the
/// same at every zoom).
pub const TOP_DOWN_PAN_PX_PER_S: f32 = 900.0;
/// How far above the cut plane the eye sits, metres.
pub const TOP_DOWN_EYE_HEIGHT_M: f32 = 512.0;
/// How far below the eye's height above the cut the view reaches, metres
/// (well past bedrock).
pub const TOP_DOWN_DEPTH_M: f32 = 1024.0;

/// The 3D rig as it was before entering top-down, restored on leaving.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedRig {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target_distance: f32,
    pub cut: bool,
}

/// Whether the top-down view is on, its zoom step, and the 3D rig to go back to.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct TopDown {
    pub on: bool,
    /// Index into [`TOP_DOWN_ZOOM_PX_PER_BLOCK`].
    pub zoom: usize,
    pub saved: Option<SavedRig>,
}

impl Default for TopDown {
    fn default() -> Self {
        Self { on: TOP_DOWN_DEFAULT, zoom: TOP_DOWN_ZOOM_START, saved: None }
    }
}

impl TopDown {
    /// Screen pixels per block at the current zoom.
    pub fn px_per_block(&self) -> f32 {
        TOP_DOWN_ZOOM_PX_PER_BLOCK[self.zoom.min(TOP_DOWN_ZOOM_PX_PER_BLOCK.len() - 1)]
    }
}

/// Run condition: the top-down view is on.
pub fn active(td: Option<Res<TopDown>>) -> bool {
    td.is_some_and(|t| t.on)
}

/// Run condition: the top-down view is off (or not installed).
pub fn inactive(td: Option<Res<TopDown>>) -> bool {
    !active(td)
}

/// Sim units per PHYSICAL screen pixel at `px_per_block` (logical pixels)
/// and the window's `scale_factor`.
pub fn units_per_px(px_per_block: f32, scale_factor: f32) -> f32 {
    BLOCK_UNITS as f32 / (px_per_block * scale_factor)
}

/// The zoom step after `notches` (positive = in: more pixels per block),
/// clamped to the table.
pub fn zoom_step(zoom: usize, notches: i32) -> usize {
    (zoom as i64 + notches as i64).clamp(0, TOP_DOWN_ZOOM_PX_PER_BLOCK.len() as i64 - 1) as usize
}

/// `p` rounded on each axis to a multiple of `units_per_px`, so the block
/// grid lands on whole screen pixels.
pub fn snap_to_pixel(p: Vec2, units_per_px: f32) -> Vec2 {
    if units_per_px <= 0.0 {
        return p;
    }
    (p / units_per_px).round() * units_per_px
}

/// Looking straight down (−Y), with the top of the screen to the north (−Z).
pub fn top_down_rotation() -> Quat {
    Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)
}

/// The eye over `focus` (world x, z), [`TOP_DOWN_EYE_HEIGHT_M`] above the
/// cut plane at `cut_plane_y`, looking down.
pub fn top_down_transform(focus: Vec2, cut_plane_y: f32) -> Transform {
    Transform {
        translation: Vec3::new(focus.x, cut_plane_y + TOP_DOWN_EYE_HEIGHT_M * UNITS_PER_METER, focus.y),
        rotation: top_down_rotation(),
        scale: Vec3::ONE,
    }
}

/// The orthographic projection at zoom step `zoom`. Bevy sizes a
/// `WindowSize` projection in LOGICAL pixels, so its scale is the sim units
/// per logical pixel: [`units_per_px`] × the scale factor.
pub fn top_down_projection(zoom: usize, scale_factor: f32) -> OrthographicProjection {
    let px = TopDown { zoom, ..default() }.px_per_block();
    OrthographicProjection {
        near: 0.0,
        far: (TOP_DOWN_EYE_HEIGHT_M + TOP_DOWN_DEPTH_M) * UNITS_PER_METER,
        scaling_mode: ScalingMode::WindowSize,
        scale: units_per_px(px, scale_factor) * scale_factor,
        ..OrthographicProjection::default_3d()
    }
}

/// Ground displacement (sim units) of one keyboard / screen-edge pan frame in
/// screen direction `dir` (x right = east, y down = south): a constant
/// [`TOP_DOWN_PAN_PX_PER_S`] across the screen at `units_per_px`.
pub fn top_down_pan_step(dir: Vec2, units_per_px: f32, dt: f32) -> Vec2 {
    dir.normalize_or_zero() * TOP_DOWN_PAN_PX_PER_S * units_per_px * dt
}

/// Ground displacement of a middle-drag of `delta` pixels: the ground under
/// the cursor moves with it.
pub fn top_down_drag(delta: Vec2, units_per_px: f32) -> Vec2 {
    -delta * units_per_px
}

/// Wheel notches this frame (touchpads: ~one per 40 px, as in 3D).
fn wheel_notches(scroll: &AccumulatedMouseScroll) -> f32 {
    match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
    }
}

/// The top-down view's input (instead of `super::pan_and_zoom`): the wheel
/// steps the level, Ctrl+wheel and `[` / `]` zoom, WASD / arrows / the
/// screen edge pan (sprint as in 3D), a middle-drag moves the ground with the
/// mouse. Any pan ends following. Space / Shift / PgUp / PgDn / Home stay
/// `level::level_input`'s (the cut is on).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn top_down_input(
    mut follow: ResMut<super::follow::Follow>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    policy: Res<CursorPolicy>,
    minimap: Res<super::minimap::MinimapPointer>,
    grid: Res<super::height::HeightGrid>,
    mut td: ResMut<TopDown>,
    mut cam: ResMut<CommanderCamera>,
    mut working: ResMut<WorkingLevel>,
    mut sprint: crate::session::SessionLocal<super::Sprint>,
    mut carry: crate::session::SessionLocal<[f32; 2]>,
) {
    let Ok(window) = window.single() else { return };
    let sprint = sprint.get();
    let carry = carry.get();
    if policy.blocks_game() {
        *sprint = super::Sprint::default();
        *carry = [0.0; 2];
        return;
    }
    *sprint = sprint.update(keys.just_pressed(super::SPRINT_TAP_KEY), keys.pressed(super::SPRINT_TAP_KEY), time.elapsed_secs());
    let speed = if sprint.latched || keys.any_pressed(super::SPRINT_KEYS) { super::SPRINT_FACTOR } else { 1.0 };
    let size = window.size();
    let logical = units_per_px(td.px_per_block(), 1.0);
    let physical = units_per_px(td.px_per_block(), window.scale_factor());

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
    let dragging = buttons.any_pressed([MouseButton::Middle, MouseButton::Right]);
    let cursor = window.cursor_position();
    let over_minimap = cursor.is_some_and(|c| super::minimap::screen_rect(size).contains(c));
    if let Some(c) = cursor {
        if window.focused && !over_minimap && !minimap.dragging && !policy.ui_pointer && !dragging {
            dir += super::edge_pan_dir(c, size, super::EDGE_BAND_PX);
        }
    }
    let mut step = top_down_pan_step(dir, logical, time.delta_secs()) * speed;
    if buttons.pressed(MouseButton::Middle) && window.focused {
        step += top_down_drag(motion.delta, physical);
    }
    if step != Vec2::ZERO && follow.0.is_some() {
        follow.0 = None;
    }
    cam.focus += step;

    let mut zoom = keys.any_just_pressed(TOP_DOWN_ZOOM_IN_KEYS) as i32 - keys.any_just_pressed(TOP_DOWN_ZOOM_OUT_KEYS) as i32;
    let notches = wheel_notches(&scroll);
    if notches != 0.0 && !over_minimap && !policy.ui_pointer {
        if keys.any_pressed(super::level::LEVEL_FAST_KEYS) {
            zoom += super::level::wheel_level_steps(notches, &mut carry[1]);
        } else if let Some(current) = working.level {
            let range = grid.surface_range().map_or((current, current), |s| super::level::level_range(s));
            let next = super::level::wheel_step_level(current, notches, &mut carry[0], false, range);
            if next != current {
                working.level = Some(next);
            }
        }
    }
    if zoom != 0 {
        let next = zoom_step(td.zoom, zoom);
        if next != td.zoom {
            td.zoom = next;
        }
    }
}

/// The surface level of the lowest loaded ground (`HeightGrid::surface_range`).
pub fn lowest_surface_level(surface_range: Option<(f32, f32)>) -> Option<i32> {
    surface_range.map(|(lo, _)| super::level::surface_level(lo))
}

/// Whether the top-down view looks at an underground level: the working
/// `level` below `surface` — the lowest loaded surface's level
/// ([`lowest_surface_level`]), so the whole view is under ground and panning
/// over a hill does not relight the screen. Lit then like the 3D eye inside
/// rock — no sun shadows, more ambient (`super::underground_view`) — since
/// the cut lets the sun onto every dug level.
pub fn top_down_underground(level: Option<i32>, surface: Option<i32>) -> bool {
    matches!((level, surface), (Some(l), Some(s)) if l < s)
}

/// Toggle the top-down view (the context box's "3D view" button).
#[derive(Message, Debug, Clone, Copy)]
pub struct ToggleTopDownRequest;

/// Enter top-down: remember the 3D rig and the cut, and turn the cut on.
pub fn enter(td: &mut TopDown, cam: &CommanderCamera, working: &mut WorkingLevel) {
    td.saved = Some(SavedRig {
        yaw: cam.yaw,
        pitch: cam.pitch,
        distance: cam.distance,
        target_distance: cam.target_distance,
        cut: working.cut,
    });
    working.cut = true;
    td.on = true;
}

/// Leave top-down: the 3D rig and the cut come back as they were on entry
/// (the focus, its height and the level stay where top-down left them);
/// `cut`, when given, overrides the restored cut — leaving because the cut
/// was turned off keeps it off.
pub fn leave(td: &mut TopDown, cam: &mut CommanderCamera, working: &mut WorkingLevel, cut: Option<bool>) {
    if let Some(saved) = td.saved.take() {
        (cam.yaw, cam.pitch, cam.distance, cam.target_distance) =
            (saved.yaw, saved.pitch, saved.distance, saved.target_distance);
        working.cut = saved.cut;
    }
    if let Some(cut) = cut {
        working.cut = cut;
        if !cut {
            // Into the free camera, which has no zoom: its opening distance,
            // as `level::track_camera_level` gives it on any cut-off.
            cam.distance = super::ZOOM_START_M * UNITS_PER_METER;
            cam.target_distance = cam.distance;
        }
    }
    cam.rise = 0.0;
    cam.pivot = None;
    td.on = false;
}

/// V (or the context box's button) enters / leaves top-down; turning the cut
/// off (C, the tool wheel) leaves it, into the 3D free camera. Not in first
/// person (which leaves top-down on its way in).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn toggle_top_down(
    mut requests: MessageReader<ToggleTopDownRequest>,
    keys: Res<ButtonInput<KeyCode>>,
    policy: Res<CursorPolicy>,
    fp: Option<Res<FirstPerson>>,
    mut td: ResMut<TopDown>,
    mut cam: ResMut<CommanderCamera>,
    mut working: ResMut<WorkingLevel>,
) {
    let pressed = requests.read().count() > 0 || (keys.just_pressed(TOP_DOWN_KEY) && !policy.blocks_game());
    if fp.is_some_and(|fp| fp.on) {
        return;
    }
    if td.on && !working.cut {
        leave(&mut td, &mut cam, &mut working, Some(false));
    } else if pressed && td.on {
        leave(&mut td, &mut cam, &mut working, None);
    } else if pressed {
        enter(&mut td, &cam, &mut working);
    }
}

/// The commander camera's projection from [`TopDown`]: orthographic at the
/// zoom while on, the 3D perspective otherwise. Written only on a change.
pub fn sync_top_down_projection(
    td: Option<Res<TopDown>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut views: Query<&mut Projection, With<CommanderView>>,
    mut suns: Query<&mut CascadeShadowConfig, With<DirectionalLight>>,
) {
    let on = td.as_ref().is_some_and(|t| t.on);
    // The sun's cascades are measured from the eye: the top-down one is far
    // above the ground.
    let cascades = if on { crate::renderer::atmosphere::top_down_cascades() } else { crate::renderer::atmosphere::sun_cascades() };
    for mut config in &mut suns {
        if config.bounds != cascades.bounds || config.minimum_distance != cascades.minimum_distance {
            *config = cascades.clone();
        }
    }
    let scale_factor = window.single().map_or(1.0, |w| w.scale_factor());
    for mut projection in &mut views {
        match (on, &*projection) {
            (true, Projection::Orthographic(o)) => {
                let want = top_down_projection(td.as_ref().map_or(TOP_DOWN_ZOOM_START, |t| t.zoom), scale_factor);
                if o.scale != want.scale || o.far != want.far {
                    *projection = Projection::Orthographic(want);
                }
            }
            (true, _) => {
                let zoom = td.as_ref().map_or(TOP_DOWN_ZOOM_START, |t| t.zoom);
                *projection = Projection::Orthographic(top_down_projection(zoom, scale_factor));
            }
            (false, Projection::Perspective(_)) => {}
            (false, _) => *projection = Projection::Perspective(super::commander_perspective()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_straight_down_with_north_up() {
        let r = top_down_rotation();
        assert!((r * Vec3::NEG_Z).abs_diff_eq(Vec3::NEG_Y, 1e-6), "forward is down");
        assert!((r * Vec3::Y).abs_diff_eq(Vec3::NEG_Z, 1e-6), "screen up is north");
    }

    #[test]
    fn the_eye_sits_above_the_cut_over_the_focus() {
        let tf = top_down_transform(Vec2::new(100.0, -50.0), 1024.0);
        assert_eq!(tf.translation, Vec3::new(100.0, 1024.0 + TOP_DOWN_EYE_HEIGHT_M * UNITS_PER_METER, -50.0));
    }

    #[test]
    fn zoom_is_whole_physical_pixels_per_block_at_any_scale_factor() {
        assert_eq!(units_per_px(16.0, 1.0) * 16.0, game::BLOCK_UNITS as f32);
        assert_eq!(units_per_px(16.0, 2.0) * 32.0, game::BLOCK_UNITS as f32);
    }

    #[test]
    fn zoom_steps_clamp_to_the_table() {
        assert_eq!(zoom_step(TOP_DOWN_ZOOM_START, 1), TOP_DOWN_ZOOM_START + 1);
        assert_eq!(zoom_step(0, -3), 0);
        let last = TOP_DOWN_ZOOM_PX_PER_BLOCK.len() - 1;
        assert_eq!(zoom_step(last, 5), last);
    }

    #[test]
    fn the_focus_snaps_to_whole_pixels() {
        let u = units_per_px(16.0, 1.0);
        let s = snap_to_pixel(Vec2::new(10.3 * u, -4.6 * u), u);
        assert_eq!(s, Vec2::new(10.0 * u, -5.0 * u));
    }

    #[test]
    fn the_projection_reaches_from_the_eye_past_bedrock() {
        let p = top_down_projection(TOP_DOWN_ZOOM_START, 1.0);
        assert_eq!(p.near, 0.0);
        assert!(p.far >= (TOP_DOWN_EYE_HEIGHT_M + game::UNDERGROUND_DEPTH as f32 + 8.0) * UNITS_PER_METER);
    }

    fn rig() -> (TopDown, CommanderCamera, WorkingLevel) {
        let cam = CommanderCamera { yaw: 1.0, pitch: 0.7, distance: 300.0, target_distance: 320.0, ..default() };
        (TopDown::default(), cam, WorkingLevel { level: Some(70), cut: false, ..Default::default() })
    }

    #[test]
    fn entering_forces_the_cut_and_leaving_restores_the_rig() {
        let (mut td, mut cam, mut working) = rig();
        enter(&mut td, &cam, &mut working);
        assert!(td.on && working.cut);
        cam.focus = Vec2::new(5.0, 6.0);
        (cam.yaw, cam.pitch, cam.distance) = (0.0, 1.5, 10.0);
        leave(&mut td, &mut cam, &mut working, None);
        assert!(!td.on && td.saved.is_none());
        assert_eq!((cam.yaw, cam.pitch, cam.distance, cam.target_distance), (1.0, 0.7, 300.0, 320.0));
        assert!(!working.cut, "the cut as it was");
        assert_eq!(cam.focus, Vec2::new(5.0, 6.0), "the focus stays where top-down left it");
        assert_eq!(working.level, Some(70), "the level is untouched");
    }

    #[test]
    fn leaving_by_turning_the_cut_off_keeps_it_off() {
        let (mut td, mut cam, mut working) = rig();
        working.cut = true;
        enter(&mut td, &cam, &mut working);
        leave(&mut td, &mut cam, &mut working, Some(false));
        assert!(!working.cut);
        // The free camera has no zoom: the cut-on wheel zoom does not carry over.
        let start = super::super::ZOOM_START_M * UNITS_PER_METER;
        assert_eq!((cam.distance, cam.target_distance), (start, start));
    }

    #[test]
    fn underground_is_below_the_lowest_loaded_surface() {
        let b = game::tile_height_units();
        assert_eq!(lowest_surface_level(Some((40.5 * b, 90.0 * b))), Some(40));
        assert_eq!(lowest_surface_level(None), None);
        // A hill under the focus does not make the valley's level underground.
        let lowest = lowest_surface_level(Some((40.0 * b, 90.0 * b)));
        assert!(!top_down_underground(Some(45), lowest));
        assert!(top_down_underground(Some(39), lowest));
    }

    #[test]
    fn the_projection_follows_the_state_so_a_session_reset_brings_back_perspective() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(TopDown { on: true, ..default() });
        let cam = world.spawn((super::super::CommanderView, Projection::Perspective(super::super::commander_perspective()))).id();
        world.run_system_once(sync_top_down_projection).unwrap();
        assert!(matches!(world.get::<Projection>(cam), Some(Projection::Orthographic(_))));
        *world.resource_mut::<TopDown>() = TopDown::default();
        world.run_system_once(sync_top_down_projection).unwrap();
        let Some(Projection::Perspective(p)) = world.get::<Projection>(cam) else { panic!("perspective again") };
        assert_eq!(p.fov, super::super::FOV_Y_DEG.to_radians());
    }

    #[test]
    fn a_home_change_recentres_the_top_down_eye() {
        let rect = super::super::region_rect(game::RegionCoords { x: 2, z: -1 });
        let focus = super::super::home_change_focus(Vec2::ZERO, Some(rect), false);
        let tf = top_down_transform(focus, 0.0);
        assert_eq!(Vec2::new(tf.translation.x, tf.translation.z), rect.center());
    }

    #[test]
    fn pan_speed_is_constant_on_screen_at_every_zoom() {
        for px in [16.0, 4.0] {
            let u = units_per_px(px, 1.0);
            let step = top_down_pan_step(Vec2::X, u, 1.0);
            assert!((step.x / u - TOP_DOWN_PAN_PX_PER_S).abs() < 1e-3, "{px} px/block");
        }
        let u = units_per_px(16.0, 1.0);
        assert_eq!(top_down_pan_step(Vec2::new(3.0, 4.0), u, 1.0).length(), TOP_DOWN_PAN_PX_PER_S * u, "normalised");
    }

    #[test]
    fn a_middle_drag_moves_the_ground_with_the_mouse() {
        let u = units_per_px(16.0, 1.0);
        assert_eq!(top_down_drag(Vec2::new(10.0, -5.0), u), Vec2::new(-10.0 * u, 5.0 * u));
    }

    #[test]
    fn a_level_below_the_surface_is_underground() {
        assert!(top_down_underground(Some(10), Some(20)));
        assert!(!top_down_underground(Some(20), Some(20)));
        assert!(!top_down_underground(Some(25), Some(20)));
        assert!(!top_down_underground(None, Some(20)));
        assert!(!top_down_underground(Some(10), None));
    }

    #[test]
    fn top_down_starts_off_at_the_start_zoom() {
        assert_eq!(TopDown::default(), TopDown { on: TOP_DOWN_DEFAULT, zoom: TOP_DOWN_ZOOM_START, saved: None });
    }
}
