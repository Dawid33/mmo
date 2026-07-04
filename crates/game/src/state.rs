//! The rollbackable simulation state: `GameData` plus the `#[rollback]`
//! invocation that generates the undo/transaction infrastructure
//! (`Rollback`, `Undo`, `UndoCell`, `UndoMap`, `UndoSlotMap`, ...) for it.
//! The generated code assumes `GameDataUpdate`/`GameDataUpdateKind`/
//! `GameDataTransactionKind` and `serde` are reachable at the crate root —
//! `lib.rs` re-exports this module to satisfy that.

use nalgebra as na;
use na::{Perspective3, Quaternion, Translation3, Unit};
use macros::rollback;
use parry3d::math::{RawReal, Real};
use rapier3d::prelude::{RigidBodyBuilder, RigidBodyHandle};
use slotmapd::Key as _;
use slotmapd::{new_key_type, SlotMap, SparseSecondaryMap};
use std::sync::{Arc, atomic::AtomicUsize};

use crate::camera::Camera;
use crate::input::InputState;
use crate::voxel::{Chunk, ChunkCoords, Voxel};

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

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Client {
    pub input: InputState,
    pub fps_cam_mode: bool,
}

#[rollback(GameData)]
mod game_data {
    use parry3d::math::{Real, Vector};
    use rapier3d::prelude::{
        CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
        IslandManager, MultibodyJointSet, NarrowPhase, RigidBodySet,
    };

    use super::{
        Camera, Chunk, Client, ClientId, Component, EntityKey, IsometryReal, RigidBodyHandle,
        RollbackInfo, SlotMap,
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
    }

    pub struct Ecs {
        #[undo(slotmap)]
        #[emit(insert = CreateEntity, remove = RemoveEntity)]
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        chunk: Component<Chunk>,
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
    pub fn create_mesh(&mut self, coords: ChunkCoords) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        let chunk = Chunk::default();
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
        e
    }

    pub fn create_player_safe(&mut self, client_id: ClientId) {
        let e = self.ecs.create_entity_safe();
        let position = IsometryReal::from_parts(
            Translation3::new(Real::from(0.0), Real::from(1.0), Real::from(5.0)),
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
        self.data.player_entites.insert(client_id, e);
    }
}
