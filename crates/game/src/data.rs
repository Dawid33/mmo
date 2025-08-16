use crossbeam::channel::Sender;
use rapier3d::na::Vector3;
use serde::{Deserialize, Serialize};

use crate::{
    camera::Camera, common::ClientUpdateEvent, common::RegionId, common::Tick,
    physics::PhysicsState, EntityId,
};

pub struct GameData {
    raw: GameDataRaw,
    id: RegionId,
    client: Option<Sender<ClientUpdateEvent>>,
}

pub enum UpdateGameData {
    CreateEntity(Entity),
    UpdateCamera(EntityId),
    RemoveEntity(EntityId),
    SetCameraVelocity(EntityId, f32, f32, f32),
}

impl GameData {
    pub fn new(data: GameDataRaw, client: Option<Sender<ClientUpdateEvent>>, id: RegionId) -> Self {
        Self {
            raw: data,
            client,
            id,
        }
    }

    pub fn raw<'a>(&'a self) -> &'a GameDataRaw {
        &self.raw
    }

    fn send(&mut self, e: UpdateGameData) {
        self.client
            .as_ref()
            .inspect(|c| c.send(ClientUpdateEvent::UpdateRegion(self.id, e)).unwrap());
    }

    pub fn tick(&mut self) {
        self.raw.tick += 1;
    }

    pub fn untick(&mut self) {
        self.raw.tick -= 1;
    }

    pub fn update_camera(&mut self, e: EntityId) {
        let camera = match &mut self.raw.entities.get_mut(e).unwrap().kind {
            EntityType::Camera(camera) => camera,
            _ => panic!("Tried to translate camera but entity isn't a camera"),
        };

        // use cgmath::InnerSpace;
        camera.position += camera.velocity;
        // let forward = camera.target - camera.eye;
        // let forward_norm = forward.normalize();

        // camera.eye += forward_norm * camera.velocity.x;
        // let right = forward_norm.cross(camera.up);
        // camera.eye += right * camera.velocity.z;

        // Redo radius calc in case the forward/backward is pressed.
        // let forward = camera.target - camera.eye;
        // let forward_mag = forward.magnitude();

        // if camera.velocity.z > 0.0 {
        //     camera.eye =
        //         camera.target - (forward + right * camera.velocity.z).normalize() * forward_mag;
        // } else {
        //     camera.eye =
        //         camera.target - (forward - right * camera.velocity.z).normalize() * forward_mag;
        // }
        camera.update_view_proj();
        self.send(UpdateGameData::UpdateCamera(e));
    }

    pub fn create_entity(&mut self, e: Entity) -> EntityId {
        let index = self.raw.entities.len();
        self.raw.entities.insert(index, e.clone());
        self.send(UpdateGameData::CreateEntity(e));
        index
    }

    pub fn remove_entity(&mut self, e: EntityId) {
        self.raw.entities.remove(e);
        self.send(UpdateGameData::RemoveEntity(e));
    }

    pub fn set_camera_velocity(&mut self, e: EntityId, x: f32, y: f32, z: f32) {
        let camera = match &mut self.raw.entities.get_mut(e).unwrap().kind {
            EntityType::Camera(camera) => camera,
            _ => panic!("Tried to translate camera but entity isn't a camera"),
        };
        camera.velocity = cgmath::Vector3::new(x, y, z);
        self.send(UpdateGameData::SetCameraVelocity(e, x, y, z));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Entity {
    pub kind: EntityType,
    pub position: Vector3<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityType {
    Layout,
    Camera(Camera),
    Text { content: String },
    Default,
}

impl Default for EntityType {
    fn default() -> Self {
        Self::Default
    }
}

impl EntityType {
    pub fn as_camera(&self) -> &Camera {
        match self {
            EntityType::Camera(camera) => camera,
            _ => panic!("Entity not camera"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub enum VoxelKind {
    Empty,
}

impl Default for VoxelKind {
    fn default() -> Self {
        VoxelKind::Empty
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Voxel {
    kind: VoxelKind,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct SimulationChunk {
    pub voxels: [[[Voxel; 4]; 4]; 4],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GameDataRaw {
    pub tick: Tick,
    pub chunk: SimulationChunk,
    pub entities: Vec<Entity>,
    pub physics: PhysicsState,
}

impl GameDataRaw {
    pub fn new() -> Self {
        Self {
            tick: 0,
            physics: PhysicsState::new(),
            entities: Vec::new(),
            chunk: SimulationChunk::default(),
        }
    }
}
