# Home-Only Prediction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scope the client's rollback prediction to the home region only; drive all other near regions purely from authoritative server events (no local ticks, no prediction, no rollback), fixing the client clock/backlog runaway.

**Architecture:** Introduce two client region modes — *predicted* (home; local ticks + `event_log` + reconcile) and *replicated* (neighbors; apply-and-forget each server event, `event_log` stays empty). Mode membership lives in a `predicted_regions: BTreeSet<RegionCoords>` on `GameInstanceManager`. Boundary crossings transfer the predicted role: promote the target on the predicted flip, demote the old source on the authoritative `PlayerRegion` confirmation (rolling its predictions back). No protocol, threading, or render-pipeline changes.

**Tech Stack:** Rust, `game` crate (`World`/`Region`, rollback log), `client` crate (`GameInstanceManager`), crossbeam channels.

## Global Constraints

- `game` and `server` crates stay Bevy-free and windowing-free. This plan only touches `game` (pure sim) and `client`.
- Rollback correctness bar is unchanged: `hash(before) == hash(after undo)` bit-exact. Demotion reuses the existing exact-inverse rollback; do not add new undo paths.
- `RegionId` is a type alias for `RegionCoords` — a `BTreeSet<RegionCoords>` is a set of `RegionId`.
- Do not change `progress_world_one_tick` or `take_transfers` signatures — `crates/game/tests/multi_client.rs` depends on them. Add new scoped methods instead.
- Determinism forks (nalgebra/rapier/etc.) are untouched.
- Existing diagnostic instrumentation in `instance.rs` (`tick_ms`, `reconcile_ms`, backlog gauges, `recv_q`) stays for verification.

---

### Task 1: `game` crate — replicated-apply, demotion, and predicted-scoped tick/transfer primitives

**Files:**
- Modify: `crates/game/src/region.rs` (add two `Region` methods after `rollback_last_event`, ~line 404)
- Modify: `crates/game/src/lib.rs` (add four `World` methods near `reconcile_event`/`take_transfers`, ~lines 168-199)
- Test: `crates/game/tests/home_only_prediction.rs` (new)

**Interfaces:**
- Produces:
  - `Region::apply_authoritative(&mut self, kind: GameEventKind) -> Result<GameEvent, GameError>` — advance the sim once, commit it (no rollback capability), and drop the log entry so `event_log` stays empty.
  - `Region::discard_predictions(&mut self)` — roll every predicted event back to the last authoritative state and empty `event_log`.
  - `World::apply_authoritative_event(&mut self, event: GameEvent) -> Result<(), GameError>` — replicated-region routing; ignores events for unloaded regions.
  - `World::discard_region_predictions(&mut self, region_id: &RegionId)` — demotion.
  - `World::progress_predicted_tick(&mut self, predicted: &BTreeSet<RegionId>, results: &mut BTreeMap<RegionId, Result<GameEvent, GameError>>)` — tick only predicted regions.
  - `World::take_predicted_transfers(&mut self, predicted: &BTreeSet<RegionId>) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>)` — drain transfers from predicted regions only.
  - `World::region_event_log_len(&self, region_id: &RegionId) -> Option<usize>` — test/inspection helper.

- [ ] **Step 1: Write the failing test file**

Create `crates/game/tests/home_only_prediction.rs`:

