//! All of `worldgen`'s integration tests in ONE binary (`--test it`): one
//! link instead of one per file, which is most of a dev target dir. Each
//! file is a module, so filter by its name: `cargo test -p worldgen --test it <file>::`.
//!
//! Tests here share a process and run in parallel with each other: never
//! change process-global state (hash mode, the installed world, a logger)
//! from one — give such a file its own `[[test]]` in Cargo.toml instead.
//! The one exception is the block props (`game::install_block_props`):
//! every installer here installs the same `assets/blocks/blocks.ron` props,
//! so the process flips once from the all-solid default to those. A test
//! whose outcome reads them (starting kit, drops, solidity) must install
//! them before it builds state — `game::install_block_props` with the
//! registry parsed from `blocks.ron`, as `tree_fall` does, or
//! game's `common::install_real_block_props`, which `scene_hash_golden`
//! mounts by `#[path]` — or it races that flip.

mod forge_lib;
mod scene_hash_golden;
mod tree_fall;
