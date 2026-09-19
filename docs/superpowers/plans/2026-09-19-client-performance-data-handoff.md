# Client Performance Analysis — Data Collection Handoff

**Date:** 2026-09-19

**Branch:** `develop`

**HEAD at handoff:** `c1c497a` (`feat(client): world-size knobs, join robustness, capped builds and smoke check`)

**Status:** Groundwork landed (small worlds, join robustness, capped builds, smoke check), and
the measurement tooling below is now built (see "Progress"). No performance data has been
collected yet, and no optimisation has started. The next session's
job is to make the client, server and game **produce performance data an LLM session can read
and analyse**, then use it.

## Progress — instrumentation landed (2026-09-19, second session)

Steps 2 and 3 of "Suggested order" are implemented (uncommitted on `develop` at the time of
writing — check `git log`). **Step 1 and step 4 still need the user: no capture has been run
with a real client yet.** Gap status:

| Gap | Status |
|---|---|
| 1 single capture | **Done.** `just perf-capture` → `target/perf-capture/<utc>-<adapter>-<world>/` with `summary.txt` (read first), `manifest.json`, `client.log`, `server.log`, `rss.csv`, `gpu.csv`, `diag.txt`. `scripts/perf_capture.sh` orchestrates, `scripts/perf_sample.py` samples, `scripts/perf_capture_report.py` summarises (re-runnable on an old directory). `PERF_CAPTURE_SECONDS=<n>` makes it hands-off. |
| 2 join phase invisible | **Done.** `[perf] event=… t_ms=…` timeline on both sides (`crates/game/src/perf.rs`; event list in `CLAUDE.md` → Profiling). `process_start` carries `unix_ms`, so the report merges client and server onto one clock. `load_progress` heartbeats every 2 s until `world_ready` — the evidence for a load that never completes. `just trace-join` / `trace_summary.py --until-ms` summarise only the start of a Chrome trace, with a `max ms` column. |
| 3 budget line hidden | **Done.** `[client-budget]` and `[client] mispredicts/s` moved to their own log target `perf_budget`; `PERF_LOG=1` (set by `perf-capture`) raises only that target to DEBUG. `RUST_LOG` replaces Bevy's whole filter, so it was not used. |
| 4 main-thread join burst | **Instrumented, not yet measured.** `game::perf::StallGuard` reports any call of `drain_client_updates`, `mark_remesh_dirty`, `apply_lod_events` over 8 ms as `main_stall system=… ms=… work=…`; `bridge_region_spawned` carries `spawn_ms` per region. Still hypotheses until a capture exists. |
| 5 server visibility | **Done.** Periodic `[server]` line (`crates/server/src/perf.rs`): manager-thread worldgen count/ms, LOD cache hits / submitted / built / in-flight / waiting-on-terrain, LOD submit→result latency vs worker build time, writer-task packets / bytes / send ms / `timeouts`, and the three channel backlogs. Per-event: `client_connected`, `region_generate`, `region_genesis`, `snapshot_sent` (bytes, serialize ms, send ms, timed_out). |
| 6 no memory data | **Done.** `rss.csv`: 1 Hz VmRSS / VmHWM / threads / CPU ticks for both processes; the summary prints peak RSS, CPU % and an RSS-over-time series. |
| 7 iGPU GPU signal | **Done without root.** `gpu.csv` reads the client's DRM fdinfo (`/proc/<pid>/fdinfo`): per-engine busy ns → busy %, plus GPU memory. Verified live on this box's i915; amdgpu exposes the same; **nouveau/NVK exposes no engine time** (the T600 gets `diag` timings only). sysfs adds amdgpu `gpu_busy_percent`/VRAM and i915 clock frequency. `intel_gpu_top` is not needed. |
| 8 raw artefacts too big | **Done.** `scripts/perf_summary.py` + `just perf-summary <side> [--thread … --prefix …]`; `perf-client`/`perf-server` print it. |
| 9 trace-chrome uncapped | **Done.** Builds under `{{cap}}`, launches the binary directly. |

Also: `diag_summary.py --timeline` prints one row per DIAG dump (frame ms, GPU ms, costliest
pass), so GPU cost *during* the join is visible, not just the last steady-state dump.

### Verified vs not

