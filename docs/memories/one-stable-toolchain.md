---
name: one-stable-toolchain
description: One toolchain for everything, pinned in rust-toolchain.toml — since 2026-10-31 a DATED nightly (user asked, for -Zembed-metadata=no / target size); still no cargo-clif
metadata:
  type: feedback
---

Use ONE toolchain for everything, pinned in `rust-toolchain.toml`. 2026-10-20 it was stable
1.99.1; on 2026-10-31 the user asked to switch to nightly for the target-dir savings
(`[unstable] embed-metadata = false` in .cargo/config.toml) — pinned `nightly-2026-10-04`
(1.101.0-nightly). Still no Cranelift `cargo-clif` wrapper; the justfile's `cargo` is plain `cargo`.

**Why:** a broken floating `nightly-2025-12-23` install (old cargo-clif path) once broke the
user's builds — they want a single predictable toolchain; nightly is acceptable only as a dated
pin, and only because target/ reached 200 GB ([[target-dir-size]]).
**How to apply:** never a floating `nightly`; bump the date deliberately and re-run `just smoke`
+ `just determinism-wasm`. `-Z` flags are fine now, but keep them in config, not ad hoc.
