//! The body view in Adventure: the view is in (or just behind) the
//! adventurer and the keyboard and mouse drive the body, Minecraft style —
//! WASD walk, Space jumps, Ctrl sprints, the mouse looks, left / right click
//! break / place.
//!
//! It is level-triggered ([`ensure_first_person`]): while the world has the
//! screen, the local player is known and the play mode is Adventure, the body
//! view is on — every frame it is not, it is put on again (a one-shot request
//! written before `LocalPlayer` was set used to be lost, leaving the player in
//! the commander camera). Leaving Adventure (Overview: only the autopilot goes
//! there now) turns it off. [`BODY_VIEW_KEY`] (F5) switches between first and
//! third person ([`BodyView`]) and never leaves the body: the player has no
//! path into the commander camera.
//!
//! Client-local view state over the existing first-person sim path: the sim
//! drives the body from the keyboard once the client sends
//! `InputEvent::FpsCam(true)` (which also ends any walk, `tick_walkers_safe`),
//! `input::forward_input` forwards keys / mouse / clicks while [`FirstPerson`]
//! is on, and the view is the [`Eyes`] camera ([`sync_first_person_camera`]: one
//! entity for the session, following whichever body is home, so a seam crossing
//! neither drops nor re-creates it). Third person is the same in every respect
//! but the render camera, which [`place_third_person_camera`] puts
//! [`THIRD_PERSON_DISTANCE_M`] behind the eyes (closer when terrain is in the
//! way), and the body's model, which shows. The commander camera, its input
//! systems ([`inactive`]) and (in first person) the body's own model stand
//! aside; the cut and the dug-space x-ray are switched off (they would clip
//! the view) and put back on the way out, and the commander camera comes back
//! over the body. Esc opens the menu and frees the pointer.

use bevy::prelude::*;
use game::movement::UNITS_PER_METER;
use game::InputEvent;

use super::adventure::ClientPlayMode;
use super::level::WorkingLevel;
use super::tiles::TileMirror;
use super::CommanderCamera;
use crate::renderer::bridge::{LocalBody, SimEntity, SimTarget};
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::look::look_rotation;
use crate::renderer::voxel_pull::xray::XraySettings;
use crate::renderer::{GameEvents, LocalPlayer, UiCamera};
use crate::session::{AppState, InPlay};

/// Switches first ↔ third person (Minecraft's perspective key).
pub const BODY_VIEW_KEY: KeyCode = KeyCode::F5;
/// Vertical field of view of the eyes.
pub const FIRST_PERSON_FOV_Y_DEG: f32 = 70.0;
/// Near clip plane of the eyes, metres.
pub const FIRST_PERSON_NEAR_M: f32 = 0.05;
/// Third person: how far behind the eyes the camera sits, metres.
pub const THIRD_PERSON_DISTANCE_M: f32 = 4.0;
/// Third person: how far short of terrain behind the eyes the camera stops, metres.
pub const THIRD_PERSON_WALL_MARGIN_M: f32 = 0.25;
/// Third person: a camera pulled in closer than this to the eyes (metres)
/// would sit inside the head, so the body is hidden as in first person.
pub const THIRD_PERSON_HIDE_BODY_M: f32 = 0.5;

/// Where the body view looks from: the eyes, or behind the body.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyView {
    #[default]
    First,
    Third,
}

impl BodyView {
    /// The other view ([`BODY_VIEW_KEY`]).
    pub fn toggled(self) -> Self {
        match self {
            BodyView::First => BodyView::Third,
            BodyView::Third => BodyView::First,
        }
    }
}

/// Whether the view is with the local body, from where, and what it switched
/// off to get there.
#[derive(Resource, Default, Debug)]
pub struct FirstPerson {
    pub on: bool,
    /// First or third person (F5); kept while `on` is off.
    pub view: BodyView,
    /// Third person: terrain pulled the camera inside the head
    /// ([`third_person_hides_body`]); written by [`place_third_person_camera`].
    pub camera_in_body: bool,
    /// The cut and the x-ray switch as they were on entry.
    saved: Option<(bool, bool)>,
}

impl FirstPerson {
    /// The body view's camera, while the body view is on.
    pub fn body_view(&self) -> Option<BodyView> {
        self.on.then_some(self.view)
    }

    /// The body view as the body's model sees it: a third-person camera
    /// pulled inside the head counts as first person (the model is hidden).
    pub fn view_for_body(&self) -> Option<BodyView> {
        match self.body_view() {
            Some(BodyView::Third) if self.camera_in_body => Some(BodyView::First),
            view => view,
        }
    }
}

