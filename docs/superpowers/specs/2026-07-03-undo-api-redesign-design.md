# Undo API Redesign — Design Spec

Date: 2026-07-03
Status: approved (design), pending implementation plan
Scope: `crates/rollback`, `crates/macros`, vendored `crates/slotmapd` and `crates/rapier`, call sites in `crates/game`

## Motivation

The current undo API makes application code both verbose and easy to get wrong.
The client desync fixed in `5111aae` came from exactly these weaknesses, and the
fix itself (whole-`Ecs`/`PhysicsState` snapshots) is not viable at the planned
world scale.

Pain points, each observed in a real bug this cycle:

1. **Ordering discipline.** `Undo::undo()` records the hash of the state the
   closure restores to, so it must be registered *before* mutating;
   `delayed_undo()` exists for mutate-first flows. Nothing enforces the choice —
   picking wrong panics at rollback time, far from the bug site.
2. **Unlogged mutation.** `Undo<T>: DerefMut` silently mutates with no undo
   recorded (the `clients.insert` bug in `region.rs`).
3. **Hand-written inverses that aren't inverses.** `remove(insert(x))` on
   SlotMap / rapier arenas is deterministic and looks correct but provably does
   not restore state: slot versions, generations, and free lists differ, and
   the *next* insert allocates a different key — a genuine client/server
   determinism break, not just a failed hash check
   (`crates/rollback/tests/hash_restore.rs` documents this).
4. **Manual render notifications.** Every mutation site sends
   `GameDataUpdate`s by hand and weaves the compensating send into its undo
   closure. Data changes and notifications can silently drift.
5. **Snapshot cost.** The interim fix clones whole `Ecs`/`PhysicsState` per
   entity spawn. Prohibitive at target world size; per-entry deltas required.

## Constraints (fixed by prior decisions)

- **No wholesale snapshots** of chunks, ECS maps, or physics state inside
  transactions. World scale makes cloning prohibitive; the incremental design
  is the point of the system.
- **Closures must remain available.** Not every structure can be vendored or
  made rollback-aware. A deterministic structure wrapped in a closure-based
  undo is a first-class, permanent use case — not a legacy path.
- **Hash verification stays strict and universal.** `hash(before) ==
  hash(after undo)` is the definition of a correct undo. State is serialized
  and shipped between client and server, so allocator internals (free lists,
  versions) are semantic state. No per-field opt-outs or relaxed tiers.
- **Determinism invariants** (vendored math stack, `enhanced-determinism`,
  ordered floats) are untouched.
- Transaction semantics (`new_transaction` / `rollback` / `forget`) and the
  reconcile flow in `region.rs` keep their current shape.

## Design

### 1. Log model

One global, transaction-tagged log. Entries are data, not parallel queues:

```rust
enum LogEntry {
    Typed(Delta),                 // inspectable, auto-invertible, auto-emitting
    Opaque {
        field: FieldId,           // which root field the closure applies to
        undo: FieldUndoFn,        // enum-dispatched FnOnce over known field types
        pre_hash: u32,            // captured by the guard, not the caller
    },
}
// log: VecDeque<(TransactionId, LogEntry)>
```

Replacing today's split (global `(trans, field, hash)` index + per-field
`VecDeque<Box<dyn FnOnce>>` queues) with a single queue removes the class of
bugs where the two structures disagree. `FieldUndoFn` is a macro-generated enum
(one variant per root field type) so closures stay statically typed without
per-field queues.

`rollback()` walks entries of the current transaction back-to-front, applies
each inverse, and verifies the recorded hash. `forget()` drops the oldest
transaction's entries front-to-back. Unchanged semantics, simpler mechanics.

### 2. Tier 1 — typed deltas for owned containers

Logged wrapper types whose mutating methods log-and-mutate in one call.
Ordering bugs become unrepresentable; nobody writes inverses by hand.

