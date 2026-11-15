---
name: smoke-tests-not-full-suites
description: "Never run whole game/server/client test suites as routine verification; use `just test-one` for touched tests and `just smoke` for sanity"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: ab94b011-65cd-4a21-8156-09a0c14d3c11
  modified: 2026-09-19T14:00:25.368Z
---

Do not run `just test`, `just test-full`, or whole per-crate suites (`cargo test -p game|client|server`) as a routine check. Verify with `just test-one <crate> "<args naming --lib or --test target>"` for the tests a change touches, plus `just smoke` as the general sanity check. Rule is also in CLAUDE.md.

**Why:** the user stopped a suite run on 2026-09-19 — "running game / server client tests take far too long, only run a couple of smoke tests in the future". Even the quick suites link ~30 game test binaries and ~9 Bevy-sized client binaries; `cargo test -p client` also rebuilds the naga/wgpu/Bevy chain because the client's `naga` dev-dependency adds the `wgsl-in` feature.

**Budget:** verification must finish in about a minute. `just smoke` is hard-limited to 60 s (~12 s warm): 2 game test binaries + server lib tests + a type-check of the client. It deliberately never builds a client test binary — the user killed a `just test-one client --lib` run on 2026-09-19 because the cold Bevy test build ran for minutes. Do not start client test builds without asking; say client tests are "compile-checked, not run" instead.

**How to apply:** always name the test target so cargo skips building the rest. Run a full suite only if the user asks or the change is genuinely wide, and say so before starting. Keep runs under the core cap ([[cap-build-test-parallelism]]). This supersedes the old "just test-full before declaring done" habit noted in [[genesis-cost-reduction-landed]].
