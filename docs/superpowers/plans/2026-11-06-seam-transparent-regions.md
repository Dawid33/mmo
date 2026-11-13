# Seam-Transparent Regions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Region seams disappear from gameplay code. Regions share one tick numbering, and transfers land deterministically at T+1. Every component declares its crossing policy. Each region sees a read-only halo of its neighbours. Gameplay writes to anything it does not own go through intents. Seam behaviour is then proved by one seam-translation test harness instead of a special case per system.

**Architecture:** Regions stay the threading, rollback and persistence unit, and **no region ever waits for another**. Rollback state and gameplay code change underneath them in seven phases:
1. One tick numbering: world tick N means the same moment in every region.
2. Stable `EntityId`s and a generated, exhaustively classified transfer bundle.
3. Cross-region messages applied at `max(T+1, receiver's next tick)` and recorded in the receiver's own event stream, which the client replays.
4. A seam-translation harness.
5. A read-only neighbour halo: terrain, then entities.
6. A `WorldView` / `Effects` API with T+1 intents, onto which systems migrate one by one.
7. Dynamic bodies and the wake policy, carried over from the replaced October plan.

**Tech Stack:** Rust (dated nightly in `rust-toolchain.toml`).
- `game`: rollback sim, `#[rollback]` macro, rapier 0.36 / parry 0.31 forks, `libm`.
- `server`: region threads, region store.
- `client`: Bevy 0.19.1, `regions/` host.

**Spec:** `docs/superpowers/specs/2026-11-06-region-seam-crossing-overview-and-prior-art.md`. Part 1 describes today's code, Part 3 the design, and §3.8 the decisions resolved below.

**Replaces:** `docs/superpowers/plans/2026-10-29-entity-seam-crossing.md`, which is now a pointer here:
- its Task 1 is Task 2.2;
- its Tasks 2–7 are Phase 7;
- its Part B is Task 5.1.

## Decisions (spec §3.8, resolved by "go with your recommendations")

1. **Intents apply at T+1 everywhere**, including within one region. Self-writes stay immediate.
2. **Catch-up:** a late region runs up to `MAX_CATCH_UP_BURST` = 5 ticks back to back, then services its inputs, and repeats. It never skips a tick number. If it cannot keep up, it simply runs behind (local time dilation) and the lag is a metric.
3. **Wake policy:**
   - players wake their target;
   - history people are absorbed into the history, as today;
   - every other kind waits in a manager mailbox until its region next runs (Task 7.4).
4. **Halo width:** `HALO_BLOCKS` = 8, with a debug assertion that every registered reach fits inside it (Task 5.1).
5. The October entity-crossing plan is superseded.

## Global Constraints

- **Workspace and commits:**
  - Work on `develop` in the main checkout. **No git worktrees**, not even for subagents (CLAUDE.md).
  - `git add` only the files a task changed. Dawid has uncommitted work in progress (e.g. `pathfind.rs`, `lib.rs`, `ui.rs`, the client renderer). Never commit or revert it. Run `git status` before every commit.
- **Verification:**
  - Run `just test-one <crate> "<target> <filter>"` for the touched tests.
  - Run `just test-threads` after **every** task in Phases 1–3 and 5.
  - Run `just determinism-wasm` after any task that changes hashed or serialized state.
  - Run `just smoke` before each commit.
  - Never run whole suites. Client unit tests and `just test-one client` are a slow opt-in: ask first.
  - Ask before any benchmark, perf capture or live run (Dawid runs his own on this box). A live check uses one client and `adapter=Intel`.
- **Sim code rules:**
  - Only ordered containers (`BTreeMap`/`BTreeSet`/`Vec`), never `HashMap`.
  - No inherent `f32` transcendentals; use `libm::`.
  - Integer ids and orderings only.
  - `game` and `server` stay Bevy-free.
- **Tests and tunables:**
  - A new test slower than 100 ms gets `#[ignore = "slow: >100ms — run via just test-full"]`.
  - Seam tests go in files `just test-threads` already runs: `crates/game/tests/handoff_schedule.rs` and `crates/server/tests/threaded_world.rs`.
  - Tunables are named `pub const`s.
- **Never weaken the server state-hash interval check** (memory: never-skip-hash-verification).
- **CLAUDE.md:** update its `crates/game` / `crates/server` paragraphs in the task that changes the described behaviour, as a separate commit containing only those hunks.

## Review Focus

1. **A parked region is restored minutes later.** Its piles must not all vanish, because pile despawn counts loaded time. Expired slime timers just fire. Test: Task 1.2 `clock_jump_keeps_pile_loaded_time`.
2. **A player crosses while holding a key** (today it oscillates forever). Expected: one crossing, and no rubber-band or eviction. Test: Task 3.3 `held_key_across_seam_settles`.
3. **The source region lags by k ticks when an entity leaves it.** Expected: the entity appears in the target at the target's next tick, is never duplicated or lost, and the client converges. Test: Task 3.4 `lagging_source_transfer_conserved`.
4. **Two crossings within two ticks at a four-region corner.** Expected: one owner per tick, ending in the diagonal region. Test: Task 3.4 `double_crossing_at_corner`.
5. **The target region thread dies with a transfer in flight.** Expected: the transfer is re-delivered after the respawn, and the entity is not lost. Test: Task 3.4 `dead_target_redelivers_in_flight`.

