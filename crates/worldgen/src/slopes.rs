//! Where generated terrain carries a slope (spec
//! `docs/superpowers/specs/2026-11-06-terrain-slopes-design.md`, "Placement
//! in worldgen"): the top block of the HIGHER column of a one-block rise is
//! carved into a `game::slope_chisel`; the lower column is untouched. The
//! decision reads only samples — the column and its 8 neighbours, never other
//! slopes — so it is a pure function of world block coordinates and agrees in
//! every window and across region seams.
use game::{Side, SlopeShape};

use crate::{has_ground, ColSample};

/// Cardinal sides, in the order the table checks them.
const CARDINALS: [Side; 4] = [Side::PosX, Side::NegX, Side::PosZ, Side::NegZ];
/// Diagonals as (x side, z side).
const DIAGONALS: [(Side, Side); 4] =
    [(Side::PosX, Side::PosZ), (Side::PosX, Side::NegZ), (Side::NegX, Side::PosZ), (Side::NegX, Side::NegZ)];

fn is_x(s: Side) -> bool {
    matches!(s, Side::PosX | Side::NegX)
}

/// The slope column `c` carries, given its neighbours `n(dx, dz)`; `None` =
/// a whole top block. Eligible only with ground, a dry surface (`s` above the
/// water: a slope's open quarters would be air pockets under water) and no
/// glacier ice (`top == s`). A neighbour is low when its top is exactly one
/// block under `c`'s (under water or not). One low cardinal → Straight; two
/// adjacent → Outer; none and exactly one low diagonal → Inner; anything else
/// (a ridge, a nose, two diagonals, a 2+ block drop) → `None`.
pub(crate) fn slope_for(c: &ColSample, n: impl Fn(i32, i32) -> ColSample) -> Option<SlopeShape> {
    if !has_ground(c) || c.s <= c.water_top || c.top != c.s {
        return None;
    }
    let low = |[dx, dz]: [i32; 2]| n(dx, dz).top == c.top - 1;
    let mut cards = CARDINALS.into_iter().filter(|s| low(s.offset()));
    match (cards.next(), cards.next(), cards.next()) {
        (Some(low), None, None) => Some(SlopeShape::Straight { low }),
        (Some(a), Some(b), None) if is_x(a) != is_x(b) => {
            let (low_x, low_z) = if is_x(a) { (a, b) } else { (b, a) };
            Some(SlopeShape::Outer { low_x, low_z })
        }
        (None, _, _) => {
            let mut diags = DIAGONALS.into_iter().filter(|&(x, z)| {
                let (a, b) = (x.offset(), z.offset());
                low([a[0] + b[0], a[1] + b[1]])
            });
            match (diags.next(), diags.next()) {
                (Some((low_x, low_z)), None) => Some(SlopeShape::Inner { low_x, low_z }),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const T: i32 = 100;
    const SEA: i32 = 63;

    fn col(top: i32) -> ColSample {
        ColSample {
            s: top,
            water_top: SEA,
            top,
            ice_from: SEA + 1,
            cover: None,
            trees: true,
            unbounded: false,
        }
    }

    /// `slope_for` of a column at `T` whose neighbours are at `T` except
    /// those listed in `tops` (`(dx, dz) -> top`).
    fn shape_with(c: ColSample, tops: &[((i32, i32), i32)]) -> Option<SlopeShape> {
        let tops: BTreeMap<(i32, i32), i32> = tops.iter().copied().collect();
        slope_for(&c, |dx, dz| col(*tops.get(&(dx, dz)).unwrap_or(&T)))
    }

    fn shape(tops: &[((i32, i32), i32)]) -> Option<SlopeShape> {
        shape_with(col(T), tops)
    }

    #[test]
    fn one_low_cardinal_is_straight_toward_it() {
        for side in CARDINALS {
            let [dx, dz] = side.offset();
            assert_eq!(shape(&[((dx, dz), T - 1)]), Some(SlopeShape::Straight { low: side }), "{side:?}");
            // Low diagonals do not change a straight slope.
            assert_eq!(
                shape(&[((dx, dz), T - 1), ((1, 1), T - 1), ((-1, -1), T - 1)]),
                Some(SlopeShape::Straight { low: side }),
                "{side:?} with diagonals"
            );
        }
    }

    #[test]
    fn two_adjacent_low_cardinals_are_an_outer_corner() {
        assert_eq!(
            shape(&[((1, 0), T - 1), ((0, -1), T - 1)]),
            Some(SlopeShape::Outer { low_x: Side::PosX, low_z: Side::NegZ })
        );
        assert_eq!(
            shape(&[((0, 1), T - 1), ((-1, 0), T - 1), ((-1, 1), T - 1)]),
            Some(SlopeShape::Outer { low_x: Side::NegX, low_z: Side::PosZ })
        );
    }

    #[test]
    fn one_low_diagonal_alone_is_an_inner_corner() {
        assert_eq!(shape(&[((-1, 1), T - 1)]), Some(SlopeShape::Inner { low_x: Side::NegX, low_z: Side::PosZ }));
        assert_eq!(shape(&[((1, -1), T - 1)]), Some(SlopeShape::Inner { low_x: Side::PosX, low_z: Side::NegZ }));
    }

    #[test]
    fn ridges_noses_and_two_diagonals_stay_whole() {
        assert_eq!(shape(&[((1, 0), T - 1), ((-1, 0), T - 1)]), None, "opposite");
        assert_eq!(shape(&[((1, 0), T - 1), ((-1, 0), T - 1), ((0, 1), T - 1)]), None, "three");
        assert_eq!(shape(&[((1, 0), T - 1), ((-1, 0), T - 1), ((0, 1), T - 1), ((0, -1), T - 1)]), None, "four");
        assert_eq!(shape(&[((1, 1), T - 1), ((-1, -1), T - 1)]), None, "two diagonals");
        assert_eq!(shape(&[]), None, "flat");
    }

    #[test]
    fn a_drop_of_two_blocks_is_a_cliff() {
        assert_eq!(shape(&[((1, 0), T - 2)]), None);
        assert_eq!(shape(&[((1, 1), T - 2)]), None);
        // A cliff beside a one-block step: the step still decides.
        assert_eq!(shape(&[((1, 0), T - 2), ((0, 1), T - 1)]), Some(SlopeShape::Straight { low: Side::PosZ }));
    }

    #[test]
    fn submerged_and_waterline_surfaces_stay_whole() {
        let mut wet = col(T);
        wet.water_top = T + 3;
        assert_eq!(shape_with(wet, &[((1, 0), T - 1)]), None, "under water");
        wet.water_top = T;
        assert_eq!(shape_with(wet, &[((1, 0), T - 1)]), None, "at the water surface");
    }

    #[test]
    fn glacier_ice_stays_whole() {
        let mut ice = col(T);
        ice.s = T - 4; // four blocks of ice over the ground
        assert_eq!(shape_with(ice, &[((1, 0), T - 1)]), None);
    }

    #[test]
    fn no_ground_no_slope() {
        let mut air = col(crate::SCAN_BOTTOM - 1);
        air.water_top = crate::SCAN_BOTTOM - 10;
        assert_eq!(slope_for(&air, |_, _| col(crate::SCAN_BOTTOM - 2)), None);
    }

    #[test]
    fn a_low_neighbour_under_water_still_counts() {
        // A shore step: the high column just above the sea, its neighbour's
        // surface under the water.
        let c = col(SEA + 1);
        let shore = slope_for(&c, |dx, dz| {
            if (dx, dz) == (0, -1) {
                ColSample { water_top: SEA, ..col(SEA) }
            } else {
                col(SEA + 1)
            }
        });
        assert_eq!(shore, Some(SlopeShape::Straight { low: Side::NegZ }));
    }
}
