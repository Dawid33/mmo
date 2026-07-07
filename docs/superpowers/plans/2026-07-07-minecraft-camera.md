# Minecraft-style Camera Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the camera follow the mouse snappily and roll-free, Minecraft-style, by storing look as clamped yaw/pitch scalars in the sim and snapping the render camera instead of lerping it.

**Architecture:** The sim's `Camera` gains authoritative `yaw`/`pitch` scalars (rollback-tracked). Each tick the `CameraController` accumulates raw mouse delta into them (Minecraft cubic sensitivity, pitch clamped, no roll), rebuilds the rigidbody's full-look quaternion, and drives WASD movement from yaw only. The render camera's `SimTarget` smoothing goes to `1.0` so it snaps to each sim pose, eliminating the slerp-arc tilt artifact.

**Tech Stack:** Rust, nalgebra/rapier (vendored forks), `ordered_float::OrderedFloat` (= `Real`), Bevy (client render), crc32fast (rollback hash).

## Global Constraints

- Look stays in the sim: `yaw`/`pitch` are rollback state; the body/head read the derived quaternion. Do NOT move look client-side.
- Rollback bar is bit-exact: `hash(before) == hash(after undo)` must hold with the new fields. All new sim floats are `Real = OrderedFloat<f32>` for deterministic hashing.
- `game` and `server` stay Bevy-free / windowing-free. Render-only changes live in `crates/client`.
- Tunables are named `pub const`s (no settings UI yet), per project convention.
- Tick rate is 50 Hz (`game::TICK_RATE`).

---

### Task 1: Sim look model — `Camera` fields + pure math helpers

Adds the `yaw`/`pitch` fields and the pure, unit-testable helper functions (`look_delta_rad`, `apply_look`, `yaw_quat`, `look_quat`) plus tunable constants. No controller wiring yet — that is Task 2. These helpers are `pub` and reachable as `game::*` (via `pub use camera::*` in `crates/game/src/lib.rs:34`).

**Files:**
- Modify: `crates/game/src/camera.rs` (add fields to `Camera`, init in `new`/`Default`, add consts + helpers)
- Test: `crates/game/tests/camera_look.rs` (create)

**Interfaces:**
- Produces:
  - `game::PITCH_LIMIT: Real` — max pitch magnitude (radians).
  - `game::LOOK_SENSITIVITY: f32`, `game::LOOK_BASE_X: f32`, `game::LOOK_BASE_Y: f32` — tunables.
  - `game::look_delta_rad(px: f32, base: f32) -> f32`
  - `game::apply_look(yaw: Real, pitch: Real, dx: f32, dy: f32) -> (Real, Real)`
  - `game::yaw_quat(yaw: Real) -> UnitQuaternion<Real>`
  - `game::look_quat(yaw: Real, pitch: Real) -> UnitQuaternion<Real>`
  - `Camera.yaw: Real`, `Camera.pitch: Real` (public fields).

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/camera_look.rs`:

```rust
use game::na::Vector3;
use game::parry::math::Real;
use game::na::UnitQuaternion;
use ordered_float::OrderedFloat;

#[test]
fn look_delta_matches_minecraft_cubic_formula() {
    // Pin the documented curve independent of the constant's value.
    let f = game::LOOK_SENSITIVITY * 0.6 + 0.2;
    let expected = 100.0 * game::LOOK_BASE_X * (f * f * f * 8.0);
    assert!((game::look_delta_rad(100.0, game::LOOK_BASE_X) - expected).abs() < 1e-6);
    // Zero pixels => zero rotation.
    assert_eq!(game::look_delta_rad(0.0, game::LOOK_BASE_X), 0.0);
}

#[test]
fn pitch_clamps_and_yaw_is_free() {
    let mut yaw = OrderedFloat(0.0_f32);
    let mut pitch = OrderedFloat(0.0_f32);
    // Slam pitch up hard, many ticks.
    for _ in 0..10_000 {
        let (y, p) = game::apply_look(yaw, pitch, 0.0, 10_000.0);
        yaw = y;
        pitch = p;
    }
    assert!(pitch <= game::PITCH_LIMIT, "pitch exceeded +limit: {}", pitch);
    assert!(pitch >= OrderedFloat(1.0), "pitch should reach near the top");
    // Now slam down; must clamp at -limit.
    for _ in 0..20_000 {
        let (y, p) = game::apply_look(yaw, pitch, 0.0, -10_000.0);
        yaw = y;
        pitch = p;
    }
    assert!(pitch >= -game::PITCH_LIMIT, "pitch exceeded -limit: {}", pitch);
    // Yaw accumulated freely (no clamp), stays finite.
    assert!(yaw.into_inner().is_finite());
}