/// Run condition: the commander's own input (pan, zoom, levels, tools,
/// selection, move orders) — off while the body has the keyboard.
pub fn inactive(fp: Option<Res<FirstPerson>>) -> bool {
    fp.is_none_or(|fp| !fp.on)
}

/// Whether the body view should be on: always in Adventure once the world has
/// the screen; while the frontend has it, it stays as it is (it is not entered
/// behind a menu screen); never outside Adventure.
pub fn wanted(on: bool, adventure: bool, world_owns: bool) -> bool {
    adventure && (on || world_owns)
}

/// Keeps the body view on in Adventure and off elsewhere — level-triggered,
/// every frame: a frame with no local player yet changes nothing and the next
/// one tries again.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn ensure_first_person(
    mode: Res<ClientPlayMode>,
    state: Option<Res<State<AppState>>>,
    in_play: Option<Res<InPlay>>,
    mut policy: ResMut<CursorPolicy>,
    mut fp: ResMut<FirstPerson>,
    mut working: ResMut<WorkingLevel>,
    mut xray: Option<ResMut<XraySettings>>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
    mirror: Res<TileMirror>,
    bodies: Query<(&SimEntity, &GlobalTransform), With<LocalBody>>,
    mut cam: ResMut<CommanderCamera>,
    mut top_down: Option<ResMut<super::top_down::TopDown>>,
) {
    let world_owns = !crate::session::frontend_has_screen(state.as_deref(), in_play.as_deref());
    let want = wanted(fp.on, mode.adventure(), world_owns);
    if want == fp.on {
        return;
    }
    let Some(client) = local.0 else { return };
    fp.on = want;
    // Nobody pressed for the eyes (Adventure starting): the pointer stays free
    // until a click in the window (`cursor::retake_pointer`).
    policy.released = want;
    let _ = events.0.send(game::GameEventKind::PlayerInput(client, InputEvent::FpsCam(want)));
    if want {
        // The eyes replace the top-down view: back to the 3D rig first, so
        // leaving the body view returns to it.
        if let Some(td) = top_down.as_mut().filter(|td| td.on) {
            super::top_down::leave(td, &mut cam, &mut working, None);
        }
        fp.saved = Some((working.cut, xray.as_ref().is_some_and(|x| x.enabled)));
        working.cut = false;
        if let Some(x) = xray.as_mut() {
            x.enabled = false;
        }
    } else {
        if let Some((cut, x)) = fp.saved.take() {
            working.cut = cut;
            if let Some(xray) = xray.as_mut() {
                xray.enabled = x;
            }
        }
        // Back over the body, where the eyes were.
        let home = mirror.region();
        if let Some((_, gt)) = bodies.iter().find(|(s, _)| Some(s.region) == home) {
            let at = gt.translation();
            cam.focus = Vec2::new(at.x, at.z);
            cam.ground_y = at.y;
            cam.rise = 0.0;
        }
    }
    policy.first_person = want;
}

/// [`BODY_VIEW_KEY`] → first ↔ third person, in the body view only. It never
/// leaves the body view.
pub fn cycle_body_view(
    keys: Res<ButtonInput<KeyCode>>,
    policy: Res<CursorPolicy>,
    mode: Res<ClientPlayMode>,
    mut fp: ResMut<FirstPerson>,
) {
    if keys.just_pressed(BODY_VIEW_KEY) && !policy.blocks_game() && mode.adventure() && fp.on {
        fp.view = fp.view.toggled();
    }
}

/// The first-person render camera: ONE entity for the whole session, in world
/// space, not the body's. A seam crossing despawns the body in the region it
/// leaves one tick before the next region has it (`EntityArrived` is applied
/// at T+1), and a camera on the body went with it: three frames with no eyes
/// (the sky black, the terrain drawn without its fog), then a new camera that
/// started at the body's centre and rose to the eye height. The eyes follow
/// whichever local body is home and hold their pose while there is none.
#[derive(Component)]
pub struct Eyes;

/// The body the eyes follow: the one in the home region, or the only one
/// there is (the home the tile mirror reports can trail the body by a frame).
fn eyes_body<'a>(
    home: Option<game::RegionId>,
    bodies: impl Iterator<Item = (&'a SimEntity, &'a SimTarget)>,
) -> Option<(&'a SimEntity, &'a SimTarget)> {
    let mut only = None;
    let mut count = 0;
    for (sim, target) in bodies {
        if Some(sim.region) == home {
            return Some((sim, target));
        }
        only = Some((sim, target));
        count += 1;
    }
    only.filter(|_| count == 1)
}

