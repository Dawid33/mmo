//! Wire types for the world map (per-region tile summaries of the forged
//! world) and the Play request. Pure data: the server fills them from
//! `ForgedWorld` rasters, the client builds the picture.

use crate::biome::{Biome, RegionType};
use crate::protocol::RegionCoords;

/// What one region looks like from above, summarised from the forged rasters.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TileSummary {
    /// Ground height over the tile, metres relative to sea level.
    pub ground_min_m: f32,
    pub ground_mean_m: f32,
    pub ground_max_m: f32,
    /// p90 - p10 of ground over the tile.
    pub relief_m: f32,
    /// Mean d(ground)/dx, d(ground)/dz (m per m), for client hillshade.
    pub slope: [f32; 2],
    /// 0..=1 of raster cells under sea.
    pub sea_fraction: f32,
    /// 0..=1 of raster cells with a lake surface.
    pub lake_fraction: f32,
    pub lake_level_m: Option<f32>,
    /// River reaches with a point inside the tile.
    pub rivers: u16,
    /// 0 when `rivers == 0`.
    pub river_max_discharge_m2: f32,
    pub river_max_width_m: f32,
    /// Index into [`WorldMap::rock_names`] (mode over the tile).
    pub surface_rock: Option<u8>,
    /// Region-local BLOCK column of the landing spot; `None` = no dry land.
    pub landing: Option<[u32; 2]>,
    /// The most common stored biome over the tile (lowest discriminant wins a tie).
    pub biome: Biome,
    /// The most common forge region over the tile (lowest id wins a tie): an
    /// index into [`WorldMap::region_types`].
    pub region: u16,
    /// Mean annual temperature at the ground over the tile, deg C.
    pub temperature_c: f32,
    /// Mean annual precipitation over the tile, mm/yr.
    pub precip_mm: f32,
    /// Mean savagery over the tile, 0..=100.
    pub savagery: u8,
    /// Alignment of `region`: -1 evil, 0 neutral, 1 good.
    pub alignment: i8,
    /// A volcano's summit lies in the tile.
    pub volcano: bool,
}

/// The whole world as a grid of tile summaries, row-major by z then x.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorldMap {
    /// Region of `tiles[0]` (min x, min z).
    pub origin: RegionCoords,
    /// Tiles per row / number of rows.
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<TileSummary>,
    pub rock_names: Vec<String>,
    /// The type of every forge region, by id ([`TileSummary::region`]).
    pub region_types: Vec<RegionType>,
}

impl WorldMap {
    /// The regions the map covers.
    pub fn extent(&self) -> MapExtent {
        MapExtent { origin: self.origin, width: self.width, height: self.height }
    }

    /// Whether `rc` is one of the map's regions.
    pub fn contains(&self, rc: RegionCoords) -> bool {
        self.extent().contains(rc)
    }

    /// The tile of `rc`, or `None` outside the map.
    pub fn tile(&self, rc: RegionCoords) -> Option<&TileSummary> {
        if !self.contains(rc) {
            return None;
        }
        let dx = i64::from(rc.x) - i64::from(self.origin.x);
        let dz = i64::from(rc.z) - i64::from(self.origin.z);
        self.tiles.get((dz * i64::from(self.width) + dx) as usize)
    }

    /// Region coordinates of `tiles[index]`.
    pub fn coords(&self, index: usize) -> RegionCoords {
        let w = self.width.max(1) as usize;
        RegionCoords::new(
            self.origin.x + (index % w) as i32,
            self.origin.z + (index / w) as i32,
        )
    }
}

/// The rectangle of regions a finite (forged) world covers: the playable
/// world ends at its outer seam. Region sims get it from the manager
/// (`RegionInput::SetRouteMap`) and the client from its `WorldMap`, so both
/// sides hold bodies and walks at that seam alike (`Region::set_route_map`).
/// A world without one (procedural) is unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapExtent {
    /// Min-x, min-z region.
    pub origin: RegionCoords,
    pub width: u32,
    pub height: u32,
}

impl MapExtent {
    pub fn contains(&self, rc: RegionCoords) -> bool {
        let dx = i64::from(rc.x) - i64::from(self.origin.x);
        let dz = i64::from(rc.z) - i64::from(self.origin.z);
        (0..i64::from(self.width)).contains(&dx) && (0..i64::from(self.height)).contains(&dz)
    }
}

/// Whether `rc` lies outside `map` (never, with no map: an unbounded world).
pub fn off_map(map: Option<MapExtent>, rc: RegionCoords) -> bool {
    map.is_some_and(|m| !m.contains(rc))
}

/// Why the server refused a `ClientPacket::PlayRegion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlayRefusal {
    NoWorldMap,
    OffMap,
    NoDryLand,
    AlreadyHere,
    /// The region's stored state could not be read from the world store; it
    /// is retried after `STORE_LOAD_RETRY_SECS`.
    RegionUnavailable,
}
