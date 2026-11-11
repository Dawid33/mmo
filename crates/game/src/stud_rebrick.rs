//! Re-bricking on exposure: ground below the surface layers is plain `Filler` slabs
//! (nothing of it shows), so a stud write that opens a hole would show smooth walls.
//! Whenever a write leaves a filler cell with an open face neighbour, the *slot* that
//! cell lies in becomes real catalog bricks; the unexposed interior stays filler.
//!
//! A slot is one brick of the surface's running bond (`stud_ground::lay_layer` over a
//! full layer of one material): [`REBRICK_SLOT_WIDTH`] studs wide, up to
//! [`REBRICK_SLOT_LENGTH`] long, one brick course tall; strips run along x on even
//! world courses and along z on odd ones, odd strips are shifted by
//! [`REBRICK_STAGGER`], and slots are cut at chunk edges. The lattice is a function of
//! the cell's position alone and a slot converts whole, so the shell behind an exposed
//! face is always at least one whole brick deep, and the bricks of a slot are a
//! function of (position, the slot's filler cells and their material/`data`) — never
//! of how the fillers around it had been cut up.
//!
//! Occupancy, materials and `data` are unchanged by a re-brick; only stud caps appear
//! (on up-facing free cells, as on the surface). Undiggable fillers (bedrock) stay.

use std::collections::{BTreeMap, BTreeSet};

use crate::stud::{StudBrick, StudChunk, StudChunkCoords, StudOccupancy, StudShape, PLATES_PER_BRICK, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS};
use crate::stud_carve::{box_minus, fill_box};
use crate::stud_material::stud_material;
use crate::Rollback;

/// Studs across a slot (the strip width of the surface bond).
pub const REBRICK_SLOT_WIDTH: u8 = 2;
/// Studs along a slot (the longest brick of the surface bond).
pub const REBRICK_SLOT_LENGTH: u8 = 8;
/// How far odd strips are shifted along their run (the surface bond's half brick).
pub const REBRICK_STAGGER: u8 = 4;
const _: () = assert!(STUD_CHUNK_STUDS % REBRICK_SLOT_LENGTH as usize == 0 && STUD_CHUNK_STUDS % REBRICK_SLOT_WIDTH as usize == 0);
const _: () = assert!(STUD_CHUNK_PLATES % PLATES_PER_BRICK as usize == 0);

/// The slot holding `cell` of a chunk of layer `layer`, as a chunk cell box
/// (`lo .. hi`, `hi` exclusive).
pub fn rebrick_slot(layer: i32, cell: [u8; 3]) -> ([u8; 3], [u8; 3]) {
    let n = STUD_CHUNK_STUDS as u8;
    let courses = (STUD_CHUNK_PLATES / PLATES_PER_BRICK as usize) as i64;
    let course = cell[1] / PLATES_PER_BRICK;
    // World course parity, as the surface bond alternates.
    let along_x = (layer as i64 * courses + course as i64).rem_euclid(2) == 0;
    let (l, row) = if along_x { (cell[0], cell[2]) } else { (cell[2], cell[0]) };
    let strip = row / REBRICK_SLOT_WIDTH;
    let (l0, l1) = if strip % 2 == 1 {
        if l < REBRICK_STAGGER {
            (0, REBRICK_STAGGER)
        } else {
            let lo = REBRICK_STAGGER + (l - REBRICK_STAGGER) / REBRICK_SLOT_LENGTH * REBRICK_SLOT_LENGTH;
            (lo, (lo + REBRICK_SLOT_LENGTH).min(n))
        }
    } else {
        let lo = l / REBRICK_SLOT_LENGTH * REBRICK_SLOT_LENGTH;
        (lo, lo + REBRICK_SLOT_LENGTH)
    };
    let (r0, r1) = (strip * REBRICK_SLOT_WIDTH, (strip + 1) * REBRICK_SLOT_WIDTH);
    let (y0, y1) = (course * PLATES_PER_BRICK, (course + 1) * PLATES_PER_BRICK);
    if along_x { ([l0, y0, r0], [l1, y1, r1]) } else { ([r0, y0, l0], [r1, y1, l1]) }
}

fn diggable(material: u16) -> bool {
    stud_material(material).is_some_and(|m| m.hardness != u16::MAX)
}

