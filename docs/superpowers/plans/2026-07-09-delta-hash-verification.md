# Delta Hash Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make rollback hash verification cost proportional to the data each undo entry actually touches ("delta digests"), so verification can be turned ON in production instead of the current `set_hash_verification(false)` in `crates/client/src/main.rs:90` and `crates/server/src/lib.rs:204`.

**Architecture:** Replace the global on/off verification latch with a three-level `HashMode { Off, Delta, Full }` stamped **per log entry** at log time. `Full` keeps today's exact behavior (crc32 of the whole owning field, used by tests/harnesses). `Delta` hashes only the entry's mutation footprint: the touched map entry (UndoMap), the touched slot + allocator fingerprint (UndoSlotMap), the touched component slot (`Component<T>` helpers), or the physics `StepJournal`'s footprint (the per-tick heavy hitter). Tier-2 closures with an unknowable footprint (e.g. `change()`, `snapshot_raw()`) stay `Full` even in Delta mode — correct, just not cheaper.

**Tech Stack:** Rust; proc-macro codegen in `crates/macros`; vendored forks `crates/slotmapd` and `crates/rapier` (edited in place, never switched to crates.io); `crc32fast` + `std::hash::Hash` for digests (same as existing hashing).

## Global Constraints

- Rollback correctness bar is unchanged: `hash(before) == hash(after undo)`, bit-exact. `Full` mode must retain **exactly** today's checking semantics (whole-field crc32 + the serde-json diff diagnostics on tier-2 mismatch).
- Delta soundness contract: a Delta digest must cover the **entire** mutation footprint of its entry. Any path whose footprint isn't statically known stays `Full`. This is why Delta is safe: tier-1 wrappers only ever mutate the one entry they log.
- Documented Delta-mode gap (accepted trade-off): Delta verifies that each undo **restored its declared footprint exactly**; a write *outside* the declared footprint (e.g. a `StepJournal` capture gap) is only caught by `Full` mode. `Full` stays the default (tests + both client/server harnesses), so the existing capture-gap safety net is not weakened. Production goes from `Off` to `Delta` — strictly more verification than today.
- Never disable verification to win perf (standing user directive). This plan only re-scopes what is hashed.
- No wire-format changes: the rollback log/`Entry` is `#[serde(skip)]`-ed and never serialized; `game::state_hash` (crates/game/src/lib.rs:63) and all state `Hash` impls are untouched.
- Digests never cross machines — they are process-local self-checks. Still use `crc32fast::Hasher` (it implements `std::hash::Hasher`; the macro already does `data.hash(&mut crc32fast::Hasher::new())`).
- The `#[rollback]` macro expands in `crates/game` (`state.rs`); all generated-code changes live in `crates/macros/src/lib.rs`. `crates/game`/`crates/server` stay Bevy-free.
- `just test` (fast loop) after every task; `just test-full` before declaring the plan done. Tag any new test >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`. Keep the justfile in sync when adding skip-listed perf tests.
- Tests that change the global hash mode MUST serialize on a shared `Mutex` and restore `Full` via a drop guard (tests in one binary run on parallel threads; the mode is a process-global).
- Commit after every task with a green suite. Executors work in a worktree (superpowers:using-git-worktrees), never on develop directly.

## Why this is safe to re-enter per entry (background for the executor)

Today `set_hash_verification` is a one-way OFF latch because entries logged while off carry a sentinel `pre_hash: 0`, and re-enabling would compare live hashes against sentinels. Stamping the mode on each `Entry` removes that coupling: rollback verifies every entry under the mode it was logged with, so the mode can change at any time without spurious failures. That deletes the latch and its footgun.

## Current cost map (what each task attacks)

| Site | Today (verification on) | After |
|---|---|---|
| `PhysicsController::on_tick` (crates/game/src/physics.rs:19) | crc32 of the whole `PhysicsState` **every tick** | O(journal footprint) — Task 6 |
| `Component<T>::set_safe/insert_safe/remove_safe` (state.rs) | crc32 of **all** chunks/components per single-key op (O(N²) genesis) | O(1 slot) — Task 5 |
| `emit_on_undo` (no data change at all) | crc32 of the whole field | O(1) — Task 4 |
| `UndoMap` insert/remove/get_mut | crc32 of the whole BTreeMap | O(1 entry) — Task 2 |
| `UndoSlotMap` insert/remove | crc32 of the whole slotmap | O(1 slot + fingerprint) — Task 3 |
| body-insert scopes (`create_mesh`, `create_player_safe`, `build_kinematic_body_safe`) | crc32 of the whole `RigidBodySet` | O(1 body) — Task 5 |
| `change()` / `snapshot_raw()` sites (`attach_*_collider_safe`, `set_body_pose_safe`, camera look in camera.rs) | crc32 of whole field | unchanged (`Full` even in Delta) — documented follow-up; the undo *payload* there is a whole-field clone anyway, so hashing is not the dominant cost |

## File Structure

- `crates/macros/src/lib.rs` — `HashMode`, per-`Entry` mode stamp, all wrapper log sites, rollback verify arms, `undo_digest`, `UndoScopeDigest`.
- `crates/game/src/state.rs` — `component_slot_digest`, `body_slot_digest`, migrated `Component` helpers and body-insert scopes; unit tests for `pub(crate)` digest APIs.
- `crates/game/src/physics.rs` — journal-digest wiring of the physics tick.
- `crates/slotmapd/src/basic.rs` — `alloc_fingerprint` / `alloc_fingerprint_head`.
- `crates/rapier/src/pipeline/step_journal.rs` (+ small paired digest helpers in `island_manager.rs`, `broad_phase_bvh.rs`) — `pre_digest` / `revert_digest`.
- `crates/game/tests/hash_mode.rs` (new) — mode-toggling integration tests + perf evidence.
- `crates/game/tests/hash_restore.rs` — slotmap fingerprint invariants.
- `crates/game/tests/step_journal.rs` — fork digest invariants (reuses the `World` harness there).
- `crates/client/src/main.rs`, `crates/client/src/harness.rs`, `crates/client/src/bevy_harness.rs`, `crates/server/src/lib.rs` — mode call sites.
- `justfile`, `CLAUDE.md` — perf-evidence skip + docs.

---

### Task 1: HashMode infrastructure — per-entry verification stamps

**Files:**
- Modify: `crates/macros/src/lib.rs` (Entry ~line 753, VERIFY_HASHES ~763, set_hash_verification ~770, every `pre_hash` computation, every rollback verify gate)
- Modify: `crates/client/src/harness.rs:44`, `crates/client/src/bevy_harness.rs:40`, `crates/client/src/main.rs:90`, `crates/server/src/lib.rs:204`
- Test: `crates/game/tests/hash_mode.rs` (new)

**Interfaces:**
- Produces: `game::HashMode` (`Off | Delta | Full`, `Copy + PartialEq`), `game::set_hash_mode(HashMode)`, `game::hash_mode() -> HashMode`, `Entry { transaction, undo, pre_hash: u32, verify: HashMode }`. Removes `game::set_hash_verification` and `VERIFY_HASHES`.
- Behavior contract for later tasks: log sites compute `(pre_hash, verify)` from `hash_mode()`; rollback matches on `entry.verify`. In this task Delta logs **fall back to Full** (no delta digests exist yet), so behavior is identical to today for all three modes.

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/hash_mode.rs`:

