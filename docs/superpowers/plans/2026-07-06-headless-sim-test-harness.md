# Headless Deterministic Sim Test Harness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A headless, deterministic, tick-by-tick `SimHarness` that drives a real `WorldManager<InlineSpawner>` server + real `GameInstanceManager` client in-process with scripted input, plus the region-crossing regression test that would have caught the input-freeze.

**Architecture:** Extract a `client` library target so `GameInstanceManager`/`LocalServer`/the harness are reachable from `crates/client/tests/`. `SimHarness` composes `LocalServer` (authoritative) + `GameInstanceManager` (client) over the existing crossbeam channels and advances them in a fixed lockstep loop (client predict → server ingest+tick → client reconcile). Assertions: no-panic, liveness, bit-exact client/server convergence via a shared `state_hash`.

**Tech Stack:** Rust (client crate, edition 2021), crossbeam channels, `game` (`WorldManager`/`InlineSpawner`/`GameInstanceManager` types), `worldgen`, `crc32fast`. No Bevy/tokio in the harness.

## Global Constraints

- **Integration-test visibility:** items used by `crates/client/tests/*.rs` must be `pub` in the client **lib** and compiled **unconditionally** (NOT behind `#[cfg(test)]` — integration tests link the lib built without `cfg(test)`). Specifically `local_server` must lose its `test` gate.
- **No Bevy/tokio/threads/wall-clock in the harness.** Determinism: `LocalServer` owns `now_ms`, advanced only in `tick()`; ticks driven manually; the `Arc<AtomicU64>` tick-rate timer is unused.
- **Both build targets must keep working:** native (`cargo build -p client`) AND `cargo build -p client --target wasm32-unknown-unknown`. Preserve every existing `#[cfg(not(target_arch = "wasm32"))]` / `#[cfg(target_arch = "wasm32")]` gate when moving code.
- **No behaviour change** from the crate restructure — pure reachability refactor.
- Determinism hasher is `crc32fast` over `Rollback.data` (stable across runs/machines); do not use a default-seeded `DefaultHasher`.
- `TICK_RATE = 50`, `INDUCED_LATENCY = 0`, `LOCAL_CLIENT_ID = 0` (existing consts).
- Rollback bar `hash(before) == hash(after undo)` stays enforced (`game::set_hash_verification` on in debug); the harness must run with it on.

---

### Task 1: Extract a `client` library target (reachability refactor)

**Files:**
- Create: `crates/client/src/lib.rs`
- Modify: `crates/client/Cargo.toml` (add `[lib]`)
- Modify: `crates/client/src/main.rs` (becomes a thin binary)
- Modify: `crates/client/src/local_server.rs:1` area (drop the `test` cfg gate — see below)

**Interfaces:**
- Produces (relied on by all later tasks): crate `client` exposes, unconditionally on native:
  - `pub struct GameInstanceManager` + its existing `impl` blocks (`new`, `start`, `send_tick`, `client_packet_recv`, `pump`, and the rest — unchanged signatures).
  - `pub struct LocalServer` with `new(recv: Receiver<ClientPacket>, send: Sender<ServerPacket>) -> Self`, `pump(&mut self) -> Result<(), GameError>`, `tick(&mut self)`, and `pub const LOCAL_CLIENT_ID: ClientId = 0`.
  - `pub mod renderer;` (needed for `ClientUpdateEvent` bridge wiring) and whatever else the manager references.

- [ ] **Step 1: Add the `[lib]` target to `crates/client/Cargo.toml`**

Add after the `[package]` block (a crate may have both a lib and a bin):

```toml
[lib]
name = "client"
path = "src/lib.rs"

[[bin]]
name = "client"
path = "src/main.rs"
```

- [ ] **Step 2: Create `crates/client/src/lib.rs` owning the module tree**

The library owns all modules; the binary will consume them. Move the module
declarations out of `main.rs` into `lib.rs`, keeping every `cfg` gate verbatim:

```rust
//! Game client library: the engine-neutral, in-process pieces (netcode
//! routing, rollback client, local server, render bridge) that the binary
//! wires into a Bevy app — and that tests drive headlessly via `harness`.

#[cfg(not(target_arch = "wasm32"))]
pub mod netcode;
// Compiled unconditionally now (was cfg(any(wasm32, test))): the harness and
// integration tests need LocalServer in a normal (non-cfg(test)) lib build.
pub mod local_server;
#[cfg(target_arch = "wasm32")]
pub mod netcode_web;
#[cfg(target_arch = "wasm32")]
pub mod sim_driver;
pub mod renderer;
pub mod instance;   // GameInstanceManager (moved out of main.rs) — see Step 3
pub mod harness;    // SimHarness — added in Task 3 (empty stub file for now)

pub use instance::GameInstanceManager;
pub use local_server::{LocalServer, LOCAL_CLIENT_ID};
```

Create an empty `crates/client/src/harness.rs` now (just a doc comment) so the
`pub mod harness;` compiles; Task 3 fills it.

- [ ] **Step 3: Move `GameInstanceManager` into `crates/client/src/instance.rs`**

Cut the `pub struct GameInstanceManager { .. }` (main.rs:44) and BOTH `impl
GameInstanceManager` blocks (main.rs:89 and main.rs:495), plus the
`#[cfg(test)] mod manager_tests` (main.rs:707) and any free helpers they use
(e.g. `state_hash` closures — leave those for Task 2 to unify), verbatim into a
new `crates/client/src/instance.rs`. Add the `use` imports they need at the top
(copy the relevant `use` lines from main.rs — `game::{...}`, `crossbeam::...`,
`std::...`, `crate::local_server::...` if referenced). Change nothing but
module location and visibility (`pub` on the struct + public methods, already
`pub`). The `manager_tests` module moves with it and now refers to
`crate::GameInstanceManager` / `crate::LocalServer`.

- [ ] **Step 4: Make `crates/client/src/main.rs` a thin binary**

`main.rs` keeps only: the Bevy `App` setup, `main()`, `connect_and_run`
(`#[cfg(not(target_arch = "wasm32"))]`), the pyroscope/wasm entrypoints, and
their `use`s. Replace its former `mod` declarations + `GameInstanceManager`
definition with imports from the lib:

```rust
use client::{GameInstanceManager, LocalServer};
use client::renderer;            // if main.rs references renderer directly
// (netcode / netcode_web / sim_driver: `use client::netcode;` etc., keeping
//  the same cfg gates main.rs already had around their call sites)
```

Delete the now-moved `mod renderer; mod netcode; ...` lines and the
`GameInstanceManager` struct/impls/tests from `main.rs`.

- [ ] **Step 5: Drop the `test` cfg gate on `local_server`**

In `crates/client/src/local_server.rs`, the module is reachable via
`pub mod local_server;` in lib.rs now. Ensure its own `#[cfg(...)]` (if any at
the top of the file or on items) no longer restricts it to `test`. Its internal
`#[cfg(test)] mod tests` stays as-is (unit tests). If `LocalServer` referenced
`crate::GameInstanceManager`, update to the lib path.

- [ ] **Step 6: Verify all builds + tests (no behaviour change)**

```bash
cargo build -p client
cargo build -p client --target wasm32-unknown-unknown
cargo test -p client
```
Expected: all succeed; `cargo test -p client` runs the same manager/local_server
tests as before (now under the lib), same pass count (≈44), zero failures.

- [ ] **Step 7: Commit**

```bash
git add crates/client/Cargo.toml crates/client/src/lib.rs crates/client/src/main.rs crates/client/src/instance.rs crates/client/src/harness.rs crates/client/src/local_server.rs
git commit -m "refactor(client): extract library target for headless test reachability"
```

---

### Task 2: Shared `state_hash` helper