```rust
//! Unit coverage for the home-only-prediction primitives: replicated
//! apply-and-forget keeps event_log empty; demotion rolls predictions back;
//! predicted-scoped tick advances only the chosen regions.

use std::collections::{BTreeMap, BTreeSet};

use game::{GameEvent, GameEventKind, RegionCoords, World};

fn basic_world() -> (World, RegionCoords) {
    let world = World::basic();
    (world, RegionCoords::new(0, 0))
}

#[test]
fn apply_authoritative_advances_without_logging() {
    let (mut world, home) = basic_world();
    let before = world.current_tick(&home);

    // Two authoritative ticks applied as a replicated region would.
    for id in 0..2 {
        world
            .apply_authoritative_event(GameEvent::new(GameEventKind::Tick, id, home))
            .unwrap();
    }

    assert_eq!(world.current_tick(&home), before + 2, "sim advanced");
    assert_eq!(
        world.region_event_log_len(&home),
        Some(0),
        "replicated apply must leave event_log empty"
    );
}

#[test]
fn discard_predictions_rolls_back_to_baseline() {
    let (mut world, home) = basic_world();
    let before = world.current_tick(&home);

    // Build three *predicted* ticks (logged, not forgotten).
    for _ in 0..3 {
        world.handle_region_event(GameEventKind::Tick, home).unwrap();
    }
    assert_eq!(world.region_event_log_len(&home), Some(3));
    assert_eq!(world.current_tick(&home), before + 3);

    world.discard_region_predictions(&home);

    assert_eq!(world.region_event_log_len(&home), Some(0), "log emptied");
    assert_eq!(
        world.current_tick(&home),
        before,
        "state rolled back to authoritative baseline"
    );
}

#[test]
fn progress_predicted_tick_only_ticks_selected() {
    let (mut world, home) = basic_world();
    let before = world.current_tick(&home);
    let mut results: BTreeMap<RegionCoords, _> = BTreeMap::new();

    // Empty set: nothing advances.
    world.progress_predicted_tick(&BTreeSet::new(), &mut results);
    assert_eq!(world.current_tick(&home), before);
    assert!(results.is_empty());

    // Home selected: it advances once.
    let mut predicted = BTreeSet::new();
    predicted.insert(home);
    world.progress_predicted_tick(&predicted, &mut results);
    assert_eq!(world.current_tick(&home), before + 1);
    assert!(results.contains_key(&home));
}
```

- [ ] **Step 2: Run it to verify it fails to compile**

Run: `cargo test -p game --test home_only_prediction`
Expected: FAIL — `no method named apply_authoritative_event / region_event_log_len / discard_region_predictions / progress_predicted_tick found for struct World`.

- [ ] **Step 3: Add the two `Region` methods**

In `crates/game/src/region.rs`, immediately after `rollback_last_event` (the method ending ~line 404, before the closing `}` of `impl Region`):

```rust
    /// Replicated apply: the event is server-authoritative and permanent, and
    /// is NOT a local prediction. Advance the sim (`handle_event` pushes a log
    /// entry and opens a transaction), commit it (`forget`, so no rollback
    /// capability is retained), then drop the just-pushed prediction entry so
    /// `event_log` stays empty. This is the client analogue of the server's
    /// RegionRunner apply-and-forget, but it also keeps the prediction log clean.
    pub fn apply_authoritative(
        &mut self,
        kind: GameEventKind,
    ) -> Result<GameEvent, GameError> {
        let event = self.handle_event(kind)?;
        self.data.forget();
        self.event_log.pop_back();
        Ok(event)
    }

    /// Demotion: drop every locally-predicted event, rolling the sim back to
    /// the last server-confirmed (authoritative) state via exact-inverse
    /// rollback, and empty the prediction log. After this the region holds no
    /// predictions and can be driven purely by authoritative events.
    pub fn discard_predictions(&mut self) {
        while self.event_log.pop_back().is_some() {
            self.data.rollback();
        }
    }
```

- [ ] **Step 4: Add the four `World` methods**

In `crates/game/src/lib.rs`, after `reconcile_event` (~line 199) and alongside `take_transfers`/`progress_world_one_tick`, add:

