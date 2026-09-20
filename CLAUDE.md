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
pins the client to a GPU (e.g. `just adapter=6700 run`).
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

**World size** is a client-side dial (`crates/client/src/world_scale.rs`); the server only generates what clients subscribe to, so it shrinks both sides. `just world=tiny run` (1 simulated region, no LOD rings), `world=small` (1 region + one factor-2 ring), default `full` (3×3 regions + all five rings, ~570 LOD keys). The variable exports `VOXEL_WORLD`; `VOXEL_NEAR_RADIUS=0..3` and `VOXEL_LOD_RINGS=off|small|full|"2:2,4:4"` override its halves. Chunk (32³) and region (1024) dimensions are baked into packed records, shaders and the LOD math — shrink the window, not those. `just render-bench` honours it too (its readiness gates and JSON metadata follow the active scale; `RENDER_BENCH_LOAD_TIMEOUT_SECS` stretches the 120 s load budget for slow adapters). Use a small world for iteration on weak GPUs, but compare benchmark numbers only within one scale.

**Builds and tests are capped at 80% of cores** so they never take the whole desktop. A job count alone does not do it — each rustc runs multi-threaded codegen, and 16 jobs still saturated all 20 cores — so the justfile's `cap` variable prefixes every build/test/check/clippy invocation with `nice -n 10 taskset -c 0-<jobs-1>`, pinning the whole process tree (test binaries and their thread pools included) to the first `jobs` cores. `cargo run` and directly-launched binaries are never pinned, so the game and its perf numbers are unaffected. Tune with `just cpu_percent=50 build` or `just jobs=4 smoke`. **Raw `cargo` outside `just` is not pinned** (`.cargo/config.toml` only sets `jobs = -4`, and `cargo test` threads are uncapped) — go through `just`, or prefix `nice -n 10 taskset -c 0-15` yourself. Server-side, `LOD_BUILD_THREADS` sizes the LOD worker pool (default: a quarter of the logical cores, clamped 2..=6 — `crates/server/src/lod_workers.rs`; the server logs the resolved count at startup and `perf-capture` summaries show it).

The game crate has the rollback test suite: `cargo test -p game` (transaction/undo invariants in `tests/log_model.rs` and `tests/simple.rs`, seeded randomized rollback in `tests/random_ops.rs`, vendored-container inverse-op guarantees in `tests/hash_restore.rs`). Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact. The client has its own headless test suite: `cargo test -p client` (sim-bridge, coordinate conversion, interpolation, async chunk meshing, and full client/server convergence harnesses, run via `MinimalPlugins`/`AssetPlugin` with no window or GPU).

**Do not run the whole game / server / client suites as a routine check — they take far too long** (even the "quick" `just test` links ~30 game test binaries and ~9 Bevy-sized client ones). Verify a change with:

- `just test-one <crate> "<cargo test args>"` for exactly the tests the change touches, always naming the target so nothing else gets built — e.g. `just test-one game "--test lod_streaming pristine"`, `just test-one client "--lib world_scale::"`.
- `just smoke` as the general sanity check — **this is the default verification, and it is hard-limited to 60 s** (`smoke_timeout`; ~12 s warm): two game test binaries, the server lib tests, and a *type-check* of the client (lib + bin + tests). It never builds a client test binary, because that means a full Bevy codegen + link (many minutes cold). Running client unit tests is an explicit, slow opt-in (`just test-one client "--lib <filter>"`) — ask before doing it.

`just test` (whole quick suites) and `just test-full` (`--include-ignored`, minus the release-mode perf gates) still exist for a deliberate broad sweep; run them only when the user asks or a change is wide enough to need them, and say so first. Tests measured slower than 100ms are tagged `#[ignore = "slow: >100ms — run via just test-full"]`, which keeps them out of `smoke`, `test-one` and `just test`; tag any new test that takes >100ms the same way.

Bevy is pinned to `0.18` in the root `Cargo.toml`; Bevy's API surface moves fast between minor versions, so treat upgrades as a deliberate, tested migration rather than a routine bump — API drift (renamed types/traits, moved modules, changed system-set names) is a known hazard.

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

