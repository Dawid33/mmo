# Periodic State-Hash Desync Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The server stamps a crc32 of each region's full post-tick state onto every `STATE_HASH_INTERVAL_TICKS`-th Tick event; the client verifies its confirmed state at the same event-log position and hard-crashes with `GameError::StateDesync` on mismatch.

**Architecture:** `GameEventKind::Tick` gains a `state_hash: Option<u32>` field (transport metadata — never read by the sim). `RegionRunner::tick` stamps it server-side. `Region::reconcile` verifies client-side at the confirmed boundary: on the match path it rolls back outstanding predictions with the exact-inverse undo machinery (same pattern as `evict_orphan_local_inputs`), hashes, compares, and replays; on the mispredict path the boundary state is already materialized so it hashes directly. Mismatch returns `GameError::StateDesync`, which propagates out of the client game loop and terminates the process.

**Tech Stack:** Rust workspace; `game` crate (engine-agnostic sim + rollback), `client` (Bevy app), `server`. Hashing via existing `game::state_hash` (crc32fast). Spec: `docs/superpowers/specs/2026-07-10-periodic-state-hash-desync-check-design.md`.

## Global Constraints

- `crates/game` and `crates/server` must stay Bevy-free and windowing-free.
- Never weaken or disable rollback hash verification; the desync check is additive.
- The `state_hash` field must NEVER influence `handle_event`, the undo log, or any sim behavior — it is transport metadata only.
- Determinism: do not touch simulation math or the vendored forks.
- Wire format is bincode; client+server ship together, so the `Tick` variant change needs no migration. `u32` is wasm32-safe.
- Tag any test measured >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- Fast loop is `just test`; final check is `just test-full`.
- If an EXISTING convergence/harness test starts failing with `StateDesync` after Task 3, that is a real determinism gap this feature just detected. Do not weaken the check or the test — stop and report it.

## Reference: current code facts (verified 2026-07-10)

