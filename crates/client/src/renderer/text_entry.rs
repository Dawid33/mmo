//! The typing gate. While a Slint text field has the keyboard ([`TextEntry`]),
//! no key reaches the game: `ButtonInput<KeyCode>` is emptied each frame at
//! one point, before any game system reads it, instead of every key reader
//! checking a flag. Slint is fed from `KeyboardInput` messages
//! (`slint_overlay::slint_input`), which this leaves alone.
//!
//! The frame the gate closes on the game, every held key is released once, so
//! `input::forward_input` sends the sim its key-ups and the body stops. A key
//! that was down at any time while typing stays up afterwards until it is let
//! go: neither a key held through, nor the operating system's key repeat of
//! the Esc or Enter that ended the typing, presses anything in the game.

use std::collections::BTreeSet;

use bevy::input::keyboard::KeyboardInput;
use bevy::input::ButtonState;
use bevy::prelude::*;

use super::cursor::CursorPolicy;
use crate::session::SessionLocal;

/// A Slint text field has the keyboard. Written by whoever owns the field
/// (the chat box).
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEntry {
    pub on: bool,
}

/// What [`mute_keys_while_typing`] remembers between frames.
#[derive(Default)]
pub struct MuteState {
    was_on: bool,
    /// Keys that were down while typing and have not been released since.
    held: BTreeSet<KeyCode>,
}

/// Keeps the game's key state empty while [`TextEntry`] is on (see the
/// module). Runs in `PreUpdate` after Bevy's input systems.
pub fn mute_keys_while_typing(
    entry: Res<TextEntry>,
    mut state: SessionLocal<MuteState>,
    mut events: MessageReader<KeyboardInput>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut policy: Option<ResMut<CursorPolicy>>,
) {
    let state = state.get();
    if let Some(policy) = policy.as_mut().filter(|p| p.typing != entry.on) {
        policy.typing = entry.on;
    }
    let rising = entry.on && !state.was_on;
    state.was_on = entry.on;
    if !entry.on && state.held.is_empty() {
        events.clear();
        return;
    }
    if rising {
        state.held.extend(keys.get_pressed().copied());
    }
    for event in events.read() {
        match event.state {
            ButtonState::Pressed if entry.on => {
                state.held.insert(event.key_code);
            }
            ButtonState::Pressed => {}
            ButtonState::Released => {
                state.held.remove(&event.key_code);
            }
        }
    }
    if entry.on {
        keys.release_all();
        if !rising {
            // The rising frame keeps its `just_released`, the sim's key-ups.
            keys.clear();
        }
        return;
    }
    // Typing is over: what was down then stays up until it is let go.
    for key in state.held.iter().copied() {
        if keys.pressed(key) {
            keys.release(key);
        }
        keys.clear_just_pressed(key);
        keys.clear_just_released(key);
    }
}

