# Repository Guidelines

## Project Structure & Module Organization

This is a Rust workspace for a deterministic voxel MMO. First-party code lives in `crates/client` (Bevy rendering, input, and networking), `crates/game` (engine-neutral simulation and rollback), `crates/server` (Tokio/QUIC services), `crates/worldgen` (deterministic terrain generation), and `crates/macros` (rollback proc macros). Unit and integration tests sit beside each crate in `src/` and `tests/`. Runtime textures, block definitions, and WGSL shaders are under `assets/`; scripts and profiling helpers are under `scripts/`; design notes and implementation plans are under `docs/`.

Several other directories in `crates/` are patched vendored forks (`rapier`, `parry`, `nalgebra`, and others). Do not replace them with crates.io dependencies: their changes preserve rollback and cross-machine determinism.

## Build, Test, and Development Commands

Use the root `justfile` as the canonical task interface; run `just --list` for all recipes.

- `just build` builds all workspace binaries.
- `just check` type-checks the workspace.
- `just run` launches the local server and Bevy client; `just server` and `just client` run them separately.
- `just test` runs the quick first-party test suites.
- `just test-full` includes slow tests and is the required final verification.
- `just fmt-check` checks formatting; `just clippy` rejects Clippy warnings.

Keep the `justfile` synchronized when changing a build, run, or test workflow.

## Coding Style & Naming Conventions

Follow standard `rustfmt` output (four-space indentation) and run `just fmt` before submitting. Use `snake_case` for modules, functions, and test names; `UpperCamelCase` for types and traits; and `SCREAMING_SNAKE_CASE` for constants. Keep `game` and `server` free of Bevy/windowing dependencies. Preserve deterministic ordering, serialization, hashing, and exact inverse operations in simulation code.

## Testing Guidelines

Add focused unit tests near implementation code and cross-component cases in `crates/<name>/tests/`. Name tests by observable behavior. Tests exceeding 100 ms must use `#[ignore = "slow: >100ms — run via just test-full"]`. Rollback changes must prove bit-exact restoration, typically `hash(before) == hash(after undo)`.

## Commit & Pull Request Guidelines

History follows scoped Conventional Commit-style subjects, such as `feat(client): ...`, `fix(justfile): ...`, `perf(worldgen): ...`, and `docs: ...`. Keep commits narrow and imperative. Pull requests should explain behavior and architectural impact, link relevant issues or design docs, list verification commands, and include screenshots or profiling evidence for rendering/UI/performance changes.
