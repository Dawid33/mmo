# Headless Deterministic Sim Test Harness — Design

Date: 2026-07-06
Status: Design

## Overview

A headless, deterministic, tick-by-tick test harness that drives the game
simulation with scripted inputs — no Bevy app, no window, no button clicks — so
that client-behaviour bugs (e.g. the region-crossing input freeze) can be
reproduced and regression-tested in ordinary `cargo test`.

It runs a real authoritative server (`WorldManager<InlineSpawner>` via
`LocalServer`) and a real client (`GameInstanceManager`) in-process over the
existing crossbeam channels, and advances both in a deterministic lockstep loop
while injecting `PlayerInput`. Every simulation primitive it needs already
exists and is engine-agnostic; the harness is the missing *lockstep glue* plus a
scenario/assertion surface.

## Motivation

The region-crossing input freeze had two root causes at two layers:
1. Sim-layer: `remove_entity_safe` left dangling component slots, breaking
   rollback exactness on a same-transaction slot reuse (`hash(before) !=
   hash(after undo)` on `ecs.kind`).
2. Client-netcode-layer: the `SyncClock` handler in `GameInstanceManager`
   lacked a `region_exists` guard and panicked on a `SyncClock` for a region
   released mid-window-shift.

Both were found only by running the GUI client and manually roaming. Cause 1 is
unit-testable via `Region`/`Rollback`; cause 2 lives in the client netcode loop
that had **no headless test coverage of a real crossing against a real server**.
The existing crossing tests hand-author a fake authoritative event stream and
deliberately bypass the `WorldManager` routing that contained the bug. This
harness closes that gap.

## Goals

- Drive a full **client ↔ authoritative-server lockstep** headlessly and
  deterministically, tick by tick, with scripted player input.
- Reproduce the region-crossing scenario (window shift + handoff + reconcile +
  `SyncClock`) through the *real* `WorldManager` routing and `GameInstanceManager`
  reconcile — the paths both bugs lived in.
- Assert three levels of guarantee: no panic, liveness (input keeps applying),
  and bit-exact client/server convergence.
- Ship the regression test that would have caught the crossing freeze.
- Be reusable: usable from idiomatic `crates/client/tests/` integration tests.

## Non-Goals

- No Bevy, no rendering, no window, no GPU, no tokio/quinn netcode. The harness
  pulls in none of those.
- No wall-clock timing or threads — fully deterministic (`InlineSpawner`,
  integer `now_ms`, manual ticks). It does NOT test the real thread-timer pacing
  (`connect_and_run`) or the QUIC/WebTransport transport — those remain out of
  scope (the transport is a channel seam the harness substitutes).
- Not a fuzzer or property-test framework (though it composes with seeded
  scripts). No new scenario DSL beyond the method surface below.
- Does not change simulation behaviour. The crate restructure is mechanical.

## Prior Art (in-repo primitives this builds on)

Confirmed present and headless/deterministic:
- `Region::handle_event(GameEventKind)` — `Tick`/`PlayerInput`/`CreateClient`/
  `EntityArrived`/`GhostUpdate`; `reconcile`, `forget_last_event`, `with_data`,
  `data()`, `current_tick()`, `take_transfers()`.
- `WorldManager<InlineSpawner>` — threadless authoritative server; explicit
  `now_ms`; `handle_server_event(ServerEvent, now_ms)`, `handle_region_output`,
  `maintain(now_ms)`; `InlineSpawner::{pump, tick_all, with_region}`.
- `LocalServer` (`crates/client/src/local_server.rs`) — already wraps
  `WorldManager<InlineSpawner>` + `worldgen::generate_region`, speaks
  `ClientPacket`/`ServerPacket` over channels: `new(recv, send)`, `pump()`,
  `tick()` (`now_ms += TICK_RATE; tick_all; maintain; drain`). Its tests wire a
  real `GameInstanceManager` to it — but only exercise join/window, never a
  crossing.
- `GameInstanceManager` (`crates/client/src/main.rs`) — fully in-process over
  crossbeam channels, no Bevy/threads: `new`, `start`, `send_tick`, `pump(&Receiver<ServerPacket>)`,
  `client_packet_recv()`. Bevy/tokio live only in `connect_and_run`/`main`.
- `Rollback: Hash` — the determinism substrate; per-transaction hash
  self-verification via `game::set_hash_verification(bool)` (on in debug).

The gap: no driver that couples a live `WorldManager` crossing to a
`GameInstanceManager` reconcile in deterministic lockstep with a shared
convergence assertion.

## Architecture

### Crate restructure (client gains a library target)

`GameInstanceManager` and `LocalServer` live in the client **binary** crate, so
they're only reachable from in-binary `#[cfg(test)]` code. To make the harness
reusable from integration tests, extract a library target:

```
crates/client/
  src/
    lib.rs        NEW — the client library. `pub` exposes GameInstanceManager,
                  LocalServer, the harness, and the types tests need (re-exports
                  from renderer/netcode as required). Contains everything that
                  is not the Bevy/tokio entrypoint.
    main.rs       Thin binary: `fn main()` + connect_and_run + Bevy plugin
                  wiring, all behind #[cfg(not(target_arch = "wasm32"))] as
                  today. Depends on the lib.
    harness.rs    NEW — the SimHarness driver. Always compiled (not cfg(test))
                  so tests/ can use it; imports only in-process pieces (no
                  Bevy/tokio). Module declared in lib.rs.
    local_server.rs, main.rs manager code, renderer/, netcode.rs — unchanged in
                  behaviour; moved/re-exported under the lib as needed.
  tests/
    crossing.rs   NEW — integration tests using SimHarness (the regression + a
                  few scenarios).
```

`Cargo.toml` gains a `[lib]` alongside `[[bin]]`; the bin becomes a thin wrapper
(`fn main() { client::run() }` or similar). No code logic changes — purely
making already-in-process pieces reachable. Existing in-binary manager/crossing
tests move to `tests/` (or stay, importing from the lib).

Risk to watch: the client crate has native-only (`connect_and_run`) and
wasm-only (`sim_driver.rs`) code behind `cfg`. The lib must keep those `cfg`
gates so `lib.rs` still builds for both `--target wasm32-unknown-unknown` and
native. The harness itself is native-test-only in practice but has no
Bevy/tokio deps, so it compiles anywhere.

### `SimHarness` composition

```rust
pub struct SimHarness {
    server: LocalServer,                        // WorldManager<InlineSpawner> + worldgen;
                                                //   owns the client→server Receiver<ClientPacket>
                                                //   and its own now_ms clock (advanced in tick())
    client: GameInstanceManager,                // the client netcode/reconcile brain
    server_to_client: Receiver<ServerPacket>,   // server → client (drained by client.pump)
    _bridge_recv: Receiver<ClientUpdateEvent>,  // render-bridge sink, kept alive
    held_keys: BTreeSet<Key>,                   // current held input, applied each step
}
```

Wiring mirrors `local_server.rs`'s existing test setup:
- `client = GameInstanceManager::new(...)`; take `client.client_packet_recv()` →
  the client→server `Receiver<ClientPacket>`.
- create a `(Sender, Receiver)<ServerPacket>` pair; keep the receiver
  (`server_to_client`), give the sender to the server.
- `server = LocalServer::new(client_packet_recv, server_packet_send)` — it owns
  the client→server receiver and drains it in `pump()`, and owns `now_ms`
  (advanced by `tick()`).
- hold the `ClientUpdateEvent` bridge receiver alive.

## The lockstep loop

Time (`now_ms`) is owned by `LocalServer` and advanced only inside its `tick()`
(never wall clock). Ticks are driven manually; the `Arc<AtomicU64>` tick-rate
timer (real-thread only) is unused. One `step()`:

1. **Client input + predict.** For each held key, push `GameEventKind::PlayerInput(client, InputEvent::Key{..})`
   onto the client game-event channel (plus any queued `look`/one-shot inputs),
   then `client.send_tick()`. `client.pump(&self.server_to_client)` — processes
   the queued input/tick: predicts locally into its `World` and emits
   `ClientPacket`s to the server (drained via the receiver `LocalServer` owns).
2. **Server ingest + advance.** `server.pump()` — drains the client→server
   `ClientPacket`s through `WorldManager::handle_server_event`. Then
   `server.tick()` — `now_ms += TICK_RATE`, `tick_all`, `maintain(now_ms)`,
   drain region outputs into `ServerPacket`s on the server→client channel. This
   runs the real `WorldManager` routing/handoff and emits `SyncClock` every 10
   ticks.
3. **Client reconcile.** `client.pump(&self.server_to_client)` — drains the
   authoritative `ServerPacket`s: `GameEvent`→reconcile, `SyncClock`→(guarded
   handler), `Region` snapshots, `PlayerRegion` home-flips, window updates.

Determinism: fixed order, integer `now_ms`, no threads, no RNG in the loop → two
runs of the same script produce bit-identical state, so convergence is a hard
assertion, not a flaky one.

## Scenario / input API

