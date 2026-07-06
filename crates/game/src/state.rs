//! The rollbackable simulation state: `GameData` plus the `#[rollback]`
//! invocation that generates the undo/transaction infrastructure
//! (`Rollback`, `Undo`, `UndoCell`, `UndoMap`, `UndoSlotMap`, ...) for it.
//! The generated code assumes `GameDataUpdate`/`GameDataUpdateKind`/
//! `GameDataTransactionKind` and `serde` are reachable at the crate root —
//! `lib.rs` re-exports this module to satisfy that.

use nalgebra as na;
use na::{Perspective3, Quaternion, Translation3, Unit};
use macros::rollback;
use block_mesh::ndshape::ConstShape;
use parry3d::math::{Point, RawReal, Real, Vector};
use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder, RigidBodyHandle};
use slotmapd::Key as _;
use slotmapd::{new_key_type, SlotMap, SparseSecondaryMap};
use std::sync::{Arc, atomic::AtomicUsize};

use crate::camera::Camera;
use crate::input::InputState;
use crate::protocol::{ColliderSpec, EntityBundle, GhostData, GHOST_TTL_TICKS};
use crate::voxel::{Chunk, ChunkCoords, ChunkShape, Voxel, VoxelType};

pub type IsometryReal = na::Isometry<Real, na::Unit<na::Quaternion<Real>>, 3>;

pub type ClientId = usize;

new_key_type! { pub struct EntityKey; }
new_key_type! { pub struct PlayerKey; }

#[derive(Clone, Debug, Default)]
pub struct RollbackInfo {
    pub current: Arc<AtomicUsize>,
    pub oldest: Arc<AtomicUsize>,
}

impl RollbackInfo {
    pub fn new() -> Self {
        Self {
            current: Arc::new(AtomicUsize::new(0)),
            oldest: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Clone, Debug)]
pub enum GameDataUpdateKind {
    SetVoxelComponent(EntityKey, Option<Vec<Voxel>>),
    SetEntityPosition(EntityKey, IsometryReal),
    AddCameraComponent(EntityKey, ClientId, Perspective3<Real>, IsometryReal),
    RemoveCameraComponent(EntityKey),
    UpdateCameraViewProj(EntityKey, Perspective3<Real>),
    UpdateCameraViewMatrix(EntityKey, IsometryReal),
    CreateEntity(EntityKey),
    RemoveEntity(EntityKey),
    SetFreeCam(ClientId, bool),
    SetEntityKind(EntityKey, Option<EntityKind>),
    /// Marks an entity as a ghost mirror of (source region, source key), or
    /// clears the mark on upgrade/expiry. The bridge uses it to hide ghosts
    /// whose source region is also loaded locally.
    SetGhostSource(EntityKey, Option<crate::protocol::RegionCoords>),
}

#[derive(Clone, Debug)]
pub struct GameDataUpdate {
    pub do_kind: GameDataTransactionKind,
    pub update_kind: GameDataUpdateKind,
}

impl GameDataUpdate {
    pub fn new(do_kind: GameDataTransactionKind, update_kind: GameDataUpdateKind) -> Self {
        Self {
            do_kind,
            update_kind,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum GameDataTransactionKind {
    Do,
    Undo,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash, PartialEq)]
#[module(crate)]
pub struct Client {
    pub input: InputState,
    pub fps_cam_mode: bool,
}

/// What an entity *is*, for rendering and future gameplay. The player is the
/// first kind; NPC kinds extend this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, Hash)]
pub enum EntityKind {
    #[default]
    Player,
}

/// A live ghost mirror in this region, keyed in `GameData::ghosts` by its
/// source identity. `last_update_tick` drives the TTL reaper.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash, PartialEq)]
#[module(crate)]
pub struct GhostEntry {
    pub entity: EntityKey,
    pub last_update_tick: usize,
}

#[rollback(GameData)]
mod game_data {
    use parry3d::math::{Real, Vector};
    use rapier3d::prelude::{
        CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
        IslandManager, MultibodyJointSet, NarrowPhase, RigidBodySet,
    };

    use super::{
        Camera, Chunk, Client, ClientId, Component, EntityKey, EntityKind, GhostEntry,
        IsometryReal, RigidBodyHandle, RollbackInfo, SlotMap,
    };
    use std::collections::BTreeMap;

    pub struct GameData {
        ecs: Ecs,
        physics: PhysicsState,
        #[undo(cell)]
        tick: usize,
        #[undo(cell)]
        next_game_event_id: usize,
        #[undo(map)]
        player_entites: BTreeMap<ClientId, EntityKey>,
        #[undo(map)]
        clients: BTreeMap<ClientId, Client>,
        /// Ghost mirrors hosted here, by source identity.
        #[undo(map)]
        ghosts: BTreeMap<(crate::RegionCoords, EntityKey), GhostEntry>,
        /// Owned entities that arrived via handoff, by the identity they
        /// arrived under — makes replayed arrivals idempotent.
        #[undo(map)]
        arrivals: BTreeMap<(crate::RegionCoords, EntityKey), EntityKey>,
    }

    pub struct Ecs {
        #[undo(slotmap)]
        #[emit(insert = CreateEntity, remove = RemoveEntity)]
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        chunk: Component<Chunk>,
        kind: Component<EntityKind>,
    }

    pub struct PhysicsState {
        bodies: RigidBodySet,
        broad_phase: DefaultBroadPhase,
        implules_joint_set: ImpulseJointSet,
        multi_body_joint_set: MultibodyJointSet,
        ccd_solver: CCDSolver,
        colliders: ColliderSet,
        gravity: Vector<Real>,
        integration_parameters: IntegrationParameters,
        islands: IslandManager,
        narrow_phase: NarrowPhase,
    }
}

pub use game_data::*;

impl GameData {
    pub fn into_game_update_iter(self) {}
}

impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        // CreateEntity / RemoveEntity emits ride the entities delta in both
        // directions — see #[emit] on the field.
        let key = self.entities.insert(());
        self.camera.insert_safe(key);
        self.isometry.insert_safe(key);
        self.rigidbody.insert_safe(key);
        self.chunk.insert_safe(key);
        self.kind.insert_safe(key);
        key
    }
}

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize, Hash)]
pub struct Component<T>
where
    T: 'static + Default,
{
    list: SparseSecondaryMap<EntityKey, Option<T>>,
}

