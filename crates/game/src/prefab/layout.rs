//! The one Build layout routine, shared by runtime stamping and worldgen so the two
//! cannot drift: a prefab turned, moved to an anchor, and cut at chunk edges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ResolvedBuild;
use crate::stud_catalog::STUD_PARTS;
use crate::{fill_box, StudBrick, StudChunk, StudShape, WorldChunkPos, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS};

/// A stud chunk's size in cells: studs x, plates y, studs z.
const CHUNK_CELLS: [i64; 3] = [STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64, STUD_CHUNK_STUDS as i64];

/// One chunk's share of a stamp, in chunk cells.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct ChunkStamp {
    /// Boxes `(min, size)` cleared before anything is laid.
    pub carve: Vec<([u8; 3], [u8; 3])>,
    /// Bricks to lay, in canonical (ascending) order.
    pub bricks: Vec<StudBrick>,
}

/// A whole stamp, per world chunk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildLayout {
    pub chunks: BTreeMap<WorldChunkPos, ChunkStamp>,
    /// Non-box parts (slopes) left out because they would straddle a chunk edge.
    pub skipped_split: u32,
}

/// Turns a cell by `turn` quarter turns about the anchor column (the anchor cell stays
/// put, `y` is unchanged). The direction is `rotate_cell`'s, so a part turned with its
/// prefab is the same part with `rot + turn`.
pub fn turn_cell(c: [i32; 3], turn: u8) -> [i32; 3] {
    let [x, y, z] = c;
    match turn & 3 {
        0 => [x, y, z],
        1 => [-z, y, x],
        2 => [-x, y, -z],
        _ => [z, y, -x],
    }
}

/// The world cell box `(min, size)` of the prefab box `min .. min + size` after turning
/// and moving to `anchor`. `size` must be non-zero (validated prefabs).
fn world_box(min: [i32; 3], size: [u32; 3], anchor: [i64; 3], turn: u8) -> ([i64; 3], [i64; 3]) {
    let max = [0, 1, 2].map(|i| (i64::from(min[i]) + i64::from(size[i]) - 1) as i32);
    let (a, b) = (turn_cell(min, turn), turn_cell(max, turn));
    let lo = [0, 1, 2].map(|i| anchor[i] + i64::from(a[i].min(b[i])));
    let turned = if turn & 1 == 1 { [size[2], size[1], size[0]] } else { size };
    (lo, turned.map(i64::from))
}

/// Cuts a world cell box at chunk edges: each chunk it meets with its slice `(min, size)`
/// in chunk cells, in ascending chunk order.
fn chunk_slices(min: [i64; 3], size: [i64; 3]) -> Vec<(WorldChunkPos, [u8; 3], [u8; 3])> {
    let first = [0, 1, 2].map(|i| min[i].div_euclid(CHUNK_CELLS[i]));
    let last = [0, 1, 2].map(|i| (min[i] + size[i] - 1).div_euclid(CHUNK_CELLS[i]));
    let mut out = Vec::new();
    for cx in first[0]..=last[0] {
        for cy in first[1]..=last[1] {
            for cz in first[2]..=last[2] {
                let c = [cx, cy, cz];
                let lo = [0, 1, 2].map(|i| min[i].max(c[i] * CHUNK_CELLS[i]));
                let hi = [0, 1, 2].map(|i| (min[i] + size[i]).min((c[i] + 1) * CHUNK_CELLS[i]));
                out.push((
                    WorldChunkPos::new(cx, cy as i32, cz),
                    [0, 1, 2].map(|i| (lo[i] - c[i] * CHUNK_CELLS[i]) as u8),
                    [0, 1, 2].map(|i| (hi[i] - lo[i]) as u8),
                ));
            }
        }
    }
    out
}

/// Lays `p` out with its anchor on world cell `anchor` (studs x, plates y, studs z),
/// turned `turn` quarter turns. Pure: the same inputs give the same layout everywhere.
/// `p` must have passed validation.
pub fn layout_build(p: &ResolvedBuild, anchor: [i64; 3], turn: u8) -> BuildLayout {
    let turn = turn & 3;
    let mut layout = BuildLayout::default();
    for part in &p.parts {
        let Some(def) = STUD_PARTS.get(part.part as usize) else { continue };
        let size = def.size.map(u32::from);
        let unturned = if part.rot & 1 == 1 { [size[2], size[1], size[0]] } else { size };
        let (min, size) = world_box(part.min, unturned, anchor, turn);
        let slices = chunk_slices(min, size);
        if let [(chunk, local, _)] = slices[..] {
            let shape = StudShape::Part { part: part.part, rot: (part.rot + turn) & 3 };
            let brick = StudBrick { min: local, shape, material: part.material, data: part.data };
            layout.chunks.entry(chunk).or_default().bricks.push(brick);
        } else if def.hull.is_empty() {
            for (chunk, local, size) in slices {
                fill_box(local, size, part.material, part.data, &mut layout.chunks.entry(chunk).or_default().bricks);
            }
        } else {
            layout.skipped_split += 1;
        }
    }
    for b in &p.boxes {
        let (min, size) = world_box(b.min, b.size, anchor, turn);
        for (chunk, local, size) in chunk_slices(min, size) {
            fill_box(local, size, b.material, b.data, &mut layout.chunks.entry(chunk).or_default().bricks);
        }
    }
    for c in &p.carve {
        let (min, size) = world_box(c.min, c.size, anchor, turn);
        for (chunk, local, size) in chunk_slices(min, size) {
            layout.chunks.entry(chunk).or_default().carve.push((local, size));
        }
    }
    for stamp in layout.chunks.values_mut() {
        stamp.bricks.sort();
        stamp.carve.sort();
    }
    layout
}

impl ChunkStamp {
    /// Carves, then lays the bricks. A brick that overlaps something already there (or
    /// is not a valid brick of this chunk) is skipped; returns how many were.
    pub fn apply(&self, chunk: &mut StudChunk) -> u32 {
        for &(min, size) in &self.carve {
            let hi = [0, 1, 2].map(|i| min[i].saturating_add(size[i]));
            chunk.carve(min, hi);
        }
        let mut skipped = 0;
        for &b in &self.bricks {
            if chunk.place(b).is_err() {
                skipped += 1;
            }
        }
        skipped
    }

    pub fn is_empty(&self) -> bool {
        self.carve.is_empty() && self.bricks.is_empty()
    }
}