#[test]
fn look_quat_never_rolls() {
    // The camera-right vector must stay horizontal for any yaw/pitch.
    for &yaw in &[0.0_f32, 0.7, 3.0, -2.1] {
        for &pitch in &[0.0_f32, 0.5, -1.4, 1.4] {
            let q = game::look_quat(OrderedFloat(yaw), OrderedFloat(pitch));
            let right = q.transform_vector(&Vector3::<Real>::x());
            assert!(
                right.y.into_inner().abs() < 1e-6,
                "roll detected at yaw={yaw} pitch={pitch}: right.y={}",
                right.y
            );
        }
    }
}

#[test]
fn look_quat_identity_at_zero() {
    let q = game::look_quat(OrderedFloat(0.0), OrderedFloat(0.0));
    assert_eq!(q, UnitQuaternion::identity());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p game --test camera_look`
Expected: FAIL to compile — `game::PITCH_LIMIT`, `game::look_quat`, etc. do not exist yet.

- [ ] **Step 3: Add fields and helpers to `camera.rs`**

In `crates/game/src/camera.rs`, add this import near the top (after the existing `use` lines):

```rust
use std::f32::consts::FRAC_PI_2;
```

Add these items just below the `pub const FOV_Y` line (around line 18):

```rust
/// Max pitch magnitude (radians), a hair under straight up/down to dodge
/// the gimbal singularity at the poles.
pub const PITCH_LIMIT: Real = OrderedFloat(FRAC_PI_2 - 0.001);
/// Minecraft-style look sensitivity in [0, 1]; tune to taste.
pub const LOOK_SENSITIVITY: f32 = 0.5;
/// Radians of yaw per pixel of mouse motion, before the sensitivity curve.
pub const LOOK_BASE_X: f32 = 0.0026;
/// Radians of pitch per pixel of mouse motion, before the sensitivity curve.
pub const LOOK_BASE_Y: f32 = 0.0026;

/// Minecraft cubic sensitivity: fine control near center, fast at the ends.
/// `f = sens*0.6 + 0.2; mult = f^3 * 8`, then `px * base * mult`.
pub fn look_delta_rad(px: f32, base: f32) -> f32 {
    let f = LOOK_SENSITIVITY * 0.6 + 0.2;
    let mult = f * f * f * 8.0;
    px * base * mult
}

/// Accumulate a mouse delta into (yaw, pitch). Yaw wraps freely; pitch is
/// clamped to +/-PITCH_LIMIT. Non-finite deltas are ignored so a stray NaN
/// cannot corrupt the look. Sign convention matches the previous controller
/// (rightward/downward mouse decreases the angle).
pub fn apply_look(yaw: Real, pitch: Real, dx: f32, dy: f32) -> (Real, Real) {
    let dx = if dx.is_finite() { dx } else { 0.0 };
    let dy = if dy.is_finite() { dy } else { 0.0 };
    let new_yaw = yaw - Real::from(look_delta_rad(dx, LOOK_BASE_X));
    let new_pitch = clamp(
        pitch - Real::from(look_delta_rad(dy, LOOK_BASE_Y)),
        -PITCH_LIMIT,
        PITCH_LIMIT,
    );
    (new_yaw, new_pitch)
}

/// World-Y yaw only — the horizontal facing used for movement direction.
pub fn yaw_quat(yaw: Real) -> UnitQuaternion<Real> {
    UnitQuaternion::from_axis_angle(&Vector3::y_axis(), yaw)
}

/// Full look orientation: yaw about world-Y, then pitch about local-X.
/// Roll is structurally impossible (the X axis is invariant under its own
/// rotation, so the camera-right vector stays horizontal).
pub fn look_quat(yaw: Real, pitch: Real) -> UnitQuaternion<Real> {
    yaw_quat(yaw) * UnitQuaternion::from_axis_angle(&Vector3::x_axis(), pitch)
}
```

Add the two fields to the `Camera` struct (after `pub view_matrix: Option<RigidBodyHandle>,`):

```rust
    pub yaw: Real,
    pub pitch: Real,
```

In `Camera::new`, add to the returned struct literal (alongside `view_matrix: Some(handle),`):

```rust
            yaw: OrderedFloat(0.0),
            pitch: OrderedFloat(0.0),
```

In `impl Default for Camera`, add to the returned struct literal (alongside `view_matrix: Default::default(),`):

```rust
            yaw: OrderedFloat(0.0),
            pitch: OrderedFloat(0.0),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p game --test camera_look`
Expected: PASS (4 tests).

- [ ] **Step 5: Confirm the crate still builds (struct field additions ripple into rollback/serde)**

Run: `cargo build -p game`
Expected: builds clean. If the `#[rollback]`/`borrow::Partial` derives complain, the new fields are `Real` (already `Serialize`/`Hash`/`Clone`) so no extra bounds are needed — re-read the error, but do not change field types away from `Real`.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/camera.rs crates/game/tests/camera_look.rs
git commit -m "feat(camera): yaw/pitch look scalars + minecraft sensitivity helpers"
```

---

### Task 2: Wire the controller — scalar look, yaw-only movement, no gate/clamp

Rewrites the look and movement sections of `CameraController::on_tick` to use the Task 1 helpers: read `yaw`/`pitch` from the `Camera` component, move horizontally by `yaw_quat`, accumulate mouse into the scalars (undo-tracked), and rebuild the body's full-look rotation. Deletes the `abs > 2.0` gate and the `±0.2` per-tick clamp.

**Files:**
- Modify: `crates/game/src/camera.rs` (the `on_tick` body, roughly lines 128-240)
- Test: `crates/game/tests/camera_controller.rs` (create)

**Interfaces:**
- Consumes: `game::apply_look`, `game::yaw_quat`, `game::look_quat` (Task 1); `Camera.yaw`/`Camera.pitch` (Task 1); existing `client.input.mouse_diff() -> (f32, f32)` (`crates/game/src/input.rs:137`).
- Produces: after a tick with mouse motion, `region.data().ecs.camera.get(e).{yaw,pitch}` reflect the accumulated look, and the body's rotation equals `look_quat(yaw, pitch)`.

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/camera_controller.rs`:

```rust
use game::na::Vector3;
use game::parry::math::Real;
use game::{GameEventKind, InputEvent, Key, Region, RegionCoords};
use ordered_float::OrderedFloat;
use std::hash::Hash;

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.hash(&mut h);
    h.finalize()
}

/// One region, one player, FPS-cam enabled, positioned by create_player_safe.
fn region_fps_ready() -> (Region, game::EntityKey) {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(GameEventKind::CreateClient(7)).unwrap();
    // Enable FPS cam: KeyE press, then a Tick toggles it on (controllers run
    // before the toggle on this tick, so no look happens yet).
    region
        .handle_event(GameEventKind::PlayerInput(
            7,
            InputEvent::Key { key: Key::KeyE, pressed: true },
        ))
        .unwrap();
    region.handle_event(GameEventKind::Tick).unwrap();
    let e = *region.data().player_entites.get(&7).unwrap();
    (region, e)
}

#[test]
fn mouse_tick_accumulates_look_and_rebuilds_body_rotation() {
    let (mut region, e) = region_fps_ready();
    region
        .handle_event(GameEventKind::PlayerInput(
            7,
            InputEvent::MouseMotion { dx: 40.0, dy: -25.0 },
        ))
        .unwrap();
    region.handle_event(GameEventKind::Tick).unwrap();

    let cam = region.data().ecs.camera.get(e).clone();
    assert_ne!(cam.yaw, OrderedFloat(0.0), "yaw should accumulate");
    assert_ne!(cam.pitch, OrderedFloat(0.0), "pitch should accumulate");

    // Body's post-step rotation equals the derived full-look quaternion.
    let handle = *region.data().ecs.rigidbody.get(e);
    let body_rot = region.data().physics.bodies.get(handle).unwrap().rotation().clone();
    let expected = game::look_quat(cam.yaw, cam.pitch);
    assert!(
        body_rot.angle_to(&expected).into_inner() < 1e-5,
        "body rotation must match look_quat(yaw,pitch)"
    );
    // No roll: camera-right stays horizontal.
    let right = body_rot.transform_vector(&Vector3::<Real>::x());
    assert!(right.y.into_inner().abs() < 1e-5, "body look must not roll");
}

#[test]
fn look_ticks_roll_back_bit_exact() {
    let (mut region, _e) = region_fps_ready();
    // Baseline hash, then a sequence of one-event-per-transaction steps.
    let mut hashes = vec![crc(&region)];
    let events = [
        GameEventKind::PlayerInput(7, InputEvent::MouseMotion { dx: 40.0, dy: -25.0 }),
        GameEventKind::Tick,
        GameEventKind::PlayerInput(7, InputEvent::MouseMotion { dx: -15.0, dy: 30.0 }),
        GameEventKind::Tick,
        GameEventKind::PlayerInput(7, InputEvent::MouseMotion { dx: 5.0, dy: -5.0 }),
        GameEventKind::Tick,
    ];
    for ev in events {
        region.handle_event(ev).unwrap();
        hashes.push(crc(&region));
    }
    // Roll back each transaction; every boundary hash must be restored.
    for expected in hashes.iter().rev().skip(1) {
        region.rollback_last_event();
        assert_eq!(*expected, crc(&region), "rollback hash mismatch");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p game --test camera_controller`
Expected: `mouse_tick_accumulates_look_and_rebuilds_body_rotation` FAILS — with the old controller, `cam.yaw`/`cam.pitch` stay 0 (look lives only on the body), so the `assert_ne!` fires. (`look_ticks_roll_back_bit_exact` may pass against the old code; that's fine — it guards the new path.)

- [ ] **Step 3: Rewrite the movement + look sections of `on_tick`**

In `crates/game/src/camera.rs`, inside `on_tick`, after `if !client.fps_cam_mode { continue; }`:

Replace the block that starts at `let handle = ecs.rigidbody.get(e_id).clone();` and originally read:

```rust
            let handle = ecs.rigidbody.get(e_id).clone();
            let b = data.physics.bodies.get(handle).unwrap();
            let rotation = b.rotation();
```

with (capture the look scalars, drop the stale `rotation` read):

```rust
            let handle = ecs.rigidbody.get(e_id).clone();
            // Look scalars are authoritative; the body quaternion is derived.
            let (yaw0, pitch0) = {
                let cam = ecs.camera.get(e_id);
                (cam.yaw, cam.pitch)
            };
            let b = data.physics.bodies.get(handle).unwrap();
```

Change the movement direction line (originally `let mut linvel = rotation.transform_vector(&linvel);`) to use yaw only:

```rust
            // Horizontal movement follows yaw only — walk speed stays full
            // regardless of pitch (Minecraft behavior).
            let mut linvel = crate::yaw_quat(yaw0).transform_vector(&linvel);
            linvel.y = Real::from(0.0);
```

Then replace the ENTIRE trailing look block — from `let b = data.physics.bodies.get(handle).unwrap();` (the re-fetch at ~line 203) through the closing of `if rotation != r { ... }` (~line 240) — with:

```rust
            // --- Look: accumulate mouse delta into yaw/pitch scalars ---
            let diff = client.input.mouse_diff();
            let (new_yaw, new_pitch) = crate::apply_look(yaw0, pitch0, diff.0, diff.1);
            if new_yaw != yaw0 || new_pitch != pitch0 {
                // Undo-tracked scalar update on the Camera component (same
                // pattern as the proj_matrix mutation above).
                let mut scope = ecs.camera.undo_scope();
                {
                    let c = scope.get_mut(e_id);
                    c.yaw = new_yaw;
                    c.pitch = new_pitch;
                }
                scope.register(move |d, _| {
                    let c = d.get_mut(e_id);
                    c.yaw = yaw0;
                    c.pitch = pitch0;
                });
                drop(scope);
                // Rebuild the body's full-look rotation. change(): same
                // whole-set snapshot rationale as the translation site above
                // (set_next_kinematic_rotation wakes + marks the body).
                let r = crate::look_quat(new_yaw, new_pitch);
                let bodies = data.physics.bodies.change();
                bodies
                    .get_mut(handle)
                    .unwrap()
                    .set_next_kinematic_rotation(r);
            }
```

Remove the now-unused `const SENSITIVITIY: f32 = 0.001;` and any leftover `use` of `clamp` in `on_tick` if it becomes unused (the helpers use `clamp`, so the import stays at module scope — leave it).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p game --test camera_controller`
Expected: PASS (2 tests). If `look_ticks_roll_back_bit_exact` fails, the camera undo `register` closure is not restoring both scalars — re-check that it sets `c.yaw = yaw0; c.pitch = pitch0;`.

- [ ] **Step 5: Run the full game rollback suite (guards determinism regressions)**

Run: `cargo test -p game`
Expected: PASS, including `tests/random_ops.rs`, `tests/kinematic.rs`, `tests/hash_restore.rs`. A failure here means the new camera undo entry breaks bit-exactness — do not paper over it.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/camera.rs crates/game/tests/camera_controller.rs
git commit -m "feat(camera): scalar look + yaw-only movement in controller"
```

---

### Task 3: Snap the render camera (remove the rotation lerp)

Sets the camera `SimTarget` smoothing to `1.0` so `lerp`/`slerp` return the target exactly each frame — position and rotation both snap. This kills the dominant visual lag and the slerp-arc tilt artifact. No change to `interpolate.rs` logic is needed; only the constructor default and a new test.

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs:50-52` (`SimTarget::camera`)
- Test: `crates/client/src/renderer/interpolate.rs` (add a test to the existing `mod tests`)

**Interfaces:**
- Consumes: existing `interpolate_transforms` system and `SimTarget` (`crates/client/src/renderer/bridge.rs`, `interpolate.rs`).
- Produces: camera `SimTarget` snaps to its pose within a single `Update`.

- [ ] **Step 1: Write the failing test**

In `crates/client/src/renderer/interpolate.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn camera_snaps_in_one_frame() {
        // Camera targets must not lerp — one update lands exactly on the pose.
        let target = SimTarget::camera(
            Vec3::new(5.0, -3.0, 2.0),
            Quat::from_rotation_y(1.2),
        );
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        app.update();
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert_eq!(t.translation, Vec3::new(5.0, -3.0, 2.0), "position must snap in one frame");
        assert!(
            t.rotation.angle_between(Quat::from_rotation_y(1.2)) < 1e-6,
            "rotation must snap in one frame"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p client --lib camera_snaps_in_one_frame`
Expected: FAIL — with `smoothing: 0.1`, one update moves ~10% of the way, so `translation` is `(0.5, -0.3, 0.2)`, not the target.

- [ ] **Step 3: Set camera smoothing to snap**

In `crates/client/src/renderer/bridge.rs`, change `SimTarget::camera` (currently line 50-52):

```rust
    pub fn camera(pos: Vec3, rot: Quat) -> Self {
        // Look lives in the 50Hz sim; the render camera snaps to it every
        // frame (smoothing = 1.0). No lerp => no slerp-arc tilt during turns.
        Self { pos, rot, smoothing: 1.0, pos_snap: 0.008, rot_snap: 0.001 }
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p client --lib camera_snaps_in_one_frame`
Expected: PASS.

- [ ] **Step 5: Run the client suite (nothing else should regress)**

Run: `cargo test -p client`
Expected: PASS, including the existing `converges_and_snaps_exactly` and `first_step_moves_partway` (the latter uses `SimTarget::body`, unaffected).

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer/bridge.rs crates/client/src/renderer/interpolate.rs
git commit -m "feat(camera): snap render camera to sim pose (no rotation lerp)"
```

---

## Verification & wrap-up (not a code task)

After all three tasks land, verify end-to-end behavior — the felt "snappy, no tilt" outcome is not fully covered by unit tests.

- [ ] **Full first-party test sweep.** Run `just test` and `just test-client` (or `cargo test -p game` and `cargo test -p client`). Do NOT run `cargo test --workspace` — it fails compiling the vendored nalgebra fork's own tests (known issue). Expected: all first-party tests pass.
- [ ] **Live drive.** Start server + one client (`just run`, or `WGPU_ADAPTER_NAME=6700 just run` for the eGPU). Enter FPS cam, then confirm by feel:
  - Fast mouse flicks turn the camera immediately (no truncation, no chase-lag).
  - Slow, small mouse moves register (no dead-zone).
  - The horizon never tilts/rolls during turns.
  - Looking straight up/down clamps just short of vertical and cannot flip over.
  - Holding W while looking down still walks at full horizontal speed.
- [ ] **Tune if needed.** If sensitivity feels off, adjust `LOOK_SENSITIVITY` / `LOOK_BASE_X` / `LOOK_BASE_Y` in `crates/game/src/camera.rs` and re-drive. These are the intended knobs.
- [ ] **Finish the branch.** Use superpowers:finishing-a-development-branch to decide merge/PR.

---

## Self-Review Notes

- **Spec coverage:** yaw/pitch scalars + clamp + no-roll (Task 1 helpers + Task 2 wiring); cubic sensitivity as tunable consts (Task 1); gate/clamp removal + yaw-only movement (Task 2); full-look on body / `physics.rs` untouched (Task 2 leaves the `UpdateCameraViewMatrix` emit alone); render snap of position AND rotation (Task 3); rollback bit-exactness (Task 2 Step 5); tests for clamp/no-roll/curve/snap (Tasks 1 & 3). All spec sections map to a task.
- **Type consistency:** `apply_look`/`yaw_quat`/`look_quat`/`look_delta_rad`/`PITCH_LIMIT`/`LOOK_*` names are used identically in Tasks 1 and 2. `Real = OrderedFloat<f32>` throughout. `SimTarget::camera` signature unchanged.
- **No placeholders:** every code step shows complete code; every run step shows the exact command and expected result.
