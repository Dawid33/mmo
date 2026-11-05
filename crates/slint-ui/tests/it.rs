//! All of `slint-ui`'s integration tests in ONE binary (`--test it`): one
//! link instead of one per file, which is most of a dev target dir. Each
//! file is a module, so filter by its name: `cargo test -p slint-ui --test it <file>::`.
//!
//! Tests here share a process and run in parallel with each other: never
//! change process-global state (hash mode, the installed world, a logger)
//! from one — give such a file its own `[[test]]` in Cargo.toml instead.

mod asset_viewer;
mod controls;
mod dev_panel;
mod embark;
mod fortress_panel;
mod fortress_wheel;
mod frontend;
mod game_panel;
mod hud;
mod inventory;
mod world_map;
