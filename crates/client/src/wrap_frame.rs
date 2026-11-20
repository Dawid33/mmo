//! The client's unwrapped frame on a world that wraps east–west (a planet).
//!
//! Every region the server names, the sims hold and the client sends is
//! CANONICAL (x folded onto the map's columns, `game::MapExtent::canonical`).
//! What the player sees is not: walking east over the seam must not teleport
//! the camera a world's width back west. The client therefore views the world
//! in one unwrapped frame — the copy of each canonical region nearest the
//! frame's `anchor` ([`WrapFrame::alias`]) — and moves the anchor with the
//! home region ([`WrapFrame::follow_home`]), one region at a time, so the frame
//! never jumps. It is never re-centred onto the canonical columns: after two
//! laps of the largest world f32 positions still hold centimetres.
//!
//! The near window and the LOD rings are computed around `alias(home)` in this
//! frame; the near window is then de-duplicated to canonical regions for the
//! subscriptions, while LOD keys stay in the frame (the server answers each
//! under the key it was asked).
//!
//! With no map, or one that does not wrap, every method is the identity and
//! the frame is the canonical (world) frame.
//!
//! The game thread owns one (`GameInstanceManager`) and pushes it to the render
//! world on change (`ClientUpdateEvent::WrapFrame`), where it is this Bevy
//! resource, reset with the session. Everything the renderer places for a
//! region goes to [`WrapFrame::offset`]; a region's tree is placed once, at
//! its spawn (`renderer::bridge::RegionRoots::alias`). A loaded region's alias
//! never changes while the anchor follows the home (the near window is far
//! narrower than half the narrowest wrapping world), only when the extent
//! arrives — and then the client asks for every held region again, whose fresh
//! snapshot replaces its tree at the new alias.
use bevy::prelude::Resource;
use game::{MapExtent, RegionCoords};

/// The unwrapped frame around `anchor` on `extent` (see the module docs).
#[derive(Resource, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct WrapFrame {
    /// The world's extent (`None`: unbounded, or no map yet).
    pub extent: Option<MapExtent>,
    /// The frame's centre: the home region's copy in this frame.
    pub anchor: RegionCoords,
}

impl WrapFrame {
    /// The copy of `rc` (canonical or not) nearest `anchor`: its x differs from
    /// the anchor's by `MapExtent::delta` (in `(-width/2, width/2]`).
    pub fn alias(&self, rc: RegionCoords) -> RegionCoords {
        match self.extent.filter(MapExtent::wraps) {
            Some(m) => RegionCoords::new(self.anchor.x + m.delta(self.anchor, rc).0, rc.z),
            None => rc,
        }
    }

    /// World-space origin of `rc`'s copy in this frame (`alias(rc).world_offset()`).
    pub fn offset(&self, rc: RegionCoords) -> [f32; 3] {
        self.alias(rc).world_offset()
    }

    /// `rc`'s canonical form (`game::canonical`).
    pub fn canonical(&self, rc: RegionCoords) -> RegionCoords {
        game::canonical(self.extent, rc)
    }

    /// Regions from `from` to `to` by the nearest image (`game::region_delta`).
    pub fn delta(&self, from: RegionCoords, to: RegionCoords) -> (i32, i32) {
        game::region_delta(self.extent, from, to)
    }

    /// The canonical region that owns the point `(x, z)` of this frame.
    pub fn region_at_world(&self, x: f32, z: f32) -> RegionCoords {
        self.canonical(RegionCoords::from_world(x, z))
    }

    /// The home is `home` (canonical) now: the anchor becomes its copy nearest
    /// the old anchor, so the frame (and the camera in it) never jumps.
    pub fn follow_home(&mut self, home: RegionCoords) {
        self.anchor = self.alias(home);
    }

    /// The world's extent is `extent` now. Unchanged, nothing moves; a new one
    /// starts the frame at the canonical copy of the anchor.
    pub fn set_extent(&mut self, extent: Option<MapExtent>) {
        if self.extent == extent {
            return;
        }
        self.extent = extent;
        self.anchor = self.canonical(self.anchor);
    }
}
