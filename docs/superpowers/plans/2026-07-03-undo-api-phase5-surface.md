# Undo API Phase 5 — Misuse-Proof Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make unlogged mutation of rollback state unrepresentable: every `&mut` to leaf game data flows through a log-producing or snapshot-producing API.

**Architecture (option B, refined by a survey of all 16 deref/undo/change sites):**
Module structs (`GameData`, `Ecs`, `PhysicsState`) contain ONLY guarded wrappers, so `&mut` on them is harmless — the hole is exclusively `DerefMut` on wrappers of leaf types (`Undo<RigidBodySet>`, `Undo<Component<T>>`, ...). Therefore:

1. Delete the blanket `impl<T> DerefMut for Undo<T>`; the macro instead emits `impl DerefMut for Undo<S>` for each module struct `S`. All pass-through (`data.physics.bodies`), `region.rs` derefs, and `borrow::Partial` `as_refs_mut()` splits keep compiling unchanged.
2. Raw `&mut` to leaf data exists in exactly two licensed places: `change()` (snapshot-backed, unchanged) and inside an `UndoScope` (registration enforced by the drop-assert). For code that needs raw access to MANY fields of a snapshotted struct at once (the physics step), the macro generates per struct `S`: a `#SRaw<'a>` struct of `&'a mut <original field type>` plus `Undo<S>::snapshot_raw(&mut self) -> SRaw<'_>` (clone + undo entry + project through the wrappers) and `UndoScope<S>::raw_fields(&mut self) -> SRaw<'_>` (marks touched; obligation already carried by the scope).
3. `Undo::undo()` becomes `pub(crate)` — outside the rollback crate, tier-2 mutation is `undo_scope()`/`change()`/`snapshot_raw()` only. `emit_on_undo`, `change`, `undo_scope`, `register` stay public.

Migration sites: `camera.rs` (3 undo-before-mutate sites → `undo_scope`), `physics.rs` (`change().as_refs_mut()` → `snapshot_raw()`), `log_model.rs` scope tests (`scope.camera.deref_mut()` → `scope.raw_fields().camera`). `region.rs` and `rollback.rs` compile unchanged.

## Global Constraints

Same as Phases 1–4. Test command: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops`. The full battery (suite + workspace build + 15 s smoke run) gates completion.

---

### Task 1: Macro — per-struct DerefMut, `SRaw` projections, demote `undo()`

**Files:**
- Modify: `crates/macros/src/lib.rs`

**Interfaces (generated, per module struct `S` with fields `f_i` of original types `I_i`):**
- `impl ::std::ops::DerefMut for Undo<#S>` (blanket impl deleted from `boilerplate`).
- `pub struct #SRaw<'a> { pub #f_i: &'a mut #I_i, ... }` — names like `GameDataRaw`, `EcsRaw`, `PhysicsStateRaw`.
- `impl Undo<#S> { pub fn snapshot_raw(&mut self) -> #SRaw<'_> { let old = self.data.clone(); self.undo(move |d, _| *d = old); #SRaw { f_i: &mut self.data.f_i.data, ... } } }`
- `impl<'a> UndoScope<#S, 'a> { pub fn raw_fields(&mut self) -> #SRaw<'_> { self.touched = true; #SRaw { f_i: &mut self.value.data.f_i.data, ... } } }`
- `Undo::undo` signature gains `pub(crate)`.

Implementation notes: collect per-struct `(struct_ident, Vec<(field_ident, original_ty)>)` in the field-mutation loop BEFORE the types are wrapped. `.data` access on every wrapper kind is same-module. For fields whose type is another module struct (e.g. `GameData.ecs`), the raw type is that struct itself (`&mut Ecs`) — harmless, all its fields are wrapped.

- [ ] Implement; `cargo build -p rollback` must fail only at the known migration sites in `game` (that failure list IS the misuse-detector working — verify nothing else breaks).
- [ ] Commit: `feat(macros): per-struct DerefMut, SRaw projections, pub(crate) undo`

### Task 2: Migrate `camera.rs`, `physics.rs`, tests

**Files:**
- Modify: `crates/game/src/camera.rs`, `crates/game/src/physics.rs`, `crates/rollback/tests/log_model.rs`

Camera proj-matrix site (undo-before-mutate → scope):

```rust
            if let Some(resolution) = client.input.window_resized() {
                let old = ecs.camera.get(e_id).proj_matrix.clone();
                ecs.camera.emit_on_undo(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Undo,
                    crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, old),
                ));
                let mut scope = ecs.camera.undo_scope();
                scope
                    .get_mut(e_id)
                    .proj_matrix
                    .set_aspect(OrderedFloat((resolution.width / resolution.height) as f32));
                let m = scope.get_mut(e_id).proj_matrix.clone();
                scope.register(move |d, _| d.get_mut(e_id).proj_matrix = old);
                ecs.camera.send(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::UpdateCameraViewProj(e_id, m),
                ));
            }
```

(`old` is moved into `register`; the `emit_on_undo` needs its own clone — add one.) Both kinematic body sites follow the same shape: reads via `Deref` first, then `let mut scope = data.physics.bodies.undo_scope();`, mutate via `scope.get_mut(handle).unwrap()`, `scope.register(...)` with the previously captured old values.

Physics step:

```rust
        let p = data.physics.snapshot_raw();
        self.pipeline.step(
            p.gravity,
            p.integration_parameters,
            p.islands,
            p.broad_phase,
            p.narrow_phase,
            p.bodies,
            p.colliders,
            p.implules_joint_set,
            p.multi_body_joint_set,
            p.ccd_solver,
            &(),
            &(),
        );
```

(`&mut` coerces to `&` for the read-only params. Same snapshot cost as the old `change()`.) Scope tests: `scope.camera.deref_mut().insert(..)` → `scope.raw_fields().camera.insert(..)`.

- [ ] Full battery: suite + `cargo build --workspace --bins` + 15 s smoke run.
- [ ] Update the spec (Phase 5 section: describe the final surface).
- [ ] Commit: `feat: unlogged mutation unrepresentable outside rollback crate; phase 5 complete`
