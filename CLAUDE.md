# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**Do not create git worktrees.** Work directly on `develop` in this checkout — including
for test runs, bisection/attribution checks, and subagent dispatches. Shared build caches
and the user's concurrent edits make extra checkouts more trouble than they're worth here.

## Project Overview

A voxel MMO in Rust with client/server architecture over QUIC, built around deterministic simulation and rollback netcode. The client is a Bevy app (rendering, windowing, input); the simulation crates (`game`, `server`) stay engine-agnostic — no Bevy, no windowing library.

## Building and Running

Common tasks are wrapped in the root `justfile` — prefer it over retyping raw
`cargo`/`cargo-clif` invocations. `just` (or `just --list`) shows every recipe;
the important ones are `just build`, `just run` (server + client together),
`just server`, `just client`, `just smoke`, `just test-one`, `just web`, and
`just clippy`. Machine-specific bits are overridable variables: `cargo` selects the
toolchain (defaults to stable `cargo`; set `cargo=$CARGO_CLIF` or export `CARGO_CLIF`
to opt into the Cranelift `cargo-clif` wrapper for faster builds), and `adapter=6700`
pins the client to a GPU (e.g. `just adapter=6700 run`; defaults to `$VOXEL_ADAPTER`, so exporting that once pins every recipe — unpinned, this box lands on the NVK T600, which is ~30× slower on the voxel opaque pass than even the Intel iGPU).
**Keep the justfile in sync**: when you add, rename, or change how a build/run/test
command is invoked, update the corresponding recipe (and add a new one for any
workflow worth a one-word command) in the same change.

The raw commands the recipes wrap, for reference:

`cargo build --workspace --bins` works on stable, which is the default. Cranelift
is opt-in for faster dev builds — point the recipes at a locally built `cargo-clif`
via `cargo=…` or `CARGO_CLIF`:

```sh
# Build everything on stable (see scripts/build.sh)
cargo build --workspace --bins

# Or with Cranelift:
~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins

# Run server + client together (see scripts/run.sh)
cargo run --bin server
cargo run --bin client
```

The server listens on `127.0.0.1:6466` (hardcoded in `crates/server/src/lib.rs`); the client connects to localhost and retries the connect for up to two minutes (`CONNECT_DEADLINE` in `crates/client/src/netcode.rs`), so start order no longer matters. `just run` / `just run-release` build both binaries first, start the server in the background, and stop it when the client window closes.

**World size** is a client-side dial (`crates/client/src/world_scale.rs`); the server only generates what clients subscribe to, so it shrinks both sides. `just world=tiny run` (1 simulated region, no LOD rings), `world=small` (1 region + one factor-2 ring), default `full` (5×5 regions + the LOD rings out to `LOD_HORIZON_UNITS` = 1 km — 16 regions at the default size, ~1,060 LOD keys; the fog goes opaque there. The ring table is not widened for the bigger window: the near window is subtracted from it, so the factor-2 ring (bound 2) is entirely simulated terrain and factor 4 starts right outside it). The variable exports `VOXEL_WORLD`; `VOXEL_NEAR_RADIUS=0..3` and `VOXEL_LOD_RINGS=off|small|full|"2:2,4:4"` override its halves. The chunk edge (32³) is baked into packed records, shaders and the LOD math — shrink the window, not that. `just render-bench` honours it too (its readiness gates and JSON metadata follow the active scale; `RENDER_BENCH_LOAD_TIMEOUT_SECS` stretches the 120 s load budget for slow adapters). Use a small world for iteration on weak GPUs, but compare benchmark numbers only within one scale.

**Region size** is a BUILD-time dial, for experiments: `just region_chunks=64 run` compiles everything with `game::REGION_CHUNKS = 64` (2048-unit / 128 m regions instead of 1024 / 64 m; `crates/game/src/protocol.rs` parses `VOXEL_REGION_CHUNKS` with `option_env!`). Legal values are `REGION_CHUNKS_CHOICES` = 32, 64 and 128 — each has its own `LOD_RING_TABLE` (re-cut to the same pixel budget and horizon) and checked fixtures; the tiling contract in `lod.rs` const-asserts a power of two and anything not in the list fails the build. 128 (4096 units / 256 m) is the practical ceiling: ~50k chunks and a 578 MiB raw snapshot per region, ~7 GB of server state for one 3×3 window. The default lives in `.cargo/config.toml` `[env]` so raw `cargo`, rust-analyzer and `just` share one build fingerprint; switching recompiles `game` and its dependents (not Bevy or the forks). Server and client must be built with the same value — there is no connect handshake to refuse a mismatch. Every per-region cost scales with AREA (measured: region start 0.37 → 1.3 → 3.9 s and snapshot 40 → 161 → 578 MiB at 32 → 64 → 128), so compare numbers only within one region size; findings and the distance-based-window recommendation are in `docs/superpowers/specs/2026-09-20-region-size-investigation.md`. **In cell mode** (`cells=`, see the cell-loading notes under `crates/game`) the dial goes on up to 32768 (65.5 km; powers of two): cells generate lazily per cell (`worldgen::generate_cell`, bit-identical to the region build), so region size no longer bounds start-up or memory — e.g. `just region_chunks=1024 cells=16 run` is a 2 km region. Above 128 cell mode is required (build-time assert) and LOD is a placeholder `[(1, 32)]` ring (to be revisited). Never write the region edge as a literal — derive from `REGION_CHUNKS` / `REGION_SIZE`.