| Wrapper | Backing | Delta examples |
|---|---|---|
| `UndoCell<T: Clone>` | scalar fields (`tick`, `next_game_event_id`) | `CellSet { old: T }` |
| `UndoBTreeMap<K, V: Clone>` | `player_entites`, `clients` | `MapInserted { key, prev: Option<V> }`, `MapRemoved { key, value: V }` |
| `UndoSlotMap<K, V>` | `ecs.entities` | `SlotInserted { key }`, `SlotRemoved { key, slot: SlotState<V> }` |
| `UndoSparseSecondary<K, V>` | components | `ComponentSet { key, old: Option<V> }` |
| `UndoArena` (rapier fork) | bodies, colliders | `BodyInserted { handle }`, `BodyRemoved { handle, body, slot } ` |
| voxel access on `Chunk` | `set_voxel(idx, v)` | `VoxelSet { chunk, idx, old: Voxel }` |

Fork surgery required:

- **slotmapd**: expose true inverse ops — `revert_insert(key)` restores
  `free_head` and slot version exactly; `revert_remove(key, slot_state)`
  restores the slot bit-for-bit. Delta payloads are ~bytes, not clones of the
  map.
- **rapier**: same pair for the body/collider arenas (generation + free list
  restore). Bookkeeping updates in `IslandManager`/joint sets on remove are
  part of the delta payload where rapier mutates them.

Whole-chunk `set_safe(e, Some(chunk))` remains for chunk load/unload (that is
a genuine whole-value event), but voxel edits go through `set_voxel` deltas.

`DerefMut` on all wrappers is **removed**. Reads stay via `Deref`. A
greppable `raw_mut()` (doc-flagged: bypasses rollback; setup/loading only)
replaces incidental unlogged mutation.

### 3. Tier 2 — closure escape hatch, made safe

For structures that can't feasibly be made rollback-aware. Same power as
today's closures, with the ordering and hash capture moved into a guard:

```rust
let mut scope = field.undo_scope();          // pre-hash captured here, once
let handle = scope.get_mut().insert(body);   // mutate freely, values available
scope.register(move |data, emit| {           // must be a TRUE inverse
    // restore state; emit compensating GameDataUpdates via `emit`
});
// dropping a scope that mutated without register() => debug_assert / log error
```

Contract, stated in docs and enforced by the strict hash check: the closure
must restore the full serialized state bit-for-bit. `hash_restore.rs` shows
which "obvious inverses" fail this (slotmap/arena remove) — for those, use the
tier-1 wrappers or carry the needed state in the closure.

### 4. Render updates: auto-emit from deltas

Each `Delta` derives its `GameDataUpdate` in both directions: applying
`ComponentSet { key, old: None }` on the camera component emits
`AddCameraComponent`; reverting it emits `RemoveCameraComponent`. Emission
happens where the delta is logged/reverted, so notifications cannot drift from
data changes.

- The delta→update mapping is declared per field in the macro, e.g.
  `#[emit(add = AddCameraComponent, remove = RemoveCameraComponent)]`.
  Fields without an `emit` attribute emit nothing.
- Semantic events with no 1:1 data change (`SetFreeCam`) use an explicit
  `emit()` on the transaction/root — the only remaining manual sends besides
  tier-2 closures, which keep explicit compensating sends via the `emit`
  handle passed to the closure.

### 5. Macro (`#[rollback]`) v2

Still applied to the module; still wraps struct fields. Changes to what it
emits:

- Field wrapper selection via per-field attribute:
  `#[undo(cell)]`, `#[undo(map)]`, `#[undo(slotmap)]`, `#[undo(component)]`,
  `#[undo(opaque)]` — explicit beats type-name guessing.
- Generates: the `Delta` enum (variants only for fields present), the
  `FieldUndoFn` dispatch enum, `LogEntry`, wrapper field types, `Rollback`
  root with `new`/`reinitialize`/`new_transaction`/`rollback`/`forget`,
  auto-emit glue from `#[emit(...)]` attributes.
- No `DerefMut` impls. `Deref` (read) impls stay.
- Hash capture lives inside wrapper methods / `undo_scope()`, never at call
  sites.

