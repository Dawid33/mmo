# Mouse-Delta Coalescing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cap client→server `MouseMotion` packets at one per configurable time window instead of one per rendered frame, without changing simulation output.

**Architecture:** A `MouseCoalescer` accumulator lives as a `Local` inside the existing `forward_input` Bevy system (`crates/client/src/renderer/input.rs`). It sums `AccumulatedMouseMotion` across frames and emits a single `InputEvent::MouseMotion` when a window (`MouseCoalesceWindow`, defaulting to the `MOUSE_COALESCE_MS` constant) has elapsed since the last emission. The emit decision is a pure method taking explicit `delta`/`dt`/`window`, so it unit-tests without a Bevy `App`. Coalescing happens *before* the event enters the game-event channel, so the client-predicted and server-applied event streams stay identical and rollback is unaffected.

**Tech Stack:** Rust, Bevy 0.18 (`Local`, `Res<Time>`, `Resource`), the existing headless `BevyInputHarness`.

**Spec:** `docs/superpowers/specs/2026-07-11-mouse-delta-coalescing-design.md`

## Global Constraints

- All production changes are confined to `crates/client/src/renderer/input.rs` and one `init_resource` line in `crates/client/src/renderer/mod.rs`. **Do not** touch `crates/game`, `crates/server`, or `crates/client/src/netcode*.rs`.
- Timing must come from Bevy's `Res<Time>` (`time.delta()`), **never** `std::time::Instant` — the client also builds for `wasm32-unknown-unknown`.
- `MOUSE_COALESCE_MS = 16` is the single source of truth for the production window; `MouseCoalesceWindow`'s `Default` reads it.
- The window must stay well under `game::TICK_RATE` (currently 200 ms). The sim sums mouse deltas and drains them once per tick (`InputState::step`), so a sub-tick window is simulation-neutral.
- Only `MouseMotion` is coalesced. Key edges, mouse-button edges, wheel, and resize events keep their existing one-event-per-occurrence behavior.
- Tests measured slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]` (repo convention). The unit tests in Task 1 are microsecond-scale and must NOT be tagged; the harness tests in Task 2 spin up a local server and MUST be tagged, like every other test in `crates/client/tests/bevy_input.rs`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/client/src/renderer/input.rs` (modify) | Adds `MOUSE_COALESCE_MS`, `MouseCoalesceWindow`, `MouseCoalescer`, its `#[cfg(test)] mod tests`, and the `forward_input` wiring. |
| `crates/client/src/renderer/mod.rs` (modify, ~line 73) | Registers `MouseCoalesceWindow` as a resource on the renderer plugin. |
| `crates/client/src/bevy_harness.rs` (modify) | Adds mouse-motion injection, per-frame motion clearing, a window setter, and a `MouseMotion` inspector to `BevyInputHarness`. |
| `crates/client/tests/bevy_input.rs` (modify) | Adds the two end-to-end coalescing tests. |

---

### Task 1: The `MouseCoalescer` accumulator (pure, unwired)

Builds the type and its tests in isolation. Nothing calls it yet, so `forward_input` behavior is unchanged and the client still compiles and runs exactly as before.

**Files:**
- Modify: `crates/client/src/renderer/input.rs` (append the type + a `#[cfg(test)] mod tests` at the end of the file)

**Interfaces:**
- Consumes: nothing.
- Produces, for Task 2:
  - `pub const MOUSE_COALESCE_MS: u64 = 16;`
  - `pub struct MouseCoalesceWindow(pub Duration);` — a Bevy `Resource`, `Default` = `Duration::from_millis(MOUSE_COALESCE_MS)`
  - `pub(crate) struct MouseCoalescer` with `Default`, and
    `pub(crate) fn accumulate(&mut self, delta: Vec2, dt: Duration, window: Duration) -> Option<Vec2>`

- [ ] **Step 1: Write the failing tests**

Append to the END of `crates/client/src/renderer/input.rs`:

```rust
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
        let total: Vec2 = emitted.iter().copied().sum();
        assert_eq!(
            total,
            Vec2::new(240.0, 120.0),
            "conservation: emitted deltas must sum to the injected deltas"
        );
        // One leading edge + one per 16ms window over 960ms of frames.
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p client mouse 2>&1 | tail -20`
Expected: FAIL to compile — `cannot find type MouseCoalescer in this scope` (and `Duration` unresolved). Compile failure IS the failing-test state here.

- [ ] **Step 3: Write the minimal implementation**

Add the `Duration` import at the top of `crates/client/src/renderer/input.rs`, alongside the existing imports:

```rust
use std::time::Duration;
```

Then insert this block AFTER `map_button` and BEFORE `forward_input`:

