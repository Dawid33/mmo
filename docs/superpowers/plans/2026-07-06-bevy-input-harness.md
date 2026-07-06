# Bevy-glue Input Harness + "E never grabs cursor" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a headless Bevy-App harness that drives the real client input pipeline (`ButtonInput<KeyCode>` → `forward_input` → `GameInstanceManager` gates → sim tick → `SetFreeCam` → `drain_region_updates` → `CursorGrabMode`), reproduce/localize the "pressing E never grabs the cursor" bug with per-boundary inspectors, then root-cause and fix it.

**Architecture:** A `BevyInputHarness` (sibling to `SimHarness`) owns the same real `LocalServer` + `GameInstanceManager` lockstep core `SimHarness` wires, plus a headless `App` (`MinimalPlugins`+`AssetPlugin`) running the *real* `forward_input`/`drain_client_updates`/`drain_region_updates` with `SimBridgePlugin`'s ordering, a stub `PrimaryWindow`+`CursorOptions`, and a dedicated "tap" channel so the harness observes every forwarded input event before relaying it into the manager. Inspectors map 1:1 to the three bug suspects so a red test names the broken seam.

**Tech Stack:** Rust, Bevy 0.18 (headless `MinimalPlugins`), crossbeam channels, existing `game`/`client` crates. Build/test via the Cranelift backend.

## Global Constraints

- `crates/game` and `crates/server` stay Bevy-free and windowing-free — all new code lives in `crates/client`.
- Headless only: `MinimalPlugins` + `AssetPlugin::default()`, no window, no GPU.
- `game::set_hash_verification(true)` stays on along the driven path (as `SimHarness` does).
- Determinism bar unchanged; do not perturb the rollback/hash suites.
- Build/test command (Cranelift): `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client`. If a timing-sensitive test flakes under Cranelift, re-confirm with stock `cargo test -p client` (known Cranelift flake). The new harness is deterministic (no threads, no wall clock) and should not flake.
- Bevy 0.18: `CursorOptions` is a component split out of `Window`; the primary-window entity must carry `Window`, `CursorOptions`, and `PrimaryWindow` together.
- No `justfile` exists in this worktree; use the raw `cargo-clif` commands above (nothing to keep in sync).

---

### Task 1: Harness skeleton — construction, connect, step, `local_player()`

Builds the full harness plumbing and proves the front/back Bevy wiring drives `LocalPlayer` end to end (bug suspect **c**: `LocalPlayer` never set).

**Files:**
- Modify: `crates/client/src/renderer/mod.rs:13-16` (bump three module visibilities)
- Create: `crates/client/src/bevy_harness.rs`
- Modify: `crates/client/src/lib.rs:17` (add `pub mod bevy_harness;`)
- Test: `crates/client/tests/bevy_input.rs` (create)

**Interfaces:**
- Consumes: `GameInstanceManager::new(Sender<GameEventKind>, Receiver<GameEventKind>, Sender<ClientUpdateEvent>, SocketAddr)`, `GameInstanceManager::{start, send_tick, client_packet_recv, push_game_event, pump}`, `LocalServer::new(Receiver<ClientPacket>, Sender<ServerPacket>)`, `LocalServer::{pump, tick}`, renderer items `forward_input`, `drain_client_updates`, `drain_region_updates`, `ClientUpdates`, `GameEvents`, `LocalPlayer`, `Regions`, `RegionRoots`, `SimEntityMap`, `HudStatus`.
- Produces: `client::bevy_harness::BevyInputHarness` with `new() -> Self`, `connect(&mut self)`, `step(&mut self)`, `press_key(&mut self, KeyCode)`, `release_key(&mut self, KeyCode)`, `press_e(&mut self)`, `release_e(&mut self)`, `local_player(&self) -> Option<ClientId>`, `e_forwarded_count(&self) -> usize`, `cursor_grab_mode(&self) -> CursorGrabMode`.

- [ ] **Step 1: Make the pipeline submodules reachable from a sibling module**

In `crates/client/src/renderer/mod.rs`, change the three private module declarations to crate-visible (leave `avatar`, `block_textures`, `interpolate`, `voxel_material`, `convert`, `meshing` as they are):

