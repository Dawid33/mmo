# Undo API Phase 3 — Auto-Emit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render notifications (`GameDataUpdate`) derive automatically from slotmap deltas in both apply and undo directions via a `#[emit(...)]` field attribute; the remaining compensating sends get a first-class `emit_on_undo` API replacing `set_safe_with_closure`.

**Architecture:** Scoping decision (verified against `render_world.rs:440-461`): `AddCameraComponent` carries the rigid-body pose — cross-field data a camera delta can't provide — so camera events stay explicit but drift-resistant via `emit_on_undo`. Entities' `CreateEntity`/`RemoveEntity` are pure functions of the slot key, so they auto-emit. `GameDataUpdateKind`'s tuple-variant constructors are the emit fn pointers, same trick as `Delta`/`FieldUndo` wiring.

**Tech Stack:** Rust stable, existing macro structure.

## Global Constraints

Same as Phases 1–2. Test command: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops`. Emits must tolerate `client: None` (server regions) exactly like the existing `send()` does — never unwrap the sender for an emit.

---

### Task 1: `#[emit(insert = ..., remove = ...)]` on slotmap fields

**Files:**
- Modify: `crates/macros/src/lib.rs`
- Modify: `crates/rollback/src/rollback.rs` (annotate `entities`, shrink `create_entity_safe`)
- Test: `crates/rollback/tests/log_model.rs`

**Interfaces:**
- Field attribute (slotmap kind only, both keys required):
  `#[emit(insert = CreateEntity, remove = RemoveEntity)]` — values are variant names of `crate::GameDataUpdateKind` whose payload is exactly `(K)`.
- `UndoSlotMap<K, V>` gains `emit_insert: Option<fn(K) -> crate::GameDataUpdateKind>` and `emit_remove: Option<fn(K) -> crate::GameDataUpdateKind>` (serde/debug-skipped).
- Behavior: `insert()` emits `Do`+`emit_insert(k)`; `remove()` emits `Do`+`emit_remove(k)`; reverting `Inserted(k)` emits `Undo`+`emit_remove(k)`; reverting `Removed(k, _)` emits `Undo`+`emit_insert(k)`. All emits skipped when the field has no `#[emit]` or the log has no client sender.

- [ ] **Step 1: Write failing test** (append to `log_model.rs`)

```rust
#[test]
fn entity_creation_auto_emits_in_both_directions() {
    let (mut r, recv) = new_rollback();
    r.new_transaction();
    let key = r.ecs.create_entity_safe();

    let applied: Vec<_> = recv.try_iter().collect();
    assert!(
        applied.iter().any(|u| matches!(u.update_kind, rollback::GameDataUpdateKind::CreateEntity(k) if k == key)),
        "apply must emit CreateEntity, got {applied:?}"
    );

    r.rollback();
    let undone: Vec<_> = recv.try_iter().collect();
    assert!(
        undone.iter().any(|u| matches!(u.update_kind, rollback::GameDataUpdateKind::RemoveEntity(k) if k == key)),
        "undo must emit RemoveEntity, got {undone:?}"
    );
}
```

This passes TODAY through the manual sends — it is the behavior lock. Verify it
passes, implement auto-emit, remove the manual sends, verify it still passes.

- [ ] **Step 2: Macro implementation**

2a. Parse the attribute during path traversal (fields are pre-strip there) and
extend `paths` to `(TokenStream, syn::Field, Option<String>, Option<(syn::Path, syn::Path)>)`:

```rust
/// Reads `#[emit(insert = Variant, remove = Variant)]` off a field.
fn emit_pair(f: &syn::Field) -> Option<(syn::Path, syn::Path)> {
    let attr = f.attrs.iter().find(|a| a.path().is_ident("emit"))?;
    let mut insert = None;
    let mut remove = None;
    attr.parse_nested_meta(|meta| {
        let value: syn::Path = meta.value()?.parse()?;
        if meta.path.is_ident("insert") { insert = Some(value); }
        else if meta.path.is_ident("remove") { remove = Some(value); }
        Ok(())
    })
    .expect("malformed #[emit(insert = ..., remove = ...)]");
    Some((insert.expect("emit: missing insert"), remove.expect("emit: missing remove")))
}
```

Strip `emit` alongside `undo` in the field-mutation loop
(`!a.path().is_ident("undo") && !a.path().is_ident("emit")`). Panic if a
non-slotmap field carries `#[emit]`.

2b. `UndoSlotMap` fields + emit in `log_op`/ops:

```rust
// struct fields (serde(skip), debug(skip)):
emit_insert: ::std::option::Option<fn(K) -> crate::GameDataUpdateKind>,
emit_remove: ::std::option::Option<fn(K) -> crate::GameDataUpdateKind>,
// in insert(), after log_op (global lock scope or reacquire — send via the
// same lock guard's client field):
if let (Some(mk), Some(client)) = (self.emit_insert, global.client.as_ref()) {
    client.send(crate::GameDataUpdate::new(crate::GameDataTransactionKind::Do, mk(k))).unwrap();
}
```

(Restructure `log_op` to take the op AND perform the emit while the guard is
held, so insert/remove don't lock twice. `K: Copy`-like usage: keys are Copy
(`slotmapd::Key: Copy`), pass by value.)

2c. Revert-arm emits (slot arm in `rollback()`), after the hash check:

```rust
match op_kind_for_emit {
    // Inserted(k) reverted => the entity is gone
    /* inside the Inserted arm: */
    if let (Some(mk), Some(client)) = (self.#slot_path1.emit_remove, rollback_log.client.as_ref()) {
        client.send(crate::GameDataUpdate::new(crate::GameDataTransactionKind::Undo, mk(k))).unwrap();
    }
    // Removed(k, v) reverted => the entity is back
    /* inside the Removed arm: */
    if let (Some(mk), Some(client)) = (self.#slot_path1.emit_insert, rollback_log.client.as_ref()) {
        client.send(crate::GameDataUpdate::new(crate::GameDataTransactionKind::Undo, mk(k))).unwrap();
    }
}
```

(Capture `k` before moving it into `revert_insert`; it's Copy.)

2d. Wiring in `new()`/`reinitialize()` for fields with an emit pair (build
`slot_emit_*` iterators from `paths` entries that have both a slotmap kind and
an emit pair):

```rust
#(r.#emit_path.emit_insert = Some(crate::GameDataUpdateKind::#emit_insert_variant);)*
#(r.#emit_path.emit_remove = Some(crate::GameDataUpdateKind::#emit_remove_variant);)*
```

2e. Annotate and shrink in `crates/rollback/src/rollback.rs`:

```rust
        #[undo(slotmap)]
        #[emit(insert = CreateEntity, remove = RemoveEntity)]
        entities: SlotMap<EntityKey, ()>,
```

```rust
impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        // CreateEntity / RemoveEntity emits ride the entities delta (both
        // directions) — see #[emit] on the field.
        let key = self.entities.insert(());
        self.camera.insert_safe(key);
        self.isometry.insert_safe(key);
        self.rigidbody.insert_safe(key);
        self.chunk.insert_safe(key);
        key
    }
}
```

(The compensation closure and both manual sends are deleted.)

- [ ] **Step 3: Run the behavior-lock test + full suite + workspace build**

Run: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops && cargo build --workspace --bins`
Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/macros/src/lib.rs crates/rollback/src/rollback.rs crates/rollback/tests/log_model.rs
git commit -m "feat: #[emit] auto-derives CreateEntity/RemoveEntity from entities deltas"
```

---

### Task 2: `emit_on_undo` replaces `set_safe_with_closure`

**Files:**
- Modify: `crates/macros/src/lib.rs` (add `emit_on_undo` to `impl Undo<T>`)
- Modify: `crates/rollback/src/rollback.rs` (camera site in `create_player_safe`; delete `set_safe_with_closure`)
- Modify: `crates/game/src/camera.rs` (undo-send site)
- Test: `crates/rollback/tests/log_model.rs`

**Interfaces:**
- `Undo<T>::emit_on_undo(&mut self, event: crate::GameDataUpdate)` — registers a compensation-only entry: mutates nothing, sends `event` when the undo runs. Hash-safe by construction (state unchanged ⇒ recorded pre-hash matches).

- [ ] **Step 1: Write failing test** (append to `log_model.rs`)

```rust
#[test]
fn player_creation_emits_camera_pair() {
    let (mut r, recv) = new_rollback();
    r.new_transaction();
    r.create_player_safe(0);

    let applied: Vec<_> = recv.try_iter().collect();
    assert!(
        applied.iter().any(|u| matches!(u.update_kind, rollback::GameDataUpdateKind::AddCameraComponent(..))),
        "apply must emit AddCameraComponent, got {applied:?}"
    );

    r.rollback();
    let undone: Vec<_> = recv.try_iter().collect();
    assert!(
        undone.iter().any(|u| matches!(u.update_kind, rollback::GameDataUpdateKind::RemoveCameraComponent(_))),
        "undo must emit RemoveCameraComponent, got {undone:?}"
    );
}
```

(Passes today via `set_safe_with_closure` — behavior lock; keep green through the refactor.)

- [ ] **Step 2: Implement**

Macro (`impl Undo<T>` item):

```rust
/// Registers a compensation-only undo entry: no data changes, `event` is
/// sent when the undo runs. Use for render notifications whose payload
/// isn't derivable from a single field's delta.
pub fn emit_on_undo(&mut self, event: crate::GameDataUpdate) {
    self.undo(move |_, s| {
        s.send(event).unwrap();
    });
}
```

`crates/rollback/src/rollback.rs` — camera site becomes:

```rust
        let cam = Camera::new(handle);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(e, cam.proj_matrix, position),
        ));
        self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::RemoveCameraComponent(e),
        ));
        self.data.ecs.camera.set_safe(e, Some(cam));
        self.data.player_entites.insert(client_id, e);
```

(`emit_on_undo` BEFORE `set_safe`: LIFO pops the value-restore first, then the
notification — same order as the old closure.) Delete `set_safe_with_closure`.

`crates/game/src/camera.rs` — replace the hand-rolled undo-send (the closure
around line 37 sending `UpdateCameraViewProj(e_id, old)`) with `emit_on_undo`
carrying the old value, keeping the `set_safe`/mutation as-is. Read the
function first; preserve its exact event payloads and ordering (emit_on_undo
registered before the data mutation it compensates).

- [ ] **Step 3: Run full suite + workspace build; smoke run**

Run: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops && cargo build --workspace --bins`
Then the 15s server+client smoke run (0 panics, region loads).

- [ ] **Step 4: Commit**

```bash
git add crates/macros/src/lib.rs crates/rollback/src/rollback.rs crates/game/src/camera.rs crates/rollback/tests/log_model.rs
git commit -m "feat: emit_on_undo compensation API; delete set_safe_with_closure; phase 3 complete"
```

## After this plan

Phase 4 (rapier arena inverse ops + `UndoArena`, replacing the `PhysicsState` `change()` snapshots) and Phase 5 (surface cleanup: remove `DerefMut`/public `undo()`, `raw_mut()` audit).