---

## Phase 1: One tick numbering

### Task 1.1: `WorldClock`, start-at-world-tick, `Tick = u64`

**Files:**
- Create: `crates/game/src/world_clock.rs` (+ `mod`/re-export in `crates/game/src/lib.rs`).
- Modify:
  - `crates/game/src/protocol.rs:25`: `pub type Tick = u64;` (bincode and `StateHash` already encode `usize` as 64-bit, so the bytes and hashes are unchanged; `just determinism-wasm` confirms).
  - `crates/game/src/state.rs` `GameData.tick: Tick`.
  - `crates/game/src/region.rs:887` (`current_tick() -> Tick`).
  - `crates/game/src/region_runner.rs` (`RegionInput::StartAt`).
  - `crates/game/src/world_manager.rs` (`RegionSpawner::world_tick`, `ensure_running`, `InlineSpawner`).
  - Every `*self.data.tick as u64` site (`grep -n "data.tick as u64"`).
- Test: `crates/game/tests/world_clock.rs` (new module in `tests/it.rs`), `crates/game/tests/handoff_manager.rs`.

**Interfaces:**
- Produces:
  ```rust
  // world_clock.rs — wall time in ms (the manager's explicit now_ms), never Instant: wasm-safe.
  #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  pub struct WorldClock { pub anchor_ms: u64, pub anchor_tick: Tick }
  impl WorldClock {
      pub fn starting_at(anchor_ms: u64, anchor_tick: Tick) -> Self;
      pub fn tick_at(&self, now_ms: u64) -> Tick;   // anchor_tick + (now_ms - anchor_ms) / TICK_RATE, saturating
      pub fn ms_of(&self, tick: Tick) -> u64;       // inverse: when `tick` is due
  }
  // region_runner.rs
  RegionInput::StartAt(Tick)   // sent FIRST after spawn (before SetMapExtent)
  // world_manager.rs
  trait RegionSpawner { fn world_tick(&self) -> Tick; /* existing methods */ }
  // region.rs
  impl Region { pub fn jump_clock_to(&mut self, tick: Tick); }
  ```
- `jump_clock_to`:
  - is **unlogged** state surgery, allowed only before the region's first tick (`debug_assert!` the event log is empty);
  - is a no-op when `tick <= current_tick()`;
  - otherwise sets `GameData.tick` and calls `Rollback::on_clock_jump(delta)` (Task 1.2).
- `RegionRunner::handle_input(StartAt(t))` calls it.
- `InlineSpawner::world_tick()` = a round counter that `tick_all` advances by one per call.

- [ ] **Step 1: Write the failing tests.**
  - `world_clock.rs::tick_at_counts_tick_rate_periods`: `starting_at(1000, 40)` → `tick_at(1000) == 40`, `tick_at(1049) == 40`, `tick_at(1050) == 41`, `ms_of(41) == 1050`.
  - `handoff_manager.rs::late_started_region_starts_at_world_tick`: lockstep `InlineSpawner`. Run 30 rounds with only the spawn region, then subscribe a neighbour. After the next round both regions report the same `current_tick()`.
- [ ] **Step 2: Run them to see them fail:** `just test-one game "--test it world_clock::"`, then `"--test it handoff_manager::late_started"`.
- [ ] **Step 3: Implement** per the Interfaces block. `ensure_running` sends `StartAt(self.spawner.world_tick())` as the region's first input.
- [ ] **Step 4: Run** both filters, `just test-threads`, `just determinism-wasm` (goldens must be unchanged; if one moves, stop and report — `Tick` width must not change bytes), and `just smoke`.
- [ ] **Step 5: Commit** `feat(game): world tick numbering — regions start at the world tick`.

### Task 1.2: Clock jumps keep loaded-time deadlines; world tick persisted

**Files:**
- Create: `crates/game/src/clock_jump.rs` (`Rollback::on_clock_jump`).
- Modify:
  - `crates/game/src/region_runner.rs` (`restore_saved` is followed by `StartAt` like any spawn).
  - `crates/server/src/region_store.rs`.
  - `crates/server/src/lib.rs` (load and save the world tick next to the region autosave).
- Test: `crates/game/tests/item_piles.rs`, `crates/server/tests/` (the existing region-store test file; add one if there is none, with its own `[[test]]` if it touches process-global state).

**Interfaces:**
- Produces:
  ```rust
  impl Rollback {
      /// Shift deadlines that count LOADED time by `delta` ticks: each pile's `expire_tick`
      /// and `pickup_after_tick` and the `pile_expiry` index. Absolute
      /// deadlines that mean "time passed" (slime timers, cooldowns) are left to expire.
      pub(crate) fn on_clock_jump(&mut self, delta: Tick);
  }
  // region_store.rs
  impl RegionStore {
      pub fn save_world_tick(&self, tick: Tick) -> std::io::Result<()>; // `<dir>/world.tick`, atomic temp + rename
      pub fn load_world_tick(&self) -> Option<Tick>;
  }
  ```
- The server builds its spawner's `WorldClock` as `starting_at(0, store.load_world_tick().unwrap_or(0))`. It calls `save_world_tick(clock.tick_at(now_ms))` in the same burst as every region autosave and at orderly exit. Procedural worlds persist nothing and start at 0.

