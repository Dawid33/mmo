# Undo API Phase 2 — SlotMap True Inverses Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add exact inverse operations (`revert_insert`/`revert_remove`) to the vendored `slotmapd` fork, expose them through a tier-1 `UndoSlotMap` wrapper, and rewrite `create_entity_safe` to log per-entry deltas instead of snapshotting the whole `Ecs`.

**Architecture:** The fork ops restore free-list head, slot version, and `num_elems` bit-exactly; they are valid inverses ONLY for the most recent operation (LIFO), which the transaction log guarantees. Components stay tier-2 in this phase (SparseSecondaryMap remove(insert) is already hash-exact, proven by `hash_restore.rs`) via a new `insert_safe` helper; the renderer's `RemoveEntity` undo notification moves to a compensation-only closure (auto-emit is Phase 3).

**Tech Stack:** Rust stable, vendored slotmapd fork, syn/quote macro from Phase 1.

## Global Constraints

Same as Phase 1 (stable build at every commit; never bare `cargo test -p rollback`; no new deps; strict hash verification; no HashMap iteration in logged state). Test command: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops`.

## slotmapd internals (verified against `crates/slotmapd/src/basic.rs`)

- Sentinel slot at index 0; `free_head` starts at 1. Insert reuse-case (`slots.get_mut(free_head)` hits): `occupied_version = slot.version | 1`, `free_head ← slot.u.next_free`, `num_elems += 1` (`basic.rs:427-441`). Append-case: push `Slot { value, version: 1 }`, `free_head ← idx + 1` (`basic.rs:444-457`). Append only happens when the free list is empty, so the free-list terminator always equals `slots.len()`.
- Remove (`remove_from_slot`, `basic.rs:463-475`): take value, `slot.u.next_free ← free_head`, `free_head ← idx`, `num_elems -= 1`, `version += 1`.

---

### Task 1: Fork ops `revert_insert` / `revert_remove` on `SlotMap`

**Files:**
- Modify: `crates/slotmapd/src/basic.rs` (after `remove`, ~line 497)
- Test: `crates/rollback/tests/hash_restore.rs`

**Interfaces:**
- Produces: `pub fn revert_insert(&mut self, key: K)` and `pub fn revert_remove(&mut self, key: K, value: V)` on `SlotMap<K, V>`. Contract: exact inverse of the MOST RECENT insert/remove only (LIFO); panics on detectable misuse.

- [ ] **Step 1: Write failing tests** (append to `hash_restore.rs`)

```rust
#[test]
fn slotmap_revert_insert_restores_hash_exactly() {
    let mut m: SlotMap<K, u32> = SlotMap::with_key();
    // Build some history so the free list is non-trivial.
    let a = m.insert(1);
    let _b = m.insert(2);
    m.remove(a);
    let before = h(&m);

    // Reuse-case insert (free list non-empty) then revert.
    let k = m.insert(3);
    m.revert_insert(k);
    assert_eq!(before, h(&m), "reuse-case revert_insert must be exact");

    // Append-case insert (drain free list first) then revert.
    let c = m.insert(4); // reuses slot a
    let before2 = h(&m);
    let d = m.insert(5); // appends
    m.revert_insert(d);
    assert_eq!(before2, h(&m), "append-case revert_insert must be exact");
    let _ = c;
}

#[test]
fn slotmap_revert_remove_restores_hash_exactly() {
    let mut m: SlotMap<K, u32> = SlotMap::with_key();
    let a = m.insert(1);
    let _b = m.insert(2);
    let before = h(&m);
    let v = m.remove(a).unwrap();
    m.revert_remove(a, v);
    assert_eq!(before, h(&m), "revert_remove must be exact");
    // The key must still resolve to the restored value.
    assert_eq!(m.get(a).copied(), Some(1));
}

