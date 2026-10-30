# Handoff — any entity crosses region seams, like players do

**Date:** 2026-10-29 · **Status:** not started · **Asked by:** Dawid: "entities need to be able
to travel across seams like players (from a technical standpoint). This should not be done in
tree work, it should be its own handoff."
**Motivating case:** a felled tree (a dynamic rigid body, tens of metres long, see
`docs/superpowers/specs/2026-10-29-trees-investigation.md` §6) falling across a
seam. Also slimes (today leashed away from edges), items, later dwarves, animals and projectiles.

Work on `develop` in the main checkout (no worktrees). Seam work is threading-sensitive. Put
correctness in the seeded layer (`crates/game/tests/handoff_schedule.rs`, `game::InlineSchedule`)
and run `just test-threads` after every step. Run `just determinism-wasm` if anything hashed or
serialized changes. Do it after the chunk-size change
(`2026-10-29-chunk-size-increase-handoff.md`) if both are in flight, since both touch
`state.rs` and the region code.

---

## 1. What exists (as of `548f7f1`)

- **The ownership flip is point-based.** `Rollback::scan_boundaries` (`game/src/state.rs:1333`)
  runs after the step. For every entity with a `kind` (terrain is kindless), the *body
  translation* is tested with `departure_offset` (`protocol.rs:202`). Once it is outside the
  region, `extract_entity_safe` produces an `EntityBundle`, and the `WorldManager` relays it as
  `GameEventKind::EntityArrived` (`world_manager.rs:609`). Within `GHOST_MARGIN` (= 32 units =
  **2 m**, `protocol.rs:191`) of an edge, a `GhostData` goes to the neighbour each tick
  (`ghost_offsets` :220, TTL `GHOST_TTL_TICKS` = 25). A ghost upgrades in place when its
  arrival lands. Input follows players through the manager's `homes` map.
- **`EntityBundle`** (`protocol.rs:254-272`) has **one hand-written field per kind**: `motion`,
  `slime`, `client`, `yaw` / `pitch`, plus `linvel` only (no angular velocity, no sleep state,
  no body type).
- **`ColliderSpec`** (`protocol.rs:245`) has **only `CapsuleY`**.
- **Ghosts have no body.** "Stage 2" (a ghost with a collider so neighbours physically
  interact) is not built (`state.rs:1129` comment, `GhostData.collider` "rides along for
  stage 2").
- **Slimes** are leashed away from edges (`slime.rs:5,69,142`) because crossing wasn't trusted
  for them.
- **Tests:** seam / corner walks for players under seeded jitter (`handoff_schedule.rs`) and on
  real threads (`server/tests/threaded_world.rs`), plus the client twin
  `crossing_converges_under_late_region_scheduling` (`client/tests/crossing.rs`).

## 2. Gaps for "any entity"

1. **Generic payload.** Each new entity kind would add a field to `EntityBundle` and to
   extract / insert. Replace that with a **component bag**: every rollback component that
   travels with the entity is serialized by one registry (kind → list of component
   (de)serializers), so a new kind registers its components once. Keep the bundle canonical
   (fixed component order) since it is hashed via the arrival event.
2. **Full body state.** Add angular velocity, body type (dynamic / kinematic), sleep /
   activation state, CCD flag, damping, gravity scale, and **explicit mass properties**.
   Dynamic voxel or compound shapes must not hit the fork's eigen-decomposition path (std
   `acos` / `cos`). Fixed bodies skip mass props today for that reason. So the sender carries
   mass / COM / inertia, and the receiver sets them, never recomputes.
3. **Collider shapes.** `ColliderSpec` needs at least cuboid, ball, compound (list of
   `(Pose, primitive)`) and a **voxel model reference** (a falling tree's blocks, items).
   A big voxel shape should travel as its block data once, not per ghost tick. Ghost updates
   carry pose and velocity only, referencing the collider by an id sent once.
4. **Large entities and ownership.** Point ownership is fine for a 2 m dwarf but wrong for a
   20 m log whose centre is on one side while its crown lies across the seam. The source sim
   has **no terrain past the seam**, so that part falls through nothing. The options:
   - **(a) Terrain margin mirror**: each region keeps a read-only strip of each neighbour's
     terrain colliders `R` metres wide (R ≥ the largest entity's reach, ~16–32 m; with 16 m
     chunks that's 1–2 chunk rows). The strip is refreshed by the existing `TerrainChanged`
     pushes. The owner then simulates the whole body correctly; the flip still happens at
     the centre. Costs memory and collider count per region edge, and needs edit
     invalidation latency rules (a neighbour's edit becomes visible one relay later; that has
     to be deterministic, i.e. stamped with a tick).
   - **(b) Early handoff by extent**: transfer when the body's AABB crosses, not its centre.
     It still fails for an entity wider than the margin on both sides.
   - Recommendation: **(a)**. It is also what stage-2 ghost interaction needs (a ghost body
     needs ground to stand on).
5. **Ghost bodies (stage 2).** Neighbours should physically see a body near the seam: a
   kinematic body driven by `GhostData` poses. Otherwise a falling tree passes through a
   dwarf standing just over the line. Decide whether ghost contacts may affect the owner
   (two-way would require cross-region solving, so don't) or only the receiving region's own
   bodies (one-way, latency one relay). Recommend one-way.
6. **Determinism of the flip** under jitter. Arrival must be idempotent (exists:
   `source_region` / `source_key` token). A dynamic body must not be simulated by both
   regions for the same tick (double step) or by neither (lost tick). The seeded layer
   already exercises late or missed ticks for players; extend it to bodies with velocity
   and rotation.
7. **Client.** The bridge must re-parent the render entity across a home flip without a pop
   (players already do this). For non-player entities, ghost / owned dedup via
   `SetGhostSource` must hold for dynamic bodies and voxel-model entities. Interpolation of
   rotation across the region-local frame change must hold too.
8. **Cell mode.** Phase-1b cell regions don't support walking into a neighbour region at all
   (CLAUDE.md). Keep this handoff to legacy / fully materialised regions, or make cell support
   an explicit later step.

## 3. Suggested order

1. **Component-bag bundle + full body state + explicit mass props.** Port player and slime
   onto it with behaviour identical. Existing handoff tests stay green, and the wasm goldens
   change only if the bundle's wire bytes are hashed (re-record with a note).
2. **Collider shapes** (cuboid / ball / compound / voxel-model-by-id).
3. **Seeded tests for a dynamic body** crossing a seam and a corner, with rotation and
   velocity: hash and trajectory equal to a single-region reference run where the seam is
   absent (build a 2×1 world vs one double-width reference if the tooling allows; otherwise
   pin end-state goldens).
4. **Terrain margin mirror (gap 4a).** A large body lying across a seam rests on the
   neighbour's ground.
5. **Ghost kinematic bodies (gap 5, one-way).**
6. **Remove the slime edge leash** as the acceptance test: slimes wander across seams in
   `just test-threads` and a live `just run` with `world=small`.
7. Then the tree work can drop its "steer the fall inside the region" fallback.

## 4. Open questions for Dawid

- Margin mirror width: tie it to the largest entity (trees ~32 m) or keep it small and cap
  entity size?
- Should ghosts be physical one-way (recommended), or is two-way contact across seams ever
  needed (e.g. two dwarves pushing on a log from both sides)?
- Cell-mode support in scope now or later?
