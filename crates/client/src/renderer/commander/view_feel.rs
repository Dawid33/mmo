//! How the body view feels in motion, Minecraft style: the view bobs with
//! each step while the body strides, and the field of view widens while it
//! sprints.
//!
//! Render-only: aim comes from the look the client sends the sim (never from
//! the camera's `Transform`), so neither effect moves the targeted block.
//!
//! [`apply_view_feel`] runs after the look rotation is written
//! (`look::apply_local_look`) and after `motion_probe` measured the eye, and
//! before the third-person camera leaves the eye. Like the third-person pull, what it
//! adds is taken off again before `interpolate_transforms`
//! ([`restore_view_feel`]), so the glide always works on the eye itself.
//!
//! - **Bob** (first person only, Settings → Graphics "View bobbing"): its
//!   strength eases toward the horizontal speed (full at
//!   [`VIEW_BOB_FULL_SPEED_MPS`]) while the body's own gait clip is a walk or
//!   a run (`game::anim::gait`: never airborne, never standing), else toward
//!   zero. The phase runs with the distance walked: one dip per
//!   [`VIEW_BOB_STEP_M`]. Minecraft's `bobView` shape at 40 % of its size
//!   (its full size read as exaggerated).
//! - **Sprint FOV** ("Sprint FOV"): the sprint input (held Ctrl or the
//!   double-tapped W, [`SprintInput`]) while the body moves at least
//!   [`SPRINT_FOV_MIN_SPEED_MPS`] eases the field of view to
//!   [`SPRINT_FOV_SCALE`] × the FOV setting; flying fast without sprint does
//!   not widen it.
//!
//! The speed is measured from the eye's own glide (world space, so a seam
//! changes nothing); a jump longer than [`VIEW_SPEED_SNAP_M`] in one frame is
//! a teleport and counts as standing still.

use std::f32::consts::PI;

use bevy::prelude::*;
use game::movement::UNITS_PER_METER;

use super::first_person::{BodyView, Eyes, FirstPerson};
use super::smooth;
use super::tiles::TileMirror;
use crate::graphics_settings::GraphicsSettings;
use crate::renderer::bridge::{LocalBody, SimEntity};
use crate::renderer::rigs::SimAnimator;

/// Metres walked per bob dip (Minecraft: `walkDist` grows 0.6 per block, a dip
/// per unit of it).
pub const VIEW_BOB_STEP_M: f32 = 1.0 / 0.6;
/// Horizontal speed (m/s) at and above which the bob is at full strength
/// (Minecraft caps it at 0.1 block per tick).
pub const VIEW_BOB_FULL_SPEED_MPS: f32 = 2.0;
/// Time constant (s) of the bob strength's ease (Minecraft: 0.4 of the gap
/// per 50 ms tick).
pub const VIEW_BOB_EASE_SECS: f32 = 0.1;
/// Full-strength sideways sway, metres either side.
pub const VIEW_BOB_SIDE_M: f32 = 0.02;
/// Full-strength dip, metres down at each step.
pub const VIEW_BOB_DIP_M: f32 = 0.04;
/// Full-strength roll, degrees either side.
pub const VIEW_BOB_ROLL_DEG: f32 = 0.12;
/// Full-strength nod, degrees down at each step.
pub const VIEW_BOB_NOD_DEG: f32 = 0.2;
/// Time constant (s) of the measured speed's smoothing.
pub const VIEW_SPEED_EASE_SECS: f32 = 0.1;
/// A one-frame eye jump longer than this (metres) is a teleport, not motion.
pub const VIEW_SPEED_SNAP_M: f32 = 4.0;
/// The field of view while sprinting, × the FOV setting (Minecraft's sprint:
/// (1.3 + 1) / 2).
pub const SPRINT_FOV_SCALE: f32 = 1.15;
/// Sprint held while moving slower than this (m/s) does not widen the view.
pub const SPRINT_FOV_MIN_SPEED_MPS: f32 = 1.0;
/// Time constant (s) of the FOV's ease (Minecraft: half the gap per tick).
pub const SPRINT_FOV_EASE_SECS: f32 = 0.08;

/// Whether the body's sprint input is on — Ctrl held or the W latch closed —
/// as `input::forward_input` last sent it (false while it sends the body nothing).
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SprintInput(pub bool);

/// Per-[`Eyes`] state of the bob and the sprint FOV.
#[derive(Component, Debug)]
pub struct ViewFeel {
    /// The eye (without this frame's effects) last frame, world space.
    last_eye: Option<Vec3>,
    /// Smoothed horizontal speed, m/s.
    speed_mps: f32,
    /// Distance walked, metres, wrapped to one bob period.
    walked_m: f32,
    /// Bob strength, 0..=1.
    bob: f32,
    /// The FOV's current multiple of the setting.
    fov_scale: f32,
    /// What [`apply_view_feel`] added this frame: translation and the tilt
    /// right-multiplied onto the rotation.
    applied: (Vec3, Quat),
}

impl Default for ViewFeel {
    fn default() -> Self {
        Self {
            last_eye: None,
            speed_mps: 0.0,
            walked_m: 0.0,
            bob: 0.0,
            fov_scale: 1.0,
            applied: (Vec3::ZERO, Quat::IDENTITY),
        }
    }
}

