# Entity/Region Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Entities (players included) transfer between region sims via deterministic in-tick extraction + explicit arrival events, with ghost mirrors in boundary margins and upgrade-in-place ownership flips — fully predicted on the client.

**Architecture:** The boundary/margin scan runs inside `Tick` processing (deterministic, undo-tracked, replayable). New `RegionOutput::{Departures, GhostUpdates}` carry results to the manager, which rebases translations and feeds `GameEventKind::{EntityArrived, GhostUpdate}` into target regions (waking them for arrivals, dropping ghost updates to cold regions). The client synthesizes the same events into its local sibling regions after each predicted tick; per-region reconcile corrects both sides.

**Tech Stack:** Rust workspace; crossbeam channels; bincode; crc32fast; Bevy 0.18 (client only).

**Spec:** `docs/superpowers/specs/2026-07-05-entity-region-handoff-design.md` — read it before starting any task.

## Global Constraints

- `cargo build --workspace --bins` (stable) must pass at the end of every task.
- `game`, `server`, `worldgen` stay Bevy-free; tokio confined to networking edges.
- Vendored forks must not be modified. `crates/macros` is first-party and MAY gain undo primitives (Task 2) — with hash-restore tests.
- Constants (exact): `GHOST_MARGIN: f32 = 32.0`, `FLIP_HYSTERESIS: f32 = 2.0`, `GHOST_TTL_TICKS: usize = 25`. `REGION_SIZE = 256.0`, `TICK_RATE = 50` unchanged.
- Rollback bar everywhere: `hash(before) == hash(after undo)`, bit-exact, for every new mutation (extraction, arrival, ghost upsert/expiry). Never weaken a hash assertion to pass a test.
- Ghost updates NEVER wake a cold region; arrivals ALWAYS do (`ensure_running`).
- Wire format is bincode; no compatibility to preserve (state/wire shape may change).
- Stage 1 = Tasks 1–8 (ghosts render-only, no colliders). Stage 2 = Task 9 (ghost colliders).
- Both transports keep working; `cargo test -p server --test webtransport_handshake` must keep passing.
- Locate code by SYMBOL, not line number — the tree moves.
- Commit after every task with trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: Protocol layer — constants, boundary math, transfer payloads, event kinds

Pure types + pure functions. No sim behavior yet.

**Files:**
- Modify: `crates/game/src/protocol.rs` (constants, `ColliderSpec`, `EntityBundle`, `GhostData`, boundary math, rebase, new `GameEventKind` variants, `matches_prediction`)
- Modify: `crates/game/src/state.rs` (add `PartialEq` to `Client`; add `GameDataUpdateKind::SetGhostSource`)
- Modify: `crates/game/src/input.rs` (derive `PartialEq` on `InputState` and anything it contains that lacks it — compiler chases)
- Modify: `crates/client/src/renderer/bridge.rs` (exhaustive-match stub arm for `SetGhostSource`)
- Test: `crates/game/tests/handoff_protocol.rs` (new)

**Interfaces (produced, all in `game`, re-exported at crate root):**

```rust
pub const GHOST_MARGIN: f32 = 32.0;
pub const FLIP_HYSTERESIS: f32 = 2.0;
pub const GHOST_TTL_TICKS: usize = 25;

pub enum ColliderSpec { CapsuleY { half_height: f32, radius: f32 } }

pub struct EntityBundle {
    pub kind: EntityKind,
    pub isometry: IsometryReal,
    pub linvel: parry3d::math::Vector<Real>,
    pub collider: ColliderSpec,
    pub has_camera: bool,
    /// Client-attached entities carry their input state so held keys survive the flip.
    pub client: Option<(ClientId, Client)>,
    pub source_region: RegionCoords,
    pub source_key: EntityKey,
}

pub struct GhostData {
    pub source_region: RegionCoords,
    pub source_key: EntityKey,
    pub kind: EntityKind,
    pub isometry: IsometryReal,
    pub linvel: parry3d::math::Vector<Real>,
    pub collider: ColliderSpec, // unused until stage 2; on the wire from day one
}

pub fn departure_offset(x: f32, z: f32) -> Option<(i32, i32)>;
pub fn ghost_offsets(x: f32, z: f32) -> Vec<(i32, i32)>;
pub fn rebase_isometry(iso: &IsometryReal, from: RegionCoords, to: RegionCoords) -> IsometryReal;

// GameEventKind gains: EntityArrived(EntityBundle), GhostUpdate(GhostData)
// GameEventKind::matches_prediction(&self, &Self) -> bool
// GameDataUpdateKind gains: SetGhostSource(EntityKey, Option<RegionCoords>)
```

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/handoff_protocol.rs`:

```rust
use game::{
    departure_offset, ghost_offsets, rebase_isometry, IsometryReal, RegionCoords,
    FLIP_HYSTERESIS, GHOST_MARGIN, REGION_SIZE,
};

#[test]
fn departure_needs_hysteresis() {
    // Inside, on the line, and within the hysteresis band: no flip.
    assert_eq!(departure_offset(128.0, 128.0), None);
    assert_eq!(departure_offset(-0.0, 10.0), None);
    assert_eq!(departure_offset(-FLIP_HYSTERESIS, 10.0), None);
    assert_eq!(departure_offset(REGION_SIZE + FLIP_HYSTERESIS, 10.0), None);
    // Past the band: flip toward the right neighbour.
    assert_eq!(departure_offset(-FLIP_HYSTERESIS - 0.1, 10.0), Some((-1, 0)));
    assert_eq!(departure_offset(REGION_SIZE + FLIP_HYSTERESIS + 0.1, 10.0), Some((1, 0)));
    assert_eq!(departure_offset(10.0, -3.0), Some((0, -1)));
    // Corner: diagonal neighbour.
    assert_eq!(departure_offset(-3.0, 259.0), Some((-1, 1)));
}

#[test]
fn ghost_offsets_cover_edges_and_corners() {
    assert!(ghost_offsets(128.0, 128.0).is_empty());
    assert_eq!(ghost_offsets(GHOST_MARGIN - 1.0, 128.0), vec![(-1, 0)]);
    assert_eq!(ghost_offsets(REGION_SIZE - GHOST_MARGIN + 1.0, 128.0), vec![(1, 0)]);
    assert_eq!(ghost_offsets(128.0, 10.0), vec![(0, -1)]);
    // Corner mirrors into 3 neighbours.
    let corner = ghost_offsets(10.0, 10.0);
    assert_eq!(corner.len(), 3);
    for o in [(-1, 0), (0, -1), (-1, -1)] {
        assert!(corner.contains(&o));
    }
}

#[test]
fn rebase_is_exact_for_boundary_walk() {
    // Walking off A(0,0) at x=258 lands at x=2 in B(1,0), bit-exact.
    use game::na::{Quaternion, Translation3, Unit};
    use game::parry::math::Real;
    let iso = IsometryReal::from_parts(
        Translation3::new(Real::from(258.0), Real::from(26.0), Real::from(100.0)),
        Unit::<Quaternion<Real>>::identity(),
    );
    let out = rebase_isometry(&iso, RegionCoords::new(0, 0), RegionCoords::new(1, 0));
    assert_eq!(out.translation.x, Real::from(2.0));
    assert_eq!(out.translation.z, Real::from(100.0));
    // Round-trip is identity.
    let back = rebase_isometry(&out, RegionCoords::new(1, 0), RegionCoords::new(0, 0));
    assert_eq!(back, iso);
}

#[test]
fn matches_prediction_is_identity_based_for_transfers() {
    use game::{Client, ColliderSpec, EntityBundle, EntityKind, GameEventKind, GhostData};
    use game::parry::math::Vector;
    let mk = |x: f32| EntityBundle {
        kind: EntityKind::Player,
        isometry: IsometryReal::translation(x.into(), 0.0.into(), 0.0.into()),
        linvel: Vector::zeros(),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        has_camera: true,
        client: Some((7, Client::default())),
        source_region: RegionCoords::new(0, 0),
        source_key: Default::default(),
    };
    // Same identity, different pose (predicted vs authoritative tick): matches.
    let a = GameEventKind::EntityArrived(mk(2.0));
    let b = GameEventKind::EntityArrived(mk(3.5));
    assert!(a.matches_prediction(&b));
    assert_ne!(a, b, "full equality still detects divergence");
    // Different identity: no match.
    let mut other = mk(2.0);
    other.source_region = RegionCoords::new(5, 5);
    assert!(!a.matches_prediction(&GameEventKind::EntityArrived(other)));
    // Non-transfer kinds: exact equality.
    assert!(GameEventKind::Tick.matches_prediction(&GameEventKind::Tick));
    let g = |x: f32| GameEventKind::GhostUpdate(GhostData {
        source_region: RegionCoords::new(0, 0),
        source_key: Default::default(),
        kind: EntityKind::Player,
        isometry: IsometryReal::translation(x.into(), 0.0.into(), 0.0.into()),
        linvel: Vector::zeros(),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
    });
    assert!(g(1.0).matches_prediction(&g(9.0)));
}
```

Note: if `IsometryReal::translation(...)` with `Real` args doesn't compile that way, build via `from_parts` as in `rebase_is_exact_for_boundary_walk` — the assertion content is what matters.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test handoff_protocol`
Expected: FAIL to compile — `departure_offset` etc. not found.

- [ ] **Step 3: Implement in `crates/game/src/protocol.rs`**

Below the `RegionCoords` impl:

```rust
/// Owned entities within this distance of a region edge are mirrored as
/// ghosts into that neighbour (up to 3 at a corner).
pub const GHOST_MARGIN: f32 = 32.0;
/// Ownership flips only this far PAST the boundary; flipping back needs 2x
/// the travel. Kills boundary thrash.
pub const FLIP_HYSTERESIS: f32 = 2.0;
/// A ghost not refreshed for this many host ticks is removed by the host's
/// tick (owner parked/died/left the margin).
pub const GHOST_TTL_TICKS: usize = 25;

/// Neighbour offset that now owns a region-local point, None while this
/// region still owns it. Pure; the same function runs on server regions and
/// in the client's predicted ticks.
pub fn departure_offset(x: f32, z: f32) -> Option<(i32, i32)> {
    let axis = |v: f32| {
        if v < -FLIP_HYSTERESIS {
            -1
        } else if v > REGION_SIZE + FLIP_HYSTERESIS {
            1
        } else {
            0
        }
    };
    match (axis(x), axis(z)) {
        (0, 0) => None,
        o => Some(o),
    }
}

/// Neighbour offsets whose margin a region-local point is inside. Order is
/// fixed (x edge, z edge, corner) — determinism requires a stable order.
pub fn ghost_offsets(x: f32, z: f32) -> Vec<(i32, i32)> {
    let dx = if x < GHOST_MARGIN { -1 } else if x > REGION_SIZE - GHOST_MARGIN { 1 } else { 0 };
    let dz = if z < GHOST_MARGIN { -1 } else if z > REGION_SIZE - GHOST_MARGIN { 1 } else { 0 };
    let mut out = Vec::new();
    if dx != 0 { out.push((dx, 0)); }
    if dz != 0 { out.push((0, dz)); }
    if dx != 0 && dz != 0 { out.push((dx, dz)); }
    out
}

/// Move an isometry from one region's local frame to another's. Offsets are
/// exact multiples of 256 so the delta is exact in f32; both the server
/// relay and the client's predicted synthesis MUST use this one function.
pub fn rebase_isometry(iso: &IsometryReal, from: RegionCoords, to: RegionCoords) -> IsometryReal {
    let f = from.world_offset();
    let t = to.world_offset();
    let mut out = *iso;
    out.translation.x += Real::from(f[0] - t[0]);
    out.translation.z += Real::from(f[2] - t[2]);
    out
}
```

