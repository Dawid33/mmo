//! Wire types for the world map (per-region tile summaries of the forged
//! world) and the Play request. Pure data: the server fills them from
//! `ForgedWorld` rasters, the client builds the picture.

use crate::biome::{Biome, RegionType};
use crate::protocol::{REGION_BLOCKS, REGION_CHUNKS, RegionCoords, WorldBlockPos, WorldChunkPos};

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
    /// The map wraps east–west: the region east of the last column is the
    /// first column ([`MapExtent::wrap_x`]).
    pub wrap_x: bool,
}

impl WorldMap {
    /// The regions the map covers.
    pub fn extent(&self) -> MapExtent {
        MapExtent {
            origin: self.origin,
            width: self.width,
            height: self.height,
            wrap_x: self.wrap_x,
        }
    }

    /// Whether `rc` is one of the map's regions.
    pub fn contains(&self, rc: RegionCoords) -> bool {
        self.extent().contains(rc)
    }

    /// The tile of `rc` (folded east–west on a wrapping map), or `None`
    /// outside the map.
    pub fn tile(&self, rc: RegionCoords) -> Option<&TileSummary> {
        let rc = self.extent().canonical(rc);
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
///
/// With `wrap_x` the map is a cylinder: x wraps east–west (the region east of
/// the last column is the first column) and only z ends at an outer seam. A
/// **canonical** region has its x folded into `[origin.x, origin.x + width)`;
/// without `wrap_x` every region is its own canonical form.
///
/// A wrapped map is at least [`MIN_WRAP_REGIONS`] wide; a narrower one is held
/// like an unwrapped one (its east and west neighbour would be one region).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct MapExtent {
    /// Min-x, min-z region.
    pub origin: RegionCoords,
    pub width: u32,
    pub height: u32,
    /// x wraps east–west.
    pub wrap_x: bool,
}

/// The narrowest map that wraps: below it the east and the west neighbour of
/// a region are one and the same (at 2, `delta` is +1 both ways, so a
/// hand-back is not the inverse of its departure).
pub const MIN_WRAP_REGIONS: u32 = 3;

/// `d` folded into `(-span/2, span/2]` (a tie is positive), `span > 0`: the
/// one wrap rule behind [`MapExtent::delta`] and [`nearest_x`].
fn fold_x(d: i64, span: i64) -> i64 {
    let d = d.rem_euclid(span);
    if d > span / 2 { d - span } else { d }
}

impl MapExtent {
    /// Whether x wraps (a map narrower than [`MIN_WRAP_REGIONS`] never folds
    /// anything).
    pub fn wraps(&self) -> bool {
        self.wrap_x && self.width >= MIN_WRAP_REGIONS
    }

    /// `rc` with x folded onto the map's columns when it wraps; z untouched
    /// (an off-map z stays off the map).
    pub fn canonical(&self, rc: RegionCoords) -> RegionCoords {
        if !self.wraps() {
            return rc;
        }
        let ox = i64::from(self.origin.x);
        let x = ox + (i64::from(rc.x) - ox).rem_euclid(i64::from(self.width));
        RegionCoords::new(x as i32, rc.z)
    }

    /// Whether `rc` is one of the map's regions (wrapped: any x, z in range).
    pub fn contains(&self, rc: RegionCoords) -> bool {
        let rc = self.canonical(rc);
        let dx = i64::from(rc.x) - i64::from(self.origin.x);
        let dz = i64::from(rc.z) - i64::from(self.origin.z);
        (0..i64::from(self.width)).contains(&dx) && (0..i64::from(self.height)).contains(&dz)
    }

    /// The canonical region `(dx, dz)` from `rc`, or `None` off the map.
    pub fn neighbour(&self, rc: RegionCoords, dx: i32, dz: i32) -> Option<RegionCoords> {
        let n = RegionCoords::new(rc.x.checked_add(dx)?, rc.z.checked_add(dz)?);
        self.contains(n).then(|| self.canonical(n))
    }

    /// Regions from `from` to `to`: z the plain difference; x the plain
    /// difference, or — wrapped — its shortest signed representative modulo
    /// `width` (in `(-width/2, width/2]`: a tie is positive, east).
    pub fn delta(&self, from: RegionCoords, to: RegionCoords) -> (i32, i32) {
        let mut dx = i64::from(to.x) - i64::from(from.x);
        if self.wraps() {
            dx = fold_x(dx, i64::from(self.width));
        }
        (dx as i32, to.z.wrapping_sub(from.z))
    }

    /// `pos` with x folded into the map's blocks
    /// (`[origin.x · REGION_BLOCKS, (origin.x + width) · REGION_BLOCKS)`) when
    /// it wraps; y and z untouched.
    pub fn canonical_block(&self, pos: WorldBlockPos) -> WorldBlockPos {
        if !self.wraps() {
            return pos;
        }
        let n = REGION_BLOCKS as i64;
        let lo = i64::from(self.origin.x) * n;
        let span = i64::from(self.width) * n;
        WorldBlockPos {
            x: lo + (pos.x - lo).rem_euclid(span),
            ..pos
        }
    }