#[test]
fn slotmap_lifo_revert_chain_restores_hash() {
    let mut m: SlotMap<K, u32> = SlotMap::with_key();
    let a = m.insert(1);
    m.remove(a);
    let before = h(&m);
    // insert, insert, remove — revert in reverse order.
    let x = m.insert(10);
    let y = m.insert(20);
    let vy = m.remove(y).unwrap();
    m.revert_remove(y, vy);
    m.revert_insert(y);
    m.revert_insert(x);
    assert_eq!(before, h(&m), "LIFO revert chain must be exact");
}
```

Adjust the `K` key type import: tests already declare `new_key_type! { struct K; }` — reuse it. Value type changes from `()` to `u32` in the new tests (existing tests keep `()`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rollback --test hash_restore`
Expected: compile FAIL — `revert_insert` not found.

- [ ] **Step 3: Implement in `crates/slotmapd/src/basic.rs`**

```rust
    /// Exact inverse of the MOST RECENT `insert` that returned `key`.
    ///
    /// Restores the free list, slot version, and length bit-for-bit, so the
    /// map's full serialized state (and hash) match the pre-insert state and
    /// the next insert allocates the same key it would have originally.
    ///
    /// LIFO contract: only valid if no other insert/remove happened on this
    /// map since the insert being reverted. Panics if `key` is not occupied.
    pub fn revert_insert(&mut self, key: K) {
        let kd = key.data();
        let idx = kd.idx as usize;
        assert!(self.contains_key(key), "revert_insert: key not occupied");
        if kd.version.get() == 1 && idx == self.slots.len() - 1 {
            // Append-case: the slot was pushed; before the insert the free
            // list was empty and free_head == old len == idx.
            let mut slot = self.slots.pop().unwrap();
            unsafe { ManuallyDrop::drop(&mut slot.u.value) };
            core::mem::forget(slot);
            self.free_head = idx as u32;
        } else {
            // Reuse-case: the insert popped this slot off the free list; the
            // old next_free became free_head, so the current free_head is
            // exactly what the slot must point at again.
            let free_head = self.free_head;
            let slot = &mut self.slots[idx];
            unsafe { ManuallyDrop::drop(&mut slot.u.value) };
            slot.u.next_free = free_head;
            slot.version -= 1; // occupied odd -> prior even
            self.free_head = kd.idx;
        }
        self.num_elems -= 1;
    }

    /// Exact inverse of the MOST RECENT `remove` of `key` that returned
    /// `value`. Same LIFO contract as [`Self::revert_insert`].
    pub fn revert_remove(&mut self, key: K, value: V) {
        let kd = key.data();
        let idx = kd.idx as usize;
        assert!(
            self.free_head == kd.idx,
            "revert_remove: key was not the most recently removed slot"
        );
        let slot = &mut self.slots[idx];
        // remove() stored the old free_head in next_free and bumped version.
        self.free_head = unsafe { slot.u.next_free };
        slot.u.value = ManuallyDrop::new(value);
        slot.version = slot.version.wrapping_sub(1);
        self.num_elems += 1;
    }
```

NOTE while implementing: check how `Slot::drop` is defined in this fork — if
`Slot` has a `Drop` impl that reads the union, the `pop().unwrap()` +
`ManuallyDrop::drop` + `mem::forget` dance must match it (look at how `retain`
or `clear` free occupied slots and copy that idiom). Check whether
`kd.version` is a plain `u32` or `NonZeroU32` (`.get()`) and adjust.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rollback --test hash_restore`
Expected: all 6 PASS (3 old + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/slotmapd/src/basic.rs crates/rollback/tests/hash_restore.rs
git commit -m "feat(slotmapd): exact LIFO inverse ops revert_insert/revert_remove"
```

---

### Task 2: `#[undo(slotmap)]` kind + `UndoSlotMap` wrapper in the macro

