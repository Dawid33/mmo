# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A voxel MMO in Rust with client/server architecture over QUIC, built around deterministic simulation and rollback netcode. No framework — custom engine using winit + wgpu directly (Bevy was removed).

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

The rollback crate has the test suite: `cargo test -p rollback` (transaction/undo invariants in `tests/log_model.rs` and `tests/simple.rs`, seeded randomized rollback in `tests/random_ops.rs`, vendored-container inverse-op guarantees in `tests/hash_restore.rs`). Rollback correctness bar: `hash(before) == hash(after undo)`, bit-exact.

### Profiling

- `docker-compose up` starts Pyroscope (`:4040`) + Grafana (`:3000`); build client/server with the `pyroscope` feature to push profiles.
- `scripts/perf.sh` records with `perf` and opens `hotspot`.

## Workspace Layout

First-party crates (workspace members):

- `crates/rollback` — Core rollback-netcode state layer. Defines `Chunk` (32³ voxels), `GameData`, transactions/undo, input types. The design spec lives in `docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md`.
- `crates/macros` — Proc macro crate providing `#[rollback]` on the state module. Fields marked `#[undo(cell|map|slotmap)]` get tier-1 wrappers (`UndoCell`/`UndoMap`/`UndoSlotMap`) whose mutating methods log typed, exactly-invertible deltas automatically; `#[emit(insert = ..., remove = ...)]` derives renderer `GameDataUpdate`s from those deltas in both apply and undo directions. Unmarked fields stay `Undo<T>` (tier 2): mutate via `undo_scope()`/`undo()` closures that MUST be true inverses of the full serialized state (`hash(before) == hash(after undo)` is enforced at rollback time), or `change()` for snapshot-restore; `emit_on_undo()` registers compensating render events. The vendored `slotmapd`/`rapier` forks expose exact LIFO inverses (`revert_insert`/`revert_remove`) because plain `remove(insert(x))` does NOT restore allocator state (free lists, versions/generations) — see `crates/rollback/tests/hash_restore.rs`.
- `crates/game` — Deterministic simulation shared by client and server: `World`, `Region`/`RegionGroup`, camera, physics (rapier), meshing. `TICK_RATE = 50`.
- `crates/server` — Tokio + quinn QUIC server. Deliberately a "dumb router" of game event packets: orders incoming client packets by tick, executes ticks, broadcasts. Self-signed cert generated at startup via rcgen.
- `crates/client` — winit/wgpu client. `GameInstanceManager` in `main.rs` coordinates netcode + rollback; rendering in `render_world.rs`/`window.rs`, networking in `netcode.rs`.
- `crates/worldgen` — World generation (currently a stub).

## Vendored Forks (do not treat as dependencies to update)

`crates/nalgebra`, `crates/simba`, `crates/parry`, `crates/rapier`, `crates/approx`, `crates/ordered-float`, `crates/slotmapd`, and `crates/block-mesh` are vendored, locally patched forks wired in via `[workspace.dependencies]` path overrides. They exist to guarantee **cross-machine determinism** for the rollback netcode: `simba` is forced onto `libm` (`libm_force`), `rapier3d` uses `enhanced-determinism`, floats are wrapped in `ordered-float`. Don't switch these back to crates.io versions, and be aware that changes to simulation math can break determinism between client and server.

## Conventions

- Serialization over the wire is `bincode`; state hashing uses `crc32fast`.
- Cross-thread communication uses `crossbeam` channels (game loop ↔ network loop ↔ render loop); async (tokio) is confined to the QUIC networking edges.
- Editions are mixed intentionally: older crates are 2021, newer ones (`rollback`, `macros`, `worldgen`) are 2024.
- `TODO.md` tracks the current high-level direction (multi-region/chunk-grid simulation instances).
