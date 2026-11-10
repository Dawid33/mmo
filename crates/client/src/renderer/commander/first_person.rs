//! First person in Adventure: [`FIRST_PERSON_KEY`] puts the view in the adventurer's eyes and hands the
//! keyboard and mouse to the body, Minecraft style — WASD walk, Space jumps,
//! Ctrl sprints, the mouse looks, left / right click break / place.
//!
//! Client-local view state over the existing first-person sim path: the sim
//! drives the body from the keyboard once the client sends
//! `InputEvent::FpsCam(true)` (which also ends any walk, `tick_walkers_safe`),
//! `input::forward_input` forwards keys / mouse / clicks while [`FirstPerson`]
//! is on, and the local body gets the render camera ([`sync_first_person_camera`],
//! re-attached to whichever body is home, so a seam crossing keeps it). The
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

/// The render camera on the home region's local body while first person is
/// on, and nowhere on it otherwise (the commander camera stands aside in
/// `yield_commander_camera`; `bridge::enforce_single_active_camera` hands the
/// body camera the `UiCamera` marker). Idempotent, so a body re-created by a
/// seam handoff gets the eyes as soon as it is home.
pub fn sync_first_person_camera(
    mut commands: Commands,
    fp: Res<FirstPerson>,
    mirror: Res<TileMirror>,
    body_scale: Res<crate::renderer::stud::ClientBodyScale>,
    bodies: Query<(Entity, &SimEntity, &SimTarget, Has<Camera3d>), With<LocalBody>>,
) {
    let home = mirror.region();
    for (e, sim, target, has_camera) in &bodies {
        match (fp.on && Some(sim.region) == home, has_camera) {
            (true, false) => {
                commands.entity(e).insert((
                    Camera3d::default(),
                    Projection::Perspective(PerspectiveProjection {
                        fov: FIRST_PERSON_FOV_Y_DEG.to_radians(),
                        near: FIRST_PERSON_NEAR_M * UNITS_PER_METER,
                        far: game::lod::LOD_HORIZON_UNITS,
                        ..default()
                    }),
                    SimTarget::camera_eyed(target.pos, target.rot, body_scale.0.eye_offset()),
                ));
            }
            (false, true) => {
                commands
                    .entity(e)
                    .remove::<(Camera3d, Camera, Projection, UiCamera)>()
                    .insert(SimTarget::body(target.pos, target.rot));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_person_only_in_adventure() {
        assert!(wanted(false, true, true), "the button enters");
        assert!(!wanted(true, true, true), "and leaves");
        assert!(wanted(true, false, true), "stays on");
        assert!(!wanted(true, false, false), "leaving Adventure leaves first person");
        assert!(!wanted(false, true, false), "no first person in Fortress");
    }
}
