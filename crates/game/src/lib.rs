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
    any::{Any, TypeId}, collections::BTreeMap, hash::Hasher, ops::Deref, rc::Rc, sync::{Arc, Mutex}, time::Instant, hash::Hash
};

mod camera;
mod common;
mod data;
mod input;
mod mesh;
pub mod parry;
pub(crate) use parry as parry3d;
mod physics;
pub mod rapier;
mod region;
pub mod taffy;

pub use crate::camera::ASPECT;
pub use crate::data::{EntityKey, GameData, PlayerKey, Rollback, UIElement, Undo};
pub use crate::mesh::{ChunkMesh, Vertex};
pub use crate::taffy::Style;
use crate::{mesh::ChunkVoxels, parry::math::{HashableReal, Real}};
use na::{Matrix4, Matrix4x2, Perspective3, RealField};
use rapier::prelude::{RigidBody, RigidBodyHandle};

trait DynHash {
    /// Feeds this value into the given [`Hasher`].
    fn dyn_hash(&self, state: &mut dyn Hasher);
}

impl<H: Hash + ?Sized> crate::DynHash for H {
    fn dyn_hash(&self, mut state: &mut dyn Hasher) {
        self.hash(&mut state);
    }
}

#[derive(Copy, Clone, Debug)]
pub enum GameDataTransactionKind {
    Do,
    Undo,
}

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

#[derive(Clone, Debug)]
pub enum GameDataUpdateKind {
    CreateUIElement(DefaultKey, UIElement, IsometryReal),
    SetUIElementStyle(DefaultKey, Style),
    SetUIElementContent(DefaultKey, Option<String>),
    RemoveUIElement(DefaultKey),
    SetVoxelComponent(EntityKey, Option<ChunkVoxels>),
    SetEntityPosition(EntityKey, IsometryReal),
    UpdateCameraViewProj(EntityKey, Perspective3<HashableReal>),
    UpdateCameraViewMatrix(EntityKey, IsometryReal),
    CreateEntity(EntityKey),
    RemoveEntity(EntityKey),
    SetFreeCam(EntityKey, bool),
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

pub type IsometryReal = na::Isometry<Real, na::Unit<na::Quaternion<Real>>, 3>;

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

    pub fn current_tick(&self, id: &usize) -> usize {
        *self.regions.get(id).unwrap().data.tick.deref()
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

    /// Used by server
    pub fn forget_last_event(&mut self, region_id: usize) {
        let region = self.regions.get_mut(&region_id).unwrap();
        region.data.forget();
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
        let id = self.regions.get(&region_id).unwrap().next_game_event_id;
        let data = self.clone_game_data(&region_id);
        ServerPacket::Region(region_id, data, id, player)
    }
}
