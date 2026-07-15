# Configurable Cargo Justfile Design

## Goal

Make the `cargo` variable defined at the top of the root `justfile` control every
Cargo command executed by a recipe. This keeps stable Cargo and an optional
`CARGO_CLIF` or command-line override consistent across build, run, test, web,
formatting, linting, profiling, and cleanup workflows.

## Scope

Replace each executable `cargo ...` occurrence in the root `justfile` with
`{{cargo}} ...`. Preserve prose, comments, installation examples, and the
variable's stable-Cargo default. The `build-stable` recipe will consequently use
the configured Cargo command despite its current name and description, because
the requested invariant is that every recipe uses `{{cargo}}`.

No Rust source, Cargo manifest, dependency, profile, target directory, or recipe
command beyond the Cargo executable selection will change.

## Verification

Use a static scan to prove no recipe command begins with bare `cargo`, then run
`just --dry-run` for representative build, test, run, web, formatting, linting,
and cleanup recipes with a sentinel `cargo` override. Each rendered Cargo command
must contain the sentinel executable.