Open `http://127.0.0.1:1334/` — connecting to the real server is the default.
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
- `crates/game` — Deterministic simulation shared by client and server, and the rollback-netcode state layer (absorbed from the former `crates/rollback`): `World`, `Region`, controllers (camera, rapier physics), plus `state.rs` (`GameData`, the `#[rollback]` invocation, `GameDataUpdate`), `voxel.rs` (`Chunk` 32³ voxels), `protocol.rs` (packets/events), `input.rs` (engine-neutral input). `lib.rs` glob re-exports these at the crate root — the macro expansion and `borrow::Partial` derives depend on that. `TICK_RATE` is the tick period in ms (currently being tuned — read it from `crates/game/src/lib.rs`, and note the name says rate but it is a period). Also hosts the world-level multi-region layer: `region_runner.rs` (the per-region actor core — `RegionInput`/`RegionOutput` message pair, `RegionRunner`, `SerializedRegion` parking format; this message pair is the future network seam) and `world_manager.rs` (`WorldManager` — threadless sessions/homes/subscriptions/parking-lot brain, parameterized by a `RegionSpawner`: OS threads on the server, `InlineSpawner` for wasm/tests). Regions tile the plane in signed `RegionCoords` (`RegionId` is no longer `ChunkCoords`), each `REGION_SIZE = 1024.0` world units (32×32 chunks); sims stay region-local, the world offset exists only at the render boundary. Beyond the subscribed near window, terrain streams as server-built LOD (`lod.rs`: `LodKey` — per-region for factors 2/4/16, per-tile for 64/256 — plus downsamplers and `PackedLodGrid` RLE; `lod_builder.rs`: `LodBuilder` jobs the `WorldManager` submits on `RequestLod`, sourcing `TerrainSnapshot`s from running/parked regions or worldgen, invalidated by `TerrainChanged` cadence pushes). Boundary handoff (`scan_boundaries` producing `EntityArrived`/`GhostUpdate` events, ghost mirrors with TTL, and upgrade-in-place of a ghost into an owned entity) runs inside the region tick; `PlayerInput` routes by the manager's `homes` map, not the client's region stamp, so input follows the player across a flip.
- `crates/server` — Tokio + quinn QUIC server. Hub-and-spoke topology: the netcode edge (tokio/quinn + WebTransport ingress) feeds a manager thread running the shared `WorldManager` core, which routes events to N region threads (`region_threads.rs`, one `RegionRunner` + tick timer per running region) over crossbeam channels — never shared memory, so the thread boundary can later become a network boundary. Regions self-tick; cycled-out regions park as `SerializedRegion` blobs after a grace period and restore bit-exact on resubscribe. Self-signed cert generated at startup via rcgen.
- `crates/client` — A Bevy app. `renderer/` holds the bridge modules that translate sim state into Bevy render state (`bridge.rs`/`convert.rs`/`interpolate.rs`/`meshing/` (binary greedy mesher, AO, packed vertex format, neighbor gather, remesh queue, cave-culling visibility, LOD meshing; `records.rs` packs 8-byte per-quad records for the pulled render path — its layout is locked to `assets/shaders/voxel_pull.wgsl`)/`voxel_pull/` (the default pulled-quad render path: per-chunk storage buffers drawn by custom Opaque3d/prepass/Shadow phase items, no Mesh assets; the `RenderPath` resource toggles back to the legacy Mesh/Material path)/`lod_rings.rs` (renders the server-streamed LOD rings: decodes `LodData` payloads into per-key roots, retires superseded roots only once a replacement is visibly meshed)/`voxel_material.rs`), plus engine-neutral input handling (`input.rs`); networking lives in `netcode.rs`; `main.rs` coordinates the game loop (netcode + rollback) and wires up the Bevy `App`. The instance layer (`instance.rs`) diffs a ring table into `RequestLod`/`ReleaseLod` subscriptions each tick. `game` and `server` must stay Bevy-free and windowing-library-free — they are shared by both client and server and must not pull in rendering/windowing deps.
- `crates/worldgen` — Deterministic world generation: `generate_region(RegionCoords)` is a pure function from region coordinates to the full 32×32 chunk grid, which is what makes "cycle out = park, cycle in = restore-or-regenerate" safe. Depends on `game` (not vice versa) — generation is injected into the `WorldManager` as a closure. `coarse_tile` point-samples one profile per LOD cell column for the 64/256 tile factors — the cheap pristine-terrain base the server overlays authoritative regions onto.

## Vendored Forks (do not treat as dependencies to update)

`crates/nalgebra`, `crates/simba`, `crates/parry`, `crates/rapier`, `crates/approx`, `crates/ordered-float`, `crates/slotmapd`, and `crates/block-mesh` are vendored, locally patched forks wired in via `[workspace.dependencies]` path overrides. They exist to guarantee **cross-machine determinism** for the rollback netcode: `simba` is forced onto `libm` (`libm_force`), `rapier3d` uses `enhanced-determinism`, floats are wrapped in `ordered-float`. The rapier/parry forks also carry the `StepJournal` rollback machinery — a per-tick mutation journal plus exact LIFO inverses — that lets `PhysicsPipeline::step` roll back without whole-state snapshots. Don't switch these back to crates.io versions, and be aware that changes to simulation math can break determinism between client and server.

## Conventions

- Serialization over the wire is `bincode`, framed by `game::encode_packet` / `decode_packet` (`crates/game/src/protocol.rs`): one format byte, and packets above `PACKET_COMPRESS_THRESHOLD` are lz4-compressed (`lz4_flex`; a region snapshot goes 40 MiB → 0.9 MiB). Every transport edge — server writer + QUIC/WebTransport ingress, native client, wasm client — must use that pair; never call `bincode` on the wire directly. State hashing uses `crc32fast`.
- Cross-thread communication uses `crossbeam` channels (game loop ↔ network loop ↔ render loop); async (tokio) is confined to the QUIC networking edges.
- Editions are mixed intentionally: older crates are 2021, newer ones (`macros`, `worldgen`) are 2024.
- `TODO.md` tracks the current high-level direction (multi-region/chunk-grid simulation instances).
- The root `justfile` is the canonical set of build/run/test commands — use it, and edit it whenever the underlying commands change so it never drifts from reality.