/// A region-local sim position in world space (bodies are children of their
/// region's root, which sits at the region's offset).
fn world_pos(region: game::RegionId, local: Vec3) -> Vec3 {
    let offset = region.world_offset();
    Vec3::new(offset[0], offset[1], offset[2]) + local
}

/// The [`Eyes`] while the body view is on and the world has the screen, and
/// none otherwise (the commander camera stands aside in
/// `yield_commander_camera`; the frontend camera alone carries `UiCamera`
/// while it has the screen). Each frame their `SimTarget` takes the home
/// body's pose in world space, and `interpolate_transforms` /
/// `look::apply_local_look` move them like any local camera; with no body (a
/// crossing's one-tick gap) they stay put.
pub fn sync_first_person_camera(
    mut commands: Commands,
    fp: Res<FirstPerson>,
    state: Option<Res<State<AppState>>>,
    in_play: Option<Res<InPlay>>,
    mirror: Res<TileMirror>,
    bodies: Query<(&SimEntity, &SimTarget), (With<LocalBody>, Without<Eyes>)>,
    mut eyes: Query<(Entity, &mut SimTarget), With<Eyes>>,
) {
    if !fp.on || crate::session::frontend_has_screen(state.as_deref(), in_play.as_deref()) {
        for (e, _) in &eyes {
            // `try_`: the session's end may despawn them (`SessionScoped`)
            // in the same frame the frontend takes the screen.
            commands.entity(e).try_despawn();
        }
        return;
    }
    let Some((sim, body)) = eyes_body(mirror.region(), bodies.iter()) else { return };
    let (pos, rot) = (world_pos(sim.region, body.pos), body.rot);
    if let Ok((_, mut target)) = eyes.single_mut() {
        if target.pos != pos || target.rot != rot {
            target.pos = pos;
            target.rot = rot;
        }
        return;
    }
    let target = SimTarget::camera(pos, rot);
    commands.spawn((
        Eyes,
        ThirdPersonPull::default(),
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: FIRST_PERSON_FOV_Y_DEG.to_radians(),
            near: FIRST_PERSON_NEAR_M * UNITS_PER_METER,
            far: game::lod::LOD_HORIZON_UNITS,
            ..default()
        }),
        // At the eye height from the first frame: no rise from the body's centre.
        Transform { translation: target.goal(), rotation: rot, ..default() },
        target,
        UiCamera,
        crate::session::SessionScoped,
        Name::new("first-person eyes"),
    ));
}

/// What [`place_third_person_camera`] added to the [`Eyes`]' translation this
/// frame (zero in first person). `interpolate_transforms` glides the
/// translation toward the eye, so the pull is taken off again before it runs
/// ([`restore_eye_point`]): the glide always works on the eye itself.
#[derive(Component, Default, Debug, Clone, Copy)]
pub struct ThirdPersonPull(pub Vec3);

/// Takes last frame's third-person pull off the [`Eyes`] (before
/// `interpolate_transforms`).
pub fn restore_eye_point(mut eyes: Query<(&mut Transform, &mut ThirdPersonPull), With<Eyes>>) {
    for (mut tf, mut pull) in &mut eyes {
        if pull.0 != Vec3::ZERO {
            tf.translation -= pull.0;
            pull.0 = Vec3::ZERO;
        }
    }
}

/// Where the third-person camera sits for an eye at `eye` looking along
/// (`yaw`, `pitch`): [`THIRD_PERSON_DISTANCE_M`] straight behind it along the
/// look, or — when terrain lies `hit` sim units behind the eye —
/// [`THIRD_PERSON_WALL_MARGIN_M`] short of it, never past the eye.
pub fn third_person_position(eye: Vec3, yaw: f32, pitch: f32, hit: Option<f32>) -> Vec3 {
    let full = THIRD_PERSON_DISTANCE_M * UNITS_PER_METER;
    let d = hit.map_or(full, |h| (h - THIRD_PERSON_WALL_MARGIN_M * UNITS_PER_METER).clamp(0.0, full));
    eye + third_person_back(yaw, pitch) * d
}

/// The unit direction from the eye back toward the third-person camera: the
/// look reversed (a Bevy camera looks along its local −Z).
fn third_person_back(yaw: f32, pitch: f32) -> Vec3 {
    look_rotation(yaw, pitch) * Vec3::Z
}

