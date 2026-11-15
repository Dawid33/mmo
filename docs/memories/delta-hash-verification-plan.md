---
name: delta-hash-verification-plan
description: "IMPLEMENTED (develop fc1040b..a20bbf9, 2026-07-09) — per-entry HashMode{Off,Delta,Full}; production runs Delta; don't re-plan or re-implement"
metadata: 
  node_type: memory
  type: project
  originSessionId: 7bdfd00c-33ae-4dfc-903c-3b557649bcbc
---

IMPLEMENTED and merged on develop (8 commits, fc1040b..a20bbf9, 2026-07-09; final whole-branch review: ready-to-merge, no Critical/Important). Plan doc: `docs/superpowers/plans/2026-07-09-delta-hash-verification.md`. SDD ledger: `.superpowers/sdd/progress.md`.

What exists now:
- `game::HashMode { Off, Delta, Full }` + `game::set_hash_mode` (macro-generated); every log `Entry` carries a `verify` stamp read at log time — mode can change freely, the old one-way latch and `set_hash_verification` are GONE.
- Delta digests (hash only the mutation footprint): UndoMap entry, UndoSlotMap slot + slotmapd fork `alloc_fingerprint{,_head}`, Component slots, body-insert scopes, physics `StepJournal::pre_digest/revert_digest` (fork; `revert()` delegates to `revert_inner`). Tier-2 API: `undo_digest(pre, closure -> u32)`, `undo_scope_digest()`/`UndoScopeDigest::register(pre, closure -> u32)`.
- Client and server binaries run **Delta in ALL builds** (2026-07-10: the release-only cfg gate is gone) via `game::set_hash_mode_from_env()` — `HASH_MODE=off|delta|full` env var overrides, default Delta, panics on typos, println's the chosen mode at startup (before logger init, so not log::). Use `HASH_MODE=full just run` when chasing a footprint/capture gap. Tests/harnesses still default/pin Full (process-global default unchanged). Unknown-footprint paths (`change()`, `snapshot_raw()`) stay Full-stamped by design.
- Perf evidence (release, 100 ticks/rollbacks): Off 5.40ms/269µs, Delta 6.38ms/373µs, Full 10.80ms/5.70ms — `perf_evidence_hash_modes` in crates/game/tests/hash_mode.rs (skipped in test-full).

Pending/follow-ups (bundle when macro rollback arms are next touched):
- LIVE GUI smoke under release Delta never run (needs human: `just run`, walk, place/remove block, watch for "Hash verification failed (delta)" panics).
- `body_slot_digest` hashes (free_head, len) but not the slot's own next_free/generation (weaker than the slotmap analog); corruption-injection negative test for typed Delta arms; stamp-snippet DRY across ~6 log sites; `pub static HASH_MODE` visibility; UndoCell `unreachable!` Delta arm hardening.
- ~~Surgical single-body undo for `set_body_pose_safe`/camera `change()`~~ DONE 2026-07-10: `Undo<RigidBodySet>::mutate_body_safe` (see [[undo-hashing-hotspots]] item 4) — footprint is body slot + modified-list mark only, islands untouched by kinematic-target/set_position writes.

See [[never-skip-hash-verification]] — this work is how that directive got satisfied in production.
