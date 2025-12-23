use std::collections::BTreeSet;
use std::ops::{BitAndAssign, DerefMut};

use crate::input::WinitInput;
use crate::mesh::{Chunk, VoxelType};
use crate::na::{
    Complex, ComplexField, Isometry3, Matrix4, OPoint, Perspective3, Point3, Quaternion, RealField,
    Rotation, Rotation3, Translation3, Unit, Vector3, Vector4,
};
use crate::parry::math::HashableReal;
use crate::rapier::math::{Real, Vector};
use crate::rapier::prelude::{
    CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
    IslandManager, LockedAxes, MultibodyJointSet, NarrowPhase, QueryPipeline, RigidBodyBuilder,
    RigidBodyHandle, RigidBodySet,
};
use crate::taffy::style::BlockItemStyle;
use crate::taffy::TaffyTree;
use crate::{ChunkMesh, ClientUpdateEvent};
use crate::{GameDataTransactionKind, GameDataUpdate, GameError, IsometryReal, RegionId, Usize};
use borrow::Partial;
use crossbeam::channel::Sender;
pub use game_data::*;
use log::info;
use rollback::rollback;
use slotmapd::secondary::Iter;
use slotmapd::{new_key_type, Key, KeyData, SecondaryMap, SlotMap, SparseSecondaryMap};
use winit::keyboard::KeyCode;

new_key_type! { pub struct EntityKey; }
new_key_type! { pub struct PlayerKey; }

#[derive(Debug, Default, Clone)]
pub struct UIElement {
    style: crate::taffy::Style,
    content: Option<String>,
}

#[derive(Debug, rollback::serde::Serialize, rollback::serde::Deserialize, Clone, Partial, Hash)]
#[module(crate)]
pub struct Camera {
    pub opengl_to_wgpu_matrix: Matrix4<HashableReal>,
    pub proj_matrix: Perspective3<HashableReal>,
    pub view_matrix: Option<RigidBodyHandle>,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Player {
    pub input: WinitInput,
    pub fps_cam_mode: bool,
}

#[rollback(GameData)]
mod game_data {
    use super::*;
    use std::ops::Deref;

    pub struct GameData {
        ecs: Ecs,
        physics: PhysicsState,
        tick: usize,
        // menu: TaffyTree<()>,
        // gui: TaffyTree<()>,
        players: SlotMap<PlayerKey, EntityKey>,
    }

    pub struct Ecs {
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        player: Component<Player>,
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

impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        let key = self.entities.deref_mut().insert(());
        self.camera.insert(key, None);
        self.isometry.insert(key, None);
        self.rigidbody.insert(key, None);
        self.player.insert(key, None);
        self.chunk.insert(key, None);
        self.undo(move |d, s| {
            d.entities.remove(key);
            d.camera.remove(key);
            d.isometry.remove(key);
            d.rigidbody.remove(key);
            d.player.remove(key);
            s.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                crate::GameDataUpdateKind::RemoveEntity(key),
            ));
        });
        self.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            crate::GameDataUpdateKind::CreateEntity(key),
        ));
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
        + ::rollback::serde::Serialize
        + for<'a> ::rollback::serde::Deserialize<'a>,
{
    pub fn set_safe(&mut self, key: EntityKey, item: Option<T>) {
        let old = self.list.get(key).cloned().unwrap();
        self.undo(move |d, _| *d.list.get_mut(key).unwrap() = old.clone());

        if let Some(item) = item {
            self.list.get_mut(key).unwrap().replace(item);
        } else {
            self.list.get_mut(key).unwrap().take();
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
    pub fn create_mesh(&mut self) -> EntityKey {
        let e = self.ecs.create_entity_safe();

        let mut chunk = Chunk::default();
        // for x in &mut chunk.voxels {
        //     *x = VoxelType::Blue;
        // }
        self.ecs.chunk.set_safe(e, Some(chunk));
        e
    }

    pub fn create_player_safe(&mut self) -> PlayerKey {
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
        let handle = self.data.physics.bodies.insert(body);
        self.data.physics.undo(move |d, _| {
            d.bodies.remove(
                handle,
                &mut d.islands,
                &mut d.colliders,
                &mut d.implules_joint_set,
                &mut d.multi_body_joint_set,
                true,
            );
        });
        self.data.ecs.rigidbody.set_safe(e, Some(handle));
        self.data.ecs.camera.set_safe(e, Some(Camera::new(handle)));
        self.data.ecs.player.set_safe(
            e,
            Some(Player {
                fps_cam_mode: false,
                input: WinitInput::default(),
            }),
        );
        let key = self.data.players.insert(e);
        self.data.players.undo(move |d, _| {
            d.remove(key);
        });
        key
    }
}