**Files:**
- Modify: `crates/game/src/lib.rs` (add `pub fn state_hash`)
- Test: `crates/game/tests/state_hash.rs` (Create)

**Interfaces:**
- Produces: `pub fn game::state_hash(r: &Rollback) -> u32` — deterministic crc32 of `r.data`.

- [ ] **Step 1: Write the failing test**

Create `crates/game/tests/state_hash.rs`:

```rust
use game::{state_hash, Rollback};

#[test]
fn state_hash_is_deterministic_and_sensitive() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut a = Rollback::new(Some(send.clone()));
    let b = Rollback::new(Some(send));
    assert_eq!(state_hash(&a), state_hash(&b), "identical fresh state hashes equal");

    a.new_transaction();
    a.create_player_safe(0);
    a.forget();
    assert_ne!(state_hash(&a), state_hash(&b), "a mutation changes the hash");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test state_hash`
Expected: FAIL to compile — `state_hash` not found in `game`.

- [ ] **Step 3: Add the helper**

In `crates/game/src/lib.rs`, after the existing `pub use` re-exports:

```rust
/// Deterministic, cross-machine-stable hash of a rollback's simulation state.
/// crc32fast (not a default-seeded DefaultHasher) so it is identical across
/// runs/machines — the basis for client/server convergence assertions in tests.
pub fn state_hash(r: &Rollback) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    r.data.hash(&mut h);
    h.finalize()
}
```

Ensure `crc32fast` is a dependency of `game` (it is — used by existing tests via
dev-deps; if it's only a dev-dependency, add `crc32fast = { workspace = true }`
to `[dependencies]` in `crates/game/Cargo.toml`, since this is now non-test code).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game --test state_hash`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/lib.rs crates/game/Cargo.toml crates/game/tests/state_hash.rs
git commit -m "feat(game): shared deterministic state_hash helper"
```

---

### Task 3: `SimHarness` core — construction + lockstep `step`

**Files:**
- Modify: `crates/client/src/harness.rs` (fill the stub)
- Test: `crates/client/tests/harness_smoke.rs` (Create)

**Interfaces:**
- Consumes: `client::{GameInstanceManager, LocalServer, LOCAL_CLIENT_ID}`, `game::{Key, InputEvent, GameEventKind, ClientPacket, ServerPacket, ClientUpdateEvent, state_hash, RegionCoords, SPAWN_REGION}`.
- Produces: `pub struct SimHarness` with `new() -> Self`, `connect(&mut self)`, `press(&mut self, Key)`, `release(&mut self, Key)`, `step(&mut self)`, `step_n(&mut self, usize)`.

- [ ] **Step 1: Write the failing test**

Create `crates/client/tests/harness_smoke.rs`:

```rust
use client::harness::SimHarness;

#[test]
fn harness_connects_and_ticks_without_panic() {
    let mut h = SimHarness::new();
    h.connect();
    // The client must have loaded its home region from the server snapshot.
    assert!(h.client_region_loaded(game::SPAWN_REGION), "home region loaded after connect");
    // Advancing must not panic and must advance the client sim clock.
    let t0 = h.client_tick();
    h.step_n(20);
    assert!(h.client_tick() > t0, "ticks advanced");
}
```

(`client_region_loaded` / `client_tick` are small inspectors; include them in
Step 3.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client --test harness_smoke`
Expected: FAIL to compile — `SimHarness` unimplemented.

- [ ] **Step 3: Implement the harness core**

Fill `crates/client/src/harness.rs`:

```rust
//! Headless, deterministic, tick-by-tick driver: a real LocalServer
//! (WorldManager<InlineSpawner>) + a real GameInstanceManager client, wired
//! over the existing channels and advanced in lockstep. No Bevy, no threads,
//! no wall clock. See docs/superpowers/specs/2026-07-06-headless-sim-test-harness-design.md.

use std::collections::BTreeSet;

use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientPacket, ClientUpdateEvent, GameEventKind, InputEvent, Key, RegionCoords, ServerPacket,
};