- [ ] **Step 1: Write the failing tests.**
  - `item_piles.rs::clock_jump_keeps_pile_loaded_time`:
    1. Deposit a pile at tick 100.
    2. Call `on_clock_jump(10_000)` through `Region::jump_clock_to(10_100)`.
    3. Run one tick: the pile still exists.
    4. Run `PILE_DESPAWN_TICKS` more ticks: it is gone.
  - Store test `world_tick_round_trips`: save 123456, then `load_world_tick() == Some(123456)`. A missing file gives `None`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the filters, `just test-threads`, `just smoke`.
- [ ] **Step 5: Commit** `feat: clock jumps keep pile loaded time; world tick persisted with the autosave`.

### Task 1.3: Catch-up pacing on region threads and the inline schedule

**Files:**
- Modify:
  - `crates/server/src/region_threads.rs:118-162` (`region_thread_loop` paces against the shared `WorldClock`; `TickMetrics` gains `lag_ticks`).
  - `crates/server/src/region_threads.rs:42` (`spawn` passes the clock).
  - `crates/game/src/world_manager.rs:228-255` (`InlineSpawner::tick_all`).
- Test: `crates/game/tests/handoff_schedule.rs`, `crates/server/tests/threaded_world.rs`.

**Interfaces:**
- Produces: `pub const MAX_CATCH_UP_BURST: u32 = 5;` in `game::world_clock`.
- The thread loop's deadline for the next tick is `clock.ms_of(region.current_tick() + 1)`, converted against the server's start `Instant` (the same base as the manager's `now_ms`).
  - On timeout it runs ticks while `current_tick() < clock.tick_at(now)`, at most `MAX_CATCH_UP_BURST` before going back to `recv_deadline`.
  - `lag_ticks = tick_at(now) - current_tick()` goes on the region's INFO metrics line. It replaces `overruns`, which skipped ticks.
- `InlineSchedule` "missed tick": a skipped region runs **2** ticks in the next round (catch-up) instead of falling permanently behind. Regions stay within one tick of the round counter.

- [ ] **Step 1: Write the failing tests.**
  - `handoff_schedule.rs::missed_ticks_catch_up_to_world_tick`: run the `jittery` schedule 200 rounds over a 3×3 window. After a final lockstep round, every running region's `current_tick()` equals the spawner's `world_tick()`.
  - `threaded_world.rs::regions_share_tick_numbers_on_real_threads`: run two regions for 1 s. The `SyncClock` ticks they report differ by ≤ `MAX_CATCH_UP_BURST`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-threads` (all existing seam walks must stay green), `just smoke`.
- [ ] **Step 5: Commit** `feat: regions catch up to the world tick instead of skipping`, then the CLAUDE.md commit (region threads paragraph: world tick, catch-up, `lag_ticks`).

---

## Phase 2: Identity and the generated bundle

### Task 2.1: `EntityId`, allocated deterministically

**Files:**
- Modify:
  - `crates/game/src/state.rs` (new `GameData.next_entity_serial`, `#[undo(cell)]`; new component `Ecs.id: Component<EntityId>`; set it in every spawn of a crossing-capable entity: players, slimes, people);
  - `crates/game/src/protocol.rs` (`EntityId`; `GhostData` and `EntityBundle` carry it);
  - `state.rs` `ghosts` / `arrivals` keyed by `EntityId` instead of `(RegionCoords, EntityKey)`;
  - `crates/game/src/protocol.rs:632-650` (`matches_prediction` compares `EntityId`).
- Test: `crates/game/tests/handoff_state.rs`, `handoff_protocol.rs`.

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize, StateHash)]
  pub struct EntityId { pub birth_region: RegionCoords, pub serial: u64 }
  impl Rollback { pub(crate) fn alloc_entity_id_safe(&mut self) -> EntityId; } // undo-logged counter
  impl Rollback { pub fn entity_id(&self, key: EntityKey) -> Option<EntityId>; pub fn entity_by_id(&self, id: EntityId) -> Option<EntityKey>; }
  ```
  `entity_by_id` is a `#[undo(map)] id_index: BTreeMap<EntityId, EntityKey>` kept in step with `Ecs.id`.

- [ ] **Step 1: Write the failing tests.**
  - `handoff_state.rs::entity_id_survives_crossing`: an entity extracted from A and applied in B keeps its `EntityId`, and `entity_by_id` finds it in B, not in A.
  - `handoff_state.rs::predicted_and_authoritative_ids_agree`: two `Rollback`s run the same event sequence (spawn a slime, create a client) and allocate equal `EntityId`s.
  - `handoff_state.rs::undo_spawn_frees_serial`: undo of a spawn restores `next_entity_serial` (`state_hash` before == after).
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** Remove `source_region` / `source_key` from the bundle and from `GhostData`; the identity is `EntityId`. The manager's rebase reads the source region from the departure's own `RegionOutput` framing (it already has the source `rc`).
- [ ] **Step 4: Run** `--test it handoff_`, `just test-threads`, `just determinism-wasm` (scene goldens may move because `GameData` gained fields; re-record only these, saying so in the commit), `just smoke`.
- [ ] **Step 5: Commit** `feat(game): stable EntityId for crossing entities`.

### Task 2.2: Generated, exhaustively classified bundle (absorbs October Task 1)

