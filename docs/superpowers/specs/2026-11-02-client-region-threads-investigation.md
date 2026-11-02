# Client region threads — investigation (2026-11-02)

**Ask:** the client should be multithreaded like the server — the game falls into a rollback loop fairly
quickly at the play region size.

**Status:** option B (client region actors) IMPLEMENTED 2026-11-02 — `crates/client/src/regions/`, plan `docs/superpowers/plans/2026-11-02-client-region-threads.md`; live A/B (`VOXEL_CLIENT_REGION_THREADS=0`) pending. The in-region amplifiers of §5.2 are NOT addressed yet.

## 1. Evidence

`just keepup-budget` (release; one procedural region + one idle player) passes by a wide margin and
does not model play:

| region | tick | worst frame (9 regions + 10-deep replay) | headroom vs 50 ms |
|---|---|---|---|
| 64 m  | 0.053 ms | 1.0 ms | 49.9× |
| 256 m | 0.471 ms | 8.9 ms | 5.6× |

The last real run (`worlds/debug-embarked/server.log`, 2026-11-02 19:23, 256 m regions, fortress
neighbourhood = 9 running regions), per region per 5 s window:

- 8 neighbours: tick avg **3.7–7.4 ms**, max 76–147 ms; fortress region (-1,-3): avg **13.6–17 ms**,
  max 111–130 ms.
- `state_hash` (every 100 ticks): **38–70 ms** per region. `terrain_hash` (~4×/5 s, 40–85 ms) is
  server-only (LOD cadence) and inflates the server averages by ~1.8 ms/tick; the client does not pay it.

The client predicts the same 9 regions (`FORTRESS_NEAR_RADIUS` = 1 → the 3×3 around the fortress) on
**one** thread: ≈ 8 × ~3 ms + ~14 ms ≈ **37–50 ms of sim per 50 ms tick** before any replay. There is
no headroom for a misprediction, so the first one grows the unconfirmed log, which makes the next
replay longer — the death spiral `keepup_budget.rs` was written to catch.

## 2. How the client runs the sim today

`crates/client/src/instance.rs`, one game thread (`start_game_thread` → `connect_and_run`):

- **Tick**: a timer thread sends `GameEventKind::Tick` into an *unbounded* channel every `tick_rate`
  ms; `handle_game_event(Tick)` calls `World::progress_world_one_tick_except`, a serial loop over
  `BTreeMap<RegionId, Region>`, then `apply_local_transfers` (predicted departures/ghosts injected into
  sibling regions), `update_window`, `update_lod_window`, `project_ui`, collider debug lines.
- **Server packets**: `select!` takes ONE packet per iteration. `GameEvent` → `route_server_game_event`
  → `Region::reconcile`; `HashedGameEvent` registers the hash first; `SyncClock` may run
  `align_region` (extra predicted ticks or a stall for one region); `Region(id, snapshot)` rebuilds a
  region.
- **Synchronous home reads** on the game thread: `viewer_world_xz`, `local_player_translation`,
  `local_player_fps_cam_mode`, `project_ui` (`build_hotbar` on home data), input stamping
  (`current_tick(home)` + `handle_region_event(e, home)` returning the predicted `GameEvent` sent to the
  server), `emit_collider_debug_lines` (all regions), `find_player`.

The server's shape (`crates/server/src/region_threads.rs`): one OS thread per running region, owning
a `RegionRunner` (`crates/game/src/region_runner.rs`), self-ticking on `recv_deadline`, talking to the
manager thread only through `RegionInput` / `RegionOutput` crossbeam channels.

### Amplifiers inside a region (independent of threading)

`Region::reconcile` (`crates/game/src/region.rs`):

- every misprediction rolls back the WHOLE unconfirmed log and replays it immediately; several
  mispredictions in one batch of server events each pay a full replay;
- `confirmed()` on a hashed event with predictions on top rolls the whole log back, hashes
  (38–70 ms at 256 m), and replays the whole log again. For 9 regions this is serial: ≈ 9 × (hash +
  2·L ticks of undo/replay) every 5 s — several hundred ms, i.e. a burst of ~10+ queued ticks every 5 s.
- the tick timer's unbounded queue means a stalled game thread comes back to a backlog of `Tick`s,
  each predicted serially, deepening every log at once.

## 3. Can regions run in parallel on the client?

Yes — nothing found that prevents it:

- `Controller: Send` (`crates/game/src/lib.rs`); the server already moves `Region`s onto threads.
- Process-global sim state is atomics / `OnceLock`s (`STATE_HASH_INTERVAL`, hash mode, block props,
  the dwarf rig, rapier's `rigid_contacts_offset`) or thread-local perf counters (`CHUNK_CLONES`,
  `COLLIDER_REBUILDS`) — read-only or metrics, none affects results.
- Each `Region` already owns its render-bridge `Sender<GameDataUpdate>`; the bridge does not care
  which thread sends.
- Regions only interact through the manager (`apply_local_transfers`), exactly as on the server.
  Determinism is per region, so running regions concurrently cannot change any region's result as
  long as the manager applies cross-region steps in the same order.
- Only harnesses read `World` from outside the game thread (`harness.rs`, `bevy_harness.rs`).
- wasm32: the sim runs in one Web Worker (`web_worker/sim.rs`) with no shared-memory threads; any
  design must keep an inline path there.

## 4. Options

### A. Fork-join over the existing `World` (lockstep)

