#![feature(core_intrinsics)]
#![allow(unused, internal_features)]
// #![deny(missing_docs)]
//! Game simulation code that is shared between client and server.

use borrow::AsRefsHelper;
use crossbeam::channel::Sender;
use rapier3d::{
    na::{Matrix4, Matrix4x2, Perspective3},
    prelude::{RigidBody, RigidBodyHandle},
};
use rollback::rollback;
use slotmapd::new_key_type;
use std::{
    any::{Any, TypeId},
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};

mod camera;
mod common;
mod data;
mod input;
mod mesh;
mod physics;
mod region;
pub(crate) mod taffy;
mod transaction;

use crate::data::Undo;

pub const TICK_RATE: u64 = 50;
pub const DEFAULT_EVENT_BUFFER: isize = 5;

pub use crate::data::EntityKey;
pub use crate::data::ASPECT;
pub use crate::data::{PlayerKey, Rollback};
pub use crate::mesh::{ChunkMesh, Vertex};
pub use crate::{
    data::GameData, transaction::GameDataTransaction, transaction::GameDataTransactionKind,
};

pub enum ClientUpdateEvent {
    NewRegion(
        Rollback,
        Option<PlayerKey>,
        crossbeam::channel::Receiver<GameDataUpdate>,
    ),
    GameCrash(GameError),
}

pub use common::*;
use log::info;
pub use region::Region;

#[derive(Clone, Debug)]
pub enum GameDataUpdateKind {
    SetVoxelMesh(EntityKey, Option<ChunkMesh>),
    SetEntityPosition(EntityKey, IsometryReal),
    UpdateCameraViewProj(EntityKey, Perspective3<f32>),
    UpdateCameraViewMatrix(EntityKey, IsometryReal),
    CreateEntity(EntityKey),
    RemoveEntity(EntityKey),
    SetFreeCam(bool),
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

pub type IsometryReal =
    rapier3d::na::Isometry<f32, rapier3d::na::Unit<rapier3d::na::Quaternion<f32>>, 3>;

trait Controller {
    fn on_tick<'a>(&mut self, t: &mut Undo<GameData>) {}
}

/// A word is a collection of regions that communicate with one another
/// via IPC.
pub struct World {
    regions: BTreeMap<usize, Region>,
}

impl World {
    pub fn new() -> Self {
        return Self {
            regions: BTreeMap::new(),
        };
    }

    pub fn editor() -> (Self, PlayerKey) {
        let id = 0;
        let mut raw = Rollback::new(None);
        let mut data = Region::new(raw, None, id);
        let key = data.data.create_player_safe();
        data.data.create_mesh();
        return (
            Self {
                regions: BTreeMap::from([(id, data)]),
            },
            key,
        );
    }

    pub fn next_game_id(&self, id: &usize) -> usize {
        self.regions.get(id).unwrap().next_game_event_id
    }

    pub fn clone_game_data(&self, id: &usize) -> Rollback {
        self.regions.get(id).unwrap().data.clone()
    }

    pub fn data(&self, id: &usize) -> &Rollback {
        &self.regions.get(id).unwrap().data
    }

    pub fn load(&mut self, id: &usize, mut region: Region, next_game_event_id: usize) {
        region.next_game_event_id = next_game_event_id;
        self.regions.insert(*id, region);
    }

    pub fn handle_event(
        &mut self,
        event: GameEventKind,
        region_id: usize,
    ) -> Result<GameEvent, GameError> {
        // TODO: Move this code into region impl
        let region = self.regions.get_mut(&region_id).unwrap();
        let event = GameEvent::new(event, region.next_game_event_id, region_id);
        region.next_game_event_id += 1;
        let mut time = Instant::now();
        region.handle_event(event.clone())?;
        region.event_log.push_back(event.clone());
        // println!("elapsed {:?}", time.elapsed());
        Ok(event)
    }

    pub fn handle_event_server(
        &mut self,
        event: GameEventKind,
        region_id: usize,
    ) -> Result<GameEvent, GameError> {
        // TODO: Move this code into region impl
        let region = self.regions.get_mut(&region_id).unwrap();
        let event = GameEvent::new(event, region.next_game_event_id, region_id);
        region.next_game_event_id += 1;
        region.handle_event(event.clone())?;
        region.data.forget();
        Ok(event)
    }

    pub fn reconcile_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let t = Instant::now();
        let result = self
            .regions
            .get_mut(&event.region_id)
            .unwrap()
            .reconcile(event);
        // info!("{:?}", t.elapsed());
        result
    }

    pub fn build_region_server_packet(
        &self,
        region_id: usize,
        player: Option<PlayerKey>,
    ) -> ServerPacket {
        let id = self.regions.get(&region_id).unwrap().next_game_event_id;
        let data = self.clone_game_data(&region_id);
        ServerPacket::Region(region_id, data, id, player)
    }
}
