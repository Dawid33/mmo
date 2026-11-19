//! All of `physics-next`'s integration tests in ONE binary (`--test it`): one
//! link instead of one per file, which is most of a dev target dir. Each
//! file is a module, so filter by its name: `cargo test -p physics-next --test it <file>::`.
//!
//! Tests here share a process and run in parallel with each other: never
//! change process-global state (hash mode, the installed world, a logger)
//! from one — give such a file its own `[[test]]` in Cargo.toml instead.
//! Separate binaries:
//! - `state_hash_wasm`: also built for wasm32 (just determinism-wasm).

mod common;
mod arena_revert;
mod continuous_surfaces;
mod fixed_mass_props;
mod journal_fuzz;
mod journal_parallel;
mod kinematic_sleep;
mod pipeline_memo;
mod state_hash_parity;
mod step_journal;
mod upstream_movement_probe;
mod voxels_batch_edit;
mod voxels_cast;
mod voxels_hash_cache;
mod walking_smoothness;