    /// `pos` with x folded into the map's chunks, as [`Self::canonical_block`].
    pub fn canonical_chunk(&self, pos: WorldChunkPos) -> WorldChunkPos {
        if !self.wraps() {
            return pos;
        }
        let n = REGION_CHUNKS as i64;
        let lo = i64::from(self.origin.x) * n;
        let span = i64::from(self.width) * n;
        WorldChunkPos {
            x: lo + (pos.x - lo).rem_euclid(span),
            ..pos
        }
    }
}

/// Whether `rc` lies outside `map` (never, with no map: an unbounded world).
pub fn off_map(map: Option<MapExtent>, rc: RegionCoords) -> bool {
    map.is_some_and(|m| !m.contains(rc))
}

/// [`MapExtent::canonical`]; with no map (an unbounded plane) `rc` itself.
pub fn canonical(map: Option<MapExtent>, rc: RegionCoords) -> RegionCoords {
    map.map_or(rc, |m| m.canonical(rc))
}

/// [`MapExtent::neighbour`]; with no map the plain neighbour (`None` only
/// past the coordinate range).
pub fn region_neighbour(
    map: Option<MapExtent>,
    rc: RegionCoords,
    dx: i32,
    dz: i32,
) -> Option<RegionCoords> {
    match map {
        Some(m) => m.neighbour(rc, dx, dz),
        None => Some(RegionCoords::new(
            rc.x.checked_add(dx)?,
            rc.z.checked_add(dz)?,
        )),
    }
}

/// [`MapExtent::delta`]; with no map the plain difference.
pub fn region_delta(map: Option<MapExtent>, from: RegionCoords, to: RegionCoords) -> (i32, i32) {
    match map {
        Some(m) => m.delta(from, to),
        None => (to.x.wrapping_sub(from.x), to.z.wrapping_sub(from.z)),
    }
}

/// [`MapExtent::canonical_block`]; with no map `pos` itself.
pub fn canonical_block(map: Option<MapExtent>, pos: WorldBlockPos) -> WorldBlockPos {
    map.map_or(pos, |m| m.canonical_block(pos))
}

/// [`MapExtent::canonical_chunk`]; with no map `pos` itself.
pub fn canonical_chunk(map: Option<MapExtent>, pos: WorldChunkPos) -> WorldChunkPos {
    map.map_or(pos, |m| m.canonical_chunk(pos))
}

/// THE nearest-image rule of a wrapped map, in units of `per_region` per
/// region (blocks, chunks): the x difference `dx` folded into
/// `(-W·per_region/2, W·per_region/2]` (a tie is positive, east — the rule of
/// [`MapExtent::delta`]); `dx` itself with no map or one that does not wrap.
/// Every conversion of a world position into a region's local frame goes
/// through it ([`local_block`], [`local_chunk`]), so a canonical position and
/// an unwrapped one in the region's own frame land on the same local cell.
pub fn nearest_x(map: Option<MapExtent>, dx: i64, per_region: i64) -> i64 {
    match map {
        Some(m) if m.wraps() => fold_x(dx, i64::from(m.width) * per_region),
        _ => dx,
    }
}

/// World block `p` in `rc`'s block frame, the image nearest `rc` on a wrapped
/// map (outside `0..REGION_BLOCKS` for another region's block); the plain
/// difference otherwise.
pub fn local_block(map: Option<MapExtent>, rc: RegionCoords, p: WorldBlockPos) -> [i32; 3] {
    let n = REGION_BLOCKS as i64;
    [
        nearest_x(map, p.x - i64::from(rc.x) * n, n) as i32,
        p.y,
        (p.z - i64::from(rc.z) * n) as i32,
    ]
}

/// World chunk `c` in `rc`'s chunk frame, as [`local_block`].
pub fn local_chunk(map: Option<MapExtent>, rc: RegionCoords, c: WorldChunkPos) -> [i32; 3] {
    let n = REGION_CHUNKS as i64;
    [
        nearest_x(map, c.x - i64::from(rc.x) * n, n) as i32,
        c.y,
        (c.z - i64::from(rc.z) * n) as i32,
    ]
}

/// The canonical world block of local block `local` of `rc` (which may lie
/// past `rc`'s edges).
pub fn world_block(map: Option<MapExtent>, rc: RegionCoords, local: [i32; 3]) -> WorldBlockPos {
    canonical_block(
        map,
        WorldBlockPos::from_local(rc, crate::BlockPos::new(local[0], local[1], local[2])),
    )
}

/// The canonical world chunk of local chunk `local` of `rc`.
pub fn world_chunk(map: Option<MapExtent>, rc: RegionCoords, local: [i32; 3]) -> WorldChunkPos {
    canonical_chunk(map, WorldChunkPos::from_local(rc, local))
}