```rust
//! Per-entry hash-mode behavior. These tests mutate the process-global
//! HASH_MODE, so each takes MODE_LOCK and restores Full on drop.
use std::sync::{Mutex, MutexGuard};

use game::{HashMode, Rollback};

static MODE_LOCK: Mutex<()> = Mutex::new(());

struct ModeGuard(#[allow(dead_code)] MutexGuard<'static, ()>);
impl Drop for ModeGuard {
    fn drop(&mut self) {
        game::set_hash_mode(HashMode::Full);
    }
}

/// Locks out other mode-mutating tests, sets `mode`, restores Full on drop
/// (also on panic, so #[should_panic] tests don't poison later ones).
fn with_mode(mode: HashMode) -> ModeGuard {
    let g = MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    game::set_hash_mode(mode);
    ModeGuard(g)
}

fn new_rollback() -> (
    Rollback,
    crossbeam::channel::Receiver<game::GameDataUpdate>,
) {
    let (send, recv) = crossbeam::channel::unbounded();
    (Rollback::new(Some(send)), recv)
}

#[test]
fn mode_toggles_between_transactions_without_spurious_failures() {
    // The old set_hash_verification was a one-way OFF latch precisely because
    // re-enabling compared live hashes against sentinel 0s. Per-entry stamps
    // must make Off -> Full re-enabling safe.
    let _mode = with_mode(HashMode::Off);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.rollback(); // logged under Off: no check, no panic

    game::set_hash_mode(HashMode::Full);
    let before = game::state_hash(&r);
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.rollback(); // logged under Full: checked, passes
    assert_eq!(before, game::state_hash(&r));
}

#[test]
fn mixed_mode_entries_within_one_transaction_verify_individually() {
    let _mode = with_mode(HashMode::Off);
    let (mut r, _recv) = new_rollback();
    let before = game::state_hash(&r);
    r.new_transaction();
    r.tick.update(|t| *t += 1); // Off entry
    game::set_hash_mode(HashMode::Full);
    r.next_game_event_id.update(|n| *n += 1); // Full entry, same transaction
    r.rollback();
    assert_eq!(before, game::state_hash(&r));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p game --test hash_mode`
Expected: FAIL to compile — `game::HashMode` / `game::set_hash_mode` don't exist yet.

- [ ] **Step 3: Implement in the macro**

In `crates/macros/src/lib.rs`:

3a. Replace the `VERIFY_HASHES` and `set_hash_verification` items (currently pushed around lines 763–787) with:

```rust
items.push(item(
    "HashMode",
    quote! {
        /// How much self-verification a log entry carries, stamped per entry
        /// at log time. Off: none. Delta: digest of only the data the entry's
        /// undo touches. Full: crc32 of the whole owning field (strongest).
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum HashMode { Off = 0, Delta = 1, Full = 2 }
    },
));
items.push(item(
    "HASH_MODE",
    quote! {
        /// Global mode read at LOG time; each entry records the mode it was
        /// logged under, so switching is safe at any point (no latch needed).
        /// Defaults to Full so tests get the strongest checking.
        pub static HASH_MODE: ::std::sync::atomic::AtomicU8 =
            ::std::sync::atomic::AtomicU8::new(2);
    },
));
items.push(item(
    "set_hash_mode",
    quote! {
        /// Sets the verification mode for entries logged from now on.
        pub fn set_hash_mode(mode: HashMode) {
            HASH_MODE.store(mode as u8, ::std::sync::atomic::Ordering::Relaxed);
        }
    },
));
items.push(item(
    "hash_mode",
    quote! {
        pub fn hash_mode() -> HashMode {
            match HASH_MODE.load(::std::sync::atomic::Ordering::Relaxed) {
                0 => HashMode::Off,
                1 => HashMode::Delta,
                _ => HashMode::Full,
            }
        }
    },
));
```

3b. Add `verify` to `Entry`:

```rust
pub struct Entry {
    pub transaction: usize,
    pub undo: UndoOp,
    pub pre_hash: u32,
    pub verify: HashMode,
}
```

3c. At **every** log site, replace the `VERIFY_HASHES`-gated pre_hash with the mode match, and pass `verify` through to the `Entry`. The sites (all in the generated items): `Undo::undo` (~683), `Undo::undo_scope` (~706), `UndoScope` struct + `register` (store `verify: HashMode` computed at `undo_scope()` creation next to `pre_hash`, and push it in `register`), `UndoSlotMap::insert`/`remove` + `log_op` (add a `verify: HashMode` parameter), `UndoMap::log_entry`, `UndoCell::log_old`. The uniform pattern for this task (Delta falls back to Full — later tasks specialize per wrapper):

```rust
let (pre_hash, verify) = match hash_mode() {
    HashMode::Off => (0u32, HashMode::Off),
    _ => (unsafe { self.hash_data() }, HashMode::Full),
};
```

(For `UndoMap::log_entry`/`UndoCell::log_old`, which inline the hasher instead of calling `hash_data`, keep their inline hashing inside the `_ =>` arm.)

3d. In `Rollback::rollback()`, replace every `if VERIFY_HASHES.load(...)` gate with a match on `entry.verify`:
- `HashMode::Off => {}` (just apply the undo),
- `HashMode::Full => { /* exactly today's verify block */ }`,
- `HashMode::Delta => unreachable!("no Delta loggers exist yet — added per-path in later tasks")`.

The tier-2 `previous_data` diagnostic clone becomes `let previous_data = if entry.verify == HashMode::Full { Some(self.#iter_path5.data.clone()) } else { None };`.

3e. Migrate the four call sites:
- `crates/client/src/harness.rs:44` → `game::set_hash_mode(game::HashMode::Full); // enforce hash(before)==hash(after undo) along the driven path`
- `crates/client/src/bevy_harness.rs:40` → same as harness.rs
- `crates/client/src/main.rs:90` → `game::set_hash_mode(game::HashMode::Off);` (Task 7 flips this to Delta)
- `crates/server/src/lib.rs:204` → same as main.rs

- [ ] **Step 4: Run the tests**

Run: `cargo test -p game --test hash_mode` — Expected: PASS.
Run: `just test` — Expected: PASS (behavior identical for all existing tests, which run under the default Full).

- [ ] **Step 5: Commit**

```bash
git add crates/macros/src/lib.rs crates/client/src crates/server/src/lib.rs crates/game/tests/hash_mode.rs
git commit -m "feat(rollback): per-entry HashMode stamps replace the one-way verification latch"
```

