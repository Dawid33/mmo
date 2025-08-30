use std::ops::DerefMut;

#[allow(unused)]
use log::info;
use rapier3d::math::Vector;

use crate::{transaction::GameDataTransaction, Controller, GameData};

pub struct CameraController {}

impl Controller for CameraController {
    fn on_tick<'a>(&mut self, data: &mut GameData) {
        for (p, e_id) in data.players.iter() {
            let p = data.ecs.player.get(*e_id);
            let handle = data.ecs.rigidbody.get(*e_id).clone();
            let b = data.physics.bodies.get_mut(handle).unwrap();
            let mut linvel = Vector::zeros();
            const SPEED: f32 = 10.0;
            // if p.input.key_held(winit::keyboard::KeyCode::KeyW)
            //     || p.input.key_pressed(winit::keyboard::KeyCode::KeyW)
            // {
            //     linvel.z = SPEED
            // }
            // if p.input.key_held(winit::keyboard::KeyCode::KeyS)
            //     || p.input.key_pressed(winit::keyboard::KeyCode::KeyS)
            // {
            //     linvel.z = -SPEED
            // }
            // if p.input.key_held(winit::keyboard::KeyCode::KeyA)
            //     || p.input.key_pressed(winit::keyboard::KeyCode::KeyA)
            // {
            //     linvel.x = SPEED
            // }
            // if p.input.key_held(winit::keyboard::KeyCode::KeyD)
            //     || p.input.key_pressed(winit::keyboard::KeyCode::KeyD)
            // {
            //     linvel.x = -SPEED
            // }
            // if p.input.key_held(winit::keyboard::KeyCode::Space)
            //     || p.input.key_pressed(winit::keyboard::KeyCode::Space)
            // {
            //     linvel.y = -SPEED
            // }
            // if p.input.key_held(winit::keyboard::KeyCode::ControlLeft)
            //     || p.input.key_pressed(winit::keyboard::KeyCode::ControlLeft)
            // {
            //     linvel.y = SPEED
            // }
            if linvel != *b.linvel() {
                let old = b.linvel().clone();
                b.set_linvel(linvel, true);
                data.physics
                    .bodies
                    .undo(move |d| d.get_mut(handle).unwrap().set_linvel(old, true));
            }
        }

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
