# Mouse-Delta Coalescing (client → server)

**Date:** 2026-07-11
**Status:** Design approved, ready for implementation plan
**Scope:** `crates/client/src/renderer/input.rs` only

## Problem

`forward_input` emits one `InputEvent::MouseMotion` per rendered frame with
non-zero motion (`crates/client/src/renderer/input.rs:65`):

```rust
if motion.delta != Vec2::ZERO {
    send(InputEvent::MouseMotion { dx: motion.delta.x, dy: motion.delta.y });
}
```

Each such event is predicted locally and immediately handed to the netcode
(`crates/client/src/instance.rs:376`), which opens a **fresh QUIC
unidirectional stream per packet** (`crates/client/src/netcode.rs:90-96`).

So the *frame rate*, not the tick rate, sets the mouse packet rate. At 144 fps
with `TICK_RATE = 200` (ms), continuous mouse movement sends ~29 packets — and
29 stream open/write/finish cycles — per simulation tick.

Those extra packets buy nothing. The sim sums mouse deltas
(`InputState::update`, `crates/game/src/input.rs:103-112`) and drains the sum
once per tick (`InputState::step`), and the only consumer is the camera
controller (`crates/game/src/camera.rs:263`), which reads that per-tick sum. N
motion events between two ticks produce exactly the same yaw/pitch as one event
carrying their sum.

## Design

Coalesce at the source: accumulate `AccumulatedMouseMotion` inside
`forward_input` and emit at most one `MouseMotion` event per configurable
window.

### Why at the source, not at the send boundary

The client *predicts* each `PlayerInput` locally and sends the same event to the
server, which replays it. Coalescing only on the wire would make the client
apply N events where the server applies 1 — divergent event logs, and the
rollback's `hash(before) == hash(after undo)` bar would be violated. Coalescing
before the event enters the game-event channel keeps the predicted and
authoritative streams byte-identical. Nothing in `game`, `server`, or the
netcode changes.

### Behavior neutrality

A window shorter than the tick period is simulation-neutral by the argument
above: the sim sums the deltas either way. The constant is documented with that
constraint so a future tick-rate change is evaluated against it.

### The tunable

```rust
/// Minimum wall-clock spacing between `MouseMotion` packets. Deltas arriving
/// inside the window are summed and emitted when it expires. Must stay well
/// under `game::TICK_RATE`: the sim sums mouse deltas and drains them once per
/// tick (`InputState::step`), so any window shorter than a tick is
/// simulation-neutral — it only changes how many packets carry the same sum.
pub const MOUSE_COALESCE_MS: u64 = 16;
```

Exposed to Bevy as a resource so tests can vary it without a rebuild, with the
constant as its `Default` — the single source of truth for production:

```rust
#[derive(Resource, Debug, Clone, Copy)]
pub struct MouseCoalesceWindow(pub Duration);

impl Default for MouseCoalesceWindow {
    fn default() -> Self {
        Self(Duration::from_millis(MOUSE_COALESCE_MS))
    }
}
```

### The mechanism

A `MouseCoalescer` held as a `Local` in `forward_input`, with the decision
factored into a pure method so it is testable without a Bevy `App`:

```rust
pub(crate) struct MouseCoalescer {
    pending: Vec2,
    since_send: Duration,
}

impl Default for MouseCoalescer {
    fn default() -> Self {
        // Saturated: the first delta of the session emits immediately, like any
        // other delta arriving after an idle period.
        Self { pending: Vec2::ZERO, since_send: Duration::MAX }
    }
}

impl MouseCoalescer {
    /// Fold this frame's motion in; return `Some(sum)` on the frames where an
    /// event should be emitted.
    pub(crate) fn accumulate(
        &mut self,
        delta: Vec2,
        dt: Duration,
        window: Duration,
    ) -> Option<Vec2>;
}
```

Per frame:

1. `pending += delta`.
2. `since_send = since_send.saturating_add(dt).min(window)` — clamped, so an
   idle mouse cannot drift the accumulator or overflow it.
3. If `pending != Vec2::ZERO && since_send >= window`: emit `Some(pending)`,
   then reset `pending = Vec2::ZERO` and `since_send = Duration::ZERO`.

**Leading edge.** Because `since_send` sits at `window` while the mouse is idle
(and starts there), the first motion after a pause satisfies the emit condition
on the very frame it arrives — no added latency on the initial flick. Continuous
motion then settles into one packet per window.

**Timing source.** `Res<Time>` (`time.delta()`), not `std::time::Instant` —
works unchanged on `wasm32` and under `MinimalPlugins` in the headless harness.

A window of `Duration::ZERO` degenerates to exactly today's behavior (emit every
frame with non-zero motion), which the tests exploit.

## Testing

**Unit tests on `accumulate`** (pure, deterministic `dt`, no Bevy `App`):

- Deltas inside one window sum into a single emission carrying their sum.
- A delta arriving after an idle period ≥ window emits on the same call
  (leading edge).
- Zero motion never emits, no matter how much time passes.
- Conservation: over a synthetic 240 fps burst, the sum of all emitted deltas
  equals the sum of all injected deltas, and the emission count is far below the
  frame count.
- `window == ZERO` emits every non-zero frame (today's behavior).

**Harness test** (`bevy_harness.rs`, which already registers `forward_input`
and `AccumulatedMouseMotion`): drive `AccumulatedMouseMotion` over frames with
`MouseCoalesceWindow(ZERO)` and assert the game-event tap sees one
`MouseMotion` per frame — the no-regression path — and with a window longer than
the test, assert the deltas are withheld and summed rather than emitted per
frame. Both are wall-clock-independent, so no timing flake.

## Expected outcome

Steady-state mouse packets drop from frame-rate-bound (~144/s at 144 fps, each
its own QUIC uni-stream) to ~62/s, with up to `MOUSE_COALESCE_MS` of added
latency on the tail deltas of a continuous motion and none on the leading edge.
Simulation output is unchanged.

## Out of scope

- Batching multiple `ClientPacket`s onto one QUIC stream (a netcode-layer
  change, and a larger one).
- Coalescing any other input event kind. Key and button edges are discrete and
  must not be dropped or merged.
