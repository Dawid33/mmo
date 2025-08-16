use cgmath::{SquareMatrix, Zero};
use winit::{event::ElementState, keyboard::PhysicalKey};

use crate::{Controller, Transaction};

#[rustfmt::skip]
pub const OPENGL_TO_WGPU_MATRIX: cgmath::Matrix4<f32> = cgmath::Matrix4::from_cols(
    cgmath::Vector4::new(1.0, 0.0, 0.0, 0.0),
    cgmath::Vector4::new(0.0, 1.0, 0.0, 0.0),
    cgmath::Vector4::new(0.0, 0.0, 0.5, 0.0),
    cgmath::Vector4::new(0.0, 0.0, 0.5, 1.0),
);

#[repr(C)]
#[derive(
    Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable, serde::Serialize, serde::Deserialize,
)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Camera {
    pub eye: cgmath::Point3<f32>,
    pub target: cgmath::Point3<f32>,
    pub up: cgmath::Vector3<f32>,
    pub fovy: f32,
    pub znear: f32,
    pub zfar: f32,
    pub uniform: CameraUniform,
    pub velocity: cgmath::Vector3<f32>,
    pub position: cgmath::Vector3<f32>,
}

// TODO: Pass in aspect ration from user.
const ASPECT: f32 = (16 / 9) as f32;

impl Camera {
    pub fn new() -> Self {
        Camera {
            // position the camera 1 unit up and 2 units back
            // +z is out of the screen
            eye: (0.0, 1.0, 2.0).into(),
            // have it look at the origin
            target: (0.0, 0.0, 0.0).into(),
            // which way is "up"
            up: cgmath::Vector3::unit_y(),
            fovy: 45.0,
            znear: 0.1,
            zfar: 100.0,
            uniform: CameraUniform {
                view_proj: cgmath::Matrix4::identity().into(),
            },
            velocity: cgmath::Vector3::zero(),
            position: cgmath::Vector3::zero(),
        }
    }

    fn build_view_projection_matrix(&self) -> cgmath::Matrix4<f32> {
        let view = cgmath::Matrix4::look_at_rh(self.eye, self.target, self.up);
        let proj = cgmath::perspective(cgmath::Deg(self.fovy), ASPECT, self.znear, self.zfar);
        let position = cgmath::Matrix4::from_translation(self.position);

        return (OPENGL_TO_WGPU_MATRIX * proj * view) * position;
    }

    pub fn update_view_proj(&mut self) {
        self.uniform.view_proj = self.build_view_projection_matrix().into();
    }
}

pub struct CameraController {}

impl Controller for CameraController {
    fn on_tick<'a>(&mut self, t: &mut Transaction<'a>) {
        t.update_camera();
    }

    fn on_keyboard_event<'a>(
        &mut self,
        t: &mut Transaction<'a>,
        key: PhysicalKey,
        state: ElementState,
    ) {
        let cid = t.get_camera_id().unwrap();
        let mut v = t.get_camera(cid).velocity.clone();
        if state.is_pressed() {
            match key {
                PhysicalKey::Code(key_code) => match key_code {
                    winit::keyboard::KeyCode::KeyW => v.z = 0.1,
                    winit::keyboard::KeyCode::KeyS => v.z = -0.1,
                    winit::keyboard::KeyCode::KeyA => v.x = 0.1,
                    winit::keyboard::KeyCode::KeyD => v.x = -0.1,
                    winit::keyboard::KeyCode::Space => v.y = -0.1,
                    winit::keyboard::KeyCode::ControlLeft => v.y = 0.1,
                    _ => (),
                },
                _ => (),
            }
        } else {
            match key {
                PhysicalKey::Code(key_code) => match key_code {
                    winit::keyboard::KeyCode::KeyW => v.z = 0.0,
                    winit::keyboard::KeyCode::KeyS => v.z = 0.0,
                    winit::keyboard::KeyCode::KeyA => v.x = 0.0,
                    winit::keyboard::KeyCode::KeyD => v.x = 0.0,
                    winit::keyboard::KeyCode::Space => v.y = 0.0,
                    winit::keyboard::KeyCode::ControlLeft => v.y = 0.0,
                    _ => (),
                },
                _ => (),
            }
        }
        t.set_camera_speed(v.x, v.y, v.z);
    }
}

impl CameraController {
    pub fn new() -> Box<dyn Controller> {
        Box::new(Self {})
    }
}