```rust
pub(crate) mod bridge;
pub(crate) mod hud;
pub(crate) mod input;
```

(`forward_input`, `drain_client_updates`, `drain_region_updates`, `Regions`, `RegionRoots`, `SimEntityMap`, `HudStatus` are already `pub`; only the modules were private. `ClientUpdates`/`GameEvents`/`LocalPlayer` already live directly in the `pub mod renderer`.)

- [ ] **Step 2: Register the harness module**

In `crates/client/src/lib.rs`, directly after the `pub mod harness;` line (line 17), add:

```rust
pub mod bevy_harness; // BevyInputHarness — headless Bevy-layer input harness
```

- [ ] **Step 3: Write the harness skeleton**

Create `crates/client/src/bevy_harness.rs` with the full contents below. This is the complete module for Tasks 1 and 3; Task 2 adds one more method.

```rust
//! Headless Bevy-App harness driving the REAL client input pipeline:
//! `ButtonInput<KeyCode>` -> `forward_input` -> game channel ->
//! `GameInstanceManager` gates -> sim tick -> `SetFreeCam` ->
//! `drain_region_updates` -> `CursorGrabMode`. The Bevy-layer complement to
//! `SimHarness`: no window, no GPU, no threads, no wall clock. See
//! docs/superpowers/specs/2026-07-06-bevy-input-harness-design.md.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow, Window};
use crossbeam::channel::{unbounded, Receiver};
use game::{ClientId, ClientUpdateEvent, GameEventKind, InputEvent, Key, ServerPacket};

use crate::renderer::bridge::{drain_client_updates, drain_region_updates, Regions, RegionRoots, SimEntityMap};
use crate::renderer::hud::HudStatus;
use crate::renderer::input::forward_input;
use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
use crate::{GameInstanceManager, LocalServer};

pub struct BevyInputHarness {
    app: App,
    server: LocalServer,
    client: GameInstanceManager,
    server_to_client: Receiver<ServerPacket>,
    /// The channel `forward_input` writes into. The harness drains it, records
    /// what it saw, and relays each event into the manager — a clean sniff
    /// point that avoids racing the manager on one MPMC channel.
    game_tap: Receiver<GameEventKind>,
    /// Count of `Key::KeyE` press edges `forward_input` emitted.
    e_forwarded: usize,
    /// The stub primary-window entity carrying `Window` + `CursorOptions`.
    window: Entity,
}

impl BevyInputHarness {
    pub fn new() -> Self {
        game::set_hash_verification(true);

        // Manager input channel (manager reads game_recv; send_tick/push_game_event write game_send).
        let (game_send, game_recv) = unbounded::<GameEventKind>();
        // Manager -> bridge ClientUpdateEvent stream.
        let (bridge_send, bridge_recv) = unbounded::<ClientUpdateEvent>();
        // forward_input -> harness tap (relayed into the manager each step).
        let (tap_send, tap_recv) = unbounded::<GameEventKind>();

        let addr = "127.0.0.1:0".parse().unwrap();
        let client = GameInstanceManager::new(game_send, game_recv, bridge_send, addr);

        let client_to_server = client.client_packet_recv();
        let (server_to_client_send, server_to_client) = unbounded::<ServerPacket>();
        let server = LocalServer::new(client_to_server, server_to_client_send).expect("local server");

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .insert_resource(ClientUpdates(bridge_recv))
            .insert_resource(GameEvents(tap_send))
            // Same ordering SimBridgePlugin uses: forward_input BEFORE the drains.
            .add_systems(
                PreUpdate,
                (forward_input, (drain_client_updates, drain_region_updates).chain()).chain(),
            );

        // Stub primary window: all three components on one entity so both
        // `Query<&Window, With<PrimaryWindow>>` (forward_input) and
        // `Query<&mut CursorOptions, With<PrimaryWindow>>` (grab) match.
        let window = app
            .world_mut()
            .spawn((Window::default(), CursorOptions::default(), PrimaryWindow))
            .id();

        Self { app, server, client, server_to_client, game_tap: tap_recv, e_forwarded: 0, window }
    }

    /// Handshake: request the player's region, then step until `LocalPlayer` is
    /// set, plus a few extra steps so the sim's `is_caught_up` gate opens.
    pub fn connect(&mut self) {
        self.client.start();
        for _ in 0..64 {
            self.step();
            if self.local_player().is_some() {
                break;
            }
        }
        for _ in 0..8 {
            self.step();
        }
    }

    /// One deterministic frame. `MinimalPlugins` has no `InputPlugin`, so
    /// `ButtonInput` is never auto-cleared: we clear the just_pressed/released
    /// edges ourselves after `forward_input` has consumed them, or they would
    /// re-fire every frame and toggle fps-cam every tick.
    pub fn step(&mut self) {
        // Front: forward_input pushes queued key edges to the tap; drains prior updates.
        self.app.update();
        self.drain_tap();
        self.clear_input_edges();

        // Sim lockstep (mirrors SimHarness::step). Relayed PlayerInput lands in
        // game_recv BEFORE this Tick, so the press is registered, then the Tick
        // runs the sim toggle in the same drain.
        self.client.send_tick();
        self.client.pump(&self.server_to_client).expect("client pump");
        self.server.pump().expect("server pump");
        self.server.tick();
        self.client.pump(&self.server_to_client).expect("client reconcile pump");

        // Back: drain this tick's region updates -> apply the cursor grab.
        self.app.update();
        self.clear_input_edges();
    }

    /// Relay every event `forward_input` produced into the manager, counting E
    /// press edges on the way through.
    fn drain_tap(&mut self) {
        while let Ok(ev) = self.game_tap.try_recv() {
            if let GameEventKind::PlayerInput(_, InputEvent::Key { key: Key::KeyE, pressed: true }) = ev {
                self.e_forwarded += 1;
            }
            let _ = self.client.push_game_event(ev);
        }
    }

    fn clear_input_edges(&mut self) {
        self.app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        self.app.world_mut().resource_mut::<ButtonInput<MouseButton>>().clear();
    }

    // --- input entry points ---

    pub fn press_key(&mut self, code: KeyCode) {
        self.app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(code);
    }
    pub fn release_key(&mut self, code: KeyCode) {
        self.app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release(code);
    }
    pub fn press_e(&mut self) {
        self.press_key(KeyCode::KeyE);
    }
    pub fn release_e(&mut self) {
        self.release_key(KeyCode::KeyE);
    }

    // --- inspectors (one per bug suspect) ---

    /// suspect (c): is `LocalPlayer` set?
    pub fn local_player(&self) -> Option<ClientId> {
        self.app.world().resource::<LocalPlayer>().0
    }

    /// Did `forward_input` emit the E press edge (cleared the LocalPlayer gate)?
    pub fn e_forwarded_count(&self) -> usize {
        self.e_forwarded
    }

    /// suspect (a) + end assertion: the stub window's live grab mode.
    pub fn cursor_grab_mode(&self) -> CursorGrabMode {
        self.app
            .world()
            .entity(self.window)
            .get::<CursorOptions>()
            .expect("primary window carries CursorOptions")
            .grab_mode
    }
}

impl Default for BevyInputHarness {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Write the failing test**

Create `crates/client/tests/bevy_input.rs`:

```rust
//! End-to-end headless tests for the Bevy client input glue: driving a real
//! KeyCode::KeyE through forward_input -> GameInstanceManager -> sim ->
//! SetFreeCam -> cursor grab. See
//! docs/superpowers/specs/2026-07-06-bevy-input-harness-design.md.

