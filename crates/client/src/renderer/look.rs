//! Render-side mouse look: the camera turns every frame, not every sim tick.
//!
//! Look is sim state (`Camera::yaw`/`pitch`), but the sim only folds the mouse
//! in once per tick, after `forward_input` has coalesced it — so a camera that
//! snaps to the sim look turns in `TICK_RATE`-sized steps. Here the client
//! integrates the same raw deltas with the same `game::apply_look` each frame
//! and renders that. The sim still gets every delta through `forward_input`
//! (edits, movement direction and the server all use the sim look), so this is
//! purely a view of where that look is about to be.
//!
//! The sim look only changes through input, so the two agree once the input
//! pipeline drains. After the mouse has been still for [`LOOK_RESYNC_IDLE`]
//! the local look adopts the sim's, which absorbs a spawn/teleport orientation
//! and the pitch-clamp difference of per-frame vs per-tick summing.

use std::time::Duration;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::bridge::SimTarget;

/// Mouse idle time after which the local look re-syncs to the sim's. Must
/// exceed the input pipeline's latency (coalescing window + client tick lead +
/// one tick), or it would pull the view back to a look the sim has not yet
/// caught up to.
pub const LOOK_RESYNC_IDLE: Duration = Duration::from_millis(250);

#[derive(Resource, Default, Debug)]
pub struct LocalLook {
    /// (yaw, pitch) in radians; `None` until seeded from the sim camera.
    look: Option<(f32, f32)>,
    /// Time since the last non-zero mouse delta the sim will act on.
    idle: Duration,
}

/// Fold this frame's mouse motion into the local look, under the gate the
/// sim applies it with: the cursor grabbed (fps-cam mode). A released cursor
/// (menus, overlays) turns neither the sim camera nor this one.
pub fn integrate_local_look(
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    cursor: Query<(&Window, &CursorOptions), With<PrimaryWindow>>,
    mut look: ResMut<LocalLook>,
) {
    // Same gate as `input::forward_input`: raw motion from an unfocused window
    // must not turn this camera either.
    let grabbed = cursor
        .single()
        .is_ok_and(|(w, c)| w.focused && c.grab_mode == CursorGrabMode::Locked);
    let delta = if grabbed { motion.delta } else { Vec2::ZERO };
    if delta == Vec2::ZERO {
        look.idle = look.idle.saturating_add(time.delta());
        return;
    }
    look.idle = Duration::ZERO;
    if let Some((yaw, pitch)) = look.look {
        look.look = Some(game::apply_look(yaw, pitch, delta.x, delta.y));
    }
}

/// Write the local look onto the local player's camera. Runs after
/// `interpolate_transforms`, which snaps the rotation to the (per-tick) sim
/// look; this replaces it. The camera is the one entity with both `Camera3d`
/// and a `SimTarget` (other players' cameras never get `Camera3d`).
pub fn apply_local_look(
    mut look: ResMut<LocalLook>,
    mut camera: Query<(&mut Transform, &SimTarget), With<Camera3d>>,
) {
    let Ok((mut tf, target)) = camera.single_mut() else {
        look.look = None;
        return;
    };
    if look.look.is_none() || look.idle >= LOOK_RESYNC_IDLE {
        let (yaw, pitch, _roll) = target.rot.to_euler(EulerRot::YXZ);
        look.look = Some((yaw, pitch));
    }
    let (yaw, pitch) = look.look.expect("seeded above");
    tf.rotation = look_rotation(yaw, pitch);
}

/// Bevy twin of `game::look_quat`: yaw about world-Y, then pitch about local-X.
pub fn look_rotation(yaw: f32, pitch: f32) -> Quat {
    Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_rotation_matches_sim_quat() {
        for &(yaw, pitch) in &[(0.0f32, 0.0f32), (1.3, -0.4), (-2.9, 1.2), (5.0, 0.7)] {
            let sim = Quat::from_array(game::look_quat(yaw, pitch).to_array());
            let bevy = look_rotation(yaw, pitch);
            // Components, not `angle_between`: that is 2·acos(|dot|), and
            // `dot` of two bit-identical quats can round to 1 − 2⁻²⁴, which
            // reads as ~7e-4 rad ((5.0, 0.7) does). q and −q are the same
            // rotation.
            let (a, b) = (Vec4::from(bevy), Vec4::from(sim));
            let diff = (a - b).abs().max_element().min((a + b).abs().max_element());
            assert!(diff < 1e-6, "{yaw} {pitch}: {bevy:?} vs {sim:?}");
        }
    }

    #[test]
    fn euler_round_trip_recovers_yaw_pitch() {
        let (yaw, pitch) = (0.8f32, -0.3f32);
        let (y, p, r) = look_rotation(yaw, pitch).to_euler(EulerRot::YXZ);
        assert!((y - yaw).abs() < 1e-5 && (p - pitch).abs() < 1e-5 && r.abs() < 1e-5);
    }
}
