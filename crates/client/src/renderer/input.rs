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
        Self { pending: Vec2::ZERO, since_send: Duration::MAX }
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

pub fn forward_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    coalesce_window: Res<MouseCoalesceWindow>,
    mut coalescer: crate::session::SessionLocal<MouseCoalescer>,
    window: Query<(&Window, &CursorOptions), With<PrimaryWindow>>,
    // Per session: a new world's sim has not been told the window size.
    mut last_size: crate::session::SessionLocal<Option<(u32, u32)>>,
    player: Res<LocalPlayer>,
    game: Res<GameEvents>,
    cursor: Option<Res<super::cursor::CursorPolicy>>,
    mode: Option<Res<super::commander::CameraMode>>,
    first_person: Option<Res<super::commander::first_person::FirstPerson>>,
    adventure: Option<Res<super::commander::adventure::ClientPlayMode>>,
    scroll: Res<AccumulatedMouseScroll>,
    mut wheel_acc: crate::session::SessionLocal<f32>,
) {
    let wheel_acc = wheel_acc.get();
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
    // A client-local overlay (dev panel, chat) owns the pointer: its clicks and
    // the mouse travel to reach it must not break blocks or turn the camera.
    // Releases still go through so a button held when it opened never sticks.
    // A modal game screen (the Esc menu, the map, the inventory) holds the
    // pointer the same way: its clicks must not mine or place.
    // So does a pointer given back (`CursorPolicy::released`): the click that
    // takes it again must not also break a block.
    let overlay_open = cursor.as_ref().is_some_and(|c| c.blocks_game() || (c.released && c.wants_grab()));
    let send = |ev: InputEvent| {
        let _ = game.0.send(GameEventKind::PlayerInput(player, ev));
    };

    // Behind the loading screen nothing the player does reaches the sim: no
    // key, click or mouse travel (the body would walk and turn in a world the
    // player cannot see yet). Only the window size below still goes through.
    let loading = cursor.is_some_and(|c| c.loading);
    if loading {
    } else if !commander {
        forward_body_input(&keys, &buttons, &motion, &time, &coalesce_window, coalescer, &window, overlay_open, send);
    } else if first_person {
        // E is the sim's inventory key in Adventure; elsewhere it is the
        // fps-cam toggle `first_person` owns. Q reaches the sim only here,
        // never in the commander (where it holds the tool wheel).
        let adventure = adventure.is_some_and(|m| m.adventure());
        let send_body = |ev: InputEvent| {
            if adventure || !matches!(ev, InputEvent::Key { key: game::Key::KeyE, .. }) {
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
        forward_body_input(&keys, &buttons, &motion, &time, &coalesce_window, coalescer, &window, overlay_open, send_body);
    }
    if let Ok((w, _)) = window.single() {
        let size = (w.physical_width(), w.physical_height());
        if *last_size != Some(size) {
            if last_size.is_some() {
                send(InputEvent::Resized { width: size.0, height: size.1 });
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
            send(InputEvent::Key { key, pressed: false });
        }
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
    let delta = if overlay_open || !pointer_ours { Vec2::ZERO } else { motion.delta };
    let pressing = !overlay_open && buttons.get_just_pressed().next().is_some();
    let window_len = if pressing { Duration::ZERO } else { coalesce_window.0 };
    if let Some(sum) = coalescer.accumulate(delta, time.delta(), window_len) {
        send(InputEvent::MouseMotion { dx: sum.x, dy: sum.y });
    }
    if !overlay_open {
        for b in buttons.get_just_pressed() {
            send(InputEvent::MouseButton { button: map_button(*b), pressed: true });
        }
    }
    for b in buttons.get_just_released() {
        send(InputEvent::MouseButton { button: map_button(*b), pressed: false });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Duration = Duration::from_millis(16);
    /// A frame at ~240fps: four of these fit in one 16ms window.
    const FRAME: Duration = Duration::from_millis(4);

    #[test]
    fn first_delta_after_idle_emits_immediately() {
        let mut c = MouseCoalescer::default();
        assert_eq!(
            c.accumulate(Vec2::new(3.0, -1.0), FRAME, W),
            Some(Vec2::new(3.0, -1.0)),
            "leading edge: a fresh coalescer must not withhold the first delta"
        );
    }

    #[test]
    fn deltas_inside_one_window_are_summed_into_one_emission() {
        let mut c = MouseCoalescer::default();
        // Leading edge consumes the first delta.
        assert_eq!(c.accumulate(Vec2::new(1.0, 0.0), FRAME, W), Some(Vec2::new(1.0, 0.0)));
        // Next three 4ms frames sit inside the 16ms window: withheld, summed.
        assert_eq!(c.accumulate(Vec2::new(2.0, 1.0), FRAME, W), None);
        assert_eq!(c.accumulate(Vec2::new(3.0, 1.0), FRAME, W), None);
        assert_eq!(c.accumulate(Vec2::new(4.0, 1.0), FRAME, W), None);
        // The fourth frame closes the window: one event carrying the sum.
        assert_eq!(
            c.accumulate(Vec2::new(5.0, 1.0), FRAME, W),
            Some(Vec2::new(14.0, 4.0)),
            "the withheld deltas must be summed, not dropped"
        );
    }

    #[test]
    fn idle_never_emits_however_long_it_waits() {
        let mut c = MouseCoalescer::default();
        for _ in 0..1000 {
            assert_eq!(
                c.accumulate(Vec2::ZERO, FRAME, W),
                None,
                "a still mouse must never produce a packet"
            );
        }
    }

    #[test]
    fn idle_then_motion_still_emits_immediately() {
        let mut c = MouseCoalescer::default();
        assert_eq!(c.accumulate(Vec2::new(1.0, 0.0), FRAME, W), Some(Vec2::new(1.0, 0.0)));
        // Long pause: the window fully elapses while the mouse is still.
        for _ in 0..10 {
            assert_eq!(c.accumulate(Vec2::ZERO, FRAME, W), None);
        }
        // Motion resumes: leading edge again, no waiting.
        assert_eq!(
            c.accumulate(Vec2::new(7.0, 7.0), FRAME, W),
            Some(Vec2::new(7.0, 7.0)),
            "a pause must re-arm the leading edge, not add latency"
        );
    }

    /// Idling accrues time against whatever window is in force, but the armed
    /// state must not be capped by it: a still mouse stays armed, so raising the
    /// window later cannot swallow the next leading edge.
    #[test]
    fn idling_under_a_short_window_still_arms_a_longer_one() {
        let mut c = MouseCoalescer::default();
        for _ in 0..10 {
            assert_eq!(c.accumulate(Vec2::ZERO, FRAME, W), None);
        }
        let longer = Duration::from_secs(3600);
        assert_eq!(
            c.accumulate(Vec2::new(2.0, 3.0), FRAME, longer),
            Some(Vec2::new(2.0, 3.0)),
            "an idle coalescer must stay armed regardless of the window it idled under"
        );
    }

    #[test]
    fn burst_conserves_the_sum_and_cuts_the_packet_count() {
        let mut c = MouseCoalescer::default();
        let mut emitted = Vec::new();
        // 240 frames at 240fps = 1s of continuous motion.
        for _ in 0..240 {
            if let Some(sum) = c.accumulate(Vec2::new(1.0, 0.5), FRAME, W) {
                emitted.push(sum);
            }
        }
        let mid_burst = emitted.len();

        // The burst ends mid-window, so its last few frames are still pending.
        // They must not be stranded: idle frames keep advancing the window, and
        // the tail goes out when it expires.
        for _ in 0..4 {
            if let Some(sum) = c.accumulate(Vec2::ZERO, FRAME, W) {
                emitted.push(sum);
            }
        }
        assert_eq!(
            emitted.len(),
            mid_burst + 1,
            "the tail of a burst must be flushed exactly once after motion stops"
        );

        let total: Vec2 = emitted.iter().copied().sum();
        assert_eq!(
            total,
            Vec2::new(240.0, 120.0),
            "conservation: emitted deltas must sum to the injected deltas"
        );
        // One leading edge + one per 16ms window over 960ms of frames, + the tail.
        assert!(
            (55..=65).contains(&emitted.len()),
            "expected ~60 packets for 240 frames, got {}",
            emitted.len()
        );
    }

    #[test]
    fn zero_window_reproduces_todays_per_frame_behavior() {
        let mut c = MouseCoalescer::default();
        for _ in 0..10 {
            assert_eq!(
                c.accumulate(Vec2::new(1.0, 1.0), FRAME, Duration::ZERO),
                Some(Vec2::new(1.0, 1.0)),
                "a zero window must emit every frame with motion (the pre-change behavior)"
            );
        }
    }
}