**Files:**
- Create: `crates/game/src/migrate.rs`.
- Modify:
  - `crates/game/src/protocol.rs` (`EntityBundle`, `BodyState`);
  - `crates/game/src/state.rs` (`extract_entity_safe`, `apply_arrival`, `inject_body_safe`, `collider_spec_of`);
  - `crates/game/src/state.rs:155-166` (`Client::reset_region_local` → `Client::arrive`);
  - `crates/game/src/world_manager.rs` and `crates/client/src/instance.rs` (use `EntityBundle::rebase`).
- Test: `crates/game/tests/handoff_state.rs`.

**Interfaces:**
- Produces:
  ```rust
  // protocol.rs
  pub enum BodyKind { Dynamic, KinematicPosition }
  pub struct BodyState {          // October plan Task 1, unchanged
      pub kind: BodyKind, pub pose: Pose, pub linvel: Vector, pub angvel: Vector,
      pub activation: rapier3d::dynamics::RigidBodyActivation,
      pub linear_damping: f32, pub angular_damping: f32, pub gravity_scale: f32,
      pub ccd: bool, pub locked_axes: rapier3d::dynamics::LockedAxes,
      pub mass: Option<parry3d::mass_properties::MassProperties>,
  }
  pub struct EntityBundle {
      pub id: EntityId, pub kind: EntityKind, pub body: BodyState, pub collider: ColliderSpec,
      pub carried: Carried,
      pub client: Option<(ClientId, Client)>,
      pub pending_inputs: Vec<(GameEventKind, Tick)>,   // filled in Task 3.3, empty until then
  }
  impl EntityBundle { pub fn rebase(&mut self, from: RegionCoords, to: RegionCoords); }
  // migrate.rs
  #[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
  pub struct Carried {
      pub camera: Option<(f32, f32)>,          // yaw, pitch
      pub motion: Option<crate::Motion>,
      pub slime: Option<crate::slime::Slime>,
      pub animator: Option<crate::anim::Animator>,
      pub walker: Option<crate::Walker>,
      pub wander: Option<crate::people::Wander>,
  }
  impl Rollback {
      pub(crate) fn carry_components(&self, key: EntityKey) -> Carried;
      pub(crate) fn restore_components_safe(&mut self, e: EntityKey, c: &Carried, client: Option<ClientId>);
  }
  impl Client { pub fn arrive(self) -> Client; } // replaces reset_region_local
  ```
- **The classification is enforced by exhaustive destructuring.** `carry_components` begins with `let Ecs { entities: _, camera, isometry: _, rigidbody: _, chunk: _, kind: _, motion, slime, animator, bones: _, voxel_body: _, walker, wander, id: _ } = &self.data.ecs;`, with **no `..`**, and a comment on every `_` saying why it does not travel (`local`) or how it is rebuilt.
- `Client::arrive` likewise destructures `Client { .. every field .. }` without `..`, resetting the region-local fields (`mining`, `break_cooldown_until`, `open_container`) and naming each carried one.
- Adding a field to `Ecs` or `Client` then fails to compile until someone classifies it.
- The replay branch of `apply_arrival` (an identity already in `arrivals`) re-applies the **whole** bundle (body state, `Carried`, client), not only the pose.