- Verified: `just smoke` green; `game --lib perf::` and `game --test lod_streaming` green; client
  type-checks native **and** `wasm32-unknown-unknown`; the debug server run headless prints
  `process_start` and the `[server]` line; sampler + report exercised on a live i915 process and
  on synthetic logs; `trace_summary.py`, `perf_summary.py`, `diag_summary.py --timeline` exercised
  on synthetic inputs.
- **First real capture done** (user-run, `PERF_CAPTURE_SECONDS=60 just adapter=Intel world=tiny
  perf-capture` → `target/perf-capture/20260919T150349Z-Intel-tiny/`): the whole pipeline worked
  end to end, `world_ready` fired at 14.7 s, and the ranking is in
  `docs/superpowers/specs/2026-09-19-client-perf-findings.md`. Headline: the join is 73% **server
  region genesis (10.7 s, release)**, steady state is **CPU-bound outside the game thread** (GPU
  22% of a 19 ms frame), and the client loads fine on the iGPU at `tiny`/release. The run exposed
  four report bugs, all fixed afterwards (lost `world scale` line, collapsed `load_progress` /
  `main_stall` rows, noisy `[server]` tail, a 0-RSS exit sample), and prompted three more probes
  that no capture has exercised yet: `region_genesis build_ms/terrain_hash_ms`,
  `region_first_tick`, and `terrain_hash n/sum/max` on the `[region]` line.
- **Superseded — was:** not verified: anything that needs a client window. No `[perf]` client line has been seen in
  a real log, `world_ready` has never fired for real, and `perf-capture` has not run end to end.
  First real run: `PERF_CAPTURE_SECONDS=90 just adapter=Intel world=tiny perf-capture`, then read
  `summary.txt`. If `world_ready` never appears, the last `load_progress` line says which gate
  is stuck.
- The new client unit tests (`join_timeline::tests`, `main::tests::perf_log_raises_only_the_budget_target`)
  are compile-checked only, like the ones from `c1c497a` (client test binaries need a full Bevy build).

### Next

Step 4: the capture matrix `{Intel, 6700} × {tiny, small, full}` (the 6700 eGPU was not attached
during this session). Then the findings doc and ranking (step 5). The "Open questions" below are
now narrower: `intel_gpu_top` is moot; `sudo perf` stays a separate manual step (`perf-client` +
`perf-summary`); camera driving and the target are still open.

## Objective

Improve client performance first, then server and game. The original symptom: the client did
not load at all on the integrated GPU (`just run`, debug build, production-size world).

The immediate goal is **not** to optimise. It is to get trustworthy, text-form measurements out
of all three sides, captured the same way every time, so a later session can rank costs from
evidence rather than from code reading. Optimisation work follows from that ranking.

**Out of scope for this handoff:** loading pre-existing worlds / region persistence ("step 3"
of the 2026-09-19 investigation). It does not reduce client cost. Findings are preserved in
memory (`small-world-knobs-investigation`) if it is picked up later.

## What landed in `c1c497a`

- **World-size knobs.** `crates/client/src/world_scale.rs` resolves `VOXEL_WORLD=tiny|small|full`
  plus `VOXEL_NEAR_RADIUS=0..3` and `VOXEL_LOD_RINGS=off|small|full|"2:2,4:4"`. The justfile
  `world=` variable exports it to every recipe. `tiny` = 1 simulated region, no LOD rings;
  `small` = 1 region + one factor-2 ring (24 LOD keys); `full` = 3×3 regions + ~570 LOD keys.
  The server only generates what clients subscribe to, so this shrinks both sides.
- **Render benchmark follows the scale.** Readiness gates, expected LOD keys and JSON metadata
  (`required_near_regions`, `lod_ring_table`, `load_timeout_seconds`) come from the active
  scale; a world with no LOD rings counts as far-ready; `RENDER_BENCH_LOAD_TIMEOUT_SECS`
  overrides the 120 s load budget. Output files carry the world in their name.
- **Join robustness.** Client connect retries (5 s/attempt, 120 s deadline) instead of one
  attempt that panicked; `RequestLod` paced at 32 keys/tick nearest-first; pristine
  `terrain_cache` snapshots LRU-capped at 64; `LOD_BUILD_THREADS` env-overridable;
  `just run` / `run-release` build first, start the server first, stop it with the client.
- **Builds and tests capped.** The justfile `cap` variable pins every build/test/check/clippy
  to the first 80% of cores with `nice -n 10 taskset`. `cargo run` and launched binaries are
  never pinned, so measurements are unaffected.