```rust
/// Minimum wall-clock spacing between `MouseMotion` packets. Deltas arriving
/// inside the window are summed and emitted when it expires.
///
/// Must stay well under `game::TICK_RATE`: the sim sums mouse deltas and drains
/// them once per tick (`InputState::step`), and the only consumer is the camera
/// controller, which reads that per-tick sum. So any window shorter than a tick
/// is simulation-neutral — it changes how many packets carry the same sum, not
/// the sum. Without it the packet rate is bound to the frame rate, and each
/// packet costs its own QUIC uni-stream.
pub const MOUSE_COALESCE_MS: u64 = 16;

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
        // Clamped at `window`: an idle mouse can neither drift the accumulator
        // nor overflow it, and it leaves `since_send` armed at exactly the
        // threshold, which is what gives the leading edge.
        self.since_send = self.since_send.saturating_add(dt).min(window);

        if self.pending == Vec2::ZERO || self.since_send < window {
            return None;
        }
        let sum = self.pending;
        self.pending = Vec2::ZERO;
        self.since_send = Duration::ZERO;
        Some(sum)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p client mouse 2>&1 | tail -20`
Expected: PASS — `test result: ok. 6 passed`.

If `cargo` warns that `MouseCoalescer`/`MouseCoalesceWindow` are never used outside tests, that is expected at this task boundary; Task 2 wires them in. Do not add `#[allow(dead_code)]` to silence it — just proceed.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/input.rs
git commit -m "feat(client): MouseCoalescer — sum mouse deltas over a time window

Pure accumulator with a leading edge (first delta after idle emits
immediately, then rate-limited to one emission per window). Not wired in
yet; forward_input still emits per frame.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Wire the coalescer into `forward_input`

Puts the accumulator on the real input path, registers the window resource, and proves end-to-end through the headless Bevy harness that the packet rate drops while the deltas are conserved.

**Files:**
- Modify: `crates/client/src/renderer/input.rs` (the `forward_input` signature + the `MouseMotion` arm at lines 65-67)
- Modify: `crates/client/src/renderer/mod.rs` (the `init_resource` chain, next to `.init_resource::<voxel_pull::RenderPath>()` at ~line 73)
- Modify: `crates/client/src/bevy_harness.rs` (`BevyInputHarness`: new field, resource registration, `clear_input_edges`, `drain_tap`, plus new entry points and an inspector)
- Test: `crates/client/tests/bevy_input.rs` (append two tests)

**Interfaces:**
- Consumes from Task 1: `MOUSE_COALESCE_MS`, `MouseCoalesceWindow(pub Duration)`, `MouseCoalescer::default()`, `MouseCoalescer::accumulate(Vec2, Duration, Duration) -> Option<Vec2>`.
- Produces (harness API used by the tests):
  - `BevyInputHarness::set_mouse_coalesce_window(&mut self, window: Duration)`
  - `BevyInputHarness::move_mouse(&mut self, delta: Vec2)`
  - `BevyInputHarness::mouse_motions(&self) -> &[(f32, f32)]`

- [ ] **Step 1: Write the failing tests**

First, extend the harness. In `crates/client/src/bevy_harness.rs`:

Add the import next to the existing ones:

```rust
use std::time::Duration;
```

Add a field to the `BevyInputHarness` struct, after `e_forwarded`:

```rust
    /// Every `InputEvent::MouseMotion` `forward_input` emitted, in order.
    mouse_motions: Vec<(f32, f32)>,
```

In `BevyInputHarness::new`, register the window resource — add this line to the
`app` builder chain, right after `.init_resource::<AccumulatedMouseMotion>()`:

```rust
            .init_resource::<crate::renderer::input::MouseCoalesceWindow>()
```

...and add the new field to the struct literal at the end of `new`, so it reads:

```rust
        Self {
            app,
            server,
            client,
            server_to_client,
            game_tap: tap_recv,
            e_forwarded: 0,
            mouse_motions: Vec::new(),
            window,
        }
```

Record motion events in `drain_tap` — add a second `if let` inside the existing
`while` loop, after the `KeyE` counter:

```rust
            if let GameEventKind::PlayerInput(_, InputEvent::MouseMotion { dx, dy }) = ev {
                self.mouse_motions.push((dx, dy));
            }
```

Clear the per-frame motion in `clear_input_edges`, so injected motion lasts
exactly one `app.update()` — real Bevy's `InputPlugin` recomputes
`AccumulatedMouseMotion` every frame, and `MinimalPlugins` does not:

```rust
    fn clear_input_edges(&mut self) {
        self.app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        self.app.world_mut().resource_mut::<ButtonInput<MouseButton>>().clear();
        *self.app.world_mut().resource_mut::<AccumulatedMouseMotion>() =
            AccumulatedMouseMotion::default();
    }
```

Add the entry points next to `press_key`/`release_key`:

```rust
    /// Inject one frame's worth of raw mouse motion. Consumed by the next
    /// `step()` and cleared afterwards, so one injection == one frame.
    pub fn move_mouse(&mut self, delta: Vec2) {
        self.app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = delta;
    }

    /// Override the coalescing window. `Duration::ZERO` = emit every frame.
    pub fn set_mouse_coalesce_window(&mut self, window: Duration) {
        self.app
            .world_mut()
            .resource_mut::<crate::renderer::input::MouseCoalesceWindow>()
            .0 = window;
    }
```