impl<T> Undo<Component<T>>
where
    T: 'static
        + Default
        + Clone
        + Send
        + std::hash::Hash
        + ::serde::Serialize
        + for<'a> ::serde::Deserialize<'a>,
{
    /// Creates the (empty) component entry for a new entity, undo-safely.
    /// SparseSecondaryMap remove(insert) is hash-exact (see hash_restore.rs),
    /// so a removal closure is a true inverse here.
    pub fn insert_safe(&mut self, key: EntityKey) {
        self.undo(move |d, _| {
            d.list.remove(key);
        });
        self.raw_mut().list.insert(key, None);
    }

    pub fn set_safe(&mut self, key: EntityKey, item: Option<T>) {
        let old = self.list.get(key).cloned().unwrap();
        self.undo(move |d, _| *d.list.get_mut(key).unwrap() = old.clone());

        if let Some(item) = item {
            self.raw_mut().list.get_mut(key).unwrap().replace(item);
        } else {
            self.raw_mut().list.get_mut(key).unwrap().take();
        }
    }

    /// Removes an entity's slot from this component, undo-safely — the true
    /// inverse of `insert_safe` (do: remove the slot; undo: re-insert it with
    /// its prior value). `SparseSecondaryMap` insert(remove(x)) is hash-exact
    /// (see hash_restore.rs). Use this — not `set_safe(key, None)` — when an
    /// entity is destroyed: leaving a dangling `None` slot while the entities
    /// slotmap frees the index lets a same-transaction `create_entity_safe`
    /// reuse that index and clobber the slot, which is NOT LIFO-invertible
    /// (hash(before) != hash(after undo) on region-crossing reconcile).
    pub fn remove_safe(&mut self, key: EntityKey) {
        let old = self.list.get(key).cloned().unwrap();
        self.undo(move |d, _| {
            d.list.insert(key, old.clone());
        });
        self.raw_mut().list.remove(key);
    }
}

impl<T> Component<T>
where
    T: 'static + Default + Clone + Send,
{
    pub fn iter(&self) -> slotmapd::sparse_secondary::Iter<'_, EntityKey, Option<T>> {
        self.list.iter()
    }
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn insert(&mut self, e: EntityKey, item: Option<T>) {
        self.list.insert(e, item);
    }

    pub fn remove(&mut self, item: EntityKey) -> Option<T> {
        self.list.remove(item).unwrap()
    }

    pub fn get(&self, key: EntityKey) -> &T {
        self.list.get(key).unwrap().as_ref().unwrap()
    }

    pub fn try_get(&self, key: EntityKey) -> &Option<T> {
        self.list.get(key).unwrap()
    }

    pub fn get_mut(&mut self, key: EntityKey) -> &mut T {
        self.list.get_mut(key).unwrap().as_mut().unwrap()
    }
}