Add imports at the top of protocol.rs: `use crate::{Client, EntityKey, EntityKind, IsometryReal};` and `use parry3d::math::{Real, Vector};` (adjust to the existing import style; `IsometryReal` and `EntityKind` live in state.rs, re-exported at crate root).

Add the payload types:

```rust
/// Shape spec carried across the transfer seam so the receiving region can
/// rebuild the collider. Players are capsules; new kinds extend this enum.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum ColliderSpec {
    CapsuleY { half_height: f32, radius: f32 },
}

/// The unit of ownership transfer. Assembled deterministically at the
/// extraction tick; `isometry` is source-local until the relay rebases it.
/// `source_region`/`source_key` are an identity token (ghost upgrade,
/// arrival idempotency) — never dereferenced in the target's slotmap.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct EntityBundle {
    pub kind: EntityKind,
    pub isometry: IsometryReal,
    pub linvel: Vector<Real>,
    pub collider: ColliderSpec,
    pub has_camera: bool,
    pub client: Option<(ClientId, Client)>,
    pub source_region: RegionCoords,
    pub source_key: EntityKey,
}

/// Per-tick mirror of a margin entity. `collider` rides along for stage 2.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct GhostData {
    pub source_region: RegionCoords,
    pub source_key: EntityKey,
    pub kind: EntityKind,
    pub isometry: IsometryReal,
    pub linvel: Vector<Real>,
    pub collider: ColliderSpec,
}
```

Extend `GameEventKind` and its impl:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum GameEventKind {
    Tick,
    PlayerInput(ClientId, InputEvent),
    CreateClient(ClientId),
    Quit,
    /// Ownership transfer into this region (manager-relayed or
    /// client-predicted). Injection is an ordinary undoable mutation.
    EntityArrived(EntityBundle),
    /// Margin mirror refresh from a neighbouring region.
    GhostUpdate(GhostData),
}

impl GameEventKind {
    pub fn origin_client(&self) -> Option<ClientId> {
        match self {
            GameEventKind::PlayerInput(id, _) | GameEventKind::CreateClient(id) => Some(*id),
            GameEventKind::Tick
            | GameEventKind::Quit
            | GameEventKind::EntityArrived(_)
            | GameEventKind::GhostUpdate(_) => None,
        }
    }

    /// Reconcile's prediction-removal matcher. Transfers match on IDENTITY
    /// (source region+key), not full equality: the predicted and
    /// authoritative copies differ in pose whenever the client's extraction
    /// tick differs from the server's, and the rollback-replace path must
    /// still find and remove the prediction or it re-applies forever.
    pub fn matches_prediction(&self, other: &Self) -> bool {
        use GameEventKind::*;
        match (self, other) {
            (EntityArrived(a), EntityArrived(b)) => {
                a.source_region == b.source_region && a.source_key == b.source_key
            }
            (GhostUpdate(a), GhostUpdate(b)) => {
                a.source_region == b.source_region && a.source_key == b.source_key
            }
            _ => self == other,
        }
    }
}
```

- [ ] **Step 4: Derives and downstream fallout**

- `crates/game/src/state.rs`: add `PartialEq` to `Client`'s derive list (it's inside `GameEventKind` via `EntityBundle` now). Add the update kind variant:

```rust
    /// Marks an entity as a ghost mirror of (source region, source key), or
    /// clears the mark on upgrade/expiry. The bridge uses it to hide ghosts
    /// whose source region is also loaded locally.
    SetGhostSource(EntityKey, Option<RegionCoords>),
```

(in `GameDataUpdateKind`; import `RegionCoords` from `crate::protocol` — state.rs and protocol.rs are siblings re-exported at root, use `crate::RegionCoords` if needed. If this creates a module cycle, move nothing: protocol already imports state types, and state can name `crate::protocol::RegionCoords` directly.)

- `crates/game/src/input.rs`: add `PartialEq` to `InputState` (and to any nested type the compiler flags).
- `crates/game/src/protocol.rs`: `RegionCoords` may need `Default` later (Task 3 map keys don't need it; skip unless the compiler demands it).
- `crates/game/src/region_runner.rs` `handle_input`: no change needed — `EntityArrived`/`GhostUpdate` flow through the generic `kind =>` arm.
- `crates/client/src/renderer/bridge.rs` `drain_region_updates`: the match on `GameDataUpdateKind` is exhaustive; add a stub arm (replaced in Task 7):

```rust
                GameDataUpdateKind::SetGhostSource(_, _) => {
                    // Ghost render-dedupe lands with the bridge task.
                }
```

Run `cargo build --workspace --bins` and chase remaining derive errors (`PartialEq` on nested input types is the expected fallout).

- [ ] **Step 5: Run tests**

Run: `cargo test -p game --test handoff_protocol && cargo test -p game && cargo test -p client && cargo build --workspace --bins`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(game): handoff protocol — boundary math, EntityBundle/GhostData, transfer event kinds

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Undo-safe removal + pose primitives

The forward entity-removal path that `create_player_safe` never needed, plus a pose setter. Everything here must hold the hash bar.

**Files:**
- Verify/Modify: `crates/macros/src/lib.rs` — `UndoMap` and `UndoSlotMap` need `remove()` with exactly-invertible deltas. CHECK FIRST: grep `crates/macros/src/lib.rs` for `fn remove` inside the UndoMap/UndoSlotMap impl blocks. The slotmapd fork exposes `revert_remove` (see `crates/game/tests/hash_restore.rs`), so the wrapper method may already exist. Only add what is missing, mirroring how `insert` logs its delta.
- Modify: `crates/game/src/state.rs` (`remove_entity_safe`, `set_body_pose_safe`)
- Test: `crates/game/tests/handoff_state.rs` (new)

**Interfaces:**
- Produces: `Rollback::remove_entity_safe(&mut self, key: EntityKey, cam_client: Option<ClientId>)` — undo-tracked removal of components + physics body/collider + ECS slot (auto-emits `RemoveEntity`; undo re-emits `CreateEntity` via the slotmap `#[emit]`); `Rollback::set_body_pose_safe(&mut self, key: EntityKey, pose: IsometryReal)`; `UndoMap::remove(&mut self, key) -> Option<V>` (if it was missing).
- Consumes: `snapshot_raw()` whole-PhysicsState pattern (`attach_capsule_collider_safe` precedent), `revert_remove` on the slotmap fork.

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/handoff_state.rs`:

```rust
use game::{IsometryReal, Rollback};
use game::na::{Quaternion, Translation3, Unit};
use game::parry::math::Real;
use std::hash::{Hash, Hasher};

fn crc(rb: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    rb.data.hash(&mut h);
    h.finalize()
}

fn pose(x: f32, y: f32, z: f32) -> IsometryReal {
    IsometryReal::from_parts(
        Translation3::new(Real::from(x), Real::from(y), Real::from(z)),
        Unit::<Quaternion<Real>>::identity(),
    )
}

#[test]
fn remove_entity_safe_holds_the_hash_bar() {
    let mut rb = Rollback::new(None);
    rb.new_transaction();
    rb.create_player_safe(7);
    rb.forget(); // bake the create in

    let before = crc(&rb);
    let key = *rb.data.player_entites.get(&7).unwrap();
    rb.new_transaction();
    rb.remove_entity_safe(key, Some(7));
    assert!(!rb.data.ecs.entities.contains_key(key), "entity gone");
    let after_remove = crc(&rb);
    assert_ne!(before, after_remove, "removal must change state");
    rb.rollback();
    assert_eq!(before, crc(&rb), "hash(before) == hash(after undo), bit-exact");
}

#[test]
fn set_body_pose_safe_moves_and_undoes_exactly() {
    let mut rb = Rollback::new(None);
    rb.new_transaction();
    rb.create_player_safe(3);
    rb.forget();

    let key = *rb.data.player_entites.get(&3).unwrap();
    let before = crc(&rb);
    rb.new_transaction();
    rb.set_body_pose_safe(key, pose(250.0, 26.0, 10.0));
    let handle = rb.data.ecs.rigidbody.try_get(key).unwrap();
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, Real::from(250.0));
    rb.rollback();
    assert_eq!(before, crc(&rb));
}
```

Note: `rb.data.ecs.rigidbody.try_get(key)` returns `&Option<RigidBodyHandle>`; deref/copy as the compiler directs (`.unwrap()` on the Option after `*`). The client bridge already reads these fields cross-crate (bridge.rs `spawn_region_snapshot`), so visibility is proven. `crc32fast` is already a dev-dependency of `game` (added for the multi-region tests).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test handoff_state`
Expected: FAIL to compile — `remove_entity_safe` not found.

- [ ] **Step 3: Check the macros wrappers for `remove`**

Run: `grep -n "fn remove" crates/macros/src/lib.rs`

If `UndoMap`/`UndoSlotMap` lack `remove`, add them next to `insert`, logging the exact inverse delta the same way `insert` does (for the slotmap: capture `alloc_state()` before, register `revert_remove` — mirror the pattern `hash_restore.rs` proves). Add a case to `crates/game/tests/hash_restore.rs` if you add a primitive: insert → forget → remove → rollback → hash-equal. If they already exist, skip this step entirely.

- [ ] **Step 4: Implement in `crates/game/src/state.rs`**

In `impl Rollback`, next to `create_player_safe`:

