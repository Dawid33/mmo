---
name: first-tick-massprops-spike
description: "RESOLVED 2026-07-12 (develop 34ec12b): fixed-body mass-props skip landed in the fork (attach_collider + user_changes recompute, TYPE added to trigger); don't re-plan. Original root cause below for reference"
metadata: 
  node_type: memory
  type: project
  originSessionId: ffa91ce4-4121-48e6-b328-7fd1be0bbd11
---

The "first tick of a freshly loaded region ~318ms" outlier in
[[client-prediction-runaway]] is now fully attributed:

- `seed_terrain_genesis` inserts one fixed body + one collider per chunk via
  `insert_with_parent` → `attach_collider` sets `MODIFIED|COLLIDERS` on each
  body and pushes it into `RigidBodySet::modified_bodies`.
- `modified_bodies` and `RigidBody.changes` are **serialized** (not
  serde-skipped, they're part of the hash), so a region snapshotted before its
  first server tick ships the full dirty list to the joining client.
- The client's first live tick of that region runs
  `handle_user_changes_to_rigid_bodies` → `recompute_mass_properties_from_colliders`
  for every chunk body → `MassProperties::from_voxels` = two full passes over
  all voxel cells + a 3×3 shifted-inertia matrix per solid cell, in
  OrderedFloat/libm math (`crates/parry/src/mass_properties/mass_properties_voxels.rs:172`).
- Profile evidence: 3.0s of `from_voxels` (9 regions × ~330ms), 100% under the
  live `handle_game_event` tick path, 0 under reconcile re-sim. NOT because the
  list is lost — StepJournal restores the pre-tick `modified_bodies` clone
  (hook 1 → `revert`'s `set_modified`, step_journal.rs:142) and the per-body
  flag wipes are value-saved, so hash(before)==hash(after undo) holds. Re-sim
  was cheap in this run only because rollback never crossed the region's FIRST
  tick (it was confirmed+forgotten before the first mispredict). A rollback
  that does cross an unconfirmed first tick re-pays the full ~318ms/region on
  every replay — a join-window amplifier. The actual solver is ~0.
- The values are **discarded**: bodies are `RigidBodyType::Fixed`, so
  `update_world_mass_properties` zeroes effective inv mass/inertia right after.

Re-confirmed 2026-07-10 still unlanded: `worldgen/examples/narrow_phase_probe.rs`
measures 69.2ms of a 72.9ms first tick inside the `user_changes` stage
(mass-props) for one region (169 chunk bodies), even server-side/no serde.

Fix direction (agreed candidate, not yet implemented): skip collider mass-prop
computation for fixed bodies in the vendored fork (`attach_collider` eager path
`rigid_body_components.rs:1130` + the `handle_user_changes` recompute at
`user_changes.rs:180`), recomputing if a body ever turns dynamic (TYPE change).
Both sides share the fork so determinism/hash stay aligned; also speeds server
genesis and per-voxel-edit collider rebuilds. Do NOT fix by disabling hash
verification — see [[never-skip-hash-verification]].