### What application code looks like after

```rust
pub fn create_player_safe(&mut self, client_id: ClientId) {
    let e = self.ecs.create_entity();                    // typed delta, auto CreateEntity
    let handle = {
        let mut phys = self.physics.undo_scope();        // tier 2: rapier internals
        let h = phys.get_mut().bodies.insert(body);      //   (or tier-1 UndoArena once forked)
        phys.register(move |p, _| p.bodies.revert_insert(h));
        h
    };
    self.ecs.rigidbody.set(e, Some(handle));             // typed delta
    self.ecs.camera.set(e, Some(Camera::new(handle)));   // auto Add/RemoveCameraComponent
    self.player_entites.insert(client_id, e);            // typed delta
}
```

No ordering rules, no hand-written inverse maps, no manual channel sends, no
whole-struct clones.

## Resolved: undoing a physics step (Phase 4 decision)

Option 2 adopted: `PhysicsController::on_tick` keeps the per-tick
`PhysicsState` `change()` snapshot. rapier's `step()` mutates broad-phase /
narrow-phase / island caches with no per-entry delta available, and physics
state is bounded per active region — unlike world data, cloning it once per
tick is acceptable. Body insert/remove no longer snapshots: the rapier fork
provides exact LIFO inverses (`Arena::revert_insert`/`revert_remove`,
`RigidBodySet::revert_insert`) used via `undo_scope` closures.

## Final surface (Phase 5, as built)

Unlogged mutation is unrepresentable outside the rollback crate:

- The blanket `DerefMut` on `Undo<T>` is gone. The macro emits `DerefMut` only
  for module structs (`GameData`, `Ecs`, `PhysicsState`) — safe because every
  one of their fields is a guarded wrapper. Pass-through access and
  `borrow::Partial` splits keep working.
- Raw `&mut` to leaf data exists in exactly two licensed forms: `change()` /
  `snapshot_raw()` (snapshot logged first; `snapshot_raw` also projects a
  generated `#SRaw` view of `&mut` inner fields for multi-field consumers like
  the physics step) and `UndoScope` (`DerefMut` + `raw_fields()`; the
  drop-assert enforces `register()`).
- `undo()` and a `raw_mut()` helper are `pub(crate)`: trusted tier-2 helpers
  inside the rollback crate (`insert_safe`, `set_safe`, `change`,
  `emit_on_undo`) build on them; application crates cannot.

## Migration plan (phased, each phase green before the next)

1. **Foundations**: new log model + `UndoCell`/`UndoBTreeMap` wrappers behind
   the existing macro (new attribute args), old API still compiles. Port
   `tick`, `next_game_event_id`, `player_entites`, `clients`, and the
   `region.rs` input/`fps_cam_mode` undos.
2. **slotmapd fork**: inverse ops + `UndoSlotMap`/`UndoSparseSecondary`; port
   `ecs.entities` and components; `create_entity_safe` becomes tier-1; delete
   the interim whole-`Ecs` snapshot.
3. **Auto-emit**: `#[emit]` attributes on camera/entity/voxel paths; strip
   manual sends and `set_safe_with_closure`.
4. **rapier fork**: arena inverse ops; port body/collider add/remove; delete
   the interim `PhysicsState` snapshots; resolve the physics-step question.
5. **Cleanup**: remove `DerefMut`, old `undo()`/`delayed_undo()`/`change()`
   from the public surface; `raw_mut()` audit.

## Testing

- Extend `crates/rollback/tests/rollback_restore.rs`: every wrapper op gets an
  apply→rollback→hash-identical test; every `#[emit]` mapping gets an
  emitted-updates assertion (apply and undo directions).
- Randomized sequences: seeded `oorandom` (already a workspace dep), N random
  ops across M transactions, roll all back, assert bit-identical hash; also
  partial rollback + `forget` interleavings.
- Keep `hash_restore.rs` as the vendored-container behavior canary.
- Determinism guard: same op sequence on two `Rollback` instances ⇒ identical
  hashes at every transaction boundary.
