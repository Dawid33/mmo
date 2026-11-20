//! Where generated terrain carries a slope (spec
//! `docs/superpowers/specs/2026-11-06-terrain-slopes-design.md`, "Placement
//! in worldgen"): the top block of the HIGHER column of a one-block rise is
//! carved into a `game::slope_chisel`; the lower column is untouched. The
//! decision reads only samples — the column and its 8 neighbours, never other
//! slopes — so it is a pure function of world block coordinates and agrees in
//! every window and across region seams.
use game::{Side, SlopeShape};

use crate::{ColSample, has_ground};

/// Cardinal sides, in the order the table checks them.
const CARDINALS: [Side; 4] = [Side::PosX, Side::NegX, Side::PosZ, Side::NegZ];
/// Diagonals as (x side, z side).
const DIAGONALS: [(Side, Side); 4] = [
    (Side::PosX, Side::PosZ),
    (Side::PosX, Side::NegZ),
    (Side::NegX, Side::PosZ),
    (Side::NegX, Side::NegZ),
];

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