```rust
    /// Forward inverse of the create path: undo-tracked removal of an
    /// entity's components, physics body+collider, and ECS slot. The
    /// physics removal has no exact surgical inverse (body removal touches
    /// colliders, islands, joints), so it rides a whole-PhysicsState
    /// snapshot — extractions are rare (one per boundary crossing).
    /// `cam_client`: the owning client if the entity has a camera, so the
    /// undo direction can re-emit AddCameraComponent for the renderer.
    pub fn remove_entity_safe(&mut self, key: EntityKey, cam_client: Option<ClientId>) {
        // Camera: Do-emit removal now; on undo, re-advertise it.
        if let Some(cam) = self.data.ecs.camera.try_get(key).clone() {
            let restored_pose = self
                .data
                .ecs
                .rigidbody
                .try_get(key)
                .and_then(|h| self.data.physics.bodies.get(h).map(|b| *b.position()))
                .unwrap_or_else(IsometryReal::identity);
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::RemoveCameraComponent(key),
            ));
            self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::AddCameraComponent(
                    key,
                    cam_client.unwrap_or_default(),
                    cam.proj_matrix.clone(),
                    restored_pose,
                ),
            ));
            self.data.ecs.camera.set_safe(key, None);
        }
        // Kind: Do-emit clear; undo re-emits the old kind.
        if let Some(kind) = *self.data.ecs.kind.try_get(key) {
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityKind(key, None),
            ));
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetEntityKind(key, Some(kind)),
            ));
            self.data.ecs.kind.set_safe(key, None);
        }
        // Physics: remove body + attached colliders under a full snapshot.
        if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
            let p = self.data.physics.snapshot_raw();
            p.bodies.remove(
                handle,
                p.islands,
                p.colliders,
                p.implules_joint_set,
                p.multi_body_joint_set,
                true,
            );
        }
        self.data.ecs.rigidbody.set_safe(key, None);
        self.data.ecs.isometry.set_safe(key, None);
        // ECS slot last: the slotmap #[emit] fires RemoveEntity on the delta
        // (and CreateEntity again on undo).
        self.data.ecs.entities.remove(key);
    }

    /// Teleport a body, undo-safely. change(): whole-RigidBodySet snapshot —
    /// set_position also wakes the body / marks it modified (hashed state a
    /// surgical closure can't restore); same rationale as camera.rs.
    pub fn set_body_pose_safe(&mut self, key: EntityKey, pose: IsometryReal) {
        let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { return };
        let bodies = self.data.physics.bodies.change();
        bodies.get_mut(handle).unwrap().set_position(pose, true);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityPosition(key, pose),
        ));
    }
```

Adjust to reality as the compiler directs: exact `snapshot_raw()` field names are in `attach_capsule_collider_safe`; `try_get` returns `&Option<_>` so copy/deref accordingly; `RigidBodySet::remove`'s exact parameter list is in the vendored fork (`crates/rapier/src/dynamics/rigid_body_set.rs`) — pass the sets the signature demands from the snapshot parts. If `entities.remove` doesn't exist on the wrapper, Step 3 adds it. `cam_client.unwrap_or_default()`: ClientId is usize; 0 is a display-only fallback for the undo emit.

- [ ] **Step 5: Run tests**

Run: `cargo test -p game --test handoff_state && cargo test -p game`
Expected: PASS, including the whole existing rollback suite. If the hash bar fails, the physics removal path is losing state — widen the snapshot (snapshot the entire PhysicsState, not just bodies), do NOT weaken the assertion.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(game): undo-safe entity removal + body pose primitives

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Ghost + arrival state operations

`ghosts`/`arrivals` maps in `GameData`; `apply_arrival` (fresh / upgrade-in-place / idempotent), `apply_ghost` (upsert), `expire_ghosts` (TTL). All undo-tracked, all hash-barred.

**Files:**
- Modify: `crates/game/src/state.rs` (`GhostEntry`, two new `#[undo(map)]` fields, the three ops, `inject_body_safe` helper)
- Test: extend `crates/game/tests/handoff_state.rs`

**Interfaces:**
- Produces:

```rust
pub struct GhostEntry { pub entity: EntityKey, pub last_update_tick: usize }
// GameData gains:
//   #[undo(map)] ghosts: BTreeMap<(RegionCoords, EntityKey), GhostEntry>,
//   #[undo(map)] arrivals: BTreeMap<(RegionCoords, EntityKey), EntityKey>,
impl Rollback {
    pub fn apply_arrival(&mut self, bundle: EntityBundle);
    pub fn apply_ghost(&mut self, data: GhostData);
    pub fn expire_ghosts(&mut self);
}
```

- Consumes: Task 1 payloads, Task 2 `remove_entity_safe`/`set_body_pose_safe`, `create_entity_safe`, the `create_player_safe` body/collider insert pattern.

- [ ] **Step 1: Write the failing tests**

Append to `crates/game/tests/handoff_state.rs`:

```rust
use game::{
    Client, ColliderSpec, EntityBundle, EntityKind, GhostData, RegionCoords, GHOST_TTL_TICKS,
};
use game::parry::math::Vector;

fn bundle(src: RegionCoords, key: game::EntityKey, client: game::ClientId, x: f32) -> EntityBundle {
    EntityBundle {
        kind: EntityKind::Player,
        isometry: pose(x, 26.0, 128.0),
        linvel: Vector::zeros(),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        has_camera: true,
        client: Some((client, Client::default())),
        source_region: src,
        source_key: key,
    }
}

fn ghost(src: RegionCoords, key: game::EntityKey, x: f32) -> GhostData {
    GhostData {
        source_region: src,
        source_key: key,
        kind: EntityKind::Player,
        isometry: pose(x, 26.0, 128.0),
        linvel: Vector::zeros(),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
    }
}

/// Donor rollback: create a player in a source region to get a real
/// (region, key) identity to transfer.
fn donor() -> (Rollback, game::EntityKey) {
    let mut rb = Rollback::new(None);
    rb.new_transaction();
    rb.create_player_safe(9);
    rb.forget();
    let key = *rb.data.player_entites.get(&9).unwrap();
    (rb, key)
}

#[test]
fn arrival_creates_player_with_client_state_and_holds_hash_bar() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let mut rb = Rollback::new(None);
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 2.0));
    assert!(rb.data.player_entites.contains_key(&9));
    assert!(rb.data.clients.contains_key(&9), "input state travels with the player");
    let e = *rb.data.player_entites.get(&9).unwrap();
    let handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, Real::from(2.0), "arrives at the (rebased) bundle pose");
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

#[test]
fn ghost_upsert_refresh_and_ttl_expiry_hold_hash_bar() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let mut rb = Rollback::new(None);

    // Create.
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    assert_eq!(rb.data.ghosts.len(), 1);
    let entry = rb.data.ghosts.get(&(src, src_key)).unwrap().clone();

    // Refresh keeps the same entity and holds the bar.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 252.0));
    assert_eq!(
        rb.data.ghosts.get(&(src, src_key)).unwrap().entity,
        entry.entity,
        "refresh must not respawn the ghost"
    );
    rb.rollback();
    assert_eq!(before, crc(&rb));

    // Expiry: age the region past the TTL, then tick the reaper.
    rb.new_transaction();
    for _ in 0..(GHOST_TTL_TICKS + 1) {
        rb.data.tick.update(|t| *t += 1);
    }
    rb.forget();
    let before = crc(&rb);
    rb.new_transaction();
    rb.expire_ghosts();
    assert!(rb.data.ghosts.get(&(src, src_key)).is_none(), "stale ghost reaped");
    assert!(!rb.data.ecs.entities.contains_key(entry.entity));
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

#[test]
fn arrival_upgrades_ghost_in_place_keeping_entity_key() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let mut rb = Rollback::new(None);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    let ghost_entity = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;

    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 2.0));
    rb.forget();
    let owned = *rb.data.player_entites.get(&9).unwrap();
    assert_eq!(owned, ghost_entity, "upgrade-in-place: same EntityKey continues");
    assert!(rb.data.ghosts.get(&(src, src_key)).is_none(), "ghost record dropped");
    assert!(rb.data.ecs.rigidbody.try_get(owned).is_some(), "body attached on upgrade");
}

#[test]
fn replayed_arrival_is_a_pose_correction_not_a_duplicate() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let mut rb = Rollback::new(None);
    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 2.0));
    rb.forget();
    let count = rb.data.ecs.entities.len();

    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 5.0));
    rb.forget();
    assert_eq!(rb.data.ecs.entities.len(), count, "no duplicate entity");
    let e = *rb.data.player_entites.get(&9).unwrap();
    let handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, Real::from(5.0), "replay corrected the pose");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test handoff_state`
Expected: FAIL to compile — `apply_arrival` etc. not found.

- [ ] **Step 3: State fields**

In `crates/game/src/state.rs`, above the `#[rollback]` module, define:

```rust
/// A live ghost mirror in this region, keyed in `GameData::ghosts` by its
/// source identity. `last_update_tick` drives the TTL reaper.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash, PartialEq)]
#[module(crate)]
pub struct GhostEntry {
    pub entity: EntityKey,
    pub last_update_tick: usize,
}
```

Inside the `game_data` module's `GameData`, after `clients`:

```rust
        /// Ghost mirrors hosted here, by source identity.
        #[undo(map)]
        ghosts: BTreeMap<(crate::RegionCoords, EntityKey), GhostEntry>,
        /// Owned entities that arrived via handoff, by the identity they
        /// arrived under — makes replayed arrivals idempotent.
        #[undo(map)]
        arrivals: BTreeMap<(crate::RegionCoords, EntityKey), EntityKey>,
```

Add `GhostEntry` to the module's `use super::{...}` list, and `crate::RegionCoords` paths as the macro requires (if the macro chokes on path-qualified types, `use crate::protocol::RegionCoords;` in the module's imports and write `RegionCoords` bare).

- [ ] **Step 4: The operations**

In `impl Rollback` (state.rs):

