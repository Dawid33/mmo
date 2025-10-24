use bevy::math::Vec2;
use log::info;
use serde::{Deserialize, Serialize};
use winit::dpi::PhysicalSize;
use winit::event::MouseButton;
use winit::event::{ElementState, MouseScrollDelta};
use winit::keyboard::{Key, KeyCode, PhysicalKey};

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum KeyState {
    Released,
    Pressed,
    Held,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BevyInput {
    keyboard_state: BTreeMap<bevy::input::keyboard::KeyCode, KeyState>,
    mouse_diff: Option<Vec2>,
}

impl BevyInput {
    pub fn update(
        &mut self,
        event: crate::common::BevyEvent,
    ) -> Option<Box<dyn Fn(&mut BevyInput) + 'static + Send + Sync>> {
        match event {
            crate::BevyEvent::MouseMotionInput(input) => {
                let old = self.mouse_diff;
                if let Some(m) = &mut self.mouse_diff {
                    *m += input;
                } else {
                    self.mouse_diff = Some(input);
                }
                Some(Box::new(move |s: &mut Self| {
                    s.mouse_diff = old;
                }))
            }
            crate::BevyEvent::KeyboardInput(input) => {
                info!("{:?}", input);
                if let Some(k) = self.keyboard_state.get_mut(&input.key_code) {
                    match k {
                        KeyState::Released => {
                            if !input.state.is_pressed() {
                                None
                            } else {
                                *k = KeyState::Pressed;
                                Some(Box::new(move |s| {
                                    *s.keyboard_state.get_mut(&input.key_code).unwrap() =
                                        KeyState::Released;
                                }))
                            }
                        }
                        KeyState::Held | KeyState::Pressed => {
                            if input.state.is_pressed() {
                                None
                            } else {
                                let old = k.clone();
                                *k = KeyState::Released;
                                Some(Box::new(move |s| {
                                    *s.keyboard_state.get_mut(&input.key_code).unwrap() =
                                        old.clone();
                                }))
                            }
                        }
                    }
                } else {
                    let state = if input.state.is_pressed() {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    };
                    self.keyboard_state.insert(input.key_code.clone(), state);
                    Some(Box::new(move |s| {
                        s.keyboard_state.remove(&input.key_code);
                    }))
                }
            }
            crate::BevyEvent::MouseButtonInput(mouse_button_input) => None,
            crate::BevyEvent::MouseMotionInput(mouse_motion) => None,
        }
    }

    pub fn step(&mut self) -> Option<Box<dyn Fn(&mut BevyInput) + 'static + Send + Sync>> {
        let old_mouse_diff = self.mouse_diff;
        self.mouse_diff = None;
        let mut changed = Vec::new();
        for (key, state) in &mut self.keyboard_state {
            if KeyState::Pressed == *state {
                *state = KeyState::Held;
                changed.push(*key);
            }
        }
        Some(Box::new(move |s: &mut Self| {
            s.mouse_diff = old_mouse_diff;
            for key in changed.iter() {
                *s.keyboard_state.get_mut(key).unwrap() = KeyState::Pressed;
            }
        }))
    }

    pub fn mouse_diff(&mut self) -> (f32, f32) {
        if let Some(m) = self.mouse_diff {
            (m.x, m.y)
        } else {
            (0.0, 0.0)
        }
    }

    pub fn key_pressed(&self, key: &bevy::input::keyboard::KeyCode) -> bool {
        if let Some(key) = self.keyboard_state.get(key) {
            match key {
                KeyState::Pressed => return true,
                _ => (),
            }
        }
        false
    }
    pub fn key_held(&self, key: &bevy::input::keyboard::KeyCode) -> bool {
        if let Some(key) = self.keyboard_state.get(key) {
            match key {
                KeyState::Held | KeyState::Pressed => return true,
                _ => (),
            }
        }
        false
    }
}