---

### Task 2: UndoMap — per-entry delta digests

**Files:**
- Modify: `crates/macros/src/lib.rs` (`UndoMap::log_entry` ~965; map arm of `Rollback::rollback` ~1227)
- Test: `crates/game/tests/hash_mode.rs`

**Interfaces:**
- Consumes: `HashMode`, `Entry.verify` from Task 1.
- Produces: Delta digest convention for map entries — `crc32(Hash(key) . Hash(pre-mutation Option<value-at-key>))`. Note `Option<&V>` and `Option<V>` produce identical `Hash` streams (`&V` delegates to `V`), which the verify side relies on to avoid cloning.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/hash_mode.rs`:

```rust
#[test]
fn undomap_ops_roll_back_bit_exact_under_delta() {
    let _mode = with_mode(HashMode::Delta);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.clients.insert(1, game::Client::default());
    let h1 = game::state_hash(&r);

    r.new_transaction();
    r.clients.get_mut(&1).unwrap().fps_cam_mode = true; // get_mut delta
    r.clients.insert(2, game::Client::default());        // fresh insert (prev = None)
    r.clients.insert(1, game::Client::default());        // overwrite (prev = Some)
    r.clients.remove(&1);                                 // remove
    r.player_entites.insert(9, game::EntityKey::default());
    r.rollback();
    assert_eq!(h1, game::state_hash(&r), "delta-verified map undos must be bit-exact");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p game --test hash_mode undomap_ops`
Expected: FAIL — panics at the `unreachable!("no Delta loggers exist yet ...")` arm... it will actually fail earlier: `log_entry` still stamps Full under Delta mode, so the test PASSES vacuously. To make it a real failing test first, implement Step 3a (the log side) before 3b (the verify side); between the two, the test panics at the `unreachable!` Delta arm — that's the meaningful failure. Verify that sequencing.

- [ ] **Step 3a: Implement the log side**

Replace `UndoMap::log_entry`'s body:

```rust
fn log_entry(&mut self, key: K, prev: ::std::option::Option<V>) {
    let mut global = self.global_log.lock().unwrap();
    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
    let (pre_hash, verify) = match hash_mode() {
        HashMode::Off => (0u32, HashMode::Off),
        HashMode::Full => {
            let mut hasher = ::crc32fast::Hasher::new();
            ::std::hash::Hash::hash(&self.data, &mut hasher);
            (hasher.finalize(), HashMode::Full)
        }
        HashMode::Delta => {
            // Footprint of a single-key op: the key and its pre-mutation
            // value. `prev` was read from the map before mutating, so it IS
            // the pre-state at that key.
            let mut hasher = ::crc32fast::Hasher::new();
            ::std::hash::Hash::hash(&key, &mut hasher);
            ::std::hash::Hash::hash(&prev, &mut hasher);
            (hasher.finalize(), HashMode::Delta)
        }
    };
    let make = self.make.expect("UndoMap not wired to a Delta variant");
    global.log.push_back(Entry { transaction: trans, undo: UndoOp::Typed(make(key, prev)), pre_hash, verify });
}
```

- [ ] **Step 3b: Implement the verify side**

Replace the map arm in `Rollback::rollback()`:

```rust
#(Delta::#map_log_ident2(key, prev) => {
    let vkey = key.clone();
    match prev {
        Some(v) => { self.#map_path1.data.insert(key, v); }
        None => { self.#map_path1.data.remove(&key); }
    }
    match entry.verify {
        HashMode::Off => {}
        HashMode::Full => {
            let mut hasher = ::crc32fast::Hasher::new();
            ::std::hash::Hash::hash(&self.#map_path1.data, &mut hasher);
            let new_hash = hasher.finalize();
            if new_hash != entry.pre_hash {
                panic!("Hash verification failed for typed undo of self.{} in transaction {:?}: {:?} != {:?}", #map_path_string1, entry.transaction, new_hash, entry.pre_hash);
            }
        }
        HashMode::Delta => {
            // Re-derive the entry digest from LIVE post-undo data; matches
            // the log side because Option<&V> and Option<V> hash identically.
            let mut hasher = ::crc32fast::Hasher::new();
            ::std::hash::Hash::hash(&vkey, &mut hasher);
            ::std::hash::Hash::hash(&self.#map_path1.data.get(&vkey), &mut hasher);
            let new_hash = hasher.finalize();
            if new_hash != entry.pre_hash {
                panic!("Hash verification failed (delta) for typed undo of self.{} in transaction {:?}: {:?} != {:?}", #map_path_string1, entry.transaction, new_hash, entry.pre_hash);
            }
        }
    }
})*
```

Also update `UndoCell::log_old` now: a cell's footprint is the whole value, so Full and Delta coincide — under Delta, stamp `HashMode::Full` with the existing whole-cell hash and add a comment saying so. The cell rollback arm then needs no Delta case (it can keep `unreachable!` for Delta, with a comment pointing at log_old).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p game --test hash_mode` — Expected: PASS.
Run: `just test` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/macros/src/lib.rs crates/game/tests/hash_mode.rs
git commit -m "feat(rollback): UndoMap delta digests — O(entry) verification instead of O(map)"
```

---

### Task 3: UndoSlotMap — slot + allocator-fingerprint digests

**Files:**
- Modify: `crates/slotmapd/src/basic.rs` (near `revert_insert`, ~line 507)
- Modify: `crates/macros/src/lib.rs` (`UndoSlotMap::insert`/`remove` ~878, slotmap arm of rollback ~1241)
- Test: `crates/game/tests/hash_restore.rs`, `crates/game/tests/hash_mode.rs`

**Interfaces:**
- Consumes: Task 1 infra.
- Produces: `slotmapd::SlotMap::alloc_fingerprint(key: K) -> (u32, u32, u32, u32)` and `alloc_fingerprint_head() -> (u32, u32, u32, u32)` — `(free_head, num_elems, slot_version, slot_next_free)`. This is exactly the allocator state a single insert/remove mutates besides the value; hashing it preserves the protection the whole-map hash gave against free-list/version drift (the `hash_restore.rs` hazard: `remove(insert(x))` is NOT an inverse).

- [ ] **Step 1: Write the failing fork tests**

Append to `crates/game/tests/hash_restore.rs` (it already exercises `slotmapd` directly — mirror its imports):

```rust
#[test]
fn alloc_fingerprint_head_roundtrips_revert_insert() {
    let mut sm: slotmapd::SlotMap<slotmapd::DefaultKey, u32> = slotmapd::SlotMap::new();
    // Free-list-reuse path: leave a vacant slot behind.
    let a = sm.insert(1);
    sm.remove(a);
    let pre = sm.alloc_fingerprint_head();
    let k = sm.insert(2);
    sm.revert_insert(k);
    assert_eq!(pre, sm.alloc_fingerprint_head(), "reused-slot insert revert must restore the fingerprint");
    // Fresh-slot (vec growth) path.
    let mut sm2: slotmapd::SlotMap<slotmapd::DefaultKey, u32> = slotmapd::SlotMap::new();
    let pre2 = sm2.alloc_fingerprint_head();
    let k2 = sm2.insert(7);
    sm2.revert_insert(k2);
    assert_eq!(pre2, sm2.alloc_fingerprint_head());
}

#[test]
fn alloc_fingerprint_detects_non_lifo_remove_insert() {
    // The known hazard: a naive "undo remove by re-inserting" bumps the slot
    // version and moves the free head. The whole-map hash caught this; the
    // fingerprint must too.
    let mut sm: slotmapd::SlotMap<slotmapd::DefaultKey, u32> = slotmapd::SlotMap::new();
    let a = sm.insert(1);
    let pre = sm.alloc_fingerprint(a);
    let v = sm.remove(a).unwrap();
    let b = sm.insert(v); // wrong inverse: same value, different slot version
    assert_ne!(pre, sm.alloc_fingerprint(b), "version drift must change the fingerprint");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p game --test hash_restore alloc_fingerprint`
Expected: FAIL to compile — the methods don't exist.

- [ ] **Step 3: Implement the fork accessors**

In `crates/slotmapd/src/basic.rs`, in the main `impl<K: Key, V> SlotMap<K, V>` block (near `revert_insert`, line ~507), using the file's existing `Slot::get()` / `Occupied` / `Vacant` idiom (see the `Slot` Debug impl at ~130 for the pattern) and `KeyData::idx` (used at ~455):

```rust
/// Allocator-state fingerprint of the slot at `idx`, plus the free-list
/// head and element count — exactly the allocator state a single
/// insert/remove touching that slot mutates. The rollback layer hashes
/// this instead of the whole map (delta verification).
fn fingerprint_at(&self, idx: u32) -> (u32, u32, u32, u32) {
    let (version, next_free) = match self.slots.get(idx as usize) {
        Some(slot) => match slot.get() {
            Occupied(_) => (slot.version, 0),
            Vacant(&next_free) => (slot.version, next_free),
        },
        None => (u32::MAX, u32::MAX), // fresh (not yet allocated) slot index
    };
    (self.free_head, self.num_elems, version, next_free)
}

/// Fingerprint of the slot addressed by `key`. See [`fingerprint_at`].
pub fn alloc_fingerprint(&self, key: K) -> (u32, u32, u32, u32) {
    self.fingerprint_at(key.data().idx)
}

/// Fingerprint of the slot the NEXT `insert` will occupy. After a correct
/// `revert_insert(k)`, `free_head` points back at `k`'s slot, so this
/// reads the same slot before an insert and after its exact revert.
pub fn alloc_fingerprint_head(&self) -> (u32, u32, u32, u32) {
    self.fingerprint_at(self.free_head)
}
```

(If `Occupied`/`Vacant` need importing in that scope, mirror how `Slot::get()` call sites in the same file bring them in.)

- [ ] **Step 4: Run the fork tests**

Run: `cargo test -p game --test hash_restore alloc_fingerprint` — Expected: PASS.

- [ ] **Step 5: Write the failing wrapper test**

Append to `crates/game/tests/hash_mode.rs`:

```rust
#[test]
fn entities_slotmap_rolls_back_bit_exact_under_delta() {
    let _mode = with_mode(HashMode::Delta);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    let a = r.ecs.entities.insert(());
    let h1 = game::state_hash(&r);

    r.new_transaction();
    let _b = r.ecs.entities.insert(()); // fresh-slot or reuse path
    r.ecs.entities.remove(a);           // remove path
    r.rollback();
    assert_eq!(h1, game::state_hash(&r), "delta-verified slotmap undos must be bit-exact");
}
```

Run: `cargo test -p game --test hash_mode entities_slotmap` — Expected: panic at the slotmap `unreachable!` Delta arm once the log side lands; implement log side first as in Task 2.

- [ ] **Step 6: Implement log + verify sides in the macro**

`UndoSlotMap::insert` (and thread `verify` through `log_op`):

```rust
pub fn insert(&mut self, v: V) -> K {
    let (pre_hash, verify) = match hash_mode() {
        HashMode::Off => (0u32, HashMode::Off),
        HashMode::Full => (unsafe { self.hash_data() }, HashMode::Full),
        HashMode::Delta => {
            // Pre-state footprint of an insert: the allocator state and the
            // (absent) value at the slot the insert will occupy.
            let mut hasher = ::crc32fast::Hasher::new();
            ::std::hash::Hash::hash(&self.data.alloc_fingerprint_head(), &mut hasher);
            ::std::hash::Hash::hash(&::std::option::Option::<&V>::None, &mut hasher);
            (hasher.finalize(), HashMode::Delta)
        }
    };
    let k = self.data.insert(v);
    let emit = self.emit_insert.map(|mk| mk(k));
    self.log_op(SlotOp::Inserted(k), pre_hash, verify, emit);
    k
}
```

`UndoSlotMap::remove`:

```rust
pub fn remove(&mut self, k: K) -> ::std::option::Option<V> {
    let (pre_hash, verify) = match hash_mode() {
        HashMode::Off => (0u32, HashMode::Off),
        HashMode::Full => (unsafe { self.hash_data() }, HashMode::Full),
        HashMode::Delta => {
            let mut hasher = ::crc32fast::Hasher::new();
            ::std::hash::Hash::hash(&self.data.alloc_fingerprint(k), &mut hasher);
            ::std::hash::Hash::hash(&self.data.get(k), &mut hasher);
            (hasher.finalize(), HashMode::Delta)
        }
    };
    let v = self.data.remove(k)?;
    let emit = self.emit_remove.map(|mk| mk(k));
    self.log_op(SlotOp::Removed(k, v.clone()), pre_hash, verify, emit);
    Some(v)
}
```

Rollback slotmap arm — after `revert_insert(k)` verify with `alloc_fingerprint_head()` + `get(k)` (free_head points back at k's slot, and `get(k)` is `None`, matching the log side's `Option::<&V>::None`); after `revert_remove(k, v)` verify with `alloc_fingerprint(k)` + `get(k)` (now `Some(&restored)`). Both wrapped in the same `match entry.verify` shape as Task 2, with the same panic message format (`"Hash verification failed (delta) for typed undo of self.{} ..."`). Keep the existing emit sends exactly where they are.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p game --test hash_mode && just test` — Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/slotmapd/src/basic.rs crates/macros/src/lib.rs crates/game/tests/hash_restore.rs crates/game/tests/hash_mode.rs
git commit -m "feat(rollback): UndoSlotMap delta digests with allocator fingerprints"
```

---

### Task 4: Tier-2 self-digesting undo closures

**Files:**
- Modify: `crates/macros/src/lib.rs` (`FieldUndo` ~741, `Undo::undo`/`undo_scope`/`emit_on_undo` ~672–713, `UndoScope` ~606–634, per-struct items loop ~343, opaque rollback arms ~1187–1214)
- Test: `crates/game/src/state.rs` (new `#[cfg(test)]` module — the APIs are `pub(crate)`)

**Interfaces:**
- Consumes: Task 1 infra.
- Produces:
  - `FieldUndo` / root closures now return `u32` (the post-undo footprint digest; ignored for Full/Off entries).
  - `Undo<T>::undo_digest(pre: u32, undo: impl FnOnce(&mut T, &Sender<GameDataUpdate>) -> u32 + Send + 'static)` — `pub(crate)`. Contract: `pre` hashes exactly the data `undo` touches, as it is *before* the mutation being logged; `undo` returns the same digest recomputed from **live** data after it runs. Under Full the digest is ignored (whole-field hash, today's semantics); under Off nothing is checked.
  - `Undo<T>::undo_scope_digest() -> UndoScopeDigest<T, '_>` with `Deref`/`DerefMut`/`Drop` mirroring `UndoScope`, per-module-struct `raw_fields()`, plus `wants_delta_digest(&self) -> bool` (true iff created under Delta — lets callers skip computing an expensive pre-digest otherwise) and `register(pre: u32, undo: impl FnOnce(&mut T, &Sender) -> u32 + Send + 'static)`.
  - `emit_on_undo` becomes free under Delta (digest 0 vs 0 — it mutates nothing).
- Existing `undo()` / `undo_scope()` keep their closure signatures by wrapping (`|d, s| { f(d, s); 0u32 }`) and stay Full-verified even in Delta mode (unknown footprint).

- [ ] **Step 1: Write the failing unit tests**

In `crates/game/src/state.rs`, add alongside `inventory_tests`:

```rust
#[cfg(test)]
mod delta_digest_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static MODE_LOCK: Mutex<()> = Mutex::new(());
    struct ModeGuard(#[allow(dead_code)] MutexGuard<'static, ()>);
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            set_hash_mode(HashMode::Full);
        }
    }
    fn delta_mode() -> ModeGuard {
        let g = MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_hash_mode(HashMode::Delta);
        ModeGuard(g)
    }
    fn rb() -> (Rollback, crossbeam::channel::Receiver<GameDataUpdate>) {
        let (send, recv) = crossbeam::channel::unbounded();
        (Rollback::new(Some(send)), recv)
    }

    #[test]
    fn tier2_digest_roundtrip_passes() {
        let _m = delta_mode();
        let (mut r, _recv) = rb();
        r.new_transaction();
        r.ecs.camera.undo_digest(42, |_d, _s| 42);
        r.rollback(); // matching digest: no panic
    }

    #[test]
    #[should_panic(expected = "Hash verification failed (delta)")]
    fn tier2_digest_mismatch_panics() {
        let _m = delta_mode();
        let (mut r, _recv) = rb();
        r.new_transaction();
        r.ecs.camera.undo_digest(42, |_d, _s| 43);
        r.rollback();
    }

    #[test]
    fn plain_undo_closures_stay_full_verified_under_delta() {
        // Unknown-footprint tier-2 must not silently lose checking: change()
        // logs a whole-field snapshot and is still whole-field verified.
        let _m = delta_mode();
        let (mut r, _recv) = rb();
        let before = crate::state_hash(&r);
        r.new_transaction();
        r.tick.update(|t| *t += 1); // cell — stamps Full per Task 2
        r.ecs.camera.change();      // tier-2 snapshot
        r.rollback();
        assert_eq!(before, crate::state_hash(&r));
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p game --lib delta_digest` (if `--lib` doesn't match how unit tests run here, `cargo test -p game delta_digest`)
Expected: FAIL to compile — `undo_digest` doesn't exist.

- [ ] **Step 3: Implement in the macro**

3a. `FieldUndo` and root variant closures become `Box<dyn FnOnce(&mut T, &Sender<GameDataUpdate>) -> u32 + Send>`; same for the `wrap` fn-pointer field type on `Undo<T>` and `UndoScope`'s registration path.

3b. `Undo::undo` wraps: `wrap(Box::new(move |d, s| { undo(d, s); 0u32 }))`, `(pre_hash, verify)` = Off→`(0, Off)`, else→`(full hash, Full)` with a comment: *footprint unknown for a plain closure — Delta mode still pays the whole-field hash here on purpose*.

3c. Add `undo_digest`:

```rust
pub(crate) fn undo_digest(
    &mut self,
    pre: u32,
    undo: impl FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) -> u32 + 'static + Send,
) {
    let mut global = self.global_log.lock().unwrap();
    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
    let (pre_hash, verify) = match hash_mode() {
        HashMode::Off => (0u32, HashMode::Off),
        HashMode::Full => (unsafe { self.hash_data() }, HashMode::Full),
        HashMode::Delta => (pre, HashMode::Delta),
    };
    let wrap = self.wrap.expect("Undo field not wired to a FieldUndo variant");
    global.log.push_back(Entry { transaction: trans, undo: UndoOp::Opaque(wrap(Box::new(undo))), pre_hash, verify });
}
```

3d. `emit_on_undo` rides it: `self.undo_digest(0u32, move |_, s| { let _ = s.send(event); 0u32 });` (keep the disconnected-receiver comment).

3e. `UndoScopeDigest` struct + `undo_scope_digest()` + `wants_delta_digest()`:

```rust
pub struct UndoScopeDigest<T, 'a> where T: /* same bounds as UndoScope */ {
    verify: HashMode,
    pre_hash: u32, // full-field hash iff verify == Full
    touched: bool,
    registered: bool,
    value: &'a mut Undo<T>,
}
```

`undo_scope_digest()` stamps `verify = hash_mode()` and computes the full-field `pre_hash` only when `Full`. `register(pre, undo)` pushes `(pre_hash=self.pre_hash, verify=Full)` / `(pre, Delta)` / `(0, Off)` per the stamped mode. `wants_delta_digest()` returns `self.verify == HashMode::Delta`. Mirror `UndoScope`'s `Deref`/`DerefMut`/`Drop` (touched/registered debug_assert) and add an `UndoScopeDigest` `raw_fields()` impl in the per-struct items loop next to the `UndoScope` one.

3f. Rollback opaque arms (root + per-field): match `entry.verify` — Off: run closure, ignore return; Delta: `let post = func(...); if post != entry.pre_hash { panic!("Hash verification failed (delta) for self.{} in transaction {:?}: {:?} != {:?}", #iter_path_string1, entry.transaction, post, entry.pre_hash); }`; Full: exactly today's block (clone `previous_data`, run, whole-field hash, json-diff on mismatch), with `let _ = func(...)`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p game delta_digest && just test` — Expected: PASS. (`cargo test -p client` too — the client harnesses compile against the changed macro surface: `just test`.)

- [ ] **Step 5: Commit**

```bash
git add crates/macros/src/lib.rs crates/game/src/state.rs
git commit -m "feat(rollback): tier-2 self-digesting undo closures (undo_digest, UndoScopeDigest)"
```

---

### Task 5: Component helpers + body-insert scopes go delta

**Files:**
- Modify: `crates/game/src/state.rs` (`Component` helpers ~249–281; body-insert scopes in `create_mesh` ~359, `create_player_safe` ~563, `build_kinematic_body_safe` ~701)
- Test: `crates/game/tests/hash_mode.rs`

**Interfaces:**
- Consumes: `undo_digest`, `undo_scope_digest` from Task 4.
- Produces: `fn component_slot_digest<T: Hash>(key: EntityKey, slot: Option<&Option<T>>) -> u32` and `fn body_slot_digest(alloc: (Option<u32>, u32), body: Option<&rapier3d::prelude::RigidBody>) -> u32` — private helpers in `state.rs`. `RigidBodySet::alloc_state()` already exists in the fork (`crates/rapier/src/dynamics/rigid_body_set.rs:180`).

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/hash_mode.rs`:

```rust
#[test]
fn entity_lifecycle_rolls_back_bit_exact_under_delta() {
    let _mode = with_mode(HashMode::Delta);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));
    let h1 = game::state_hash(&r);

    r.new_transaction();
    r.create_player_safe(7); // exercises insert_safe/set_safe, slotmap emits,
                             // body-insert scope, capsule snapshot (Full), emits
    r.rollback();
    assert_eq!(h1, game::state_hash(&r), "player create must invert bit-exact under Delta");
}
```

- [ ] **Step 2: Run it — it should already PASS (everything still stamps Full/Delta correctly). Its role is a regression net for this task's refactor; confirm it passes BEFORE changing state.rs, then keep running it after each edit below.**

Run: `cargo test -p game --test hash_mode entity_lifecycle` — Expected: PASS (pre-refactor baseline).

- [ ] **Step 3: Implement the digests in state.rs**

Helpers (near the `Component` impls):

```rust
/// crc32 of one component slot — the exact footprint of the single-key
/// insert_safe / set_safe / remove_safe mutations.
fn component_slot_digest<T: std::hash::Hash>(key: EntityKey, slot: Option<&Option<T>>) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    key.hash(&mut h);
    slot.hash(&mut h);
    h.finalize()
}

/// crc32 of one rigid-body slot + the set's allocator state — the exact
/// footprint of a journaled body insert (reverted via revert_insert).
fn body_slot_digest(
    alloc: (Option<u32>, u32),
    body: Option<&rapier3d::prelude::RigidBody>,
) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    alloc.hash(&mut h);
    body.hash(&mut h);
    h.finalize()
}
```

`Component` helpers (the SparseSecondaryMap ops are hash-exact single-key inverses — see the existing comments citing hash_restore.rs):

```rust
pub fn insert_safe(&mut self, key: EntityKey) {
    let pre = component_slot_digest::<T>(key, self.list.get(key)); // absent -> None
    self.undo_digest(pre, move |d, _| {
        d.list.remove(key);
        component_slot_digest(key, d.list.get(key))
    });
    self.raw_mut().list.insert(key, None);
}

pub fn set_safe(&mut self, key: EntityKey, item: Option<T>) {
    let old = self.list.get(key).cloned().unwrap();
    let pre = component_slot_digest(key, Some(&old));
    self.undo_digest(pre, move |d, _| {
        *d.list.get_mut(key).unwrap() = old.clone();
        component_slot_digest(key, d.list.get(key))
    });
    if let Some(item) = item {
        self.raw_mut().list.get_mut(key).unwrap().replace(item);
    } else {
        self.raw_mut().list.get_mut(key).unwrap().take();
    }
}

pub fn remove_safe(&mut self, key: EntityKey) {
    let old = self.list.get(key).cloned().unwrap();
    let pre = component_slot_digest(key, Some(&old));
    self.undo_digest(pre, move |d, _| {
        d.list.insert(key, old.clone());
        component_slot_digest(key, d.list.get(key))
    });
    self.raw_mut().list.remove(key);
}
```

(Keep every existing doc comment on these three — the LIFO/slot-leak rationale still applies verbatim.)

Body-insert scopes — the same three-line pattern at all three sites (`create_mesh`, `create_player_safe`, `build_kinematic_body_safe`); shown for `create_mesh`:

```rust
// Tier-2 with a true fork inverse + delta digest: reverts the arena
// allocator exactly; verification hashes just this slot + alloc state.
let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
let mut scope = self.data.physics.bodies.undo_scope_digest();
let handle = scope.insert(body);
scope.register(
    body_slot_digest((prev_head, prev_len), None),
    move |bodies, _| {
        bodies.revert_insert(handle, prev_head, prev_len);
        body_slot_digest(bodies.alloc_state(), bodies.get(handle))
    },
);
```

Leave `attach_voxels_collider_safe` / `attach_capsule_collider_safe` / `set_body_pose_safe` / camera.rs `change()` sites untouched — they stay Full-verified whole-field snapshots (their undo payload is a whole-field clone anyway; surgical single-body ops are a separate follow-up because `set_position` also touches islands/modified-lists state).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p game --test hash_mode && just test` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/state.rs crates/game/tests/hash_mode.rs
git commit -m "feat(game): per-slot delta digests for Component helpers and body-insert scopes"
```

---

### Task 6: Physics step — StepJournal footprint digests

**Files:**
- Modify: `crates/rapier/src/pipeline/step_journal.rs` (+ paired digest helpers in `crates/rapier/src/dynamics/island_manager.rs` and `crates/rapier/src/geometry/broad_phase_bvh.rs` if `IslandsSaved`/`BroadSaved` aren't plain wholesale copies)
- Modify: `crates/game/src/physics.rs:13-49`
- Test: `crates/game/tests/step_journal.rs`, `crates/game/tests/hash_mode.rs`

**Interfaces:**
- Consumes: `undo_scope_digest` / `wants_delta_digest` / `register(pre, undo)` from Task 4.
- Produces:
  - `StepJournal::pre_digest(&self) -> u32` — digest of the **pre-tick values** of everything the journal captured, section by section in a fixed order.
  - `StepJournal::revert_digest(self, islands, broad, narrow, bodies, colliders, ij, mj) -> u32` — performs exactly `revert()`, then recomputes the same digest from the **live** structures over the same footprint. Equal to `pre_digest()` iff the revert restored the footprint exactly.
- **Digest definition (both sides MUST hash identical logical content in identical order):**
  1. Bodies: handles sorted ascending; per handle hash `(handle, Some(&saved_body))` on the pre side vs `(handle, bodies.get(handle))` on the live side.
  2. Colliders: same shape.
  3. Islands: pre side hashes the `IslandsSaved` payload; live side hashes the same fields read from the live `IslandManager` in the same order. `journal_restore` copies the saved fields wholesale, so implement a paired `IslandsSaved::digest()` / `IslandManager::journal_digest()` hashing exactly the field list `journal_restore` writes, in declaration order. Read `island_manager.rs` and mirror `journal_restore`'s field list — the two digest fns and `journal_restore` must name the same fields (add a comment on each pointing at the others).
  4. Lists (`ListsSaved`): pre side hashes the five saved values; live side hashes the live equivalents (`bodies.modified` via the same accessors `set_modified` writes, colliders' modified + removed, both `to_wake_up` sets — for the HashSets, hash a sorted `Vec` of their contents, both sides, since set iteration order isn't canonical).
  5. Joints: only when `self.joints.is_some()`: pre hashes the saved `(ImpulseJointSet, MultibodyJointSet)`; live hashes the live sets (same types — trivially parallel).
  6. Narrow phase: **only** when `narrow_wholesale.is_some()`: pre hashes the saved snapshot; live hashes the live `NarrowPhase` (same type). The fine-grained `narrow` op log is **excluded from both sides** — its payloads are verified by Full mode and the fork's own step_journal tests, not by Delta digests. Hash a `bool` (wholesale-captured) into both sides so presence/absence itself is checked.
  7. Broad phase: only when `self.broad.is_some()`: pre hashes `BroadSaved`, live hashes the live `BroadPhaseBvh` — paired digest fns mirroring `journal_restore`/`journal_save`, same approach as islands. Hash the `bool` presence flag on both sides too.
- All hashing via `crc32fast::Hasher` (implements `std::hash::Hasher`; every listed type is already `Hash` — `PhysicsState` derives `Hash` through the whole tree).

- [ ] **Step 1: Write the failing fork tests**

Append to `crates/game/tests/step_journal.rs` (reuse its `World` harness, `add_floor`, `h`):

```rust
#[test]
fn journal_digest_matches_after_exact_revert() {
    let mut w = World::new();
    for x in 0..4 { for z in 0..4 { add_floor(&mut w, x, z); } }
    let ball = w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(2.0), Real::from(8.0), Real::from(2.0)])
            .build(),
    );
    w.colliders.insert_with_parent(
        ColliderBuilder::ball(Real::from(0.5)).build(), ball, &mut w.bodies,
    );
    for _ in 0..3 { let _ = w.step(); }
    // Fall, impact, settle: dirty ticks of several shapes.
    for tick in 0..60 {
        let j = w.step();
        let pre = j.pre_digest();
        let live = j.revert_digest(
            &mut w.islands, &mut w.broad, &mut w.narrow,
            &mut w.bodies, &mut w.colliders, &mut w.ij, &mut w.mj,
        );
        assert_eq!(pre, live, "tick {tick}: footprint digest after exact revert must match");
        let _ = w.step(); // advance again (state was restored)
    }
}

#[test]
fn journal_digest_matches_on_collider_removal_tick() {
    // Wholesale narrow + dirty broad path (mirrors collider_removal_tick_revert_is_hash_exact).
    let mut w = World::new();
    add_floor(&mut w, 0, 0);
    let tile_body = w.bodies.insert(
        RigidBodyBuilder::fixed()
            .translation(vector![Real::from(40.0), Real::from(-1.0), Real::from(2.0)])
            .build(),
    );
    let tile = w.colliders.insert_with_parent(
        ColliderBuilder::cuboid(Real::from(16.0), Real::from(1.0), Real::from(16.0)).build(),
        tile_body, &mut w.bodies,
    );
    let ball = w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(40.0), Real::from(1.0), Real::from(2.0)])
            .build(),
    );
    w.colliders.insert_with_parent(
        ColliderBuilder::ball(Real::from(0.5)).build(), ball, &mut w.bodies,
    );
    for _ in 0..90 { let _ = w.step(); }
    w.colliders.remove(tile, &mut w.islands, &mut w.bodies, true);
    let j = w.step();
    assert!(j.broad_captured());
    let pre = j.pre_digest();
    let live = j.revert_digest(
        &mut w.islands, &mut w.broad, &mut w.narrow,
        &mut w.bodies, &mut w.colliders, &mut w.ij, &mut w.mj,
    );
    assert_eq!(pre, live, "removal-tick digest must match after exact revert");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p game --test step_journal journal_digest`
Expected: FAIL to compile — `pre_digest`/`revert_digest` don't exist.

- [ ] **Step 3: Implement in the rapier fork**

In `step_journal.rs`, factor the digest into one private fn parameterized by a "value source" so pre and live sides cannot drift:

```rust
impl StepJournal {
    fn digest_bodies(h: &mut crc32fast::Hasher, items: impl Iterator<Item = (RigidBodyHandle, Option<&RigidBody>)>) { /* hash (handle, body) pairs */ }
    // ...same helper shape per section...

