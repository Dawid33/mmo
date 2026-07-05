# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A voxel MMO in Rust with client/server architecture over QUIC, built around deterministic simulation and rollback netcode. The client is a Bevy app (rendering, windowing, input); the simulation crates (`game`, `server`) stay engine-agnostic — no Bevy, no windowing library.

## Building and Running

`cargo build --workspace --bins` works on stable. Development typically uses the Cranelift codegen backend for fast builds, via a locally built `cargo-clif`:

```sh
# Build everything (see scripts/build.sh)
~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins

# Run server + client together (see scripts/run.sh)
~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin server
~/Software/rustc_codegen_cranelift/dist/cargo-clif run --bin client
```

The server listens on `127.0.0.1:6466` (hardcoded in `crates/server/src/lib.rs`); the client connects to localhost. Run the server before/alongside the client.

The game crate has the rollback test suite: `cargo test -p game` (transaction/undo invariants in `tests/log_model.rs` and `tests/simple.rs`, seeded randomized rollback in `tests/random_ops.rs`, vendored-container inverse-op guarantees in `tests/hash_restore.rs`). Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact. The client has its own headless test suite: `cargo test -p client` (26 tests covering the sim-bridge, coordinate conversion, interpolation, and async chunk meshing, run via `MinimalPlugins`/`AssetPlugin` with no window or GPU).

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

- `docker-compose up` starts Pyroscope (`:4040`) + Grafana (`:3000`); build client/server with the `pyroscope` feature to push profiles.
- `scripts/perf.sh` records with `perf` and opens `hotspot`.

## Workspace Layout

First-party crates (workspace members):

- `crates/macros` — Proc macro crate providing `#[rollback]` on the state module. Fields marked `#[undo(cell|map|slotmap)]` get tier-1 wrappers (`UndoCell`/`UndoMap`/`UndoSlotMap`) whose mutating methods log typed, exactly-invertible deltas automatically; `#[emit(insert = ..., remove = ...)]` derives renderer `GameDataUpdate`s from those deltas in both apply and undo directions. Unmarked fields stay `Undo<T>` (tier 2): mutate via `undo_scope()`/`undo()` closures that MUST be true inverses of the full serialized state (`hash(before) == hash(after undo)` is enforced at rollback time), or `change()` for snapshot-restore; `emit_on_undo()` registers compensating render events. The vendored `slotmapd`/`rapier` forks expose exact LIFO inverses (`revert_insert`/`revert_remove`) because plain `remove(insert(x))` does NOT restore allocator state (free lists, versions/generations) — see `crates/game/tests/hash_restore.rs`. The macro generates all undo infrastructure (`Rollback`, `Undo<T>`, the wrappers, the log) at its invocation site and hardcodes `crate::GameDataUpdate`/`crate::serde` paths, so it must expand in the crate that defines the update enums — that's `game`. The design spec lives in `docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md`.
- `crates/game` — Deterministic simulation shared by client and server, and the rollback-netcode state layer (absorbed from the former `crates/rollback`): `World`, `Region`, controllers (camera, rapier physics), plus `state.rs` (`GameData`, the `#[rollback]` invocation, `GameDataUpdate`), `voxel.rs` (`Chunk` 32³ voxels), `protocol.rs` (packets/events), `input.rs` (engine-neutral input). `lib.rs` glob re-exports these at the crate root — the macro expansion and `borrow::Partial` derives depend on that. `TICK_RATE = 50`. Also hosts the world-level multi-region layer: `region_runner.rs` (the per-region actor core — `RegionInput`/`RegionOutput` message pair, `RegionRunner`, `SerializedRegion` parking format; this message pair is the future network seam) and `world_manager.rs` (`WorldManager` — threadless sessions/homes/subscriptions/parking-lot brain, parameterized by a `RegionSpawner`: OS threads on the server, `InlineSpawner` for wasm/tests). Regions tile the plane in signed `RegionCoords` (`RegionId` is no longer `ChunkCoords`), each `REGION_SIZE = 256.0` world units (8×8 chunks); sims stay region-local, the world offset exists only at the render boundary.
- `crates/server` — Tokio + quinn QUIC server. Hub-and-spoke topology: the netcode edge (tokio/quinn + WebTransport ingress) feeds a manager thread running the shared `WorldManager` core, which routes events to N region threads (`region_threads.rs`, one `RegionRunner` + tick timer per running region) over crossbeam channels — never shared memory, so the thread boundary can later become a network boundary. Regions self-tick; cycled-out regions park as `SerializedRegion` blobs after a grace period and restore bit-exact on resubscribe. Self-signed cert generated at startup via rcgen.
- `crates/client` — A Bevy app. `renderer/` holds the bridge modules that translate sim state into Bevy render state (`bridge.rs`/`convert.rs`/`interpolate.rs`/`meshing.rs`/`voxel_material.rs`), plus engine-neutral input handling (`input.rs`); networking lives in `netcode.rs`; `main.rs` coordinates the game loop (netcode + rollback) and wires up the Bevy `App`. `game` and `server` must stay Bevy-free and windowing-library-free — they are shared by both client and server and must not pull in rendering/windowing deps.
- `crates/worldgen` — Deterministic world generation: `generate_region(RegionCoords)` is a pure function from region coordinates to the full 8×8 chunk grid (parity-checkerboard floor heights for now), which is what makes "cycle out = park, cycle in = restore-or-regenerate" safe. Depends on `game` (not vice versa) — generation is injected into the `WorldManager` as a closure.

## Vendored Forks (do not treat as dependencies to update)

`crates/nalgebra`, `crates/simba`, `crates/parry`, `crates/rapier`, `crates/approx`, `crates/ordered-float`, `crates/slotmapd`, and `crates/block-mesh` are vendored, locally patched forks wired in via `[workspace.dependencies]` path overrides. They exist to guarantee **cross-machine determinism** for the rollback netcode: `simba` is forced onto `libm` (`libm_force`), `rapier3d` uses `enhanced-determinism`, floats are wrapped in `ordered-float`. The rapier/parry forks also carry the `StepJournal` rollback machinery — a per-tick mutation journal plus exact LIFO inverses — that lets `PhysicsPipeline::step` roll back without whole-state snapshots. Don't switch these back to crates.io versions, and be aware that changes to simulation math can break determinism between client and server.

## Conventions

- Serialization over the wire is `bincode`; state hashing uses `crc32fast`.
- Cross-thread communication uses `crossbeam` channels (game loop ↔ network loop ↔ render loop); async (tokio) is confined to the QUIC networking edges.
- Editions are mixed intentionally: older crates are 2021, newer ones (`macros`, `worldgen`) are 2024.
- `TODO.md` tracks the current high-level direction (multi-region/chunk-grid simulation instances).