use crate::{GameInstanceManager, LocalServer, LOCAL_CLIENT_ID};

pub struct SimHarness {
    server: LocalServer,
    client: GameInstanceManager,
    server_to_client: Receiver<ServerPacket>,
    _bridge_recv: Receiver<ClientUpdateEvent>,
    held: BTreeSet<Key>,
}

impl SimHarness {
    /// Wire a client to a fresh local authoritative server. Mirrors the
    /// local_server.rs test setup.
    pub fn new() -> Self {
        game::set_hash_verification(true); // enforce hash(before)==hash(after undo) along the driven path
        let (game_event_send, game_event_recv) = unbounded::<GameEventKind>();
        let (bridge_send, bridge_recv) = unbounded::<ClientUpdateEvent>();
        // Dummy address: native netcode is never constructed in the harness.
        let addr = "127.0.0.1:0".parse().unwrap();
        let client = GameInstanceManager::new(game_event_send, game_event_recv, bridge_send, addr);

        let client_to_server = client.client_packet_recv(); // Receiver<ClientPacket>
        let (server_to_client_send, server_to_client) = unbounded::<ServerPacket>();
        let server = LocalServer::new(client_to_server, server_to_client_send);

        Self { server, client, server_to_client, _bridge_recv: bridge_recv, held: BTreeSet::new() }
    }

    /// Handshake: client requests its region; pump until the home region
    /// snapshot + 3x3 window have loaded.
    pub fn connect(&mut self) {
        self.client.start();
        // Drive a handful of steps so the RequestPlayerRegion -> PlayerRegion ->
        // Region-snapshot handshake and the initial window settle.
        for _ in 0..8 {
            self.step();
        }
    }

    pub fn press(&mut self, key: Key) { self.held.insert(key); }
    pub fn release(&mut self, key: Key) { self.held.remove(&key); }

    /// One deterministic tick: client predict -> server ingest+tick -> client reconcile.
    pub fn step(&mut self) {
        // 1. Client input + predict.
        for &key in &self.held {
            let _ = self
                .client
                .push_game_event(GameEventKind::PlayerInput(LOCAL_CLIENT_ID, InputEvent::Key { key, pressed: true }));
        }
        self.client.send_tick();
        self.client.pump(&self.server_to_client).expect("client pump");

        // 2. Server ingest + advance.
        self.server.pump().expect("server pump");
        self.server.tick();

        // 3. Client reconcile.
        self.client.pump(&self.server_to_client).expect("client reconcile pump");
    }

    pub fn step_n(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }

    // --- inspectors used by tests ---
    pub fn client_tick(&self) -> usize {
        let rc = self.client_home();
        self.client.world_ref().current_tick(&rc)
    }
    pub fn client_region_loaded(&self, rc: RegionCoords) -> bool {
        self.client.world_ref().region_exists(&rc)
    }
    fn client_home(&self) -> RegionCoords { self.client.home_region() }
}