- `game::state_hash(r: &Rollback) -> u32` at `crates/game/src/lib.rs:65` hashes `r.data` (the `GameData`) with crc32fast. Reuse as-is.
- `Region::handle_event` (`crates/game/src/region.rs:444`) assigns the event id from `next_game_event_id`, opens a transaction, and **pushes the event onto `event_log`** (line 527). On the client every locally executed event is a prediction in `event_log`; the server calls `forget_last_event()` after each event.
- `Region::reconcile` (`region.rs:159`) integrates a server event through exactly two paths inside its `'outer` loop:
  - **MATCH+FORGET** (`region.rs:341-348`): predicted event equals server event → `self.data.forget()`. Live state still has predictions with ids > N applied.
  - **MISPREDICT** (`region.rs:228-340`): full log rollback, `self.handle_event(server_event.kind)` + `self.data.forget()` (lines 262-263) — at that moment the live state IS confirmed-through-N — then replay of `temp_log`.
  - Events with `id < base_event_id` early-return (line 163). With an empty `event_log` events park in `input_buffer` (line 195) and are only ever integrated later through the same two paths — so two check sites cover the parked case (the spec's open point, now resolved).
- The mispredict trigger is full equality: `if event != server_event.0` (`region.rs:228`). `matches_prediction` (`protocol.rs:326` area) is a separate identity matcher used at `region.rs:298` for prediction removal; do NOT reuse it for the mispredict trigger (it would mask real EntityArrived/GhostUpdate divergence).
- `Rollback::rollback()` pops the NEWEST un-forgotten transaction; `Rollback::forget()` pops the OLDEST. `evict_orphan_local_inputs` (`region.rs:420-441`) is the exact pop-back/rollback/replay pattern the new helper mirrors.
- `RegionRunner::tick` (`crates/game/src/region_runner.rs:148`) runs `handle_event(Tick)`, `forget_last_event()`, then sends `RegionOutput::EventProcessed(event)`; it already does modulo-cadence work (`% 10` SyncClock, `% FAR_SNAPSHOT_MIN_INTERVAL_TICKS`). `data.tick` increments during the tick, so after `handle_event` the event with id-position k has `current_tick() == k+1`.
- Client error flow: `Region::reconcile` → `World::reconcile_event` (`lib.rs:242`) → `GameInstanceManager::route_server_game_event` (`instance.rs:687`) → `handle_server` → `connect_and_run` (`instance.rs:869`) → `main.rs:66` which currently only `warn!`s. The pending-events replay at `instance.rs:817` SWALLOWS reconcile errors (`if let Ok(stats)`) — must be fixed. On wasm, `pump()` callers `.expect()`/`.unwrap()` errors, so an `Err` already panics (= crash) there.
- `ReconcileStats` is at `region.rs:76-85`; `ReconMetrics::record` folds it at `instance.rs:137-146`; a second manual fold lives at `instance.rs:814-823`.
- Test helper precedent: `crates/game/tests/region_runner.rs` builds a `RegionRunner` from `Region::from_chunks(id, flat_chunks())` + an unbounded crossbeam channel.

---

### Task 1: `Tick { state_hash }` variant, `StateDesync`, `sim_eq`, interval constant

**Files:**
- Modify: `crates/game/src/protocol.rs` (Tick variant, GameError, origin_client, matches_prediction, new sim_eq)
- Modify: `crates/game/src/lib.rs` (new constant; Tick construction at line 218)
- Modify (mechanical pattern/construction updates): `crates/game/src/region.rs:118,450`, `crates/game/src/region_runner.rs:111,151`, `crates/game/src/world_manager.rs:216`, `crates/client/src/instance.rs:248,301,875,1257,1281` + test modules, `crates/client/src/main.rs` (only if it references Tick), `crates/worldgen/examples/narrow_phase_probe.rs`
- Modify (test files, mechanical): `crates/game/tests/{region_runner,camera_controller,handoff_protocol,region_coords,hash_mode,handoff_scan,multi_client,handoff_state}.rs`, `crates/client/tests/keepup_budget.rs`
- Test: `crates/game/tests/handoff_protocol.rs` (extend the matcher test)

**Interfaces:**
- Produces: `GameEventKind::Tick { state_hash: Option<u32> }`; `GameEventKind::sim_eq(&self, &Self) -> bool`; `GameEvent::sim_eq(&self, &Self) -> bool`; `GameError::StateDesync { region: RegionId, event_id: EventId, server_hash: u32, client_hash: u32 }`; `pub const STATE_HASH_INTERVAL_TICKS: usize = 10` (in `game`'s root). Tasks 2–4 consume all of these.

- [ ] **Step 1: Write the failing test (extend the matcher test in `crates/game/tests/handoff_protocol.rs`)**

Replace line 81 (`assert!(GameEventKind::Tick.matches_prediction(&GameEventKind::Tick));`) with:

```rust
    // Ticks match regardless of the server's state-hash stamp (transport
    // metadata): a predicted plain tick must pair with a stamped server tick.
    let plain = GameEventKind::Tick { state_hash: None };
    let stamped = GameEventKind::Tick { state_hash: Some(0x1234_5678) };
    assert!(plain.matches_prediction(&stamped));
    assert!(plain.matches_prediction(&plain));
    // sim_eq: same relaxation for the mispredict check…
    assert!(plain.sim_eq(&stamped));
    // …but everything else keeps exact equality.
    assert!(!plain.sim_eq(&GameEventKind::Quit));
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test handoff_protocol`
Expected: COMPILE ERROR (`Tick` takes no fields / no method `sim_eq`) — a compile failure is this step's "red".

- [ ] **Step 3: Implement the protocol changes in `crates/game/src/protocol.rs`**

Change the variant (in `GameEventKind`, `protocol.rs:291-304`):

```rust
    /// `state_hash` is a server-only stamp: every `STATE_HASH_INTERVAL_TICKS`
    /// ticks the server sets it to `state_hash()` of the region AFTER this
    /// tick applied. Transport metadata — never read by the sim, never set on
    /// client-predicted ticks. See GameError::StateDesync.
    Tick { state_hash: Option<u32> },
```

Update `origin_client` (`protocol.rs:320-332`): the `GameEventKind::Tick` arm becomes `GameEventKind::Tick { .. }`.

Add the Tick arm to `matches_prediction` (before the `_ => self == other` fallback):

```rust
            // The state-hash stamp is transport metadata; a predicted plain
            // tick pairs with the server's stamped tick.
            (Tick { .. }, Tick { .. }) => true,
```

Add `sim_eq` to the same `impl GameEventKind` block:

```rust
    /// Simulation-relevant equality: exact equality, except the Tick
    /// state-hash stamp is ignored (it is server-only transport metadata, so
    /// a predicted `Tick { state_hash: None }` must not read as a
    /// misprediction against the server's stamped tick). Unlike
    /// `matches_prediction`, everything else stays bit-exact — pose-divergent
    /// transfers must still trigger rollback+replay.
    pub fn sim_eq(&self, other: &Self) -> bool {
        use GameEventKind::*;
        match (self, other) {
            (Tick { .. }, Tick { .. }) => true,
            _ => self == other,
        }
    }
```

Add to `impl GameEvent` (near `GameEvent::new`, `protocol.rs:281-289`):

```rust
    /// See `GameEventKind::sim_eq` — id/region compare exactly.
    pub fn sim_eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.region_id == other.region_id
            && self.kind.sim_eq(&other.kind)
    }
```

Add the error variant to `GameError` (`protocol.rs:182-186`):

```rust
#[derive(Debug)]
pub enum GameError {
    CrashedOnServerEvent,
    QuitRequested,
    /// Periodic desync check failed: the client's confirmed state at
    /// `event_id` hashed differently from the server's stamp. Always a
    /// determinism bug — the client crashes, never resyncs.
    StateDesync {
        region: RegionId,
        event_id: EventId,
        server_hash: u32,
        client_hash: u32,
    },
}
```

- [ ] **Step 4: Add the constant to `crates/game/src/lib.rs` (next to `METRICS_REPORT_INTERVAL`, line 60)**

```rust
/// Every this-many ticks a server region stamps `state_hash` of its
/// post-tick state onto the outgoing Tick event; subscribed clients verify
/// their confirmed state against it and crash (`GameError::StateDesync`) on
/// mismatch. Tunable; 10 ≈ 5 s at TICK_RATE = 500.
pub const STATE_HASH_INTERVAL_TICKS: usize = 10;
```

- [ ] **Step 5: Mechanically update every remaining `GameEventKind::Tick` site**

Two rules, applied everywhere the compiler complains:
- **Construction** `GameEventKind::Tick` (as a value) → `GameEventKind::Tick { state_hash: None }`
- **Pattern** `GameEventKind::Tick` (in `match`/`assert_eq!` against a kind) → `GameEventKind::Tick { .. }` (for `assert_eq!(ev.kind, GameEventKind::Tick)` use `assert!(matches!(ev.kind, GameEventKind::Tick { .. }))`)

Find them all with:

```sh
grep -rn "GameEventKind::Tick" crates --include="*.rs" | grep -v "Tick {"
```

Known sites: `game/src/lib.rs:218`, `game/src/region.rs:118,450`, `game/src/region_runner.rs:111,151`, `game/src/world_manager.rs:216`, `client/src/instance.rs:248,301,875,1257,1281` plus its `#[cfg(test)]` module, `worldgen/examples/narrow_phase_probe.rs`, and the game/client test files listed in **Files**. Let `cargo check --workspace` be the completeness oracle.

- [ ] **Step 6: Run test to verify it passes, plus the fast suite**

Run: `cargo test -p game --test handoff_protocol`
Expected: PASS

Run: `just test`
Expected: PASS (no behavior change yet — `state_hash` is always `None`)

- [ ] **Step 7: Commit**

```bash
git add -A crates docs
git commit -m "feat(game): Tick state_hash field, sim_eq, StateDesync error, interval const"
```

---

### Task 2: Server stamps the hash in `RegionRunner::tick`

**Files:**
- Modify: `crates/game/src/region_runner.rs:148-154`
- Test: `crates/game/tests/region_runner.rs`

**Interfaces:**
- Consumes: `GameEventKind::Tick { state_hash }`, `STATE_HASH_INTERVAL_TICKS`, `state_hash()` (Task 1 + existing).
- Produces: `RegionOutput::EventProcessed` Tick events whose `state_hash` is `Some(crc32-after-this-tick)` exactly when `current_tick() % STATE_HASH_INTERVAL_TICKS == 0`. Task 3's tests rely on this stamping behavior.

- [ ] **Step 1: Write the failing test (append to `crates/game/tests/region_runner.rs`)**

```rust
#[test]
fn state_hash_stamped_every_interval() {
    use game::STATE_HASH_INTERVAL_TICKS;
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    // Deterministic twin: same construction + same tick sequence ⇒ the stamp
    // must equal the twin's post-tick state_hash.
    let mut twin = Region::from_chunks(id, flat_chunks());

    for k in 0..(2 * STATE_HASH_INTERVAL_TICKS) {
        twin.handle_event(GameEventKind::Tick { state_hash: None }).unwrap();
        twin.forget_last_event();
        r.tick();
        let mut saw_tick = false;
        while let Ok((_, output)) = out.try_recv() {
            let RegionOutput::EventProcessed(ev) = output else { continue };
            let GameEventKind::Tick { state_hash } = ev.kind else { continue };
            saw_tick = true;
            // post-tick counter: tick event k has current_tick == k+1
            if (k + 1) % STATE_HASH_INTERVAL_TICKS == 0 {
                assert_eq!(
                    state_hash,
                    Some(game::state_hash(twin.data())),
                    "tick {k}: stamp must be the post-tick state hash"
                );
            } else {
                assert_eq!(state_hash, None, "tick {k}: no stamp off-interval");
            }
        }
        assert!(saw_tick, "tick {k} must emit EventProcessed");
    }
}
```

Also add `state_hash` to the `use game::{...}` list at the top of the file if not importing via `game::` paths (the test body above uses fully qualified `game::state_hash` / `game::STATE_HASH_INTERVAL_TICKS`, so only `GameEventKind` needs to already be imported — it is).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test region_runner state_hash_stamped_every_interval`
Expected: FAIL — `state_hash` is `None` on interval ticks (assert on the `Some(...)` branch).

- [ ] **Step 3: Implement stamping in `RegionRunner::tick` (`region_runner.rs:148-154`)**

Replace the start of `tick()`:

```rust
    pub fn tick(&mut self) {
        let mut event = self
            .region
            .handle_event(GameEventKind::Tick { state_hash: None })
            .expect("region tick failed");
        self.region.forget_last_event();
        // Periodic desync stamp: crc32 of the state AFTER this tick, carried
        // on this tick's event. Subscribed clients verify at the same log
        // position and crash on mismatch (GameError::StateDesync).
        if self.region.current_tick() % crate::STATE_HASH_INTERVAL_TICKS == 0 {
            if let GameEventKind::Tick { state_hash } = &mut event.kind {
                *state_hash = Some(crate::state_hash(self.region.data()));
            }
        }
        let _ = self.out.send((self.id, RegionOutput::EventProcessed(event)));
```

(The rest of `tick()` — transfers, SyncClock, far snapshot — is unchanged. `STATE_HASH_INTERVAL_TICKS` and `state_hash` are used via `crate::`; no import churn.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p game --test region_runner`
Expected: PASS (including the pre-existing tests in that file)

Run: `just test`
Expected: PASS — clients don't check yet, so stamped events must flow through reconcile as plain matches thanks to Task 1's `matches_prediction`/`sim_eq` groundwork. A failure here means Task 1's relaxation is incomplete.

- [ ] **Step 5: Commit**

```bash
git add crates/game
git commit -m "feat(game): server stamps post-tick state hash every STATE_HASH_INTERVAL_TICKS"
```

---

### Task 3: Client verifies at the confirmed boundary in `Region::reconcile`

**Files:**
- Modify: `crates/game/src/region.rs` (ReconcileStats field, two check sites, two helper methods, `sim_eq` at line 228)
- Modify: `crates/client/src/instance.rs:137-146` and `:814-823` (fold the new stats field)
- Test: `crates/game/tests/desync_check.rs` (new file)

**Interfaces:**
- Consumes: `GameEventKind::Tick { state_hash }`, `GameEvent::sim_eq`, `GameError::StateDesync`, `STATE_HASH_INTERVAL_TICKS`, `RegionRunner` stamping (Tasks 1–2).
- Produces: `ReconcileStats.hash_checks_passed: u32` (public field — client metrics fold it); `Region::reconcile` returns `Err(GameError::StateDesync { .. })` on mismatch. Task 4 relies on that error reaching `reconcile`'s caller.

- [ ] **Step 1: Write the failing tests — new file `crates/game/tests/desync_check.rs`**

```rust
//! Periodic state-hash desync check: server stamps, client verifies at the
//! confirmed boundary, mismatch is a hard StateDesync error.

use crossbeam::channel::{unbounded, Receiver};
use game::{
    Chunk, ChunkCoords, GameError, GameEvent, GameEventKind, Region, RegionCoords, RegionOutput,
    RegionRunner, STATE_HASH_INTERVAL_TICKS,
};

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

fn runner(id: RegionCoords) -> (RegionRunner, Receiver<(RegionCoords, RegionOutput)>) {
    let (out_send, out_recv) = unbounded();
    let region = Region::from_chunks(id, flat_chunks());
    (RegionRunner::new(id, region, out_send), out_recv)
}

fn plain_tick() -> GameEventKind {
    GameEventKind::Tick { state_hash: None }
}

fn drain_events(recv: &Receiver<(RegionCoords, RegionOutput)>) -> Vec<GameEvent> {
    let mut out = Vec::new();
    while let Ok((_, output)) = recv.try_recv() {
        if let RegionOutput::EventProcessed(ev) = output {
            out.push(ev);
        }
    }
    out
}

/// Convergent client/server pair: checks fire through real stamping, pass,
/// and never read as mispredictions. Client stays one predicted tick ahead
/// so the match-path check exercises a nonzero rollback depth.
#[test]
fn convergent_client_passes_hash_checks_without_mispredicts() {
    let id = RegionCoords::new(0, 0);
    let (mut server, recv) = runner(id);
    let mut client = Region::from_chunks(id, flat_chunks());

    let mut checks = 0u32;
    let mut mispredicts = 0u32;
    client.handle_event(plain_tick()).unwrap(); // prediction id 0
    for _ in 0..(3 * STATE_HASH_INTERVAL_TICKS) {
        client.handle_event(plain_tick()).unwrap(); // stay one ahead
        server.tick();
        for ev in drain_events(&recv) {
            let stats = client.reconcile(ev).expect("convergent pair must not desync");
            checks += stats.hash_checks_passed;
            mispredicts += stats.mispredictions;
        }
    }
    assert!(checks >= 2, "expected multiple hash checks, got {checks}");
    assert_eq!(mispredicts, 0, "a stamped tick must not read as a mispredict");
}

/// A wrong stamp is a StateDesync with the right ids and both hashes.
#[test]
fn wrong_hash_is_state_desync() {
    let id = RegionCoords::new(0, 0);
    let mut client = Region::from_chunks(id, flat_chunks());
    // Compute the CORRECT post-tick hash on a twin, then corrupt it: no
    // reliance on magic constants never colliding.
    let mut twin = Region::from_chunks(id, flat_chunks());
    twin.handle_event(plain_tick()).unwrap();
    let bogus = game::state_hash(twin.data()) ^ 1;

    client.handle_event(plain_tick()).unwrap(); // prediction id 0
    let stamped = GameEvent::new(GameEventKind::Tick { state_hash: Some(bogus) }, 0, id);
    let err = client.reconcile(stamped).expect_err("corrupted stamp must desync");
    match err {
        GameError::StateDesync { region, event_id, server_hash, client_hash } => {
            assert_eq!(region, id);
            assert_eq!(event_id, 0);
            assert_eq!(server_hash, bogus);
            assert_eq!(client_hash, bogus ^ 1, "client hash must be the twin's true hash");
        }
        other => panic!("expected StateDesync, got {other:?}"),
    }
}

/// Stamped events that PARK in input_buffer (client had no predictions yet)
/// must still verify once predictions exist and the buffer drains.
#[test]
fn parked_stamped_tick_verifies_on_drain() {
    let id = RegionCoords::new(0, 0);
    let (mut server, recv) = runner(id);
    let mut client = Region::from_chunks(id, flat_chunks());

    // Server runs a full interval while the client predicts nothing: every
    // event parks, no check can fire.
    for _ in 0..STATE_HASH_INTERVAL_TICKS {
        server.tick();
    }
    let mut checks = 0u32;
    let mut mispredicts = 0u32;
    for ev in drain_events(&recv) {
        let stats = client.reconcile(ev).expect("parking must not error");
        checks += stats.hash_checks_passed;
        mispredicts += stats.mispredictions;
    }
    assert_eq!(checks, 0, "no predictions yet: nothing may verify");

    // Client catches up with matching predictions; one more server tick
    // triggers a reconcile pass that drains the parked buffer through the
    // match path — including the stamped tick.
    for _ in 0..STATE_HASH_INTERVAL_TICKS {
        client.handle_event(plain_tick()).unwrap();
    }
    server.tick();
    for ev in drain_events(&recv) {
        let stats = client.reconcile(ev).expect("drain must not desync");
        checks += stats.hash_checks_passed;
        mispredicts += stats.mispredictions;
    }
    assert!(checks >= 1, "parked stamped tick must verify on drain");
    assert_eq!(mispredicts, 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p game --test desync_check`
Expected: COMPILE ERROR — `ReconcileStats` has no field `hash_checks_passed`. (After Step 3's stats field alone they would fail on `checks >= 2` / `expect_err` instead.)

- [ ] **Step 3: Implement in `crates/game/src/region.rs`**

3a. Add the field to `ReconcileStats` (`region.rs:76-85`):

```rust
    /// Server hash stamps verified successfully in this pass (periodic
    /// desync check; a failed check is the Err return, not a counter).
    pub hash_checks_passed: u32,
```

3b. Switch the mispredict trigger at `region.rs:228` from full equality to sim-equality:

```rust
                        if !event.sim_eq(&server_event.0) {
```

(was `if event != server_event.0 {`)

3c. Add the check to the MATCH+FORGET branch (`region.rs:341-348`) — the `else` becomes:

```rust
                        } else {
                            debug!(
                                "[reconcile r={:?}] MATCH+FORGET {}",
                                self.id,
                                ev_desc(&server_event.0)
                            );
                            self.data.forget();
                            if let GameEventKind::Tick { state_hash: Some(expected) } =
                                &server_event.0.kind
                            {
                                self.verify_state_hash_at_boundary(
                                    *expected,
                                    &server_event.0,
                                    &mut stats,
                                )?;
                            }
                        }
```

3d. Add the check to the MISPREDICT path, right after `region.rs:262-263` (`self.handle_event(server_event.clone().0.kind)?; self.data.forget();`) — at this exact point the live state is confirmed-through-N with nothing predicted on top:

```rust
                            self.handle_event(server_event.clone().0.kind)?;
                            self.data.forget();
                            if let GameEventKind::Tick { state_hash: Some(expected) } =
                                &server_event.0.kind
                            {
                                self.check_confirmed_hash(
                                    *expected,
                                    &server_event.0,
                                    &mut stats,
                                )?;
                            }
```

3e. Add the two helpers to `impl Region` (next to `evict_orphan_local_inputs`, `region.rs:420`):

```rust
    /// Periodic desync check at the confirmed boundary. `expected` is the
    /// server's hash of its state AFTER `server_event` (a stamped Tick at id
    /// N). The live state carries predictions with ids > N, so materialize
    /// the boundary exactly like `evict_orphan_local_inputs` does: roll every
    /// outstanding prediction back (exact-inverse machinery), hash, then
    /// replay. Rollback rewinds `next_game_event_id`, so replayed events
    /// re-append with identical ids. On mismatch the region is left replayed
    /// and consistent, but the Err crashes the client anyway.
    fn verify_state_hash_at_boundary(
        &mut self,
        expected: u32,
        server_event: &GameEvent,
        stats: &mut ReconcileStats,
    ) -> Result<(), GameError> {
        let kinds: Vec<GameEventKind> =
            self.event_log.iter().map(|e| e.kind.clone()).collect();
        while self.event_log.pop_back().is_some() {
            self.data.rollback();
        }
        let result = self.check_confirmed_hash(expected, server_event, stats);
        for kind in kinds {
            let _ = self.handle_event(kind);
        }
        result
    }

    /// Compare the LIVE state against the server stamp — only valid when the
    /// live state is exactly the confirmed state (mispredict replay just
    /// applied server event N; or all predictions already rolled back).
    fn check_confirmed_hash(
        &mut self,
        expected: u32,
        server_event: &GameEvent,
        stats: &mut ReconcileStats,
    ) -> Result<(), GameError> {
        let client_hash = crate::state_hash(&self.data);
        if client_hash != expected {
            return Err(GameError::StateDesync {
                region: self.id,
                event_id: server_event.id,
                server_hash: expected,
                client_hash,
            });
        }
        stats.hash_checks_passed += 1;
        Ok(())
    }
```

3f. Fold the new field into the client metrics — `crates/client/src/instance.rs`:

In `ReconMetrics` (find the struct definition holding `mispredictions`/`replay_depth_sum`/`max_replay_depth`) add a `pub`-visibility-matching field:

```rust
    hash_checks_passed: u32,
```

In `ReconMetrics::record` (`instance.rs:139-145`) add:

```rust
        self.hash_checks_passed += s.hash_checks_passed;
```

In the manual fold at `instance.rs:814-823` add:

```rust
                                acc.hash_checks_passed += stats.hash_checks_passed;
```

(If `maybe_report_metrics`'s debug line is trivially extensible, append `hash_checks={}`; otherwise leave the report line alone — the counter's job is test assertions.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p game --test desync_check`
Expected: PASS (all 3 tests)

Run: `just test`
Expected: PASS. **If any existing convergence/harness test now fails with `StateDesync`, stop: that is a real determinism gap this check just found. Report it; do not weaken the check.**

- [ ] **Step 5: Check the new tests' runtime**

Run: `cargo test -p game --test desync_check -- --nocapture 2>&1 | tail -5` and time them (`time cargo test -p game --test desync_check`). If any single test exceeds 100ms, tag it `#[ignore = "slow: >100ms — run via just test-full"]`.

- [ ] **Step 6: Commit**

```bash
git add crates/game crates/client
git commit -m "feat(game): client verifies server state-hash stamps at the confirmed boundary"
```

---

### Task 4: Crash plumbing — propagate and terminate loudly

**Files:**
- Modify: `crates/client/src/instance.rs:811-824` (stop swallowing reconcile errors in the pending-replay path)
- Modify: `crates/client/src/main.rs:66-68` (escalate StateDesync to process exit)

**Interfaces:**
- Consumes: `GameError::StateDesync` propagating out of `World::reconcile_event` (Task 3).
- Produces: native client process exits nonzero on desync; wasm already panics via `pump().unwrap()` callers (no change needed there — verify, don't edit).

- [ ] **Step 1: Fix the swallowed error at `instance.rs:815-823`**

The pending-events replay after a region snapshot currently drops reconcile errors. Replace:

```rust
                    if let Some(ref mut world) = self.world {
                        for ev in pending {
                            if let Ok(stats) = world.reconcile_event(ev) {
                                acc.mispredictions += stats.mispredictions;
                                acc.replay_depth_sum += stats.replay_depth_sum;
                                acc.max_replay_depth =
                                    acc.max_replay_depth.max(stats.max_replay_depth);
                                acc.hash_checks_passed += stats.hash_checks_passed;
                            }
                        }
                    }
```

with:

```rust
                    if let Some(ref mut world) = self.world {
                        for ev in pending {
                            // Propagate — a StateDesync here is as fatal as
                            // anywhere else; swallowing it would let a
                            // diverged client keep playing.
                            let stats = world.reconcile_event(ev)?;
                            acc.mispredictions += stats.mispredictions;
                            acc.replay_depth_sum += stats.replay_depth_sum;
                            acc.max_replay_depth =
                                acc.max_replay_depth.max(stats.max_replay_depth);
                            acc.hash_checks_passed += stats.hash_checks_passed;
                        }
                    }
```

(Exact current text may already include the Task 3f `hash_checks_passed` line — keep it.)

- [ ] **Step 2: Escalate in `crates/client/src/main.rs:66-68`**

Replace:

```rust
                    if let Err(e) = manager.connect_and_run() {
                        warn!("Game Crashed: {:?}", e);
                    };
```

with:

```rust
                    if let Err(e) = manager.connect_and_run() {
                        if matches!(e, game::GameError::StateDesync { .. }) {
                            // Crash, period: divergence is always a
                            // determinism bug; a diverged client must not
                            // keep rendering a wrong world.
                            error!("STATE DESYNC — client/server simulation divergence: {e:?}");
                            std::process::exit(1);
                        }
                        warn!("Game Crashed: {:?}", e);
                    };
```

Add `error` to the existing `log`/tracing macro imports in `main.rs` if it isn't already imported (check the `use` block at the top; `warn!` is already in scope from the same source).

Note (spec deviation, intentional): the spec mentions propagating through `ClientUpdateEvent::GameCrash`, but the render bridge only `error!`-logs that event (`renderer/bridge.rs:150`) — it terminates nothing. The actual "crash, period" is this `std::process::exit(1)` at the game-thread exit point; sending an additional GameCrash event would add a second log line and nothing else, so it's omitted.

- [ ] **Step 3: Verify the full suite**

Run: `just test`
Expected: PASS

Run: `just test-full`
Expected: PASS — this is the completion bar. The client-crate convergence harnesses (`cargo test -p client`, included in `just test-full`) now exercise stamping + verification end-to-end through `LocalServer`/`bevy_harness`, because `RegionRunner::tick` stamps unconditionally.

Run: `just clippy`
Expected: clean for the touched crates.

- [ ] **Step 4: Live smoke (manual, native)**

Run `just run` (server + client), play for ≥30 s (several hash intervals across the 3×3 window, move around, place/remove a block), then check the client log: no `STATE DESYNC`, and with `RUST_LOG=debug` the `[reconcile]` DONE lines keep flowing. A desync here is a real bug find — report it.

- [ ] **Step 5: Commit**

```bash
git add crates/client
git commit -m "feat(client): hard-crash the client on StateDesync, stop swallowing pending-replay errors"
```
