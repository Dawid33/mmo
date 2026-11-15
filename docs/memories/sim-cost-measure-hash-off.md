---
name: sim-cost-measure-hash-off
description: Tick/sim cost measured in a test harness is 80-90% crc32 unless HashMode is set Off first — it hid a 3-5x KCC regression as "+10%"
metadata:
  type: feedback
---

Any tick-cost / sim-cost measurement in a test or example MUST call `game::set_hash_mode(game::HashMode::Off)` first. Tests default to `HashMode::Full` (whole-field crc32 per undo entry); the client/server binaries run `Off`.

**Why:** 2026-09-20 — measured a generated-region tick at "5.5 ms empty / 9.4 ms with a player" and judged my KCC change "+10%". A perf profile showed 80–90% of that was `crc32fast`. With hashing off the truth was: empty region 8 µs, idle player ~0.2 ms before my commit → ~1.0 ms after (5x), walking ~0.6 → ~1.9 ms (3x). The player's character controller IS the home-region tick, and every replayed event in the prediction death-spiral ([[client-prediction-runaway]]) pays it — the user hit an immediate rubber-band loop. The live capture said the same thing (server region with the player 3.95 ms vs 0.39 ms empty) and I should have weighed it over the harness.

**How to apply:** set HashMode Off, then compare; treat the `[region …] avg=` lines and `[client-budget] ctrl=/sim n=` of a `just perf-capture` as the ground truth for per-tick cost. Also: never A/B by swapping source files in the shared checkout while the user may be building (`git show HEAD~1:f > f`) — their `cargo run` can link a mixed binary; derive from measurements already taken or ask. Related: [[kcc-voxel-seam-jitter]].