- **`just smoke`** (hard-limited to 60 s, ~12 s warm) and **`just test-one <crate> "<args>"`**
  replace whole-suite runs as the routine check.

### Not verified — carry these forward

- **The client has not been launched since these changes.** Whether `world=tiny` actually loads
  on the iGPU is unknown. First action of the next session: get the result of
  `just adapter=Intel world=tiny run` from the user (the `[client] world scale:` line, the
  `AdapterInfo` line, and whether it reaches a playable state).
- **The new client unit tests were compile-checked, never run** (world-scale parsing, LOD
  request pacing, connect retry, benchmark gates for `tiny`). Running them needs a full Bevy
  test build (many minutes cold): `just test-one client "--lib world_scale:: netcode::tests
  render_bench::"`. Ask the user before starting it.
- **Which timeout the user originally hit is still unconfirmed.** They ran `just run`, so the
  render benchmark's 120 s gate is ruled out. Remaining candidates, from code reading only:
  the unretried connect (now fixed), the unbudgeted join burst on the main thread, the server
  dropping packets it cannot send within 2 s (`send to client timed out; skipping` in the
  server log), and debug builds being over the client keep-up budget.

## Working rules for this repo (read before running anything)

- **No worktrees.** Work on `develop` in the main checkout (`CLAUDE.md`).
- **Do not saturate the machine.** Always build and test through `just` (pinned + niced). A raw
  `cargo` call needs `nice -n 10 taskset -c 0-15` by hand. A job count alone does not cap CPU.
- **Verification budget is about a minute.** `just smoke` is the default. Do not run `just test`,
  `just test-full`, or any client test build without asking first.
- **Measure in release only.** The dev profile is opt-level 1 and ~2.5× slow on hot loops; release
  headroom on the client keep-up budget is already only 0.61× (`keepup_budget.rs`). A debug
  number measures the build, not the game.
- **Always record which GPU ran.** Four Vulkan adapters exist (Intel iGPU, NVK T600, RX 6700 XT,
  llvmpipe). Unpinned picks the T600 (measured 4.2× slower than the 6700). Pin with
  `adapter=<substring>` and confirm against the `AdapterInfo` log line.
- **Compare only within one world scale.** `tiny`, `small` and `full` are different workloads.
- **The user launches GUI runs.** A background session should not open a game window on the
  user's desktop. Give the exact command; the user runs it and says where the output went.
  Output lands under `target/`, which the session can read directly — no pasting needed.
- **One client** for live tests unless the check is inherently multi-client.

## Data that exists today

All of these emit text or JSON a session can read. "Who" = who has to run it.

| Command | Output | Answers | Who |
|---|---|---|---|
| `just diag` | `target/diag/client.log`, `server.log`; reduced by `scripts/diag_summary.py` | CPU-bound vs GPU-bound: frame budget vs summed per-pass GPU time, sorted pass table, `fps`, `entity_count` | user (GUI, ~30 s) |
| `just gpu-log` | `target/diag/gpu.json` (`amdgpu_top`) | GPU busy % from outside the process — **AMD only, useless for the iGPU** | user |
| `just trace-chrome` | `target/trace/client.json` → `scripts/trace_summary.py --skip-ms 5000 --top 40` | Which ECS system / extract step / render node eats the frame | user (GUI; must close window, not Ctrl-C) |
| `just perf-client` / `perf-server` | `perf-folded-client.txt` / `perf-folded-server.txt` | Whole-process native CPU stacks (`profiling` profile; needs `sudo`) | user |
| `just render-bench backend=… adapter=… repetitions=…` | `target/render-bench/<backend>-<adapter>[-<world>]-run-N.json` + `server.log` | Frame-time/FPS summaries, per-pass diagnostics, draw/geometry/memory counters, readiness at exit, provenance | user (GUI, ~3 min/rep at `full`) |
| client log, every `METRICS_REPORT_INTERVAL` | `[client] mispredicts/s=… avg_replay=… max_replay=… evlog=… tick_rate=… drift=…ms` (INFO) | Reconciliation load, prediction-log depth, clock drift | in any client log |
| client log, same cadence | `[client-budget] window=… util=…% \| tick … \| recon … \| snap … \| idle … other … \| sim …` (**DEBUG — not visible by default**) | Where the game thread's wall time goes | needs a log filter, see gap 3 |
| client log | `[client] KEEP-UP WARNING: prediction log depth …` | Death-spiral tripwire | in any client log |
| server log, every 5 s per region | `[region (x,z)] ticks=… avg=…ms max=…ms budget=…% overruns=…` (INFO) | Region sim cost vs tick period | in any server log |
| `just keepup-budget` | test stdout | Region-tick cost vs the keep-up inequality (release test build) | session, **ask first** (slow build) |
| `just bench-region`, `collider-bench`, `worldgen-bench`, `bench-column` | stdout | Worldgen / collider genesis / serde / meshing cost per region | session, **ask first** |
| `just play-test` | `target/play-test/{server,client,build}.log` + panic/error summary | Logs of an interactive session (debug build) | user |

