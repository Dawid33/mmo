use std::time::Duration;

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use game::{GameEventKind, InputEvent};

use super::{GameEvents, LocalPlayer};

/// Touchpad scroll (pixel units) needed for one hotbar slot step.
const WHEEL_PIXELS_PER_SLOT: f32 = 40.0;

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
        KeyCode::Digit1 => K::Digit1,
        KeyCode::Digit2 => K::Digit2,
        KeyCode::Digit3 => K::Digit3,
        KeyCode::Digit4 => K::Digit4,
        KeyCode::Digit5 => K::Digit5,
        KeyCode::Digit6 => K::Digit6,
        KeyCode::Digit7 => K::Digit7,
        KeyCode::Digit8 => K::Digit8,
        KeyCode::Digit9 => K::Digit9,
        KeyCode::KeyQ => K::KeyQ,
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

/// Minimum wall-clock spacing between `MouseMotion` packets. Deltas arriving
/// inside the window are summed and emitted when it expires.
///
/// Must stay well under `game::TICK_RATE`: the sim sums mouse deltas and drains
/// them once per tick (`InputState::step`), and the only consumer is the camera
/// controller, which reads that per-tick sum. So any window shorter than a tick
/// is simulation-neutral — it changes how many packets carry the same sum, not
/// the sum. Without it the packet rate is bound to the frame rate, and each
/// packet costs its own QUIC uni-stream.
pub const MOUSE_COALESCE_MS: u64 = 30;

/// The live coalescing window. Defaults to [`MOUSE_COALESCE_MS`]; a resource so
/// tests can vary it. `Duration::ZERO` restores per-frame emission.
#[derive(Resource, Debug, Clone, Copy)]
pub struct MouseCoalesceWindow(pub Duration);

impl Default for MouseCoalesceWindow {
    fn default() -> Self {
        Self(Duration::from_millis(MOUSE_COALESCE_MS))
    }
}

/// Sums per-frame mouse deltas and releases them at most once per window.
pub(crate) struct MouseCoalescer {
    pending: Vec2,
    since_send: Duration,
}

impl Default for MouseCoalescer {
    fn default() -> Self {
        // Saturated: the first delta of the session emits immediately, exactly
        // like any other delta arriving after an idle period.
        Self {
            pending: Vec2::ZERO,
            since_send: Duration::MAX,
        }
    }
}

impl MouseCoalescer {
    /// Fold one frame's motion in. Returns `Some(sum)` on the frames where an
    /// event should be emitted, having reset the accumulator.
    pub(crate) fn accumulate(
        &mut self,
        delta: Vec2,
        dt: Duration,
        window: Duration,
    ) -> Option<Vec2> {
        self.pending += delta;
        // Saturating, and deliberately NOT clamped to `window`: an idle mouse
        // must stay armed past the threshold, so that motion resuming after a
        // pause emits on its first frame. (Clamping to the window would tie the
        // armed state to whichever window was in force while idling, and
        // silently disarm the leading edge if the window were later raised.)
        self.since_send = self.since_send.saturating_add(dt);

        if self.pending == Vec2::ZERO || self.since_send < window {
            return None;
        }
        let sum = self.pending;
        self.pending = Vec2::ZERO;
        self.since_send = Duration::ZERO;
        Some(sum)
    }
}

/// Per-session keyboard state of the body's input that is not a key's own:
/// one `SessionLocal` (the system is at Bevy's 16-parameter limit).
#[derive(Default)]
pub(crate) struct BodyKeys {
    /// Touchpad scroll not yet worth a hotbar step.
    wheel_acc: f32,
    sprint: SprintLatch,
}

/// The Minecraft-style double-tap-W sprint, reduced to the virtual
/// [`game::Key::Sprint`] the sim reads (as it reads a held Ctrl): pressed when
/// the latch closes, released when W lets go.
#[derive(Default)]
pub(crate) struct SprintLatch {
    latch: super::commander::Sprint,
    /// What the sim was last told.
    sent: bool,
}

impl SprintLatch {
    /// This frame's change to send, if any.
    fn update(&mut self, keys: &ButtonInput<KeyCode>, now: f32) -> Option<bool> {
        let w = KeyCode::KeyW;
        self.latch = self
            .latch
            .update(keys.just_pressed(w), keys.pressed(w), now);
        (self.latch.latched != self.sent).then(|| {
            self.sent = self.latch.latched;
            self.sent
        })
    }
}

