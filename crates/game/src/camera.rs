use std::{fmt::Debug, ops::DerefMut, sync::Arc, time::Instant};

use assert_json_diff::{CompareMode, Config};
use borrow::PartialHelper;
use crossbeam::channel::Sender;
#[allow(unused)]
use log::info;
use na::{
    clamp, AbstractRotation, ComplexField, Matrix4, Perspective3, Quaternion, UnitQuaternion,
    Vector2, Vector3, Vector4,
};
use ordered_float::OrderedFloat;
use parley::swash::shape::Direction;
use parry3d::math::Real;
use rapier3d::math::Vector;
use rapier3d::prelude::RigidBodyHandle;
use rollback::Camera;

use crate::{ClientPacket, ClientUpdateEvent, Controller, GameData, GameDataUpdate};
use rollback::Undo;

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

            if client.input.key_held(&rollback::common::Key::KeyW) {
                linvel.z = Real::from(-0.1) * SPEED
            }
            if client.input.key_held(&rollback::common::Key::KeyS) {
                linvel.z = Real::from(0.1) * SPEED
            }
            if client.input.key_held(&rollback::common::Key::KeyA) {
                linvel.x = Real::from(-0.1) * SPEED
            }
            if client.input.key_held(&rollback::common::Key::KeyD) {
                linvel.x = Real::from(0.1) * SPEED
            }
            let mut linvel = rotation.transform_vector(&linvel);
            linvel.y = Real::from(0.0);

            if client.input.key_held(&rollback::common::Key::Space) {
                linvel.y = Real::from(0.1) * SPEED
            }
            if client.input.key_held(&rollback::common::Key::ControlLeft) {
                linvel.y = Real::from(-0.1) * SPEED
            }
            if linvel != Vector3::zeros() {
                let t = b.translation().clone();
                // change(): whole-set snapshot. Surgical field restores are
                // NOT exact here — set_next_kinematic_* also wakes the body
                // and marks it modified (hashed state the closure can't
                // restore); the snapshot covers all of it.
                let bodies = data.physics.bodies.change();
                bodies
                    .get_mut(handle)
                    .unwrap()
                    .set_next_kinematic_translation(t + linvel);
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
