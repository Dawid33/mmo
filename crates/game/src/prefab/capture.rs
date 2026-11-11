//! The editor's save: the bricks of a world around an anchor, minus the ground they
//! stand on, as a Build file.
//!
//! What counts as the prefab's: every brick that is not an untouched brick of the
//! pristine ground, except natural-material cells where the pristine ground has that
//! same material — ground a dig merely re-bricked (`stud_rebrick`) or that was dug and
//! filled back is still ground. Removed ground is not recorded (carve boxes are kept
//! from the opened file, not authored here).

use std::collections::{BTreeMap, BTreeSet};

use super::resolve::material_name;
use super::{
    BuildBoxFile, BuildFile, BuildPartFile, CellBox, Marker, PREFAB_HALF_STUDS, PREFAB_MAX_BOXES, PREFAB_MAX_PARTS,
    PREFAB_MAX_PLATE, PREFAB_MIN_PLATE,
};
use crate::stud_catalog::STUD_PARTS;
use crate::{StudChunk, StudShape, WorldChunkPos, NATURAL_BASE, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS};

/// Greedy boxes `(min, size)` covering `cells` exactly, disjoint: from the lowest
/// remaining cell (in `[x, y, z]` order) along x, then whole rows along z, then whole
/// slabs along y. A fixed point: merging the cells of its own output gives it again.
pub fn merge_boxes(mut cells: BTreeSet<[i32; 3]>) -> Vec<([i32; 3], [i32; 3])> {
    let mut out = Vec::new();
    while let Some(&s) = cells.iter().next() {
        let mut e = [1, 1, 1];
        while cells.contains(&[s[0] + e[0], s[1], s[2]]) {
            e[0] += 1;
        }
        let row = |y: i32, z: i32, e0: i32, set: &BTreeSet<[i32; 3]>| (0..e0).all(|x| set.contains(&[s[0] + x, y, z]));
        while row(s[1], s[2] + e[2], e[0], &cells) {
            e[2] += 1;
        }
        while (0..e[2]).all(|z| row(s[1] + e[1], s[2] + z, e[0], &cells)) {
            e[1] += 1;
        }
        for x in 0..e[0] {
            for y in 0..e[1] {
                for z in 0..e[2] {
                    cells.remove(&[s[0] + x, s[1] + y, s[2] + z]);
                }
            }
        }
        out.push((s, e));
    }
    out
}

/// Why a world cannot be saved as a prefab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    /// A chunk under the prefab has ground but is not there yet: saving now would
    /// leave its bricks out.
    ChunkNotLoaded(WorldChunkPos),
    TooManyParts(usize),
    /// Boxes plus the kept carve boxes.
    TooManyBoxes(usize),
    /// A brick lies outside the bounds a prefab must stay in.
    OutOfBounds,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::ChunkNotLoaded(_) => write!(f, "the world around the prefab is still loading"),
            CaptureError::TooManyParts(n) => write!(f, "{n} parts (the limit is {PREFAB_MAX_PARTS})"),
            CaptureError::TooManyBoxes(n) => write!(f, "{n} boxes (the limit is {PREFAB_MAX_BOXES})"),
            CaptureError::OutOfBounds => write!(f, "a brick lies outside the prefab bounds"),
        }
    }
}

const CHUNK_CELLS: [i64; 3] = [STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64, STUD_CHUNK_STUDS as i64];

/// `(material, data)` of each cell of `chunk`, by `(y * depth + z) * width + x`.
fn cell_grid(chunk: &StudChunk) -> Vec<Option<(u16, u16)>> {
    let mut grid = vec![None; STUD_CHUNK_STUDS * STUD_CHUNK_PLATES * STUD_CHUNK_STUDS];
    for b in chunk.bricks() {
        for [x, y, z] in b.cells() {
            grid[(y as usize * STUD_CHUNK_STUDS + z as usize) * STUD_CHUNK_STUDS + x as usize] = Some((b.material, b.data));
        }
    }
    grid
}

