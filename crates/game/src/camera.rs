
#[allow(unused)]
use log::info;
use na::{clamp, Matrix4, Perspective3, UnitQuaternion, Vector3, Vector4};
use ordered_float::OrderedFloat;
use parry3d::math::Real;
use rapier3d::math::Vector;
use rapier3d::prelude::RigidBodyHandle;
use std::f32::consts::FRAC_PI_2;

use rapier3d::control::KinematicCharacterController;
use rapier3d::prelude::QueryFilter;

use crate::input::Key;
use crate::{Controller, GameData, GameDataUpdate, Undo};

pub const ASPECT: f32 = 16.0 / 9.0;
/// Vertical field of view in radians (Perspective3 expects radians, not degrees).
pub const FOV_Y: f32 = std::f32::consts::FRAC_PI_3; // 60°

/// Max pitch magnitude (radians), a hair under straight up/down to dodge
/// the gimbal singularity at the poles.
pub const PITCH_LIMIT: Real = OrderedFloat(FRAC_PI_2 - 0.001);
/// Minecraft-style look sensitivity in [0, 1]; tune to taste.
pub const LOOK_SENSITIVITY: f32 = 0.5;
/// Radians of yaw per pixel of mouse motion, before the sensitivity curve.
pub const LOOK_BASE_X: f32 = 0.0026;
/// Radians of pitch per pixel of mouse motion, before the sensitivity curve.
pub const LOOK_BASE_Y: f32 = 0.0026;

/// Minecraft cubic sensitivity: fine control near center, fast at the ends.
/// `f = sens*0.6 + 0.2; mult = f^3 * 8`, then `px * base * mult`.
pub fn look_delta_rad(px: f32, base: f32) -> f32 {
    let f = LOOK_SENSITIVITY * 0.6 + 0.2;
    let mult = f * f * f * 8.0;
    px * base * mult
}

/// Accumulate a mouse delta into (yaw, pitch). Yaw wraps freely; pitch is
/// clamped to +/-PITCH_LIMIT. Non-finite deltas are ignored so a stray NaN
/// cannot corrupt the look. Yaw: rightward mouse decreases the angle
/// (matches the previous controller's convention). Pitch: positive `dy`
/// increases the angle toward `+PITCH_LIMIT`.
pub fn apply_look(yaw: Real, pitch: Real, dx: f32, dy: f32) -> (Real, Real) {
    let dx = if dx.is_finite() { dx } else { 0.0 };
    let dy = if dy.is_finite() { dy } else { 0.0 };
    let new_yaw = yaw - Real::from(look_delta_rad(dx, LOOK_BASE_X));
    let new_pitch = clamp(
        pitch + Real::from(look_delta_rad(dy, LOOK_BASE_Y)),
        -PITCH_LIMIT,
        PITCH_LIMIT,
    );
    (new_yaw, new_pitch)
}

/// World-Y yaw only — the horizontal facing used for movement direction.
pub fn yaw_quat(yaw: Real) -> UnitQuaternion<Real> {
    UnitQuaternion::from_axis_angle(&Vector3::y_axis(), yaw)
}

/// Full look orientation: yaw about world-Y, then pitch about local-X.
/// Roll is structurally impossible (the X axis is invariant under its own
/// rotation, so the camera-right vector stays horizontal).
pub fn look_quat(yaw: Real, pitch: Real) -> UnitQuaternion<Real> {
    yaw_quat(yaw) * UnitQuaternion::from_axis_angle(&Vector3::x_axis(), pitch)
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Camera {
    pub opengl_to_wgpu_matrix: Matrix4<Real>,
    pub proj_matrix: Perspective3<Real>,
    pub view_matrix: Option<RigidBodyHandle>,
    pub yaw: Real,
    pub pitch: Real,
}

impl Camera {
    pub fn new(handle: RigidBodyHandle) -> Self {
        let m = Matrix4::from_columns(&[
            Vector4::new(
                OrderedFloat(1.0),
                OrderedFloat(0.0),
                OrderedFloat(0.0),
                OrderedFloat(0.0),
            ),
            Vector4::new(
                OrderedFloat(0.0),
                OrderedFloat(1.0),
                OrderedFloat(0.0),
                OrderedFloat(0.0),
            ),
            Vector4::new(
                OrderedFloat(0.0),
                OrderedFloat(0.0),
                OrderedFloat(0.5),
                OrderedFloat(0.0),
            ),
            Vector4::new(
                OrderedFloat(0.0),
                OrderedFloat(0.0),
                OrderedFloat(0.5),
                OrderedFloat(1.0),
            ),
        ]);
        Camera {
            proj_matrix: Perspective3::from_matrix_unchecked(
                Perspective3::new(
                    OrderedFloat(ASPECT),
                    OrderedFloat(FOV_Y),
                    // 1 unit = 1/16 m: near 6 cm, far 125 m.
                    OrderedFloat(1.0),
                    OrderedFloat(2000.0),
                )
                .as_matrix()
                    * m,
            ),
            opengl_to_wgpu_matrix: m,
            view_matrix: Some(handle),
            yaw: OrderedFloat(0.0),
            pitch: OrderedFloat(0.0),
        }
    }
}

impl Camera {
    pub fn build_projection(&self) -> Matrix4<Real> {
        *self.proj_matrix.as_matrix()
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            opengl_to_wgpu_matrix: Default::default(),
            proj_matrix: Perspective3::new(
                OrderedFloat(ASPECT),
                OrderedFloat(FOV_Y),
                // 1 unit = 1/16 m: near 6 cm, far 125 m.
                OrderedFloat(1.0),
                OrderedFloat(2000.0),
            ),
            view_matrix: Default::default(),
            yaw: OrderedFloat(0.0),
            pitch: OrderedFloat(0.0),
        }
    }
}

