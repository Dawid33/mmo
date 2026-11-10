//! All of `client`'s integration tests in ONE binary (`--test it`): one
//! link instead of one per file, which is most of a dev target dir. Each
//! file is a module, so filter by its name: `cargo test -p client --test it <file>::`.
//!
//! Tests here share a process and run in parallel with each other: never
//! change process-global state (hash mode, the installed world, a logger)
//! from one — give such a file its own `[[test]]` in Cargo.toml instead.
//! Separate binaries:
//! - `keepup_budget`: release-mode perf gate (just keepup-budget).
//! - `reconcile_keepup`: release-mode soak; installs a process-global logger.

mod bevy_input;
mod crossing;
mod debug_lines;
mod harness_smoke;
mod region_threads;
mod session_reset;
