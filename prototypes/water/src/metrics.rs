//! Read-only measurements for reports and tests.

use std::ops::RangeInclusive;

use crate::state::{NO_BODY, World};
use crate::FULL;

const F: u64 = FULL as u64;

/// Height of the highest water in a column, in units (a member reports its body's level).
pub fn column_surface(w: &World, x: u32, z: u32) -> Option<u64> {
    for y in (0..w.dims.y).rev() {
        let i = w.dims.idx(x, y, z) as usize;
        match w.owner[i] {
            NO_BODY => {
                if w.water[i] > 0 {
                    return Some(y as u64 * F + w.water[i] as u64);
                }
            }
            id => return Some(w.bodies[&id].level()),
        }
    }
    None
}

/// Max minus min surface over the wet columns of a footprint.
pub fn surface_spread(w: &World, xs: RangeInclusive<u32>, zs: RangeInclusive<u32>) -> Option<u64> {
    let (mut lo, mut hi, mut any) = (u64::MAX, 0, false);
    for z in zs {
        for x in xs.clone() {
            if let Some(s) = column_surface(w, x, z) {
                lo = lo.min(s);
                hi = hi.max(s);
                any = true;
            }
        }
    }
    any.then(|| hi - lo)
}

/// The cell-only representation: one u16 per cell.
pub fn dense_bytes(w: &World) -> usize {
    w.dims.len() * 2
}

/// The proposed sparse layout: body headers + an 8-byte span per vertical run of
/// member cells + 6 bytes (index + amount) per CA cell holding water.
pub fn sparse_bytes(w: &World) -> usize {
    let d = w.dims;
    let (mut spans, mut ca_wet) = (0usize, 0usize);
    for z in 0..d.z {
        for x in 0..d.x {
            let mut inside = false;
            for y in 0..d.y {
                let i = d.idx(x, y, z) as usize;
                let member = w.owner[i] != NO_BODY;
                if member && !inside {
                    spans += 1;
                }
                inside = member;
                if w.water[i] > 0 {
                    ca_wet += 1;
                }
            }
        }
    }
    spans * 8 + ca_wet * 6 + w.bodies.values().map(|b| b.byte_size()).sum::<usize>()
}
