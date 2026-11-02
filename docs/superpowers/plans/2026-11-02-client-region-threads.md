# Client Region Threads (Option B) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the client runs every predicted region on its own OS thread, like the server, so the game
thread never pays the sum of all regions' ticks, reconciles and hash checks.

**Architecture:** `GameInstanceManager` stops owning a `game::World` and talks to its regions only
through a `RegionHost` with a message pair (`RegionCmd` in, `RegionReport` out), the client twin of the
server's `RegionInput`/`RegionOutput`. One pure function, `runner::handle`, executes a command on one
`Region`; the **inline** host calls it synchronously on the caller's thread (harnesses, wasm, A/B
fallback — bit-identical to today's behaviour), the **threaded** host calls it on one thread per region
(native play). The clock stays shared: the manager's tick timer broadcasts `Tick` to every region and
never waits for one; a slow region queues and catches up on its own thread.

**Tech Stack:** Rust, crossbeam channels, std threads; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-02-client-region-threads-investigation.md` (§4 option B).

## Global Constraints

- `game` and `server` stay Bevy-free; everything new lives in `crates/client/src/regions/`.
- Determinism: a region's results must not depend on the host. The inline host must reproduce today's
  order of operations exactly — every existing client test passes UNCHANGED except for the mechanical
  accessor edits Task 3 lists.
- wasm32 builds only the inline host (`#[cfg(not(target_arch = "wasm32"))]` on the threaded one).
- Threaded is the production default on native (`connect_and_run`); `VOXEL_CLIENT_REGION_THREADS=0`
  selects inline there (named knob, for A/B and bisecting).
- No `Instant::now()` on wasm32 (it panics): perf fields stay `cfg(not(target_arch = "wasm32"))`.
- Never run whole suites; verify with `just test-one client "<target> <filter>"` and `just smoke`.
  Client test binaries are a full Bevy build (slow) — the user approved them by approving this plan.
- **No commits.** `instance.rs`, `harness.rs` and other files here already hold the user's
  uncommitted work; leave the diff in the tree for the user to review and commit.

## Review Focus

1. **Input during a predicted crossing (threaded).** A `Predict` can reach the old home after its
   departure was reported but before the manager flipped home. Expected: the input is re-sent to the
   new home, never lost or applied to a region without the body — Task 2 `misrouted_input_after_departure`.
2. **Region released while its thread still works.** `ReleaseRegionConnection` / re-snapshot while the
   old thread has queued ticks. Expected: no panic, no stale `DebugLines`/updates reaching a re-created
   region's bridge, thread joined — Task 4 `stop_then_respawn_same_region`.
3. **Session end with region threads running** (Quit, lost server, Esc → Main menu). Expected: every
   region thread exits and is joined; the game thread can start the next session — Task 4
   `drop_joins_all_threads`.
4. **Reconcile error on a region thread** (today `?` ends the session). Expected: the session still ends
   with that error, not a silent dead thread — Task 2 `failed_reconcile_reports_error` + Task 4 wiring.
5. **Clock steering moved into the runner.** Expected: same step/stall/rate decisions as today for home
   and non-home — Task 1 decision tests pin today's numbers.

---

## File Structure

- Create `crates/client/src/regions/mod.rs` — module root, re-exports.
- Create `crates/client/src/regions/clock.rs` — the lead/band clock decision, moved verbatim out of
  `instance.rs` `handle_server(SyncClock)` into a pure function.
- Create `crates/client/src/regions/runner.rs` — `RegionCmd`, `RegionReport`, `HomeView`,
  `RunnerState`, `RunnerCtx`, `handle()`: everything that touches one `Region`.
- Create `crates/client/src/regions/host.rs` — `RegionHost` (`Inline` / `Threaded`).
- Modify `crates/client/src/instance.rs` — the manager drives regions through the host; all direct
  `world` reads replaced by reports/views.
- Modify `crates/client/src/lib.rs` — `pub mod regions;`.
- Modify `crates/client/src/harness.rs`, `crates/client/tests/reconcile_keepup.rs` — only if an
  accessor name changes (Task 3 keeps `world_ref()`/`world_mut()`, so ideally no change).
