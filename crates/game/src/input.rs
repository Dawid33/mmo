use ordered_float::OrderedFloat;
use parry3d::math::Real;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Key {
    KeyW,
    KeyA,
    KeyS,
    KeyD,
    KeyE,
    Space,
    ControlLeft,
    ShiftLeft,
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

/// Engine-neutral input event. This is the wire format for player input —
/// it must never contain types from a windowing library.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum InputEvent {
    Resized { width: u32, height: u32 },
    Key { key: Key, pressed: bool },
    MouseButton { button: MouseButton, pressed: bool },
    /// Line-based wheel scrolling, in lines.
    MouseWheel { x: f32, y: f32 },
    /// Accumulated raw mouse motion for one frame.
    MouseMotion { dx: f32, dy: f32 },
    Focused(bool),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
enum KeyState {
    Released,
    Pressed,
    Held,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, Hash, PartialEq)]
pub struct InputState {
    window_resized: Option<(u32, u32)>,
    keyboard_state: BTreeMap<Key, KeyState>,
    mouse_diff: Option<(Real, Real)>,
}

impl InputState {
    pub fn update(
        &mut self,
        event: InputEvent,
    ) -> Option<Box<dyn Fn(&mut InputState) + 'static + Send + Sync>> {
        match event {
            InputEvent::Resized { width, height } => {
                let old = self.window_resized.take();
                self.window_resized = Some((width, height));
                Some(Box::new(move |s| s.window_resized = old))
            }
            InputEvent::Key { key, pressed } => {
                if let Some(k) = self.keyboard_state.get_mut(&key) {
                    match k {
                        KeyState::Released => {
                            if !pressed {
                                None
                            } else {
                                *k = KeyState::Pressed;
                                Some(Box::new(move |s| {
                                    *s.keyboard_state.get_mut(&key).unwrap() = KeyState::Released;
                                }))
                            }
                        }
                        KeyState::Held | KeyState::Pressed => {
                            if pressed {
                                None
                            } else {
                                let old = k.clone();
                                *k = KeyState::Released;
                                Some(Box::new(move |s| {
                                    *s.keyboard_state.get_mut(&key).unwrap() = old.clone();
                                }))
                            }
                        }
                    }
                } else {
                    let state = if pressed {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    };
                    self.keyboard_state.insert(key, state);
                    Some(Box::new(move |s| {
                        s.keyboard_state.remove(&key);
                    }))
                }
            }
            InputEvent::MouseMotion { dx, dy } => {
                let old = self.mouse_diff;
                if let Some(m) = &mut self.mouse_diff {
                    m.0 += dx;
                    m.1 += dy;
                } else {
                    self.mouse_diff = Some((OrderedFloat(dx), OrderedFloat(dy)));
                }
                Some(Box::new(move |s: &mut Self| s.mouse_diff = old))
            }
            InputEvent::MouseButton { .. } | InputEvent::MouseWheel { .. } | InputEvent::Focused(_) => {
                None
            }
        }
    }

    pub fn step(&mut self) -> Option<Box<dyn Fn(&mut InputState) + 'static + Send + Sync>> {
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

    pub fn window_resized(&self) -> &Option<(u32, u32)> {
        &self.window_resized
    }

    pub fn key_pressed(&self, key: &Key) -> bool {
        if let Some(key) = self.keyboard_state.get(key) {
            match key {
                KeyState::Pressed => return true,
                _ => (),
            }
        }
        false
    }
    pub fn key_held(&self, key: &Key) -> bool {
        if let Some(key) = self.keyboard_state.get(key) {
            match key {
                KeyState::Held | KeyState::Pressed => return true,
                _ => (),
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_step_release_with_undo() {
        let mut input = InputState::default();

        let undo_press = input
            .update(InputEvent::Key {
                key: Key::KeyE,
                pressed: true,
            })
            .unwrap();
        assert!(input.key_pressed(&Key::KeyE));
        assert!(input.key_held(&Key::KeyE));

        let undo_step = input.step().unwrap();
        assert!(!input.key_pressed(&Key::KeyE), "step demotes Pressed to Held");
        assert!(input.key_held(&Key::KeyE));

        undo_step(&mut input);
        assert!(input.key_pressed(&Key::KeyE), "undoing step restores Pressed");

        undo_press(&mut input);
        assert!(!input.key_held(&Key::KeyE), "undoing press removes the key");
    }

    #[test]
    fn mouse_motion_accumulates_and_steps_away() {
        let mut input = InputState::default();
        input.update(InputEvent::MouseMotion { dx: 1.5, dy: -2.0 });
        input.update(InputEvent::MouseMotion { dx: 0.5, dy: 1.0 });
        assert_eq!(input.mouse_diff(), (2.0, -1.0));
        input.step();
        assert_eq!(input.mouse_diff(), (0.0, 0.0));
    }
}