/// How far (sim units) the ray from `origin` along unit `dir` travels before it
/// enters the box `min..max`; 0 when it starts inside.
fn box_entry_distance(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> f32 {
    let mut t = 0.0f32;
    for a in 0..3 {
        if dir[a] != 0.0 {
            let (t0, t1) = ((min[a] - origin[a]) / dir[a], (max[a] - origin[a]) / dir[a]);
            t = t.max(t0.min(t1));
        }
    }
    t
}

/// The distance (sim units) from `eye` (region-local) back along `dir` to the
/// first block `solid` (region-local block) accepts, within the full
/// third-person distance.
fn third_person_hit(eye: Vec3, dir: Vec3, solid: impl Fn(game::TilePos) -> bool) -> Option<f32> {
    let size = game::BLOCK_VOXELS as f32;
    let reach = THIRD_PERSON_DISTANCE_M * UNITS_PER_METER / size;
    let v = |p: Vec3| game::rapier3d::math::Vector::new(p.x, p.y, p.z);
    let hit = game::edit::raycast_blocks(v(eye), v(dir), reach, solid)?;
    let min = Vec3::new(hit.block[0] as f32, hit.block[1] as f32, hit.block[2] as f32) * size;
    Some(box_entry_distance(eye, dir, min, min + Vec3::splat(size)))
}

/// Whether a third-person camera at `camera` is close enough to the eye at
/// `eye` to sit inside the head ([`THIRD_PERSON_HIDE_BODY_M`]).
pub fn third_person_hides_body(eye: Vec3, camera: Vec3) -> bool {
    camera.distance(eye) < THIRD_PERSON_HIDE_BODY_M * UNITS_PER_METER
}

/// Third person: moves the [`Eyes`] camera from the eye to
/// [`third_person_position`] (after `look::apply_local_look` wrote the look
/// rotation, which it keeps), pulled in toward the eye when the home region's
/// terrain (or its halo) is in the way.
pub fn place_third_person_camera(
    mut fp: ResMut<FirstPerson>,
    mirror: Res<TileMirror>,
    mut eyes: Query<(&mut Transform, &mut ThirdPersonPull), With<Eyes>>,
) {
    if fp.body_view() != Some(BodyView::Third) {
        return;
    }
    for (mut tf, mut pull) in &mut eyes {
        let eye = tf.translation;
        let (yaw, pitch, _roll) = tf.rotation.to_euler(EulerRot::YXZ);
        let hit = mirror.region().and_then(|home| {
            let offset = Vec3::from_array(home.world_offset());
            let halo = mirror.with_halo();
            third_person_hit(eye - offset, third_person_back(yaw, pitch), |p| {
                game::block_props().blocks_ray(game::TileSource::block(&halo, p))
            })
        });
        let at = third_person_position(eye, yaw, pitch, hit);
        tf.translation = at;
        pull.0 = at - eye;
        // Written only on a change: `sync_body_visibility` reacts to it.
        let inside = third_person_hides_body(eye, at);
        if fp.camera_in_body != inside {
            fp.camera_in_body = inside;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eyes_follow_the_home_body_or_the_only_one() {
        let key = game::EntityKey::default();
        let (a, b) = (game::RegionCoords::new(0, 0), game::RegionCoords::new(1, 0));
        let body = |region| (SimEntity { region, key }, SimTarget::body(Vec3::ZERO, Quat::IDENTITY));
        let (in_a, in_b) = (body(a), body(b));
        let pick = |home, list: &[&(SimEntity, SimTarget)]| {
            eyes_body(home, list.iter().map(|(s, t)| (s, t))).map(|(s, _)| s.region)
        };
        assert_eq!(pick(Some(b), &[&in_a, &in_b]), Some(b), "the home body");
        assert_eq!(pick(Some(a), &[&in_b]), Some(b), "the only body, though home still names the old region");
        assert_eq!(pick(None, &[&in_a, &in_b]), None, "two bodies and no home: hold");
        assert_eq!(pick(Some(a), &[]), None, "no body (the crossing's gap): hold");
    }

    #[test]
    fn a_body_pose_is_carried_into_world_space() {
        let region = game::RegionCoords::new(2, -1);
        let offset = region.world_offset();
        let at = world_pos(region, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(at, Vec3::new(offset[0] + 1.0, offset[1] + 2.0, offset[2] + 3.0));
    }

    #[test]
    fn the_body_view_is_wanted_in_adventure_once_the_world_has_the_screen() {
        assert!(wanted(false, true, true), "Adventure in the world: on");
        assert!(wanted(true, true, true), "and it stays on");
        assert!(!wanted(false, true, false), "not entered behind a frontend screen");
        assert!(wanted(true, true, false), "nor left for one");
        assert!(!wanted(true, false, true), "leaving Adventure leaves it");
        assert!(!wanted(false, false, true), "no body view in the overview");
    }

    /// The systems the body view runs on, without a window.
    fn body_view_app() -> (App, crossbeam::channel::Receiver<game::GameEventKind>) {
        let (send, recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ClientPlayMode>()
            .init_resource::<CursorPolicy>()
            .init_resource::<FirstPerson>()
            .init_resource::<WorkingLevel>()
            .init_resource::<TileMirror>()
            .init_resource::<CommanderCamera>()
            .insert_resource(LocalPlayer(None))
            .insert_resource(GameEvents(send))
            .add_systems(Update, (ensure_first_person, cycle_body_view).chain());
        (app, recv)
    }

    fn fps_cam_sent(recv: &crossbeam::channel::Receiver<game::GameEventKind>) -> Vec<bool> {
        recv.try_iter()
            .filter_map(|e| match e {
                game::GameEventKind::PlayerInput(_, InputEvent::FpsCam(on)) => Some(on),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_body_view_waits_for_the_local_player_instead_of_dropping_the_entry() {
        let (mut app, recv) = body_view_app();
        // The first frames of a session: Adventure, but no `LocalPlayer` yet.
        for _ in 0..3 {
            app.update();
        }
        assert!(!app.world().resource::<FirstPerson>().on, "nothing to drive yet");
        assert_eq!(fps_cam_sent(&recv), Vec::<bool>::new());

        app.world_mut().resource_mut::<LocalPlayer>().0 = Some(7);
        app.update();
        let fp = app.world().resource::<FirstPerson>();
        assert!(fp.on, "on as soon as the player is known");
        assert_eq!(fp.view, BodyView::First, "in the eyes");
        assert_eq!(fps_cam_sent(&recv), vec![true], "the sim told once");
        assert!(app.world().resource::<CursorPolicy>().first_person);

        app.update();
        assert_eq!(fps_cam_sent(&recv), Vec::<bool>::new(), "and not again");

        app.world_mut().resource_mut::<ClientPlayMode>().0 = game::PlayMode::Overview;
        app.update();
        assert!(!app.world().resource::<FirstPerson>().on, "the overview (autopilot) leaves it");
        assert_eq!(fps_cam_sent(&recv), vec![false]);
    }

    #[test]
    fn f5_flips_first_and_third_person_and_never_leaves_the_body() {
        let (mut app, recv) = body_view_app();
        app.world_mut().resource_mut::<LocalPlayer>().0 = Some(7);
        app.update();
        let press = |app: &mut App| {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.reset_all();
            keys.press(BODY_VIEW_KEY);
            app.update();
            app.world_mut().resource_mut::<ButtonInput<KeyCode>>().reset_all();
            app.update();
        };
        let view = |app: &App| app.world().resource::<FirstPerson>().body_view();
        assert_eq!(view(&app), Some(BodyView::First));
        press(&mut app);
        assert_eq!(view(&app), Some(BodyView::Third), "F5: behind the body");
        press(&mut app);
        assert_eq!(view(&app), Some(BodyView::First), "F5 again: back in the eyes");
        press(&mut app);
        assert_eq!(view(&app), Some(BodyView::Third));
        assert_eq!(fps_cam_sent(&recv), vec![true], "the sim stays in the body view throughout");
    }

    const M: f32 = UNITS_PER_METER;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-3
    }

    #[test]
    fn third_person_sits_behind_the_eye_along_the_look() {
        let eye = Vec3::new(10.0, 20.0, 30.0);
        // Yaw 0, level: the look is −Z, so behind is +Z.
        assert!(close(third_person_position(eye, 0.0, 0.0, None), eye + Vec3::new(0.0, 0.0, 4.0 * M)));
        // Looking 30° down: behind is up and back.
        let pitch = -30f32.to_radians();
        let at = third_person_position(eye, 0.0, pitch, None);
        let want = eye + Vec3::new(0.0, 4.0 * M * 0.5, 4.0 * M * 30f32.to_radians().cos());
        assert!(close(at, want), "{at:?} vs {want:?}");
        assert!((at.distance(eye) - 4.0 * M).abs() < 1e-3);
        // Turned a quarter left (yaw +90°): the look is −X, behind is +X.
        let at = third_person_position(eye, std::f32::consts::FRAC_PI_2, 0.0, None);
        assert!(close(at, eye + Vec3::new(4.0 * M, 0.0, 0.0)), "{at:?}");
    }

    #[test]
    fn third_person_stops_short_of_terrain_and_never_behind_the_eye() {
        let eye = Vec3::ZERO;
        let at = third_person_position(eye, 0.0, 0.0, Some(2.0 * M));
        assert!(close(at, Vec3::new(0.0, 0.0, 1.75 * M)), "0.25 m short of a hit at 2 m: {at:?}");
        let at = third_person_position(eye, 0.0, 0.0, Some(0.1 * M));
        assert!(close(at, eye), "a hit inside the margin: at the eye");
        let at = third_person_position(eye, 0.0, 0.0, Some(10.0 * M));
        assert!(close(at, Vec3::new(0.0, 0.0, 4.0 * M)), "a hit beyond the distance changes nothing");
    }

    #[test]
    fn a_camera_pulled_into_the_head_hides_the_body() {
        let eye = Vec3::ZERO;
        let clear = third_person_position(eye, 0.0, 0.0, None);
        assert!(!third_person_hides_body(eye, clear), "4 m back: the body shows");
        let near_wall = third_person_position(eye, 0.0, 0.0, Some(0.6 * M));
        assert!(third_person_hides_body(eye, near_wall), "0.35 m back: inside the head");
        let wall = third_person_position(eye, 0.0, 0.0, Some(0.8 * M));
        assert!(!third_person_hides_body(eye, wall), "0.55 m back: still outside");

        let mut fp = FirstPerson { on: true, view: BodyView::Third, ..default() };
        assert_eq!(fp.view_for_body(), Some(BodyView::Third));
        fp.camera_in_body = true;
        assert_eq!(fp.view_for_body(), Some(BodyView::First), "hidden as in first person");
        fp.view = BodyView::First;
        fp.camera_in_body = false;
        assert_eq!(fp.view_for_body(), Some(BodyView::First));
        fp.on = false;
        assert_eq!(fp.view_for_body(), None);
    }

    #[test]
    fn the_terrain_hit_is_the_distance_to_the_first_solid_block_behind() {
        let size = game::BLOCK_VOXELS as f32;
        // The eye in the middle of block (0, 5, 0); a wall at block z = 2.
        let eye = Vec3::new(0.5, 5.5, 0.5) * size;
        let hit = third_person_hit(eye, Vec3::Z, |p| p[2] >= 2).expect("the wall");
        assert!((hit - 1.5 * size).abs() < 1e-3, "{hit}");
        assert_eq!(third_person_hit(eye, Vec3::Z, |p| p[2] >= 50), None, "beyond the reach");
        assert_eq!(third_person_hit(eye, Vec3::Z, |p| p[2] == 0), Some(0.0), "inside a block: at the eye");
    }

    #[test]
    fn no_eyes_while_the_frontend_has_the_screen() {
        use crate::session::{AppState, InPlay, SessionPlugin};
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin, SessionPlugin { skip_menu: false, login_first: false }))
            .init_resource::<TileMirror>()
            .insert_resource(FirstPerson { on: true, ..default() })
            .add_systems(Update, sync_first_person_camera);
        let region = game::RegionCoords::new(0, 0);
        app.world_mut().spawn((
            LocalBody,
            SimEntity { region, key: game::EntityKey::default() },
            SimTarget::body(Vec3::ZERO, Quat::IDENTITY),
        ));
        let eyes = |app: &mut App| app.world_mut().query::<&Eyes>().iter(app.world()).count();
        app.update();
        app.update();
        assert_eq!(eyes(&mut app), 0, "menu: the frontend has the screen");
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::InWorld);
        app.update();
        app.update();
        assert_eq!(eyes(&mut app), 0, "in the world but not in play: still the frontend's");
        app.world_mut().resource_mut::<InPlay>().0 = true;
        app.update();
        app.update();
        assert_eq!(eyes(&mut app), 1, "in play: the eyes have the screen");
        app.world_mut().resource_mut::<InPlay>().0 = false;
        app.update();
        app.update();
        assert_eq!(eyes(&mut app), 0, "the frontend takes the screen back: the eyes go");
    }
}