    pub fn pre_digest(&self) -> u32 {
        // sorted saved handles -> saved payloads; sections 3-7 from the
        // saved snapshots; presence flags for joints/narrow/broad.
    }

    pub fn revert_digest(self, /* same params as revert */) -> u32 {
        // Collect the handle lists + presence flags BEFORE consuming self,
        // run the exact body of revert(), then recompute the digest from
        // the live structures over that footprint.
    }
}
```

Implementation notes (bind them in code comments):
- `revert_digest` must not duplicate `revert`'s logic: restructure `revert` into a private `fn revert_inner(self, ...) -> Footprint` that returns the retained handle lists/flags, with `revert()` delegating (`let _ = self.revert_inner(...)`) and `revert_digest()` digesting after it.
- Sort body/collider handles ascending on BOTH sides (saved order is first-touch order — not canonical).
- HashSets (`to_wake_up`, `saved_*_set`) are hashed as sorted Vecs on both sides.
- The paired islands/broad digest fns live next to `journal_restore`/`journal_save` in their own files with cross-referencing comments.

- [ ] **Step 4: Run the fork tests**

Run: `cargo test -p game --test step_journal` — Expected: ALL PASS (including the pre-existing revert exactness suite — proves `revert_inner` refactor didn't change behavior).

- [ ] **Step 5: Wire the game tick + write its test**

`crates/game/src/physics.rs` `on_tick` becomes:

```rust
let mut journal = StepJournal::default();
let mut scope = data.physics.undo_scope_digest();
{
    let p = scope.raw_fields();
    self.pipeline.step_journaled(
        &*p.gravity, &*p.integration_parameters, p.islands, p.broad_phase,
        p.narrow_phase, p.bodies, p.colliders, p.implules_joint_set,
        p.multi_body_joint_set, p.ccd_solver, &(), &(), &mut journal,
    );
}
// Pre-digest only when Delta verification will read it (it walks the
// journal's saved payloads).
let pre = if scope.wants_delta_digest() { journal.pre_digest() } else { 0 };
scope.register(pre, move |phys, _| {
    let p = phys.raw_undo_parts();
    journal.revert_digest(
        p.islands, p.broad_phase, p.narrow_phase, p.bodies, p.colliders,
        p.implules_joint_set, p.multi_body_joint_set,
    )
});
```

Append to `crates/game/tests/hash_mode.rs`:

```rust
#[test]
fn physics_ticks_roll_back_bit_exact_under_delta() {
    let _mode = with_mode(HashMode::Delta);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));
    r.create_player_safe(7);
    let h1 = game::state_hash(&r);

    let mut physics = game::PhysicsController::new();
    for _ in 0..5 {
        r.new_transaction();
        physics.on_tick(&mut r.data);
    }
    for _ in 0..5 {
        r.rollback();
    }
    assert_eq!(h1, game::state_hash(&r), "journal-digest-verified ticks must invert bit-exact");
}
```

(Import shape: `PhysicsController::new()` returns `Box<dyn Controller>`; check `game`'s root re-exports and mirror how `crates/game/src/region.rs` constructs its controller list if the paths differ.)

- [ ] **Step 6: Run everything**

Run: `cargo test -p game --test hash_mode && just test` — Expected: PASS. The Full-mode client harnesses (`cargo test -p client` inside `just test`) are the capture-gap net — they must stay green.

- [ ] **Step 7: Commit**

```bash
git add crates/rapier/src crates/game/src/physics.rs crates/game/tests
git commit -m "feat(physics): StepJournal footprint digests — per-tick delta verification"
```

---

### Task 7: Turn verification ON in production + perf evidence + docs

**Files:**
- Modify: `crates/client/src/main.rs:90`, `crates/server/src/lib.rs:204`
- Modify: `justfile` (test-full game line, ~179), `CLAUDE.md` (macros bullet)
- Test: `crates/game/tests/hash_mode.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: production runs `HashMode::Delta`; harnesses/tests stay `Full`; printed perf evidence comparing Off/Delta/Full.

