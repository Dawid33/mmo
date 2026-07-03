# Undo API Phase 4 — Rapier Arena Inverses Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Exact LIFO inverse ops for rapier's generational `Arena` and `RigidBodySet`, so body inserts undo via tiny reverts instead of whole-`PhysicsState` snapshots. Resolve the spec's open physics-step question by documenting option 2 (per-tick `change()` snapshot stays — `step()` mutates broad/narrow-phase caches opaquely and physics state is bounded per active region).

**Architecture (verified against the fork):** `Arena { items: Vec<Entry>, generation: u32, free_list_head: Option<u32>, len: u32 }` (`crates/rapier/src/data/arena.rs`). Insert fast-path pops the free head (generation unchanged); slow-path `reserve`s a chained Free block first — so revert needs the pre-insert `(free_list_head, items.len())`, truncating on the slow path. Remove bumps `generation` and pushes the slot on the free list — revert restores the Index's generation and decrements. `RigidBodySet::insert` additionally does `modified_bodies.push_unchecked(handle, ..)`, and `modified_bodies` IS part of the derived `Hash` — revert must pop it (assert LIFO). No macro changes: `physics.bodies` is already its own loggable field (`Undo<RigidBodySet>`), and a tier-2 `undo_scope` closure calling a true fork inverse is exactly what tier 2 is for.

## Global Constraints

Same as Phases 1–3. Test command: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops`.

---

### Task 1: Fork ops on `Arena`, `ModifiedObjects`, `RigidBodySet`

**Files:**
- Modify: `crates/rapier/src/data/arena.rs`, `crates/rapier/src/data/modified_objects.rs`, `crates/rapier/src/dynamics/rigid_body_set.rs`
- Test: `crates/rollback/tests/hash_restore.rs`

**Interfaces:**
- `Arena::alloc_state(&self) -> (Option<u32>, u32)` — `(free_list_head, items.len())`, captured BEFORE an insert.
- `Arena::revert_insert(&mut self, i: Index, prev_free_head: Option<u32>, prev_items_len: u32)` — exact LIFO inverse of the insert that returned `i` given the pre-insert alloc state; panics on generation mismatch.
- `Arena::revert_remove(&mut self, i: Index, value: T)` — exact LIFO inverse of the most recent remove.
- `ModifiedObjects::pop(&mut self) -> Option<Handle>`.
- `RigidBodySet::alloc_state(&self) -> (Option<u32>, u32)` and `RigidBodySet::revert_insert(&mut self, handle: RigidBodyHandle, prev_free_head: Option<u32>, prev_items_len: u32)` — pops `modified_bodies` (assert it is `handle`) then reverts the arena insert.

- [ ] **Step 1: Failing tests** (append to `hash_restore.rs`)

```rust
#[test]
fn rigidbodyset_revert_insert_restores_hash_exactly() {
    let mut bodies = RigidBodySet::new();
    // Slow path (empty arena grows storage).
    let s0 = h(&bodies);
    let st = bodies.alloc_state();
    let h1 = bodies.insert(RigidBodyBuilder::fixed().build());
    bodies.revert_insert(h1, st.0, st.1);
    assert_eq!(s0, h(&bodies), "slow-path (grow) revert must be exact");

    // Build history: occupied + freed slot, then fast-path insert + revert.
    let a = bodies.insert(RigidBodyBuilder::fixed().build());
    let mut islands = IslandManager::new();
    let mut colliders = ColliderSet::new();
    let mut ij = ImpulseJointSet::new();
    let mut mj = MultibodyJointSet::new();
    let _b = bodies.insert(RigidBodyBuilder::fixed().build());
    bodies.remove(a, &mut islands, &mut colliders, &mut ij, &mut mj, true);
    let s1 = h(&bodies);
    let st = bodies.alloc_state();
    let c = bodies.insert(RigidBodyBuilder::fixed().build());
    bodies.revert_insert(c, st.0, st.1);
    assert_eq!(s1, h(&bodies), "fast-path (reuse) revert must be exact");
}
```

(No `revert_remove` test at the `RigidBodySet` level: `RigidBodySet::remove`
also mutates islands/colliders/joints, out of scope — body removal isn't used
by game code yet. `Arena::revert_remove` still ships for the future, tested via
the arena through... skip it: no public arena access from tests. Leave
`revert_remove` implemented + documented, untested externally, and note it in
the commit message.)

- [ ] **Step 2: Implement**

`arena.rs` (inside `impl<T> Arena<T>`):

```rust
    /// Allocator state to capture before an `insert` for [`Self::revert_insert`].
    pub fn alloc_state(&self) -> (Option<u32>, u32) {
        (self.free_list_head, self.items.len() as u32)
    }

    /// Exact inverse of the MOST RECENT `insert` that returned `i`, given the
    /// allocator state captured just before that insert. Restores the free
    /// list, storage length, generation, and len bit-for-bit. LIFO contract:
    /// invalid if any other insert/remove happened since.
    pub fn revert_insert(&mut self, i: Index, prev_free_head: Option<u32>, prev_items_len: u32) {
        let idx = i.index as usize;
        match &self.items[idx] {
            Entry::Occupied { generation, .. } if *generation == i.generation => {}
            _ => panic!("revert_insert: index not occupied with matching generation"),
        }
        if (self.items.len() as u32) > prev_items_len {
            // Slow path: the insert reserved a chained Free block and took its
            // first slot; drop the whole block.
            debug_assert_eq!(i.index, prev_items_len);
            self.items.truncate(prev_items_len as usize);
        } else {
            // Fast path: the insert popped this slot off the free list; the
            // slot's old next_free became the current free_list_head.
            self.items[idx] = Entry::Free {
                next_free: self.free_list_head,
            };
        }
        self.free_list_head = prev_free_head;
        self.len -= 1;
    }

    /// Exact inverse of the MOST RECENT `remove` of `i` that returned `value`.
    /// Same LIFO contract as [`Self::revert_insert`].
    pub fn revert_remove(&mut self, i: Index, value: T) {
        let idx = i.index as usize;
        match self.items[idx] {
            Entry::Free { next_free } => self.free_list_head = next_free,
            _ => panic!("revert_remove: slot not free"),
        }
        self.items[idx] = Entry::Occupied {
            generation: i.generation,
            value,
        };
        self.generation -= 1;
        self.len += 1;
    }
