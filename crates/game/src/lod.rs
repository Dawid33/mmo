//! Far-terrain addressing shared by the server (builds streamed far terrain,
//! `stud_lod.rs`) and the client (renders it): the LOD factors, the ring table
//! and the `LodKey` tiling. Pure data code — no Bevy, no I/O.

use crate::{RegionCoords, REGION_SIZE};

/// Every streamable LOD factor. The server validates request factors
/// against this list (a stud world serves the ones `stud_lod_supported`). Each is
/// twice the last from 2 to 512 (128 and 512 added 2026-11-05 for the 15 km
/// horizon), so [`ring_table_for`] can double the cell per doubling of
/// distance.
pub const LOD_FACTORS: [u32; 9] = [2, 4, 8, 16, 32, 64, 128, 256, 512];

/// On-screen size a LOD cell may reach where its ring begins, in pixels, at
/// the reference view below. Far terrain only reads as the real thing while
/// its cells are about this small — the same budget Voxy's screen-space
/// subdivision enforces; coarser and every rounded-up surface is a visible
/// step. `ring_table_keeps_cells_within_the_pixel_budget` holds the table to it.
pub const LOD_MAX_CELL_PX: f32 = 2.5;
/// Reference view for [`LOD_MAX_CELL_PX`]: focal length in pixels of a 1080p
/// frame at the camera's 60° vertical FOV (`540 / tan 30°`).
pub const LOD_REFERENCE_FOCAL_PX: f32 = 935.0;

/// Ring table: `(chebyshev_bound, lod_factor)` with ascending bounds. A
/// region at Chebyshev distance `d` from the viewer region renders at the
/// factor of the first entry with `d <= bound`; None past the last bound.
///
/// The factor doubles when the distance doubles, so every ring starts at
/// roughly [`LOD_MAX_CELL_PX`] and holds a similar number of grids. (The
/// previous table quadrupled the factor per doubling and showed 3 / 6 / 13 px
/// cells in its outer rings.)
///
/// Bounds are in REGIONS, so the table belongs to a region size: the pixel
/// budget is about world distance. Each doubling of `REGION_BLOCKS` starts
/// every ring twice as far out, so the same bounds carry twice the factor —
/// the near window already covers what the finest ring used to — and the
/// horizon halves in regions to keep the same [`LOD_HORIZON_UNITS`] (16
/// regions of 1024, 8 of 2048, 4 of 4096, 2 of 8192). `ring_table_keeps_cells_within_the_pixel_budget`
/// checks whichever table the build ships, `ring_table_ends_at_the_horizon`
/// that it stops at the horizon.
///
/// The play sizes (256 / 512) reach 15 km since 2026-11-05 — an overcast
/// day's visibility (the client's haze is ~2 % contrast at 80 % of the reach,
/// `atmosphere::VISIBILITY_SHARE_OF_REACH`), with factors 128 and 512 added
/// so the cells keep doubling instead of jumping 64 → 256 (~4,400 keys at
/// 256 m regions, against 80 at the old 1 km); 64 / 128 — tests —
/// keep 1 km so their key counts and baselines do not move. Every
/// table equals [`ring_table_for`] at its horizon except 64's `(13, 16)` (the
/// rule gives 12), which predates the rule and is pinned by fixtures.
/// The client's `VOXEL_VIEW_DISTANCE_M` builds another one at run time.
pub const LOD_RING_TABLE: &[(i32, u32)] = match crate::REGION_BLOCKS {
    64 => &[(2, 2), (3, 4), (6, 8), (13, 16), (16, 32)],
    128 => &[(2, 4), (3, 8), (6, 16), (8, 32)],
    256 => &[(2, 8), (3, 16), (6, 32), (12, 64), (24, 128), (47, 256), (60, 512)],
    // Factor 32 a region out would be 3.7 px per cell, hence factor 16 to 2.
    512 => &[(2, 16), (3, 32), (6, 64), (12, 128), (24, 256), (30, 512)],
    _ => panic!("no LOD_RING_TABLE for this REGION_BLOCKS"),
};

/// How far the default LOD rings reach, in world units: the last ring's
/// Chebyshev bound × `REGION_SIZE` ([`ring_table_reach_units`]). It is also
/// the NEAREST the horizon edge can be (the viewer may stand on their
/// region's edge), which is where the client's fog goes opaque by default.
/// 1 km (16 384 units) at 64 / 128 m regions, 15,360 m at the play sizes.
pub const LOD_HORIZON_UNITS: f32 = if crate::REGION_BLOCKS >= 256 { 245_760.0 } else { 16_384.0 };

