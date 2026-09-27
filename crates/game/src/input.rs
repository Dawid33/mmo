use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, state_hash::StateHash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, state_hash::StateHash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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
    /// Set the fps-camera mode (the client drives its body) to exactly this
    /// value — idempotent, unlike the E toggle — WITHOUT asking the window
    /// for the pointer: no `SetFreeCam` is emitted, so the cursor stays free.
    /// For scripted drivers (the client's autopilot) that must not take the
    /// user's cursor. Applied by the region, not `InputState`.
    FpsCam(bool),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, state_hash::StateHash)]
enum KeyState {
    Released,
    Pressed,
    Held,
    /// Pressed and released again before the tick consumed the press. Reads
    /// as pressed + held for that one tick, so a tap shorter than the tick
    /// period (a quick double-tap, a fast toggle) is not lost.
    Tapped,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, state_hash::StateHash, PartialEq)]
pub struct InputState {
    window_resized: Option<(u32, u32)>,
    keyboard_state: BTreeMap<Key, KeyState>,
    mouse_diff: Option<(f32, f32)>,
    /// Mouse buttons, with the same press / hold / tap semantics as keys.
    mouse_buttons: BTreeMap<MouseButton, KeyState>,
}

/// Undo closure for one input transition.
type InputUndo = Box<dyn Fn(&mut InputState) + 'static + Send + Sync>;

/// Applies a press or release to one button's state and returns the undo.
/// Shared by keys and mouse buttons; `map` selects which table it acts on.
fn press_transition<B>(
    s: &mut InputState,
    map: fn(&mut InputState) -> &mut BTreeMap<B, KeyState>,
    button: B,
    pressed: bool,
) -> Option<InputUndo>
where
    B: Ord + Copy + Send + Sync + 'static,
{
    if let Some(k) = map(s).get_mut(&button) {
        match k {
            KeyState::Released | KeyState::Tapped => {
                if !pressed {
                    None
                } else {
                    let old = k.clone();
                    *k = KeyState::Pressed;
                    Some(Box::new(move |s| {
                        *map(s).get_mut(&button).unwrap() = old.clone();
                    }))
                }
            }
            KeyState::Held | KeyState::Pressed => {
                if pressed {
                    None
                } else {
                    let old = k.clone();
                    // An unconsumed press survives its release until the
                    // next tick has seen it.
                    *k = if old == KeyState::Pressed { KeyState::Tapped } else { KeyState::Released };
                    Some(Box::new(move |s| {
                        *map(s).get_mut(&button).unwrap() = old.clone();
                    }))
                }
            }
        }
    } else {
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        map(s).insert(button, state);
        Some(Box::new(move |s| {
            map(s).remove(&button);
        }))
    }
}

/// One tick's demotion of a button table: Pressed -> Held, Tapped -> Released.
fn step_table<B: Ord + Copy>(map: &mut BTreeMap<B, KeyState>) -> Vec<(B, KeyState)> {
    let mut changed = Vec::new();
    for (button, state) in map.iter_mut() {
        let next = match state {
            KeyState::Pressed => KeyState::Held,
            KeyState::Tapped => KeyState::Released,
            KeyState::Released | KeyState::Held => continue,
        };
        changed.push((*button, std::mem::replace(state, next)));
    }
    changed
}

fn is_pressed(state: Option<&KeyState>) -> bool {
    matches!(state, Some(KeyState::Pressed | KeyState::Tapped))
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
                press_transition(self, |s| &mut s.keyboard_state, key, pressed)
            }
            InputEvent::MouseButton { button, pressed } => {
                press_transition(self, |s| &mut s.mouse_buttons, button, pressed)
            }
            InputEvent::MouseMotion { dx, dy } => {
                let old = self.mouse_diff;
                if let Some(m) = &mut self.mouse_diff {
                    m.0 += dx;
                    m.1 += dy;
                } else {
                    self.mouse_diff = Some((dx, dy));
                }
                Some(Box::new(move |s: &mut Self| s.mouse_diff = old))
            }
            InputEvent::MouseWheel { .. } | InputEvent::Focused(_) | InputEvent::FpsCam(_) => None,
        }
    }

    pub fn step(&mut self) -> Option<Box<dyn Fn(&mut InputState) + 'static + Send + Sync>> {
        let old_mouse_diff = self.mouse_diff;
        self.mouse_diff = None;
        let keys = step_table(&mut self.keyboard_state);
        let buttons = step_table(&mut self.mouse_buttons);
        Some(Box::new(move |s: &mut Self| {
            s.mouse_diff = old_mouse_diff;
            for (key, old) in keys.iter() {
                *s.keyboard_state.get_mut(key).unwrap() = old.clone();
            }
            for (button, old) in buttons.iter() {
                *s.mouse_buttons.get_mut(button).unwrap() = old.clone();
            }
        }))
    }

    pub fn mouse_diff(&mut self) -> (f32, f32) {
        if let Some(m) = self.mouse_diff {
            (m.0, m.1)
        } else {
            (0.0, 0.0)
        }
    }

    pub fn window_resized(&self) -> &Option<(u32, u32)> {
        &self.window_resized
    }

    pub fn key_pressed(&self, key: &Key) -> bool {
        is_pressed(self.keyboard_state.get(key))
    }

    /// The button went down since the last tick (edge, like `key_pressed`).
    pub fn mouse_pressed(&self, button: &MouseButton) -> bool {
        is_pressed(self.mouse_buttons.get(button))
    }
    pub fn key_held(&self, key: &Key) -> bool {
        if let Some(key) = self.keyboard_state.get(key) {
            match key {
                KeyState::Held | KeyState::Pressed | KeyState::Tapped => return true,
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
    fn mouse_click_is_seen_for_one_tick_even_if_released_first() {
        let mut input = InputState::default();
        let left = MouseButton::Left;
        input.update(InputEvent::MouseButton { button: left, pressed: true });
        assert!(input.mouse_pressed(&left));
        // Released before the tick ran: the click still counts for that tick.
        input.update(InputEvent::MouseButton { button: left, pressed: false });
        assert!(input.mouse_pressed(&left), "a click shorter than a tick is not lost");
        let undo = input.step().unwrap();
        assert!(!input.mouse_pressed(&left), "one click is one edge");
        undo(&mut input);
        assert!(input.mouse_pressed(&left), "step undo restores the edge");
        assert!(!input.mouse_pressed(&MouseButton::Right));
    }

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
    fn tap_within_one_tick_is_seen_once_with_undo() {
        let mut input = InputState::default();
        let key = |pressed| InputEvent::Key { key: Key::Space, pressed };
        input.update(key(true));
        let before_release = input.clone();
        let undo_release = input.update(key(false)).unwrap();
        assert!(input.key_pressed(&Key::Space), "the tick still sees the tap");

        let tapped = input.clone();
        let undo_step = input.step().unwrap();
        assert!(!input.key_pressed(&Key::Space) && !input.key_held(&Key::Space));

        undo_step(&mut input);
        assert_eq!(input, tapped, "undoing step restores Tapped");
        undo_release(&mut input);
        assert_eq!(input, before_release, "undoing the release restores Pressed");

        // A second press inside the same tick is one press, not two.
        input.update(key(false));
        let undo_repress = input.update(key(true)).unwrap();
        assert!(input.key_pressed(&Key::Space));
        undo_repress(&mut input);
        assert_eq!(input, tapped);
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