impl ViewFeel {
    /// This frame's bob as [`bob_at`] takes it — (phase, strength) — while the
    /// view bobs at all (the first-person arm bobs with it, `hand.rs`).
    pub fn bob_pose(&self) -> Option<(f32, f32)> {
        (self.bob > 1e-4).then(|| (PI * self.walked_m / VIEW_BOB_STEP_M, self.bob))
    }
}

/// Takes last frame's bob off the [`Eyes`] (before `interpolate_transforms`).
pub fn restore_view_feel(mut eyes: Query<(&mut Transform, &mut ViewFeel), With<Eyes>>) {
    for (mut tf, mut feel) in &mut eyes {
        let (offset, tilt) = std::mem::replace(&mut feel.applied, (Vec3::ZERO, Quat::IDENTITY));
        if offset != Vec3::ZERO || tilt != Quat::IDENTITY {
            tf.translation -= offset;
            tf.rotation = (tf.rotation * tilt.inverse()).normalize();
        }
    }
}

/// Whether the home body's gait clip is a walk or a run (or the only local
/// body's, while the home trails it by a frame).
fn striding<'a>(
    home: Option<game::RegionId>,
    bodies: impl Iterator<Item = (&'a SimEntity, &'a SimAnimator)>,
) -> bool {
    let mut only = None;
    let mut count = 0;
    for (sim, anim) in bodies {
        if Some(sim.region) == home {
            only = Some(anim);
            count = 1;
            break;
        }
        only = Some(anim);
        count += 1;
    }
    let Some(anim) = only.filter(|_| count == 1) else {
        return false;
    };
    let rig = game::anim::rig(anim.0.rig);
    let clip = anim.0.current.clip;
    clip == rig.walk || clip == rig.run
}

/// The bob's offset (view space, metres) and tilt at phase `f` and strength `b`.
pub fn bob_at(f: f32, b: f32) -> (Vec3, Quat) {
    let offset = Vec3::new(
        f.sin() * b * VIEW_BOB_SIDE_M,
        -(f.cos() * b).abs() * VIEW_BOB_DIP_M,
        0.0,
    );
    let roll = (f.sin() * b * VIEW_BOB_ROLL_DEG).to_radians();
    let nod = -((f - 0.2).cos() * b).abs() * VIEW_BOB_NOD_DEG.to_radians();
    (
        offset,
        Quat::from_rotation_z(roll) * Quat::from_rotation_x(nod),
    )
}

/// Measures the eye's speed, then bobs it and sets its field of view.
pub fn apply_view_feel(
    time: Res<Time>,
    fp: Res<FirstPerson>,
    settings: Res<GraphicsSettings>,
    sprint: Option<Res<SprintInput>>,
    mirror: Res<TileMirror>,
    bodies: Query<(&SimEntity, &SimAnimator), With<LocalBody>>,
    mut eyes: Query<(&mut Transform, &mut Projection, &mut ViewFeel), With<Eyes>>,
) {
    let dt = time.delta_secs();
    let striding = striding(mirror.region(), bodies.iter());
    let sprint = sprint.is_some_and(|s| s.0);
    let first = fp.body_view() == Some(BodyView::First);
    for (mut tf, mut projection, mut feel) in &mut eyes {
        let eye = tf.translation;
        let sample = match feel.last_eye {
            Some(last) if dt > 0.0 => {
                let moved = (eye - last).xz().length() / UNITS_PER_METER;
                if moved > VIEW_SPEED_SNAP_M {
                    0.0
                } else {
                    moved / dt
                }
            }
            _ => 0.0,
        };
        feel.last_eye = Some(eye);
        if dt <= 0.0 {
            continue;
        }
        feel.speed_mps = smooth(feel.speed_mps, sample, dt, VIEW_SPEED_EASE_SECS);

        // The bob.
        let target = if first && settings.view_bobbing && striding {
            (feel.speed_mps / VIEW_BOB_FULL_SPEED_MPS).min(1.0)
        } else {
            0.0
        };
        feel.bob = smooth(feel.bob, target, dt, VIEW_BOB_EASE_SECS);
        feel.walked_m = (feel.walked_m + feel.speed_mps * dt).rem_euclid(2.0 * VIEW_BOB_STEP_M);
        if feel.bob > 1e-4 {
            let (offset, tilt) = bob_at(PI * feel.walked_m / VIEW_BOB_STEP_M, feel.bob);
            let offset = tf.rotation * offset * UNITS_PER_METER;
            tf.translation += offset;
            tf.rotation = (tf.rotation * tilt).normalize();
            feel.applied = (offset, tilt);
        } else {
            feel.bob = 0.0;
        }

        // The field of view.
        let sprinting = settings.sprint_fov && sprint && feel.speed_mps >= SPRINT_FOV_MIN_SPEED_MPS;
        let target = if sprinting { SPRINT_FOV_SCALE } else { 1.0 };
        feel.fov_scale = smooth(feel.fov_scale, target, dt, SPRINT_FOV_EASE_SECS);
        if (feel.fov_scale - target).abs() < 1e-4 {
            feel.fov_scale = target;
        }
        let fov = (settings.fov_deg * feel.fov_scale).to_radians();
        // Written only on a change: the projection's change re-derives the frustum.
        if let Projection::Perspective(p) = &*projection {
            if p.fov != fov {
                if let Projection::Perspective(p) = &mut *projection {
                    p.fov = fov;
                }
            }
        }
    }
}