/// Reach of a ring table in world units: its last bound × `REGION_SIZE`
/// (0 for an empty table).
pub fn ring_table_reach_units(table: &[(i32, u32)]) -> f32 {
    table.last().map_or(0.0, |&(bound, _)| bound as f32 * REGION_SIZE)
}

/// The ring table that reaches `horizon_regions` regions while keeping every
/// ring within [`LOD_MAX_CELL_PX`] where it starts, as coarse as that allows:
/// each ring takes the coarsest [`LOD_FACTORS`] entry whose cell is within
/// budget at the ring's start (worst case: the viewer on their region's edge,
/// the first ring starting one region out, as
/// `ring_table_keeps_cells_within_the_pixel_budget` assumes) and lasts until
/// the next coarser factor fits. Factors only grow outward. Empty for
/// `horizon_regions < 1`.
pub fn ring_table_for(horizon_regions: i32) -> Vec<(i32, u32)> {
    // Nearest start distance (regions) at which `factor` is within budget.
    let fits_from = |factor: u32| -> i32 {
        let units = factor as f32 * LOD_REFERENCE_FOCAL_PX / LOD_MAX_CELL_PX;
        (units / REGION_SIZE).ceil() as i32
    };
    let mut table = Vec::new();
    let mut prev = 0;
    let mut i = 0;
    while prev < horizon_regions {
        let start = prev.max(1);
        while i + 1 < LOD_FACTORS.len() && fits_from(LOD_FACTORS[i + 1]) <= start {
            i += 1;
        }
        let next = LOD_FACTORS.get(i + 1).map_or(horizon_regions, |&f| fits_from(f));
        let bound = next.min(horizon_regions).max(prev + 1);
        table.push((bound, LOD_FACTORS[i]));
        prev = bound;
    }
    table
}

// The `LodKey` tiling contract (`tile_regions`, `cells_per_region`): every factor's 32-cell grid (32 * factor
// units) is either a whole number of regions (tile-keyed) or divides a region
// exactly (region-keyed, several grids per key). With `LOD_FACTORS` all
// powers of two that is "REGION_SIZE is a power of two"; `>= 32` keeps the
// factor-2 grid (64 units) inside a chunk pair the way the mesher expects.
const _: () = assert!(crate::REGION_BLOCKS.is_power_of_two() && crate::REGION_BLOCKS >= 64);

/// Hysteresis band for `ring_factor_in` (see that function).
pub const LOD_STICKY_BAND: i32 = 1;

/// Tile edge length in regions for a factor's `LodKey`: factors whose 32³
/// grid fits inside one region key per-region (the integer division
/// truncates to 0 and `max(1)` says "one region, several grids"); coarser
/// ones key per-tile so one key is exactly one grid footprint
/// (32 * factor == span * REGION_SIZE). At the default size that is 64/256
/// per-tile; at `REGION_BLOCKS = 256` factor 64 is per-region too.
pub fn tile_regions(factor: u32) -> i32 {
    ((32 * factor) as i32 / REGION_SIZE as i32).max(1)
}

/// Names one streamable LOD unit: a region, or a tile of
/// `tile_regions(factor)`² regions once a factor's 32-cell grid spans more
/// than one region (from 64 at 64 m regions, 256 at 256 m). `coords` are region coords
/// divided (euclid) by the tile span, so they're plain region coords for
/// the small factors.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, state_hash::StateHash,
    serde::Serialize, serde::Deserialize,
)]
pub struct LodKey {
    pub factor: u32,
    pub coords: [i32; 2],
}

impl LodKey {
    pub fn for_region(factor: u32, rc: RegionCoords) -> Self {
        let span = tile_regions(factor);
        Self { factor, coords: [rc.x.div_euclid(span), rc.z.div_euclid(span)] }
    }

    /// Tile edge in regions (1 for factors <= 16).
    pub fn region_span(&self) -> i32 {
        tile_regions(self.factor)
    }

    /// The minimum-corner region this key covers.
    pub fn origin_region(&self) -> RegionCoords {
        let span = self.region_span();
        RegionCoords::new(self.coords[0] * span, self.coords[1] * span)
    }

    /// Every region this key covers (span² of them).
    pub fn regions(&self) -> Vec<RegionCoords> {
        let o = self.origin_region();
        let span = self.region_span();
        let mut out = Vec::with_capacity((span * span) as usize);
        for dx in 0..span {
            for dz in 0..span {
                out.push(RegionCoords::new(o.x + dx, o.z + dz));
            }
        }
        out
    }

    /// World-space origin of this key's render root.
    pub fn world_offset(&self) -> [f32; 3] {
        self.origin_region().world_offset()
    }
}

