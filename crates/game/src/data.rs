use std::sync::{Arc, Mutex};

use rapier3d::{
    na::{Matrix4, Vector4},
    prelude::{RigidBody, RigidBodyHandle},
};
use serde::{Deserialize, Serialize};
use slotmap::{new_key_type, SlotMap};

use crate::{
    common::Tick, input::WinitInputHelper, physics::PhysicsState, EntityId, IsometryReal, Usize,
};
use derive_more::Debug;
use log::info;

#[derive(Clone, Debug)]
pub enum UpdateGameData {
    AddRigidBody(RigidBody),
    RemoveRigidBody(RigidBodyHandle),
    SetEntityRenderTransform(EntityId, IsometryReal),
    RemoveEntity(EntityId),
    SetCameraUniform(EntityId, IsometryReal),
    SetEntityPosition(EntityId, IsometryReal),
    UpdateEntityIsometry(EntityId, IsometryReal),
    SetTick(Tick),
}

#[derive(Debug, Default, Copy, Clone, serde::Serialize, serde::Deserialize)]
pub struct Camera {
    fovy: f32,
    znear: f32,
    zfar: f32,
    opengl_to_wgpu_matrix: Matrix4<f32>,
    pub proj_matrix: Matrix4<f32>,
}
impl Camera {
    pub fn new() -> Self {
        Camera {
            fovy: 90.0,
            znear: 0.1,
            zfar: 100.0,
            proj_matrix: Matrix4::<f32>::identity(),
            opengl_to_wgpu_matrix: Matrix4::from_columns(&[
                Vector4::new(1.0, 0.0, 0.0, 0.0),
                Vector4::new(0.0, 1.0, 0.0, 0.0),
                Vector4::new(0.0, 0.0, 0.5, 0.0),
                Vector4::new(0.0, 0.0, 0.5, 1.0),
            ]),
        }
    }
    pub fn build_projection(&mut self) -> Matrix4<f32> {
        let proj =
            rapier3d::na::Perspective3::new(self.fovy * 0.01745329, ASPECT, self.znear, self.zfar);
        let new = self.opengl_to_wgpu_matrix * proj.as_matrix();
        self.proj_matrix = new;
        return new;
    }
}

// TODO: Pass in aspect ration from user.
const ASPECT: f32 = (16 / 9) as f32;

new_key_type! { pub struct EntityKey; }
new_key_type! { pub struct PlayerKey; }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Player {
    pub input: WinitInputHelper,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Mesh {}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Component<T> {
    list: SlotMap<EntityKey, Option<T>>,
}

impl<T> Component<T> {
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn insert(&mut self, item: Option<T>) -> EntityKey {
        self.list.insert(item)
    }

    pub fn remove(&mut self, item: EntityKey) -> Option<T> {
        self.list.remove(item).unwrap()
    }

    pub fn set(&mut self, key: EntityKey, item: Option<T>) -> Option<T> {
        if let Some(item) = item {
            self.list.get_mut(key).unwrap().replace(item)
        } else {
            self.list.get_mut(key).unwrap().take()
        }
    }

    pub fn get(&self, key: EntityKey) -> &T {
        self.list.get(key).unwrap().as_ref().unwrap()
    }

    pub fn get_mut(&mut self, key: EntityKey) -> &mut T {
        self.list.get_mut(key).unwrap().as_mut().unwrap()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Ecs {
    pub camera: Component<Camera>,
    pub isometry: Component<IsometryReal>,
    pub rigidbody: Component<RigidBodyHandle>,
    pub player: Component<Player>,
    pub mesh: Component<Mesh>,
}

impl Ecs {
    pub fn add<'a>(&'a mut self) -> EntityKey {
        self.camera.insert(None);
        self.isometry.insert(None);
        self.rigidbody.insert(None);
        self.player.insert(None)
    }

    pub fn remove(&mut self, key: EntityKey) {
        self.camera.remove(key);
        self.isometry.remove(key);
        self.rigidbody.remove(key);
        self.player.remove(key);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameData {
    #[serde(skip)]
    #[debug(skip)]
    // pub log: Arc<Mutex<UndoLog>>,
    // #[debug(skip)]
    pub ecs: Ecs,
    #[debug(skip)]
    pub physics: PhysicsState,
    pub tick: Usize,
    pub players: SlotMap<PlayerKey, EntityKey>,
}

impl GameData {
    pub fn new() -> Self {
        let mut data = Self::default();
        data.set_log();
        data
    }
    pub fn set_log(&mut self) {
        // let mut fields = Vec::new();
        // self.__fields(&mut fields);
        // let log = Arc::new(Mutex::new(UndoLog::new(fields)));
        // self.set_undo_log(log.clone());
        // self.log = log;
    }
}