And the inspector, next to `e_forwarded_count`:

```rust
    /// Every `MouseMotion` delta `forward_input` emitted, in order.
    pub fn mouse_motions(&self) -> &[(f32, f32)] {
        &self.mouse_motions
    }
```

Now append the tests to `crates/client/tests/bevy_input.rs` (add
`use bevy::math::Vec2;` and `use std::time::Duration;` to its imports):

```rust
/// The pre-change contract: with the window collapsed, every frame with motion
/// still produces exactly one MouseMotion event. Guards against the coalescer
/// dropping deltas outright.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn zero_window_emits_one_motion_event_per_frame() {
    let mut h = BevyInputHarness::new();
    h.connect();
    h.set_mouse_coalesce_window(Duration::ZERO);

    for i in 0..4 {
        h.move_mouse(Vec2::new(1.0 + i as f32, -1.0));
        h.step();
    }

    assert_eq!(
        h.mouse_motions(),
        &[(1.0, -1.0), (2.0, -1.0), (3.0, -1.0), (4.0, -1.0)],
        "a zero window must forward every frame's delta untouched"
    );
}

/// The point of the change: a window longer than the test withholds every
/// delta after the leading edge, and the withheld motion is summed rather than
/// lost — so N frames of motion cost 1 packet, not N.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn long_window_coalesces_frames_into_one_packet() {
    let mut h = BevyInputHarness::new();
    h.connect();
    h.set_mouse_coalesce_window(Duration::from_secs(3600));

    for _ in 0..8 {
        h.move_mouse(Vec2::new(2.0, 3.0));
        h.step();
    }

    assert_eq!(
        h.mouse_motions(),
        &[(2.0, 3.0)],
        "only the leading edge escapes a window longer than the test; the other \
         7 frames must be held, summed, and awaiting the window's expiry"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p client --test bevy_input --include-ignored 2>&1 | tail -20`
Expected: FAIL. `zero_window_emits_one_motion_event_per_frame` passes trivially (today's code already emits per frame), but `long_window_coalesces_frames_into_one_packet` FAILS with a left/right mismatch showing 8 emitted deltas instead of 1 — `forward_input` does not consult the window yet.

- [ ] **Step 3: Write the minimal implementation**

In `crates/client/src/renderer/input.rs`, add three parameters to `forward_input`:

```rust
pub fn forward_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    coalesce_window: Res<MouseCoalesceWindow>,
    mut coalescer: Local<MouseCoalescer>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut last_size: Local<Option<(u32, u32)>>,
    player: Res<LocalPlayer>,
    game: Res<GameEvents>,
) {
```

and replace the `MouseMotion` arm (currently lines 65-67):

```rust
    if motion.delta != Vec2::ZERO {
        send(InputEvent::MouseMotion { dx: motion.delta.x, dy: motion.delta.y });
    }
```

with:

```rust
    // Coalesce HERE, at the source — not at the send boundary. Each event is
    // predicted locally and replayed by the server, so emitting N events the
    // server sees as 1 would give the two divergent logs and break rollback.
    if let Some(sum) = coalescer.accumulate(motion.delta, time.delta(), coalesce_window.0) {
        send(InputEvent::MouseMotion { dx: sum.x, dy: sum.y });
    }
```

Then register the resource in `crates/client/src/renderer/mod.rs` — add this
line to the plugin's `init_resource` chain, immediately after
`.init_resource::<voxel_pull::RenderPath>()`:

```rust
            .init_resource::<input::MouseCoalesceWindow>()
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p client --test bevy_input --include-ignored 2>&1 | tail -20`
Expected: PASS — `test result: ok. 5 passed` (the 3 pre-existing tests plus the 2 new ones).

Then the whole client suite, to confirm nothing else regressed:

Run: `cargo test -p client 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/input.rs crates/client/src/renderer/mod.rs \
        crates/client/src/bevy_harness.rs crates/client/tests/bevy_input.rs
git commit -m "feat(client): coalesce mouse deltas over a 16ms window

Mouse packets were frame-rate-bound: one InputEvent::MouseMotion per
frame with motion, each its own QUIC uni-stream (~29 per 200ms tick at
144fps). The sim sums deltas and drains them once per tick, so those
extra packets carried no information. forward_input now sums them over
MOUSE_COALESCE_MS, emitting the first delta after idle immediately and
rate-limiting from there.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final Verification

- [ ] **Full suite:** `just test-full` — expect PASS (this is the repo's declare-done bar).
- [ ] **Lint:** `just clippy` — expect no new warnings.
- [ ] **Live check:** `just adapter=6700 run`, grab the cursor with `E`, and swing the mouse. Mouse look must feel unchanged. The client logs one `[client] predicted PlayerInput` line per forwarded event (`crates/client/src/instance.rs:372`), so `just adapter=6700 run 2>&1 | grep -c MouseMotion`-style counting over a fixed swing confirms the rate dropped to ~62/s from frame-rate-bound. Confirm the `AdapterInfo` line names the RX 6700 XT before trusting any frame-rate number.