impl StudChunk {
    /// Turns the diggable filler cells inside the box `lo .. hi` (one slot) into
    /// catalog bricks; fillers reaching outside keep their other cells as smaller
    /// fillers. The bricks are a function of the box and of which of its cells were
    /// filler (with their material and `data`): cells are swept plate, z, x; each
    /// uncovered one grows a box along x, then z, then to a whole course when it sits
    /// on the box's bottom, and that box is tiled by [`fill_box`]. `false`, nothing
    /// changed, when the box holds no diggable filler cell.
    pub fn rebrick_box(&mut self, lo: [u8; 3], hi: [u8; 3]) -> bool {
        if (0..3).any(|i| lo[i] >= hi[i]) {
            return false;
        }
        let hits: Vec<StudBrick> = self
            .bricks
            .values()
            .filter(|b| {
                let s = b.size();
                matches!(b.shape, StudShape::Filler { .. }) && (0..3).all(|i| b.min[i] < hi[i] && lo[i] < b.min[i] + s[i]) && diggable(b.material)
            })
            .copied()
            .collect();
        if hits.is_empty() {
            return false;
        }
        let dim = [0, 1, 2].map(|a| (hi[a] - lo[a]) as usize);
        let at = |c: [usize; 3]| (c[1] * dim[2] + c[2]) * dim[0] + c[0];
        // (material, data) of each cell of the box that was filler.
        let mut grid: Vec<Option<(u16, u16)>> = vec![None; dim[0] * dim[1] * dim[2]];
        for b in hits {
            self.bricks.remove(&b.min);
            let s = b.size();
            let b_hi = [0, 1, 2].map(|a| b.min[a] + s[a]);
            let c_lo = [0, 1, 2].map(|a| lo[a].max(b.min[a]));
            let c_hi = [0, 1, 2].map(|a| hi[a].min(b_hi[a]));
            for (min, size) in box_minus(b.min, b_hi, c_lo, c_hi) {
                self.bricks.insert(min, StudBrick { min, shape: StudShape::Filler { size }, material: b.material, data: b.data });
            }
            for y in c_lo[1]..c_hi[1] {
                for z in c_lo[2]..c_hi[2] {
                    for x in c_lo[0]..c_hi[0] {
                        grid[at([(x - lo[0]) as usize, (y - lo[1]) as usize, (z - lo[2]) as usize])] = Some((b.material, b.data));
                    }
                }
            }
        }
        let mut out = Vec::new();
        for y in 0..dim[1] {
            for z in 0..dim[2] {
                for x in 0..dim[0] {
                    let Some(key) = grid[at([x, y, z])] else { continue };
                    let is = |g: &[Option<(u16, u16)>], c: [usize; 3]| g[at(c)] == Some(key);
                    let mut x1 = x + 1;
                    while x1 < dim[0] && is(&grid, [x1, y, z]) {
                        x1 += 1;
                    }
                    let mut z1 = z + 1;
                    while z1 < dim[2] && (x..x1).all(|i| is(&grid, [i, y, z1])) {
                        z1 += 1;
                    }
                    let course = PLATES_PER_BRICK as usize;
                    let whole = y == 0
                        && dim[1] >= course
                        && (1..course).all(|dy| (z..z1).all(|k| (x..x1).all(|i| is(&grid, [i, dy, k]))));
                    let y1 = if whole { course } else { y + 1 };
                    for j in y..y1 {
                        for k in z..z1 {
                            for i in x..x1 {
                                grid[at([i, j, k])] = None;
                            }
                        }
                    }
                    let min = [lo[0] + x as u8, lo[1] + y as u8, lo[2] + z as u8];
                    fill_box(min, [(x1 - x) as u8, (y1 - y) as u8, (z1 - z) as u8], key.0, key.1, &mut out);
                }
            }
        }
        for b in out {
            self.bricks.insert(b.min, b);
        }
        true
    }
}

/// Working chunk copies of one batched stud write: the chunk as the sim sees it
/// (`None`: nothing there, not even implied ground) and whether the write changed it.
pub(crate) type StudWork = BTreeMap<StudChunkCoords, Option<(StudChunk, bool)>>;

impl Rollback {
    /// Whether region stud cell `q` is known to be open, with `work` over this
    /// region's chunks: its own cells from the working copy, else the chunk or recipe;
    /// past the edge the halo's chunk, else the neighbour's recipe. Unknown (no halo
    /// there, an illegal layer) is not open: nothing is re-bricked on a guess.
    fn stud_cell_open(
        &self,
        work: &StudWork,
        own_occ: &mut BTreeMap<StudChunkCoords, StudOccupancy>,
        halo_occ: &mut BTreeMap<[i32; 3], StudOccupancy>,
        q: [i64; 3],
    ) -> bool {
        let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
        let n = crate::REGION_CHUNKS as i64 * cw;
        let Ok(plate) = i32::try_from(q[1]) else { return false };
        let layer = q[1].div_euclid(ch) as i32;
        if !crate::stud::stud_layer_ok(layer) {
            return false;
        }
        let local = [q[0].rem_euclid(cw) as u8, q[1].rem_euclid(ch) as u8, q[2].rem_euclid(cw) as u8];
        if (0..n).contains(&q[0]) && (0..n).contains(&q[2]) {
            let c = StudChunkCoords { x: (q[0] / cw) as usize, y: layer, z: (q[2] / cw) as usize };
            return match work.get(&c) {
                Some(Some((chunk, _))) => !own_occ.entry(c).or_insert_with(|| chunk.occupancy()).get(local.map(i32::from)),
                Some(None) => true,
                None => self.stud_material_at([q[0] as i32, plate, q[2] as i32]).is_none(),
            };
        }
        let chunk = [q[0].div_euclid(cw) as i32, layer, q[2].div_euclid(cw) as i32];
        if !crate::halo::in_halo_band(chunk[0], chunk[2]) {
            return false;
        }
        let key = crate::WorldChunkPos::from_local(*self.data.region, chunk);
        if let Some(c) = self.data.stud_halo.get(&key).and_then(|h| h.chunk.as_ref()) {
            return !halo_occ.entry(chunk).or_insert_with(|| c.occupancy()).get(local.map(i32::from));
        }
        match self.data.stud_halo_ground.get(&crate::WorldChunkPos::new(key.x, 0, key.z)) {
            Some(g) => g.material_at(local[0] as usize, plate, local[2] as usize).is_none(),
            None => false,
        }
    }