- [ ] **Step 1: Write the failing tests.**
  - `handoff_state.rs::bundle_round_trip_is_state_exact`: build an entity with motion, slime, animator, walker and a client. Extract it, then apply in a fresh region at the same local pose. Every carried component is bit-equal, and the `Client` equals `client.arrive()`.
  - `handoff_state.rs::replayed_arrival_overwrites_whole_bundle`: apply the arrival, mutate its motion locally, replay the same bundle; motion equals the bundle's.
  - `handoff_state.rs::arrival_closes_open_chest_view`: a client with `open_container` and `inventory_open` arrives with both reset (TODO.md:68).
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test it handoff_`, `just test-threads`, `just determinism-wasm`, `just smoke`.
- [ ] **Step 5: Commit** `refactor(game): generated transfer bundle with exhaustive component classification`.

---

## Phase 3: Transfers land at T+1

**Slot order (both sides implement it; deviations only cost a misprediction).** The *slot* T+1 is the gap after a region's tick T and before its tick T+1. Within a slot:
1. client inputs that were held for it, in arrival order (the rest on receipt);
2. ~~then transfers (arrivals, halo/ghost deltas, intents), in the order they reach the region;~~
   **Amended 2026-11-13 (plan `2026-11-13-seam-slot-order.md`, "Design"):** every region-to-region
   message (`RegionInput::Transfer`; the client's `Arrival` / `HaloEntity` / `Halo` / `Intent` commands)
   goes into the region's `SlotInbox` (`game::slot_order`), never straight into the sim — on the server
   and in the client's predicted regions alike. Immediately before a `Tick` the region applies every
   buffered message with `stamp <= current_tick()`, sorted by `(stamp, class, key, arrival)`: class 0
   `EntityArrived` (entity id), 1 `HaloEntityGone` (source, entity id), 2 `HaloEntity` (source, entity
   id), 3 `HaloUpdate` (source), 4 `HaloSourceGone` (region), 5 `Intent` (intents before replies; an
   intent by its sender, a reply by the entity it is for, then the answering owner, then — replies only —
   its content), 6 anything else (a held creation and what queues behind it), arrival only. So a slot is:
   `Tick`, inputs, the slot's neighbour messages sorted, the next `Tick` — thread timing no longer picks
   the order, and the same events in another arrival order are no longer a misprediction. A message
   whose slot already closed (`stamp < current_tick()`) is applied at the end of the open slot, by its
   stamp first; whether it is late can still differ between server and client (not fixed);
3. a crossing player's own inputs always come **after** that player's arrival. *(As built 2026-11-13:
   whatever follows a buffered arrival — a late input, a park or leave for that body — is pushed with
   `SlotInbox::push_after_arrival` and applied directly after it: after the inputs the arriving bundle
   carries, before the slot's other messages, followers in arrival order. What follows a held creation
   is pushed `Other` behind it, `push_behind`.)*
   *(amended in execution: a halo seed (`HaloUpdate { replace: true }`) sorts in its own class directly after
   `HaloSourceGone`, so a respawned region's seed follows the end of its lost incarnation's mirrors at one stamp.)*

### Task 3.1: Server — `RegionInput::Transfer`, held until the stamped slot

**Files:**
- Modify:
  - `crates/game/src/region_runner.rs` (new input variant; `held` queue generalised; `tick()` emits `Departures` / `GhostUpdates` tagged with the stamp);
  - `crates/game/src/world_manager.rs:1153-1222` (send `Transfer` instead of `Event(EntityArrived/GhostUpdate)`).
- Test: `crates/game/tests/handoff_manager.rs`, `handoff_schedule.rs`.

**Interfaces:**
- Produces:
  ```rust
  RegionInput::Transfer(GameEventKind, Tick)  // apply when current_tick() >= stamp; no MAX_INPUT_LEAD_TICKS cap
  RegionOutput::Departures(Vec<(EntityBundle, RegionCoords)>, Tick)   // stamp = source current_tick() after the tick = T+1
  RegionOutput::GhostUpdates(Vec<(GhostData, RegionCoords)>, Tick)
  ```
- The held queue releases, after each tick, every entry whose stamp is ≤ the new `current_tick()`: held client inputs first, then transfers, each in queue order. A `Transfer` whose stamp is already ≤ `current_tick()` on receipt is applied at once. *(Superseded 2026-11-13 by the amended slot rule above: no `Transfer` is applied on receipt any more; the slot's transfers are applied, sorted, immediately before the next `Tick`.)*

- [ ] **Step 1: Write the failing tests.**
  - `handoff_manager.rs::arrival_lands_in_slot_after_departure_tick`: lockstep. A player walks across the seam. The authoritative event stream of the target shows `EntityArrived` after the target's tick numbered (departure stamp − 1) and before the next `Tick`.
  - `handoff_manager.rs::transfer_to_lagging_target_waits`: hold the target back 3 rounds (`InlineSchedule` with a forced skip for that region). The arrival is applied only after the target has run the stamped tick.
  - `handoff_manager.rs::transfer_to_leading_target_applies_on_receipt`: the source is held back; the arrival is applied at once and appears exactly once.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test it handoff_`, `just test-threads`, `just smoke`.
- [ ] **Step 5: Commit** `feat: cross-region transfers land at T+1 (server)`.

### Task 3.2: Client — predicted transfers held to the same slot

**Files:**
- Modify:
  - `crates/client/src/regions/runner.rs:17-83,169-200` (`RegionCmd::Arrival(EntityBundle, Tick)`, `RegionCmd::Ghost(GhostData, Tick)`, a held queue in `RunnerState` released in slot order after each tick);
  - `crates/client/src/instance.rs:1137-1169` (`route_transfers` passes the stamp; the inline `TakeTransfers` drain is removed — inline relays per `Ticked` report like threads, so both hosts see the same order).
- Test: `crates/client/tests/crossing.rs`, the `crates/client/src/instance.rs` crossing unit tests.

**Interfaces:**
- Consumes: `Departures(.., Tick)` / `GhostUpdates(.., Tick)` stamps (Task 3.1). Client region reports carry the same stamp in `RegionReport::Ticked`.

- [ ] **Step 1: Run the existing red test to see it fail:** `just test-one client "--test it crossing::walking_across_a_region_boundary_keeps_input_and_converges"` (ask Dawid first: it is a client test build).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run** that test and `crossing::crossing_is_deterministic`. Both must pass. Run `crossing_converges_under_late_region_scheduling` with `-- --include-ignored`; it should pass now, so if it stays red, record why in TODO.md and do not mark it fixed. Also run `just test-threads` and `just smoke`.
- [ ] **Step 4: Commit** `feat(client): predicted transfers land in the server's slot`.

### Task 3.3: The crossing player's inputs follow the body; eviction deleted