```rust
    /// Body+collider insertion from a bundle — the create_player_safe insert
    /// pattern generalized to a transfer payload.
    fn inject_body_safe(&mut self, e: EntityKey, bundle: &EntityBundle) {
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(bundle.isometry)
            .gravity_scale(Real::from(0.0))
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(Real::from(1.0))
            .can_sleep(false)
            .user_data(e.data().as_ffi() as u128)
            .build();
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope();
        let handle = scope.insert(body);
        scope.register(move |bodies, _| bodies.revert_insert(handle, prev_head, prev_len));
        self.data.ecs.rigidbody.set_safe(e, Some(handle));
        match bundle.collider {
            ColliderSpec::CapsuleY { half_height, radius } => {
                self.attach_capsule_collider_safe(e, handle, half_height, radius);
            }
        }
    }

    /// Ownership transfer INTO this region. Three paths:
    /// replayed identity → pose correction; ghost present → upgrade in
    /// place (same EntityKey — visual continuity); else → fresh create.
    pub fn apply_arrival(&mut self, bundle: EntityBundle) {
        let identity = (bundle.source_region, bundle.source_key);

        // Idempotency: a respawn-resnapshot replay must not duplicate.
        if let Some(&e) = self.data.arrivals.get(&identity) {
            if self.data.ecs.entities.contains_key(e) {
                self.set_body_pose_safe(e, bundle.isometry);
                return;
            }
        }

        let e = if let Some(entry) = self.data.ghosts.get(&identity).cloned() {
            // Upgrade-in-place: drop the ghost record, keep the entity.
            self.data.ghosts.remove(&identity);
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetGhostSource(entry.entity, None),
            ));
            entry.entity
        } else {
            self.data.ecs.create_entity_safe()
        };

        // Kind (emit both directions, as create_player_safe does).
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(bundle.kind)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(bundle.kind));

        // Body + collider at the (already rebased) bundle pose. A stage-1
        // ghost has no body; if one exists (stage 2), correct its pose
        // instead of double-inserting.
        if self.data.ecs.rigidbody.try_get(e).is_some() {
            self.set_body_pose_safe(e, bundle.isometry);
        } else {
            self.inject_body_safe(e, &bundle);
        }

        // Camera + client attachment (players).
        if let Some((client_id, client)) = bundle.client.clone() {
            if bundle.has_camera {
                let handle = self.data.ecs.rigidbody.try_get(e).unwrap();
                let cam = Camera::new(handle);
                self.data.send(GameDataUpdate::new(
                    GameDataTransactionKind::Do,
                    GameDataUpdateKind::AddCameraComponent(
                        e, client_id, cam.proj_matrix.clone(), bundle.isometry,
                    ),
                ));
                self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
                    GameDataTransactionKind::Undo,
                    GameDataUpdateKind::RemoveCameraComponent(e),
                ));
                self.data.ecs.camera.set_safe(e, Some(cam));
            }
            self.data.clients.insert(client_id, client);
            self.data.player_entites.insert(client_id, e);
        }

        self.data.arrivals.insert(identity, e);
    }

    /// Margin mirror upsert. Stage 1: pose-only renderable (no collider).
    pub fn apply_ghost(&mut self, data: GhostData) {
        let identity = (data.source_region, data.source_key);
        let tick = *self.data.tick;
        if let Some(entry) = self.data.ghosts.get(&identity).cloned() {
            let mut refreshed = entry.clone();
            refreshed.last_update_tick = tick;
            self.data.ghosts.insert(identity, refreshed); // UndoMap logs the old value
            self.data.ecs.isometry.set_safe(entry.entity, Some(data.isometry));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityPosition(entry.entity, data.isometry),
            ));
        } else {
            let e = self.data.ecs.create_entity_safe();
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityKind(e, Some(data.kind)),
            ));
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetEntityKind(e, None),
            ));
            self.data.ecs.kind.set_safe(e, Some(data.kind));
            self.data.ecs.isometry.set_safe(e, Some(data.isometry));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityPosition(e, data.isometry),
            ));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetGhostSource(e, Some(data.source_region)),
            ));
            self.data.ghosts.insert(
                identity,
                GhostEntry { entity: e, last_update_tick: tick },
            );
        }
    }

    /// TTL reaper: called once per tick. Covers the owner region parking,
    /// dying, or the entity leaving the margin.
    pub fn expire_ghosts(&mut self) {
        let tick = *self.data.tick;
        let expired: Vec<((crate::RegionCoords, EntityKey), EntityKey)> = self
            .data
            .ghosts
            .iter()
            .filter(|(_, g)| tick.saturating_sub(g.last_update_tick) > GHOST_TTL_TICKS)
            .map(|(k, g)| (*k, g.entity))
            .collect();
        for (k, e) in expired {
            self.data.ghosts.remove(&k);
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetGhostSource(e, None),
            ));
            self.remove_entity_safe(e, None);
        }
    }
```

Imports: `use crate::protocol::{ColliderSpec, EntityBundle, GhostData, GHOST_TTL_TICKS};` (or via crate root). If `UndoMap::insert` doesn't log-and-replace on an existing key, use its `get_mut` for the refresh path instead — whatever primitive the wrapper offers that logs the prior value.

- [ ] **Step 5: Run tests**

Run: `cargo test -p game --test handoff_state && cargo test -p game && cargo build --workspace --bins`
Expected: PASS. The client suite may need a rebuild (`GameData` layout changed); `cargo test -p client` must also pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(game): ghost/arrival state ops — inject, upgrade-in-place, TTL reaper

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Boundary scan + extraction in the region tick

The deterministic core: the tick scans, extracts leavers, mirrors margin entities; `Region` buffers the results; `World` aggregates them for the client.

**Files:**
- Modify: `crates/game/src/state.rs` (`scan_boundaries`, `extract_entity_safe`, `collider_spec_of`)
- Modify: `crates/game/src/region.rs` (Tick arm additions, `pending_departures`/`pending_ghosts` fields, `take_transfers`, new event-kind arms)
- Modify: `crates/game/src/lib.rs` (`World::take_transfers`)
- Test: `crates/game/tests/handoff_scan.rs` (new)

**Interfaces:**
- Produces:

```rust
impl Rollback {
    /// (departures, ghost updates) with ABSOLUTE target coords.
    pub fn scan_boundaries(&mut self, region: RegionCoords)
        -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>);
    pub fn extract_entity_safe(&mut self, key: EntityKey, region: RegionCoords) -> EntityBundle;
}
impl Region {
    /// Buffers from the LAST executed tick (replaced, not appended — see
    /// the read-after-progress invariant below).
    pub fn take_transfers(&mut self)
        -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>);
}
impl World {
    pub fn take_transfers(&mut self)
        -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>);
}
```

- **Invariant (document in code):** tick processing REPLACES the buffers. Consumers read them immediately after driving a tick (`RegionRunner::tick` on the server, right after `progress_world_one_tick` on the client) and never at any other time — reconcile replays overwrite the buffers with stale scans, which is harmless precisely because nothing reads between a replay and the next real tick.
- Consumes: Task 1 math/payloads, Task 2/3 ops.

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/handoff_scan.rs`:

```rust
use game::{
    GameEventKind, IsometryReal, Region, RegionCoords, Rollback, FLIP_HYSTERESIS, REGION_SIZE,
};
use game::na::{Quaternion, Translation3, Unit};
use game::parry::math::Real;
use std::hash::{Hash, Hasher};

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.hash(&mut h);
    h.finalize()
}

fn pose(x: f32, z: f32) -> IsometryReal {
    IsometryReal::from_parts(
        Translation3::new(Real::from(x), Real::from(26.0), Real::from(z)),
        Unit::<Quaternion<Real>>::identity(),
    )
}

/// A region with one player, teleported to (x, z), ticked once.
fn region_with_player_at(id: RegionCoords, x: f32, z: f32) -> Region {
    let mut region = Region::from_chunks(id, Vec::new());
    region.handle_event(GameEventKind::CreateClient(7)).unwrap();
    region.forget_last_event();
    let key = *region.data().player_entites.get(&7).unwrap();
    // Test-only teleport through the public undo-safe primitive, wrapped in
    // its own forgotten transaction so the tick under test stays clean.
    region.with_data(|d| d.set_body_pose_safe(key, pose(x, z)));
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    region
}

#[test]
fn tick_extracts_a_leaver_into_a_departure() {
    let id = RegionCoords::new(0, 0);
    let mut region = region_with_player_at(id, REGION_SIZE + FLIP_HYSTERESIS + 1.0, 128.0);
    let (departures, _ghosts) = region.take_transfers();
    assert_eq!(departures.len(), 1);
    let (bundle, target) = &departures[0];
    assert_eq!(*target, RegionCoords::new(1, 0));
    assert_eq!(bundle.source_region, id);
    assert!(bundle.client.is_some(), "player carries its client input state");
    assert!(bundle.has_camera);
    assert!(
        !region.data().player_entites.contains_key(&7),
        "extracted player is gone from the source"
    );
    assert!(!region.data().clients.contains_key(&7));
}

#[test]
fn tick_mirrors_margin_entities_without_extracting() {
    let id = RegionCoords::new(0, 0);
    let mut region = region_with_player_at(id, REGION_SIZE - 10.0, 128.0);
    let (departures, ghosts) = region.take_transfers();
    assert!(departures.is_empty());
    assert_eq!(ghosts.len(), 1);
    let (data, target) = &ghosts[0];
    assert_eq!(*target, RegionCoords::new(1, 0));
    assert_eq!(data.source_region, id);
    assert!(region.data().player_entites.contains_key(&7), "still owned here");
}

#[test]
fn corner_mirrors_into_three_neighbours() {
    let mut region = region_with_player_at(RegionCoords::new(0, 0), 10.0, 10.0);
    let (_, ghosts) = region.take_transfers();
    let targets: Vec<RegionCoords> = ghosts.iter().map(|(_, t)| *t).collect();
    assert_eq!(targets.len(), 3);
    for t in [RegionCoords::new(-1, 0), RegionCoords::new(0, -1), RegionCoords::new(-1, -1)] {
        assert!(targets.contains(&t));
    }
}

#[test]
fn hysteresis_stops_boundary_thrash() {
    // Just past the line but inside the band: still owned, only mirrored.
    let mut region = region_with_player_at(RegionCoords::new(0, 0), REGION_SIZE + 1.0, 128.0);
    let (departures, ghosts) = region.take_transfers();
    assert!(departures.is_empty(), "inside the hysteresis band: no flip");
    assert!(!ghosts.is_empty());
}

#[test]
fn extracting_tick_holds_the_hash_bar() {
    let id = RegionCoords::new(0, 0);
    let mut region = Region::from_chunks(id, Vec::new());
    region.handle_event(GameEventKind::CreateClient(7)).unwrap();
    region.forget_last_event();
    let key = *region.data().player_entites.get(&7).unwrap();
    region.with_data(|d| d.set_body_pose_safe(key, pose(REGION_SIZE + 5.0, 128.0)));
    let before = crc(&region);
    // The extracting tick, NOT forgotten: roll it back.
    region.handle_event(GameEventKind::Tick).unwrap();
    assert!(!region.data().player_entites.contains_key(&7));
    region.rollback_last_event();
    assert_eq!(before, crc(&region), "hash(before) == hash(after undo) across extraction");
}

#[test]
fn identical_streams_produce_identical_scans_and_hashes() {
    // The property client prediction depends on (spec Testing #3): two
    // regions fed the same events agree bit-exactly on state AND transfers.
    let run = || {
        let id = RegionCoords::new(0, 0);
        let mut region = region_with_player_at(id, REGION_SIZE - 10.0, 10.0);
        let transfers = region.take_transfers();
        (crc(&region), format!("{:?}", transfers))
    };
    let (h1, t1) = run();
    let (h2, t2) = run();
    assert_eq!(h1, h2, "state hashes agree across runs");
    assert_eq!(t1, t2, "departure/ghost buffers agree across runs");
}

#[test]
fn ghosts_never_transfer_and_terrain_never_mirrors() {
    // A ghost sitting past the boundary must not depart; chunk entities in
    // the margin must not mirror.
    let id = RegionCoords::new(0, 0);
    let mut region = Region::from_chunks(
        id,
        vec![(game::ChunkCoords::new(7, 0, 7), game::Chunk::flat_floor(8))], // edge chunk, inside margin
    );
    let src = RegionCoords::new(1, 0);
    region
        .handle_event(GameEventKind::GhostUpdate(game::GhostData {
            source_region: src,
            source_key: Default::default(),
            kind: game::EntityKind::Player,
            isometry: pose(300.0, 128.0), // absurdly out of bounds
            linvel: game::parry::math::Vector::zeros(),
            collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        }))
        .unwrap();
    region.forget_last_event();
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (departures, ghosts) = region.take_transfers();
    assert!(departures.is_empty(), "ghosts are never extracted");
    assert!(ghosts.is_empty(), "kindless terrain never mirrors");
}
```

This needs two small Region test hooks (implement in Step 3): `Region::with_data(&mut self, f: impl FnOnce(&mut Rollback))` — runs `f` inside its own forgotten transaction — and `Region::rollback_last_event(&mut self)` (calls `self.data.rollback()`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test handoff_scan`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`crates/game/src/state.rs`, in `impl Rollback`:

```rust
    fn collider_spec_of(&self, handle: RigidBodyHandle) -> crate::ColliderSpec {
        self.data
            .physics
            .bodies
            .get(handle)
            .and_then(|b| b.colliders().first().copied())
            .and_then(|ch| self.data.physics.colliders.get(ch))
            .and_then(|c| {
                c.shape().as_capsule().map(|cap| crate::ColliderSpec::CapsuleY {
                    half_height: cap.half_height().0,
                    radius: cap.radius.0,
                })
            })
            // Non-capsule shapes don't transfer yet; the player default.
            .unwrap_or(crate::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 })
    }

    /// Assemble the bundle, then remove the entity — all undo-tracked.
    pub fn extract_entity_safe(&mut self, key: EntityKey, region: crate::RegionCoords) -> crate::EntityBundle {
        let kind = self.data.ecs.kind.try_get(key).unwrap_or(&None).unwrap_or_default();
        let handle = (*self.data.ecs.rigidbody.try_get(key)).expect("transferable entities have bodies");
        let body = self.data.physics.bodies.get(handle).unwrap();
        let isometry = *body.position();
        let linvel = *body.linvel();
        let collider = self.collider_spec_of(handle);
        let client_id = self
            .data
            .player_entites
            .iter()
            .find(|(_, e)| **e == key)
            .map(|(c, _)| *c);
        let client = client_id.map(|c| (c, self.data.clients.get(&c).unwrap().clone()));
        let bundle = crate::EntityBundle {
            kind,
            isometry,
            linvel,
            collider,
            has_camera: self.data.ecs.camera.try_get(key).is_some(),
            client,
            source_region: region,
            source_key: key,
        };
        if let Some(c) = client_id {
            self.data.player_entites.remove(&c);
            self.data.clients.remove(&c);
        }
        // This entity's own arrival identity (if it arrived here) is now stale.
        let stale: Vec<(crate::RegionCoords, EntityKey)> = self
            .data
            .arrivals
            .iter()
            .filter(|(_, e)| **e == key)
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            self.data.arrivals.remove(&k);
        }
        self.remove_entity_safe(key, client_id);
        bundle
    }

    /// Post-step boundary/margin scan. Iterates the kind component in key
    /// order (deterministic); terrain (kindless) and ghosts are excluded.
    pub fn scan_boundaries(
        &mut self,
        region: crate::RegionCoords,
    ) -> (
        Vec<(crate::EntityBundle, crate::RegionCoords)>,
        Vec<(crate::GhostData, crate::RegionCoords)>,
    ) {
        let ghost_keys: std::collections::BTreeSet<EntityKey> =
            self.data.ghosts.iter().map(|(_, g)| g.entity).collect();
        let mut leavers: Vec<(EntityKey, crate::RegionCoords)> = Vec::new();
        let mut ghosts: Vec<(crate::GhostData, crate::RegionCoords)> = Vec::new();
        for (key, kind) in self.data.ecs.kind.iter() {
            let Some(kind) = kind else { continue };
            if ghost_keys.contains(&key) {
                continue;
            }
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let t = body.translation();
            if let Some((dx, dz)) = crate::departure_offset(t.x.0, t.z.0) {
                leavers.push((key, crate::RegionCoords::new(region.x + dx, region.z + dz)));
            } else {
                for (dx, dz) in crate::ghost_offsets(t.x.0, t.z.0) {
                    ghosts.push((
                        crate::GhostData {
                            source_region: region,
                            source_key: key,
                            kind: *kind,
                            isometry: *body.position(),
                            linvel: *body.linvel(),
                            collider: self.collider_spec_of(handle),
                        },
                        crate::RegionCoords::new(region.x + dx, region.z + dz),
                    ));
                }
            }
        }
        let mut departures = Vec::new();
        for (key, target) in leavers {
            departures.push((self.extract_entity_safe(key, region), target));
        }
        (departures, ghosts)
    }
```

(`kind.iter()` yields `(EntityKey, &Option<EntityKind>)` — adjust patterns to what the compiler says; `collider_spec_of` borrows immutably so call it before the mutable extraction loop if the borrow checker objects — capture specs into `leavers`' tuples if needed.)

`crates/game/src/region.rs`:

- Add fields to `Region` (initialize empty in `new`):

```rust
    /// Boundary-scan results from the LAST executed tick. REPLACED by each
    /// tick, never appended: consumers read immediately after driving a
    /// tick; reconcile replays overwrite these with stale scans that are
    /// never read (see the plan's read-after-progress invariant).
    pending_departures: Vec<(EntityBundle, RegionCoords)>,
    pending_ghosts: Vec<(GhostData, RegionCoords)>,
```

- In `handle_event`, extend the `GameEventKind::Tick` arm — after the clients key-loop, immediately before `self.data.tick.update(|t| *t += 1);`:

```rust
                let (departures, ghosts) = self.data.scan_boundaries(self.id);
                self.pending_departures = departures;
                self.pending_ghosts = ghosts;
                self.data.expire_ghosts();
```

- Add the new event arms to the same match:

```rust
            GameEventKind::EntityArrived(bundle) => {
                self.data.apply_arrival(bundle);
            }
            GameEventKind::GhostUpdate(data) => {
                self.data.apply_ghost(data);
            }
```

- Add the accessors + test hooks:

```rust
    pub fn take_transfers(
        &mut self,
    ) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>) {
        (
            std::mem::take(&mut self.pending_departures),
            std::mem::take(&mut self.pending_ghosts),
        )
    }

    /// Test hook: run an undo-safe mutation in its own forgotten
    /// transaction (teleports, state surgery in integration tests).
    pub fn with_data(&mut self, f: impl FnOnce(&mut Rollback)) {
        self.data.new_transaction();
        f(&mut self.data);
        self.data.forget();
    }

    /// Test hook: roll back the most recent (unforgotten) event.
    pub fn rollback_last_event(&mut self) {
        self.data.rollback();
        self.event_log.pop_back();
    }
```

Import `EntityBundle, GhostData, RegionCoords` in region.rs.

`crates/game/src/lib.rs`, in `impl World`:

```rust
    /// Aggregate transfer buffers from all loaded regions. Call immediately
    /// after progress_world_one_tick and at no other time.
    pub fn take_transfers(
        &mut self,
    ) -> (Vec<(EntityBundle, RegionCoords)>, Vec<(GhostData, RegionCoords)>) {
        let mut departures = Vec::new();
        let mut ghosts = Vec::new();
        for (_, region) in &mut self.regions {
            let (d, g) = region.take_transfers();
            departures.extend(d);
            ghosts.extend(g);
        }
        (departures, ghosts)
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p game --test handoff_scan && cargo test -p game && cargo test -p client && cargo build --workspace --bins`
Expected: PASS. The extraction hash-bar test is the milestone's core guarantee — if it fails, a removal path is losing state; widen snapshots, never the assertion.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(game): boundary scan + in-tick extraction with transfer buffers

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Runner outputs + manager relay, homes flip, input routing

The server side end-to-end: runner drains buffers into new outputs; the manager rebases and relays, wakes cold targets for arrivals, drops ghost updates to cold targets, flips `homes` + pushes `PlayerRegion`, and routes `PlayerInput` by `homes` (manager-authoritative).

**Files:**
- Modify: `crates/game/src/region_runner.rs` (`RegionOutput::{Departures, GhostUpdates}`, drain in `tick()`)
- Modify: `crates/game/src/world_manager.rs` (relay arms, input routing)
- Test: `crates/game/tests/handoff_manager.rs` (new)

**Interfaces:**
- Produces:

```rust
// RegionOutput gains:
Departures(Vec<(EntityBundle, RegionCoords)>),
GhostUpdates(Vec<(GhostData, RegionCoords)>),
```

- Manager behavior (consumed by Tasks 6/8): departure → rebase isometry (source=rc, target), `ensure_running(target)`, `EntityArrived` in, `homes[client]=target` + `PlayerRegion(Some(target), client)` out + `refresh_keepalive` on both regions when the bundle has a client. Ghost update → relay iff target running && !stopping, else drop. `PlayerInput` routes to `homes[client]`, ignoring the packet's `region_id`.

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/handoff_manager.rs` (harness copied from the style of `crates/game/tests/world_manager.rs` — read that file first; reuse its `Harness`/`settle`/`event`/`drain_packets` shape verbatim):

```rust
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Chunk, ChunkCoords, ClientId, ClientPacket, GameEvent, GameEventKind, InlineSpawner,
    InputEvent, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager,
    FLIP_HYSTERESIS, REGION_SIZE, SPAWN_REGION,
};

struct Harness {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

fn harness() -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let generator = Box::new(|_rc: RegionCoords| -> Vec<(ChunkCoords, Chunk)> {
        vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))]
    });
    Harness {
        manager: WorldManager::new(InlineSpawner::default(), generator, out_send, region_out_send),
        region_out,
        packets,
    }
}

impl Harness {
    fn settle(&mut self, now_ms: u64) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now_ms);
            }
        }
    }
    fn event(&mut self, ev: ServerEvent, now_ms: u64) -> bool {
        let alive = self.manager.handle_server_event(ev, now_ms);
        self.settle(now_ms);
        alive
    }
    fn tick(&mut self, now_ms: u64) {
        self.manager.spawner_mut().tick_all();
        self.settle(now_ms);
    }
    fn drain_packets(&mut self) -> Vec<(Option<ClientId>, ServerPacket)> {
        self.packets.try_iter().collect()
    }
    /// Teleport client 0's player inside its home region (test surgery via
    /// InlineSpawner::with_region — add alongside kill()).
    fn teleport_player(&mut self, rc: RegionCoords, client: ClientId, x: f32, z: f32) {
        self.manager.spawner_mut().with_region(rc, |region| {
            let key = *region.data().player_entites.get(&client).unwrap();
            region.with_data(|d| {
                d.set_body_pose_safe(
                    key,
                    game::IsometryReal::from_parts(
                        game::na::Translation3::new(x.into(), 26.0f32.into(), z.into()),
                        game::na::Unit::<game::na::Quaternion<game::parry::math::Real>>::identity(),
                    ),
                )
            });
        });
    }
}

fn connect_and_subscribe(h: &mut Harness) {
    h.event(ServerEvent::ClientConnected(0), 0);
    for rc in SPAWN_REGION.window_3x3() {
        h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    }
    h.drain_packets();
}