**Deployment (OpenTofu, not Terraform)** lives in `deploy/`: shared `modules/{platform,world}` and per-environment roots (`envs/local/{platform,world}`; `platform` must be applied before `world` — the world's Agones/CloudNativePG custom resources need the CRDs at plan time). The local environment is a single-node k0s in docker, like the planned production box: `just deploy-local` (creates the `mmo-k0s` node + applies both roots), `just deploy-local-plan`, `just kube <kubectl args>`, `just dev-cluster-down` / `dev-cluster-destroy` (the latter deletes the DB). It is for exercising the deployment, never the inner loop — `just run` stays native. The Fleet only appears with `-var game_image=…`; the server cannot run in a pod yet (binds 127.0.0.1, no image). Setup and the settings bare k0s needs: `docs/superpowers/specs/2026-09-22-local-deployment-environment-design.md`.

**Builds and tests are capped at 80% of cores** so they never take the whole desktop. A job count alone does not do it — each rustc runs multi-threaded codegen, and 16 jobs still saturated all 20 cores — so the justfile's `cap` variable prefixes every build/test/check/clippy invocation with `nice -n 10 taskset -c 0-<jobs-1>`, pinning the whole process tree (test binaries and their thread pools included) to the first `jobs` cores. `cargo run` and directly-launched binaries are never pinned, so the game and its perf numbers are unaffected. Tune with `just cpu_percent=50 build` or `just jobs=4 smoke`. **Raw `cargo` outside `just` is not pinned** (`.cargo/config.toml` only sets `jobs = -4`, and `cargo test` threads are uncapped) — go through `just`, or prefix `nice -n 10 taskset -c 0-15` yourself. Server-side, `LOD_BUILD_THREADS` sizes the LOD worker pool (default: a quarter of the logical cores, clamped 2..=6 — `crates/server/src/lod_workers.rs`; the server logs the resolved count at startup and `perf-capture` summaries show it).

The game crate has the rollback test suite: `cargo test -p game` (transaction/undo invariants in `tests/log_model.rs` and `tests/simple.rs`, seeded randomized rollback in `tests/random_ops.rs`, vendored-container inverse-op guarantees in `tests/hash_restore.rs`). Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact. The client has its own headless test suite: `cargo test -p client` (sim-bridge, coordinate conversion, interpolation, async chunk meshing, and full client/server convergence harnesses, run via `MinimalPlugins`/`AssetPlugin` with no window or GPU).

**Do not run the whole game / server / client suites as a routine check — they take far too long** (even the "quick" `just test` links ~30 game test binaries and ~9 Bevy-sized client ones). Verify a change with:

- `just test-one <crate> "<cargo test args>"` for exactly the tests the change touches, always naming the target so nothing else gets built — e.g. `just test-one game "--test lod_streaming pristine"`, `just test-one client "--lib world_scale::"`.
- `just test-threads` (~12 s warm) after touching boundary handoff, the `WorldManager` or `region_threads` — the region is the threading unit (one OS thread per running region; nothing inside a region is parallel), and these are the only tests where a player walks between regions on different threads. Two layers, both tagged slow so nothing else runs them: `crates/game/tests/handoff_schedule.rs` walks across a seam and through a region corner, there and back, under **seeded scheduling jitter** (`game::InlineSchedule` on the `InlineSpawner`: permuted tick order, inputs a tick late, missed ticks — a failure names a seed that replays exactly), and `crates/server/tests/threaded_world.rs` does the same walks on real OS threads, including waking a cold region on the production `with_generator` spawn path. Put scheduling-sensitive correctness in the seeded layer; keep real threads for plumbing. The client twin (`crossing_converges_under_late_region_scheduling` in `crates/client/tests/crossing.rs`) is a slow client opt-in.
- `just smoke` as the general sanity check — **this is the default verification, and it is hard-limited to 60 s** (`smoke_timeout`; ~12 s warm): two game test binaries, the server lib tests, a *type-check* of the server's integration tests (all tagged slow, so nothing else notices them rotting), and a *type-check* of the client (lib + bin + tests). It never builds a client test binary, because that means a full Bevy codegen + link (many minutes cold). Running client unit tests is an explicit, slow opt-in (`just test-one client "--lib <filter>"`) — ask before doing it.

`just test` (whole quick suites) and `just test-full` (`--include-ignored`, minus the release-mode perf gates) still exist for a deliberate broad sweep; run them only when the user asks or a change is wide enough to need them, and say so first. Tests measured slower than 100ms are tagged `#[ignore = "slow: >100ms — run via just test-full"]`, which keeps them out of `smoke`, `test-one` and `just test`; tag any new test that takes >100ms the same way.

Bevy is pinned to `0.19` in the root `Cargo.toml`; Bevy's API surface moves fast between minor versions, so treat upgrades as a deliberate, tested migration rather than a routine bump — API drift (renamed types/traits, moved modules, changed system-set names) is a known hazard. The 0.18 → 0.19 migration (2026-09-22) is the worked example: the render graph is gone — custom passes are systems in the per-camera `Core3d` schedule ordered against Bevy's pass systems (`voxel_pull::gpu::add_near_gpu_systems`, `far_voxel::pass::far_voxel_pass`); `MeshPipelineViewLayouts` is built in `RenderStartup`, so pipelines that clone it are created there too; the view bind-group layout is keyed on many view features (fog, tonemapping, …), so custom mesh-view pipelines take their key from Bevy's `ViewKeyCache`, and the WGSL gates fog behind `DISTANCE_FOG`. The shader-validation tests compose against hand-written import STUBS, so an upstream WGSL signature change (0.19 added `frag_coord_xy` to `fetch_directional_shadow`) only shows at pipeline creation — after a Bevy bump, run `just pull-smoke` (self-screenshot of the production pull path) on a real adapter. `renderer::render_plugin()` disables wgpu's `EXPERIMENTAL_COOPERATIVE_MATRIX`, which NVK advertises but then fails `vkCreateDevice` with.

### WASM build (browser multiplayer)

The client also targets `wasm32-unknown-unknown`; in the browser the native
QUIC netcode is replaced by WebTransport (`crates/client/src/netcode_web.rs`):

```sh
cargo run --bin server   # quinn :6466 + WebTransport ingress :6467; writes
                         # assets/webtransport-cert-hash.json for the page
# From the repo root (so ./assets is served). The custom index adds a
# fullscreen canvas + download progress bar:
WASM_SERVER_RUNNER_CUSTOM_INDEX_HTML=crates/client/index.html \
  cargo run -p client --target wasm32-unknown-unknown   # opens via wasm-server-runner
```

Open `http://127.0.0.1:1334/` — connecting to the real server is the default,
and like the native client it retries for two minutes (`CONNECT_DEADLINE_MS` in
`netcode_web.rs`), re-fetching the cert hash each attempt, so the page may be
opened before the server is up.
Append `?offline` to opt into the embedded single-player world instead
(`crates/client/src/local_server.rs`, no server needed). Requires
`rustup target add wasm32-unknown-unknown` and `cargo install
wasm-server-runner`. See
`docs/superpowers/specs/2026-07-05-webtransport-netcode-design.md`.

### Profiling

Measure in **release** (`just diag`, `just client-release`, the `profiling` profile). The dev
profile is opt-level 1 and runs the hot loops ~2.5x slow, so an FPS number from `just client`
measures the build, not the game. And **always check which GPU you got**: this box exposes four
Vulkan devices (Intel iGPU, an NVIDIA T600 on the NVK driver, the RX 6700 XT, and llvmpipe
*software* rendering), and with no `WGPU_ADAPTER_NAME` wgpu chooses for you — pin it with
`just adapter=6700 …` and confirm against the `AdapterInfo` line in the log.

**Check the power source too.** On 2026-09-19 this laptop was fed by an ~88 W USB-C PD source
(`/sys/class/power_supply/ucsi-source-psy-*`) and the firmware clamped every core to ~400-700 MHz
under multi-threaded load (one busy P-core: 2-3 GHz of 4.6). Bursts run fast, then the clamp
bites, so anything multi-threaded (joins, region genesis, builds) measured several times slower
and noisier than the hardware allows — compare ratios, not seconds, until re-baselined on the
real adapter. Watch `scaling_cur_freq` under load to tell.

Text-emitting (readable in a terminal, pasteable into a bug report or an LLM session):

- `just perf-capture` — **the capture to hand to an analysis session.** Release server + one
  client, everything in one self-describing directory
  `target/perf-capture/<utc>-<adapter>-<world>/`: `summary.txt` (read first), `manifest.json`
  (git rev + dirty, profile, adapter asked for *and* selected, world scale, env knobs, host),
  both full logs, `rss.csv` (1 Hz RSS/threads/CPU), `gpu.csv` (1 Hz, no root: the client's own
  GPU engine time + GPU memory from DRM fdinfo — works on i915 and amdgpu, not NVK — plus sysfs
  whole-GPU counters) and `diag.txt` (the `diag` verdict + a per-dump timeline). It sets
  `DIAG=1` and `PERF_LOG=1`. `PERF_CAPTURE_SECONDS=90` closes the client by itself;
  `scripts/perf_capture_report.py <dir>` re-summarises an old capture.
- **The `[perf]` join timeline** (`crates/game/src/perf.rs`) is always on, in every log:
  `[perf] event=<name> t_ms=<ms since process start> key=value …` on both sides (client:
  `endpoint_up`, `connected`, `player_region`, `snapshot_received`, `region_loaded`,
  `bridge_region_spawned`, `first_update`, `first_near_mesh`, `near_meshing_drained`,
  `lod_requests_done`, `lod_all_received`, `connection_ready`, `load_progress` every 2 s until
  `world_ready`, and `main_stall` for any >8 ms call of the unbudgeted bridge systems; server:
  `client_connected`, `region_generate`, `region_genesis`, `snapshot_sent`). Volume is part of
  the contract — per process / region / batch, never per chunk, LOD key or frame; high-rate work
  goes into the periodic `[server]` counters line (`crates/server/src/perf.rs`: manager-thread
  worldgen, LOD latency vs build time, writer-task send time + `timeouts`, channel backlogs).
  `PERF_LOG=1` additionally raises the DEBUG `[client-budget]` / `[client] mispredicts/s` lines
  (their own `perf_budget` log target; `RUST_LOG` would replace Bevy's whole filter instead).
- `just fly-probe [seconds]` — **region-crossing repro, no human needed.** Release server + a
  client that, once `world_ready`, flies a straight line at fly-sprint (`VOXEL_AUTOPILOT=1`,
  `renderer/autopilot.rs`; `VOXEL_AUTOPILOT_SECONDS` closes it), crossing a region seam every ~3 s.
  Hands off the desktop: it sends sim inputs directly and enables body control with
  `InputEvent::FpsCam(true)` (sets fps-cam mode without the `SetFreeCam` that grabs the cursor),
  and mouse motion only reaches the sim while the window is focused AND holds the pointer. Per
  crossing it prints `[perf] event=region_crossing` (game thread: longest gap between home ticks,
  home ticks sat out by `stalled`, align steps, corrections, snapshot loads, `PlayerRegion`
  confirm latency) and `region_crossing_render` (worst frame, longest sim-pose hold; its
  `jump_max`/`back` straddle region-local frames at the flip — ignore those two). Pair
  `VOXEL_AUTOPILOT=1 VOXEL_AUTOPILOT_SECONDS=40` with the `trace_chrome` build for a per-system
  table of sustained flight (`scripts/trace_summary.py … --skip-ms 40000`).
  `just shuttle-probe [region|chunk] [seconds] [overshoot_m]` is the same benchmark for ONE
  boundary crossed back and forth (`VOXEL_AUTOPILOT=shuttle`): it flies to the first region
  seam (or 32-unit chunk edge) and reverses `overshoot` m past it each leg — every region leg is
  a handoff plus a near-window shift. The near window recentres only `WINDOW_RECENTER_MARGIN_M`
  (16 m) into a neighbouring region (`RegionCoords::recentered`); `VOXEL_WINDOW_MARGIN_M=0`
  restores recentring at the seam for A/B runs. Both probes end with `scripts/crossing_summary.py`
  (p50/p95/max per metric; flags a run that lost its GPU). Two runs have ended in a wgpu
  `DeviceLost` (once on the RX 6700, once on the Intel iGPU with nothing in dmesg) — cause
  unknown; both were runs that reloaded a row of regions on every crossing. Separately, the
  eGPU's Thunderbolt link logs ~1.2 s `pciehp: Timeout on hotplug command` stalls in dmesg,
  which line up with occasional ~2 s frames on the 6700.
- `just diag` — release client with `DIAG=1`, which adds Bevy's `RenderDiagnosticsPlugin`
  (`crates/client/src/renderer/diag.rs`, gated on the env var because the wgpu timestamp and
  pipeline-statistics queries cost a little). Every 2s (`DIAG_INTERVAL`) it dumps `fps`,
  `frame_time`, `entity_count` and, per render pass, `elapsed_gpu` / `elapsed_cpu` /
  `fragment_shader_invocations`; `scripts/diag_summary.py` then reduces the log to the
  **CPU-vs-GPU-bound verdict** — frame budget vs summed per-pass GPU time, plus the sorted pass
  table. GPU columns need Vulkan or DX12 — wasm records CPU time only. Note the client presents
  with vsync (Bevy's default `PresentMode::Fifo`), so a frame pinned at ~16.7ms is idle-waiting on
  present, not saturated; switch the `WindowPlugin` to `PresentMode::AutoNoVsync` to see real headroom.
- `just gpu-log` — `amdgpu_top` JSON sampled alongside a running client: GPU busy % from outside
  the process, as an independent check on the above. No rebuild needed.
- `just trace-chrome` — the follow-up when `diag` says CPU-bound. Builds with the `trace_chrome`
  feature (a tracing span per ECS system / extract step / render-graph node), writes a Chrome
  Trace Event JSON, and reduces it to a per-system table via `scripts/trace_summary.py`. The JSON
  only flushes on a clean exit, so close the window rather than Ctrl-C.
- `just trace-join` — the same capture reduced to ONLY its first `join_ms` (default 30 s): the
  load/join phase `trace-chrome` deliberately skips. Read the `max ms` column — one multi-second
  call is a main-thread stall. (`scripts/trace_summary.py <json> --until-ms N` on an existing trace.)
  `trace_summary.py` STREAMS the file (traces reach several GB — the rapier fork traces its solver
  internals, ~25 MB/s; never `json.load` one), sorts by **self time** (children excluded; schedule
  spans otherwise double count), prints a per-thread busy table, and takes `--thread <name>` and
  `--skip-cat rapier3d` (the recipes pass it). The tracing features include `bevy/debug`, without
  which release builds name every system `<Enable the debug feature to see the name>`.
- `just perf-client` / `just perf-server` — `perf` + inferno folded stacks (`perf-folded-*.txt`),
  the whole-process native CPU profile. The folded files are tens of MB — never open them whole;
  both recipes end with `scripts/perf_summary.py` (thread / crate / self / inclusive top-N), and
  `just perf-summary client --thread game --prefix rapier3d::` re-cuts it.

GUI-only (a human has to look at them):

- `just profile-up` starts Pyroscope (`:4040`) + Grafana (`:3000`); `just profile-client` / `just profile-server` then run the pair with that side built with the opt-in `pyroscope` feature (plain builds never compile the agent).
- `just tracy-client` — Bevy's Tracy frame profiler; needs a protocol-matching Tracy GUI attached while it runs.
- `scripts/perf.sh` records with `perf` and opens `hotspot`.

## Workspace Layout

First-party crates (workspace members):

- `crates/macros` — Proc macro crate providing `#[rollback]` on the state module. Fields marked `#[undo(cell|map|slotmap)]` get tier-1 wrappers (`UndoCell`/`UndoMap`/`UndoSlotMap`) whose mutating methods log typed, exactly-invertible deltas automatically; `#[emit(insert = ..., remove = ...)]` derives renderer `GameDataUpdate`s from those deltas in both apply and undo directions. Unmarked fields stay `Undo<T>` (tier 2): mutate via `undo_scope()`/`undo()` closures that MUST be true inverses of the full serialized state (`hash(before) == hash(after undo)` is enforced at rollback time), or `change()` for snapshot-restore; `emit_on_undo()` registers compensating render events. Verification has three per-entry modes (`game::set_hash_mode`): `Full` (whole-field crc32 — the process-global default, used by tests/harnesses), `Delta` (digest of only the entry's mutation footprint), `Off`. The client and server binaries call `game::set_hash_mode_from_env()` at startup: `Off` unless the `HASH_MODE` env var (`off`|`delta`|`full`) overrides it (desync is caught by the periodic tick-interval state hash instead) — run with `HASH_MODE=delta` for per-entry footprint digests, or `HASH_MODE=full` when chasing a suspected footprint/capture gap. The vendored `slotmapd`/`rapier` forks expose exact LIFO inverses (`revert_insert`/`revert_remove`) because plain `remove(insert(x))` does NOT restore allocator state (free lists, versions/generations) — see `crates/game/tests/hash_restore.rs`. The macro generates all undo infrastructure (`Rollback`, `Undo<T>`, the wrappers, the log) at its invocation site and hardcodes `crate::GameDataUpdate`/`crate::serde` paths, so it must expand in the crate that defines the update enums — that's `game`. The design spec lives in `docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md`.
- `crates/game` — Deterministic simulation shared by client and server, and the rollback-netcode state layer (absorbed from the former `crates/rollback`): `World`, `Region`, controllers (camera, rapier physics), plus `state.rs` (`GameData`, the `#[rollback]` invocation, `GameDataUpdate`), `voxel.rs` (`Chunk` 32³ voxels), `protocol.rs` (packets/events), `input.rs` (engine-neutral input), `movement.rs` (Minecraft-creative body movement: walk/sprint under gravity, jump, step-up, double-tap-Space flight with Space/Shift vertical — entity-generic `Motion` component + pure `plan`/`settle` over `MoveKeys`, tunables in m/s as named constants; `CameraController` in `camera.rs` feeds it player input and runs rapier's `KinematicCharacterController`, whose grounded/snap/autostep need the fork's prediction-loosened voxels-shape manifolds; the fork has no penetration recovery, so `movement::depenetration_lift` puts a body sunk into terrain back on it first), `slime.rs` (ambient hopping wildlife, the first non-player entity: `EntityKind::Slime { size }` + `Slime` component, Minecraft hop/rest AI driven from `Rollback::tick_slimes_safe` at the start of each region tick, stateless hash RNG `roll(tick, subject, salt)` — no RNG state to undo — surface spawner whose rates are named constants (they must match on client and server, since both sim), an edge leash so slimes never leave their region; squash and stretch is render-only in the client's `renderer/avatar.rs`), `edit.rs` (block editing: left click breaks, right click places, raycast in the sim from the player's eye during the tick; `set_block_safe` updates chunk data, render voxels and colliders as one undoable change). `lib.rs` glob re-exports these at the crate root — the macro expansion and `borrow::Partial` derives depend on that. `TICK_RATE` is the tick period in ms (currently being tuned — read it from `crates/game/src/lib.rs`, and note the name says rate but it is a period). Also hosts the world-level multi-region layer: `region_runner.rs` (the per-region actor core — `RegionInput`/`RegionOutput` message pair, `RegionRunner`, `SerializedRegion` parking format; this message pair is the future network seam) and `world_manager.rs` (`WorldManager` — threadless sessions/homes/subscriptions/parking-lot brain, parameterized by a `RegionSpawner`: OS threads on the server, `InlineSpawner` for wasm/tests — lockstep by default, or jittered from a seed by an `InlineSchedule` to stand in for thread timing). Regions tile the plane in signed `RegionCoords` (`RegionId` is no longer `ChunkCoords`), each `REGION_SIZE` world units (1024.0 = 32×32 chunks by default; see the region-size dial above); sims stay region-local, the world offset exists only at the render boundary. Beyond the subscribed near window, terrain streams as server-built LOD (`lod.rs`: `LodKey` — per-region for factors 2/4/8/16/32, per-tile for 64/256 — plus downsamplers and `PackedLodGrid` RLE; `LOD_RING_TABLE` doubles the factor per doubling of distance so a cell stays within `LOD_MAX_CELL_PX` (~2 px) where its ring begins — that pixel budget, not the downsample rule, is what makes far terrain read as real, so don't coarsen a ring without moving it out; occupancy is any-solid (coarse ⊇ fine keeps rings crack-free) but the material is the one visible from above (`merge_group`); pristine terrain at `COARSE_MIN_FACTOR` (16) and coarser is profile-sampled, never fully generated; `lod_builder.rs`: `LodBuilder` jobs the `WorldManager` submits on `RequestLod`, sourcing `TerrainSnapshot`s from running/parked regions or worldgen, invalidated by `TerrainChanged` cadence pushes). Boundary handoff (`scan_boundaries` producing `EntityArrived`/`GhostUpdate` events, ghost mirrors with TTL, and upgrade-in-place of a ghost into an owned entity) runs inside the region tick; `PlayerInput` routes by the manager's `homes` map, not the client's region stamp, so input follows the player across a flip. **Cell loading** (phase 1 of `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`, game-only — production still runs fully materialized regions): `cells.rs` (`CellCoords`, `CellState` in `GameData.cells` — `cell_chunks == 0` is a legacy region; the server-authored `CellChanges` event materializes/dematerializes cells with raw terrain ops at the confirmed head, the server fills a per-cell `cell_hash` and clients hard-fail on a mismatch; strays in dormant cells freeze at end of tick; edits and slime spawns are gated to active cells), `cell_tickets.rs` (pure ticket planner, tunables `SIM_RADIUS_CELLS` etc.), `cell_store.rs` (in-memory dormant cells); `RegionRunner::new_with_cells` drives them. Phase 1b wired it end to end behind the BUILD-time knob `VOXEL_CELL_CHUNKS` (`game::CELL_CHUNKS`, 0 = off; `just region_chunks=128 cells=16 world=tiny run`): both spawners build cell runners via `RegionSeed::into_runner`, cell regions never park (their dormant store is runner-private until persistence), server regions drop applied events (`Region::commit_server_event`), the client forces near radius 0 and renders cells through the ordinary `CreateEntity`/`SetEntityPosition`/`SetVoxelComponent`/`RemoveEntity` updates (voxels derived on the client's sim thread — phase 2 measures it); dormant terrain inside the home region is not rendered yet. Phase 1b limits: meant for `world=tiny` (one region per player); walking into a NEIGHBOURING region is unsupported (the client flips home before it has the new region's snapshot, so the body is missing until it lands); cell regions are never reclaimed (thread + store live for the process); a runner's terrain replies (`RegionOutput::Terrain`/`TerrainChanged`) include its dormant cells so LOD shows no holes. Tests: `just test-one game "--test cell_activation"` and `"--test cell_manager"` (+ `-- --include-ignored` for the convergence runs).
- `crates/server` — Tokio + quinn QUIC server. Hub-and-spoke topology: the netcode edge (tokio/quinn + WebTransport ingress) feeds a manager thread running the shared `WorldManager` core, which routes events to N region threads (`region_threads.rs`, one `RegionRunner` + tick timer per running region) over crossbeam channels — never shared memory, so the thread boundary can later become a network boundary. Regions self-tick; cycled-out regions park as `SerializedRegion` blobs after a grace period and restore bit-exact on resubscribe. Self-signed cert generated at startup via rcgen.
- `crates/client` — A Bevy app. `renderer/` holds the bridge modules that translate sim state into Bevy render state (`bridge.rs`/`convert.rs`/`interpolate.rs`/`meshing/` (binary greedy mesher, AO, packed vertex format, neighbor gather, remesh queue, cave-culling visibility, LOD meshing; `records.rs` packs 8-byte per-quad records for the pulled render path — its layout is locked to `assets/shaders/voxel_pull.wgsl`)/`voxel_pull/` (the default pulled-quad render path: per-chunk storage buffers drawn by custom Opaque3d/prepass/Shadow phase items, no Mesh assets; `TerrainRenderConfig` in `config.rs` picks the near backend — `VOXEL_NEAR_RENDER=per-chunk|direct|gpu` on native; the depth prepass + HZB occlusion culling is OFF by default (it re-drew every quad to cull ~3% on open terrain — `VOXEL_OCCLUSION=on` restores it); cave culling (`meshing/visibility.rs` `cull_chunks`) is also OFF by default — the world has no caves yet and the flood cost ~8 ms/turning frame at the 5×5 window for no GPU saving; F4 or the F3 dev panel turns it on — and `VoxelPullPlugin::finish` drops to the per-chunk Mesh/Material path on a device without vertex-stage storage buffers, i.e. the wasm build's WebGL2)/`lod_rings.rs` (renders the server-streamed LOD rings: decodes `LodData` payloads into per-key roots, retires superseded roots only once a replacement is visibly meshed)/`voxel_material.rs`), plus engine-neutral input handling (`input.rs`); UI: the sim-state screens (hotbar, later inventory/containers/mod UI) are a pure projection in `game::ui` (`build_player_ui` → `diff_ui` → `UiUpdate`s) rendered by `renderer/ui_bridge.rs` as `bevy_ui` nodes, whose clicks `ui_input.rs` turns into `GameEventKind::UiAction` sim events; client-local UI (HUD `hud.rs`, menus, chat) is plain `bevy_ui`; `renderer/cursor.rs` `CursorPolicy` is the only cursor-grab writer (sim request AND no overlay open), and `forward_input` drops clicks/mouse motion while an overlay holds the pointer; `renderer/dev_ui.rs` is an egui F3 developer panel behind the default-on `dev_ui` feature (`--no-default-features` builds without egui) — dev tooling only, never player-facing UI (spec: `docs/superpowers/specs/2026-09-22-ui-state-and-options.md`); networking lives in `netcode.rs`; `main.rs` coordinates the game loop (netcode + rollback) and wires up the Bevy `App`. The instance layer (`instance.rs`) diffs a ring table into `RequestLod`/`ReleaseLod` subscriptions each tick. `game` and `server` must stay Bevy-free and windowing-library-free — they are shared by both client and server and must not pull in rendering/windowing deps.
- `crates/worldgen` — Deterministic world generation: `generate_region(RegionCoords)` is a pure function from region coordinates to the full 32×32 chunk grid, which is what makes "cycle out = park, cycle in = restore-or-regenerate" safe. Terrain shape lives in `pipeline.rs`: Minecraft-weighted octave noises (`noise.rs` — first octave dominant, sd ~0.5; the splines are laid out on that distribution) → continents (`CONTINENT_SCALE`, ~30% land; `FEATURE_SCALE` = 0.5 halves every macro wavelength — tightened for bigger regions, heights unchanged) + erosion + a peaks-and-valleys fold of a domain-warped ridge noise for mountain ranges (`MOUNTAIN_OFFSET`, `RIDGE_WARP_M`); the fused `column()` must mirror the `#[cfg(test)]` `DensityFn` graph bit-for-bit. **Look at the world with `just world-map`** (`examples/world_map.rs`: colour relief + 16-bit heightmap PNGs + land/height/slope stats into `target/world-map/`, one profile per pixel, e.g. `--span 65536 --size 2048`) after any tuning change, and re-run `examples/seed_scan.rs` to re-pick `WORLD_SEED`/spawn. **Forged worlds** (the direction for super regions — one finite, pre-generated world per pod, loading screen between them): `just forge-proto` (`examples/forge_proto/`, offline, no determinism requirement) simulates a whole 16 km ocean-bounded world — geology-controlled stream-power erosion, orographic rain, Priority-Flood drainage, spill-level lakes, rivers as objects, `--search N` to rank seeds — and writes a world file `<stem>.world`; `forged.rs` reads it (`WorldArtifact`: height/lake/geology rasters + river reaches + spawn) and serves it through the SAME `generate_region`/`coarse_tile` entry points when `VOXEL_WORLD_FILE` is set (`just world_file=<path> run`; the `seed` argument is then ignored, the disk cache is keyed by the file's crc, a set-but-unreadable file panics rather than mixing worlds). Generated terrain is **whole blocks and half blocks only**: the surface is rounded to the nearest half block (`HALF_TEST_M`/`FULL_TEST_M` in `lib.rs`), and a half block is NOT a special block type — it is ordinary `ChiselData` holding one cuboid (`ChiselData::cuboid`), so it is voxel-editable like any chisel. A submerged surface block is always whole (a half block's empty upper half would be an air pocket). `examples/quantize_probe.rs` reports region cost + step walkability. Both paths build blocks from a `Patch` (column profile + topmost water block per column): forged patches interpolate the raster bicubically on the usual 4 m lattice, then carve river channels, levees and lake berms per point so water above sea level is contained (water surfaces snap to whole blocks). `just world-map --world <file>` renders a forged world exactly as the generator samples it. **Tuning the forge:** every tunable lives in one runtime table, `examples/forge_proto/knobs.rs` (its defaults ARE the tuning; CLI `--set <knob>=<value>`), and `just forge-ui` serves a local page (`serve.rs` + `ui.html`, std-only HTTP on 127.0.0.1:8787) with each knob as a slider and a live map preview — a change re-forges at 64 m cells in ~1.5 s; layers rivers/relief/geology/rain/before-erosion, press-and-hold for the previous render, "Copy as flags", "Save world file" (forges at 16 m). Coarse previews under-count lakes and peaks — check at 16 m before keeping a value. Unset, nothing changes — procedural tests never see it. Depends on `game` (not vice versa) — generation is injected into the `WorldManager` as a closure. `coarse_tile` point-samples one profile per LOD cell column for every block-scale-or-coarser factor (16/32 per region, 64/256 per tile; ~40× cheaper than generating the region, and exact at factor 16) — the cheap pristine-terrain base the server overlays authoritative regions onto. Water fills through `SEA_TOP_BLOCK`, so the sea surface is `SEA_LEVEL` = 1024 voxels, a multiple of every LOD cell: any-solid downsampling rounds surfaces up a cell, and a sea off that lattice shows as a wall at ring boundaries.

## Vendored Forks (do not treat as dependencies to update)

`crates/nalgebra`, `crates/simba`, `crates/parry`, `crates/rapier`, `crates/approx`, `crates/ordered-float`, `crates/slotmapd`, and `crates/block-mesh` are vendored, locally patched forks wired in via `[workspace.dependencies]` path overrides. They exist to guarantee **cross-machine determinism** for the rollback netcode: `simba` is forced onto `libm` (`libm_force`), `rapier3d` uses `enhanced-determinism`, floats are wrapped in `ordered-float`. The rapier/parry forks also carry the `StepJournal` rollback machinery — a per-tick mutation journal plus exact LIFO inverses — that lets `PhysicsPipeline::step` roll back without whole-state snapshots. The forks also fix three upstream shape-cast defects that made a character controller jitter on voxel ground (casts starting exactly at `target_distance`, the voxels cast's search domain, internal-edge normals between flush voxels/colliders — `QueryPipeline::cast_shape_on_continuous_surfaces`, pinned by `crates/game/tests/shape_cast_grazing.rs` and `walking_smoothness.rs`). Don't switch these back to crates.io versions, and be aware that changes to simulation math can break determinism between client and server.

## Conventions

- Serialization over the wire is `bincode`, framed by `game::encode_packet` / `decode_packet` (`crates/game/src/protocol.rs`): one format byte, and packets above `PACKET_COMPRESS_THRESHOLD` are lz4-compressed (`lz4_flex`; a region snapshot goes 40 MiB → 0.9 MiB). Every transport edge — server writer + QUIC/WebTransport ingress, native client, wasm client — must use that pair; never call `bincode` on the wire directly. State hashing uses `crc32fast`.
- Cross-thread communication uses `crossbeam` channels (game loop ↔ network loop ↔ render loop); async (tokio) is confined to the QUIC networking edges.
- Editions are mixed intentionally: older crates are 2021, newer ones (`macros`, `worldgen`) are 2024.
- `TODO.md` tracks the current high-level direction (multi-region/chunk-grid simulation instances).
- The root `justfile` is the canonical set of build/run/test commands — use it, and edit it whenever the underlying commands change so it never drifts from reality.