```

`modified_objects.rs`:

```rust
    /// Pops the most recently pushed handle (LIFO undo support).
    pub fn pop(&mut self) -> Option<Handle> {
        self.0.pop()
    }
```

`rigid_body_set.rs`:

```rust
    /// Allocator state to capture before `insert` for [`Self::revert_insert`].
    pub fn alloc_state(&self) -> (Option<u32>, u32) {
        self.bodies.alloc_state()
    }

    /// Exact inverse of the MOST RECENT `insert` that returned `handle`,
    /// given the state captured by [`Self::alloc_state`] just before it.
    /// LIFO contract: invalid if any other body was inserted/removed/marked
    /// modified since.
    pub fn revert_insert(&mut self, handle: RigidBodyHandle, prev_free_head: Option<u32>, prev_items_len: u32) {
        let popped = self.modified_bodies.pop();
        assert_eq!(popped, Some(handle), "revert_insert: not the most recent insert");
        self.bodies.revert_insert(handle.0, prev_free_head, prev_items_len);
    }
```

Check `ModifiedObjects::pop` return type vs assert (`Handle: PartialEq + Debug`
bound may be needed on the impl block — add where required).

- [ ] **Step 3: Run** `cargo test -p rollback --test hash_restore` — all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/rapier/src/data/arena.rs crates/rapier/src/data/modified_objects.rs crates/rapier/src/dynamics/rigid_body_set.rs crates/rollback/tests/hash_restore.rs
git commit -m "feat(rapier): exact LIFO inverse ops for Arena and RigidBodySet inserts"
```

---

### Task 2: Body inserts via `undo_scope` + true inverse; drop PhysicsState snapshots

**Files:**
- Modify: `crates/rollback/src/rollback.rs` (`create_mesh`, `create_player_safe`)
- Modify: `docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md` (resolve the open question)
- Test: existing suite (rollback_restore covers both paths end-to-end)

- [ ] **Step 1: Rewrite both insert sites** (same shape in `create_mesh` and `create_player_safe`):

```rust
        // Tier-2 with a true fork inverse: reverts the arena allocator state
        // exactly, no PhysicsState clone. See RigidBodySet::revert_insert.
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope();
        let handle = scope.insert(body);
        scope.register(move |bodies, _| bodies.revert_insert(handle, prev_head, prev_len));
```

(`self.data.physics.bodies` reaches `Undo<RigidBodySet>` through raw DerefMut
on `physics` — the nested field logs itself. In `create_mesh` the receiver is
`self.ecs`-style paths: adjust to `self.data.physics.bodies` exactly as the
current `change()` line does.)

- [ ] **Step 2: Resolve the spec's open question** — replace the "Open question" section body with: physics-step undo keeps the per-tick `PhysicsState` `change()` snapshot in `PhysicsController::on_tick` (option 2): `step()` mutates broad-phase/narrow-phase/island caches with no per-entry delta available, and physics state is bounded per active region, unlike world data. Body add/remove no longer snapshots.

- [ ] **Step 3: Full suite + build + smoke run** — the standard battery.

- [ ] **Step 4: Commit**

```bash
git add crates/rollback/src/rollback.rs docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md
git commit -m "feat: body inserts undo via arena revert; physics snapshots only per-tick; phase 4 complete"
```
