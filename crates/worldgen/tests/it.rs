//! All of `worldgen`'s integration tests in ONE binary (`--test it`): one
//! link instead of one per file, which is most of a dev target dir. Each
//! file is a module, so filter by its name: `cargo test -p worldgen --test it <file>::`.
//!
//! Tests here share a process and run in parallel with each other: never
//! change process-global state (hash mode, the installed world, a logger)
//! from one — give such a file its own `[[test]]` in Cargo.toml instead.
//! Separate binaries:
//! - `forged_world`: forged::install pins the process's world once.
//! - `client_region_load_cost`: sets the process-global HASH_MODE to Off.
//! - `region_load_cost`: release-mode benchmark (just bench-region).

mod common;
mod forge_lib;
mod halo_probe;
mod lod_bricks;
mod player_walk;
mod scene_hash_golden;
mod slopes;
mod tile_map;
mod tree_chop;
mod tree_fall;
mod trees_in_terrain;
mod underground;
