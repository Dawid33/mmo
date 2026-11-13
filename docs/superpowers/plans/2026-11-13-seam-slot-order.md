# One order for a slot's neighbour messages — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A region applies the messages its neighbours send it for one slot in one canonical order, the same on the server and in the client's prediction, so that the same events in another arrival order are no longer a misprediction.

**Architecture:** A small pure type in `game` (`SlotInbox`) buffers every region-to-region message with its stamp and hands a slot's messages back sorted by a key derived from the message itself. The server's `RegionRunner` stops applying such messages on receipt and after the tick; it applies the closing slot's messages, sorted, immediately before the next `Tick` — which is where the client's runner already applies them. The client's runner switches from receipt order to the same `SlotInbox`. No wire type changes.

**Tech Stack:** Rust; `crates/game` (engine-agnostic, wasm-safe), `crates/client` (`regions/runner.rs`), seeded rigs in `crates/game/tests`.

**Spec:** the finding in `TODO.md` ("the correction bursts are explained (2026-11-13 …)", option 1), the slot rule of `docs/superpowers/plans/2026-11-06-seam-transparent-regions.md` (lines ~281–301), and the "Design" section below, which amends that rule. Evidence: `target/go-nogo-r3/bursts/` (two instrumented probe runs), `target/go-nogo-r3/rounds/` (the 12 go / no-go runs).

## Design

**What happens today** (verified 2026-11-13 at `b328e1d5`; line numbers drift):

- Server, `crates/game/src/region_runner.rs`: `RegionInput::Transfer(kind, stamp)` is applied **on receipt** when `stamp <= current_tick()` (~421–429), else pushed to `held_transfers` and released **after** the `Tick` that opens slot `stamp`, behind the client inputs held for that slot (~772–791), in arrival order. Regions tick at the same wall-clock deadline, so most messages arrive after the target's tick and take the on-receipt path: their place among the slot's events is thread timing.
- Client, `crates/client/src/regions/runner.rs`: `transfer()` holds a message when `stamp >= current_tick()` (~321–334) and releases the held ones with `stamp <= now` at the **start** of `tick()`, before `handle_event(Tick)` (~274–287) — the end of the slot — in receipt order; an older stamp is applied on receipt. Receipt order is the order of `route_reports` (`instance.rs` ~1198–1334): source `(x, z)` order on the inline host (wasm, harness), channel-arrival order across OS threads on the threaded host (native).
- Every applied event takes the next value of the per-region counter `next_game_event_id` (`region.rs` ~760–769), which is rollback, hashed and saved state; reconcile compares server and predicted events by id and full equality (`region.rs` ~480). Same events, other order → misprediction → full rollback and replay, nothing moved.

**The rule** (replaces "transfers … in the order they reach the region" in the seam plan):

