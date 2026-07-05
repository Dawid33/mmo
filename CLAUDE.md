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

The server listens on `127.0.0.1:6466` (hardcoded in `crates/server/src/main.rs`); the client connects to localhost. Run the server before/alongside the client.

The game crate has the rollback test suite: `cargo test -p game` (transaction/undo invariants in `tests/log_model.rs` and `tests/simple.rs`, seeded randomized rollback in `tests/random_ops.rs`, vendored-container inverse-op guarantees in `tests/hash_restore.rs`). Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact. The client has its own headless test suite: `cargo test -p client` (26 tests covering the sim-bridge, coordinate conversion, interpolation, and async chunk meshing, run via `MinimalPlugins`/`AssetPlugin` with no window or GPU).

Bevy is pinned to `0.18` in the root `Cargo.toml`; Bevy's API surface moves fast between minor versions, so treat upgrades as a deliberate, tested migration rather than a routine bump — API drift (renamed types/traits, moved modules, changed system-set names) is a known hazard.

### WASM build (offline single-player)

The client also targets `wasm32-unknown-unknown` (native QUIC netcode is
replaced by an embedded `LocalServer`; see `crates/client/src/local_server.rs`):

```sh
# From the repo root (so ./assets is served):
cargo run -p client --target wasm32-unknown-unknown   # opens via wasm-server-runner
```

Requires `rustup target add wasm32-unknown-unknown` and
`cargo install wasm-server-runner`.

### Profiling

- `docker-compose up` starts Pyroscope (`:4040`) + Grafana (`:3000`); build client/server with the `pyroscope` feature to push profiles.
- `scripts/perf.sh` records with `perf` and opens `hotspot`.

## Workspace Layout

First-party crates (workspace members):

- `crates/macros` — Proc macro crate providing `#[rollback]` on the state module. Fields marked `#[undo(cell|map|slotmap)]` get tier-1 wrappers (`UndoCell`/`UndoMap`/`UndoSlotMap`) whose mutating methods log typed, exactly-invertible deltas automatically; `#[emit(insert = ..., remove = ...)]` derives renderer `GameDataUpdate`s from those deltas in both apply and undo directions. Unmarked fields stay `Undo<T>` (tier 2): mutate via `undo_scope()`/`undo()` closures that MUST be true inverses of the full serialized state (`hash(before) == hash(after undo)` is enforced at rollback time), or `change()` for snapshot-restore; `emit_on_undo()` registers compensating render events. The vendored `slotmapd`/`rapier` forks expose exact LIFO inverses (`revert_insert`/`revert_remove`) because plain `remove(insert(x))` does NOT restore allocator state (free lists, versions/generations) — see `crates/game/tests/hash_restore.rs`. The macro generates all undo infrastructure (`Rollback`, `Undo<T>`, the wrappers, the log) at its invocation site and hardcodes `crate::GameDataUpdate`/`crate::serde` paths, so it must expand in the crate that defines the update enums — that's `game`. The design spec lives in `docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md`.
- `crates/game` — Deterministic simulation shared by client and server, and the rollback-netcode state layer (absorbed from the former `crates/rollback`): `World`, `Region`, controllers (camera, rapier physics), plus `state.rs` (`GameData`, the `#[rollback]` invocation, `GameDataUpdate`), `voxel.rs` (`Chunk` 32³ voxels), `protocol.rs` (packets/events), `input.rs` (engine-neutral input). `lib.rs` glob re-exports these at the crate root — the macro expansion and `borrow::Partial` derives depend on that. `TICK_RATE = 50`.
- `crates/server` — Tokio + quinn QUIC server. Deliberately a "dumb router" of game event packets: orders incoming client packets by tick, executes ticks, broadcasts. Self-signed cert generated at startup via rcgen.
- `crates/client` — A Bevy app. `renderer/` holds the bridge modules that translate sim state into Bevy render state (`bridge.rs`/`convert.rs`/`interpolate.rs`/`meshing.rs`/`voxel_material.rs`), plus engine-neutral input handling (`input.rs`); networking lives in `netcode.rs`; `main.rs` coordinates the game loop (netcode + rollback) and wires up the Bevy `App`. `game` and `server` must stay Bevy-free and windowing-library-free — they are shared by both client and server and must not pull in rendering/windowing deps.
- `crates/worldgen` — World generation (currently a stub).

## Vendored Forks (do not treat as dependencies to update)

`crates/nalgebra`, `crates/simba`, `crates/parry`, `crates/rapier`, `crates/approx`, `crates/ordered-float`, `crates/slotmapd`, and `crates/block-mesh` are vendored, locally patched forks wired in via `[workspace.dependencies]` path overrides. They exist to guarantee **cross-machine determinism** for the rollback netcode: `simba` is forced onto `libm` (`libm_force`), `rapier3d` uses `enhanced-determinism`, floats are wrapped in `ordered-float`. Don't switch these back to crates.io versions, and be aware that changes to simulation math can break determinism between client and server.

## Conventions

- Serialization over the wire is `bincode`; state hashing uses `crc32fast`.
- Cross-thread communication uses `crossbeam` channels (game loop ↔ network loop ↔ render loop); async (tokio) is confined to the QUIC networking edges.
- Editions are mixed intentionally: older crates are 2021, newer ones (`macros`, `worldgen`) are 2024.
- `TODO.md` tracks the current high-level direction (multi-region/chunk-grid simulation instances).