/// Reads the prefab standing at world cell `anchor` back out of a world.
///
/// `chunk` is the world as it is, `pristine` the untouched ground, both by world chunk.
/// A chunk with pristine ground that `chunk` does not have is
/// [`CaptureError::ChunkNotLoaded`]; neither is air. Every chunk the prefab bounds
/// touch is read, and one chunk around them on x and z and above, so a brick laid just
/// outside the bounds is [`CaptureError::OutOfBounds`] rather than left out silently
/// (bricks farther out are not seen).
///
/// Catalog parts of a plastic colour become parts exactly as they lie (cells relative
/// to the anchor, part by LDraw number, `data` kept). Bricks of natural materials (and
/// plain fillers) give their cells to one cell set per `(material, data)`, merged into
/// boxes by [`merge_boxes`]. The output is canonical: parts by `(y, z, x)`, boxes by
/// material then position — so a prefab stamped at the anchor with no turn and read
/// back gives the file it came from. `kept_carve` and `kept_markers` pass through.
pub fn capture_build(
    chunk: impl Fn(WorldChunkPos) -> Option<StudChunk>,
    pristine: impl Fn(WorldChunkPos) -> Option<StudChunk>,
    anchor: [i64; 3],
    kept_carve: Vec<CellBox>,
    kept_markers: Vec<Marker>,
) -> Result<BuildFile, CaptureError> {
    let half = i64::from(PREFAB_HALF_STUDS);
    let lo = [anchor[0] - half, anchor[1] + i64::from(PREFAB_MIN_PLATE), anchor[2] - half];
    let hi = [anchor[0] + half, anchor[1] + i64::from(PREFAB_MAX_PLATE), anchor[2] + half];
    let inside = |min: [i64; 3], size: [i64; 3]| (0..3).all(|i| min[i] >= lo[i] && min[i] + size[i] <= hi[i]);
    let first = [0, 1, 2].map(|i| lo[i].div_euclid(CHUNK_CELLS[i]));
    let last = [0, 1, 2].map(|i| (hi[i] - 1).div_euclid(CHUNK_CELLS[i]));
    let relative = |cell: [i64; 3]| [0, 1, 2].map(|i| (cell[i] - anchor[i]) as i32);

    let mut parts: Vec<BuildPartFile> = Vec::new();
    let mut fills: BTreeMap<(u16, u16), BTreeSet<[i32; 3]>> = BTreeMap::new();
    for cx in first[0] - 1..=last[0] + 1 {
        // One layer above the bounds too (air unless built in). Not below: ground there
        // may be only implied, with no chunk to read.
        for cy in first[1]..=last[1] + 1 {
            for cz in first[2] - 1..=last[2] + 1 {
                let Ok(layer) = i32::try_from(cy) else { continue };
                let pos = WorldChunkPos::new(cx, layer, cz);
                let ground = pristine(pos);
                let Some(have) = chunk(pos) else {
                    if ground.is_some() {
                        return Err(CaptureError::ChunkNotLoaded(pos));
                    }
                    continue;
                };
                let origin = [cx * CHUNK_CELLS[0], cy * CHUNK_CELLS[1], cz * CHUNK_CELLS[2]];
                // The ground's cells, built only when a brick needs them.
                let mut ground_cells: Option<Vec<Option<(u16, u16)>>> = None;
                for b in have.bricks() {
                    if ground.as_ref().is_some_and(|g| g.bricks.get(&b.min) == Some(b)) {
                        continue;
                    }
                    let min = [0, 1, 2].map(|i| origin[i] + i64::from(b.min[i]));
                    let part = match b.shape {
                        StudShape::Part { part, rot } if b.material < NATURAL_BASE => STUD_PARTS.get(part as usize).map(|d| (d, rot)),
                        _ => None,
                    };
                    if let Some((def, rot)) = part {
                        if !inside(min, b.size().map(i64::from)) {
                            return Err(CaptureError::OutOfBounds);
                        }
                        parts.push(BuildPartFile {
                            min: relative(min),
                            part: def.number.to_string(),
                            rot,
                            material: material_name(b.material),
                            data: b.data,
                        });
                        continue;
                    }
                    let grid = ground_cells.get_or_insert_with(|| ground.as_ref().map(cell_grid).unwrap_or_default());
                    for [x, y, z] in b.cells() {
                        let i = (y as usize * STUD_CHUNK_STUDS + z as usize) * STUD_CHUNK_STUDS + x as usize;
                        if grid.get(i).copied().flatten() == Some((b.material, b.data)) {
                            continue;
                        }
                        let cell = [origin[0] + i64::from(x), origin[1] + i64::from(y), origin[2] + i64::from(z)];
                        if !inside(cell, [1, 1, 1]) {
                            return Err(CaptureError::OutOfBounds);
                        }
                        fills.entry((b.material, b.data)).or_default().insert(relative(cell));
                    }
                }
            }
        }
    }
    if parts.len() > PREFAB_MAX_PARTS {
        return Err(CaptureError::TooManyParts(parts.len()));
    }
    parts.sort_by(|a, b| {
        let key = |p: &BuildPartFile| ([p.min[1], p.min[2], p.min[0]], p.rot, p.data);
        key(a).cmp(&key(b)).then_with(|| a.part.cmp(&b.part))
    });
    let mut boxes = Vec::new();
    for ((material, data), cells) in fills {
        for (min, size) in merge_boxes(cells) {
            boxes.push(BuildBoxFile { min, size: size.map(|s| s as u32), material: material_name(material), data });
        }
    }
    if boxes.len() + kept_carve.len() > PREFAB_MAX_BOXES {
        return Err(CaptureError::TooManyBoxes(boxes.len() + kept_carve.len()));
    }
    Ok(BuildFile { parts, boxes, carve: kept_carve, markers: kept_markers })
}