```rust
impl SimHarness {
    pub fn new() -> Self;                       // spawn region + local player (client 0)
    pub fn connect(&mut self);                  // start(): RequestPlayerRegion; settle initial window

    // Input
    pub fn press(&mut self, key: Key);          // hold from next step
    pub fn release(&mut self, key: Key);
    pub fn look(&mut self, dx: f32, dy: f32);   // one-shot MouseMotion next step

    // Advance
    pub fn step(&mut self);                     // one tick (loop above)
    pub fn step_n(&mut self, n: usize);
    pub fn settle(&mut self);                   // step (no new input) until client drained
                                                // pending_events and caught up to server tick

    // Scenario helpers
    pub fn teleport_player(&mut self, pos: Vec3Real);   // authoritative setup via WorldManager::with_region
    pub fn cross_boundary(&mut self, dir: Dir);         // hold movement key; step (bounded) until home_region changes

    // Inspect
    pub fn player_region(&self) -> RegionCoords;
    pub fn player_pos(&self) -> Vec3Real;
    pub fn client_region_loaded(&self, rc: RegionCoords) -> bool;
    pub fn pending_events_empty(&self) -> bool;

    // Assert
    pub fn assert_progresses(&mut self, key: Key);      // hold key, step, assert tick advanced AND body moved
    pub fn assert_converged(&mut self);                 // settle, then client region hash == server region hash, all shared regions
}
```

`Key`/`InputEvent`/`RegionCoords` are the existing `game` types. `Vec3Real` is
the existing real-vector type used at the sim boundary.

## Assertion model

Three guarantees, increasing strength:

1. **No panic** (implicit): any panic on client or server fails the test. This
   alone reproduces both crossing bugs (each was a panic).
2. **Liveness** — `assert_progresses(key)`: hold `key`, `step()`, assert the
   client sim `tick` advanced *and* the player body position changed. Encodes
   "input stops working when crossing" as a positive property that fails on a
   freeze even absent a panic.
3. **Convergence** — `assert_converged()`: `settle()`, then for every region both
   client and server hold, `state_hash(client.world.data(rc)) ==
   state_hash(server_region(rc))`, bit-exact. Because the client predicts ahead,
   this is asserted at aligned ticks after settling, not per raw tick.

### Shared hash helper

Add one canonical deterministic hasher and remove the ~5 per-test-file copies:

```rust
// in game (or the harness lib), reused everywhere:
pub fn state_hash(r: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    r.data.hash(&mut h);   // Rollback/GameData: Hash
    h.finalize()
}
```

crc32fast is deterministic across runs/machines (unlike a default-seeded
`DefaultHasher` used ad hoc today). The harness and convergence checks use it.

## The proof: crossing regression test

`crates/client/tests/crossing.rs`:

```rust
#[test]
fn walking_across_a_region_boundary_keeps_input_and_converges() {
    let mut h = SimHarness::new();
    h.connect();
    h.assert_converged();                 // baseline: joined + window loaded, states agree

    // Walk east across the seam under real input, tick by tick.
    h.cross_boundary(Dir::East);          // holds the movement key, steps until home_region flips

    // The freeze bug: the sim thread panicked here (rollback hash-verify or
    // SyncClock unwrap). With the fix, it must not panic AND input must keep
    // applying AND client/server must still agree.
    h.assert_progresses(Key::KeyW);       // input still moves the player after crossing
    h.assert_converged();                 // client and server states agree post-crossing
    assert_ne!(h.player_region(), SPAWN_REGION, "player actually crossed");
}
```

Pre-fix (before the two crossing fixes) this test panics; post-fix it passes.
It exercises the real `WorldManager` routing + `GameInstanceManager` reconcile +
`SyncClock`-for-released-region path — none of which the existing hand-authored
crossing tests touch.

Additional scenarios (small, same harness): roam a full 3×3 window loop and back
(`assert_converged` throughout); release + re-subscribe a region (park/restore);
a `SyncClock` arriving for a just-released region (directly targets bug #2's
guard).

## Testing / verification of the harness itself

- The harness is exercised by `tests/crossing.rs`; a green run with
  `game::set_hash_verification(true)` (debug default) proves rollback exactness
  along the driven path.
- Determinism self-check: run the same script twice, assert identical final
  `state_hash` (guards against accidental nondeterminism in the harness).
- Migrate the existing in-binary crossing tests (`main.rs` manager_tests) to use
  `SimHarness` where it simplifies them; delete the hand-authored authoritative
  stream builders they no longer need. Keep any that test a distinct concern.
- Full workspace still builds native + `--target wasm32-unknown-unknown` (the
  lib split must not break the wasm client).

## Open Questions / Future

- SyncClock-driven catch-up/rewind pacing (the `Arc` tick-rate path) is real-
  thread-only and remains untested; if it grows logic, add a paced variant.
- A seeded multi-client variant (two `GameInstanceManager`s against one server)
  for contention scenarios — deferred (YAGNI until needed).
- The harness could later back a fuzz/property loop feeding random input
  scripts and asserting no-panic + convergence — deferred.