- Create `crates/client/tests/region_threads.rs` (module of `tests/it.rs`) — threaded-host convergence.
- Modify `CLAUDE.md`, the spec's status line.

---

### Task 1: Clock decision as a pure function

**Files:**
- Create: `crates/client/src/regions/mod.rs`, `crates/client/src/regions/clock.rs`
- Modify: `crates/client/src/instance.rs` (`handle_server` `SyncClock` arm, constants at ~473-516),
  `crates/client/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct ClockInput { pub server_tick: usize, pub server_rate_ms: u64, pub rtt_ms: u64, pub is_home: bool }
  pub struct ClockDecision {
      /// total_mili_diff: client lead in ms minus rtt (today's `recon.last_drift_ms` for home)
      pub drift_ms: isize,
      /// Some((error_ms, ticks)) when out of band; ticks already capped at MAX_ALIGN_TICKS
      pub step: Option<(isize, usize)>,
      /// Home only: Some((tick_rate_ms, ready)) — today's `tick_rate.store` + `self.ready`
      pub home_rate: Option<(u64, bool)>,
  }
  pub fn decide(client_tick: usize, input: &ClockInput) -> ClockDecision
  ```
  Constants `CLIENT_LEAD_MS`, `CLIENT_LEAD_BAND_MS`, `HOME_STEP_ALIGN_MS`, `HOME_STALL_MIN_MS`,
  `MAX_ALIGN_TICKS` move here as `pub(crate)` with their doc comments, values unchanged.

- [ ] **Step 1: Write the failing tests** in `clock.rs` `#[cfg(test)] mod tests` (TICK_RATE = 50, so
  lead 75 ms, band 50 ms, home step 150 ms, `HOME_STALL_MIN_MS` = 1000, `MAX_ALIGN_TICKS` = 40):
  - `home_in_band_runs_at_server_rate`: client_tick 102, server_tick 100, rate 50, rtt 25, home →
    drift 75, step None, home_rate Some((50, true)).
  - `home_far_behind_steps_and_slows`: client 100, server 110, rtt 0, home → drift −500,
    step Some((−575, 12)), home_rate Some((45, false)).
  - `home_slightly_ahead_is_not_stalled`: client 120, server 100, rtt 0, home → error 925 ≤ 1000 →
    step None, home_rate Some((55, true)).
  - `neighbour_ahead_is_stalled`: same numbers, not home → step Some((925, 19)), home_rate None.
  - `step_is_capped`: client 0, server 1000, home → step ticks == 40.
- [ ] **Step 2: Run** `just test-one client "--lib regions::clock::"` — expect FAIL (module missing).
- [ ] **Step 3: Implement `decide`** by moving the arithmetic of the `SyncClock` arm verbatim
  (`diff * rate − rtt`, `− TEST_INDUCED_LATENCY_MS − CLIENT_LEAD_MS`, `far`, `home_ahead_ok`, rounding
  `(|e| + rate/2) / rate`, the ±10% home rate). The `SyncClock` arm then becomes: look up the region,
  `decide(world.current_tick(&id), &input)`, apply `drift_ms`/`home_rate` to `recon`/`tick_rate`/`ready`,
  and call `align_region(id, error, ticks)` for `step`.
- [ ] **Step 4: Run** `just test-one client "--lib regions::clock::"` and
  `just test-one client "--lib instance::"` — expect PASS (no behaviour change).

---

### Task 2: The region runner (commands on one `Region`)

**Files:**
- Create: `crates/client/src/regions/runner.rs`