#[test]
fn crossing_flips_ownership_home_and_pushes_player_region() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let target = RegionCoords::new(1, 0);

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(100); // scan extracts; relay injects

    // Player owned by the target region now.
    let mut found_home = None;
    h.manager.spawner_mut().with_region(target, |region| {
        found_home = region.data().player_entites.get(&0).copied();
    });
    assert!(found_home.is_some(), "player entity arrived in {:?}", target);

    let packets = h.drain_packets();
    assert!(
        packets.iter().any(|(to, p)| *to == Some(0)
            && matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == target)),
        "authoritative home push after flip"
    );
    // The arrival also fanned out to subscribers as an EventProcessed.
    assert!(packets.iter().any(|(_, p)| matches!(
        p,
        ServerPacket::GameEvent(ev) if matches!(ev.kind, GameEventKind::EntityArrived(_))
    )));
}

#[test]
fn arrival_wakes_a_cold_region() {
    let mut h = harness();
    // Connect but subscribe to nothing beyond home: target region is cold.
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let target = RegionCoords::new(1, 0);
    assert!(!h.manager.running_regions().contains(&target));

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(100);
    assert!(h.manager.running_regions().contains(&target), "arrival woke the cold target");
}

#[test]
fn ghost_updates_never_wake_a_cold_region() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    let neighbour = RegionCoords::new(1, 0);

    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE - 5.0, 128.0); // margin, not past
    h.tick(100);
    assert!(
        !h.manager.running_regions().contains(&neighbour),
        "ghost updates to cold regions are dropped"
    );
}

#[test]
fn ghost_updates_reach_running_neighbours() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let neighbour = RegionCoords::new(1, 0);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE - 5.0, 128.0);
    h.tick(100);
    let mut ghost_count = 0;
    h.manager.spawner_mut().with_region(neighbour, |region| {
        ghost_count = region.data().ghosts.len();
    });
    assert_eq!(ghost_count, 1, "margin player mirrored into the running neighbour");
}

#[test]
fn input_routes_by_home_ignoring_the_stamp() {
    let mut h = harness();
    connect_and_subscribe(&mut h);
    let target = RegionCoords::new(1, 0);
    h.teleport_player(SPAWN_REGION, 0, REGION_SIZE + FLIP_HYSTERESIS + 2.0, 128.0);
    h.tick(100);
    h.drain_packets();

    // Client still stamps the OLD home (its prediction hasn't confirmed).
    let ev = GameEvent::new(
        GameEventKind::PlayerInput(0, InputEvent::Key { key: game::Key::KeyE, pressed: true }),
        0,
        SPAWN_REGION,
    );
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 200);
    // The input reached the NEW home: its EventProcessed comes from `target`.
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(_, p)| matches!(
        p,
        ServerPacket::GameEvent(ev)
            if ev.region_id == target && matches!(ev.kind, GameEventKind::PlayerInput(0, _))
    )));
}
```

If `InputEvent::Key { .. }` isn't the right constructor, open `crates/game/src/input.rs` and use the simplest real variant (the multi-region tests did the same dance).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test handoff_manager`
Expected: FAIL to compile — `with_region` / new outputs missing.

- [ ] **Step 3: Implement**

`crates/game/src/region_runner.rs`:

- Extend the enum:

```rust
#[derive(Debug)]
pub enum RegionOutput {
    EventProcessed(GameEvent),
    Snapshot(ClientId, Rollback),
    SyncClock { tick_rate: u64, tick: Tick },
    Stopped(SerializedRegion),
    /// Boundary crossings this tick: bundles + ABSOLUTE target regions.
    /// Isometries are still source-local; the manager rebases.
    Departures(Vec<(EntityBundle, RegionCoords)>),
    /// Margin mirrors this tick, same framing.
    GhostUpdates(Vec<(GhostData, RegionCoords)>),
}
```

(import `EntityBundle, GhostData` from crate.)

- In `RegionRunner::tick`, after the `EventProcessed` send and before the SyncClock block:

```rust
        let (departures, ghosts) = self.region.take_transfers();
        if !departures.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::Departures(departures)));
        }
        if !ghosts.is_empty() {
            let _ = self.out.send((self.id, RegionOutput::GhostUpdates(ghosts)));
        }
```

`crates/game/src/world_manager.rs`:

- In `handle_region_output`, add the arms:

```rust
            RegionOutput::Departures(list) => {
                for (mut bundle, target) in list {
                    let client = bundle.client.as_ref().map(|(c, _)| *c);
                    bundle.isometry = crate::rebase_isometry(&bundle.isometry, rc, target);
                    // Arrivals ALWAYS wake the target (parked blob or gen).
                    self.ensure_running(target);
                    self.send_to_region(target, RegionInput::Event(GameEventKind::EntityArrived(bundle)));
                    if let Some(c) = client {
                        self.homes.insert(c, target);
                        let _ = self.out.send((Some(c), ServerPacket::PlayerRegion(Some(target), c)));
                        // The old home may have just lost its keep-alive
                        // reason; the new one just gained it.
                        self.refresh_keepalive(rc, now_ms);
                        self.refresh_keepalive(target, now_ms);
                    }
                }
            }
            RegionOutput::GhostUpdates(list) => {
                for (mut data, target) in list {
                    // Ghost updates NEVER wake a region.
                    let running = self
                        .regions
                        .get(&target)
                        .map_or(false, |link| !link.stopping);
                    if !running {
                        continue;
                    }
                    data.isometry = crate::rebase_isometry(&data.isometry, rc, target);
                    self.send_to_region(target, RegionInput::Event(GameEventKind::GhostUpdate(data)));
                }
            }
```

- In `handle_server_event`'s `ClientPacket::GameEvent` match, route input by home. Replace the existing `kind =>` fallthrough arm with:

```rust
                    kind @ GameEventKind::PlayerInput(..) => {
                        // Manager-authoritative routing: the client's stamp
                        // lags its own predicted handoff; homes is truth.
                        let GameEventKind::PlayerInput(cid, _) = &kind else { unreachable!() };
                        match self.homes.get(cid).copied() {
                            Some(home) if self.regions.contains_key(&home) => {
                                self.send_to_region(home, RegionInput::Event(kind));
                            }
                            _ => log::debug!("dropping input from {cid}: home not running"),
                        }
                    }
                    kind => {
                        let subscribed = self
                            .sessions
                            .get(&id)
                            .map_or(false, |s| s.subscribed.contains(&event.region_id));
                        if subscribed && self.regions.contains_key(&event.region_id) {
                            self.send_to_region(event.region_id, RegionInput::Event(kind));
                        } else {
                            log::debug!(
                                "dropping event from client {id} for unsubscribed region {:?}",
                                event.region_id
                            );
                        }
                    }
```

- Add the test hook on `InlineSpawner` (next to `kill`):

```rust
    /// Test support: run a closure against a live region (state assertions
    /// and teleports in headless harnesses).
    pub fn with_region(&mut self, id: RegionCoords, f: impl FnOnce(&mut Region)) {
        if let Some((_, runner)) = self.runners.get_mut(&id) {
            f(runner.region_mut());
        }
    }
```

with `RegionRunner::region_mut(&mut self) -> &mut Region` added in region_runner.rs (`pub fn region_mut(&mut self) -> &mut Region { &mut self.region }`). Import `Region` in world_manager.rs.

- [ ] **Step 4: Run tests**

Run: `cargo test -p game --test handoff_manager && cargo test -p game && cargo test -p server && cargo build --workspace --bins`
Expected: PASS (including the existing `threaded_world` and `webtransport_handshake` server suites).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(game): manager relays departures/ghosts, flips homes, routes input by home

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Client — predicted synthesis, home flip, reconcile matcher

The fully-predicted crossing: after each predicted tick the client synthesizes the same arrivals/ghost updates into its local sibling regions; reconcile's prediction-removal matcher goes identity-based; `PlayerRegion` becomes an idempotent "current home" signal.

**Files:**
- Modify: `crates/game/src/region.rs` (reconcile matcher: one line)
- Modify: `crates/client/src/main.rs` (`apply_local_transfers`, Tick arm, `PlayerRegion` handler guard)
- Test: extend `crates/game/tests/handoff_scan.rs` (reconcile matcher), extend `manager_tests` in `crates/client/src/main.rs`

**Interfaces:**
- Consumes: `World::take_transfers` (Task 4), `rebase_isometry`/`matches_prediction` (Task 1).
- Produces: `GameInstanceManager::apply_local_transfers(&mut self)` (private), called from the Tick arm between `progress_world_one_tick` and `update_window`.

- [ ] **Step 1: Reconcile matcher — failing test**

Append to `crates/game/tests/handoff_scan.rs`:

```rust
#[test]
fn reconcile_replaces_a_diverged_predicted_arrival_without_sticking() {
    // Client region B predicted an arrival with pose X; the authoritative
    // arrival has pose Y (server extracted on a different tick). Reconcile
    // must (a) end with the authoritative pose and (b) leave no unconfirmed
    // prediction stuck in the event log.
    let (_, src_key) = {
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(9);
        rb.forget();
        let key = *rb.data.player_entites.get(&9).unwrap();
        (rb, key)
    };
    let src = RegionCoords::new(0, 0);
    let id = RegionCoords::new(1, 0);
    let mk = |x: f32| game::EntityBundle {
        kind: game::EntityKind::Player,
        isometry: pose(x, 128.0),
        linvel: game::parry::math::Vector::zeros(),
        collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        has_camera: true,
        client: Some((9, game::Client::default())),
        source_region: src,
        source_key: src_key,
    };

    // Client-side region (local_client_id = Some → predictions reconcile).
    let mut region = Region::new(Rollback::new(None), None, id, Some(9));
    // Predict the arrival.
    region.handle_event(GameEventKind::EntityArrived(mk(2.0))).unwrap();
    // Authoritative copy arrives at the same event id with a different pose.
    region
        .reconcile(GameEvent::new(GameEventKind::EntityArrived(mk(4.0)), 0, id))
        .unwrap();

    assert!(
        region.pending_event_ids().is_empty(),
        "identity-matched prediction must be consumed, not stuck: {:?}",
        region.pending_event_ids()
    );
    let e = *region.data().player_entites.get(&9).unwrap();
    let handle = region.data().ecs.rigidbody.try_get(e).unwrap();
    let t = region.data().physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, Real::from(4.0), "authoritative pose won");
}
```

Run: `cargo test -p game --test handoff_scan reconcile_replaces` — Expected: FAIL (the prediction sticks or the count is wrong) because reconcile still matches on full equality.

- [ ] **Step 2: Fix the matcher**

`crates/game/src/region.rs`, in `reconcile`, the prediction-removal loop — find:

```rust
                                    if local_origin && e.kind == server_event.0.kind {
```

replace with:

```rust
                                    if local_origin && e.kind.matches_prediction(&server_event.0.kind) {
```

