//! First person in Adventure: [`FIRST_PERSON_KEY`] puts the view in the adventurer's eyes and hands the
//! keyboard and mouse to the body, Minecraft style — WASD walk, Space jumps,
//! Ctrl sprints, the mouse looks, left / right click break / place.
//!
//! Client-local view state over the existing first-person sim path: the sim
//! drives the body from the keyboard once the client sends
//! `InputEvent::FpsCam(true)` (which also ends any walk, `tick_walkers_safe`),
//! `input::forward_input` forwards keys / mouse / clicks while [`FirstPerson`]
//! is on, and the view is the [`Eyes`] camera ([`sync_first_person_camera`]: one
//! entity for the session, following whichever body is home, so a seam crossing
//! neither drops nor re-creates it). The
//! commander camera, its input systems ([`inactive`]) and the body's own model
//! stand aside; the cut and the dug-space x-ray are switched off (they would
//! clip the view) and put back on the way out, and the commander camera comes
//! back over the body. Esc opens the menu and frees the pointer, so the panel
//! button works from there too. Leaving Adventure leaves first person.

use bevy::prelude::*;
use game::movement::UNITS_PER_METER;
use game::InputEvent;

use super::adventure::ClientPlayMode;
use super::level::WorkingLevel;
use super::tiles::TileMirror;
use super::CommanderCamera;
use crate::renderer::bridge::{LocalBody, SimEntity, SimTarget};
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::voxel_pull::xray::XraySettings;
use crate::renderer::{GameEvents, LocalPlayer, UiCamera};

/// Toggles first person (Minecraft's perspective key).
pub const FIRST_PERSON_KEY: KeyCode = KeyCode::F5;
/// Vertical field of view of the eyes.
pub const FIRST_PERSON_FOV_Y_DEG: f32 = 70.0;
/// Near clip plane of the eyes, metres.
pub const FIRST_PERSON_NEAR_M: f32 = 0.05;

/// Whether the view is in the local body's eyes, and what it switched off
/// to get there.
#[derive(Resource, Default, Debug)]
pub struct FirstPerson {
    pub on: bool,
    /// The cut and the x-ray switch as they were on entry.
    saved: Option<(bool, bool)>,
}

/// Toggle first person (the F9 panel's button that wrote it is gone; F5
/// remains).
#[derive(Message, Debug, Clone, Copy)]
pub struct ToggleFirstPersonRequest;

/// Run condition: the commander's own input (pan, zoom, levels, tools,
/// selection, move orders) — off while the body has the keyboard.
pub fn inactive(fp: Option<Res<FirstPerson>>) -> bool {
    fp.is_none_or(|fp| !fp.on)
}

/// Whether first person should be on after this frame's requests: a toggle
/// flips it, and it only exists in Adventure.
pub fn wanted(on: bool, toggled: bool, adventure: bool) -> bool {
    adventure && (on != toggled)
}

/// Button / key → enter or leave first person; leaving Adventure leaves it.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn toggle_first_person(
    mut requests: MessageReader<ToggleFirstPersonRequest>,
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<ClientPlayMode>,
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
    let pressed = requests.read().count() > 0 || (keys.just_pressed(FIRST_PERSON_KEY) && !policy.blocks_game());
    let want = wanted(fp.on, pressed, mode.adventure());
    if want == fp.on {
        return;
    }
    let Some(client) = local.0 else { return };
    fp.on = want;
    // The key is the player asking for the eyes: the pointer comes with them.
    // An entry nobody pressed for (Adventure starting) leaves it free until a
    // click in the window (`cursor::retake_pointer`).
    policy.released = want && !keys.just_pressed(FIRST_PERSON_KEY);
    let _ = events.0.send(game::GameEventKind::PlayerInput(client, InputEvent::FpsCam(want)));
    if want {
        // The eyes replace the top-down view: back to the 3D rig first, so
        // leaving first person returns to it.
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

/// The [`Eyes`] while first person is on, and none otherwise (the commander
/// camera stands aside in `yield_commander_camera`). Each frame their
/// `SimTarget` takes the home body's pose in world space, and
/// `interpolate_transforms` / `look::apply_local_look` move them like any
/// local camera; with no body (a crossing's one-tick gap) they stay put.
pub fn sync_first_person_camera(
    mut commands: Commands,
    fp: Res<FirstPerson>,
    mirror: Res<TileMirror>,
    bodies: Query<(&SimEntity, &SimTarget), (With<LocalBody>, Without<Eyes>)>,
    mut eyes: Query<(Entity, &mut SimTarget), With<Eyes>>,
) {
    if !fp.on {
        for (e, _) in &eyes {
            commands.entity(e).despawn();
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
    fn first_person_only_in_adventure() {
        assert!(wanted(false, true, true), "the button enters");
        assert!(!wanted(true, true, true), "and leaves");
        assert!(wanted(true, false, true), "stays on");
        assert!(!wanted(true, false, false), "leaving Adventure leaves first person");
        assert!(!wanted(false, true, false), "no first person in Fortress");
    }
}