1. A slot `S` of a region is the gap after the `Tick` that brings its counter to `S` and before its next `Tick`.
2. Client inputs are applied as today: held ones right after the `Tick` that opens their slot, the rest on receipt, in arrival order.
3. **Every** region-to-region message (`RegionInput::Transfer`, the client's `Arrival` / `HaloEntity` / `Halo` / `Intent` commands) goes into the region's `SlotInbox`, never straight into the sim. Immediately before a `Tick`, the region applies every buffered message with `stamp <= current_tick()` in canonical order. So a slot's events are: `Tick`, inputs, then the slot's neighbour messages sorted, then the next `Tick`.
4. Canonical order: by `stamp`, then class, then the message's own key, then arrival (a stable tie-break; per-source arrival order is deterministic — one FIFO per source):

   | class | kind | key within the class |
   |---|---|---|
   | 0 | `EntityArrived` | entity id (birth region `x`, `z`, serial) |
   | 1 | `HaloEntityGone` | `from` `(x, z)`, entity id |
   | 2 | `HaloEntity` | `source_region` `(x, z)`, entity id |
   | 3 | `HaloUpdate` | `from` `(x, z)` |
   | 4 | `HaloSourceGone` | region `(x, z)` |
   | 5 | `Intent` | arrival only (its `seq` is assigned on receipt, which is now canonical) |
   | 6 | anything else, and anything queued **behind** another entry | arrival only |

   Classes 0–3 are in a source's own emission order (`Departures`, then `HaloEntities` with every `Gone` before every `Update`, then `HaloDeltas`), so one source's messages keep their relative order.
5. An entry that must follow another (a client's event behind its held creation; a crossing player's input behind that player's arrival) is pushed as class 6 with the stamp of the entry it follows: it sorts after every class 0–5 entry of that stamp and keeps arrival order among class 6.

**What does not change:** the tick a message takes effect in (the next `Tick` sees it either way); the wire; the stamps; the intent queue's `(due, source, seq)` order; where the state hash and a save cut's snapshot are taken (after the `Tick`, at the opening of a slot — they now see every message of the closed slot, on both sides alike).

**What does change, on purpose:** the server announces an on-time neighbour message (`EventProcessed`) up to one tick later than today — at the end of its slot instead of on receipt — and event ids within a slot are renumbered. Server and client are built together (no version handshake), so both move at once. Entries that share the buffer today move with it: a client creation or restore held for a save cut (`cut_hold_stamp`, `cut.tick + 1`) is applied at the end of that slot rather than at its start — still after the cut's snapshot, at most one tick later than now.

**Not fixed here** (stated, not hidden): a message that arrives after its slot closed (`stamp < current_tick()`) is applied at the end of the slot that is open, and whether a message is late can still differ between server and client; a message from a region the client does not run is still unpredicted (`tick>transfer`).

## Global Constraints

- Work on `develop` in the main checkout; **no git worktrees**; never `git add -A` (the user has uncommitted work: `deploy/**`, `justfile`, `CLAUDE.md`, `stud/mesh.rs`, `stud_assets.rs`, others) — stage only your own files, and only your own hunks in `justfile` / `CLAUDE.md`.
- `game` stays engine-agnostic and IO-free: no Bevy, no `HashMap` for ordered state (`BTreeMap` / `VecDeque`), no wall clock, no inherent `f32` transcendentals.
- Rollback bar: `hash(before) == hash(after undo)`. The `SlotInbox` is runner state, not `GameData`: it is never hashed, saved or rolled back — a parked region has an empty inbox (drained at `Shutdown`).
- No wire enum changes. Client and server must match and are built together.
- Never run whole suites as a routine check: `just test-one <crate> "--test <target> -- <filters>"` (cargo needs `--` before several filters or flags); `just test-threads` after touching seams; `just determinism-wasm` after touching anything hashed or sim-ordering; `just smoke` last. Tests slower than 100 ms: `#[ignore = "slow: >100ms — run via just test-full"]`.
- Client tests (`just test-one client …`), live runs and measurements: **ask the user first**.
- After the last task run each touched crate's whole non-client suite once (`just test-one game "--test it"`, `just test-threads`, `just test-r2`, `just test-r3`, `just test-one server "--features test-hooks"`), because filtered runs have hidden breakage before.

## Review Focus

1. **A crossing player's inputs land before that player's body.** The body's arrival now waits for the end of the slot; inputs for it arriving meanwhile must queue behind it (class 6, same stamp) and inputs carried in `bundle.pending_inputs` must still be routed right after `EntityArrived`. Test: Task 2 `input_for_a_buffered_arrival_follows_it`.
2. **A save cut at tick T counts a crossing entity twice or not at all.** The snapshot is taken after the `Tick`, and the closed slot's messages are now applied before that `Tick`: the manager's in-flight rule (output-channel order, `save_cut.rs` ~16–24) and the world cut's `InCut` rule must still put each entity in exactly one place. Test: Task 2 `cut_during_a_crossing_holds_each_entity_once_under_every_schedule`.
3. **A region parks with messages still buffered and they are lost.** `Shutdown` must drain the whole inbox in canonical order (before a pending cut's snapshot: only entries with a stamp before the cut tick, as `take_before` does today). Test: Task 2 `shutdown_applies_everything_buffered`.
4. **A late message (its slot already closed) is dropped or applied twice.** It is applied once, at the end of the open slot, ordered by its stamp first. Test: Task 1 `late_entries_sort_by_stamp_first`, Task 2 `late_transfer_lands_at_the_end_of_the_open_slot`.
5. **An existing order-dependent rule breaks silently:** a parked neighbour's reseed must land after a held delta (`halo.rs` `parked_reseed_wins_over_held_delta_in_lagging_neighbour`), an arrival takes over its own mirror (`halo.rs` `upgrade_in_place_keeps_key`), conflicting takes resolve by entity id (`intents.rs` `conflicting_takes_resolve_by_entity_id`), `ListPeople` waits for the latest buffered stamp. Each of those tests stays green unchanged, or its changed expectation is justified in the task report.

---

### Task 1: `game::slot_order` — the inbox and the canonical key

**Files:**
- Create: `crates/game/src/slot_order.rs`
- Modify: `crates/game/src/lib.rs` (`mod slot_order; pub use slot_order::*;` — the crate root glob re-exports)
- Test: unit tests in `slot_order.rs`

**Interfaces:**
- Consumes: `GameEventKind` (`protocol.rs`), `HaloEntityMsg` payloads (`HaloEntityDelta { id, source_region, .. }`, `HaloEntityGone { id, from }`), `HaloUpdate { from, .. }`, `EntityId { birth_region, serial }`, `Tick`.
- Produces:
  - `pub enum SlotClass { Arrival = 0, HaloEntityGone = 1, HaloEntity = 2, HaloUpdate = 3, HaloSourceGone = 4, Intent = 5, Other = 6 }`
  - `pub fn slot_class(kind: &GameEventKind) -> SlotClass`
  - `pub struct SlotInbox` (no `StateHash`, no serde) with:
    - `pub fn new() -> Self`
    - `pub fn push(&mut self, kind: GameEventKind, stamp: Tick)` — class and key from the kind
    - `pub fn push_behind(&mut self, kind: GameEventKind, stamp: Tick)` — class `Other`, whatever the kind
    - `pub fn drain_due(&mut self, now: Tick) -> Vec<GameEventKind>` — every entry with `stamp <= now`, in canonical order (stamp, class, key, arrival); the rest stay
    - `pub fn drain_before(&mut self, tick: Tick) -> Vec<GameEventKind>` — every entry with `stamp < tick`, canonical order
    - `pub fn drain_all(&mut self) -> Vec<GameEventKind>` — canonical order
    - `pub fn latest_stamp(&self) -> Option<Tick>`, `pub fn is_empty(&self) -> bool`, `pub fn len(&self) -> usize`
    - `pub fn holds(&self, pred: impl Fn(&GameEventKind) -> bool) -> Option<Tick>` — the stamp of the first matching entry (what `behind_held_creation` and the client's "arrival held here" checks need)

- [ ] **Step 1: Write the failing tests** (pure; build the event kinds with the smallest real constructors the crate's tests already use):
  - `two_sources_sort_the_same_whatever_the_arrival_order`: `HaloEntity` updates from sources `(-1,0)` and `(0,-1)` pushed in both orders at one stamp → `drain_due` returns the same sequence.
  - `one_source_keeps_its_emission_order`: an arrival, a `Gone`, two `Update`s and a `HaloUpdate` of one source pushed in emission order → returned in that order.
  - `classes_order_arrival_gone_update_delta_intent_other`: one entry per class pushed in reverse → returned in class order.
  - `late_entries_sort_by_stamp_first`: stamps 5 and 4 pushed in that order → the stamp-4 entry first.
  - `push_behind_follows_everything_of_its_stamp`: a `PlayerInput` pushed behind at stamp 7, then an arrival at stamp 7 → arrival first; two pushed behind keep arrival order.
  - `drain_due_leaves_later_stamps`: entries at 3 and 4, `drain_due(3)` returns one, `len() == 1`, `latest_stamp() == Some(4)`.
  - `drain_before_takes_only_earlier_stamps`; `holds_finds_a_buffered_arrival`.
- [ ] **Step 2: Run** `just test-one game "--lib slot_order::"` — FAIL (the module does not exist).
- [ ] **Step 3: Implement.** Entries in a `VecDeque<(Tick, SlotClass, [i64; 5] /* key */, u64 /* arrival counter */, GameEventKind)>`; sort the drained set with `sort_by_key` on the first four fields. Key fields unused by a class are 0.
- [ ] **Step 4: Run** the Step 2 command — PASS; `just smoke`.
- [ ] **Step 5: Commit** `feat(game): SlotInbox — a slot's neighbour messages in one canonical order`.

### Task 2: The server region applies a slot's messages at its end, sorted

**Files:**
- Modify: `crates/game/src/region_runner.rs` (`handle_input` `Transfer` / `Event` / `EventAfterTick` arms, `route_input`, `behind_held_creation`, `tick()`, the `Shutdown` drain and `take_before`, the `ListPeople` deferral)
- Test: `crates/game/tests/region_runner.rs`, `crates/game/tests/handoff_manager.rs`, `crates/game/tests/handoff_schedule.rs`, `crates/game/tests/peer_schedule.rs`, `crates/game/tests/halo.rs`, `crates/game/tests/save_cut.rs` (the names the `it` binary really uses — list with `-- --list`)

**Interfaces:**
- Consumes: `SlotInbox` (Task 1).
- Produces (behaviour, no new public API): `RegionRunner.held_transfers` is replaced by `inbox: SlotInbox`; `RegionInput::Transfer(kind, stamp)` always `inbox.push(kind, stamp)` (or `push_behind(kind, created.max(stamp))` behind a held creation of its client); a client input that must follow a buffered arrival or creation is `push_behind` with that entry's stamp. `tick()` begins with `for kind in inbox.drain_due(current_tick()) { apply_input(kind) }` — before `handle_event(Tick)`; the release of held transfers after the `Tick` is removed (held client inputs keep their place after the `Tick`). `Shutdown`: with a pending cut, `drain_before(cut_tick)` is applied before the snapshot and `drain_all()` after; without one, `drain_all()`. `ListPeople` defers on `inbox.latest_stamp()`.

- [ ] **Step 1: Failing tests.**
  - `region_runner::transfers_of_a_slot_apply_before_the_next_tick_in_canonical_order`: a runner at tick T gets, on receipt (stamp T), a `HaloEntity` from source B then one from source A (A < B) → no `EventProcessed` until `tick()`; then the outputs are `EventProcessed(A's)`, `EventProcessed(B's)`, then the `Tick` event, with consecutive ids.
  - `region_runner::input_for_a_buffered_arrival_follows_it`: an arrival of client C's body (stamp T) then C's `PlayerInput` → after `tick()`: `EntityArrived`, then the input, then `Tick`.
  - `region_runner::late_transfer_lands_at_the_end_of_the_open_slot`: stamp T−1 received at tick T → applied at the next `tick()`, before the `Tick`, ahead of a stamp-T entry pushed earlier.
  - `region_runner::shutdown_applies_everything_buffered`: two buffered arrivals, `Shutdown` → both `EventProcessed` before `Stopped`, and the parked region holds both entities.
  - `handoff_schedule::shuffled_schedule_matches_lockstep`: for each existing handoff scenario and 12 seeds of `InlineSchedule::shuffled`, `Rig::fingerprint()` (`(rc, tick, state_hash)` per region) **equals** the lockstep run's, and each region's emitted event stream (kinds and ids) is identical. (Today only reproducibility per seed is asserted; `late` and `jittery` keep their `perturbed > 0` assertions — they make messages late, which this plan does not order.)
  - `peer_schedule::shuffled_two_hosts_match_lockstep`: the same for a two-host rig at link delay 0.
  - `save_cut::cut_during_a_crossing_holds_each_entity_once_under_every_schedule`: an entity crossing while a cut's tick passes, under lockstep and 12 shuffled seeds → the cut plus in-flight rows hold it exactly once; a rebuild from the cut has one live copy.
- [ ] **Step 2: Run** `just test-one game "--test it -- region_runner::transfers_of_a_slot region_runner::input_for_a_buffered region_runner::late_transfer region_runner::shutdown_applies handoff_schedule::shuffled_schedule peer_schedule::shuffled_two_hosts save_cut::cut_during_a_crossing --include-ignored"` — FAIL.
- [ ] **Step 3: Implement** as in "Interfaces". Keep `held` (client inputs) as it is.
- [ ] **Step 4: Update the tests that pinned the old timing** and say in the report which and why: `handoff_manager` "a leading target applies on receipt" (now: at the end of the open slot), `region_runner::held_inputs_keep_their_arrival_order` (unchanged — inputs), any assertion that an on-receipt transfer "takes the very next id". The Review Focus 5 tests must pass **unchanged**; if one cannot, stop and report it — do not edit its expectation.
- [ ] **Step 5: Run** the Step 2 command, then `just test-one game "--test it -- region_runner:: handoff_manager:: halo:: intents:: save_cut:: people:: --include-ignored"`, `just test-threads`, `just test-r2`, `just test-r3`, `just determinism-wasm`, `just smoke` — PASS.
- [ ] **Step 6: Commit** `feat(game): a region applies a slot's neighbour messages before the next tick, in canonical order`.

### Task 3: The client's predicted regions use the same inbox

**Ask the user before running the client tests.**

**Files:**
- Modify: `crates/client/src/regions/runner.rs` (`transfer()`, `tick()`, `apply_transfer`, `held_transfers`, the "arrival held here" checks for `held_inputs`), `crates/client/src/harness.rs` (a seeded server schedule for the harness's `LocalServer`s)
- Test: `crates/client/tests/crossing.rs` (module of the client's `it` binary), unit tests in `regions/runner.rs`

**Interfaces:**
- Consumes: `SlotInbox` (Task 1); the server behaviour of Task 2.
- Produces: `RunnerState.held_transfers` becomes `inbox: SlotInbox`; `transfer(kind, stamp)` always `inbox.push(kind, stamp)` — the `stamp >= current_tick()` test and the apply-on-receipt branch are removed; `tick()` begins with `for kind in inbox.drain_due(now) { apply_transfer(kind) }` before `handle_event(Tick)`. `SimHarness::with_server_schedule(self, seed: u64) -> Self` — the harness's servers tick their regions in a seeded permuted order (`InlineSchedule::shuffled`, `world_manager/mod.rs` ~198–278) instead of id order.

- [ ] **Step 1: Failing tests.**
  - `regions::runner::tests::routed_messages_apply_in_canonical_order_whatever_the_report_order`: two `HaloEntity` commands from different sources delivered in both orders → the region's event log after the next `Tick` is identical.
  - `crossing::two_neighbours_halo_in_one_slot_does_not_mispredict`: `SimHarness::threaded()` with a home region and two loaded neighbours, a slime walking in each neighbour's border band beside the home, 200 steps → `total_mispredictions()` grows by 0 for the `transfer>transfer` kind (assert on `ReconcileStats::by_kinds[2][2]` summed over the run).
  - `crossing::two_hosts_shuffled_server_order_does_not_mispredict`: `SimHarness::with_hosts(2).with_server_schedule(seed)` for 6 seeds, the same two-band scenario with the neighbours on different hosts → the same assertion.
- [ ] **Step 2: Ask the user**, then run `just test-one client "--lib regions::runner::"` and `just test-one client "--test it -- crossing::two_neighbours crossing::two_hosts_shuffled --include-ignored"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-one client "--test it -- crossing:: --include-ignored"` (the existing crossing tests, including `predicted_halo_updates_do_not_mispredict`, `host_flip_matches_thread_flip_without_delay` and the rubber-band tests, must stay green unchanged), `just test-one client "--lib connections:: instance::manager_tests::"`, `just web-check`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(client): predicted regions apply a slot's neighbour messages in the server's canonical order`.

### Task 4: Documents, the wide run, and the measurement

**Files:**
- Modify: `docs/superpowers/plans/2026-11-06-seam-transparent-regions.md` (the slot rule, ~281–301: point 2 becomes the rule of this plan's "Design", dated), `CLAUDE.md` ("Regions & seams": one line — a slot's neighbour messages go through `SlotInbox` and are applied before the next `Tick` in canonical order, on server and client; never apply a `Transfer` on receipt), `TODO.md` (the burst finding → fixed by this plan; what is left: late messages, unpredicted sources)

- [ ] **Step 1:** Write the documents (own hunks only in `CLAUDE.md`).
- [ ] **Step 2: Run** each touched crate's whole non-client suite once: `just test-one game "--test it"`, `just test-threads`, `just test-r2`, `just test-r3`, `just test-one server "--features test-hooks"`, `just determinism-wasm`, `just web-check`, `just smoke` — PASS. A failure is reported with the test, the text and the commit that caused it; it is not edited away.
- [ ] **Step 3: Commit** `docs: a slot's neighbour messages have one order`.
- [ ] **Step 4: Ask the user before measuring** (the probe opens a game window for ~2.5 minutes per run). With their go-ahead: three rounds of `just adapter=6700 go-nogo-r3 2 60` and three of `just adapter=6700 go-nogo-r3 0 60`, keeping each run's logs. **Expected:** `[client-mispredict]` `transfer>transfer` near zero during the shuttle; the share of crossings with ≥ 5 corrections at 2 ms falls from 7–11 % (pooled, `TODO.md`) to the 0 ms level (≤ 2 %); `confirm_ms` p95 within one tick (50 ms) of the earlier ~100 ms. Compare **pooled** rounds — single runs flip. Record the numbers in `TODO.md`. If the bursts remain, stop and report which mispredict kind they are now (the late-message race is the first suspect) — do not tune the probe.

---

## Self-review notes

- **Spec coverage.** The finding's cause — same-slot `HaloEntity` messages from two sources applied in different orders — is removed on the server (Task 2), on the client (Task 3), by one shared rule (Task 1); the two "not fixed" cases are named in the Design and carried to `TODO.md` (Task 4).
- **Review Focus → tests:** 1 → Task 2 `input_for_a_buffered_arrival_follows_it`; 2 → Task 2 `cut_during_a_crossing…`; 3 → Task 2 `shutdown_applies_everything_buffered`; 4 → Task 1 `late_entries_sort_by_stamp_first`, Task 2 `late_transfer_lands…`; 5 → the three named existing tests, required to pass unchanged in Task 2.
- **Names across tasks:** `SlotInbox`, `SlotClass`, `slot_class`, `push`, `push_behind`, `drain_due`, `drain_before`, `drain_all`, `latest_stamp`, `holds` (Task 1) are the only names Tasks 2 and 3 use.
- **Order of tasks:** between Task 2 and Task 3 the server sorts and the client does not; that is no worse than today (the client reconciles, as it does now) and lasts one task. Do not ship or measure between them.
- **What a reviewer should distrust:** the claim that end-of-slot application changes no sim tick. It holds because both the on-receipt path and the end-of-slot path put the message before the same next `Tick`; `shuffled_schedule_matches_lockstep` is the test that would show otherwise, and the 12 recorded go / no-go runs give the before numbers for the measurement.