Run the test again: PASS. Run `cargo test -p game`: all PASS (exact-equality kinds behave identically).

- [ ] **Step 3: Client synthesis — failing tests**

Append to `manager_tests` in `crates/client/src/main.rs`:

```rust
    /// A predicted departure in one local region synthesizes a predicted
    /// arrival in the (loaded) target and flips home_region immediately.
    #[test]
    fn predicted_crossing_synthesizes_arrival_and_flips_home() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let target = RegionCoords::new(1, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        manager.pump(&server_recv).unwrap();

        // Load home (with the player) and the empty target region.
        let mut world = game::World::new();
        let mut home_region = Region::from_chunks(home, Vec::new());
        home_region.handle_event(GameEventKind::CreateClient(0)).unwrap();
        home_region.forget_last_event();
        world.load(&home, home_region);
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        let mut target_world = game::World::new();
        target_world.load(&target, Region::from_chunks(target, Vec::new()));
        server_send.send(target_world.build_region_server_packet(&target)).unwrap();
        manager.pump(&server_recv).unwrap();
        manager.is_caught_up = true;

        // Teleport the local player's predicted body past the boundary.
        {
            let w = manager.world.as_mut().unwrap();
            let key = *w.data(&home).player_entites.get(&0).unwrap();
            w.regions.get_mut(&home).unwrap().with_data(|d| {
                d.set_body_pose_safe(
                    key,
                    game::IsometryReal::from_parts(
                        game::na::Translation3::new(
                            (game::REGION_SIZE + game::FLIP_HYSTERESIS + 2.0).into(),
                            26.0f32.into(),
                            128.0f32.into(),
                        ),
                        game::na::Unit::<game::na::Quaternion<game::parry::math::Real>>::identity(),
                    ),
                )
            });
        }

        manager.send_tick();
        manager.pump(&server_recv).unwrap();

        assert_eq!(manager.home_region, Some(target), "home flipped on prediction");
        let w = manager.world.as_ref().unwrap();
        assert!(
            w.data(&target).player_entites.contains_key(&0),
            "predicted arrival applied in the target region"
        );
        assert!(
            !w.data(&home).player_entites.contains_key(&0),
            "predicted extraction removed the player from the old home"
        );
    }

    /// PlayerRegion after the first one must NOT re-request the whole 3x3
    /// window (that would resnapshot 9 regions on every crossing).
    #[test]
    fn player_region_push_updates_home_without_window_burst() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        manager.pump(&server_recv).unwrap();
        while manager.client_packet_recv().try_recv().is_ok() {}

        let target = RegionCoords::new(1, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(target), 0)).unwrap();
        manager.pump(&server_recv).unwrap();

        assert_eq!(manager.home_region, Some(target));
        let bursts: Vec<ClientPacket> = manager.client_packet_recv().try_iter().collect();
        assert!(
            !bursts.iter().any(|p| matches!(p, ClientPacket::RequestRegionConnection(_))),
            "no re-subscription burst on a home push: {:?}",
            bursts
        );
    }
```

Run: `cargo test -p client predicted_crossing` — Expected: FAIL (no synthesis; home unchanged). (`manager.world.regions` is `pub`; `with_data` is the Task 4 hook.)

- [ ] **Step 4: Implement in `crates/client/src/main.rs`**

- Tick arm of `handle_game_event`:

```rust
            GameEventKind::Tick => {
                self.world
                    .as_mut()
                    .unwrap()
                    .progress_world_one_tick(&mut self.results_buffer);
                // MUST run here and only here: transfer buffers reflect the
                // tick just executed (read-after-progress invariant), and a
                // predicted home flip must land before update_window recenters.
                self.apply_local_transfers();
                self.update_window();
            }
```

- New method on `GameInstanceManager`:

```rust
    /// Mirror of the server manager's relay, on the predicted timeline:
    /// drain this tick's transfer buffers and inject them into loaded
    /// sibling regions as predicted events. Targets outside the local
    /// window are dropped — their authoritative streams cover them.
    fn apply_local_transfers(&mut self) {
        let Some(world) = self.world.as_mut() else { return };
        let (departures, ghosts) = world.take_transfers();
        for (bundle, target) in departures {
            let source = bundle.source_region;
            let is_local_player =
                bundle.client.as_ref().map(|(c, _)| *c) == self.client_id && self.client_id.is_some();
            if world.region_exists(&target) {
                let mut b = bundle;
                b.isometry = game::rebase_isometry(&b.isometry, source, target);
                let _ = world.handle_region_event(GameEventKind::EntityArrived(b), target);
            }
            if is_local_player {
                // Predicted home flip: reroute input now; the server's
                // PlayerRegion push confirms (or corrects) it.
                self.home_region = Some(target);
            }
        }
        for (data, target) in ghosts {
            if world.region_exists(&target) {
                let mut d = data;
                d.isometry = game::rebase_isometry(&d.isometry, d.source_region, target);
                let _ = world.handle_region_event(GameEventKind::GhostUpdate(d), target);
            }
        }
    }
```

- `PlayerRegion` handler in `handle_server`: make the window burst first-time-only. Replace the body of the `game::ServerPacket::PlayerRegion(id, client_id)` arm's tail (after `self.client_id = Some(client_id);`) with:

```rust
                let home = id.unwrap_or(RegionCoords::new(0, 0));
                let first_home = self.home_region.is_none();
                self.home_region = Some(home);
                if first_home {
                    // Initial join: ask for the whole 3x3 window up front;
                    // update_window keeps it in sync from then on. Later
                    // pushes (handoff confirmations) must NOT re-burst —
                    // that would resnapshot nine regions per crossing.
                    for rc in home.window_3x3() {
                        self.server_game_send
                            .send(ClientPacket::RequestRegionConnection(rc))
                            .unwrap();
                        self.subscribed.insert(rc);
                    }
                }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p client && cargo test -p game && cargo build --workspace --bins`
Expected: PASS (38 existing client tests + the 2 new ones).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(client): fully-predicted handoff — local synthesis, home flip, identity reconcile

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Bridge — ghost render dedupe

`SetGhostSource` becomes a component; a system hides ghosts whose source region is loaded locally (the owned copy renders instead) and unhides them when it unloads.

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (`GhostSource` component, real `SetGhostSource` arm, `dedupe_ghosts` system + tests)
- Modify: `crates/client/src/renderer/mod.rs` (register `dedupe_ghosts` after the drain systems — locate the `add_systems` call that registers `drain_client_updates`/`drain_region_updates` and chain it)

**Interfaces:**
- Produces: `pub struct GhostSource(pub RegionId)` component; `dedupe_ghosts` system.
- Consumes: `SetGhostSource` emits (Task 3), `RegionRoots` (existing).

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/client/src/renderer/bridge.rs`:

```rust
    #[test]
    fn ghost_hidden_only_while_its_source_region_is_loaded() {
        let (mut app, client, updates, region_id) = test_app();
        app.add_systems(Update, dedupe_ghosts);
        app.update();

        let k = key(11);
        let src = RegionCoords::new(1, 0);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetGhostSource(k, Some(src)),
        )).unwrap();
        app.update();
        app.update(); // component insert lands a frame after spawn-in-same-drain

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        // Source region NOT loaded: ghost visible.
        assert_ne!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Hidden,
            "ghost renders while its source region is absent"
        );

        // Load the source region: the owned copy renders there; hide the ghost.
        let (_s2, recv2) = crossbeam::channel::unbounded();
        let rb2 = Rollback::new(None);
        client.send(ClientUpdateEvent::NewRegion(src, (*rb2.data).clone(), recv2)).unwrap();
        app.update();
        app.update();
        assert_eq!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Hidden,
            "ghost hides when its source region loads"
        );

        // Unload the source: ghost visible again.
        client.send(ClientUpdateEvent::RemoveRegion(src)).unwrap();
        app.update();
        app.update();
        assert_ne!(*app.world().entity(e).get::<Visibility>().unwrap(), Visibility::Hidden);

        // Upgrade clears the mark: never hidden again.
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetGhostSource(k, None),
        )).unwrap();
        client.send(ClientUpdateEvent::NewRegion(src, (*Rollback::new(None).data).clone(), {
            let (_s3, r3) = crossbeam::channel::unbounded();
            r3
        })).unwrap();
        app.update();
        app.update();
        assert_ne!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Hidden,
            "upgraded (owned) entity is never deduped"
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p client ghost_hidden`
Expected: FAIL to compile — `dedupe_ghosts` not found.

- [ ] **Step 3: Implement in bridge.rs**

Component (next to `SimKind`):

```rust
/// Marks a bevy entity as a ghost mirror sourced from another region. Used
/// only for render dedupe: when the source region is also loaded locally,
/// the owned copy renders and the ghost hides (sim state keeps both).
#[derive(Component, Clone, Copy)]
pub struct GhostSource(pub RegionId);
```

Replace the Task 1 stub arm in `drain_region_updates`:

```rust
                GameDataUpdateKind::SetGhostSource(key, src) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetGhostSource for unmapped {:?}", key);
                        continue;
                    };
                    match src {
                        Some(rc) => { commands.entity(e).insert(GhostSource(rc)); }
                        None => { commands.entity(e).remove::<GhostSource>(); }
                    }
                }