Keep `GameInstanceManager` and `World` as they are; run the per-region parts of each phase in
parallel and join before the manager's cross-region steps:

1. `progress_world_one_tick_except`: all regions' `Tick` in parallel, then `apply_local_transfers`
   serially, as now.
2. Reconcile: drain every pending server packet (not one per `select!`), group consecutive per-region
   ops (`expect_state_hash` + `reconcile`) into per-region queues, run the queues in parallel, merge
   `ReconcileStats`. Packets that touch several regions or the window (`Region` snapshot, `PlayerRegion`,
   `SyncClock` with an align step, `WorldMap`) are barriers: flush the queues, then handle them serially.
3. Executor: scoped threads or a small dedicated pool (`rayon::ThreadPool` sized to the region count —
   `rayon` is already in the client build via worldgen; NOT the global pool the forge uses, NOT Bevy's
   pools). Serial on wasm32.

- **For:** small (World + pump + a pool, roughly `game/src/lib.rs` + `instance.rs`); results are
  bit-identical to today, so every harness and crossing test keeps passing unchanged; per-tick cost
  becomes ≈ the slowest region instead of the sum (≈ 37–50 ms → ≈ 14–17 ms at the numbers above); the
  9 hash-check bursts overlap instead of queueing.
- **Against:** a barrier per phase — home waits for the slowest region every tick, so one neighbour's
  hash-check replay still delays home; does nothing for the browser; not "like the server" in shape.

### B. Client region actors (like the server)

Each predicted region on its own thread owning its `Region`, with its own tick schedule, reconciling
the server events the manager routes to it by region id; the manager becomes a router (window, LOD,
UI, transfers), like `WorldManager`.

- Needs a client-side runner (sibling of `RegionRunner` — that one commits every event; a client one
  predicts + reconciles) and a message vocabulary: `Tick`, `Predict(event) -> GameEvent`,
  `ServerEvent`, `ExpectHash`, `Align(n)`, `Arrival/Ghost`, snapshot replace, shutdown; outputs:
  transfers, reconcile stats, perf, and a per-tick **home view** (player translation, fps-cam mode,
  hotbar projection inputs, current tick) replacing every synchronous home read in §2.
- Input stamping becomes async: the home actor stamps `after_tick` and sends the `ClientPacket`
  itself (it can hold a `Sender` clone), so input never waits on the game thread.
- Harnesses drive the client in lockstep through `pump()`; they need an inline spawner with optional
  seeded jitter, as `WorldManager` has (`InlineSpawner` + `InlineSchedule`), and the client crossing
  tests (`predicted_crossing_*` in `instance.rs`, `tests/crossing.rs`) assume a synchronous
  departure → arrival, which becomes asynchronous (one more misprediction class, converges like the
  server's real-thread handoff that `just test-threads` covers).
- **For:** home never waits for a neighbour; matches the server and its message seam; maps onto
  one Web Worker per region later (the browser's only route to parallel sim).
- **Against:** most of `instance.rs` (3.3k lines) changes; the client's lead control
  (`SyncClock` → `tick_rate` + per-region `stalled`/align) moves into the actors; new failure modes
  (ordering of predicted transfers across threads) to test; weeks, not days.

### A′. Persistent region threads driven in lockstep (A's behaviour, B's shape)

Implement A's executor as one long-lived thread per region (like the server), each owning its
`Region`, driven by messages with the manager waiting for the replies it needs (barrier). Same
results and tests as A; the per-region message vocabulary and the home view are B's, so B is
"stop waiting on the tick barrier" plus async input stamping. Costs more than A up front (the home
reads in §2 go through the view immediately).

## 5. Recommendation

1. **Do A now.** It removes the sum-of-regions cost with no behaviour change and is verifiable by the
   existing tests (`just test-one client "--lib instance::"`, `just test-one client "--test it -- --include-ignored crossing::"`
   (slow), `just soak`) plus a new keep-up benchmark (below). Expect ~3× less sim time per tick at
   the fortress neighbourhood.
2. **Fix the in-region amplifiers in the same effort, they compound with any threading:**
   - reconcile a region's batch of server events with ONE rollback to the first divergence and ONE
     replay, instead of a full replay per misprediction;
   - fold the hash check into that pass (hash at the confirmed point while the log is already rolled
     back) instead of a separate roll back + replay per hashed event;
   - coalesce queued `Tick`s on the client (the timer must not queue a backlog the game thread then
     predicts serially).
3. **Move to B (via A′) when** measurements after step 1 show home stalled by neighbour bursts, or
   when the browser build needs parallel sim (one worker per region).
4. **Separately**, the fortress region costs 13–17 ms/tick idle (vs 0.47 ms for the benchmark's empty
   256 m region) — threading caps the frame at that, so it needs its own profile (people + animator
   colliders, trees, forged terrain are the obvious suspects; measure with `HASH_MODE=off` for A/Bs).

### Benchmark to add first

`keepup_budget.rs` measures the wrong scene. Before changing anything, add a release benchmark that
loads the forged `worlds/debug-embarked` world (or a fixture forged with `ForgeParams::default()`),
builds the fortress region + its 3×3 with the history's people, runs with `HashMode::Full` and the
state-hash interval on, and reports per-region tick cost, the serial sum vs the parallel max, and the
cost of one hashed-event confirmation at log depth 10. That gives the before/after for A and the
fortress profile in step 4.
