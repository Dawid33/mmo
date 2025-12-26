use log::info;
use ordered_float::OrderedFloat;
use parry3d::math::Real;
use rapier3d::prelude::PhysicsHooks;
use serde::{Deserialize, Serialize};
use winit::dpi::PhysicalSize;
use winit::event::MouseButton;
use winit::event::{ElementState, MouseScrollDelta};
use winit::keyboard::{Key, KeyCode, PhysicalKey};

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
enum KeyState {
    Released,
    Pressed,
    Held,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, Hash)]
pub struct WinitInput {
    window_resized: Option<PhysicalSize<u32>>,
    keyboard_state: BTreeMap<winit::keyboard::KeyCode, KeyState>,
    mouse_diff: Option<(Real, Real)>,
}

impl WinitInput {
    pub fn update(
        &mut self,
        event: crate::common::WinitEvent,
    ) -> Option<Box<dyn Fn(&mut WinitInput) + 'static + Send + Sync>> {
        match event {
            crate::WinitEvent::WindowEvent(window_event) => match window_event {
                crate::WindowEvent::Resized(size) => {
                    let old = self.window_resized.take();
                    self.window_resized = Some(size);
                    Some(Box::new(move |s| {
                        s.window_resized = old;
                    }))
                }
                crate::WindowEvent::KeyboardInput {
                    physical_key,
                    logical_key,
                    location,
                    state,
                    repeat,
                    is_synthetic,
                } => {
                    let physical_key = if let PhysicalKey::Code(key) = physical_key {
                        key
                    } else {
                        return None;
                    };
                    if let Some(k) = self.keyboard_state.get_mut(&physical_key) {
                        match k {
                            KeyState::Released => {
                                if !state.is_pressed() {
                                    None
                                } else {
                                    *k = KeyState::Pressed;
                                    Some(Box::new(move |s| {
                                        *s.keyboard_state.get_mut(&physical_key).unwrap() =
                                            KeyState::Released;
                                    }))
                                }
                            }
                            KeyState::Held | KeyState::Pressed => {
                                if state.is_pressed() {
                                    None
                                } else {
                                    let old = k.clone();
                                    *k = KeyState::Released;
                                    Some(Box::new(move |s| {
                                        *s.keyboard_state.get_mut(&physical_key).unwrap() =
                                            old.clone();
                                    }))
                                }
                            }
                        }
                    } else {
                        let state = if state.is_pressed() {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        };
                        self.keyboard_state.insert(physical_key.clone(), state);
                        Some(Box::new(move |s| {
                            s.keyboard_state.remove(&physical_key);
                        }))
                    }
                }
                _ => None,
            },
            crate::WinitEvent::DeviceEvent(device_event) => match device_event {
                crate::DeviceEvent::MouseMotion { delta } => {
                    let old = self.mouse_diff;
                    if let Some(m) = &mut self.mouse_diff {
                        m.0 += delta.0 as f32;
                        m.1 += delta.1 as f32;
                    } else {
                        self.mouse_diff =
                            Some((OrderedFloat(delta.0 as f32), OrderedFloat(delta.1 as f32)));
                    }
                    Some(Box::new(move |s: &mut Self| {
                        s.mouse_diff = old;
                    }))
                }
                _ => None,
            },
            crate::WinitEvent::NewEvents => return None,
            crate::WinitEvent::AboutToWait => return None,
        }
    }

    pub fn step(&mut self) -> Option<Box<dyn Fn(&mut WinitInput) + 'static + Send + Sync>> {
        let old_mouse_diff = self.mouse_diff;
        self.mouse_diff = None;
        let mut changed = Vec::new();
        for (key, state) in &mut self.keyboard_state {
            if KeyState::Pressed == *state {
                *state = KeyState::Held;
                changed.push(key.clone());
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
            (*m.0, *m.1)
        } else {
            (0.0, 0.0)
        }
    }

    pub fn window_resized(&self) -> &Option<PhysicalSize<u32>> {
        &self.window_resized
    }

    pub fn key_pressed(&self, key: &winit::keyboard::KeyCode) -> bool {
        if let Some(key) = self.keyboard_state.get(key) {
            match key {
                KeyState::Pressed => return true,
                _ => (),
            }
        }
        false
    }
    pub fn key_held(&self, key: &winit::keyboard::KeyCode) -> bool {
        if let Some(key) = self.keyboard_state.get(key) {
            match key {
                KeyState::Held | KeyState::Pressed => return true,
                _ => (),
            }
        }
        false
    }
}