pub fn forward_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    // One parameter: the system is at Bevy's 16-parameter limit.
    (time, coalesce_window, mut sprint_input): (
        Res<Time>,
        Res<MouseCoalesceWindow>,
        Option<ResMut<super::commander::view_feel::SprintInput>>,
    ),
    mut coalescer: crate::session::SessionLocal<MouseCoalescer>,
    window: Query<(&Window, &CursorOptions), With<PrimaryWindow>>,
    // Per session: a new world's sim has not been told the window size.
    mut last_size: crate::session::SessionLocal<Option<(u32, u32)>>,
    player: Res<LocalPlayer>,
    game: Res<GameEvents>,
    mut cursor: Option<ResMut<super::cursor::CursorPolicy>>,
    mode: Option<Res<super::commander::CameraMode>>,
    first_person: Option<Res<super::commander::first_person::FirstPerson>>,
    adventure: Option<Res<super::commander::adventure::ClientPlayMode>>,
    editor: Option<Res<super::commander::adventure::EditorState>>,
    scroll: Res<AccumulatedMouseScroll>,
    mut body_keys: crate::session::SessionLocal<BodyKeys>,
) {
    let BodyKeys { wheel_acc, sprint } = body_keys.get();
    let last_size = last_size.get();
    let coalescer = coalescer.get();
    let Some(player) = player.0 else { return };
    // The commander drives no body: its keys, clicks and mouse travel are
    // client-local (camera, minimap, selection) and must not walk the inert
    // sim body or break blocks. Only the window size still reaches the sim.
    let commander = mode.is_some_and(|m| *m == super::commander::CameraMode::Commander);
    // ...unless the commander's adventurer is in first person: then the body
    // takes the keys and the mouse, all but E — the sim's own fps-cam toggle,
    // which `first_person` owns here.
    let first_person = commander && first_person.is_some_and(|fp| fp.on);
    // A client-local overlay (later chat) owns the pointer: its clicks and
    // the mouse travel to reach it must not break blocks or turn the camera.
    // Releases still go through so a button held when it opened never sticks.
    // A modal game screen (the Esc menu, the map, the inventory) holds the
    // pointer the same way: its clicks must not mine or place.
    // So does a pointer given back (`CursorPolicy::released`): the click that
    // takes it again must not also break a block.
    //
    // E opening the inventory stops the look on the press's own frame: the
    // sim opens it a tick later, and the view turning on until then read as
    // the camera running past the press. Set before the gate is read, so this
    // frame's travel is held back here and in `look::integrate_local_look`
    // (ordered after) alike.
    let body_driven = adventure.as_ref().is_some_and(|m| m.adventure())
        || editor.as_ref().is_some_and(|e| e.active);
    let adventure_body = first_person && body_driven;
    if let Some(c) = cursor.as_mut() {
        let now = time.elapsed();
        if c.inventory_opening.is_some_and(|at| {
            c.ui_modal || now.saturating_sub(at) >= super::cursor::INVENTORY_OPEN_WAIT
        }) {
            c.inventory_opening = None;
        }
        if adventure_body && !c.loading && c.look_live() && keys.just_pressed(KeyCode::KeyE) {
            c.inventory_opening = Some(now);
        }
    }
    let overlay_open = cursor.as_ref().is_some_and(|c| !c.look_live());
    let send = |ev: InputEvent| {
        let _ = game.0.send(GameEventKind::PlayerInput(player, ev));
    };

    // Behind the loading screen nothing the player does reaches the sim: no
    // key, click or mouse travel (the body would walk and turn in a world the
    // player cannot see yet). Only the window size below still goes through.
    let loading = cursor.as_ref().is_some_and(|c| c.loading);
    // What the sim reads as sprint (`held(ControlLeft) || held(Sprint)`), for
    // the view's sprint FOV; off while the body gets no keys.
    let body_keys = !loading && (!commander || first_person);
    let sprinting = body_keys && (keys.pressed(KeyCode::ControlLeft) || sprint.sent);
    if let Some(s) = sprint_input.as_mut() {
        s.set_if_neq(super::commander::view_feel::SprintInput(sprinting));
    }
    if loading {
    } else if !commander {
        forward_body_input(
            &keys,
            &buttons,
            &motion,
            &time,
            &coalesce_window,
            coalescer,
            sprint,
            &window,
            overlay_open,
            send,
        );
    } else if first_person {
        // E is the sim's inventory key in Adventure; elsewhere it is the
        // fps-cam toggle `first_person` owns. Q reaches the sim only here,
        // never in the commander (where it holds the tool wheel).
        let adventure = body_driven;
        let send_body = |ev: InputEvent| {
            if adventure
                || !matches!(
                    ev,
                    InputEvent::Key {
                        key: game::Key::KeyE,
                        ..
                    }
                )
            {
                send(ev);
            }
        };
        // One hotbar step per notch: lines count one each, pixels (touchpads)
        // accumulate until WHEEL_PIXELS_PER_SLOT.
        if !overlay_open && scroll.delta.y != 0.0 {
            let per_notch = match scroll.unit {
                MouseScrollUnit::Line => 1.0,
                MouseScrollUnit::Pixel => WHEEL_PIXELS_PER_SLOT,
            };
            *wheel_acc += scroll.delta.y;
            while wheel_acc.abs() >= per_notch {
                let step = wheel_acc.signum();
                *wheel_acc -= step * per_notch;
                send_body(InputEvent::MouseWheel { x: 0.0, y: step });
            }
        }
        forward_body_input(
            &keys,
            &buttons,
            &motion,
            &time,
            &coalesce_window,
            coalescer,
            sprint,
            &window,
            overlay_open,
            send_body,
        );
    }
    if let Ok((w, _)) = window.single() {
        let size = (w.physical_width(), w.physical_height());
        if *last_size != Some(size) {
            if last_size.is_some() {
                send(InputEvent::Resized {
                    width: size.0,
                    height: size.1,
                });
            }
            *last_size = Some(size);
        }
    }
}