- [ ] **Step 1: Add the perf-evidence test**

Append to `crates/game/tests/hash_mode.rs` (evidence, not a CI gate — same convention as `perf_evidence_journal_scales_with_activity` in step_journal.rs):

```rust
/// Perf evidence (not a CI gate — run with
/// `cargo test -p game --test hash_mode -- --ignored --nocapture perf_evidence`).
/// Times 100 physics ticks + full rollback under each mode. Expectation:
/// delta ~ off (within noise), full markedly slower as the world grows.
#[test]
#[ignore = "perf evidence, prints numbers; run with --ignored --nocapture"]
fn perf_evidence_hash_modes() {
    fn run(mode: HashMode) -> (std::time::Duration, std::time::Duration) {
        let _m = with_mode(mode);
        let (mut r, _recv) = new_rollback();
        r.new_transaction();
        for x in 0..4 {
            for z in 0..4 {
                r.create_mesh(game::ChunkCoords::new(x, 0, z), game::Chunk::flat_floor(8));
            }
        }
        r.create_player_safe(7);
        let mut physics = game::PhysicsController::new();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            r.new_transaction();
            physics.on_tick(&mut r.data);
        }
        let ticks = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            r.rollback();
        }
        (ticks, t.elapsed())
    }
    for mode in [HashMode::Off, HashMode::Delta, HashMode::Full] {
        let (ticks, rollbacks) = run(mode);
        println!("{mode:?}: 100 ticks = {ticks:?}, 100 rollbacks = {rollbacks:?}");
    }
}
```