impl Default for SimHarness {
    fn default() -> Self { Self::new() }
}
```

This requires three tiny `pub` accessors on `GameInstanceManager` (add them in
`instance.rs`, they expose existing private fields read-only + one input helper):

```rust
impl GameInstanceManager {
    /// Push a game event as if it came from local input (test/harness hook).
    pub fn push_game_event(&self, ev: GameEventKind) -> Result<(), crossbeam::channel::SendError<GameEventKind>> {
        self.game_event_send.send(ev)
    }
    pub fn world_ref(&self) -> &game::World { self.world.as_ref().expect("world loaded") }
    pub fn home_region(&self) -> game::RegionCoords { self.home_region }
}
```

(If `send_tick`/`pump`/`client_packet_recv` are already `pub`, reuse them; do
not duplicate. `home_region` field name per main.rs — confirm and match.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p client --test harness_smoke`
Expected: PASS. If `connect()`'s 8 steps aren't enough for the window to load,
raise the count until `client_region_loaded(SPAWN_REGION)` holds (the join
handshake is a few round-trips).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/harness.rs crates/client/src/instance.rs crates/client/tests/harness_smoke.rs
git commit -m "feat(client): SimHarness core — lockstep client<->server driver"
```

---

### Task 4: Scenario helpers + assertions

**Files:**
- Modify: `crates/client/src/harness.rs`
- Test: `crates/client/tests/harness_smoke.rs` (extend)

**Interfaces:**
- Consumes: Task 3's `SimHarness`, `game::state_hash`, `WorldManager`/`InlineSpawner` `with_region` hook (via `LocalServer`), `Region::data()`.
- Produces: `settle`, `assert_converged`, `assert_progresses`, `player_pos`, `player_region`, `teleport_player`, `cross_boundary`, plus `Dir`.

- [ ] **Step 1: Write the failing test**

Extend `crates/client/tests/harness_smoke.rs`:

```rust
#[test]
fn static_world_converges_client_and_server() {
    let mut h = SimHarness::new();
    h.connect();
    h.step_n(30);           // no input; both sides advance
    h.assert_converged();   // client home-region state == server home-region state, bit-exact
}