/// Keys, mouse motion and buttons for the walking body (first-person mode).
#[allow(clippy::too_many_arguments)]
fn forward_body_input(
    keys: &ButtonInput<KeyCode>,
    buttons: &ButtonInput<MouseButton>,
    motion: &AccumulatedMouseMotion,
    time: &Time,
    coalesce_window: &MouseCoalesceWindow,
    coalescer: &mut MouseCoalescer,
    sprint: &mut SprintLatch,
    window: &Query<(&Window, &CursorOptions), With<PrimaryWindow>>,
    overlay_open: bool,
    send: impl Fn(InputEvent),
) {
    for code in keys.get_just_pressed() {
        if let Some(key) = map_key(*code) {
            send(InputEvent::Key { key, pressed: true });
        }
    }
    for code in keys.get_just_released() {
        if let Some(key) = map_key(*code) {
            send(InputEvent::Key {
                key,
                pressed: false,
            });
        }
    }
    if let Some(pressed) = sprint.update(keys, time.elapsed_secs()) {
        send(InputEvent::Key {
            key: game::Key::Sprint,
            pressed,
        });
    }
    // Coalesce HERE, at the source — not at the send boundary. Each event is
    // predicted locally and replayed by the server, so emitting N events that
    // the server sees as 1 would give the two divergent logs and break rollback.
    // A press flushes the window first: the camera shows the per-frame look
    // (`look.rs`), so a click must reach the sim after all the motion that
    // aimed it, or an edit would hit the block the crosshair just left.
    // Raw mouse motion arrives from the device whether or not this window has
    // the pointer: only a grabbed cursor in a focused window turns the camera
    // (as `look::integrate_local_look`). Otherwise moving the mouse over
    // another window — alt-tabbed away, or beside an autopilot run, whose
    // `FpsCam` input never grabs — turned the player.
    let pointer_ours = window
        .single()
        .is_ok_and(|(w, c)| w.focused && c.grab_mode == CursorGrabMode::Locked);
    let delta = if overlay_open || !pointer_ours {
        Vec2::ZERO
    } else {
        motion.delta
    };
    let pressing = !overlay_open && buttons.get_just_pressed().next().is_some();
    let window_len = if pressing {
        Duration::ZERO
    } else {
        coalesce_window.0
    };
    if let Some(sum) = coalescer.accumulate(delta, time.delta(), window_len) {
        send(InputEvent::MouseMotion {
            dx: sum.x,
            dy: sum.y,
        });
    }
    if !overlay_open {
        for b in buttons.get_just_pressed() {
            send(InputEvent::MouseButton {
                button: map_button(*b),
                pressed: true,
            });
        }
    }
    for b in buttons.get_just_released() {
        send(InputEvent::MouseButton {
            button: map_button(*b),
            pressed: false,
        });
    }
}