- [ ] **Step 2: Run it and record the numbers**

Run: `cargo test -p game --release --test hash_mode -- --ignored --nocapture perf_evidence`
Expected: prints three lines; Delta within ~1.2x of Off for both phases, Full clearly slower. Paste the numbers into the commit message.

- [ ] **Step 3: Flip production**

`crates/client/src/main.rs:90` and `crates/server/src/lib.rs:204`:

```rust
// Delta verification: every undo self-checks against a digest of only the
// data it touches — cheap enough to keep ON in production. Tests and the
// convergence harnesses run HashMode::Full (whole-field hashes), which is
// what catches footprint/capture gaps.
game::set_hash_mode(game::HashMode::Delta);
```

- [ ] **Step 4: Keep the justfile + CLAUDE.md in sync**

- justfile test-full game line gains `--skip perf_evidence_hash_modes` (evidence tests print, they don't gate).
- CLAUDE.md macros bullet: after the sentence about `hash(before) == hash(after undo)` being enforced at rollback time, add: `Verification has three per-entry modes (game::set_hash_mode): Full (whole-field crc32 — default, used by tests/harnesses), Delta (digest of only the entry's mutation footprint — production default), Off.`

- [ ] **Step 5: Full suite + live smoke**

Run: `just test-full` — Expected: PASS.
Run a live smoke per the testing-one-client convention (`just run`, walk around, place/remove a block) and confirm no `Hash verification failed` panics and no perceptible tick-time regression.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/main.rs crates/server/src/lib.rs justfile CLAUDE.md crates/game/tests/hash_mode.rs
git commit -m "feat: always-on delta hash verification in production (Off -> Delta)"
```

---

## Explicit non-goals / follow-ups (do NOT do these in this plan)

- Surgical single-body undo for `set_body_pose_safe` / camera-look `bodies.change()` (whole-`RigidBodySet` clone per input): a real cost on the client re-sim path, but it's undo-*payload* cost, not hash cost, and needs islands/modified-list surgery in the fork. Separate plan.
- Delta digests for the fine-grained narrow-phase op log (excluded from the physics digest by design — see Task 6 note 6).
- Verifying `attach_*_collider_safe` snapshots incrementally (rare ops; whole-field clone dominates regardless).
- Incremental/homomorphic *maintained* hashes (running XOR-multiset hash per field): rejected — every raw mutation path (`raw_mut`, closures, fork internals) would have to keep the running hash coherent, and a missed update silently corrupts verification itself. Recompute-on-footprint is self-contained.

## Self-Review Notes

- Spec coverage: "only hash data that has changed" → Tasks 2/3/5/6 cover every hot log site found in the audit (physics tick, component slots, maps, slotmaps, emit_on_undo, body inserts); unknown-footprint paths deliberately stay Full and are enumerated.
- The standing "never skip hash verification" directive is honored: nothing is disabled; production goes Off → Delta (more checking than today), tests keep Full.
- Type consistency: `undo_digest(pre: u32, ...)` (Task 4) matches call sites in Task 5; `UndoScopeDigest::register(pre: u32, closure -> u32)` matches Task 5's body-insert sites and Task 6's physics wiring; `alloc_fingerprint{,_head}` (Task 3 fork) match the macro's Task 3 usage; `pre_digest`/`revert_digest` signatures match Task 6's wiring.