impl Rollback {
    pub fn create_mesh(&mut self, coords: ChunkCoords, chunk: Chunk) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        let voxels = crate::derive_voxels(&chunk.blocks, &chunk.chisel);
        // Deterministic linearize order; grid coords are body-local.
        let solid: Vec<Point<i32>> = (0..ChunkShape::SIZE)
            .filter(|i| voxels[*i as usize].kind != VoxelType::Air)
            .map(|i| {
                let [x, y, z] = ChunkShape::delinearize(i);
                Point::new(x as i32, y as i32, z as i32)
            })
            .collect();
        self.ecs.chunk.set_safe(e, Some(chunk));
        let position = IsometryReal::from_parts(
            Translation3::new(
                Real::from((coords.x * 32) as RawReal),
                Real::from((coords.y * 32) as RawReal),
                Real::from((coords.z * 32) as RawReal),
            ),
            Unit::<Quaternion<Real>>::identity(),
        );
        let body = RigidBodyBuilder::fixed()
            .pose(position)
            .user_data(e.data().as_ffi() as u128)
            .build();
        // Tier-2 with a true fork inverse: reverts the arena allocator state
        // exactly, no PhysicsState clone. See RigidBodySet::revert_insert.
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope();
        let handle = scope.insert(body);
        scope.register(move |bodies, _| bodies.revert_insert(handle, prev_head, prev_len));
        self.ecs.rigidbody.set_safe(e, Some(handle));
        if !solid.is_empty() {
            self.attach_voxels_collider_safe(e, handle, &solid);
        }
        e
    }

    /// Attach a voxel-grid collider to an entity's body. One cell per solid
    /// voxel; cell g spans [g, g+1) in body-local space, matching the render
    /// mesh. Entity-generic. Undo restores the whole PhysicsState snapshot —
    /// same rationale as attach_capsule_collider_safe.
    pub fn attach_voxels_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        coords: &[Point<i32>],
    ) {
        let collider = ColliderBuilder::voxels(Vector::repeat(Real::from(1.0)), coords)
            .user_data(e.data().as_ffi() as u128)
            .build();
        let p = self.data.physics.snapshot_raw();
        p.colliders.insert_with_parent(collider, body, p.bodies);
    }

    /// Attach a capsule collider to an entity's body. Entity-generic: player
    /// today, NPCs later. Undo restores the whole PhysicsState snapshot —
    /// the vendored fork has no exact ColliderSet inverse, and
    /// insert_with_parent also mutates the parent body's mass properties.
    pub fn attach_capsule_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        half_height: f32,
        radius: f32,
    ) {
        let collider = ColliderBuilder::capsule_y(Real::from(half_height), Real::from(radius))
            .user_data(e.data().as_ffi() as u128)
            .build();
        let p = self.data.physics.snapshot_raw();
        p.colliders.insert_with_parent(collider, body, p.bodies);
    }

    pub fn create_player_safe(&mut self, client_id: ClientId) {
        let e = self.ecs.create_entity_safe();
        // Spawn at the center of the 8x8-chunk floor (256x256 units = 16m x
        // 16m). Floor top y=8, capsule half-extent 14.4, ~3.6 units clear.
        let position = IsometryReal::from_parts(
            Translation3::new(Real::from(128.0), Real::from(26.0), Real::from(128.0)),
            Unit::<Quaternion<Real>>::identity(),
        );
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(position)
            .gravity_scale(Real::from(0.0))
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(Real::from(1.0))
            .can_sleep(false)
            .user_data(e.data().as_ffi() as u128)
            .build();
        // True fork inverse, same shape as in create_mesh.
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope();
        let handle = scope.insert(body);
        scope.register(move |bodies, _| bodies.revert_insert(handle, prev_head, prev_len));
        self.data.ecs.rigidbody.set_safe(e, Some(handle));
        // 28.8 units = 1.8 m at 1 unit = 1/16 m (~29 voxels, VS proportions).
        self.attach_capsule_collider_safe(e, handle, 8.0, 6.4);
        let cam = Camera::new(handle);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(e, client_id, cam.proj_matrix, position),
        ));
        // LIFO: registered before set_safe, so the notification fires after
        // the camera value is restored.
        self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::RemoveCameraComponent(e),
        ));
        self.data.ecs.camera.set_safe(e, Some(cam));
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(EntityKind::Player)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(EntityKind::Player));
        self.data.player_entites.insert(client_id, e);
    }

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
            self.data.ecs.camera.remove_safe(key);
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
            self.data.ecs.kind.remove_safe(key);
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
        // Remove every component slot this entity owns (symmetric to
        // create_entity_safe's insert_safe for camera/isometry/rigidbody/
        // chunk/kind). Removing the slots — not just nulling their values —
        // keeps the component maps in sync with the entities slotmap, so the
        // freed index can be reused later in the SAME transaction and still
        // roll back bit-exact.
        self.data.ecs.rigidbody.remove_safe(key);
        self.data.ecs.isometry.remove_safe(key);
        self.data.ecs.chunk.remove_safe(key);
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

    /// Insert a kinematic character body + its collider for entity `e`,
    /// undo-tracked (the create_player_safe insert pattern generalized). The
    /// shared build used by transfer arrivals (`inject_body_safe`) and stage-2
    /// ghost hosting (`apply_ghost`): both need the same kinematic body from a
    /// pose + collider spec, so the builder chain lives here, once.
    fn build_kinematic_body_safe(
        &mut self,
        e: EntityKey,
        pose: IsometryReal,
        collider: &ColliderSpec,
    ) {
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(pose)
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
        match collider {
            ColliderSpec::CapsuleY { half_height, radius } => {
                self.attach_capsule_collider_safe(e, handle, *half_height, *radius);
            }
        }
    }

    /// Body+collider insertion from a bundle — the create_player_safe insert
    /// pattern generalized to a transfer payload.
    fn inject_body_safe(&mut self, e: EntityKey, bundle: &EntityBundle) {
        self.build_kinematic_body_safe(e, bundle.isometry, &bundle.collider);
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
            // The entity persists across rollback (it was baked as a ghost in a
            // prior transaction), so on undo we must re-apply the ghost mark —
            // otherwise the renderer keeps a phantom owned mirror. Compensating
            // emit rides `kind` (a tracked field; the ghosts map has no
            // emit_on_undo), fires LIFO after state is restored. source_region
            // == identity.0.
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetGhostSource(entry.entity, Some(bundle.source_region)),
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

    /// Margin mirror upsert. Stage 2: the ghost carries a kinematic body +
    /// collider (built from `GhostData.collider`) so it participates in the
    /// receiving region's physics — cross-boundary collision. A pose refresh
    /// moves that body; the arrival upgrade path reuses it (set_body_pose vs
    /// re-inject), and expiry removes it via remove_entity_safe.
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
            // Move the ghost's body to match (no-ops if it has none — safe
            // during a mixed-version rollout of parked blobs).
            self.set_body_pose_safe(entry.entity, data.isometry);
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
            // Stage 2: a kinematic body + collider so the ghost collides.
            self.build_kinematic_body_safe(e, data.isometry, &data.collider);
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
            // remove_entity_safe's undo re-creates the entity (its slot #[emit]
            // fires CreateEntity on undo), so on rollback the ghost survives and
            // its mark must be restored. Registered BEFORE remove_entity_safe so
            // it replays LIFO-after the entity is re-created/re-mapped — else the
            // bridge would warn on an unmapped key. k.0 is the source region.
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetGhostSource(e, Some(k.0)),
            ));
            self.remove_entity_safe(e, None);
        }
    }

    /// The transferable shape spec of a body's first collider, or the player
    /// default. Non-capsule shapes don't cross the seam yet.
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
    pub fn extract_entity_safe(
        &mut self,
        key: EntityKey,
        region: crate::RegionCoords,
    ) -> crate::EntityBundle {
        let kind = (*self.data.ecs.kind.try_get(key)).unwrap_or_default();
        let handle = (*self.data.ecs.rigidbody.try_get(key))
            .expect("transferable entities have bodies");
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
    /// Returns `(departures, ghost updates)` with ABSOLUTE target coords.
    pub fn scan_boundaries(
        &mut self,
        region: crate::RegionCoords,
    ) -> (
        Vec<(crate::EntityBundle, crate::RegionCoords)>,
        Vec<(crate::GhostData, crate::RegionCoords)>,
    ) {
        let ghost_keys: std::collections::BTreeSet<EntityKey> =
            self.data.ghosts.iter().map(|(_, g)| g.entity).collect();
        // Read-only pass: gather leavers/ghosts before any mutation, so the
        // immutable borrows of physics/ecs don't collide with extraction.
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
        // Mutating pass: extraction is undo-tracked, one per leaver.
        let mut departures = Vec::new();
        for (key, target) in leavers {
            departures.push((self.extract_entity_safe(key, region), target));
        }
        (departures, ghosts)
    }
}
