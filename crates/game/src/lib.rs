// #![deny(missing_docs)]
//! Game simulation code that is shared between client and server.

#[cfg_attr(test, macro_use)]
extern crate alloc;
pub extern crate nalgebra as na;
extern crate num_traits as num;
extern crate std;

use std::collections::BTreeMap;

mod camera;
mod physics;
mod region;
pub use parry3d as parry;

pub use rollback::common::*;
pub use rollback::{ChunkCoords, ChunkShape, GameData, Rollback, Undo, ASPECT};
pub use rollback::{
    ClientId, EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, PlayerKey,
    VoxelType,
};

pub const TICK_RATE: u64 = 50;
pub const INDUCED_LATENCY: isize = 0;

#[derive(Debug)]
pub enum ClientUpdateEvent {
    NewRegion(
        RegionId,
        GameData,
        crossbeam::channel::Receiver<GameDataUpdate>,
    ),
    GameCrash(GameError),
    SetPlayer(ClientId),
}

pub use region::Region;

trait Controller {
    fn on_tick<'a>(&mut self, _t: &mut Undo<GameData>) {}
}

pub struct World {
    pub regions: BTreeMap<ChunkCoords, Region>,
}

impl World {
    pub fn new() -> Self {
        return Self {
            regions: BTreeMap::new(),
        };
    }

    pub fn basic() -> Self {
        let one = ChunkCoords::new(0, 0, 0);
        let mut data = Region::new(Rollback::new(None), None, one);
        let _key = data.create_basic(one);

        return Self {
            regions: BTreeMap::from([(one, data)]),
        };
    }

    pub fn current_tick(&self, id: &RegionId) -> usize {
        self.regions.get(id).unwrap().current_tick()
    }

    pub fn region_exists(&self, id: &RegionId) -> bool {
        self.regions.contains_key(id)
    }

    pub fn data(&self, id: &RegionId) -> &Rollback {
        self.regions.get(id).unwrap().data()
    }

    pub fn load(&mut self, id: &RegionId, region: Region) {
        self.regions.insert(*id, region);
    }

    pub fn handle_region_event(
        &mut self,
        event: GameEventKind,
        region_id: RegionId,
    ) -> Result<GameEvent, GameError> {
        let region = self.regions.get_mut(&region_id).unwrap();
        region.handle_event(event)
    }

    pub fn progress_world_one_tick(
        &mut self,
        results: &mut BTreeMap<RegionId, Result<GameEvent, GameError>>,
    ) {
        results.clear();
        for (id, region) in &mut self.regions {
            results.insert(*id, region.handle_event(GameEventKind::Tick));
        }
    }

    /// Used by server
    pub fn forget_last_event(&mut self, region_id: &RegionId) {
        self.regions.get_mut(region_id).unwrap().forget_last_event();
    }

    pub fn reconcile_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let result = self
            .regions
            .get_mut(&event.region_id)
            .unwrap()
            .reconcile(event);
        result
    }

    pub fn find_player(&self, client: &ClientId) -> Option<RegionId> {
        let mut region = None;
        for (id, r) in &self.regions {
            if r.data().player_entites.contains_key(client) {
                region = Some(*id);
                break;
            };
        }
        region
    }

    pub fn build_region_server_packet(&self, region_id: &RegionId) -> ServerPacket {
        self.regions
            .get(&region_id)
            .unwrap()
            .build_region_server_packet(region_id)
    }

    pub fn get_region_data(&self, region_id: &RegionId) -> Rollback {
        self.regions.get(&region_id).unwrap().data().clone()
    }
}
