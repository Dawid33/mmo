//! All of `game`'s integration tests in ONE binary (`--test it`): one
//! link instead of one per file, which is most of a dev target dir. Each
//! file is a module, so filter by its name: `cargo test -p game --test it <file>::`.
//!
//! Tests here share a process and run in parallel with each other: never
//! change process-global state (hash mode, the installed world, a logger)
//! from one — give such a file its own `[[test]]` in Cargo.toml instead.
//! The one exception is the block props (`game::install_block_props`): many
//! tests install them, always the same `assets/blocks/blocks.ron` props, so
//! the process flips once from the all-solid default to those. A test whose
//! outcome reads them (starting kit, drops, hardness, solidity) must install
//! them before it builds state (`common::install_real_block_props`; the seam
//! rig does), or it races that flip.
//! Separate binaries:
//! - `wasm_determinism`: also built for wasm32 (just determinism-wasm).
//! - `hash_mode`: flips the process-global HASH_MODE to Off/Delta.
//! - `voxel_body`: flips the process-global HASH_MODE to Delta.
//! - `state_hash_check`: sets the process-global state hash interval.

mod body;
mod common;
mod handoff_state;
mod history_handover;
mod lod_streaming;
mod peer_schedule;
mod physics_goldens;
mod region_runner;
mod rollback_restore;
mod shape_cast_grazing;
mod spawn_surface;
mod terrain_stairs;
