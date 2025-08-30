#![feature(core_intrinsics)]
#![allow(unused, internal_features)]
// #![deny(missing_docs)]
//! Game simulation code that is shared between client and server.

use rapier3d::prelude::{RigidBody, RigidBodyHandle};
use rollback::rollback;
use slotmap::new_key_type;
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
mod physics;
mod region;
mod transaction;

pub use crate::data::{PlayerKey, Rollback};
pub use crate::{
    data::GameData, transaction::GameDataTransaction, transaction::GameDataTransactionKind,
};

pub use common::*;
use log::info;
pub use region::Region;

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

pub type IsometryReal =
    rapier3d::na::Isometry<f32, rapier3d::na::Unit<rapier3d::na::Quaternion<f32>>, 3>;

trait Controller {
    fn on_tick<'a>(&mut self, _t: &mut GameData) {}
    fn on_player_event<'a>(&mut self, _t: &mut GameData, _player: PlayerKey, _event: &WinitEvent) {}
}

/// A word is a collection of regions that communicate with one another
/// via IPC.
pub struct World {
    regions: BTreeMap<usize, Region>,
    last_game_event_id: usize,
}

impl World {
    pub fn new() -> Self {
        return Self {
            regions: BTreeMap::new(),
            last_game_event_id: 0,
        };
    }

    pub fn editor() -> (Self, PlayerKey) {
        let id = 0;
        let mut raw = Rollback::default();
        let mut t = GameDataTransaction::new(&mut raw, &None, id);
        let key = t.create_player();
        let data = Region::new(raw, None, id);
        return (
            Self {
                regions: BTreeMap::from([(id, data)]),
                last_game_event_id: 0,
            },
            key,
        );
    }

    pub fn clone_game_data(&self, id: &usize) -> Rollback {
        self.regions.get(id).unwrap().data.clone()
    }

    pub fn load(&mut self, id: &usize, region: Region, last_game_event_id: usize) {
        self.last_game_event_id = last_game_event_id;
        self.regions.insert(*id, region);
    }

    pub fn handle_event(
        &mut self,
        event: GameEventKind,
        region_id: usize,
    ) -> Result<GameEvent, GameError> {
        let event = GameEvent::new(event, self.last_game_event_id, region_id);
        self.last_game_event_id += 1;
        let region = self.regions.get_mut(&event._region_id).unwrap();
        let mut time = Instant::now();
        region.handle_event(event.clone())?;
        // println!("elapsed {:?}", time.elapsed());
        Ok(event)
    }

    pub fn reconcile_event(&mut self, _event: GameEvent) -> Result<(), GameError> {
        // self.regions
        //     .get_mut(&event._region_id)
        //     .unwrap()
        //     .reconcile(event)
        Ok(())
    }

    pub fn build_region_server_packet(
        &self,
        region_id: usize,
        player: Option<PlayerKey>,
    ) -> ServerPacket {
        ServerPacket::Region(
            region_id,
            self.clone_game_data(&region_id),
            self.last_game_event_id,
            player,
        )
    }
}