The paused render-benchmark profiling work (Tasks 4–7: aggregation across repetitions, human
report, baseline comparison, Chrome/Tracy capture workflows) is described in
`docs/superpowers/plans/2026-07-16-render-bench-profiling-handoff.md`. Do not redo it here;
finish it there if benchmark reporting is what the ranking needs.

## Gaps — what cannot be analysed yet

Ordered by how much they block the first useful analysis.

1. **No single capture.** Every tool writes to a different place with different lifetimes, and
   nothing records *what was run*: git revision, dirty flag, build profile, adapter actually
   selected, world scale, backend pair, thread counts. A session handed five loose files cannot
   tell whether they describe the same run. **Build one recipe** (working name `just
   perf-capture`) that runs a release server + client and writes everything into
   `target/perf-capture/<timestamp>-<adapter>-<world>/`: `manifest.json` (provenance above,
   taken from the same sources `render-bench` already uses), `client.log`, `server.log`, the
   `diag_summary.py` table, and RSS samples (gap 6). This is the deliverable that makes the rest
   analysable; everything below feeds it.
2. **The load/join phase is invisible — and it is the phase that fails on the iGPU.**
   `trace_summary.py` is invoked with `--skip-ms 5000` precisely to hide startup, `diag` reports
   steady-state averages, and nothing timestamps join milestones. Add a structured timeline,
   one line per milestone with a monotonic `t_ms` since process start, on both sides:
   client — endpoint up, connected, `PlayerRegion` received, each region snapshot received
   (bytes, decode ms), each region spawned in the bridge (chunk entity count, ms), first near
   mesh uploaded, near remesh queue drained, LOD keys requested / received / meshed, connection
   state `Ready`; server — client connected, each region genesis (generate ms, collider ms),
   each snapshot sent (bytes, send ms), LOD job queued → built (ms), send timeouts. A fixed
   prefix (e.g. `[perf] event=… t_ms=… …`) or JSON lines keeps it greppable. Also add a
   `trace-chrome` variant (or flag) that summarises **only** the first N seconds.
