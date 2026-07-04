
#[allow(unused)]
use log::info;
use na::{clamp, Matrix4, Perspective3, UnitQuaternion, Vector3, Vector4};
use ordered_float::OrderedFloat;
use parry3d::math::Real;
use rapier3d::math::Vector;
use rapier3d::prelude::RigidBodyHandle;

use rapier3d::control::KinematicCharacterController;
use rapier3d::prelude::QueryFilter;

use crate::input::Key;
use crate::{Controller, GameData, GameDataUpdate, Undo};

pub const ASPECT: f32 = 16.0 / 9.0;
/// Vertical field of view in radians (Perspective3 expects radians, not degrees).
pub const FOV_Y: f32 = std::f32::consts::FRAC_PI_3; // 60°

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Camera {
    pub opengl_to_wgpu_matrix: Matrix4<Real>,
    pub proj_matrix: Perspective3<Real>,
    pub view_matrix: Option<RigidBodyHandle>,
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
                    OrderedFloat(0.1),
                    OrderedFloat(100.0),
                )
                .as_matrix()
                    * m,
            ),
            opengl_to_wgpu_matrix: m,
            view_matrix: Some(handle),
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
                OrderedFloat(0.1),
                OrderedFloat(100.0),
            ),
            view_matrix: Default::default(),
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
            const SPEED: Real = OrderedFloat(5.0);

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
