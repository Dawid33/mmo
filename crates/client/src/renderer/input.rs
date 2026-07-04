use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::{GameEventKind, InputEvent};

use super::{GameEvents, LocalPlayer};

fn map_key(code: KeyCode) -> Option<game::Key> {
    use game::Key as K;
    Some(match code {
        KeyCode::KeyW => K::KeyW,
        KeyCode::KeyA => K::KeyA,
        KeyCode::KeyS => K::KeyS,
        KeyCode::KeyD => K::KeyD,
        KeyCode::KeyE => K::KeyE,
        KeyCode::Space => K::Space,
        KeyCode::ControlLeft => K::ControlLeft,
        KeyCode::ShiftLeft => K::ShiftLeft,
        KeyCode::Escape => K::Escape,
        _ => return None,
    })
}

fn map_button(b: MouseButton) -> game::MouseButton {
    match b {
        MouseButton::Left => game::MouseButton::Left,
        MouseButton::Right => game::MouseButton::Right,
        MouseButton::Middle => game::MouseButton::Middle,
        MouseButton::Back => game::MouseButton::Other(3),
        MouseButton::Forward => game::MouseButton::Other(4),
        MouseButton::Other(n) => game::MouseButton::Other(n),
    }
}

pub fn forward_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut last_size: Local<Option<(u32, u32)>>,
    player: Res<LocalPlayer>,
    game: Res<GameEvents>,
) {
    let Some(player) = player.0 else { return };
    let send = |ev: InputEvent| {
        let _ = game.0.send(GameEventKind::PlayerInput(player, ev));
    };

    for code in keys.get_just_pressed() {
        if let Some(key) = map_key(*code) {
            send(InputEvent::Key { key, pressed: true });
        }
    }
    for code in keys.get_just_released() {
        if let Some(key) = map_key(*code) {
            send(InputEvent::Key { key, pressed: false });
        }
    }
    for b in buttons.get_just_pressed() {
        send(InputEvent::MouseButton { button: map_button(*b), pressed: true });
    }
    for b in buttons.get_just_released() {
        send(InputEvent::MouseButton { button: map_button(*b), pressed: false });
    }
    if motion.delta != Vec2::ZERO {
        send(InputEvent::MouseMotion { dx: motion.delta.x, dy: motion.delta.y });
    }
    if let Ok(w) = window.single() {
        let size = (w.physical_width(), w.physical_height());
        if *last_size != Some(size) {
            if last_size.is_some() {
                send(InputEvent::Resized { width: size.0, height: size.1 });
            }
            *last_size = Some(size);
        }
    }
}
