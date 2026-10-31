# Entity Seam Crossing (Part A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any entity, including a free dynamic rigid body with rotation, velocity, sleep state
and explicit mass, crosses region seams the way players do. Slimes lose their edge leash.

**Architecture:** The transfer payload becomes generic. `EntityBundle` carries a `BodyState`
(the full rapier body state, including parry `MassProperties` set explicitly and never
recomputed), a `ColliderSpec` with cuboid / ball / compound shapes, and a canonical
`Vec<Travelling>` component bag instead of one hand-written field per kind. A minimal generic
dynamic entity, `EntityKind::Body`, is the test vehicle; the tree work's voxel body builds on
it later. Ownership stays point-based (the body centre flips). The terrain margin mirror
needed by bodies wider than the seam is **Part B, a separate plan** (see "Deferred" at the end).

**Tech Stack:** Rust stable 1.96, `game` (rollback sim, rapier 0.36 / parry 0.31 forks, `libm`),
`server` (real-thread twin tests), Bevy 0.19.1 `client` (render + dev hook only).

**Spec:** `docs/superpowers/plans/2026-10-29-entity-seam-crossing-handoff.md` (read §1–§4).
Related: `docs/superpowers/specs/2026-10-29-trees-investigation.md` §5.3 and §5.7.

## Corrections to the handoff (verified against `c6abba7`)

The plan argues from these. Where they contradict the handoff, these win.

1. **Ghosts already have bodies.** `Rollback::apply_ghost` (`state.rs:1163`) builds a kinematic
   body + collider via `build_kinematic_body_safe`. The KCC query (`movement.rs:274`,
   `QueryFilter::default().exclude_rigid_body(handle)`) hits them. So one-way ghost contact
   (handoff gap 5) **exists for KCC movers**. What is missing: ghosts are teleported with
   `set_body_pose_safe`, so a rapier dynamic body touching one gets no push velocity. Ghost
   bodies are kinematic and posed, so they need no ground under them. That argument for the
   mirror in handoff gap 4 does not hold.
2. **There are no dynamic bodies in the sim.** Players, slimes, arrivals and ghosts are all
   `kinematic_position_based` (`state.rs:878`, `:1036`). Only `debug_render.rs` builds a dynamic
   one. Gaps 2, 3 and 6 need an entity to test with, hence `EntityKind::Body` (Task 3).