**Files:**
- Modify: `crates/macros/src/lib.rs`
- Test: `crates/rollback/tests/log_model.rs` (via Task 3's port — this task is compile-neutral until a field uses the kind)

**Interfaces:**
- Consumes: Phase 1 iterator/wiring structure (`paths` with kinds, `Delta`, `UndoOp`, `Entry`, `log_ident_of`), `map_kv` for K/V extraction.
- Produces (macro-generated):
  - `pub enum SlotOp<K, V> { Inserted(K), Removed(K, V) }`
  - `Delta` gains one variant per slotmap field: `#ident(SlotOp<#K, #V>)`
  - `UndoSlotMap<K, V>` with `data: ::slotmapd::SlotMap<K, V>`, `make: Option<fn(SlotOp<K, V>) -> Delta>`, plus `global_log`/`info` like the other wrappers. Methods: `pub fn insert(&mut self, v: V) -> K` (pre-hash, mutate, log `Inserted(k)`), `pub fn remove(&mut self, k: K) -> Option<V>` (logs `Removed(k, v.clone())` only when the key existed), `unsafe fn hash_data`. `Deref<Target = SlotMap<K, V>>`, `Hash`, serde/Clone/Default derives. No `DerefMut`.
  - Rollback arms: `Inserted(k)` → `revert_insert(k)`; `Removed(k, v)` → `revert_remove(k, v)`; hash-verified like every entry.
  - Wiring in `new()`/`reinitialize()`: `make = Some(Delta::#ident)`.
  - Bounds: `K: ::slotmapd::Key + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static`, V same bounds as UndoCell's T.

- [ ] **Step 1: Implement** — mirror the UndoMap additions from Phase 1 exactly (kind validation accepts `"slotmap"`; field type rewrite `UndoSlotMap<#k, #v>` via `map_kv`; `slotmaps` filtered vec + `slot_log_ident`/`slot_k`/`slot_v`/`slot_path`/`slot_path_string` iterators; Delta variants; revert arms; wiring). Insert logs pre-hash BEFORE mutating even though the delta payload (the key) is only known after:

```rust
pub fn insert(&mut self, v: V) -> K {
    let mut hasher = ::crc32fast::Hasher::new();
    ::std::hash::Hash::hash(&self.data, &mut hasher);
    let pre_hash = hasher.finalize();
    let k = self.data.insert(v);
    let mut global = self.global_log.lock().unwrap();
    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
    let make = self.make.expect("UndoSlotMap not wired to a Delta variant");
    global.log.push_back(Entry { transaction: trans, undo: UndoOp::Typed(make(SlotOp::Inserted(k))), pre_hash });
    k
}
```

If `SlotMap<K, V>` lacks a `Default` impl, implement `Default` for `UndoSlotMap` manually with `SlotMap::with_key()`.

- [ ] **Step 2: Verify compile-neutrality**

Run: `cargo build -p rollback && cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops`
Expected: builds, all tests still pass (no field uses the kind yet).

- [ ] **Step 3: Commit**

```bash
git add crates/macros/src/lib.rs
git commit -m "feat(macros): #[undo(slotmap)] kind with UndoSlotMap and SlotOp deltas"
```

---

### Task 3: Port `ecs.entities`; delete the whole-Ecs snapshot

**Files:**
- Modify: `crates/rollback/src/rollback.rs` (`entities` annotation, `Undo<Component<T>>::insert_safe`, `create_entity_safe` rewrite)
- Test: `crates/rollback/tests/log_model.rs`

**Interfaces:**
- Consumes: `UndoSlotMap::insert`, fork revert ops, `Undo::undo` (compensation closure), existing `set_safe`.
- Produces: `Undo<Component<T>>::insert_safe(&mut self, key: EntityKey)` (tier-2: registers entry-removal undo BEFORE inserting `None`); `create_entity_safe` with no `Ecs` clone.

- [ ] **Step 1: Write failing test** (append to `log_model.rs`)

```rust
#[test]
fn create_entity_rolls_back_without_snapshot_and_reuses_key() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);

    r.new_transaction();
    let k1 = r.ecs.create_entity_safe();
    r.rollback();
    assert_eq!(h0, state_hash(&r), "entity creation must fully revert");

    // Determinism: after rollback the next insert must allocate the SAME key.
    r.new_transaction();
    let k2 = r.ecs.create_entity_safe();
    assert_eq!(k1, k2, "key allocation must be deterministic across rollback");
}
```

Run: `cargo test -p rollback --test log_model` — this passes TODAY via the
snapshot; it is the behavior lock for the rewrite. Verify it passes, then
rewrite and verify it still passes.

- [ ] **Step 2: Implement**

Annotate in the `game_data` module:

```rust
    pub struct Ecs {
        #[undo(slotmap)]
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        ...
    }
```

Add near the existing `Undo<Component<T>>` impl:

```rust
impl<T> Undo<Component<T>>
where
    T: 'static + Default + Clone + Send + std::hash::Hash + ::serde::Serialize + for<'a> ::serde::Deserialize<'a>,
{
    /// Creates the (empty) component entry for a new entity, undo-safely.
    /// SparseSecondaryMap remove(insert) is hash-exact (see hash_restore.rs),
    /// so a removal closure is a true inverse here.
    pub fn insert_safe(&mut self, key: EntityKey) {
        self.undo(move |d, _| {
            d.list.remove(key);
        });
        self.list.insert(key, None);
    }
}
```

Rewrite `create_entity_safe`:

```rust
impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        let key = self.entities.insert(());
        // Compensation-only undo: mutates nothing (the typed entities delta
        // reverts the slot); tells the renderer the entity is gone. Ordered
        // here so it fires after the component undos, before the slot revert.
        self.undo(move |_, s| {
            s.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                crate::GameDataUpdateKind::RemoveEntity(key),
            ))
            .unwrap();
        });
        self.camera.insert_safe(key);
        self.isometry.insert_safe(key);
        self.rigidbody.insert_safe(key);
        self.chunk.insert_safe(key);
        self.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            crate::GameDataUpdateKind::CreateEntity(key),
        ));
        key
    }
}
```

(`self.entities` reaches the nested wrapper through `Undo<Ecs>`'s DerefMut —
raw access is fine, the nested wrapper logs itself. The `UndoScope` import
and the `old: Ecs` clone go away.)

- [ ] **Step 3: Run the full suite + workspace build**

Run: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops && cargo build --workspace --bins`
Expected: all PASS (the two `rollback_restore` tests prove `create_player_safe`/`create_mesh` still hash-restore through the new path), workspace builds.

- [ ] **Step 4: Commit**

```bash
git add crates/rollback/src/rollback.rs crates/rollback/tests/log_model.rs
git commit -m "feat: entities on typed slotmap deltas; drop whole-Ecs snapshot in create_entity_safe"
```

---

### Task 4: Extend randomized suite + smoke run

**Files:**
- Modify: `crates/rollback/tests/random_ops.rs`

- [ ] **Step 1: Add entity ops to the random mix** — change the op range to `0..6` and add:

```rust
                    5 => {
                        r.ecs.create_entity_safe();
                    }
```

(`create_mesh` already exercises entity+chunk+physics; this adds bare entity creation pressure on the slotmap free list across transactions.)

- [ ] **Step 2: Run**

Run: `cargo test -p rollback --test random_ops`
Expected: PASS on all 8 seeds.

- [ ] **Step 3: Smoke run** — same 15s server+client run as Phase 1 Task 6; expect 0 panics, region loaded.

- [ ] **Step 4: Commit**

```bash
git add crates/rollback/tests/random_ops.rs
git commit -m "test: entity creation in randomized rollback suite; phase 2 complete"
```

## After this plan

Phase 3 (auto-emit `#[emit]` attributes, removing the compensation closures and manual sends), Phase 4 (rapier arena inverse ops), Phase 5 (public-surface cleanup: no DerefMut, no raw `undo()` outside the crate).