```rust
    /// Apply a server-authoritative event to a REPLICATED region: advance and
    /// commit it, no logging, no rollback. Ignores events for regions we don't
    /// hold (steady-state window churn), matching `reconcile_event`.
    pub fn apply_authoritative_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let Some(region) = self.regions.get_mut(&event.region_id) else {
            log::debug!(
                "dropping authoritative event for unloaded region {:?}",
                event.region_id
            );
            return Ok(());
        };
        region.apply_authoritative(event.kind)?;
        Ok(())
    }

    /// Demote a region from predicted to replicated: discard its predictions.
    pub fn discard_region_predictions(&mut self, region_id: &RegionId) {
        if let Some(region) = self.regions.get_mut(region_id) {
            region.discard_predictions();
        }
    }

    /// Advance ONLY the predicted regions by one local (client-predicted) tick.
    /// Replicated regions are driven by authoritative events instead, so they
    /// are not ticked here. Same read-after-progress contract as
    /// `progress_world_one_tick`.
    pub fn progress_predicted_tick(
        &mut self,
        predicted: &std::collections::BTreeSet<RegionId>,
        results: &mut BTreeMap<RegionId, Result<GameEvent, GameError>>,
    ) {
        results.clear();
        for (id, region) in &mut self.regions {
            if predicted.contains(id) {
                results.insert(*id, region.handle_event(GameEventKind::Tick));
            }
        }
    }

    /// Drain transfers from ONLY the predicted regions. Replicated regions also
    /// populate `pending_departures` when they apply an authoritative `Tick`,
    /// but the client must NOT act on those — the server relays the
    /// authoritative `EntityArrived`/`GhostUpdate` directly. Only predicted
    /// regions' transfers (the local player's predicted crossing) are relayed.
    pub fn take_predicted_transfers(
        &mut self,
        predicted: &std::collections::BTreeSet<RegionId>,
    ) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>) {
        let mut departures = Vec::new();
        let mut ghosts = Vec::new();
        for (id, region) in &mut self.regions {
            if predicted.contains(id) {
                let (d, g) = region.take_transfers();
                departures.extend(d);
                ghosts.extend(g);
            }
        }
        (departures, ghosts)
    }

    /// Inspection/test helper: length of a region's prediction log, if loaded.
    pub fn region_event_log_len(&self, region_id: &RegionId) -> Option<usize> {
        self.regions.get(region_id).map(|r| r.event_log_len())
    }
```

