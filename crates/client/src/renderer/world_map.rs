//! The world map and Play refusals on the main thread. The server sends the
//! forged world's per-region tile summaries once on connect
//! (`ClientUpdateEvent::WorldMap`) and answers a refused Play request with
//! `ClientUpdateEvent::PlayRefused`; the bridge forwards both as
//! [`WorldMapMsg`]s and [`apply_world_map_msgs`] keeps [`WorldMapState`],
//! which the menu / map UI reads. A procedural or offline world never gets a
//! map, so `map` stays `None` there ("no world map").

use bevy::prelude::*;
use game::{PlayRefusal, RegionCoords, WorldMap};

/// What the server told us about the world map and our Play requests.
#[derive(Resource, Default)]
pub struct WorldMapState {
    pub map: Option<WorldMap>,
    /// The latest refused Play request; replaced by the next refusal.
    pub refused: Option<(RegionCoords, PlayRefusal)>,
    /// `Time::elapsed_secs` when `refused` arrived.
    pub refused_at_secs: f32,
}

/// How long a refusal stays on the status line.
pub const REFUSAL_SHOW_SECS: f32 = 5.0;

impl WorldMapState {
    /// The refusal still worth showing at `now_secs`.
    pub fn visible_refusal(&self, now_secs: f32) -> Option<PlayRefusal> {
        self.refused
            .filter(|_| now_secs - self.refused_at_secs < REFUSAL_SHOW_SECS)
            .map(|(_, why)| why)
    }
}

/// Bridge → [`WorldMapState`].
#[derive(Message, Debug, Clone)]
pub enum WorldMapMsg {
    Map(Box<WorldMap>),
    Refused(RegionCoords, PlayRefusal),
}

pub fn apply_world_map_msgs(
    mut msgs: MessageReader<WorldMapMsg>,
    mut state: ResMut<WorldMapState>,
    time: Res<Time>,
) {
    for msg in msgs.read() {
        match msg {
            WorldMapMsg::Map(map) => state.map = Some(WorldMap::clone(map)),
            WorldMapMsg::Refused(rc, why) => {
                state.refused = Some((*rc, *why));
                state.refused_at_secs = time.elapsed_secs();
            }
        }
    }
}
