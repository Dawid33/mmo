//! Step edges: the top block of the HIGHER column of a one-block rise. Voxel
//! terrain carved that block into a slope (spec
//! `docs/superpowers/specs/2026-11-06-terrain-slopes-design.md`, "Placement
//! in worldgen"); stud terrain has no slope parts, but tree placement still
//! keeps trees off these columns, so the trees of a world stand where they
//! always did. The decision reads only samples — the column and its 8
//! neighbours — so it is a pure function of world block coordinates and
//! agrees in every window and across region seams.
use crate::{has_ground, ColSample};

/// Cardinal neighbour offsets `(dx, dz)`: the x axis first.
const CARDINALS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const DIAGONALS: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];

/// Whether column `c` is a step edge, given its neighbours `n(dx, dz)`.
/// Eligible only with ground, a dry surface (`s` above the water) and no
/// glacier ice (`top == s`). A neighbour is low when its top is exactly one
/// block under `c`'s (under water or not). One low cardinal (a straight
/// step), two adjacent ones (an outer corner), or none and exactly one low
/// diagonal (an inner corner) make a step edge; anything else (a ridge, a
/// nose, two diagonals, a 2+ block drop) does not.
pub(crate) fn step_edge(c: &ColSample, n: impl Fn(i32, i32) -> ColSample) -> bool {
    if !has_ground(c) || c.s <= c.water_top || c.top != c.s {
        return false;
    }
    let low = |(dx, dz): (i32, i32)| n(dx, dz).top == c.top - 1;
    let low_x = CARDINALS[..2].iter().filter(|&&d| low(d)).count();
    let low_z = CARDINALS[2..].iter().filter(|&&d| low(d)).count();
    match (low_x, low_z) {
        (1, 0) | (0, 1) | (1, 1) => true,
        (0, 0) => DIAGONALS.iter().filter(|&&d| low(d)).count() == 1,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const T: i32 = 100;
    const SEA: i32 = 63;

    fn col(top: i32) -> ColSample {
        ColSample { s: top, water_top: SEA, top, cover: None, trees: true, unbounded: false }
    }

    /// `step_edge` of column `c` whose neighbours are at `T` except those
    /// listed in `tops` (`(dx, dz) -> top`).
    fn edge_with(c: ColSample, tops: &[((i32, i32), i32)]) -> bool {
        let tops: BTreeMap<(i32, i32), i32> = tops.iter().copied().collect();
        step_edge(&c, |dx, dz| col(*tops.get(&(dx, dz)).unwrap_or(&T)))
    }

    fn edge(tops: &[((i32, i32), i32)]) -> bool {
        edge_with(col(T), tops)
    }

    #[test]
    fn one_low_cardinal_is_a_step_edge() {
        for d in CARDINALS {
            assert!(edge(&[(d, T - 1)]), "{d:?}");
            // Low diagonals do not change a straight step.
            assert!(edge(&[(d, T - 1), ((1, 1), T - 1), ((-1, -1), T - 1)]), "{d:?} with diagonals");
        }
    }

    #[test]
    fn two_adjacent_low_cardinals_are_a_step_edge() {
        assert!(edge(&[((1, 0), T - 1), ((0, -1), T - 1)]));
        assert!(edge(&[((0, 1), T - 1), ((-1, 0), T - 1), ((-1, 1), T - 1)]));
    }

    #[test]
    fn one_low_diagonal_alone_is_a_step_edge() {
        assert!(edge(&[((-1, 1), T - 1)]));
        assert!(edge(&[((1, -1), T - 1)]));
    }

    #[test]
    fn ridges_noses_and_two_diagonals_are_not() {
        assert!(!edge(&[((1, 0), T - 1), ((-1, 0), T - 1)]), "opposite");
        assert!(!edge(&[((0, 1), T - 1), ((0, -1), T - 1)]), "opposite on z");
        assert!(!edge(&[((1, 0), T - 1), ((-1, 0), T - 1), ((0, 1), T - 1)]), "three");
        assert!(!edge(&[((1, 0), T - 1), ((-1, 0), T - 1), ((0, 1), T - 1), ((0, -1), T - 1)]), "four");
        assert!(!edge(&[((1, 1), T - 1), ((-1, -1), T - 1)]), "two diagonals");
        assert!(!edge(&[]), "flat");
    }

    #[test]
    fn a_drop_of_two_blocks_is_a_cliff() {
        assert!(!edge(&[((1, 0), T - 2)]));
        assert!(!edge(&[((1, 1), T - 2)]));
        // A cliff beside a one-block step: the step still decides.
        assert!(edge(&[((1, 0), T - 2), ((0, 1), T - 1)]));
    }

    #[test]
    fn submerged_and_waterline_surfaces_are_not() {
        let mut wet = col(T);
        wet.water_top = T + 3;
        assert!(!edge_with(wet, &[((1, 0), T - 1)]), "under water");
        wet.water_top = T;
        assert!(!edge_with(wet, &[((1, 0), T - 1)]), "at the water surface");
    }

    #[test]
    fn glacier_ice_is_not() {
        let mut ice = col(T);
        ice.s = T - 4; // four blocks of ice over the ground
        assert!(!edge_with(ice, &[((1, 0), T - 1)]));
    }

    #[test]
    fn no_ground_no_step_edge() {
        let mut air = col(crate::SCAN_BOTTOM - 1);
        air.water_top = crate::SCAN_BOTTOM - 10;
        assert!(!step_edge(&air, |_, _| col(crate::SCAN_BOTTOM - 2)));
    }

    #[test]
    fn a_low_neighbour_under_water_still_counts() {
        // A shore step: the high column just above the sea, its neighbour's
        // surface under the water.
        let c = col(SEA + 1);
        assert!(step_edge(&c, |dx, dz| if (dx, dz) == (0, -1) { col(SEA) } else { col(SEA + 1) }));
    }
}