3. **Arrivals always wake the target** (`world_manager.rs:609`, "Arrivals ALWAYS wake the
   target"). That is the leash's stated reason (`slime.rs:69`: "wildlife is not worth a region
   genesis"). Removing the leash needs a wake policy for non-player arrivals (Task 5).
4. **Explicit mass properties are feasible as the handoff hoped.** With density-0 colliders,
   parry's `impl Add for MassProperties` (dim3, `mass_properties.rs:469`) short-circuits on a
   zero side. Rapier's recompute (`rigid_body_components.rs:451ff.`) then returns the
   additional `MassProperties` **bit-unchanged**, without `SymmetricEigen3`. So carry parry's
   `MassProperties` itself (not principal values: `inv_principal_inertia` would not round-trip
   bit-exactly through `1/x`). Only the spawner computes one, and a compound's principal frame
   needs our own deterministic eigen solver (Task 2).
5. **No golden hashes a bundle.** `wasm_determinism.rs` has no transfer; bundles reach hashes
   only through the state they create. Bundle wire-format changes need no golden re-record.
   Body/mass changes still need `just determinism-wasm`.
6. **The arrival is untimed.** The target applies it before its next tick, whatever its own
   tick count is. The source steps the body at tick T and extracts it in that tick's
   `scan_boundaries` (after the physics controller). The target never steps it before
   receiving it. **No double step is possible**, and under jitter the body only *waits* (in
   the channel) for missed target ticks. Exact equality with a no-seam reference is therefore
   impossible, because frame offsets change float rounding and the solver's warm-start cache
   does not travel. Task 4 compares against a reference within a tolerance.

## Global Constraints

- Work on `develop` in the main checkout. **No git worktrees.** `git add` only the files a task
  changed. Never commit Dawid's uncommitted work: `git status` before each commit.
- **Order with the chunk-16 plan** (`2026-10-29-chunk-16-blocks.md`, Tasks 1–10 landed or in
  flight, Task 11 flips `CHUNK_BLOCKS` 2 → 16): both touch `state.rs`. Start after its
  Task 11, or between two of its tasks with a clean `state.rs`. Never interleave hunks.
- Verify with `just test-one game "--test <target> [filter]"`, `just test-threads` after **every**
  task (seam work), `just determinism-wasm` after Tasks 2, 3 and 4, and `just smoke` before each
  commit. Never run whole suites. Client unit tests are a slow opt-in: ask first.
- Ask before any benchmark or perf capture (Dawid runs his own on this box).
- No inherent `f32` transcendental in sim code: `libm::` only. `sqrt` is IEEE-exact and allowed.
- A new test slower than 100 ms gets `#[ignore = "slow: >100ms — run via just test-full"]`;
  seam tests go in the files `just test-threads` already runs.
- Tunables are named `pub const`s (`BODY_DEFAULT_DENSITY`, tolerances, etc.), never literals.
- Cell mode is out of scope (CLAUDE.md: phase-1b cell regions don't support crossing into a
  neighbour). Cell mode reuses `EntityBundle` for dormant cells (`cells.rs:327,335`), so it must
  keep compiling and passing `--test cell_activation`.

## Review Focus

1. **A dynamic body arrives where its ghost already exists** (the normal path, since bodies cross the
   margin first). `apply_arrival` today keeps the ghost's kinematic body and only re-poses it, so
   the body would stay kinematic forever. Expected: becomes dynamic with full state (test in Task 3).
2. **Client mispredicts the crossing, then the authoritative arrival replaces it.** Expected:
   undo of a dynamic arrival on an upgraded ghost restores ghost body + mark bit-exactly (Task 3).
3. **A sleeping body near the seam.** Expected: it arrives asleep (no wake-up jolt). A body asleep
   inside the margin keeps ghosting without thrashing ownership (Task 3, Task 4).
4. **A non-player arrival targets a region that is parked, stopping or never generated.**
   Expected: no region genesis, the entity is not lost, and it appears when the region next runs (Task 5).
5. **A fast body skips the ghost stage** (moves > `GHOST_MARGIN` = 2 m in one tick: a thrown or
   falling object). Expected: the fresh-create arrival path builds the right dynamic body (Task 3).

---

### Task 1: Generic bundle (`BodyState` + component bag), players and slimes ported unchanged

**Files:**
- Modify: `crates/game/src/protocol.rs:262-307` (`ColliderSpec`, `EntityBundle`, `GhostData`)
- Modify: `crates/game/src/state.rs:1030-1320` (`build_kinematic_body_safe` → `build_body_safe`,
  `inject_body_safe`, `apply_arrival`, `collider_spec_of`, `extract_entity_safe`, `scan_boundaries`)
- Modify: `crates/game/src/world_manager.rs:609-640` (relay uses the new rebase methods)
- Modify: `crates/client/src/instance.rs:697-725` and its tests at `:2233-2480`
- Modify: `crates/game/src/slime.rs:284` (call `build_body_safe`)
- Test: `crates/game/tests/handoff_state.rs`, `handoff_scan.rs`, `handoff_protocol.rs`,
  `handoff_manager.rs` (fixture constructors)

**Interfaces:**
- Produces (in `protocol.rs`, re-exported at the crate root):
  ```rust
  pub enum BodyKind { Dynamic, KinematicPosition }
  pub struct BodyState {
      pub kind: BodyKind,
      pub pose: Pose,              // source-local until rebased
      pub linvel: Vector,
      pub angvel: Vector,
      pub activation: rapier3d::dynamics::RigidBodyActivation, // sleep + energy, bit-exact
      pub linear_damping: f32,
      pub angular_damping: f32,
      pub gravity_scale: f32,
      pub ccd: bool,
      pub locked_axes: rapier3d::dynamics::LockedAxes,
      /// Dynamic bodies only. Set as additional mass props over density-0 colliders, never recomputed.
      pub mass: Option<parry3d::mass_properties::MassProperties>,
  }
  pub enum Travelling {   // variant order IS the canonical and the apply order
      Motion(crate::Motion),
      Slime(crate::slime::Slime),
      Client(ClientId, Client),
      Camera { yaw: f32, pitch: f32 },  // requires a preceding Client
  }
  pub struct EntityBundle {
      pub kind: EntityKind, pub body: BodyState, pub collider: ColliderSpec,
      pub components: Vec<Travelling>, // sorted by variant, at most one of each
      pub source_region: RegionCoords, pub source_key: EntityKey,
  }
  impl EntityBundle { pub fn rebase(&mut self, to: RegionCoords); }      // from = source_region
  impl GhostData    { pub fn rebase(&mut self, to: RegionCoords); }
  impl BodyState    { pub fn kinematic(pose: Pose) -> Self; }  // the player/slime defaults
  ```
- Produces (in `state.rs`): `pub(crate) fn build_body_safe(&mut self, e: EntityKey, body: &BodyState, collider: &ColliderSpec) -> RigidBodyHandle`,
  `fn body_state_of(&self, handle: RigidBodyHandle) -> BodyState`.
- `GhostData` keeps its fields (`isometry`, `linvel`, `collider`); only `rebase` is added.

- [ ] **Step 1: Write the failing tests** in `handoff_state.rs`:
  - `bundle_round_trip_preserves_player_state`: extract a player (moved, mid-air `Motion`, yaw 0.7,
    pitch −0.2) from region A, `rebase` + `apply_arrival` into B, extract again from B and rebase
    back. `assert_eq!` on `body` (all fields, so `f32` bits), `components` and `collider`. Assert
    `components` is sorted with no duplicate variant.
  - `bundle_round_trip_preserves_slime_state`: the same for a slime mid-hop.
  - `rebase_moves_only_translation`: `rebase` from (0,0) to (1,0) subtracts `REGION_SIZE` from
    `pose.translation.x`. Rotation, `linvel` and `angvel` are bit-identical.
- [ ] **Step 2: Run them to see them fail to compile**: `just test-one game "--test handoff_state"`.
- [ ] **Step 3: Implement.**
  - `build_body_safe` is `build_kinematic_body_safe` with each builder field taken from
    `BodyState`. `BodyState::kinematic` must reproduce today's chain exactly: gravity 0,
    rotations enabled, no CCD, angular damping 1.0, `can_sleep(false)`.
  - Dynamic bodies are built in Task 3. Here `build_body_safe` may `unimplemented!()` for
    `BodyKind::Dynamic`.
  - `extract_entity_safe` gathers `Travelling` in variant order from `ecs.motion`, `ecs.slime`,
    `player_entites`/`clients` and `ecs.camera`. `apply_arrival` walks `components` once, in
    order.
  - Replace both `rebase_isometry` call sites with `rebase`. Keep `rebase_isometry` as the one
    implementation `rebase` calls.
  - Move test fixtures to `EntityBundle { body: BodyState::kinematic(pose), .. }`.
- [ ] **Step 4: Run** `just test-one game "--test handoff_state"`, then `--test handoff_scan`,
  `--test handoff_protocol`, `--test handoff_manager` and `--test cell_activation`. All pass.
- [ ] **Step 5: Run** `just test-threads` (seeded walks + real threads, behaviour unchanged) and
  `just smoke` (type-checks the client).
- [ ] **Step 6: Commit** `refactor(game): generic entity bundle — body state + component bag`.

### Task 2: Collider shapes and deterministic explicit mass properties

**Files:**
- Create: `crates/game/src/mass_props.rs`
- Modify: `crates/game/src/protocol.rs` (`ColliderSpec` variants), `crates/game/src/state.rs`
  (`build_body_safe` collider arm, `collider_spec_of` read-back), `crates/game/src/lib.rs` (module)
- Test: `crates/game/tests/mass_props.rs` (new), `crates/game/tests/wasm_determinism.rs`

**Interfaces:**
- Consumes: `build_body_safe`, `ColliderSpec` (Task 1).
- Produces:
  ```rust
  pub enum ColliderSpec {
      CapsuleY { half_height: f32, radius: f32 },
      Cuboid { half_extents: Vector },
      Ball { radius: f32 },
      /// Primitive parts only (no nested compounds), in the order given.
      Compound(Vec<(Pose, ColliderSpec)>),
  }
  // mass_props.rs
  pub const BODY_DEFAULT_DENSITY: f32 = /* kg per unit³; 1 m = 16 units — pick wood-like ~600 kg/m³ */;
  pub fn mass_properties(spec: &ColliderSpec, density: f32) -> MassProperties;
  pub fn jacobi_eigen_sym3(m: [[f32; 3]; 3]) -> (Vector, Rotation); // principal values, right-handed frame
  ```
  `mass_properties` computes each primitive analytically with `libm` (cuboid
  `m/12·(b²+c²)…`, ball `2/5·m·r²`, capsule as parry does). For a compound it sums the parts'
  tensors with the parallel-axis shift in **part order**, then calls `jacobi_eigen_sym3` and
  `MassProperties::with_principal_inertia_frame`. It **never** calls
  `MassProperties::with_inertia_matrix` or `Add` on two non-zero props, since those go through
  glamx `SymmetricEigen3`.
  `jacobi_eigen_sym3` uses cyclic Jacobi with a fixed sweep count (`JACOBI_SWEEPS`, e.g. 8) in a
  fixed (p, q) order, using only `+ − × ÷` and `libm::sqrtf`. If the eigenvector matrix has
  det < 0, negate column 2, then convert with `Rotation::from_mat3`.
  Colliders of a dynamic body are built with `density(0.0)`. Kinematic bodies keep today's
  default density, so their behaviour is unchanged.

- [ ] **Step 1: Write the failing tests** in `tests/mass_props.rs`:
  - `cuboid_and_ball_match_parry`: `mass_properties` vs parry's own
    `Cuboid::new(h).mass_properties(d)` / `Ball`: mass and principal inertia equal within 1e-5 relative.
  - `compound_matches_parry_within_tolerance`: an L-shape of two cuboids, one rotated 30° about Y.
    Mass, COM and the reconstructed inertia tensor match parry's compound result within 1e-4 relative.
  - `jacobi_is_orthonormal_and_right_handed`: for three fixed symmetric matrices, `R·diag·Rᵀ` reproduces
    the input within 1e-5, and det(R) = +1.
  - `explicit_mass_survives_insertion_bit_exact`: build a dynamic body over a density-0 compound
    collider with `additional_mass_properties(mp)`, step once. `body.mass_properties().local_mprops == mp`
    (bitwise, via `StateHash` or `to_bits` on every field).
  - `collider_spec_read_back_round_trips`: `collider_spec_of` returns the spec each variant was built from.
- [ ] **Step 2: Add a wasm determinism scene** in `wasm_determinism.rs`:
  `explicit_mass_compound_is_cross_target`. Pin the crc32 of `mass_properties(L-shape)` field bits
  on both targets (add the expected constant after the first native run, and say so in the
  commit message).
- [ ] **Step 3: Run them to see them fail**: `just test-one game "--test mass_props"`.
- [ ] **Step 4: Implement** per the Interfaces block.
- [ ] **Step 5: Run** `just test-one game "--test mass_props"`, `just determinism-wasm`,
  `just test-one game "--test physics_goldens"` (must stay green unchanged), `just test-threads`,
  `just smoke`.
- [ ] **Step 6: Commit** `feat(game): cuboid/ball/compound collider specs + deterministic explicit mass props`.

### Task 3: `EntityKind::Body`: a dynamic body that crosses, ghosts and rolls back

**Files:**
- Create: `crates/game/src/body.rs` (spawn + kind-specific bits; the tree's `voxel_body.rs`
  later builds on it)
- Modify: `crates/game/src/state.rs` (`EntityKind::Body`, `build_body_safe` dynamic arm,
  `apply_arrival` upgrade path, `apply_ghost` posing)
- Modify: `crates/client/src/renderer/avatar.rs:34`, `crates/client/src/renderer/slint_overlay.rs:325`
  (exhaustive `match` arms only: a placeholder that compiles. Real rendering is Task 6.)
- Test: `crates/game/tests/handoff_state.rs`, `crates/game/tests/body.rs` (new)

**Interfaces:**
- Consumes: `BodyState`, `build_body_safe` (Task 1), `mass_properties`, `ColliderSpec` (Task 2).
- Produces:
  ```rust
  pub enum EntityKind { Player, Slime { size: u8 }, Body }  // Body appended: keep discriminants
  // body.rs
  impl Rollback {
      pub fn spawn_body_safe(&mut self, pose: Pose, collider: ColliderSpec, linvel: Vector, angvel: Vector) -> EntityKey;
      /// Removes only the body + colliders of `e` (undo-tracked), never the entity: no
      /// RemoveEntity/CreateEntity is emitted, so the bridge keeps the same render entity.
      fn remove_body_safe(&mut self, e: EntityKey);
  }
  pub const BODY_SETTLE_TICKS: usize = /* 3 s of ticks, from TICK_RATE */;
  ```
  `spawn_body_safe` builds `BodyState { kind: Dynamic, mass: Some(mass_properties(&collider, BODY_DEFAULT_DENSITY)), ccd: false, gravity_scale: 1.0, .. }`,
  sets the kind and emits `SetEntityKind` / `SetEntityPosition`, mirroring `spawn_slime_safe`
  (`slime.rs:281`).
- **Arrival rule:** when a ghost body exists for the identity and the bundle's `body.kind` differs
  from the ghost's body type (always for `Dynamic`), remove the ghost's body + colliders
  with `remove_body_safe` and `build_body_safe` fresh **on the same
  `EntityKey`**. Otherwise keep today's re-pose path. Never call `set_body_type` on a live body:
  the fork's type-change recompute is exactly the mass path we avoid.
- **Ghost posing:** `apply_ghost` refreshes a ghost with `set_next_kinematic_position` instead of a
  teleport, so rapier gives touching dynamic bodies a contact velocity (one-way). Ghost bodies
  stay `BodyState::kinematic` with the shape from `GhostData.collider`.

- [ ] **Step 1: Write the failing tests.**
  - `body.rs::body_falls_and_sleeps`: spawn a 1 m cuboid 3 m above a flat floor; within
    `BODY_SETTLE_TICKS` it is asleep resting on the floor (y within 1 cm).
  - `body.rs::rollback_mid_fall_is_bit_exact`: `state_hash` before 10 ticks of tumbling == after
    undoing them (under `HashMode::Full`).
  - `handoff_state.rs::dynamic_arrival_upgrades_ghost_to_dynamic`: ghost then arrival with
    `BodyKind::Dynamic`. Same `EntityKey`; body `is_dynamic()`; linvel, angvel and
    `local_mprops` bit-equal to the bundle's.
  - `handoff_state.rs::dynamic_arrival_without_ghost` (fast body, Review Focus 5): fresh create
    with the same assertions.
  - `handoff_state.rs::undo_dynamic_upgrade_restores_ghost`: undo the upgrade transaction.
    `state_hash` equals the pre-arrival hash; the ghost record and its kinematic body are back.
  - `handoff_state.rs::sleeping_body_arrives_asleep`: the bundle from an asleep body yields an
    asleep body (`is_sleeping()`), with `activation` bit-equal.
  - `handoff_state.rs::ghost_pushes_dynamic_body_one_way`: a kinematic ghost refreshed toward a
    resting dynamic cuboid moves it (displacement > 0 after N ticks). The owner side is
    untouched (no event goes back).
- [ ] **Step 2: Run them to see them fail**: `just test-one game "--test body"`, then `--test handoff_state`.
- [ ] **Step 3: Implement** per the Interfaces block. `body_state_of` reads `mass` from
  `body.mass_properties().local_mprops` only for dynamic bodies.
- [ ] **Step 4: Run** both targets, `--test handoff_scan`, `just determinism-wasm`,
  `just test-threads`, `just smoke`.
- [ ] **Step 5: Commit** `feat(game): generic dynamic Body entity crosses seams with full body state`.

### Task 4: Seeded and threaded seam tests for a tumbling body

**Files:**
- Modify: `crates/game/tests/handoff_schedule.rs` (new routes for a body; reuse
  `seam_floor_columns`, `sweep`, the rig)
- Modify: `crates/server/tests/threaded_world.rs` (one real-thread twin)

**Interfaces:**
- Consumes: `spawn_body_safe` (Task 3). The rig reaches regions through
  `manager.spawner_mut().with_region(rc, |region| …)` (`world_manager.rs:257`), but `Region` only
  exposes `data(&self)` (`region.rs:660`), and `Order` derives `Eq`, so it can't carry `f32`
  launch vectors. Add `#[doc(hidden)] pub fn Region::dev_spawn_body(&mut self, pose: Pose, collider: ColliderSpec, linvel: Vector, angvel: Vector) -> EntityKey`.
  It opens a transaction the way `handle_event` does (`region.rs:479`) and calls
  `spawn_body_safe`. It is server/test-only, never sent over the wire. No test-only `GameEventKind`.
- Tolerances as named consts at the top of the file: `BODY_REST_POS_TOL` (start at 0.02 m =
  0.32 units) and `BODY_REST_ANGLE_TOL` (start at 1°). Tighten to the measured spread × 2 and
  record the measured values in a comment.

- [ ] **Step 1: Write the tests.**
  - `body_slides_across_seam_lockstep`: a 1 m cuboid launched on the floor toward the north seam
    with `linvel` 6 m/s and `angvel` about Y. It must end owned by `north()`, asleep. Its
    world-frame rest pose is within tolerance of a **reference run**: the same launch, rotated to
    travel parallel to the seam inside the spawn region, so it never crosses. Compare
    displacement and yaw change, not absolute pose.
  - `body_corner_crossing_lockstep`: the same through the −x/−z corner; ends owned by `north_west()`.
  - Extend `sweep` (or add `sweep_body`) so `handoff_survives_permuted_tick_order`,
    `…_late_inputs` and `…_missed_ticks` also run the body routes. Per round, assert the body is
    owned by **at most one** region (the existing `owners()` check, generalised to an
    `EntityKind`). At the end, assert the per-seed fingerprint is reproducible, the body is
    asleep in the target, and the rest pose is within tolerance of the lockstep reference.
  - `threaded_world.rs::body_crosses_seam_on_real_threads`: one launch, real region threads. It
    ends owned by the neighbour, asleep, and was never seen in two regions at once.
- [ ] **Step 2: Run** `just test-threads`. If a jitter seed fails, the failure names the seed:
  fix the cause in `game`, never the tolerance, unless the spread is explainable (a missed
  target tick only delays the body). Write that reasoning into the const's comment.
- [ ] **Step 3: Run** `just determinism-wasm` and `just smoke`.
- [ ] **Step 4: Commit** `test(game): tumbling body crosses seams under seeded jitter and real threads`.

### Task 5: Non-player arrivals don't wake regions (manager mailbox)

**Assumption (Q2 below):** players still wake their target. Any other arrival whose target is not
running (absent, parked or `stopping`) waits in a manager-side mailbox. It is delivered, in
arrival order, as ordinary `EntityArrived` events right after the target next becomes running.
The mailbox is memory-only, like parking today. On shutdown its contents are lost, the same as
running regions' state.

**Files:**
- Modify: `crates/game/src/world_manager.rs:609-625` (`Departures` arm), and the point where a
  region becomes running (`ensure_running` / restore path)
- Test: `crates/game/tests/handoff_manager.rs`

**Interfaces:**
- Produces: `WorldManager { pending_arrivals: BTreeMap<RegionCoords, Vec<EntityBundle>> }` (private),
  `pub fn pending_arrival_count(&self, rc: RegionCoords) -> usize` (for tests and the `[server]`
  counters line).

- [ ] **Step 1: Write the failing tests** in `handoff_manager.rs`:
  - `body_arrival_does_not_wake_unloaded_region`: a body departs toward a never-started region.
    The spawner's started set does not contain it, and `pending_arrival_count == 1`.
  - `mailbox_delivers_when_region_starts`: subscribe a client to that region. After it runs, the
    body exists there exactly once and the mailbox is empty.
  - `mailbox_survives_stopping_region`: the target is `stopping` when the body departs; it is
    delivered after the resubscribe cycle.
  - `player_arrival_still_wakes`: the unchanged behaviour, pinned.
- [ ] **Step 2: Run them to see them fail**: `just test-one game "--test handoff_manager"`.
- [ ] **Step 3: Implement.** Add `pending_arrivals` to the `[server]` periodic counters line
  (`crates/server/src/perf.rs`) as one number: never log per entity.
- [ ] **Step 4: Run** the target, `just test-threads`, `just smoke`.
- [ ] **Step 5: Commit** `feat(game): non-player arrivals wait for their region instead of waking it`.

### Task 6: Slimes roam across seams; render bodies; live check

**Files:**
- Modify: `crates/game/src/slime.rs:63-75,135-160` (delete `SLIME_EDGE_MARGIN` and `inward_yaw`;
  rewrite the module doc's "turns back before it reaches a region edge")
- Modify: `crates/game/src/orders.rs:125` (add `Order::DevDropBody { at: TilePos }`: spawns a 1 m
  cuboid 4 m above the tile with a small random `angvel` from `roll`. Dev-only, like `DevCompleteAll`)
- Modify: `crates/client/src/renderer/avatar.rs` (`EntityKind::Body`: one box per cuboid of its
  collider. Shape comes from a new `GameDataUpdateKind` only if the kind alone can't carry it.
  For Part A the dev body is always the 1 m cuboid, so `Body` renders a 1 m box, and a shape
  update is left to the tree work)
- Modify: `crates/client/src/renderer/dev_ui.rs:161` (a "Drop body at cursor" button, using
  `GroundPick`)
- Test: `crates/game/tests/slime.rs`, `crates/game/tests/handoff_schedule.rs`

- [ ] **Step 1: Write the failing test** `handoff_schedule.rs::slime_wanders_across_seam`:
  spawn a slime 1 m from the north seam with its heading pinned north. Under lockstep and the
  `jittery` sweep it ends owned by `north()` with its `Slime` component intact (bit-equal except
  fields the hop updated). Also run `just test-one game "--test slime"`, which must be adjusted
  where it asserts the leash (delete those assertions; don't weaken others).
- [ ] **Step 2: Implement** the leash removal, `DevDropBody`, and the client render + button.
- [ ] **Step 3: Run** `just test-one game "--test slime"`, `just test-threads`, `just smoke`.
- [ ] **Step 4: Live check** (functional, not perf; `adapter=Intel` per memory), one client:
  `just adapter=Intel world=small run`. Drop bodies on a slope toward the home region edge.
  Confirm each crosses without a pop or a frozen frame and tumbles on in the neighbour. Watch
  slimes drift across the seam. The orientation must interpolate across the region-local frame
  change. Write what was seen (and anything not checked) into the commit message.
- [ ] **Step 5: Commit** `feat: slimes cross seams; dev-droppable dynamic bodies`.
- [ ] **Step 6: Docs.** Update the CLAUDE.md `crates/game` paragraph: slimes are no longer
  leashed; `EntityKind::Body`; the mailbox rule; `Order::DevDropBody`. Mark the handoff
  "Part A done", with a link to this plan. Separate commit, `git add` only those hunks.

### Task 7: Creature colliders follow their `.bbmodel` (rest pose, from the manifest)

Asked by Dawid after the plan was drafted: "could you make colliders follow bbmodels?"

**Why offline, not parsed in the sim.** `crates/bbmodel` rotates cubes with std `sin`/`cos`
(`lib.rs:696`, a native-vs-wasm hazard) and decodes PNG textures. The sim must not load it.
`tools/creatures/build.py` already computes every cube corner through its bones' rest rotations
(`rest_points`, used by `rest_bounds` for the manifest's `bounds`). It writes the collider as
plain decimal numbers into `assets/models/creatures/creatures.json`. `game` embeds that file
with `include_str!` and parses it once (`serde_json` is already a `game` dependency). Server and
client parse the same text, so the shapes are bit-identical, and no transcendental runs in the sim.

**Two shapes per creature** (units = world units; 1 Blockbench px = 1 unit, so no scaling;
feet on y = 0, facing −Z):
- `move`: one upright `CapsuleY` for the character controller. The KCC sweeps a single
  yaw-free shape (`movement.rs:282`, `Pose::from_translation`). Radius = the smaller horizontal
  half-extent of `bounds`, clamped to `[CREATURE_MIN_RADIUS, CREATURE_MAX_RADIUS]`. Half-height
  = (height / 2 − radius), floored at 0. Long animals overhang their capsule (an aardvark's snout
  and tail). That is accepted: fortress movement is tile-scale. The alternative, a yaw-rotated
  footprint, changes the KCC sweep and is out of scope.
- `body`: a `Compound` of one cuboid per bone: the axis-aligned box of that bone's rest-pose
  cube corners, in model space, in bone order, with bones that have no cubes skipped. It is used
  when the creature becomes a dynamic `Body` (a corpse, a thrown animal) and later for
  hit/selection. Mass comes from `mass_properties(&body, CREATURE_DENSITY)` (Task 2), so it is
  never recomputed by rapier.

Animation does **not** move sim colliders: that would need sim-side clip playback (Molang,
keyframes, trig) as rollback state per creature. Client-side picking may use animated meshes on
its own.

**Coordination.** Another session is building creatures (`tools/creatures/src/*.toml` and
`creatures.json` are uncommitted and moving). Change `build.py` only with that work committed or
paused. Regenerate the manifest with `just creatures` (all ids), never by hand.

**Files:**
- Modify: `tools/creatures/build.py:538-565` (`manifest_entry` gains `"collider": {"move": …, "body": […]}`)
- Modify: `crates/bbmodel/tests/creatures.rs` (lint the new field)
- Create: `crates/game/src/creature_shapes.rs`
- Test: `crates/game/tests/creature_shapes.rs` (new)

**Interfaces:**
- Consumes: `ColliderSpec::{CapsuleY, Compound, Cuboid}`, `mass_properties` (Task 2).
- Produces:
  ```rust
  pub struct CreatureShape { pub move_shape: ColliderSpec, pub body: ColliderSpec }
  pub fn creature_shape(df_id: &str) -> Option<&'static CreatureShape>; // OnceLock<BTreeMap<String, CreatureShape>>
  pub const CREATURE_MIN_RADIUS: f32 = /* 2.0 units */;
  pub const CREATURE_MAX_RADIUS: f32 = /* 12.0 units */;
  pub const CREATURE_DENSITY: f32 = /* ~1000 kg/m³, in BODY_DEFAULT_DENSITY's units */;
  ```
  Manifest JSON shape per creature:
  `"collider": {"move": {"radius": r, "half_height": h}, "body": [{"min": [x,y,z], "max": [x,y,z]}, …]}`.
  Each `body` box becomes `(Pose::from_translation(centre), Cuboid { half_extents })`.

- [ ] **Step 1: Write the failing tests** in `crates/game/tests/creature_shapes.rs`:
  - `every_manifest_creature_has_shapes`: every key in `creatures.json` resolves;
    `move_shape` is a `CapsuleY` with radius inside the clamp; `body` is a non-empty `Compound`.
  - `body_parts_stay_inside_bounds`: the union AABB of every `body` part lies within the
    manifest `bounds` (within 1e-3 units).
  - `move_capsule_fits_height`: for `dwarf`, `dog` and `aardvark`, capsule top
    (`half_height + radius` above its centre at height/2) equals the bounds height within 1e-3.
  - `shapes_are_cross_target`: in `wasm_determinism.rs`, the crc32 of `mass_properties(&creature_shape("dog").body, CREATURE_DENSITY)` bits is pinned on both targets.
- [ ] **Step 2: Extend** `crates/bbmodel/tests/creatures.rs`: every manifest entry has
  `collider`, and its `body` count ≤ `bones`. Run `just creature-check`; it fails.
- [ ] **Step 3: Implement** the `build.py` field (reuse `rest_points`, grouped by bone), run
  `just creatures`, then `creature_shapes.rs`.
- [ ] **Step 4: Run** `just creature-check`, `just test-one game "--test creature_shapes"`,
  `just determinism-wasm`, `just smoke`.
- [ ] **Step 5: Commit** `feat: creature colliders from their bbmodel rest pose (manifest)`.
  `git add` only `build.py`, the lint, the `game` files and the regenerated `creatures.json`,
  and only if the other session's manifest changes are already committed (ask otherwise).

Wiring a creature entity to `creature_shape` (spawning a dog with `move_shape` instead of a
slime capsule) waits for the first DF creature entity kind, which this plan does not add.

---

## Deferred: Part B, terrain margin mirror (own plan, after Q1)

Point ownership leaves a large body (a 20 m log) falling through nothing on the side of the
seam its owner has no terrain for. Part B gives each region a read-only strip of each
neighbour's terrain colliders. Decisions that plan must make (not decided here):

- **Width** R (Q1). With 16 m chunks that's 1–2 chunk rows per edge.
- **Source:** the strip at region start comes from the same `TerrainSnapshot` sourcing the LOD
  builder uses (running / parked / worldgen). Edits arrive as **manager-authored region events**
  (like `GhostUpdate`, never client-injectable, dropped at the same filter in
  `world_manager.rs:519`). They therefore sit in the receiver's event log and replay identically
  on clients, which gives the deterministic "stamped" invalidation the handoff asks for, with no
  separate tick protocol.
- **Implied ground** (`ColumnGround`) inside the strip, and how the edit ray treats mirror
  chunks (never editable from the mirror side).
- **Cost:** collider count and memory per region edge, measured at the 256 m play size.

Until Part B lands, the trees work keeps its fallback: steer the fall so the reach stays inside
the home region.

## Open questions for Dawid (the plan assumes the first option of each)

1. **Mirror width (Part B only).** *Cap entity size to a small mirror (16 m):* cheap and simple,
   but trees taller than ~16 m must split or steer their fall. *Size it to trees (~32 m):*
   twice the collider strip per edge and twice the edit relay traffic, but no tree special case.
   Nothing in Part A depends on this.
2. **Waking regions** (Task 5). *Mailbox (assumed):* wildlife and debris never cause a region
   genesis, but a body that slides into an unloaded region is frozen in time until someone
   loads it. *Always wake (today's rule):* physically continuous, but a slime herd on the edge
   of the explored area keeps spawning regions (seconds of genesis each at 256 m) and keeps them
   alive.
3. **Ghost contact direction.** *One-way (assumed; mostly exists already):* a ghost pushes the
   receiving region's bodies and nothing pushes back, so two dwarves on opposite sides can't
   jointly shove a log. *Two-way:* needs cross-region solving and breaks "a region is the
   threading unit". It is not in this plan at all.
4. **Voxel-model collider by id.** The handoff wants big shapes sent once and referenced by id in
   ghost updates. Part A sends the whole `ColliderSpec` in every ghost update. For cuboid
   compounds that is ~40 bytes a part, and the voxel model doesn't exist yet. Assumed: add the
   by-id path in the tree work once `VoxelModel` exists and a measurement shows the ghost
   traffic matters.
5. **Cell mode:** later (assumed). Phase 1b can't cross into a neighbour at all.
