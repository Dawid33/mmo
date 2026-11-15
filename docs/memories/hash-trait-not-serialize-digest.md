---
name: hash-trait-not-serialize-digest
description: "User measured serialize-then-hash as far slower than Hash impls; that is why the OrderedFloat math forks exist — don't propose bincode digests for state hashing"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 28f83fc4-8486-47dd-a26b-563e6835b48e
  modified: 2026-09-26T11:41:09.928Z
---

State hashing must stay a zero-copy traversal (Hash-style impls). Do not propose hashing serialized (bincode) bytes as the state digest.

**Why:** When architecting the engine, the user had exactly that setup (hash the serialized state) and it was far slower than implementing Hash for everything. That measurement is the reason the nalgebra/simba/approx/ordered-float forks exist (OrderedFloat scalar so nalgebra/parry/rapier types derive Hash). My 2026-09-26 rapier 0.36 spike only compared unbuffered vs buffered bincode digests (5.1 vs 0.83 ms for a region ColliderSet), never against Hash — it does not overturn this.

**How to apply:** In the rapier 0.36 upgrade ([[vendored-deps-update-investigation]]), plan D2 (`StateDigest` over bincode) is rejected. glam f32 types can't get std Hash (orphan rule), so the direction is our own zero-copy `StateHash`-style trait + derive (floats via to_bits, usize as u64 for wasm parity), derived in the rapier/parry forks, keeping cached per-shape digests as custom impls. CONFIRMED by the user 2026-09-27 (plan D2 + Task 4.4 rewritten).