use client::bevy_harness::BevyInputHarness;

#[test]
fn local_player_is_set_after_connect() {
    let mut h = BevyInputHarness::new();
    h.connect();
    assert!(
        h.local_player().is_some(),
        "LocalPlayer must be set after connect (SetPlayer -> drain_client_updates); suspect (c)"
    );
}
```

- [ ] **Step 5: Run the test to verify it compiles and observe pass/fail**

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client --test bevy_input local_player_is_set_after_connect -- --nocapture`
Expected: compiles; PASS if the wiring is correct. If it FAILS, that is a live reproduction of suspect (c) — record the output; Task 4 will root-cause it.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer/mod.rs crates/client/src/lib.rs crates/client/src/bevy_harness.rs crates/client/tests/bevy_input.rs
git commit -m "test(client): headless Bevy input harness skeleton + LocalPlayer e2e test"
```

---

### Task 2: `fps_cam_mode` read hook + E-forward/sim-toggle boundary test

Adds a read-only sim inspector and asserts a real E press is forwarded and toggles the sim (bug suspect **b**: the press eaten by the `is_caught_up`/`home_region` gates).

**Files:**
- Modify: `crates/client/src/instance.rs` (add `local_player_fps_cam_mode` near the other test/harness hooks, e.g. after `local_player_translation` at line ~326)
- Modify: `crates/client/src/bevy_harness.rs` (add `sim_fps_cam_mode` inspector)
- Modify: `crates/client/tests/bevy_input.rs` (add one test)

**Interfaces:**
- Consumes: `GameInstanceManager` private fields `home_region`, `world`, `client_id`; `game::World::{region_exists, data}`; `GameData.clients` (`UndoMap`, `.get(&ClientId) -> Option<&Client>`); `Client.fps_cam_mode: bool`.
- Produces: `GameInstanceManager::local_player_fps_cam_mode(&self) -> Option<bool>`; `BevyInputHarness::sim_fps_cam_mode(&self) -> Option<bool>`.

- [ ] **Step 1: Write the failing test**

Append to `crates/client/tests/bevy_input.rs`:

```rust
#[test]
fn e_press_forwards_and_toggles_sim() {
    let mut h = BevyInputHarness::new();
    h.connect();
    h.press_e();
    h.step();
    assert!(
        h.e_forwarded_count() >= 1,
        "the E press must be forwarded by forward_input (LocalPlayer gate); suspects (b)/(c)"
    );
    assert_eq!(
        h.sim_fps_cam_mode(),
        Some(true),
        "the E press must survive the manager gates and toggle fps_cam_mode in the sim; suspect (b)"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client --test bevy_input e_press_forwards_and_toggles_sim`
Expected: FAIL to compile — `no method named `sim_fps_cam_mode` found`.

- [ ] **Step 3: Add the read hook on `GameInstanceManager`**

In `crates/client/src/instance.rs`, directly after the `local_player_translation` method (ends at line ~326), add:

```rust
    /// The local player's fps-cam toggle state in its home region, if the
    /// client + home region are readable yet. `None` before the handshake
    /// settles. Test/harness hook.
    pub fn local_player_fps_cam_mode(&self) -> Option<bool> {
        let home = self.home_region?;
        let world = self.world.as_ref()?;
        if !world.region_exists(&home) {
            return None;
        }
        let data = world.data(&home);
        let client_id = self.client_id?;
        data.clients.get(&client_id).map(|c| c.fps_cam_mode)
    }
```

- [ ] **Step 4: Add the harness inspector**

In `crates/client/src/bevy_harness.rs`, add this method inside the `impl BevyInputHarness` block, next to the other inspectors (after `e_forwarded_count`):

```rust
    /// suspect (b): the local player's fps_cam_mode after the sim tick. `Some`
    /// once the home region is loaded.
    pub fn sim_fps_cam_mode(&self) -> Option<bool> {
        self.client.local_player_fps_cam_mode()
    }
```

- [ ] **Step 5: Run the test and observe pass/fail**

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client --test bevy_input e_press_forwards_and_toggles_sim -- --nocapture`
Expected: compiles; PASS if the press reaches and toggles the sim. If it FAILS, it localizes the bug to the front/gate boundary — record which assertion failed (`e_forwarded_count` vs `sim_fps_cam_mode`); Task 4 uses this.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/instance.rs crates/client/src/bevy_harness.rs crates/client/tests/bevy_input.rs
git commit -m "test(client): assert E press forwards and toggles fps-cam in the sim"
```

---

### Task 3: The reproducer — `pressing_e_grabs_and_releases_cursor`

The pivotal end-to-end test asserting the cursor grab flips `Locked` then back. Its pass/fail decides Task 4's direction. No new production code — `cursor_grab_mode`, `press_e`, `release_e` already exist from Task 1.

**Files:**
- Modify: `crates/client/tests/bevy_input.rs` (add one test)

**Interfaces:**
- Consumes: `BevyInputHarness::{connect, press_e, release_e, step, cursor_grab_mode}`; `bevy::window::CursorGrabMode`.
- Produces: nothing new.

- [ ] **Step 1: Write the reproducer test**

Append to `crates/client/tests/bevy_input.rs` (add the import at the top of the file alongside the existing `use`):

```rust
use bevy::window::CursorGrabMode;

#[test]
fn pressing_e_grabs_and_releases_cursor() {
    let mut h = BevyInputHarness::new();
    h.connect();
    assert_eq!(
        h.cursor_grab_mode(),
        CursorGrabMode::None,
        "cursor starts ungrabbed before any E press"
    );

    // First E press: fps-cam ON -> cursor grabbed.
    h.press_e();
    h.step();
    assert_eq!(
        h.cursor_grab_mode(),
        CursorGrabMode::Locked,
        "pressing E must grab/lock the cursor (the reported bug)"
    );

    // Release, then a second press: fps-cam OFF -> cursor released. (The sim
    // toggles on the press edge, so the second toggle needs release-then-press.)
    h.release_e();
    h.step();
    h.press_e();
    h.step();
    assert_eq!(
        h.cursor_grab_mode(),
        CursorGrabMode::None,
        "pressing E again must release the cursor"
    );
}
```

- [ ] **Step 2: Run the reproducer and record the outcome**

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client --test bevy_input pressing_e_grabs_and_releases_cursor -- --nocapture`
Expected: one of two informative outcomes —
- **FAIL** (grab stayed `None`): the harness reproduces the bug inside the pipeline it covers. Proceed to Task 4 to root-cause with the inspectors.
- **PASS**: the harnessed pipeline is correct; the bug lives *outside* it (real window construction / threading / the `is_caught_up` startup window). Task 4 pivots to that.

Record the exact outcome and, if failing, the values of `local_player()`, `e_forwarded_count()`, `sim_fps_cam_mode()` from the Task 1/2 tests.

- [ ] **Step 3: Commit the reproducer**

```bash
git add crates/client/tests/bevy_input.rs
git commit -m "test(client): reproducer — pressing E must grab then release the cursor"
```

---

### Task 4: Root-cause and fix

Use `superpowers:systematic-debugging`. The failing-test *pattern* from Tasks 1-3 localizes the seam before any fix is written. This task has no pre-written fix code — the fix is discovered empirically — but the diagnostic tree and verification commands are exact.

**Files:**
- Modify: whichever seam the diagnosis implicates (candidates below).
- Possibly modify: `crates/client/tests/bevy_input.rs` (tighten a test if the repro needs a more specific trigger, e.g. pressing E before `is_caught_up`).

**Diagnostic decision tree (read test results top-down):**

1. `local_player_is_set_after_connect` **fails** ⇒ suspect **(c)**: `SetPlayer` never reaches the bridge or `drain_client_updates` never sets `LocalPlayer`. Trace `PlayerRegion → SetPlayer` in `instance.rs:463-485` and `bridge.rs:98`.
2. `e_press_forwards_and_toggles_sim` **fails on `e_forwarded_count`** ⇒ front boundary: `forward_input` early-returned (`input.rs:44`, `LocalPlayer` unset that frame) or the key never registered — check the harness system ordering matches the real `SimBridgePlugin` and that `LocalPlayer` is set before the press.
3. `e_press_forwards_and_toggles_sim` **fails on `sim_fps_cam_mode`** (but `e_forwarded_count >= 1`) ⇒ suspect **(b)**: the `PlayerInput` was dropped by the `!is_caught_up` gate (`instance.rs:175`) or the `home_region` unset/not-loaded gate (`instance.rs:184-193`). Instrument those gates.
4. Only `pressing_e_grabs_and_releases_cursor` **fails** (the two boundary tests pass) ⇒ suspect **(a)** / the grab branch: `SetFreeCam` reached `drain_region_updates` but `local_player.0 != Some(client_id)` (`bridge.rs:313-327`), or the region update channel isn't wired for the client's predicted timeline. Compare the `client_id` in `SetFreeCam` (from `data.clients` key, `region.rs:250`) against `LocalPlayer.0` (from `SetPlayer`).
5. **All three pass** ⇒ the harnessed pipeline is correct and the bug is outside it. Pivot to the real Bevy integration: inspect `main.rs:121-148` primary-window construction (does the live primary window actually carry a `CursorOptions` component the `Query<&mut CursorOptions, With<PrimaryWindow>>` can match, or does the 0.18 split leave it unattached?), pointer-lock activation on the real window, and the live `is_caught_up`/~12s-connect gating window (per memory `wasm-input-gate-suspect`). Add a targeted assertion or a small `main.rs`-level check as the regression guard for whatever is found.

- [ ] **Step 1: Reproduce and localize**

Run the whole harness suite and read the pattern against the tree above:

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client --test bevy_input -- --nocapture`
Record which tests fail and map to the seam.

- [ ] **Step 2: Form and verify a hypothesis (systematic-debugging)**

Add temporary instrumentation (`eprintln!`/`dbg!`) at the implicated seam, re-run the failing test, and confirm the exact mechanism before editing. Do not edit production logic until the mechanism is confirmed by observed output.

- [ ] **Step 3: Apply the minimal fix**

Edit only the implicated seam. Keep `game`/`server` Bevy-free. Remove all temporary instrumentation.

- [ ] **Step 4: Verify the reproducer now passes**

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client --test bevy_input -- --nocapture`
Expected: all three tests PASS.

- [ ] **Step 5: Verify no regressions across the client suite**

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p client`
Expected: the full client suite (lib unit tests + `tests/*.rs`, including `crossing`/`harness_smoke`) is green. If a threaded integration test flakes, re-confirm with `cargo test -p client` (stock backend) per the known Cranelift flake.

- [ ] **Step 6: Verify the sim determinism suite is untouched**

Run: `~/Software/rustc_codegen_cranelift/dist/cargo-clif test -p game`
Expected: green (the fix must not perturb rollback/hash invariants).

- [ ] **Step 7: Commit the fix**

```bash
git add -A
git commit -m "fix(client): <seam>: pressing E now grabs the cursor"
```

- [ ] **Step 8 (recommended): Confirm live**

Per memory `testing-one-client`, live-test with the server + one client and confirm pressing E grabs the cursor in the real window:

```
~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin server   # terminal 1
WGPU_ADAPTER_NAME=6700 ~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin client   # terminal 2
```

(Ask the user to run these via `! <command>` if an interactive/GPU session is needed.) If the harness was green but live still fails, that confirms tree branch 5 — continue the investigation there.

---

## Self-Review

**Spec coverage:**
- Architecture (BevyInputHarness = SimHarness core + headless App) → Task 1. ✓
- MinimalPlugins+AssetPlugin, no full SimBridgePlugin, real systems with SimBridgePlugin ordering → Task 1 Step 3. ✓
- Stub `PrimaryWindow`+`Window`+`CursorOptions` → Task 1 Step 3. ✓
- Game-channel tee + relay → Task 1 (`game_tap`, `drain_tap`). ✓
- Sandwich `step()` + `ButtonInput::clear()` rationale → Task 1 Step 3. ✓
- Four inspectors mapped to suspects: `local_player` (Task 1), `e_forwarded_count` (Task 1), `sim_fps_cam_mode` (Task 2), `cursor_grab_mode` (Task 1). ✓
- Three tests (`local_player_is_set_after_connect`, `e_press_forwards_and_toggles_sim`, `pressing_e_grabs_and_releases_cursor`) → Tasks 1/2/3. ✓
- `local_player_fps_cam_mode` hook (spec: "add a small read-only hook if not reachable") → Task 2. ✓
- Root-cause + fix + both-outcomes handling → Task 4 (tree branches 1-5). ✓
- Constraints (Bevy-free game/server, headless, hash verification, determinism suite) → Global Constraints + Task 4 Steps 5-6. ✓

**Placeholder scan:** No TBD/TODO/"handle edge cases"; the only intentionally-open step is Task 4's discovered fix, which is a genuine debugging task with an exact diagnostic tree and verification commands rather than a fabricated patch. ✓

**Type consistency:** `BevyInputHarness` method names/signatures are identical across Tasks 1-3; `local_player_fps_cam_mode`/`sim_fps_cam_mode` names match between instance.rs and the harness; `game_tap`/`e_forwarded`/`window` fields defined in Task 1 and used consistently. ✓
