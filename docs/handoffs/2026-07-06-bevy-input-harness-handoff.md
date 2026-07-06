# Handoff: Bevy-glue input harness + "E never grabs cursor" bug

**Date:** 2026-07-06
**Repo state:** branch `develop` @ `27f2013` (data-driven block registry just merged; unrelated to this bug).
**Goal for next session:** Build a **headless Bevy-App test harness** that drives the *real* client input pipeline (`forward_input → instance.rs gates → sim → SetFreeCam → bridge cursor-grab`), reproduce the "pressing **E** never grabs the cursor" bug as a failing test, then root-cause and fix it.

Start by reading this whole doc, then use `superpowers:brainstorming` to shape the harness design (it's new test infrastructure), then `superpowers:writing-plans` → execution. Consider working in a worktree off `develop`.

---

## The bug (user report)

Running the native client (`cargo run --bin server` + `cargo run --bin client`), on launch **pressing `E` never grabs/locks the cursor**. `E` is the fps-cam toggle; without the grab, mouse-look/movement are effectively unusable. User's characterization: "does not capture input on e press and seems not to capture input." When asked to disambiguate, they chose **"E never grabs cursor"** (not "dead forever"), and it's their **first time testing** the native client recently (so the regression window is unknown — could be develop's recent `sim-test-harness` refactor, or pre-existing).

## What is already established (do NOT re-derive)

**This is NOT caused by the block-registry work.** Verified this session:
- Server starts cleanly with the new manifest-loading code (`crates/server/src/lib.rs`) — listens on quinn `127.0.0.1:6466` + WebTransport `:6467`, no panic.
- Client connects, receives `PlayerRegion`→`SetPlayer`, loads the region, and renders the new texture/mesh pipeline with **no error** (observed live: `connected: addr=127.0.0.1:6466` … `region RegionCoords { x: 0, z: 0 } loaded`).
- `SimBridgePlugin` system registration is intact (`crates/client/src/renderer/mod.rs:52-73`): `forward_input`, `drain_client_updates`, `drain_region_updates`, `dedupe_ghosts` all present.

**The sim-side E path is verified working (headless).** The existing `SimHarness` (`crates/client/src/harness.rs:277`) presses `Key::KeyE` to enable fps-cam before moving; the `crossing` and `harness_smoke` integration tests pass, and they require movement (hence fps-cam on). So `region.rs`'s toggle (`crates/game/src/region.rs:243-255`: on `Tick`, `key_pressed(&Key::KeyE)` flips `client.fps_cam_mode` and emits `GameDataUpdateKind::SetFreeCam(client_id, mode)`) is sound.

**Therefore the fault is in the Bevy client glue that `SimHarness` bypasses** — the layer between the OS window and the sim, and the layer that turns `SetFreeCam` into an actual cursor grab. That is exactly what has **no test coverage** today, and what the new harness must cover.

## The input pipeline and its gates (with file:line)

Native flow: `winit` key event → `forward_input` → `game_send` channel → `GameInstanceManager` (game thread) → server; and back: server/predicted `SetFreeCam` → `client_recv` → bridge → `CursorGrabMode::Locked`.

1. `crates/client/src/renderer/input.rs:44` — `forward_input` **early-returns unless `LocalPlayer.0` is `Some`**. Maps `KeyCode::KeyE → Key::KeyE` (`input.rs:15`). Registered in `PreUpdate` `.after(bevy::input::InputSystems)` (`renderer/mod.rs:54`).
2. `crates/client/src/renderer/bridge.rs:98` — `ClientUpdateEvent::SetPlayer(id) → LocalPlayer.0 = Some(id)`.
3. `crates/client/src/instance.rs:175` — drops all `PlayerInput` while `!self.is_caught_up`.
4. `crates/client/src/instance.rs:184-198` — drops `PlayerInput` unless `self.home_region` is `Some` **and** that region is loaded (`region_exists`); otherwise silently dropped (no `else`) or `warn!("dropping PlayerInput: home region {:?} not loaded")`. On success it both applies locally (`handle_region_event(e, home)`) and forwards to the server.
5. `crates/client/src/instance.rs:463-485` — `PlayerRegion(id, client_id)` handler sets `client_id`, sends `SetPlayer`, sets `home_region = Some(home)`, and on first join requests the 3×3 window.
6. `crates/client/src/instance.rs:452-461` — `ServerPacket::GameEvent` sets `is_caught_up = true` once `world` exists.
7. `crates/game/src/region.rs:243-255` — sim toggle → `SetFreeCam(client_id, mode)` (verified working).
8. `crates/client/src/renderer/bridge.rs:313-327` — `GameDataUpdateKind::SetFreeCam(client_id, enabled)` → **grab only if `local_player.0 == Some(client_id)`**, then `cursor.grab_mode = CursorGrabMode::Locked; cursor.visible = false`.

**Prime suspects to instrument/assert, in order:** (a) `SetFreeCam` reaching the bridge but `local_player.0 != Some(client_id)` mismatch at `bridge.rs:315`; (b) the E `PlayerInput` dropped at the `is_caught_up`/`home_region` gates (`instance.rs:175,184`) so no `SetFreeCam` is ever produced on the predicted timeline; (c) `LocalPlayer` never set live. Also a real anomaly seen live: a **~12-second connect→region-load delay** (`INDUCED_LATENCY` is 0 — `crates/game/src/lib.rs:43`), during which input is gated shut.

## Deliverable

A **headless Bevy-App harness** (no window/GPU) that:
- Builds an `App` with `MinimalPlugins` + `AssetPlugin` and the render-bridge systems (`SimBridgePlugin` or a focused subset), following the existing headless-client test pattern.
- Drives a real `KeyCode::KeyE` press through `ButtonInput<KeyCode>` and pumps the sim (via `LocalServer`/`GameInstanceManager` or a minimal stub feeding the same channels), and **asserts the primary window's `CursorGrabMode` flips to `Locked`** (and back on second press).
- Fails today, reproducing the bug; passes after the fix.
- Ideally decomposed so each gate is independently assertable (LocalPlayer set → E forwarded → SetFreeCam delivered → grab applied), so the harness pinpoints *which* boundary breaks, not just that the end-to-end fails.

Then use the harness to root-cause (`superpowers:systematic-debugging`), fix the glue, and confirm the harness + existing suites (`cargo test -p client`) are green.

## Existing patterns to copy (headless client tests already do this)

- `crates/client/src/renderer/meshing.rs` tests — `App::new().add_plugins((MinimalPlugins, AssetPlugin::default()))`, `init_asset`, `init_resource`, run `app.update()` loops. This is the canonical no-window/no-GPU client test setup.
- `crates/client/src/renderer/hud.rs` `toggle_debug_flips_visibility` test — `init_resource::<ButtonInput<KeyCode>>()` then `world.resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::F3)` and `app.update()`. This is exactly how to inject a keypress headlessly. **Caveat:** `forward_input` runs `.after(bevy::input::InputSystems)`, and `get_just_pressed()` semantics depend on Bevy's input clearing each frame — verify the harness reproduces "just_pressed" correctly (may need to press, update, then release, mirroring `SimHarness::press/step/release`).
- `crates/client/src/renderer/bridge.rs` tests — send `ClientUpdateEvent::SetPlayer(0)` and drive `drain_client_updates`; shows how to set `LocalPlayer` and feed `ClientUpdates`/region updates in a test.
- `crates/client/src/harness.rs` (`SimHarness`) + `crates/client/tests/{crossing,harness_smoke}.rs` — the sim-level harness. The new harness is its Bevy-layer complement; reuse its channel-wiring approach (`LocalServer` over crossbeam) but pump a Bevy `App` instead of driving the sim directly.
- Window/cursor in a headless `App`: a `Window` entity with `CursorOptions` must exist for `bridge.rs`'s `windows.single_mut()` to succeed — the harness will need to spawn/insert one (see `bridge.rs:4` `use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow}`). Confirm whether `MinimalPlugins` provides a `PrimaryWindow`; if not, insert a stub window entity with `CursorOptions`.

## Constraints & environment

- `crates/game` and `crates/server` stay **Bevy-free and windowing-free** — the harness lives entirely in `crates/client`.
- Determinism bar unchanged (rollback/hash suites in `cargo test -p game`); don't perturb it.
- Build/test uses the Cranelift backend: `~/Software/rustc_codegen_cranelift/dist/cargo-clif build|test ...`. Client is a lib+bin crate now (develop's refactor): `cargo test -p client` covers lib unit tests + `tests/*.rs` integration.
- Client tests run headless via `MinimalPlugins`/`AssetPlugin` — no window, no GPU. Keep it that way.
- Note the cranelift flake: timing-sensitive threaded integration tests occasionally hit region-timeouts under cranelift; if a new timing-sensitive test flakes, confirm with standard `cargo test`.
- Relevant memory: `wasm-input-gate-suspect` (a prior input-drop gate was removed in 190fdc0; if input dies, check pointer-lock activation or a wedged server writer) — consistent with this being a grab/pointer-lock-path issue.

## Suggested first steps

1. Read `crates/client/src/instance.rs` (the whole file — it's the extracted `GameInstanceManager`, ~1400 lines, the heart of the gates) and `crates/client/src/renderer/{input,bridge,mod}.rs`.
2. Brainstorm the harness shape: full `SimBridgePlugin` + `LocalServer` end-to-end, vs a focused rig that feeds `ClientUpdates`/region-update channels directly and just exercises `forward_input`→`GameEvents` and `SetFreeCam`→grab. The focused rig is likely faster to get a failing test; the end-to-end catches the `is_caught_up`/`home_region`/12s-delay interactions.
3. Write the failing test (`superpowers:test-driven-development`), watch which gate trips, fix, verify.
