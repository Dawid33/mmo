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
//! - `state_hash_check`: sets the process-global state hash interval.

mod common;
mod adventure;
mod anim;
mod body;
mod body_scale;
mod block_props;
mod camera_controller;
mod camera_look;
mod chests;
mod chop;
mod chunk_units;
mod credential;
mod coordinator;
mod coordinator_lifecycle;
mod coordinator_requests;
mod creature_shapes;
mod gjk_upstream;
mod halo;
mod handoff_manager;
mod world_clock;
mod handoff_protocol;
mod handoff_scan;
mod handoff_schedule;
mod handoff_state;
mod history;
mod inventory;
mod item_piles;
mod items;
mod mining;
mod history_handover;
mod history_play;
mod hash_restore;
mod intents;
mod jump_arc;
mod kinematic;
mod log_model;
mod mass_props;
mod multi_client;
mod orders;
mod ownership;
mod packet_codec;
mod pathfind;
mod portable;
mod player;
mod people;
mod physics_goldens;
mod play_region;
mod random_ops;
mod region_coords;
mod region_runner;
mod rollback_restore;
mod save_cut;
mod shape_cast_grazing;
mod simple;
mod slime;
mod state_hash;
mod stud;
mod stud_carve;
mod stud_gameplay;
mod stud_collider;
mod stud_ground;
mod stud_halo;
mod stud_lod;
mod stud_ops;
mod stud_rebrick;
mod stud_orders;
mod stud_portable;
mod stud_tiles;
mod sunk_body;
mod swimming;
mod ui_roundtrip;
mod walking_smoothness;
mod water;
mod world_manager;
