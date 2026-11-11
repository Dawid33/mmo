//! Carving stud chunks: removing a box of cells and re-splitting what is left of the
//! bricks it cuts into smaller catalog parts (fillers into smaller fillers).
//!
//! Everything here is a pure function of the chunk and the box, so a carve replays
//! bit-exactly on every machine. A non-box part (slope…) touched by a carve is removed
//! whole. Bricks of an undiggable material (`hardness == u16::MAX`) are left alone.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::stud::{StudBrick, StudChunk, StudShape, PLATES_PER_BRICK};
use crate::stud_catalog::STUD_PARTS;
use crate::stud_material::stud_material;

/// Lengths tried along a strip, longest first.
const STRIP_LENGTHS: [u8; 6] = [8, 6, 4, 3, 2, 1];

/// The fully studded box part with this rotated footprint, if the catalog has one.
/// The lowest catalog index wins, so the choice is stable as parts are appended.
pub fn box_part(size: [u8; 3]) -> Option<(u16, u8)> {
    static TABLE: OnceLock<BTreeMap<[u8; 3], (u16, u8)>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let mut t = BTreeMap::new();
            for (i, p) in STUD_PARTS.iter().enumerate() {
                let [sx, sy, sz] = p.size;
                if !p.hull.is_empty() || p.studs.len() != sx as usize * sz as usize {
                    continue;
                }
                t.entry([sx, sy, sz]).or_insert((i as u16, 0));
                t.entry([sz, sy, sx]).or_insert((i as u16, 1));
            }
            t
        })
        .get(&size)
        .copied()
}

/// Tiles the box `min .. min + size` with catalog box parts: brick courses from the
/// bottom, plates for the rest, each course in strips two studs wide (one for an odd
/// rest) along its longer side.
pub fn fill_box(min: [u8; 3], size: [u8; 3], material: u16, data: u16, out: &mut Vec<StudBrick>) {
    let [w, h, d] = size;
    let mut y = 0;
    while y < h {
        let course = if h - y >= PLATES_PER_BRICK { PLATES_PER_BRICK } else { 1 };
        // `along_x`: strips run along x and stack across z.
        let along_x = w >= d;
        let (long, short) = if along_x { (w, d) } else { (d, w) };
        let mut s = 0;
        while s < short {
            let strip = if short - s >= 2 { 2 } else { 1 };
            let mut l = 0;
            while l < long {
                let rest = long - l;
                let piece = |len: u8| if along_x { [len, course, strip] } else { [strip, course, len] };
                let found = STRIP_LENGTHS
                    .iter()
                    .filter(|&&len| len <= rest)
                    .find_map(|&len| box_part(piece(len)).map(|(part, rot)| (len, part, rot)));
                let at = if along_x { [min[0] + l, min[1] + y, min[2] + s] } else { [min[0] + s, min[1] + y, min[2] + l] };
                match found {
                    Some((len, part, rot)) => {
                        out.push(StudBrick { min: at, shape: StudShape::Part { part, rot }, material, data });
                        l += len;
                    }
                    // The catalog always has 1 x 1 bricks and plates; a filler keeps the
                    // cells solid if it ever does not.
                    None => {
                        out.push(StudBrick { min: at, shape: StudShape::Filler { size: piece(1) }, material, data });
                        l += 1;
                    }
                }
            }
            s += strip;
        }
        y += course;
    }
}

/// `a \ b` for two boxes (`lo` inclusive, `hi` exclusive) as up to six boxes
/// `(min, size)`: the slabs under and over `b` first, then the x sides, then the z sides.
pub(crate) fn box_minus(a_lo: [u8; 3], a_hi: [u8; 3], b_lo: [u8; 3], b_hi: [u8; 3]) -> Vec<([u8; 3], [u8; 3])> {
    let mut out = Vec::new();
    let (mut lo, mut hi) = (a_lo, a_hi);
    for axis in [1, 0, 2] {
        if b_lo[axis] > lo[axis] {
            let mut h = hi;
            h[axis] = b_lo[axis];
            out.push((lo, [h[0] - lo[0], h[1] - lo[1], h[2] - lo[2]]));
            lo[axis] = b_lo[axis];
        }
        if b_hi[axis] < hi[axis] {
            let mut l = lo;
            l[axis] = b_hi[axis];
            out.push((l, [hi[0] - l[0], hi[1] - l[1], hi[2] - l[2]]));
            hi[axis] = b_hi[axis];
        }
    }
    out
}

impl StudChunk {
    /// Empties the cells `lo .. hi` (`hi` exclusive, chunk cells). Bricks cut by the box
    /// keep their cells outside it, re-split into smaller parts of the same material and
    /// `data`. Returns the removed cell count per material, ascending by material id;
    /// empty when the box met nothing diggable.
    pub fn carve(&mut self, lo: [u8; 3], hi: [u8; 3]) -> Vec<(u16, u32)> {
        if (0..3).any(|i| lo[i] >= hi[i]) {
            return Vec::new();
        }
        let hit: Vec<StudBrick> = self
            .bricks
            .values()
            .filter(|b| {
                let s = b.size();
                (0..3).all(|i| b.min[i] < hi[i] && lo[i] < b.min[i] + s[i])
                    && stud_material(b.material).is_some_and(|m| m.hardness != u16::MAX)
            })
            .copied()
            .collect();
        let mut removed: BTreeMap<u16, u32> = BTreeMap::new();
        let mut rest = Vec::new();
        for b in hit {
            self.bricks.remove(&b.min);
            let s = b.size();
            let b_hi = [b.min[0] + s[0], b.min[1] + s[1], b.min[2] + s[2]];
            let volume = |size: [u8; 3]| size.iter().map(|&v| v as u32).product::<u32>();
            if !b.is_box() {
                *removed.entry(b.material).or_default() += volume(s);
                continue;
            }
            // The cut, clamped to the brick.
            let c_lo = [lo[0].max(b.min[0]), lo[1].max(b.min[1]), lo[2].max(b.min[2])];
            let c_hi = [hi[0].min(b_hi[0]), hi[1].min(b_hi[1]), hi[2].min(b_hi[2])];
            *removed.entry(b.material).or_default() += volume([c_hi[0] - c_lo[0], c_hi[1] - c_lo[1], c_hi[2] - c_lo[2]]);
            for (min, size) in box_minus(b.min, b_hi, c_lo, c_hi) {
                match b.shape {
                    StudShape::Filler { .. } => {
                        rest.push(StudBrick { min, shape: StudShape::Filler { size }, material: b.material, data: b.data })
                    }
                    StudShape::Part { .. } => fill_box(min, size, b.material, b.data, &mut rest),
                }
            }
        }
        for b in rest {
            self.bricks.insert(b.min, b);
        }
        removed.into_iter().collect()
    }
}