/// Pure ring assignment: the factor a region at Chebyshev distance `dist`
/// should render at, given the factor it currently renders at (if any).
///
/// Fresh assignment picks the first table entry whose bound covers `dist`
/// (`None` past the last bound). A current assignment is sticky: its ring's
/// distance interval, widened by `band` on both edges, keeps claiming the
/// region — both promote (moved inward) and demote (moved outward) need the
/// distance to clear the boundary by more than `band`. The innermost ring has
/// no inner edge: the near/far exclusion sets bound it, not distance.
pub fn ring_factor_in(
    table: &[(i32, u32)],
    band: i32,
    dist: i32,
    current: Option<u32>,
) -> Option<u32> {
    if let Some(cur) = current {
        if let Some(i) = table.iter().position(|&(_, f)| f == cur) {
            let held_from = if i == 0 { i32::MIN } else { table[i - 1].0 + 1 - band };
            if dist >= held_from && dist <= table[i].0 + band {
                return Some(cur);
            }
        }
    }
    table.iter().find(|&&(bound, _)| dist <= bound).map(|&(_, f)| f)
}

/// x/z cells one region occupies in a grid at `factor` (16 at 64, 4 at 256;
/// 32 or more for the small factors, where a region spans whole grids).
pub fn cells_per_region(factor: u32) -> usize {
    (REGION_SIZE as u32 / factor) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_key_region_to_tile_mapping_handles_negatives() {
        use crate::RegionCoords;
        // Factors <= 16: key coords ARE the region coords.
        assert_eq!(LodKey::for_region(4, RegionCoords::new(-3, 7)).coords, [-3, 7]);
        assert_eq!(tile_regions(2), 1);
        assert_eq!(tile_regions(16), 1);
        // Factor 64: one 2048-unit grid — 2x2-region tiles at the default
        // region size; div_euclid so negatives round toward -inf.
        let s64 = tile_regions(64);
        assert_eq!(s64, (2048 / crate::REGION_SIZE as i32).max(1));
        assert_eq!(
            LodKey::for_region(64, RegionCoords::new(-1, 3)).coords,
            [(-1i32).div_euclid(s64), 3i32.div_euclid(s64)]
        );
        // Factor 256: 8192-unit grids, 8x8-region tiles at the default size.
        let s256 = tile_regions(256);
        assert_eq!(s256, (8192 / crate::REGION_SIZE as i32).max(1));
        assert_eq!(
            LodKey::for_region(256, RegionCoords::new(-9, 15)).coords,
            [(-9i32).div_euclid(s256), 15i32.div_euclid(s256)]
        );
    }

    #[test]
    fn lod_key_regions_and_world_offset() {
        use crate::{RegionCoords, REGION_SIZE};
        // `span` is 2 at the default region size (1 at 128 blocks).
        let span = tile_regions(64);
        let key = LodKey { factor: 64, coords: [-1, 1] };
        assert_eq!(key.origin_region(), RegionCoords::new(-span, span));
        let regions = key.regions();
        assert_eq!(regions.len(), (span * span) as usize);
        assert!(regions.contains(&RegionCoords::new(-span, span)));
        assert!(regions.contains(&RegionCoords::new(-1, 2 * span - 1)));
        let edge = span as f32 * REGION_SIZE;
        assert_eq!(key.world_offset(), [-edge, 0.0, edge]);
        // A tile grid spans exactly 32 cells * factor world units = span
        // regions — or, once a grid is smaller than a region (factor 64 at
        // 256 blocks), the key is one region of several grids.
        for factor in [64, 256] {
            let grid = 32 * factor;
            let span = tile_regions(factor as u32);
            if grid >= REGION_SIZE as i32 {
                assert_eq!(grid, span * REGION_SIZE as i32, "factor {factor}: whole regions");
            } else {
                assert_eq!(span, 1);
                assert_eq!(REGION_SIZE as i32 % grid, 0, "factor {factor}: whole grids");
            }
        }
        // Region-keyed factor: world offset == the region's own offset.
        let rk = LodKey::for_region(4, RegionCoords::new(3, -2));
        assert_eq!(rk.world_offset(), RegionCoords::new(3, -2).world_offset());
    }

    #[test]
    fn ring_table_is_ascending_and_factors_valid() {
        // Every bound must strictly ascend.
        let mut prev = -1;
        for &(bound, factor) in LOD_RING_TABLE {
            assert!(bound > prev, "bounds ascend");
            prev = bound;
            assert!(LOD_FACTORS.contains(&factor));
        }
    }

    /// Fixture for the `ring_factor_in` tests: they pin the assignment and
    /// hysteresis ALGORITHM, so they must not move with `LOD_RING_TABLE`, which
    /// is re-cut per `REGION_BLOCKS`. (This is the default-size table.)
    const RING_FIXTURE: &[(i32, u32)] = &[(2, 2), (3, 4), (6, 8), (13, 16), (27, 32), (32, 64)];

    #[test]
    fn ring_factor_fresh_assignment_over_all_distances() {
        for d in 0..=35 {
            let expect = match d {
                0..=2 => Some(2),
                3 => Some(4),
                4..=6 => Some(8),
                7..=13 => Some(16),
                14..=27 => Some(32),
                28..=32 => Some(64),
                _ => None,
            };
            assert_eq!(
                ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, d, None),
                expect,
                "dist {d}, no current"
            );
        }
    }

    #[test]
    fn ring_factor_sticky_band_holds_factor_4_one_past_its_bound() {
        // Factor 4's fresh band is d == 3; band 1 widens the held interval to
        // 2..=4 on both edges.
        for d in 0..=8 {
            let expect = match d {
                0..=1 => Some(2),
                2..=4 => Some(4),
                5..=6 => Some(8),
                7..=8 => Some(16),
                _ => unreachable!(),
            };
            assert_eq!(
                ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, d, Some(4)),
                expect,
                "dist {d}, current 4"
            );
        }
    }

    #[test]
    fn ring_factor_sticky_band_holds_factor_16_both_directions() {
        // Factor 16's fresh band is 7..=13; band 1 widens the held interval
        // to 6..=14.
        for d in 0..=15 {
            let expect = match d {
                0..=2 => Some(2),
                3 => Some(4),
                4..=5 => Some(8),
                6..=14 => Some(16),
                15 => Some(32),
                _ => unreachable!(),
            };
            assert_eq!(
                ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, d, Some(16)),
                expect,
                "dist {d}, current 16"
            );
        }
    }

    #[test]
    fn ring_table_keeps_cells_within_the_pixel_budget() {
        // Worst case for a ring: the viewer stands on the edge of their own
        // region, so a ring whose inner neighbour ends at `prev` regions
        // starts `prev` whole regions away.
        // Near radius 1 (3x3) is the tightest window any preset runs a full
        // table with; the production 5x5 only pushes the first ring further out.
        let mut prev = 1;
        for &(bound, factor) in LOD_RING_TABLE {
            let start = prev as f32 * REGION_SIZE;
            let px = factor as f32 / start * LOD_REFERENCE_FOCAL_PX;
            assert!(
                px <= LOD_MAX_CELL_PX,
                "factor {factor} starts {start} voxels out: {px:.2} px per cell"
            );
            prev = bound;
        }
    }

    #[test]
    fn ring_table_ends_at_the_horizon() {
        assert_eq!(ring_table_reach_units(LOD_RING_TABLE), LOD_HORIZON_UNITS);
    }

    #[test]
    fn generated_ring_table_matches_the_shipped_one() {
        let &(horizon, _) = LOD_RING_TABLE.last().unwrap();
        let generated = ring_table_for(horizon);
        if crate::REGION_BLOCKS == 64 {
            // The one hand-cut table: factor 16 ends at 13, the rule says 12.
            assert_eq!(generated, [(2, 2), (3, 4), (6, 8), (12, 16), (16, 32)]);
        } else {
            assert_eq!(generated, LOD_RING_TABLE);
        }
    }

    #[test]
    fn generated_ring_tables_keep_the_pixel_budget_and_reach_the_horizon() {
        assert!(ring_table_for(0).is_empty());
        for horizon in 1..=64 {
            let table = ring_table_for(horizon);
            assert_eq!(table.last().unwrap().0, horizon, "horizon {horizon}: {table:?}");
            let mut prev = 1;
            let mut last_factor = 0;
            for &(bound, factor) in &table {
                assert!(bound >= prev && factor > last_factor, "{table:?}");
                let px = factor as f32 / (prev as f32 * REGION_SIZE) * LOD_REFERENCE_FOCAL_PX;
                assert!(px <= LOD_MAX_CELL_PX, "horizon {horizon}: factor {factor} at {prev}: {px:.2} px");
                assert!(LOD_FACTORS.contains(&factor));
                prev = bound;
                last_factor = factor;
            }
        }
    }

    #[test]
    fn ring_factor_unknown_current_falls_back_to_fresh() {
        // A current factor not in the table (table shrank between sessions,
        // say) gets no stickiness — plain fresh assignment.
        assert_eq!(ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, 3, Some(99)), Some(4));
        assert_eq!(ring_factor_in(RING_FIXTURE, LOD_STICKY_BAND, 33, Some(99)), None);
    }
}