#[test]
fn held_input_moves_the_player() {
    let mut h = SimHarness::new();
    h.connect();
    // fps-cam on (KeyE), let it take effect, then walk.
    h.press(game::Key::KeyE); h.step(); h.release(game::Key::KeyE); h.step();
    h.assert_progresses(game::Key::KeyW);   // holding W advances tick AND moves the body
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p client --test harness_smoke`
Expected: FAIL to compile — `assert_converged`/`assert_progresses` missing.

- [ ] **Step 3: Implement helpers + assertions**

Add to `impl SimHarness` in `harness.rs`:

```rust
use game::{state_hash, ClientPacket}; // extend the existing use

#[derive(Copy, Clone)]
pub enum Dir { East, West, North, South }

impl SimHarness {
    /// Step with no new input until the client has drained buffered server
    /// events and its home-region tick has caught up to the server's.
    pub fn settle(&mut self) {
        for _ in 0..64 {
            self.step();
            if self.pending_events_empty() && self.client_tick() == self.server_tick(self.client_home()) {
                return;
            }
        }
        // Not fatal on its own; assertions below will surface a real divergence.
    }

    /// Bit-exact: every region both hold must hash-match after settling.
    pub fn assert_converged(&mut self) {
        self.settle();
        for rc in self.client.world_ref().loaded_regions() {
            if let Some(server_hash) = self.server_region_hash(rc) {
                let client_hash = state_hash(self.client.world_ref().data(&rc));
                assert_eq!(
                    client_hash, server_hash,
                    "client and server disagree on region {:?} after settle", rc
                );
            }
        }
    }

    /// Liveness: holding `key` advances the sim tick AND moves the player body.
    pub fn assert_progresses(&mut self, key: Key) {
        let t0 = self.client_tick();
        let p0 = self.player_pos();
        self.press(key);
        self.step_n(4);
        self.release(key);
        assert!(self.client_tick() > t0, "sim tick did not advance while holding {:?}", key);
        assert!(self.player_pos() != p0, "player did not move while holding {:?} (input frozen?)", key);
    }

    pub fn player_region(&self) -> RegionCoords { self.client.home_region() }
    pub fn player_pos(&self) -> [f32; 3] { self.client.local_player_translation() }

    /// Authoritative teleport for scenario setup (server-side, undo-safe).
    pub fn teleport_player(&mut self, pos: [f32; 3]) {
        self.server.teleport_local_player(pos);
        self.settle();
    }

    /// Hold the movement key for `dir` and step (bounded) until the client's
    /// home region changes — i.e. the player crossed a seam.
    pub fn cross_boundary(&mut self, dir: Dir) {
        let start = self.player_region();
        let key = match dir { Dir::North => Key::KeyW, Dir::South => Key::KeyS, Dir::East => Key::KeyD, Dir::West => Key::KeyA };
        // fps-cam must be on for movement; toggle it first.
        self.press(Key::KeyE); self.step(); self.release(Key::KeyE); self.step();
        self.press(key);
        for _ in 0..400 {
            self.step();
            if self.player_region() != start { break; }
        }
        self.release(key);
        assert_ne!(self.player_region(), start, "player never crossed a boundary");
    }

    // internal
    fn server_tick(&self, rc: RegionCoords) -> usize { self.server.region_tick(rc) }
    fn server_region_hash(&self, rc: RegionCoords) -> Option<u32> { self.server.region_hash(rc) }
    pub fn pending_events_empty(&self) -> bool { self.client.pending_events_empty() }
}
```

Add the supporting accessors (existing state, read-only):

`instance.rs` — on `GameInstanceManager`:
```rust
pub fn pending_events_empty(&self) -> bool { self.pending_events.values().all(|v| v.is_empty()) }
pub fn local_player_translation(&self) -> [f32; 3] { /* look up LOCAL player entity's body pos in world_ref().data(&home) via ecs.rigidbody + physics.bodies; return [x,y,z] */ }
```
`local_server.rs` — on `LocalServer`, thin pass-throughs into `WorldManager` +
`InlineSpawner::with_region`:
```rust
pub fn region_tick(&self, rc: RegionCoords) -> usize { /* self.manager … region current_tick */ }
pub fn region_hash(&self, rc: RegionCoords) -> Option<u32> { /* game::state_hash of the region's Rollback, if running */ }
pub fn teleport_local_player(&mut self, pos: [f32; 3]) { /* with_region(home, |r| r.with_data(|d| d.set_body_pose_safe(local_key, pose(pos)))) */ }
```
`game::World` — add `pub fn loaded_regions(&self) -> Vec<RegionCoords> { self.regions.keys().copied().collect() }` if not present.

Implement `local_player_translation` / `teleport_local_player` using the exact
ecs/physics accessors the existing crossing tests use (`data(&home).player_entites`,
`ecs.rigidbody.try_get`, `physics.bodies.get(handle).position().translation`,
and `with_region`/`with_data` + `set_body_pose_safe`). Match those call
patterns verbatim.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p client --test harness_smoke`
Expected: PASS (both tests). Tune the `connect`/`settle`/`cross_boundary` step
bounds if a handshake needs more round-trips.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/harness.rs crates/client/src/instance.rs crates/client/src/local_server.rs crates/game/src/lib.rs crates/client/tests/harness_smoke.rs
git commit -m "feat(client): SimHarness scenario helpers + convergence/liveness assertions"
```

---

### Task 5: Crossing regression test + determinism self-check

**Files:**
- Test: `crates/client/tests/crossing.rs` (Create)

**Interfaces:**
- Consumes: everything from Tasks 3–4.

- [ ] **Step 1: Write the tests**

Create `crates/client/tests/crossing.rs`:

```rust
use client::harness::{Dir, SimHarness};

#[test]
fn walking_across_a_region_boundary_keeps_input_and_converges() {
    let mut h = SimHarness::new();
    h.connect();
    h.assert_converged();                 // baseline agreement

    h.cross_boundary(Dir::East);          // walk across the seam under real input

    // Pre-fix this panicked (rollback hash-verify on ecs.kind, or SyncClock
    // unwrap on a released region). Post-fix: no panic, input still applies,
    // states still agree.
    h.assert_progresses(game::Key::KeyW);
    h.assert_converged();
    assert_ne!(h.player_region(), game::SPAWN_REGION, "player actually crossed");
}

#[test]
fn crossing_is_deterministic() {
    // Same script twice -> identical final client state hash.
    fn run() -> u32 {
        let mut h = SimHarness::new();
        h.connect();
        h.cross_boundary(Dir::East);
        h.settle();
        game::state_hash(h.client_home_data())
    }
    assert_eq!(run(), run(), "harness must be deterministic");
}
```

Add `pub fn client_home_data(&self) -> &game::Rollback { self.client.world_ref().data(&self.client.home_region()) }` to `SimHarness` in `harness.rs` for the determinism check.

- [ ] **Step 2: Run to verify (expect PASS on current develop — the fixes are already merged)**

Run: `cargo test -p client --test crossing`
Expected: PASS. NOTE: the region-crossing fixes (remove_entity_safe slot
exactness + SyncClock guard) are already on `develop`, so this test passes now —
it is the regression guard proving they stay fixed. To confirm it actually
bites, temporarily revert either fix and observe the panic (do not commit the
revert).

- [ ] **Step 3: Full verification**

```bash
cargo test -p client
cargo test -p game
cargo build -p client --target wasm32-unknown-unknown
```
Expected: all green; the new `crossing` + `harness_smoke` + `state_hash` tests
pass alongside the existing suites.

- [ ] **Step 4: Commit**

```bash
git add crates/client/tests/crossing.rs crates/client/src/harness.rs
git commit -m "test(client): headless region-crossing regression via SimHarness"
```

---

### Task 6 (optional): Migrate hand-authored crossing tests

**Files:**
- Modify: `crates/client/src/instance.rs` (the moved `manager_tests`)

- [ ] **Step 1:** Identify the two hand-authored crossing tests
(`predicted_crossing_target_region_converges_after_authoritative_catchup`,
`predicted_crossing_streaming_orphan_and_legit_inputs_coexist`) that build a
fake authoritative stream from bare `server_a`/`server_b` `Region`s.
- [ ] **Step 2:** Where `SimHarness` expresses the same scenario more directly,
rewrite them to use it and delete the fake-stream builders (`rebase_isometry`
hand-calls, forced `source_key`, manual `PlayerRegion` batches). Keep any
assertion they make that the harness doesn't yet cover (e.g. a specific
orphan-eviction id check) — port it as a new inspector rather than dropping it.
- [ ] **Step 3:** `cargo test -p client` green.
- [ ] **Step 4:** Commit `test(client): migrate crossing tests onto SimHarness`.

(Optional/YAGNI: skip if the existing tests still pull their weight. The final
review decides.)

---

## Self-Review

**Spec coverage:** client lib extraction (T1), shared state_hash (T2), SimHarness
core + lockstep loop (T3), input/scenario API + no-panic/liveness/convergence
assertions (T4), crossing regression + determinism self-check (T5), migration
(T6, optional). The spec's non-goals (no Bevy/tokio/threads/wall-clock) are held
by construction. wasm build gate verified in T1/T5.

**Placeholder scan:** The harness/test code is complete. Three accessor bodies
in T4 (`local_player_translation`, `teleport_local_player`, `region_hash`) are
specified by exact call-pattern reference ("match the existing crossing tests'
`ecs.rigidbody.try_get` + `physics.bodies.get(handle).position()`") rather than
verbatim, because they mechanically wrap existing accessors whose exact names
the implementer reads at the call site — not new logic. All test code and the
lockstep loop are verbatim.

**Type consistency:** `SimHarness::{new,connect,press,release,step,step_n,settle,assert_converged,assert_progresses,player_region,player_pos,teleport_player,cross_boundary,client_tick,client_region_loaded,pending_events_empty,client_home_data}`; `Dir::{East,West,North,South}`; `game::state_hash(&Rollback)->u32`; `GameInstanceManager::{push_game_event,world_ref,home_region,pending_events_empty,local_player_translation}`; `LocalServer::{region_tick,region_hash,teleport_local_player}`; `World::loaded_regions` — used consistently across tasks.

**Risk note:** the T1 lib extraction is the one non-mechanical-looking step;
its whole deliverable is "same build + same tests pass," so a reviewer gates it
purely on the three `cargo build`/`cargo test` invocations in T1 Step 6.