**Interfaces:**
- Consumes: `clock::{ClockInput, ClockDecision, decide}`.
- Produces:
  ```rust
  pub enum RegionCmd {
      Tick,                                         // one predicted tick, or sit it out while stalled
      ServerEvent(GameEvent, Option<(usize, u32)>), // + (tick, hash) of a HashedGameEvent
      Predict(GameEventKind),                       // PlayerInput / UiAction / Order from this client
      Arrival(EntityBundle),                        // isometry already rebased into this region
      Ghost(GhostData),                             // ditto
      Clock(ClockInput),
      SetMapExtent(Option<game::MapExtent>),
      DebugLines([f32; 3]),                         // viewer WORLD position
      Ping(u64),
      Stop,
  }
  pub enum RegionReport {
      Ticked { sat_out: bool, departures: Vec<(EntityBundle, RegionCoords)>,
               ghosts: Vec<(GhostData, RegionCoords)>,
               #[cfg(not(target_arch = "wasm32"))] perf: game::RegionTickPerf },
      Reconciled { stats: ReconcileStats, correction_m: Option<f32>,
                   #[cfg(not(target_arch = "wasm32"))] took: Duration },
      Clock(ClockDecision),
      View(Option<HomeView>),                       // None = this region no longer holds our client
      Misrouted(GameEventKind),
      Pong(u64),
      Failed(GameError),
  }
  #[derive(Clone, PartialEq)]
  pub struct HomeView { pub tick: usize, pub translation: Option<[f32; 3]>, pub fps_cam_mode: bool,
                        pub hotbar: game::HotbarView, pub mode: game::PlayMode,
                        pub inventory_open: bool, pub log_len: usize }
  #[derive(Clone)]
  pub struct RunnerCtx { pub id: RegionId, pub client_id: ClientId,
                         pub server_send: Sender<ClientPacket>,
                         pub client_event_send: Sender<ClientUpdateEvent>,
                         pub debug_flags: game::DebugFlags }
  #[derive(Default)]
  pub struct RunnerState { stall: usize, player_departed: bool,
                           collider_debug: game::ColliderDebug, last_view: Option<HomeView> }
  pub fn handle(region: &mut Region, st: &mut RunnerState, ctx: &RunnerCtx,
                cmd: RegionCmd, out: &mut Vec<RegionReport>) -> bool  // false after Stop
  ```