3. **The game-thread budget line is hidden.** `[client-budget]` is logged at DEBUG and the
   client's `LogPlugin` filter is `wgpu=error,naga=warn` at the default INFO level, so it never
   appears. Confirm how to enable it (Bevy's `LogPlugin` honours `RUST_LOG`; verify), then have
   the capture recipe turn it on. Without it the split between sim tick, reconcile, snapshot
   load and idle is unknown.
4. **Main-thread join-burst costs are unmeasured.** Code reading (not measurement) flagged these
   as unbudgeted per-frame work that scales with world size: `bridge.rs` `drain_client_updates`
   (no cap) → `spawn_region_snapshot` (one entity per chunk, 1024/region, with `derive_voxels`
   per chunk on the main thread); `meshing/queue.rs` `mark_remesh_dirty` (26 crc32 boundary
   fingerprints per changed chunk, all in one frame); `lod_rings.rs` `apply_lod_events` /
   `spawn_lod_root` (decodes each LOD cell to a dense 32 768-voxel `Vec`, no budget). The near
   mesh path *is* budgeted (`MAX_IN_FLIGHT_MESH_TASKS = 64`, `MESH_UPLOADS_PER_FRAME = 32`).
   **Treat these as hypotheses.** The join-phase trace summary from gap 2 confirms or kills them.
5. **Server-side visibility is thin.** Beyond the per-region tick line there is no: LOD build
   queue depth or job latency, count of `send to client timed out; skipping` drops (a silent
   permanently-incomplete join), per-join `generate_region` count and time, writer-task stall
   time (one writer task serves all clients). Add counters to a periodic `[server]` line in the
   same style as the region line.
6. **No memory data.** Nothing samples RSS for either process. `terrain_cache` was unbounded
   until `c1c497a`; `ClientUpdateEvent::NewRegion` carries a full cloned `GameData`, and
   `GameInstanceManager::new_region` clones the `Rollback` twice. Sample `/proc/<pid>/status`
   `VmRSS`/`VmHWM` once a second from the capture script into `rss.csv`.
7. **iGPU has no outside-the-process GPU signal.** `gpu-log` is `amdgpu_top`. For the Intel iGPU
   use `intel_gpu_top -J` (needs `intel-gpu-tools`, usually root) or accept `diag`'s in-process
   timestamps as the only GPU evidence. Decide with the user.
8. **Raw artefacts are too big to read.** Chrome traces and folded perf stacks run to tens or
   hundreds of MB. A session must never open them whole. `trace_summary.py` already reduces
   traces; add an equivalent top-N reducer for `perf-folded-*.txt` (inclusive/exclusive by
   frame, filterable by thread and by crate prefix `game::`, `client::`, `rapier3d::`, `bevy_`).
9. **`trace-chrome` builds outside the core cap.** It uses `cargo run --release --features
   trace_chrome`, which compiles unpinned. Split it into `{{cap}} {{cargo}} build …` plus a
   direct launch of the binary, like `run` and `diag`.

## Hypotheses to test first

Each names the measurement that settles it. None is confirmed.

| Hypothesis | Settled by |
|---|---|
| On the iGPU the client is GPU-bound in steady state at `tiny` | `just adapter=Intel world=tiny diag` verdict; per-pass table (depth prepass, occlusion culling/HZB and SMAA are all on unconditionally on native) |
| The load failure is a main-thread stall during join, not the GPU | Join-phase trace summary (gap 2): `drain_client_updates`, `mark_remesh_dirty`, `apply_lod_events` dominating early frames |
| First-frame pipeline/shader compilation dominates time-to-first-frame on the iGPU | Timeline gap between window creation and first presented frame; compare adapters at the same world scale |
| The server silently drops join packets to a slow client | Count of `send to client timed out; skipping` in `server.log`; fewer than the expected `bridge: region … loaded` lines in `client.log` |
| The client sim cannot keep up even in release at `full` | `[client-budget] util` and `[client] … evlog= drift=` trend across windows; `KEEP-UP WARNING` |
| Physics dominates client sim time (measured 2026-07-08: ~99.8%, first-tick-after-load spike ~318 ms/region) | Re-measure with `[client-budget]` + `perf-client` reducer after the mass-props and genesis fixes |
| Cost scales with world size roughly linearly, so `full` is just 9× `tiny` plus LOD | Same capture at `tiny`, `small`, `full` on the same adapter; compare timelines and steady-state tables |

## Suggested order

1. Get the user's result for `just adapter=Intel world=tiny run` and, if it loads,
   `just adapter=Intel world=tiny run-release`. This decides whether the next step is "measure a
   working client" or "diagnose a load failure" — the instrumentation is the same, the focus differs.
2. Build `just perf-capture` with the manifest (gap 1), the log filter for `[client-budget]`
   (gap 3) and RSS sampling (gap 6). Small, no sim changes, verifiable with `just smoke`.
3. Add the join timeline on client and server (gap 2) and the server counters (gap 5).
   `game` and `server` must stay Bevy-free; timeline lines in `game`/`server` go through `log`.
4. Have the user run the capture matrix: `{Intel, 6700} × {tiny, small, full}`, release, one
   client. Read the capture directories; write the ranking into a findings doc under
   `docs/superpowers/specs/`.
5. Only then pick optimisation targets, largest measured cost first, one at a time, each with a
   before/after capture at the same adapter and world scale.

## Open questions for the user

- Is `intel_gpu_top` (root) acceptable for iGPU busy %, or is in-process `diag` timing enough?
- Is `sudo` for `perf` acceptable in the capture flow, or should CPU stacks stay a separate,
  manual step?
- Should the capture recipe drive the camera (the benchmark's `yaw_360` path) for
  repeatability, or capture free play? Driving it means sharing the render benchmark's
  controller rather than duplicating it.
- What is the target: "loads and is playable on the iGPU at `tiny`", a frame-rate number at
  `full` on the 6700, or both? The ranking is only meaningful against a target.