(`EntityBundle`/`GhostData` are already in scope — `take_transfers` above uses them.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p game --test home_only_prediction`
Expected: PASS (3 tests).

- [ ] **Step 6: Confirm no regression in the sim suite**

Run: `cargo test -p game`
Expected: PASS (existing rollback/multi_client suites still green).

- [ ] **Step 7: Commit**

```bash
git add crates/game/src/region.rs crates/game/src/lib.rs crates/game/tests/home_only_prediction.rs
git commit -m "feat(game): replicated-apply + demotion + predicted-scoped tick primitives"
```

---

### Task 2: client — `predicted_regions` set and mode-based event routing

**Files:**
- Modify: `crates/client/src/instance.rs` (struct field ~line 43, `new()` init ~line 171, `route_server_game_event` ~line 532, pending-replay in `Region` packet handler ~line 653, `PlayerRegion` handler ~line 620)
- Test: `crates/client/src/instance.rs` `mod manager_tests`

**Interfaces:**
- Consumes: `World::apply_authoritative_event`, `World::region_event_log_len` (Task 1).
- Produces: `GameInstanceManager.predicted_regions: BTreeSet<RegionCoords>`; routing where `event.region_id ∈ predicted_regions` reconciles and all else applies-authoritatively.

- [ ] **Step 1: Write the failing test**

Add to `mod manager_tests` in `crates/client/src/instance.rs`:

```rust
    /// A loaded region that is NOT the predicted home applies authoritative
    /// Ticks directly: its prediction log must stay empty (no rollback backlog).
    #[test]
    fn neighbor_region_applies_without_logging() {
        let (mut manager, server_send, server_recv, client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let neighbor = RegionCoords::new(1, 0);
        let world = game::World::basic();

        // Handshake: subscribes the 3x3 window and marks home predicted.
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        server_send.send(world.build_region_server_packet(&neighbor)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());

        // Keep the render-bridge receivers alive (region emits go to a channel).
        let mut keep = Vec::new();
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(_, _, recv) = ev {
                keep.push(recv);
            }
        }

        // Two authoritative Ticks for the neighbor.
        for id in 0..2 {
            server_send
                .send(ServerPacket::GameEvent(game::GameEvent::new(
                    GameEventKind::Tick,
                    id,
                    neighbor,
                )))
                .unwrap();
        }
        assert!(manager.pump(&server_recv).unwrap());

        assert_eq!(
            manager.world_ref().region_event_log_len(&neighbor),
            Some(0),
            "replicated neighbor must not accumulate a prediction log"
        );
        drop(keep);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p client neighbor_region_applies_without_logging`
Expected: FAIL — reconcile pushes the neighbor Ticks into an `event_log`, so `region_event_log_len` is `Some(2)`, not `Some(0)` (and `predicted_regions` doesn't exist yet → compile error first).

- [ ] **Step 3: Add the `predicted_regions` field**

In the `GameInstanceManager` struct, after `home_region` (~line 41):

```rust
    /// Regions the client rollback-predicts (local ticks + event_log +
    /// reconcile). Normally just the home region; transiently the source+target
    /// pair during a boundary crossing. Every other loaded region is
    /// "replicated": server events are applied-and-forgotten, no prediction.
    predicted_regions: std::collections::BTreeSet<RegionCoords>,
```

In `new()`, after `home_region: None,` (~line 170):

```rust
            predicted_regions: std::collections::BTreeSet::new(),
```

- [ ] **Step 4: Mark home predicted in the `PlayerRegion` handler**

In the `ServerPacket::PlayerRegion` arm, after `self.home_region = Some(home);` (~line 622), add:

```rust
                self.predicted_regions.insert(home);
```

- [ ] **Step 5: Route by mode in `route_server_game_event`**

Replace the body of `route_server_game_event` (the `if world.region_exists(...)` block, ~lines 532-553) so the predicted check happens before borrowing `world`:

```rust
    fn route_server_game_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let predicted = self.predicted_regions.contains(&event.region_id);
        let world = self.world.as_mut().expect("routed only when world exists");
        if world.region_exists(&event.region_id) {
            if predicted {
                // Predicted (home) region: full reconcile / rollback path.
                #[cfg(not(target_arch = "wasm32"))]
                let reconcile_start = std::time::Instant::now();
                let stats = match world.reconcile_event(event) {
                    Ok(stats) => stats,
                    Err(e) => {
                        warn!("reconcile failed: {:?}", e);
                        return Err(e);
                    }
                };
                self.recon.record(stats);
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.recon.reconcile_count += 1;
                    self.recon.reconcile_dur_sum += reconcile_start.elapsed();
                }
            } else {
                // Replicated neighbor: apply-and-forget, no prediction log.
                world.apply_authoritative_event(event)?;
            }
            Ok(())
        } else if self.subscribed.contains(&event.region_id) {
```

(The `else if`/`else` tail below is unchanged.)

- [ ] **Step 6: Route pending-snapshot replay by mode too**

In the `ServerPacket::Region` handler, the pending-replay loop currently calls `world.reconcile_event(ev)` directly (~lines 653-665). Replace that loop so it uses the same mode routing (a freshly-loaded neighbor must replicate, not reconcile):

```rust
                if let Some(pending) = pending {
                    for ev in pending {
                        self.route_server_game_event(ev)?;
                    }
                }
```

(Delete the now-unused local `acc`/`ReconcileStats` accumulation for this block — `route_server_game_event` already records reconcile stats for predicted regions.)

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p client neighbor_region_applies_without_logging`
Expected: PASS.

- [ ] **Step 8: Run the full client suite**

Run: `cargo test -p client`
Expected: PASS (existing tests unaffected — home is still predicted/reconciled).

- [ ] **Step 9: Commit**

```bash
git add crates/client/src/instance.rs
git commit -m "feat(client): predicted_regions set + mode-based event routing"
```

---

### Task 3: client — scope local tick and transfers to predicted regions

**Files:**
- Modify: `crates/client/src/instance.rs` (`Tick` arm ~line 244; `apply_local_transfers` ~line 304)
- Test: `crates/client/src/instance.rs` `mod manager_tests`

**Interfaces:**
- Consumes: `World::progress_predicted_tick`, `World::take_predicted_transfers` (Task 1); `predicted_regions` (Task 2).

- [ ] **Step 1: Write the failing test**

Add to `mod manager_tests`:

```rust
    /// A local Tick advances only predicted regions; a loaded replicated
    /// neighbor does not move on the client's local clock.
    #[test]
    fn local_tick_advances_only_predicted() {
        let (mut manager, server_send, server_recv, client_recv, game_send) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let neighbor = RegionCoords::new(1, 0);
        let world = game::World::basic();

        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        server_send.send(world.build_region_server_packet(&neighbor)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());
        let mut keep = Vec::new();
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(_, _, recv) = ev {
                keep.push(recv);
            }
        }

        let home_before = manager.world_ref().current_tick(&home);
        let neighbor_before = manager.world_ref().current_tick(&neighbor);

        // One local tick.
        game_send.send(GameEventKind::Tick).unwrap();
        assert!(manager.pump(&server_recv).unwrap());

        assert_eq!(manager.world_ref().current_tick(&home), home_before + 1);
        assert_eq!(
            manager.world_ref().current_tick(&neighbor),
            neighbor_before,
            "replicated neighbor must not advance on the local clock"
        );
        drop(keep);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p client local_tick_advances_only_predicted`
Expected: FAIL — `progress_world_one_tick` still ticks all regions, so the neighbor also advances.

- [ ] **Step 3: Scope the local tick to predicted regions**

In the `GameEventKind::Tick` arm, replace the `progress_world_one_tick` call (line 247) with the predicted-scoped variant:

```rust
                self.world
                    .as_mut()
                    .unwrap()
                    .progress_predicted_tick(&self.predicted_regions, &mut self.results_buffer);
```

(Leave the surrounding `#[cfg]` timing block intact.)

- [ ] **Step 4: Scope transfers to predicted regions and drop neighbor injection**

Replace `apply_local_transfers` (~lines 304-329) with a version that only drains predicted regions and only acts on the local player's crossing (neighbor arrivals/ghosts come authoritatively from the server):

```rust
    fn apply_local_transfers(&mut self) {
        let predicted = self.predicted_regions.clone();
        let Some(world) = self.world.as_mut() else { return };
        // Only predicted regions produce client-relayed transfers; replicated
        // regions' scan results are ignored (the server relays them).
        let (departures, _ghosts) = world.take_predicted_transfers(&predicted);
        for (bundle, target) in departures {
            let is_local_player =
                bundle.client.as_ref().map(|(c, _)| *c) == self.client_id && self.client_id.is_some();
            // Only the LOCAL player's crossing is predicted locally. Other
            // entities leaving into a (replicated) neighbor are covered by the
            // server's authoritative EntityArrived.
            if !is_local_player {
                continue;
            }
            let source = bundle.source_region;
            // Predicted home flip: promote the target and reroute input now;
            // the server's PlayerRegion push confirms (or corrects) it, and
            // demotes the old source (Task 4).
            self.home_region = Some(target);
            self.predicted_regions.insert(target);
            if world.region_exists(&target) {
                let mut b = bundle;
                b.isometry = game::rebase_isometry(&b.isometry, source, target);
                let _ = world.handle_region_event(GameEventKind::EntityArrived(b), target);
            }
        }
    }
```

Note: the `self.predicted_regions.insert(target)` and `self.home_region = Some(target)` write to `self` while `world` is borrowed — reorder so the `world` borrow is released first if the borrow checker objects. Concretely, collect the local-player `(bundle, target, source)` first, then drop `world`, then mutate `self` and re-borrow `world` for the injection. If needed, structure as:

```rust
    fn apply_local_transfers(&mut self) {
        let predicted = self.predicted_regions.clone();
        let departures = {
            let Some(world) = self.world.as_mut() else { return };
            world.take_predicted_transfers(&predicted).0
        };
        for (bundle, target) in departures {
            let is_local_player =
                bundle.client.as_ref().map(|(c, _)| *c) == self.client_id && self.client_id.is_some();
            if !is_local_player {
                continue;
            }
            let source = bundle.source_region;
            self.home_region = Some(target);
            self.predicted_regions.insert(target);
            let Some(world) = self.world.as_mut() else { return };
            if world.region_exists(&target) {
                let mut b = bundle;
                b.isometry = game::rebase_isometry(&b.isometry, source, target);
                let _ = world.handle_region_event(GameEventKind::EntityArrived(b), target);
            }
        }
    }
```

Use whichever of the two compiles cleanly; the second avoids the borrow conflict.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p client local_tick_advances_only_predicted`
Expected: PASS.

- [ ] **Step 6: Run the full client suite**

Run: `cargo test -p client`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/client/src/instance.rs
git commit -m "feat(client): tick + transfers scoped to predicted regions"
```

---

### Task 4: client — demote non-home predicted regions on authoritative flip

**Files:**
- Modify: `crates/client/src/instance.rs` (`PlayerRegion` handler ~line 620)
- Test: `crates/client/src/instance.rs` `mod manager_tests`

**Interfaces:**
- Consumes: `World::discard_region_predictions` (Task 1); `predicted_regions` (Task 2).

- [ ] **Step 1: Write the failing test**

Add to `mod manager_tests`:

```rust
    /// When the authoritative home changes, every predicted region that is not
    /// the new home is demoted: removed from predicted_regions and its
    /// prediction log rolled back to empty.
    #[test]
    fn authoritative_flip_demotes_old_home() {
        let (mut manager, server_send, server_recv, client_recv, game_send) = manager_with_player();
        let a = RegionCoords::new(0, 0);
        let b = RegionCoords::new(1, 0);
        let world = game::World::basic();

        server_send.send(ServerPacket::PlayerRegion(Some(a), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&a)).unwrap();
        server_send.send(world.build_region_server_packet(&b)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());
        let mut keep = Vec::new();
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(_, _, recv) = ev {
                keep.push(recv);
            }
        }

        // Simulate a predicted flip A->B: promote B, build a prediction in A.
        manager.predicted_regions_mut().insert(b);
        game_send.send(GameEventKind::Tick).unwrap(); // ticks A and B (both predicted)
        assert!(manager.pump(&server_recv).unwrap());
        assert!(manager.world_ref().region_event_log_len(&a).unwrap() > 0);

        // Server confirms home is now B.
        server_send.send(ServerPacket::PlayerRegion(Some(b), 0)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());

        assert!(
            !manager.predicted_regions_ref().contains(&a),
            "old home A demoted out of predicted_regions"
        );
        assert!(manager.predicted_regions_ref().contains(&b), "B stays predicted");
        assert_eq!(
            manager.world_ref().region_event_log_len(&a),
            Some(0),
            "A's predictions rolled back on demotion"
        );
        drop(keep);
    }
```

- [ ] **Step 2: Add the test-only accessors, then run to verify it fails**

Add near the other test/harness hooks in `impl GameInstanceManager` (e.g. after `world_ref`, ~line 197):

```rust
    /// Test/harness hook: mutable access to the predicted-region set.
    #[cfg(test)]
    pub fn predicted_regions_mut(&mut self) -> &mut std::collections::BTreeSet<RegionCoords> {
        &mut self.predicted_regions
    }

    /// Test/harness hook: read-only access to the predicted-region set.
    #[cfg(test)]
    pub fn predicted_regions_ref(&self) -> &std::collections::BTreeSet<RegionCoords> {
        &self.predicted_regions
    }
```

Run: `cargo test -p client authoritative_flip_demotes_old_home`
Expected: FAIL — after the second `PlayerRegion`, A is still in `predicted_regions` and its log is non-empty (no demotion yet).

- [ ] **Step 3: Demote stale predicted regions in the `PlayerRegion` handler**

In the `ServerPacket::PlayerRegion` arm, replace the home-set lines (~lines 620-622):

```rust
                let home = id.unwrap_or(RegionCoords::new(0, 0));
                let first_home = self.home_region.is_none();
                self.home_region = Some(home);
```

with:

```rust
                let home = id.unwrap_or(RegionCoords::new(0, 0));
                let first_home = self.home_region.is_none();
                self.home_region = Some(home);
                // Authoritative flip: demote every predicted region that is not
                // the confirmed home (the old source after a crossing, or a
                // mispredicted target). Roll its predictions back and drop it to
                // replicated. Then ensure the new home is predicted.
                let stale: Vec<RegionCoords> = self
                    .predicted_regions
                    .iter()
                    .copied()
                    .filter(|rc| *rc != home)
                    .collect();
                if let Some(world) = self.world.as_mut() {
                    for rc in &stale {
                        world.discard_region_predictions(rc);
                    }
                }
                for rc in stale {
                    self.predicted_regions.remove(&rc);
                }
                self.predicted_regions.insert(home);
```

(Remove the now-redundant `self.predicted_regions.insert(home);` line added in Task 2 Step 4, since it is folded in here.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p client authoritative_flip_demotes_old_home`
Expected: PASS.

- [ ] **Step 5: Run the full client suite**

Run: `cargo test -p client`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/instance.rs
git commit -m "feat(client): demote stale predicted regions on authoritative flip"
```

---

### Task 5: client — restrict clock/drift control to the home region

**Files:**
- Modify: `crates/client/src/instance.rs` (`SyncClock` handler ~line 577)
- Test: `crates/client/src/instance.rs` `mod manager_tests`

**Interfaces:**
- Consumes: `home_region`, `tick_rate` (existing).

- [ ] **Step 1: Write the failing test**

Add to `mod manager_tests`:

```rust
    /// Only the home region's SyncClock tunes the client tick rate; a
    /// neighbor's SyncClock must not move the clock.
    #[test]
    fn only_home_syncclock_tunes_tick_rate() {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        let (mut manager, server_send, server_recv, client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let neighbor = RegionCoords::new(1, 0);
        let world = game::World::basic();

        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        server_send.send(world.build_region_server_packet(&neighbor)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());
        let mut keep = Vec::new();
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(_, _, recv) = ev {
                keep.push(recv);
            }
        }

        let rate_before = manager.tick_rate_for_test().load(Ordering::SeqCst);
        // A neighbor SyncClock reporting a large lag must NOT change the rate.
        server_send
            .send(ServerPacket::SyncClock(neighbor, game::TICK_RATE, 9999, Duration::ZERO))
            .unwrap();
        assert!(manager.pump(&server_recv).unwrap());
        assert_eq!(
            manager.tick_rate_for_test().load(Ordering::SeqCst),
            rate_before,
            "neighbor SyncClock must not tune the clock"
        );
        drop(keep);
    }
```

- [ ] **Step 2: Add the test accessor, then run to verify it fails**

Add near the other `#[cfg(test)]` hooks:

```rust
    /// Test/harness hook: the shared adaptive tick-rate handle.
    #[cfg(test)]
    pub fn tick_rate_for_test(&self) -> Arc<AtomicU64> {
        self.tick_rate.clone()
    }
```

Run: `cargo test -p client only_home_syncclock_tunes_tick_rate`
Expected: FAIL — the neighbor SyncClock (server_tick 9999 vs client's low tick) currently retunes `tick_rate`.

- [ ] **Step 3: Gate the SyncClock tuning on the home region**

In the `ServerPacket::SyncClock` arm, tighten the existing guard (~line 577). Change:

```rust
                if let Some(ref mut world) = self.world.as_mut().filter(|w| w.region_exists(&region_id)) {
```

to also require the region to be home:

```rust
                if self.home_region == Some(region_id)
                    && self.world.as_ref().is_some_and(|w| w.region_exists(&region_id))
                {
                    let world = self.world.as_ref().unwrap();
```

Adjust the inner body accordingly: `current_tick` reads through `&world`, and the `self.ready`/`self.tick_rate`/`self.recon` writes no longer conflict with a mutable `world` borrow (the read of `current_tick` ends before them). If the borrow checker objects, capture `let client_tick = self.world.as_ref().unwrap().current_tick(&region_id);` up front and drop the `world` binding before the `self.*` writes. The tuning arithmetic (diff/milisecond_diff/total_mili_diff and the three tick_rate branches) is unchanged.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p client only_home_syncclock_tunes_tick_rate`
Expected: PASS.

- [ ] **Step 5: Run the full client suite**

Run: `cargo test -p client`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/instance.rs
git commit -m "feat(client): drive clock/drift control from the home region only"
```

---

### Task 6: verification — suites green + live metrics bounded

**Files:** none (validation only).

- [ ] **Step 1: Full first-party test suites**

Run: `cargo test -p game && cargo test -p client`
Expected: PASS. (Do not run `cargo test --workspace` — it fails compiling the vendored nalgebra fork's own tests.)

- [ ] **Step 2: Clippy on the touched crates**

Run: `just clippy` (or `cargo clippy -p game -p client`)
Expected: no new warnings in `game`/`client` beyond the pre-existing `StandardVoxelMaterial` visibility warning.

- [ ] **Step 3: Live run — confirm the runaway is gone**

Run `just run`, connect one client, move around and cross a couple of region boundaries for ~30s, and read the `[client]` metrics lines. Expected (contrast with the pre-fix logs where `recv_q` climbed 282→4397 and `reconcile_ms` 0→129):
- `recv_q` stays flat / near zero (not monotonically growing).
- `drift` stabilizes (does not climb without bound).
- `evlog` sum/max stays small (only the home region's shallow lead; transiently 2 during a crossing).
- `reconcile_ms` stays low; `mispredicts/s` low.
- `tps` actual ≈ target.

- [ ] **Step 4: Live run — crossing correctness**

While moving across boundaries, confirm: movement stays responsive, no rubber-banding, neighbor entities render smoothly, and the local player does not briefly double-render at a crossing.

- [ ] **Step 5: Commit any doc/cleanup**

If the diagnostic instrumentation is to be trimmed, do it now in a separate commit; otherwise leave it. Update `TODO.md` if it tracks this item.

```bash
git add -A
git commit -m "chore(client): home-only prediction verification notes"
```

---

## Self-Review

**Spec coverage:**
- Two region modes (predicted/replicated) → Tasks 1-2. ✓
- Local tick only home → Task 3. ✓
- Event routing by mode → Task 2. ✓
- Clock/drift home-only → Task 5. ✓
- Predicted transfers narrow to local player → Task 3. ✓
- Promotion on predicted flip → Task 3 (apply_local_transfers). ✓
- Demotion on authoritative flip (rollback) → Task 4. ✓
- Orphan inputs unchanged (reused) → no task needed (reconcile path unchanged for home). ✓
- Unchanged protocol/render/threading → nothing to do. ✓
- Verification via metrics + suites → Task 6. ✓

**Type consistency:** `predicted_regions: BTreeSet<RegionCoords>` used consistently; `apply_authoritative`/`apply_authoritative_event`/`discard_predictions`/`discard_region_predictions`/`progress_predicted_tick`/`take_predicted_transfers`/`region_event_log_len` names match between Task 1 definitions and Tasks 2-5 call sites. `RegionId == RegionCoords`. `tick_rate: Arc<AtomicU64>`.

**Placeholder scan:** no TBD/TODO; every code step has full code and exact commands.
