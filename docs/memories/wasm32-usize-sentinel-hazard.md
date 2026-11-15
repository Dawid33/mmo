---
name: wasm32-usize-sentinel-hazard
description: Serialized usize::MAX sentinels break 64-bit ↔ wasm32 wire/hash parity — the lesson stands, but the rapier sentinel_usize adapter is GONE since the 0.36 switch (upstream ids are u32; StateHash writes usize as u64)
metadata: 
  node_type: memory
  type: project
  originSessionId: 247ddb70-03e4-4c24-b129-4980ae9a5f24
---

**PARTLY SUPERSEDED 2026-09-27 by the rapier 0.36 switch** ([[vendored-deps-update-investigation]]): the `sentinel_usize` serde adapter no longer exists — upstream 0.36's `RigidBodyIds` are `u32` (`INVALID_ISLAND`), and hashing is the `StateHash` trait, which writes `usize` as `u64` (native = wasm32 hash parity pinned by `just determinism-wasm`). The WIRE hazard itself still applies to any new serialized `usize` field holding `usize::MAX`: bincode writes it as u64 and a wasm32 peer can't deserialize it. The paths below are historical.

Found during WebTransport netcode work (2026-07-05, merge d936f0f): bincode encodes `usize` as u64, so a 64-bit peer's `usize::MAX` sentinel arrives as `u64::MAX` and fails to deserialize into wasm32's 32-bit `usize`, silently killing the whole packet (the browser connected but dropped every Region snapshot).

**Why:** the vendored forks guarantee cross-machine determinism, and cross-*architecture* wire/hash parity is part of that mandate; sentinels are the only values that exceed 32 bits in practice.

**How to apply:** the fix pattern is `crates/rapier/src/dynamics/rigid_body_components.rs`'s `pub(crate) mod sentinel_usize` — a `serde(with = ...)` adapter mapping `usize::MAX ⇄ u64::MAX` (64-bit bytes provably unchanged, so native wire + CRC hashes untouched). Applied to `RigidBodyIds::{active_island_id, active_set_id}` and `IndexSequence::first_to_remove` (multibody.rs). If a wasm client ever again logs "failed deserializing packet" with `invalid value: integer 18446744073709551615, expected usize`, hunt the new serialized sentinel field and annotate it the same way. Related: [[wasm-input-gate-suspect]].