    /// Plans the re-brick after a write that may have opened the region cell boxes
    /// `boxes` (`lo .. hi`, `hi` exclusive; they may lie past the region's edge — a
    /// neighbour's edited halo chunk): every own diggable filler cell that touches an
    /// open cell of a box across a face has its slot ([`rebrick_slot`]) turned into
    /// bricks in `work` (loading the chunk — materialised or implied — when it is not
    /// there yet). Writes no state.
    pub(crate) fn plan_stud_rebrick(&self, work: &mut StudWork, boxes: &[([i64; 3], [i64; 3])]) {
        let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
        let n = crate::REGION_CHUNKS as i64 * cw;
        // Occupancy of the chunks probed (nothing in `work` changes until the scan is over).
        let mut own_occ: BTreeMap<StudChunkCoords, StudOccupancy> = BTreeMap::new();
        let mut halo_occ: BTreeMap<[i32; 3], StudOccupancy> = BTreeMap::new();
        // Diggable filler cells of each own chunk looked at (`None`: no chunk).
        let mut fillers: BTreeMap<StudChunkCoords, Option<StudOccupancy>> = BTreeMap::new();
        let mut slots: BTreeMap<StudChunkCoords, BTreeSet<([u8; 3], [u8; 3])>> = BTreeMap::new();
        for &(lo, hi) in boxes {
            if (0..3).any(|i| lo[i] >= hi[i]) {
                continue;
            }
            for axis in 0..3 {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                // The plane just outside each of the two faces, and the step back in.
                for (plane, inward) in [(lo[axis] - 1, 1), (hi[axis], -1)] {
                    for a in lo[u]..hi[u] {
                        for b in lo[v]..hi[v] {
                            let mut p = [0; 3];
                            p[axis] = plane;
                            p[u] = a;
                            p[v] = b;
                            if !(0..n).contains(&p[0]) || !(0..n).contains(&p[2]) {
                                continue;
                            }
                            let Ok(layer) = i32::try_from(p[1].div_euclid(ch)) else { continue };
                            if !crate::stud::stud_layer_ok(layer) {
                                continue;
                            }
                            let mut q = p;
                            q[axis] += inward;
                            if !self.stud_cell_open(work, &mut own_occ, &mut halo_occ, q) {
                                continue;
                            }
                            let c = StudChunkCoords { x: (p[0] / cw) as usize, y: layer, z: (p[2] / cw) as usize };
                            let local = [(p[0] % cw) as u8, p[1].rem_euclid(ch) as u8, (p[2] % cw) as u8];
                            let occ = fillers.entry(c).or_insert_with(|| {
                                let slot = work.entry(c).or_insert_with(|| self.stud_chunk_or_pristine(c).map(|chunk| (chunk.into_owned(), false)));
                                slot.as_ref().map(|(chunk, _)| {
                                    let mut occ = StudOccupancy::default();
                                    for b in chunk.bricks().filter(|b| matches!(b.shape, StudShape::Filler { .. }) && diggable(b.material)) {
                                        for cell in b.cells() {
                                            occ.set(cell.map(i32::from), true);
                                        }
                                    }
                                    occ
                                })
                            });
                            if occ.as_ref().is_some_and(|o| o.get(local.map(i32::from))) {
                                slots.entry(c).or_default().insert(rebrick_slot(layer, local));
                            }
                        }
                    }
                }
            }
        }
        for (c, slots) in slots {
            let Some(Some((chunk, changed))) = work.get_mut(&c) else { continue };
            for (lo, hi) in slots {
                *changed |= chunk.rebrick_box(lo, hi);
            }
        }
    }

    /// Writes the changed chunks of `work`, undo-safe, one write per chunk.
    pub(crate) fn write_stud_work_safe(&mut self, work: StudWork) {
        for (c, slot) in work {
            let Some((chunk, true)) = slot else { continue };
            match self.stud_key_at(c) {
                Some(key) => self.set_stud_chunk_safe(key, chunk),
                None => {
                    self.create_stud_chunk_safe(c, chunk);
                }
            }
        }
    }

    /// Re-bricks this region's ground exposed by the open cells of the region cell
    /// boxes `boxes` ([`Self::plan_stud_rebrick`]), undo-safe. For writes that do not
    /// batch their own chunk writes, and for a neighbour's edited halo chunks.
    pub fn rebrick_stud_exposed_safe(&mut self, boxes: &[([i64; 3], [i64; 3])]) {
        let mut work = StudWork::new();
        self.plan_stud_rebrick(&mut work, boxes);
        self.write_stud_work_safe(work);
    }
}
