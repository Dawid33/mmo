---
name: workspace-test-nalgebra-fails
description: "STALE since 2026-09-27: the nalgebra fork is gone (crates.io nalgebra inside rapier 0.36); vendored rapier/parry are path deps, not workspace members — still scope tests per crate via just test-one"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 79a844a4-e827-425c-81e8-b8b2c80ee746
---

**STALE since 2026-09-27** ([[vendored-deps-update-investigation]]): `crates/nalgebra` no longer exists; workspace members are client, game, macros, physics-next, server, state-hash, state-hash-derive, worldgen. Scoping tests per crate is still the rule (for speed — see CLAUDE.md); the history below no longer applies.

`cargo test --workspace` does NOT pass cleanly: the vendored `crates/nalgebra` fork's own test suite (`nalgebra --test lib`) fails to COMPILE with E0277 (`ShapeConstraint: SameNumberOfColumns/Rows`) / E0282. This is pre-existing and internal to the vendored fork — first-party crates (`game`, `client`, `server`, `worldgen`) are all downstream of nalgebra and cannot affect its tests. Per CLAUDE.md the vendored forks are not deps to fix.

**How to apply:** To verify first-party work, scope tests to the crates you touched: `cargo test -p game -p worldgen -p client -p server`. Treat a `nalgebra --test lib` failure in a `--workspace` run as pre-existing noise, not a regression. `cargo build --workspace --bins` and `cargo check -p client --target wasm32-unknown-unknown` are still fine as full-workspace gates. Related: [[wasm32-usize-sentinel-hazard]].
