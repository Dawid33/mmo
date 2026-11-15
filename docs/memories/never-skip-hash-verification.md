---
name: never-skip-hash-verification
description: "User directive — never disable/skip rollback hash verification, even for perf; optimize around it instead"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: ffa91ce4-4121-48e6-b328-7fd1be0bbd11
---

When hash verification (`VERIFY_HASHES` / `set_hash_verification`) shows up as a hotspot, do NOT propose disabling it — the user explicitly said "don't ever skip hash verification" (2026-07-08, after a perf profile showed hashing at 38% of samples).

**Why:** `hash(before) == hash(after undo)` is the rollback correctness bar for the whole netcode; the user values that guarantee over the CPU cost.

**How to apply:** Treat hashing cost as fixed; optimize the thing being hashed (state size, hash frequency, incremental hashing) or the surrounding algorithm (re-sim count, [[client-prediction-runaway]]), never the verification itself.

Update 2026-07-09: satisfied in production via [[delta-hash-verification-plan]] — verification now runs in Delta mode (footprint digests) on client and server; Full stays the test/harness default. Off remains only as an escape hatch; still never suggest disabling.

Update 2026-07-11: at the user's explicit request, the production default (`set_hash_mode_from_env` unset) is now `Off` — desync is caught by the periodic tick-interval state hash (crc32 stamped on every 10th Tick, `STATE_HASH_INTERVAL_TICKS`) instead of per-entry verification. `HASH_MODE=delta`/`full` re-enable per-entry checks for debugging; tests/harnesses still pin Full. The directive still holds for the verification *machinery*: don't remove or weaken the interval hash or the Full/Delta modes for perf.

Update 2026-11-02: the 2026-07-11 interval hash had been lost (no production caller of `game::state_hash`; the doc comments still claimed it). Rebuilt at user request: server region hashes whole state every `game::DEFAULT_STATE_HASH_INTERVAL` = 100 ticks (`VOXEL_STATE_HASH_INTERVAL`, server-side, 0 = off) and ships it as `ServerPacket::HashedGameEvent(event, tick, hash)`; client checks on reconcile confirmation (`Region::expect_state_hash`). Same day the user set the binaries' per-entry default to `HashMode::Full` (`game::DEFAULT_HASH_MODE`) — "hash mode full with state hash interval set to 100" is normal play now; `HASH_MODE=delta|off` are the opt-outs.

Update 2026-11-02 (later): user set `DEFAULT_HASH_MODE` back to `Off` after Full made "Complete all orders" a multi-second rollback loop (per-entry Full hashing runs on EVERY undo entry, cost ∝ region size; not the interval check). Condition the user gave: Off is fine only because it doesn't touch the desync check — keep the 100-tick whole-state interval hash on. Normal play = per-entry Off + interval 100; `HASH_MODE=full|delta` for debugging undo closures.
