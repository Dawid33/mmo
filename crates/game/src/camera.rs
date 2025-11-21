use std::{fmt::Debug, ops::DerefMut, sync::Arc, time::Instant};

use crate::data::Camera;
use crate::rapier::math::Vector;
use crate::rapier::prelude::RigidBodyHandle;
use borrow::PartialHelper;
use crossbeam::channel::Sender;
#[allow(unused)]
use log::info;
use na::{
    clamp, AbstractRotation, ComplexField, Matrix4, Perspective3, Quaternion, UnitQuaternion,
    Vector2, Vector3, Vector4,
};
use parley::swash::shape::Direction;
use simba::scalar::{SubsetOf, SupersetOf};
use winit::event::MouseButton;

use crate::{
    data::Undo, transaction::GameDataTransaction, ClientPacket, ClientUpdateEvent, Controller,
    GameData, GameDataUpdate,
};

pub const ASPECT: f32 = (16 / 9) as f32;

impl Camera {
    pub fn new(handle: RigidBodyHandle) -> Self {
        let m = Matrix4::from_columns(&[
            Vector4::new(1.0, 0.0, 0.0, 0.0),
            Vector4::new(0.0, 1.0, 0.0, 0.0),
            Vector4::new(0.0, 0.0, 0.5, 0.0),
            Vector4::new(0.0, 0.0, 0.5, 1.0),
        ]);
        Camera {
            proj_matrix: Perspective3::from_matrix_unchecked(
                Perspective3::new(ASPECT, 90.0, 0.1, 100.0).as_matrix() * m,
            ),
            opengl_to_wgpu_matrix: m,
            view_matrix: Some(handle),
        }
    }
}

impl Camera {
    pub fn build_projection(&self) -> Matrix4<f32> {
        *self.proj_matrix.as_matrix()
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            opengl_to_wgpu_matrix: Default::default(),
            proj_matrix: Perspective3::new(ASPECT, 90.0, 0.1, 100.0),
            view_matrix: Default::default(),
        }
    }
}

pub struct CameraController {}

impl Controller for CameraController {
    fn on_tick<'a>(&mut self, data: &mut Undo<GameData>) {
        let data = data.as_refs_mut();
        for (p, e_id) in data.players.iter() {
            let e_id = *e_id;
            let ecs = data.ecs.as_refs_mut();
            let p = ecs.player.get_mut(e_id);
            // if let Some(resolution) = p.input.window_resized() {
            //     let old = ecs.camera.get(e_id).proj_matrix.clone();
            //     ecs.camera.undo(move |d, s| {
            //         s.send(GameDataUpdate::new(
            //             crate::GameDataTransactionKind::Undo,
            //             crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, old),
            //         ));
            //         d.get_mut(e_id).proj_matrix = old;
            //     });
            //     ecs.camera
            //         .get_mut(e_id)
            //         .proj_matrix
            //         .set_aspect(resolution.width as f32 / resolution.height as f32);
            //     let m = ecs.camera.get_mut(e_id).proj_matrix.clone();
            //     ecs.camera.send(GameDataUpdate::new(
            //         crate::GameDataTransactionKind::Do,
            //         crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, m),
            //     ));
            // }

            if !p.fps_cam_mode {
                continue;
            }

            let handle = ecs.rigidbody.get(e_id).clone();
            let b = data.physics.bodies.get_mut(handle).unwrap();
            let rotation = b.rotation();
            let mut linvel = Vector::zeros();
            const SPEED: f32 = 5.0;

            if p.input.key_held(&winit::keyboard::KeyCode::KeyW) {
                linvel.z = -0.1 * SPEED
            }
            if p.input.key_held(&winit::keyboard::KeyCode::KeyS) {
                linvel.z = 0.1 * SPEED
            }
            if p.input.key_held(&winit::keyboard::KeyCode::KeyA) {
                linvel.x = -0.1 * SPEED
            }
            if p.input.key_held(&winit::keyboard::KeyCode::KeyD) {
                linvel.x = 0.1 * SPEED
            }
            let mut linvel = rotation.transform_vector(&linvel);

            if p.input.key_held(&winit::keyboard::KeyCode::Space) {
                linvel.y = 0.1 * SPEED
            }
            if p.input.key_held(&winit::keyboard::KeyCode::ControlLeft) {
                linvel.y = -0.1 * SPEED
            }
            if linvel != Vector3::zeros() {
                let t = b.translation().clone();
                let old = b.next_position().translation.clone().vector;
                data.physics.bodies.undo(move |d, _| {
                    d.get_mut(handle)
                        .unwrap()
                        .set_next_kinematic_translation(old);
                });
                data.physics
                    .bodies
                    .get_mut(handle)
                    .unwrap()
                    .set_next_kinematic_translation(t + linvel);
            }

            let b = data.physics.bodies.get_mut(handle).unwrap();
            let rotation = b.next_position().rotation.clone();
            let mut r = b.rotation().clone();
            let diff = p.input.mouse_diff();
            if diff.0.abs() > 15.0 {
                if !diff.0.is_nan() {
                    r = UnitQuaternion::from_axis_angle(
                        &Vector3::y_axis(),
                        -clamp(diff.0 as f32, -0.09, 0.09),
                    ) * rotation;
                }
            }
            if diff.1.abs() > 15.0 {
                if !diff.1.is_nan() {
                    r = r * UnitQuaternion::from_axis_angle(
                        &Vector3::x_axis(),
                        -clamp(diff.1 as f32, -0.09, 0.09),
                    );
                }
            }

            if rotation != r {
                data.physics.bodies.undo(move |d, _| {
                    d.get_mut(handle)
                        .unwrap()
                        .set_next_kinematic_rotation(rotation.clone());
                });
                data.physics
                    .bodies
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
