#![feature(core_intrinsics)]
#![allow(unused, internal_features)]
// #![deny(missing_docs)]
//! Game simulation code that is shared between client and server.

#[macro_use]
extern crate serde;
#[macro_use]
extern crate approx;

#[cfg_attr(test, macro_use)]
extern crate alloc;
pub extern crate nalgebra as na;
extern crate num_traits as num;
extern crate std;

use borrow::AsRefsHelper;
use crossbeam::channel::Sender;
use ordered_float::OrderedFloat;
use rollback::rollback;
use slotmapd::{new_key_type, DefaultKey};
use std::{
    any::{Any, TypeId},
    collections::BTreeMap,
    hash::Hash,
    hash::Hasher,
    ops::Deref,
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
pub use parry3d as parry;

pub use crate::camera::ASPECT;
pub use crate::data::{GameData, Rollback, UIElement, Undo};
pub use crate::mesh::ChunkShape;
use na::{Matrix4, Matrix4x2, Perspective3, RealField};
use parry3d::math::Real;
use rapier3d::prelude::{RigidBody, RigidBodyHandle};
pub use rollback::{
    EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, PlayerKey, VoxelType,
};

pub const TICK_RATE: u64 = 50;
pub const INDUCED_LATENCY: isize = 0;

#[derive(Debug)]
pub enum ClientUpdateEvent {
    NewRegion(
        usize,
        GameData,
        crossbeam::channel::Receiver<GameDataUpdate>,
    ),
    GameCrash(GameError),
    SetPlayer(PlayerKey),
}

pub use common::*;
use log::info;
pub use region::Region;

trait Controller {
    fn on_tick<'a>(&mut self, t: &mut Undo<GameData>) {}
}

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
        let mut data = Region::new(Rollback::new(None), None, 0);
        let key = data.create_basic(true).unwrap();
        let mut second = Region::new(Rollback::new(None), None, 1);
        second.create_basic(false);
        return (
            Self {
                regions: BTreeMap::from([(0, data), (1, second)]),
            },
            key,
        );
    }

    pub fn current_tick(&self, id: &usize) -> usize {
        self.regions.get(id).unwrap().current_tick()
    }

    pub fn data(&self, id: &usize) -> &Rollback {
        self.regions.get(id).unwrap().data()
    }

    pub fn load(&mut self, id: &usize, mut region: Region) {
        self.regions.insert(*id, region);
    }

    pub fn handle_region_event(
        &mut self,
        event: GameEventKind,
        region_id: usize,
    ) -> Result<GameEvent, GameError> {
        let region = self.regions.get_mut(&region_id).unwrap();
        region.handle_event(event)
    }

    pub fn progress_world_one_tick(
        &mut self,
        results: &mut BTreeMap<usize, Result<GameEvent, GameError>>,
    ) {
        results.clear();
        for (id, region) in &mut self.regions {
            results.insert(*id, region.handle_event(GameEventKind::Tick));
        }
    }

    /// Used by server
    pub fn forget_last_event(&mut self, region_id: usize) {
        self.regions
            .get_mut(&region_id)
            .unwrap()
            .forget_last_event();
    }

    pub fn reconcile_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let result = self
            .regions
            .get_mut(&event.region_id)
            .unwrap()
            .reconcile(event);
        result
    }

    pub fn build_region_server_packet(&self, region_id: usize, player: PlayerKey) -> ServerPacket {
        self.regions
            .get(&region_id)
            .unwrap()
            .build_region_server_packet(region_id, player)
    }
}