pub struct CameraController {}

impl Controller for CameraController {
    fn on_tick<'a>(&mut self, data: &mut Undo<GameData>) {
        let data = data.as_refs_mut();
        for (client_id, e_id) in data.player_entites.iter() {
            let e_id = *e_id;
            let ecs = data.ecs.as_refs_mut();
            let client = data.clients.get_mut(client_id).unwrap();
            if let Some((width, height)) = *client.input.window_resized() {
                let old = ecs.camera.get(e_id).proj_matrix.clone();
                ecs.camera.emit_on_undo(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Undo,
                    crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, old.clone()),
                ));
                let mut scope = ecs.camera.undo_scope();
                scope
                    .get_mut(e_id)
                    .proj_matrix
                    .set_aspect(OrderedFloat(width as f32 / height as f32));
                let m = scope.get_mut(e_id).proj_matrix.clone();
                scope.register(move |d, _| d.get_mut(e_id).proj_matrix = old);
                ecs.camera.send(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, m),
                ));
            }

            if !client.fps_cam_mode {
                continue;
            }

            let handle = ecs.rigidbody.get(e_id).clone();
            let b = data.physics.bodies.get(handle).unwrap();
            let rotation = b.rotation();
            let mut linvel = Vector::zeros();
            // 0.1 * 80 = 8 units/tick = 0.5 m/tick = 10 m/s at 1/16 m units.
            const SPEED: Real = OrderedFloat(80.0);

            if client.input.key_held(&Key::KeyW) {
                linvel.z = Real::from(-0.1) * SPEED
            }
            if client.input.key_held(&Key::KeyS) {
                linvel.z = Real::from(0.1) * SPEED
            }
            if client.input.key_held(&Key::KeyA) {
                linvel.x = Real::from(-0.1) * SPEED
            }
            if client.input.key_held(&Key::KeyD) {
                linvel.x = Real::from(0.1) * SPEED
            }
            let mut linvel = rotation.transform_vector(&linvel);
            linvel.y = Real::from(0.0);

            if client.input.key_held(&Key::Space) {
                linvel.y = Real::from(0.1) * SPEED
            }
            if client.input.key_held(&Key::ControlLeft) {
                linvel.y = Real::from(-0.1) * SPEED
            }
            if linvel != Vector3::zeros() {
                let t = b.translation().clone();
                // Collision-corrected movement: slide along / stop at terrain
                // instead of teleporting through it. Queries are read-only;
                // the sole write below stays under the change() snapshot.
                let corrected = match b.colliders().first().copied() {
                    Some(collider_handle) => {
                        let collider = data.physics.colliders.get(collider_handle).unwrap();
                        let queries = data.physics.broad_phase.as_query_pipeline(
                            data.physics.narrow_phase.query_dispatcher(),
                            &data.physics.bodies,
                            &data.physics.colliders,
                            QueryFilter::default().exclude_rigid_body(handle),
                        );
                        let controller = KinematicCharacterController {
                            // Pure fly movement: no downward snap while
                            // skimming the ground.
                            snap_to_ground: None,
                            ..Default::default()
                        };
                        controller
                            .move_shape(
                                Real::from(1.0),
                                &queries,
                                collider.shape(),
                                collider.position(),
                                linvel,
                                |_| {},
                            )
                            .translation
                    }
                    // No collider on the mover: keep the uncorrected motion.
                    None => linvel,
                };
                if corrected != Vector3::zeros() {
                    // change(): whole-set snapshot. Surgical field restores are
                    // NOT exact here — set_next_kinematic_* also wakes the body
                    // and marks it modified (hashed state the closure can't
                    // restore); the snapshot covers all of it.
                    let bodies = data.physics.bodies.change();
                    bodies
                        .get_mut(handle)
                        .unwrap()
                        .set_next_kinematic_translation(t + corrected);
                }
            }

            let b = data.physics.bodies.get(handle).unwrap();
            let rotation = b.next_position().rotation.clone();
            let mut r = b.rotation().clone();
            let diff = client.input.mouse_diff();
            const SENSITIVITIY: f32 = 0.001;
            if diff.0.abs() > 2.0 {
                if !diff.0.is_nan() {
                    r = UnitQuaternion::from_axis_angle(
                        &Vector3::y_axis(),
                        -clamp(
                            Real::from(diff.0 as f32 * (0.00136 + SENSITIVITIY)),
                            Real::from(-0.2),
                            Real::from(0.2),
                        ),
                    ) * rotation;
                }
            }
            if diff.1.abs() > 2.0 {
                if !diff.1.is_nan() {
                    r = r * UnitQuaternion::from_axis_angle(
                        &Vector3::x_axis(),
                        -clamp(
                            Real::from(diff.1 as f32 * (0.0008 + SENSITIVITIY)),
                            Real::from(-0.2),
                            Real::from(0.2),
                        ),
                    );
                }
            }

            if rotation != r {
                // Same snapshot rationale as the translation site above.
                let bodies = data.physics.bodies.change();
                bodies
                    .get_mut(handle)
                    .unwrap()
                    .set_next_kinematic_rotation(r);
            }
        }
    }
}

impl CameraController {
    pub fn new() -> Box<dyn Controller> {
        Box::new(Self {})
    }
}
