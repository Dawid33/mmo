---
name: target-dir-size
description: 2026-10-31 target/ hit 200 GB; fixes landed: nightly embed-metadata, split-debuginfo + line-tables deps, one `it` test binary per crate, target/variants for Bevy-feature builds
metadata:
  type: project
---

target/ grew to 200 GB (2026-10-31). Stable cargo never GCs target/. User picked fixes #1, #3, #4
+ nightly `-Zembed-metadata=no` (see [[one-stable-toolchain]]); declined/deferred cargo-sweep (#2)
and cargo-hakari/feature-unification (#5).

- Baseline measurement: `cargo test -p game --no-run` on stable + old profiles = 10.8 GB (49 binaries).
- Integration tests: `tests/it.rs` per crate (`autotests = false`); separate-process files listed at
  its top (HASH_MODE flippers hash_mode/voxel_body, forged_world, wasm tests, release perf gates).
- Measured after: same game test build 10.8 → 2.2 GB. slint-ui incremental was 11 GB (generated Slint code, ~3 GB per build kind) → `[profile.dev.package.slint-ui]` line-tables + incremental=false → 0.
- `just clippy` (-D warnings) was ALREADY red on stable at c3f6d62 (crates/macros lints); nightly adds a few in bbmodel/block-mesh. Not fixed.
- Concurrent cargo builds by the user in the same checkout happen — a link error
  (`undefined symbol: main` in a build script) appeared while racing one; see whether it recurs.

**Why:** disk was 80% full. **How to apply:** new test files go in it.rs unless they touch
process-global state; Bevy-feature builds use `--target-dir {{variant_dir}}/<feat>`.