Semantics (each is today's code moved, not new behaviour):
- `Tick`: `stall > 0` → `stall -= 1`, `Ticked { sat_out: true, .. }` with empty transfers; else
  `region.handle_event(Tick)`, then `region.take_transfers()` into `Ticked`; set
  `player_departed = true` if a departure bundle carries `ctx.client_id`; `perf = region.take_tick_perf()`.
- `ServerEvent(e, hash)`: `expect_state_hash` first when `Some`, then `reconcile(e)`; `correction_m` =
  distance between the local body's translation before and after when `stats.mispredictions > 0` and the
  body exists (today's `route_server_game_event` metric); `Err` → `Failed`.
- `Predict(kind)`: if `player_departed` and our client has no body here → `Misrouted(kind)`; else
  `after = region.current_tick()`, `event = region.handle_event(kind)?`, the `info!` log line of today,
  `ctx.server_send.send(ClientPacket::GameEventAfterTick(event, after as u64))`.
- `Arrival`/`Ghost`: `handle_event(EntityArrived/GhostUpdate)`, result ignored (as today); an arrival of
  our client clears `player_departed`.
- `Clock(input)`: `d = decide(region.current_tick(), &input)`; push `Clock(d)`; for `step`:
  `error > 0` → `stall = ticks`; else `stall = 0` and run `ticks` × the `Tick` path, one `Ticked` each.
- `DebugLines(viewer_world)`: only if `ctx.debug_flags.collider_wireframes()`; rebase by
  `ctx.id.world_offset()`, `st.collider_debug.collect(..)`, send `ClientUpdateEvent::DebugLines(id, lines)`.
- `SetMapExtent`: `region.set_map_extent`. `Ping(n)` → `Pong(n)`. `Stop` → return false.
- After every command: compute the view (Some iff `data.clients` has `ctx.client_id`; `translation` from
  the body exactly as `local_player_translation` reads it today; hotbar via `game::build_hotbar`;
  `mode`/`fps_cam_mode`/`inventory_open` from the `Client`; `log_len = region.pending_event_ids().len()`);
  push `View(..)` only when it differs from `st.last_view`.

- [ ] **Step 1: Write the failing tests** in `runner.rs` (build regions with `Region::from_chunks` +
  `CreateClient` as the instance tests' `minimal_world` does; `ctx` with unbounded channels):
  - `tick_reports_once_and_stall_sits_out`: `stall = 2` via `Clock` with a neighbour 925 ms ahead →
    the next two `Tick`s report `sat_out: true` and `current_tick` unchanged; the third advances it.
  - `trailing_clock_runs_align_ticks`: a `Clock` 10 ticks behind (non-home, rtt 0) → 12 `Ticked`
    reports with `sat_out: false` (error −575 ms → 12 ticks) and the tick advanced by 12.
  - `predict_stamps_and_sends`: `Predict(PlayerInput(client, ..))` at tick 5 → the server channel holds
    `GameEventAfterTick(ev, 5)` and `ev.id` == the region's predicted id.
  - `misrouted_input_after_departure`: mark `player_departed` with the client's body removed
    (`ClientLeft`) → `Predict` yields `Misrouted(kind)` and sends nothing.
  - `view_changes_only_when_state_changes`: two `Ping`s in a row after a `Tick` → no `View` report for
    the pings.
  - `failed_reconcile_reports_error`: predict one `Tick` (log id N), then
    `ServerEvent(GameEvent { id: N, region_id, kind: GameEventKind::Quit }, None)` — a mispredict whose
    replay of `Quit` is the one `handle_event` error (`GameError::QuitRequested`) → the reports contain
    `Failed(GameError::QuitRequested)`.
- [ ] **Step 2: Run** `just test-one client "--lib regions::runner::"` — FAIL.
- [ ] **Step 3: Implement** `handle` and the types above.
- [ ] **Step 4: Run** `just test-one client "--lib regions::"` — PASS.

---

### Task 3: Manager on the inline host (no behaviour change)

**Files:**
- Create: `crates/client/src/regions/host.rs` (inline half)
- Modify: `crates/client/src/instance.rs`

**Interfaces:**
- Consumes: Task 2's types and `handle`.
- Produces:
  ```rust
  pub enum RegionHost { Inline(InlineHost), #[cfg(not(target_arch = "wasm32"))] Threaded(ThreadedHost) }
  impl RegionHost {
      pub fn inline() -> Self;
      /// Build the region from the snapshot (bridge `NewRegion` copy + `Region::new`) and start it.
      pub fn spawn(&mut self, ctx: RunnerCtx, snapshot: Rollback, extent: Option<game::MapExtent>);
      pub fn send(&mut self, id: RegionId, cmd: RegionCmd) -> bool;   // false = not held
      pub fn broadcast(&mut self, cmd: impl Fn() -> RegionCmd);       // every region, id order
      pub fn stop(&mut self, id: RegionId) -> bool;                    // true = was held
      pub fn contains(&self, id: &RegionId) -> bool;
      pub fn ids(&self) -> Vec<RegionId>;
      pub fn drain(&mut self) -> Vec<(RegionId, RegionReport)>;        // non-blocking
      pub fn settle(&mut self);                                        // inline: no-op
      pub fn world(&self) -> Option<&game::World>;                     // Inline only
      pub fn world_mut(&mut self) -> Option<&mut game::World>;
  }
  pub struct InlineHost { world: game::World, states: BTreeMap<RegionId, (RunnerState, RunnerCtx)>,
                          queue: Vec<(RegionId, RegionReport)> }
  ```
  `GameInstanceManager` gains `fn drain_reports(&mut self) -> Result<(), GameError>` (routes every
  report; `Failed(e)` → `Err(e)`), and keeps `world_ref()`/`world_mut()` as
  `self.host.world().expect("inline region host")`.

Manager changes (replace each direct `World` use):
- Fields: `world: Option<World>` → `host: RegionHost` + `world_started: bool` (today's
  `world.is_none()` checks); `stalled` and `results_buffer` and `collider_debug` go (runner state);
  add `views: BTreeMap<RegionId, HomeView>`.
- `Tick`: `host.broadcast(|| RegionCmd::Tick)` then `drain_reports()`, then today's
  `update_window` / `update_lod_window` / `project_ui` / debug lines (`DebugLines(viewer_world)` to every
  region when the flag is on). `crossing.home_tick` takes the home `Ticked.sat_out`.
- Transfers: `drain_reports` gathers every `Ticked`'s departures and ghosts of the batch and routes them
  AFTER the batch, all departures (in report order) before all ghosts — today's `apply_local_transfers`
  order — rebasing with `game::rebase_isometry` and sending `Arrival`/`Ghost`; the local player's
  departure flips `home_region` and records `crossing.flip` as today. Routing may itself produce reports
  (arrivals change views): loop `drain` until empty.
- `PlayerInput`/`UiAction`/`Order`: `host.send(home, Predict(kind))` (warn + drop if `!contains(home)`);
  `Misrouted(kind)` → resend to `home_region` if it differs from the reporting region, else warn + drop.
- `GameEvent`/`HashedGameEvent`: `ServerEvent(e, hash)` to the region if held; pending buffering and
  the `buffer`/`is_caught_up` rules unchanged; `Reconciled` feeds `recon.record` and the home
  correction counters; `perf.reconcile_total += took`.
- `SyncClock`: `send(id, Clock(ClockInput{..., is_home}))`; `Clock(d)` report → for home: drift,
  `tick_rate`, `ready`; `step` → `recon.align_steps/ticks`, `home_align_steps`, `crossing.align_home`.
- `Region` snapshot: `host.spawn(ctx, rollback, map_extent)` (the "before PlayerRegion" guard stays in
  the manager), then replay pending as `ServerEvent(e, None)`. `drop_region`: `host.stop` +
  `RemoveRegion` + `views.remove`. `WorldMap`: `SetMapExtent` to every region.
- Views: `local_player_translation`, `local_player_fps_cam_mode`, `viewer_world_xz`, `project_ui`
  (hotbar default when the home view is `None`, play mode only when `Some`, grab = `!inventory_open`)
  and the metrics' home log depth read `views.get(&home)`.
- `pump()` calls `drain_reports()?` after each server packet and each game event.
- In-file tests: replace `manager.world.as_ref().unwrap()` / `.as_mut().unwrap()` with
  `manager.world_ref()` / `manager.world_mut()`; no assertion changes.

- [ ] **Step 1: Implement `InlineHost`** (`handle` called synchronously, reports queued) and switch the
  manager as above.
- [ ] **Step 2: Type-check** `just smoke` — PASS.
- [ ] **Step 3: Run the unchanged suites** (these are the bit-identical guard):
  `just test-one client "--lib instance::"`, `just test-one client "--lib local_server::"`,
  `just test-one client "--test it -- --include-ignored crossing::"`,
  `just test-one client "--test it -- --include-ignored harness_smoke::"`,
  `just test-one client "--test it -- --include-ignored debug_lines::"`,
  `just test-one client "--test it -- --include-ignored session_reset::"`,
  `just test-one client "--test reconcile_keepup"` — all PASS. A failure here is an ordering
  difference from today, not a test to update.

---

### Task 4: Threaded host

**Files:**
- Modify: `crates/client/src/regions/host.rs`

**Interfaces:**
- Produces:
  ```rust
  #[cfg(not(target_arch = "wasm32"))]
  pub struct ThreadedHost { regions: BTreeMap<RegionId, (Sender<RegionCmd>, JoinHandle<Region>)>,
                            report_send: Sender<(RegionId, RegionReport)>,
                            report_recv: Receiver<(RegionId, RegionReport)>, next_ping: u64 }
  impl RegionHost {
      pub fn threaded() -> Self;
      /// The report stream for `select!`; `crossbeam::channel::never()` for inline.
      pub fn report_recv(&self) -> Receiver<(RegionId, RegionReport)>;
  }
  ```
- Thread per region, named `region-<x>,<z>` (so `just perf-summary client --thread region-` works).
  `spawn` moves `ctx` + snapshot + extent onto the thread, which sends the bridge's `NewRegion` copy and
  builds the `Region` there (snapshot cost leaves the game thread), then loops `recv` → `handle` → send
  each report. `stop` sends `Stop` and joins. `settle` sends `Ping(n)` to every region and blocks until
  every `Pong(n)` is seen, queueing other reports it reads meanwhile for the next `drain`.
  `Drop` stops and joins all.

- [ ] **Step 1: Write the failing tests** in `host.rs`:
  - `threaded_tick_matches_inline`: one region, the same snapshot into an inline and a threaded host,
    50 `Tick`s each, then `game::state_hash` of the region taken back with `stop_and_take(id)` equals
    `game::state_hash(inline.world().unwrap().data(&id))`. (`#[cfg(test)] fn stop_and_take(&mut self,
    id) -> Option<Region>`: the thread body returns its `Region`, so `JoinHandle<Region>`.)
  - `stop_then_respawn_same_region`: spawn, queue 20 `Tick`s, `stop`, spawn again with a fresh
    snapshot, `settle` → no panic, `contains` true, one thread for the id.
  - `drop_joins_all_threads`: spawn 3 regions, drop the host → every `JoinHandle` joined (count live
    threads via a static `AtomicUsize` decremented on thread exit, test-only).
- [ ] **Step 2: Run** `just test-one client "--lib regions::host::"` — FAIL.
- [ ] **Step 3: Implement** `ThreadedHost` (thread body returns its `Region` so tests can inspect it).
- [ ] **Step 4: Run** `just test-one client "--lib regions::"` — PASS.

---

### Task 5: Threaded host in play

**Files:**
- Modify: `crates/client/src/instance.rs` (`connect_and_run`, a `pub fn use_region_threads(&mut self)`
  native-only setter, a `pub fn settle_regions(&mut self) -> Result<(), GameError>` harness hook)
- Create: `crates/client/tests/region_threads.rs`, add `mod region_threads;` to `crates/client/tests/it.rs`
- Modify: `crates/client/src/harness.rs` (a constructor option to call `use_region_threads()`)

**Interfaces:**
- `use_region_threads()` must be called before the first snapshot (panics if regions are held).
- `settle_regions()`: loop `host.settle(); drain_reports()?` until a pass routes nothing new.
- `connect_and_run`: `use_region_threads()` unless `VOXEL_CLIENT_REGION_THREADS=0` (logged once:
  `[client] region threads: on|off`); `select!` gains
  `recv(report_recv) -> r => { route it; self.drain_reports()? }`.
- Harness on threads: after each lockstep step it calls `settle_regions()`, and `world_ref()` is not
  available — so the threaded test compares through the runner's views and the server: home tick
  (`HomeView.tick`) and `local_player_translation()`.

- [ ] **Step 1: Write the failing tests** in `region_threads.rs` (tagged slow like `crossing.rs`):
  - `threaded_walk_across_seam_converges`: the `crossing.rs` walk with the threaded harness; after
    releasing input and settling, the client's home equals the server's player region and
    `local_player_translation()` is within 0.01 m of the server's body translation for the same tick
    (reuse `crossing.rs`'s helpers).
  - `threaded_input_survives_crossing`: hold W across the seam; the server's player region changes and
    the server applied every input id the client sent (no `Misrouted` dropped: count `Predict` sends vs
    server-applied `PlayerInput` events).
- [ ] **Step 2: Run** `just test-one client "--test it -- --include-ignored region_threads::"` — FAIL.
- [ ] **Step 3: Implement** the setter, `settle_regions`, the harness option and `connect_and_run` wiring.
- [ ] **Step 4: Run** the Task 5 tests and re-run Task 3's list — all PASS.
- [ ] **Step 5: `just smoke`** — PASS (also type-checks wasm-free paths; run
  `cargo check -p client --target wasm32-unknown-unknown` through `just` if a recipe exists, else
  `nice -n 10 taskset -c 0-15 cargo check -p client --target wasm32-unknown-unknown`) — PASS.

---

### Task 6: Docs + live check hand-off

**Files:**
- Modify: `CLAUDE.md` (client bullet: regions run on `region-x,z` threads via `RegionHost`, inline for
  harnesses/wasm, `VOXEL_CLIENT_REGION_THREADS=0`; the `[client-budget]` `tick` now measures the
  broadcast, the `sim` column the region threads)
- Modify: `docs/superpowers/specs/2026-11-02-client-region-threads-investigation.md` (status line:
  option B implemented, pointer to this plan)

- [ ] **Step 1: Edit both docs.**
- [ ] **Step 2: Report** the live check for the user to run (they own benchmarks on this box):
  `PERF_LOG=1 just run` on the debug-embarked world, compare `[client-budget]` / `mispredicts/s` with
  `VOXEL_CLIENT_REGION_THREADS=0 PERF_LOG=1 just run`.
