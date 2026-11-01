//! All of `game`'s integration tests in ONE binary (`--test it`): one
//! link instead of one per file, which is most of a dev target dir. Each
//! file is a module, so filter by its name: `cargo test -p game --test it <file>::`.
//!
//! Tests here share a process and run in parallel with each other: never
//! change process-global state (hash mode, the installed world, a logger)
//! from one — give such a file its own `[[test]]` in Cargo.toml instead.
//! Separate binaries:
//! - `wasm_determinism`: also built for wasm32 (just determinism-wasm).
//! - `hash_mode`: flips the process-global HASH_MODE to Off/Delta.
//! - `voxel_body`: flips the process-global HASH_MODE to Delta.

mod common;
mod adventure;
mod anim;
mod block_edit;
mod block_props;
mod camera_controller;
mod camera_look;
mod chop;
mod chunk_blocks;
mod chunk_units;
mod collider_genesis;
mod gjk_upstream;
mod ground;
mod handoff_manager;
mod handoff_protocol;
mod handoff_scan;
mod handoff_schedule;
mod handoff_state;
mod history;
mod hash_restore;
mod jump_arc;
mod kinematic;
mod lod_streaming;
mod log_model;
mod multi_client;
mod orders;
mod packet_codec;
mod pathfind;
mod physics_goldens;
mod play_region;
mod random_ops;
mod region_coords;
mod region_from_chunks;
mod region_from_columns;
mod region_runner;
mod rollback_restore;
mod shape_cast_grazing;
mod simple;
mod slime;
mod spawn_surface;
mod state_hash;
mod sunk_body;
mod terrain_snapshot;
mod ui_roundtrip;
mod underground_edit;
mod walking_smoothness;
mod world_manager;
