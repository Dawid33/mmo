//! The world map on the main thread. The server sends the forged world's
//! per-region tile summaries once on connect (`ClientUpdateEvent::WorldMap`);
//! the bridge forwards them as a [`WorldMapMsg`] and [`apply_world_map_msgs`] keeps [`WorldMapState`],
//! which the menu / map UI reads. A procedural or offline world never gets a
//! map, so `map` stays `None` there ("no world map").

use bevy::prelude::*;
use game::WorldMap;

/// What the server told us about the world map.
#[derive(Resource, Default)]
pub struct WorldMapState {
    pub map: Option<WorldMap>,
    /// The coarse-route map built from `map`, as the sim's regions route
    /// walks over it (`game::RouteMap::from_world_map`).
    pub route: Option<std::sync::Arc<game::RouteMap>>,
}

/// Bridge → [`WorldMapState`].
#[derive(Message, Debug, Clone)]
pub enum WorldMapMsg {
    Map(Box<WorldMap>),
}

pub fn apply_world_map_msgs(
    mut msgs: MessageReader<WorldMapMsg>,
    mut state: ResMut<WorldMapState>,
) {
    for msg in msgs.read() {
        match msg {
            WorldMapMsg::Map(map) => {
                state.route = Some(std::sync::Arc::new(game::RouteMap::from_world_map(map)));
                state.map = Some(WorldMap::clone(map));
            }
        }
    }
}
