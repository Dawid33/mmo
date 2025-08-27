#[allow(unused)]
use log::info;

use crate::{transaction::GameDataTransaction, Controller};

pub struct CameraController {}

impl Controller for CameraController {
    fn on_tick<'a>(&mut self, _t: &mut GameDataTransaction) {
        // for (p, e_id) in iter {
        //     let p = t.get_player(p);

        //     let mut b = t.get_body(e_id).clone();
        //     let mut v = b.linvel().clone();
        //     v.x = 0.0;
        //     v.y = 0.0;
        //     v.z = 0.0;
        //     const SPEED: f32 = 10.0;
        //     if p.input.key_held(winit::keyboard::KeyCode::KeyW)
        //         || p.input.key_pressed(winit::keyboard::KeyCode::KeyW)
        //     {
        //         v.z = SPEED
        //     }
        //     if p.input.key_held(winit::keyboard::KeyCode::KeyS)
        //         || p.input.key_pressed(winit::keyboard::KeyCode::KeyS)
        //     {
        //         v.z = -SPEED
        //     }
        //     if p.input.key_held(winit::keyboard::KeyCode::KeyA)
        //         || p.input.key_pressed(winit::keyboard::KeyCode::KeyA)
        //     {
        //         v.x = SPEED
        //     }
        //     if p.input.key_held(winit::keyboard::KeyCode::KeyD)
        //         || p.input.key_pressed(winit::keyboard::KeyCode::KeyD)
        //     {
        //         v.x = -SPEED
        //     }
        //     if p.input.key_held(winit::keyboard::KeyCode::Space)
        //         || p.input.key_pressed(winit::keyboard::KeyCode::Space)
        //     {
        //         v.y = -SPEED
        //     }
        //     if p.input.key_held(winit::keyboard::KeyCode::ControlLeft)
        //         || p.input.key_pressed(winit::keyboard::KeyCode::ControlLeft)
        //     {
        //         v.y = SPEED
        //     }
        //     b.set_linvel(v, true);
        // }

        // let mut a = t.get_body(cid).unwrap().angvel().clone();
        // if t.get_input(&0).mouse_held(MouseButton::Left) {
        //     const SENSITIVITY: f32 = 1.0;
        //     let diff = t.get_input(&0).mouse_diff();
        //     a.x = -diff.0 / SENSITIVITY;
        //     a.y = -diff.1 / SENSITIVITY;
        // } else {
        //     a.x = 0.0;
        //     a.y = 0.0;
        //     a.z = 0.0;
        // }
        // t.set_entity_angular_velocity(cid, a);
    }
}

impl CameraController {
    pub fn new() -> Box<dyn Controller> {
        Box::new(Self {})
    }
}
