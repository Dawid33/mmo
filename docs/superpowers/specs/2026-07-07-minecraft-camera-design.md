# Minecraft-style Camera — Design

**Date:** 2026-07-07
**Status:** Approved, pending implementation plan

## Problem

The camera feels sluggish and behaves oddly during turns. Moving the mouse does
not translate accurately or snappily into look direction, and the camera visibly
tilts/un-tilts ("straightens along the horizon") while turning.

### Root causes (traced in code)

Two independent smoothing stages are stacked, plus input-handling choices that
fight responsiveness:

1. **Render-side rotation slerp** — `crates/client/src/renderer/interpolate.rs`
   slerps the render camera toward the sim pose at `smoothing: 0.1` (10%/frame)
   with `rot_snap: 0.001` (`crates/client/src/renderer/bridge.rs:51`,
   `SimTarget::camera`). This is the dominant lag: the visible camera perpetually
   chases the sim pose and almost never snaps. Slerp between full orientation
   quaternions follows the shortest arc, which introduces **transient roll**
   during a turn — the tilt artifact the user reported.

2. **Sim-side look** — `crates/game/src/camera.rs:203-240`, runs at the 50 Hz
   tick:
   - Mouse delta is **gated** (`diff.0.abs() > 2.0`): small movements are dropped,
     so slow precise aim feels dead.
   - Delta is **hard-clamped to ±0.2 rad/tick**: fast flicks are truncated.
   - Rotation is a **free-accumulated rigidbody quaternion** with **no pitch clamp**
     (can flip over the top) and **no yaw/pitch decomposition** (roll can drift in).
   - Magic sensitivity constants (`0.00136`, `0.0008`, `+0.001`).

### How Minecraft does it (reference)

Camera orientation is two scalars, `yaw` and `pitch` (degrees). Every frame:
`yaw += dx * sens; pitch += dy * sens; pitch = clamp(pitch, -90, 90)`. No roll
ever — the view matrix is rebuilt from yaw/pitch so the up vector is always
world-up. Look is applied instantly, no smoothing. Sensitivity uses a cubic
response curve. Position may interpolate; the look never does.

## Constraints

- **Look must stay in the sim.** The look quaternion is authoritative rollback
  state and, in the future, the player character's head will rotate according to
  it. It cannot move client-side.
- Rollback correctness bar is unchanged: `hash(before) == hash(after undo)`,
  bit-exact. New scalar state must roll back with everything else.
- `game` stays engine-agnostic (no Bevy).

## Design

### Sim look model — `crates/game/src/camera.rs` + `Camera` in `state.rs`

- Add `yaw: Real` and `pitch: Real` scalar fields to the `Camera` struct
  (`Real = OrderedFloat<f32>` for deterministic hashing). These are the single
  logical source of truth for look; the rigidbody quaternion is *derived* from
  them each tick. They are rollback-tracked like the existing `proj_matrix`
  mutations (via `ecs.camera.undo_scope()` + `register` + emit), so they roll
  back consistently with the derived body rotation.
- Each tick, from raw mouse delta `(dx, dy)`:
  - `yaw += curve(dx)` — wraps freely, no clamp.
  - `pitch = clamp(pitch + curve(dy), -PITCH_LIMIT, +PITCH_LIMIT)` where
    `PITCH_LIMIT = FRAC_PI_2 - EPS`.
  - **Sensitivity: Minecraft cubic curve**, as named tunable constants:
    `let f = SENS * 0.6 + 0.2; let mult = f * f * f * 8.0;`
    `delta = px * mult * BASE_RAD`. Separate X/Y base constants so horizontal and
    vertical sensitivity can differ. `SENS`, `BASE_RAD_X`, `BASE_RAD_Y`,
    `PITCH_LIMIT` are named consts (no UI to change them yet, but exposed as
    tunables per project convention).
  - The `abs > 2.0` gate and the `±0.2` per-tick clamp are **deleted**. Even a
    1px move rotates; a fast flick rotates fully in one tick.
- Rebuild the body rotation only when yaw or pitch changed this tick (keep the
  existing change-guard so idle ticks don't wake/churn the kinematic body):
  `rot = yaw_quat(yaw) * pitch_quat(pitch)`, applied via
  `set_next_kinematic_rotation`. Roll is structurally impossible.
- **Movement uses yaw only.** Transform the WASD `linvel` by `yaw_quat(yaw)`
  (not the full body rotation), then zero Y as today. This stops walk speed from
  shrinking when looking up/down — matching Minecraft, where movement is purely
  horizontal regardless of pitch.

### Body carries the full look (decision)

The rigidbody rotation continues to carry the **full** look quaternion
(`yaw_quat * pitch_quat`), exactly as today. `crates/game/src/physics.rs:57`
still emits `b.position()` as the camera view isometry, unchanged — so the render
camera inherits the full look with no new plumbing.

Rationale: the head bone that will consume this quaternion does not exist yet, so
splitting the body (yaw-only) from the head/camera (yaw·pitch) now would add
plumbing to `physics.rs` with no consumer. YAGNI: keep full-look-on-body until the
head bone lands, then revisit whether the physics collider should be yaw-only.

### Render side — `crates/client/src/renderer/bridge.rs` + `interpolate.rs`

- `SimTarget::camera` gets `smoothing: 1.0` (snap thresholds become irrelevant).
  At `t = 1.0`, `lerp`/`slerp` return the target exactly, so the render camera's
  **position and rotation both snap** to the latest sim pose every frame. The
  `interpolate.rs` logic needs no change — it already returns the target at
  `t = 1.0`. The tilt artifact disappears because the render camera never takes a
  slerp arc; it holds each 50 Hz sim pose until the next arrives.

### Behavioral note on refresh rate

Because look stays sim-authoritative at 50 Hz, look updates step at 20 ms. This
is far snappier than the current perpetual 10%/frame chase (which added hundreds
of ms of visual lag). Per-frame client-side prediction to smooth 120 Hz+ monitors
is explicitly out of scope; if 50 Hz stepping ever feels rough, two-snapshot
interpolation with tick timestamps is the future upgrade path (noted in
`interpolate.rs`).

## Testing

**`game` (`cargo test -p game`):**
- Pitch clamp: repeated upward deltas never push pitch past `+PITCH_LIMIT`; same
  for down.
- No-roll invariant: for any (yaw, pitch), the rebuilt quaternion has zero roll
  (its local up projected onto the view has no lateral tilt / world-up stays up).
- Yaw wrap: large accumulated yaw stays finite and continuous.
- Cubic-curve mapping: `curve(px)` matches the documented Minecraft formula for
  sample inputs.
- Rollback bar: `hash(before) == hash(after undo)` holds with the new `yaw`/`pitch`
  fields exercised (mouse-look tick then undo).

**`client` (`cargo test -p client`):**
- Camera `SimTarget` with `smoothing: 1.0` snaps to the target pose in a single
  frame (position and rotation), no multi-frame convergence.

## Out of scope

- Client-side per-frame look prediction / two-snapshot rotation interpolation.
- Splitting body (yaw-only) from head/camera (yaw·pitch).
- Any settings UI for sensitivity (constants only for now).