**Files:**
- Modify:
  - `crates/game/src/region_runner.rs`:
    - at extraction, held inputs of the departing client with stamp ≥ the departure stamp move into `bundle.pending_inputs`;
    - an input for a client that departed this region in the last `MAX_INPUT_LEAD_TICKS` is returned as `RegionOutput::Forward`;
  - `crates/game/src/region.rs` (`apply_arrival` re-queues `pending_inputs` with their stamps right after the arrival);
  - `crates/game/src/world_manager.rs:990-1023` (always honour the stamp: `EventAfterTick(kind, tick)` regardless of `event.region_id`; route `Forward` to the client's current home);
  - `crates/client/src/regions/runner.rs` (`Predict` for a local client whose body has not arrived yet is held and released right after the arrival, replacing the `Misrouted` report);
  - `crates/game/src/region.rs:583-625` (**delete** `evict_orphan_local_inputs`) and `RegionReport::Misrouted` with its re-send path in `instance.rs`.
- Test: `crates/game/tests/handoff_schedule.rs`, `crates/client/tests/crossing.rs`.

**Interfaces:**
- Produces: `RegionOutput::Forward(GameEventKind, Tick)`. The runner keeps a transient (non-state) `departed: BTreeMap<ClientId, (RegionCoords, Tick)>` pruned after `MAX_INPUT_LEAD_TICKS`.

- [ ] **Step 1: Write the failing tests.**
  - `handoff_schedule.rs::held_key_across_seam_settles`:
    1. Hold W across the north seam and keep holding for 3 s.
    2. The player is owned by `north()` from the crossing on (no ownership change back).
    3. Every input event appears exactly once across both regions' authoritative streams.
    4. The `jittery` sweep passes too.
  - `crossing.rs::held_key_crossing_has_no_rubber_band`: the client harness with 100 ms RTT, W held across the seam. Ask before running. The predicted local body pose never moves backwards along the walk axis by more than 1 unit after a reconcile.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement**, including the deletions.
- [ ] **Step 4: Run** the tests, `just test-threads`, `just smoke`, `just determinism-wasm`.
- [ ] **Step 5: Commit** `feat: inputs follow the crossing body; orphan-input eviction removed`. Then update TODO.md (remove the held-input and crossing-red items, if green) and CLAUDE.md (input routing), as a separate commit.

### Task 3.4: Conservation invariant and crossing fault tests

**Files:**
- Modify:
  - `crates/game/src/world_manager.rs` (an in-flight table: `in_flight: BTreeMap<EntityId, (RegionCoords /*target*/, Tick, EntityBundle)>`, entered on `Departures` and cleared when the target's `EventProcessed(EntityArrived)` comes back; a respawned target thread is re-sent its in-flight transfers);
  - `crates/game/tests/handoff_schedule.rs`, `crates/server/tests/threaded_world.rs`.
- Test helper: `fn assert_conserved(rig)`. Every `EntityId` is owned by exactly one running region or appears exactly once in `in_flight`. It is called every round in all seam tests.

**Interfaces:**
- Produces: `pub fn in_flight_count(&self) -> usize` on `WorldManager` (tests + the `[server]` counters line).

- [ ] **Step 1: Write the failing tests** in `handoff_schedule.rs`:
  - `lagging_source_transfer_conserved`: the source skips 5 rounds around the crossing; the invariant holds every round; the client converges.
  - `double_crossing_at_corner`: a diagonal sprint through the −x/−z corner crossing two seams within 2 ticks; one owner per round; ends owned by `north_west()`.
  - `park_target_mid_transfer`: the target is `stopping` when the departure arrives; the entity appears after the resubscribe.
  - `dead_target_redelivers_in_flight`: `InlineSpawner::kill` the target with the transfer unconsumed; after the respawn the entity exists exactly once.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-threads`, `just smoke`.
- [ ] **Step 5: Commit** `feat(game): in-flight transfer table + conservation invariant`.

---

## Phase 4: Seam-translation harness

### Task 4.1: `Scenario` placed centred, across a seam and at a corner

**Files:**
- Create: `crates/game/tests/common/seam_translate.rs`.
- Test: `crates/game/tests/handoff_schedule.rs` (ports `walk_there_and_back` and the adventure walk onto it).

**Interfaces:**
- Produces:
  ```rust
  pub enum Placement { Centre, Seam(Axis), Corner }
  pub trait Scenario { fn setup(&self, rig: &mut Rig, origin: WorldBlockPos); fn drive(&self, rig: &mut Rig, round: u32); fn rounds(&self) -> u32; }
  pub fn world_state_hash(rig: &mut Rig) -> u32;  // every EntityId's world pose + carried state, sorted by id; terrain cells touched
  pub fn assert_seam_invariant<S: Scenario>(s: &S, schedule: Option<fn(u64) -> InlineSchedule>);
  ```
  `WorldBlockPos` is `game::WorldBlockPos { x: i64, y: i32, z: i64 }` (added here in `protocol.rs`, with `from_local(rc, BlockPos)` / `to_local() -> (RegionCoords, BlockPos)`).
- Under lockstep the three placements produce **equal** `world_state_hash`. Under a jittery schedule they produce equal entity sets with no duplication or loss (spec §3.5).

- [ ] **Step 1: Write the tests** `walk_is_seam_invariant` and `adventure_walk_is_seam_invariant` (MoveTo across the placement origin).
- [ ] **Step 2: Run them.** They pass for walking after Phase 3. A failure is a Phase-3 bug, so fix it there, never in the harness. Record any placement still differing in TODO.md with its seed.
- [ ] **Step 3: Commit** `test(game): seam-translation invariance harness`.

From here on, **every new gameplay test uses `assert_seam_invariant`** unless it is region-internal by nature.

---

## Phase 5: Halo

### Task 5.1: Terrain halo (absorbs October Part B)

**Files:**
- Create: `crates/game/src/halo.rs`.
- Modify:
  - `crates/game/src/state.rs` (`GameData.halo: Halo`, `#[undo(map)]` per chunk);
  - `crates/game/src/region_runner.rs` (the boundary outbox carries `HaloDelta`s for chunks within `HALO_BLOCKS` of an edge that changed this tick; at region start the manager seeds the halo from neighbours' `TerrainSnapshot`s — running or parked — or from worldgen, the LOD builder's sourcing);
  - `crates/game/src/edit.rs` (raycast reads the halo; the edit itself stays owner-only until Task 6.2);
  - `crates/game/src/pathfind.rs` (`TileSource` over region ∪ halo).
- Test: `crates/game/tests/halo.rs` (new module).

**Interfaces:**
- Produces:
  ```rust
  pub const HALO_BLOCKS: i32 = 8;
  pub struct HaloDelta { pub chunk: WorldChunkPos, pub data: Option<Chunk> } // None = all air / removed
  impl Rollback {
      pub fn halo_block(&self, p: WorldBlockPos) -> Option<Block>;     // None outside own ∪ halo
      pub(crate) fn apply_halo_delta_safe(&mut self, d: &HaloDelta);   // builds fixed colliders
  }
  ```
- Halo deltas travel as `Transfer`s (Phase 3), so the client replays them.
- **Halo chunks are fixed colliders in the region's physics**, so a body crossing has a floor. `movement::depenetration_lift` stays for real sinks; its seam use is pinned gone by a test.
- A `const _: () = assert!(...)` in `halo.rs`: `edit::REACH` (5.0 blocks, ceil) ≤ `HALO_BLOCKS`, `CONTAINER_REACH_BLOCKS` (6) ≤ `HALO_BLOCKS`, `PICKUP_REACH_H_BLOCKS` (1) ≤ `HALO_BLOCKS`. A new reach constant must be added to it.

- [ ] **Step 1: Write the failing tests.**
  - `halo.rs::neighbour_edit_reaches_halo_next_tick`: break a block 2 m inside `north()` at tick T. The spawn region's `halo_block` shows air from slot T+1.
  - `halo.rs::walk_across_seam_never_dips`: a player walks across a flat seam; the minimum feet y over the walk is ≥ the floor − 0.01 m; the `depenetration_lift` call count is 0.
  - `halo.rs::raycast_hits_neighbour_block`: an eye ray from 3 blocks inside the spawn region hits a pillar 2 blocks inside `east()`.
  - `walk_is_seam_invariant` still passes.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.** Measure and record in the commit message the halo memory per region at `region_blocks=256` from a `just test-one` probe (no live run).
- [ ] **Step 4: Run** the tests, `just test-threads`, `just determinism-wasm`, `just smoke`.
- [ ] **Step 5: Commit** `feat(game): read-only terrain halo of HALO_BLOCKS around each region`.

### Task 5.2: Entity halo replaces ghosts

**Files:**
- Modify:
  - `crates/game/src/state.rs:1454-1526` (ghosts → `HaloEntity` keyed by `EntityId` with kind and shape; delete `GHOST_TTL_TICKS` / `expire_ghosts`: the owner sends an explicit `HaloEntityGone` when the entity leaves the halo band, despawns, parks or crosses);
  - `crates/game/src/protocol.rs` (`GhostData` → `HaloEntityDelta`; `GHOST_MARGIN` → `HALO_BLOCKS`);
  - `crates/client/src/renderer/bridge.rs:1298-1311` (dedupe by `EntityId`).
- Test: `crates/game/tests/halo.rs`, `handoff_scan.rs`.

- [ ] **Step 1: Write the failing tests.**
  - `halo.rs::halo_entity_removed_explicitly_on_park`: the owner region parks while an entity is in its edge band; the neighbour's halo entity is gone in the next slot, not after a TTL.
  - `halo.rs::upgrade_in_place_keeps_key`: the halo entity becomes the arrival under the same `EntityKey` (renderer continuity).
  - Client `crossing.rs::predicted_halo_updates_do_not_mispredict` (ask first): a slime walks along the seam for 5 s; the client mispredict counter stays 0 after the join settles.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests, `just test-threads`, `just determinism-wasm`, `just smoke`.
- [ ] **Step 5: Commit** `feat: entity halo with explicit removal replaces TTL ghosts`.

---

## Phase 6: `WorldView`, `Effects` and intents

### Task 6.1: The substrate API and intent routing

**Files:**
- Create: `crates/game/src/world_view.rs`, `crates/game/src/intents.rs`.
- Modify:
  - `crates/game/src/world_manager.rs` (location directory `locations: BTreeMap<EntityId, RegionCoords>`, updated from arrivals and spawns via a new `RegionOutput::Spawned(Vec<EntityId>)`, and on despawn; routes `RegionOutput::Intents` to the owner as `Transfer`s);
  - `crates/game/src/region.rs` (apply intents at the slot start, sorted by `(source EntityId, seq)`).
- Test: `crates/game/tests/intents.rs` (new module).

**Interfaces:**
- Produces (spec §3.3):
  ```rust
  pub trait WorldView {
      fn tick(&self) -> Tick;
      fn block(&self, p: WorldBlockPos) -> Option<Block>;            // own ∪ halo
      fn entity(&self, id: EntityId) -> Option<EntityRef<'_>>;
      fn entities_in(&self, aabb: WorldAabb) -> Vec<EntityRef<'_>>;  // sorted by EntityId
      fn raycast(&self, ray: WorldRay, max_blocks: f32) -> Option<Hit>;
  }
  pub enum Target { Entity(EntityId), Block(WorldBlockPos), Pile(WorldBlockPos), Container(WorldBlockPos) }
  pub enum Intent { BreakBlock { by: EntityId }, PlaceBlock { block: Block, by: EntityId },
                    TakeFromPile { item: ItemId, count: u32, by: EntityId },
                    PutInContainer { stack: ItemStack, by: EntityId },
                    TakeFromContainer { slot: u8, by: EntityId } }
  pub enum IntentReply { Granted(Intent), Refused(Intent, IntentRefusal) } // delivered to `by` at the next slot
  pub trait Effects { fn send(&mut self, target: Target, intent: Intent); }
  ```
  The `Intent` variants are exactly what Tasks 6.2–6.4 migrate. Add a variant only in the task that needs it.
- Applied **at T+1 even when the target is in the same region** (Decision 1). Conflicts resolve in `(source EntityId, seq)` order, and the loser gets `Refused`.

- [ ] **Step 1: Write the failing tests.**
  - `intents.rs::intent_applies_next_slot_same_region`.
  - `intents.rs::intent_applies_next_slot_across_seam`: same result, same tick, via `assert_seam_invariant`.
  - `intents.rs::conflicting_takes_resolve_by_entity_id`.
  - `intents.rs::intent_to_unloaded_owner_is_refused`: `Refused(.., OwnerNotRunning)`, never lost silently.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests, `just test-threads`, `just determinism-wasm`, `just smoke`.
- [ ] **Step 5: Commit** `feat(game): WorldView/Effects substrate with T+1 intents`.

### Tasks 6.2–6.6: Migrate systems, one task and one commit each

Each task follows the same shape:
1. Port the system to `(&dyn WorldView, &mut Effects)`.
2. Delete its seam special case (spec §1.5, row named).
3. Convert its existing tests to `assert_seam_invariant`.
4. Run the crate filter, `just test-threads`, `just determinism-wasm` and `just smoke`.
5. Commit.

| Task | System | Deletes | Seam test that must pass |
|---|---|---|---|
| 6.2 | Mining + block edits | `edge_ring_locked`, `TileSource::edge_locked`; the owner materialises its own implied ground | `mining::break_block_across_seam`, `block_edit::dig_edge_ring_below_surface` |
| 6.3 | Item piles + pickup | the `settle_cell` clamp; piles keyed by `WorldBlockPos`, owned by the cell's region | `item_piles::throw_across_seam_lands_in_neighbour`, `item_piles::pickup_from_neighbour_pile` |
| 6.4 | Chests | region-local reach; containers addressed by `WorldBlockPos` | `chests::open_chest_across_seam` |
| 6.5 | Pathfinding + walker | `edge_toward`, `MAX_GOAL_REGION_STEP`, `resume_walker_safe` re-plan as walker code → `PathPlan` (coarse region route from `WorldMap` / `ColumnGround`, fine A* in own ∪ halo, re-planned per region inside the substrate) | `adventure::move_to_two_regions_away`, `adventure_walk_is_seam_invariant` |
| 6.6 | People wander, slimes | the wander goal clip, `SLIME_EDGE_MARGIN` / `inward_yaw` | `people::wander_across_seam`, `slime::wanders_across_seam` |

Tree felling (`TIP_EDGE_MARGIN_BLOCKS`) and voxel bodies wait for Phase 7.

---

## Phase 7: Bodies and the wake policy (from the October plan, Tasks 2–7)

These tasks keep the October plan's test lists and interfaces as written there, preserved in git (`git show bc7cdfd:docs/superpowers/plans/2026-10-29-entity-seam-crossing.md`), with these amendments:
- identity is `EntityId`;
- bundles come from Task 2.2;
- ghosts are the Task 5.2 entity halo;
- transfers are Phase 3 `Transfer`s;
- seam tests use `assert_seam_invariant` where a reference-run tolerance used to be (dynamic bodies compare within the October tolerances, since float rounding differs across frames).

| Task | October task | Deliverable |
|---|---|---|
| 7.1 | 2 | `ColliderSpec::{Cuboid, Ball, Compound}` + deterministic explicit mass properties (`mass_props.rs`, `jacobi_eigen_sym3`) |
| 7.2 | 3 | `EntityKind::Body`, `spawn_body_safe`, dynamic upgrade of a halo entity on arrival |
| 7.3 | 4 | tumbling body across a seam and a corner, under jitter and on real threads |
| 7.4 | 5 | manager mailbox: non-player, non-person arrivals into a non-running region wait (Decision 3); `pending_arrival_count` |
| 7.5 | 6 + trees | slimes' leash gone (if not already in 6.6), `Order::DevDropBody`, voxel bodies become migratable (`EntityKind::Body` with the compound shape) and `TIP_EDGE_MARGIN_BLOCKS` is deleted; live check (ask first) |
| 7.6 | 7 | creature colliders from the `.bbmodel` manifest |

Each task needs its own detailed steps before it starts. The executor writes them from the October text and these amendments, then gets them reviewed like any task.
