//! Which region host owns which part of the world: regions are grouped into
//! square super-regions of `s`×`s` regions, the unit a host claims and the
//! world store fences (owner epochs).

use crate::RegionCoords;

/// Regions per super-region side unless the world row says otherwise
/// (`world.super_region_regions`).
pub const DEFAULT_SUPER_REGION_REGIONS: u32 = 16;

/// The super-region holding `rc` with `s` regions per side (floor division,
/// so `(-1, -1)` is in `(-1, -1)`, never `(0, 0)`).
pub fn super_region_of(rc: RegionCoords, s: u32) -> (i32, i32) {
    let s = s.max(1) as i32;
    (rc.x.div_euclid(s), rc.z.div_euclid(s))
}