```

The dedupe system (after `drain_region_updates`):

```rust
/// Render rule: a ghost is visible only while its source region is NOT
/// loaded locally — with both regions in the window you see the owned copy,
/// not its mirror. Runs every frame; ghosts are few.
pub fn dedupe_ghosts(
    roots: Res<RegionRoots>,
    mut ghosts: Query<(&GhostSource, &mut Visibility)>,
) {
    for (src, mut vis) in ghosts.iter_mut() {
        let hidden = roots.0.contains_key(&src.0);
        let want = if hidden { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want {
            *vis = want;
        }
    }
}
```

Register in `crates/client/src/renderer/mod.rs` where the bridge systems are added — chain after the drains, e.g. `(bridge::drain_client_updates, bridge::drain_region_updates, bridge::dedupe_ghosts).chain()` (match the existing registration style; the test registers it manually so the unit test passes either way, but the plugin must register it for the real app).

- [ ] **Step 4: Run tests**

Run: `cargo test -p client && cargo build --workspace --bins && cargo build -p client --target wasm32-unknown-unknown`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(client): ghost render dedupe — hide mirrors when the source region is loaded

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Stage-1 acceptance, docs, cleanup

**Files:**
- Modify: `CLAUDE.md`, `TODO.md`
- No new code; fixes only if acceptance surfaces bugs.

- [ ] **Step 1: Full test sweep**

```bash
cargo test -p game && cargo test -p client && cargo test -p worldgen && cargo test -p server
cargo build --workspace --bins
cargo build -p client --target wasm32-unknown-unknown
```

Expected: all PASS.

- [ ] **Step 2: Threaded crossing smoke test**

Extend `crates/server/tests/threaded_world.rs` (read its existing scripted-client harness first and reuse its channel plumbing) with one test: real manager + region threads, a scripted client that connects, subscribes to `SPAWN_REGION.window_3x3()`, then teleports its player past the +x boundary using the same trick as `handoff_manager.rs` is NOT available here (threads own the regions) — instead drive the crossing with real input: send `PlayerInput(0, KeyE press)`, then `PlayerInput(0, KeyW held)` stamped to region (0,0), and wait (with a deadline of ~10 s wall time) until a `ServerPacket::PlayerRegion(Some(rc), 0)` with `rc == RegionCoords::new(0, -1)` arrives (W walks -z at 8 units/tick from z=128: ~17 ticks ≈ 0.9 s). Assert it arrives, and that a subsequent `ServerPacket::GameEvent` with `kind == EntityArrived(_)` for region (0, -1) was observed. Bound the wait loop; on deadline, fail with the packets seen so far.

Run: `cargo test -p server --test threaded_world`
Expected: PASS.

- [ ] **Step 3: Manual acceptance (server + one client, then two)**

```bash
cargo run --bin server &
sleep 2
WGPU_ADAPTER_NAME=6700 cargo run --bin client
```

Checklist:
1. Walk (not free-cam) toward a region boundary: crossing is seamless — no pose snap, no despawn flicker; the server log shows the `PlayerRegion` push.
2. After crossing, input still works instantly (home rerouted); the floor height changes (different region parity) confirming you're simulated in the new region.
3. Walk back: flip reverses. Oscillate on the line: no rapid flip-flops (hysteresis).
4. Second client (`cargo run --bin client` again): stand either side of a boundary — each sees the other (ghost mirror) with ~1-tick lag; walk apart >32 units from the line and the mirror despawns (TTL).
5. Kill one client; within ~0.5 s its ghost disappears from the other's view when the owner's region parks (or on TTL).
6. Offline wasm build: `?offline` roam across a boundary works single-threaded.

If any step fails: superpowers:systematic-debugging, fix, re-run Step 1.

- [ ] **Step 4: Docs**

- `CLAUDE.md` `crates/game` bullet: append a sentence — boundary handoff (`scan_boundaries`/`EntityArrived`/`GhostUpdate`, ghost mirrors with TTL, upgrade-in-place) lives in the tick; `PlayerInput` routes by the manager's `homes`, not the client's stamp.
- `TODO.md`: remove the "Entity/player handoff between regions" follow-up; add:

```markdown
- Ghost colliders (stage 2 of the handoff spec) if not yet landed.
- Cross-region interactions beyond collision (combat, pickup) — future spec.
- Ghost mirrors for parked-region persistence are TTL'd, not persisted.
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: entity handoff stage 1 — CLAUDE.md + TODO rollforward

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Stage 2 — ghost colliders (cross-boundary collision)

Ghosts gain kinematic bodies + colliders built from `GhostData.collider`; pose refreshes move the body. `apply_arrival`'s upgrade path already handles a body being present (Task 3 wrote both branches). Expiry already removes bodies via `remove_entity_safe`.

**Files:**
- Modify: `crates/game/src/state.rs` (`apply_ghost` only)
- Test: extend `crates/game/tests/handoff_state.rs`

**Interfaces:** unchanged — this is an internal behavior upgrade of `apply_ghost`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/game/tests/handoff_state.rs`:

```rust
#[test]
fn stage2_ghost_has_a_collidable_body_that_tracks_updates() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let mut rb = Rollback::new(None);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    let e = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;
    let handle = rb.data.ecs.rigidbody.try_get(e).expect("stage 2: ghost has a body");
    assert!(
        rb.data.physics.bodies.get(handle).unwrap().colliders().len() == 1,
        "ghost carries its collider"
    );

    // Refresh moves the body (and holds the hash bar).
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 240.0));
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, Real::from(240.0));
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

#[test]
fn stage2_upgrade_reuses_the_ghost_body() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let mut rb = Rollback::new(None);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    let e = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;
    let ghost_handle = rb.data.ecs.rigidbody.try_get(e).unwrap();

    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 245.0));
    rb.forget();
    let owned_handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    assert_eq!(ghost_handle, owned_handle, "upgrade corrects pose, keeps the body");
    let t = rb.data.physics.bodies.get(owned_handle).unwrap().translation();
    assert_eq!(t.x, Real::from(245.0));
}

#[test]
fn stage2_ghost_collider_blocks_a_walking_player() {
    // A player walking into a ghost must be collision-corrected (the same
    // KinematicCharacterController path terrain uses). Place a ghost dead
    // ahead of the player and drive one input tick forward.
    use game::{GameEventKind, InputEvent, Key, Region};
    let (_, src_key) = donor();
    let id = RegionCoords::new(0, 0);
    let mut region = Region::from_chunks(id, Vec::new());
    region.handle_event(GameEventKind::CreateClient(1)).unwrap();
    region.forget_last_event();
    let player = *region.data().player_entites.get(&1).unwrap();

    // Ghost 10 units in front of the player (player spawns at 128,26,128
    // facing -z; W walks -z at 8 units/tick).
    region
        .handle_event(GameEventKind::GhostUpdate(ghost(RegionCoords::new(0, -1), src_key, 0.0)))
        .unwrap();
    region.forget_last_event();
    // Reposition the ghost precisely via its entity.
    let ghost_e = region.data().ghosts.get(&(RegionCoords::new(0, -1), src_key)).unwrap().entity;
    region.with_data(|d| d.set_body_pose_safe(ghost_e, pose(128.0, 26.0, 118.0)));

    // fps-cam on (KeyE press+step happens in the tick), then hold W.
    for (key, pressed) in [(Key::KeyE, true), (Key::KeyW, true)] {
        region
            .handle_event(GameEventKind::PlayerInput(1, InputEvent::Key { key, pressed }))
            .unwrap();
        region.forget_last_event();
    }
    for _ in 0..3 {
        region.handle_event(GameEventKind::Tick).unwrap();
        region.forget_last_event();
    }
    let handle = region.data().ecs.rigidbody.try_get(player).unwrap();
    let z = region.data().physics.bodies.get(handle).unwrap().translation().z;
    // Unblocked: 128 - 3*8 = 104. The ghost capsule (radius 6.4) at z=118
    // must stop the player short of ~111.
    assert!(
        z.0 > 110.0,
        "ghost collider must block the walk: z={} (unblocked would be 104)",
        z.0
    );
}
```

(Exact `InputEvent` variant syntax: match what `player_input_flows_while_not_ready_once_caught_up` in client main.rs uses. If the camera controller needs a mouse-move to initialize rotation, the default identity rotation faces -z — sufficient.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test handoff_state stage2`
Expected: FAIL — stage-1 ghosts have no body.

- [ ] **Step 3: Implement — `apply_ghost` builds/moves a body**

In `apply_ghost` (state.rs): in the refresh branch, add after the isometry/emit lines:

```rust
            self.set_body_pose_safe(entry.entity, data.isometry);
```

(`set_body_pose_safe` no-ops when no body — safe during a mixed-version rollout of parked blobs.)

In the create branch, after the `SetGhostSource` send and before `ghosts.insert`:

```rust
            let body = RigidBodyBuilder::kinematic_position_based()
                .pose(data.isometry)
                .gravity_scale(Real::from(0.0))
                .enabled_rotations(true, true, true)
                .ccd_enabled(false)
                .angular_damping(Real::from(1.0))
                .can_sleep(false)
                .user_data(e.data().as_ffi() as u128)
                .build();
            let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
            let mut scope = self.data.physics.bodies.undo_scope();
            let handle = scope.insert(body);
            scope.register(move |bodies, _| bodies.revert_insert(handle, prev_head, prev_len));
            self.data.ecs.rigidbody.set_safe(e, Some(handle));
            match data.collider {
                ColliderSpec::CapsuleY { half_height, radius } => {
                    self.attach_capsule_collider_safe(e, handle, half_height, radius);
                }
            }
```

(This duplicates `inject_body_safe`'s chain with a `GhostData` source; if you factor a shared private helper taking `(pose, &ColliderSpec)`, both call sites must stay readable.)

- [ ] **Step 4: Run everything**

Run: `cargo test -p game && cargo test -p client && cargo test -p server && cargo build --workspace --bins && cargo build -p client --target wasm32-unknown-unknown`
Expected: PASS. Note: existing stage-1 tests must keep passing — the `arrival_upgrades_ghost_in_place` test's body assertion already tolerates a pre-existing body (Task 3 wrote `apply_arrival` with both branches).

- [ ] **Step 5: Manual acceptance**

Two clients either side of a boundary: walk into each other — you collide (each blocked by the other's ghost). Then re-run the full Task 8 checklist items 1–3 (crossings still seamless with colliders in play).

- [ ] **Step 6: Update TODO.md and commit**

Remove the stage-2 line from TODO.md.

```bash
git add -A
git commit -m "feat(game): stage 2 — ghost colliders enable cross-boundary collision

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Known risks (read before Task 1)

- **`UndoMap`/`UndoSlotMap` `remove()` availability** (Task 2): the vendored forks expose the exact inverses (`revert_remove`), but the macro wrappers may not surface `remove` yet. Adding it is in-scope for `crates/macros` (first-party); prove it with a `hash_restore.rs`-style test.
- **`b.linvel()` on kinematic bodies**: expected to hold the step-computed implied velocity; if it's zero, ship zero — `linvel` is informational (display/extrapolation), nothing in stage 1/2 consumes it.
- **`InputState` `PartialEq`** (Task 1): needed because `Client` rides in `EntityBundle` inside `GameEventKind`. If a nested type can't derive it (float payloads), implement it manually field-by-field — `OrderedFloat` is `Eq`, so this should stay derive-only.
- **`emit_on_undo` on `UndoMap`**: `ghosts.insert`/`remove` don't emit renderer events themselves; all render notifications in Task 3 go through explicit `send`/`emit_on_undo` on `data`/components. If `emit_on_undo` isn't available where written, attach it to the component whose `set_safe` follows (LIFO pairing, as `create_player_safe` does).
- **Reconcile depth**: the identity matcher (Task 6) is deliberately surgical. If reconcile misbehaves in ways the new test doesn't cover, do NOT restructure reconcile in this milestone — file it, keep the matcher change minimal.
- **Read-after-progress invariant** (Task 4): transfer buffers are replaced per tick and must be read only immediately after driving a tick. If a future refactor moves `apply_local_transfers` away from the Tick arm, duplicated predicted arrivals will appear — the invariant comment must survive refactors.
- **`PlayerRegion` semantics change** (Task 6): it is now a recurring "current home" signal. Anything that treated it as join-only (wasm `sim_driver`, netcode_web) should be grepped for `PlayerRegion` handling — the shared `GameInstanceManager::handle_server` is the only consumer today.
- **Bevy 0.18 drift**: `Visibility` toggling and `Query<&mut Visibility>` are stable in 0.18; check pinned docs, not memory, if anything moves.
