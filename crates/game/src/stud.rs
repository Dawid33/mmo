//! Stud-world chunk data: LDraw-unit bricks on a 32 x 90 x 32 grid of stud cells.
//!
//! Units: 1 stud = 8 sim units, 1 plate = 3.2 sim units (5 plates = 16 units), a chunk is
//! 32 studs wide and 90 plates (288 units = 18 m = 15 tiles) tall. A [`StudChunk`] is a canonical `BTreeMap`
//! keyed by each brick's min cell, so equal contents are equal bytes and equal hashes.

use std::collections::BTreeMap;

use rapier3d::math::Vector;
use serde::{Deserialize, Serialize};

use crate::block::CHUNK_UNITS;
use crate::stud_assets::StudPartDef;
use crate::stud_catalog::STUD_PARTS;
use crate::stud_material::stud_material;

pub const STUD_UNITS: usize = 8;
pub const STUD_CHUNK_STUDS: usize = 32;
pub const STUD_CHUNK_PLATES: usize = 90;
pub const STUD_CHUNK_HEIGHT_UNITS: usize = 288;
pub const PLATES_PER_BRICK: u8 = 3;
/// Sim units per LDraw unit.
pub const LDU_UNITS: f32 = 0.4;

/// The sim's nav / designation tile on the stud grid: 2 x 2 studs (1 m) wide,
/// 6 plates (2 bricks, 1.2 m) tall. A person is two tiles tall.
pub const TILE_STUDS: usize = 2;
pub const TILE_PLATES: usize = 6;

// Whole sim units (every 5 plates = 16 units) and whole coarse collider cells.
const _: () = assert!(STUD_CHUNK_PLATES % 15 == 0);
// Whole tiles.
const _: () = assert!(STUD_CHUNK_PLATES % TILE_PLATES == 0 && STUD_CHUNK_STUDS % TILE_STUDS == 0);
// Plate coordinates fit `u8`, one past the top included.
const _: () = assert!(STUD_CHUNK_PLATES < 256);
/// Whether this build's sim chunk is the stud chunk's footprint (32 studs x 8 units =
/// 256 units, i.e. `chunk_blocks=16`). Other chunk sizes still compile, but the stud
/// experiment is off: the server refuses `VOXEL_WORLD_FILE=studs` and `client debug
/// studs` errors out.
pub const STUDS_SUPPORTED: bool = STUD_UNITS * STUD_CHUNK_STUDS == CHUNK_UNITS;
const _: () = assert!(STUD_CHUNK_PLATES * 16 / 5 == STUD_CHUNK_HEIGHT_UNITS);

/// Height in sim units of `plates` plates (5 plates = 16 units).
pub fn plate_units(plates: i32) -> f32 {
    (plates * 16) as f32 / 5.0
}

/// Stud-chunk layers (`StudChunkCoords::y`) run `-STUD_LAYER_LIMIT..=STUD_LAYER_LIMIT`
/// (about +-73 km): far beyond any forged world, and small enough that
/// `StudChunkCoords::origin` stays an exact `f32` (< 2^24 units).
pub const STUD_LAYER_LIMIT: i32 = 1 << 12;
const _: () = assert!((STUD_LAYER_LIMIT as i64 + 1) * (STUD_CHUNK_HEIGHT_UNITS as i64) < 1 << 24);

/// Whether `y` is a legal stud-chunk layer.
pub fn stud_layer_ok(y: i32) -> bool {
    (-STUD_LAYER_LIMIT..=STUD_LAYER_LIMIT).contains(&y)
}

/// Chunk position in the stud grid: x/z non-negative within the region, y signed.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, state_hash::StateHash,
)]
pub struct StudChunkCoords {
    pub x: usize,
    pub y: i32,
    pub z: usize,
}

impl StudChunkCoords {
    /// Min corner in sim units: `(x*256, y*288, z*256)`.
    pub fn origin(&self) -> Vector {
        Vector::new(
            (self.x * CHUNK_UNITS) as f32,
            (self.y * STUD_CHUNK_HEIGHT_UNITS as i32) as f32,
            (self.z * CHUNK_UNITS) as f32,
        )
    }

    /// Inverse of [`Self::origin`] (exact division, rounded).
    pub fn from_origin(t: Vector) -> Self {
        Self {
            x: (t.x / CHUNK_UNITS as f32).round() as usize,
            y: (t.y / STUD_CHUNK_HEIGHT_UNITS as f32).round() as i32,
            z: (t.z / CHUNK_UNITS as f32).round() as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, state_hash::StateHash)]
pub enum StudShape {
    /// A catalog part (`STUD_PARTS[part]`) turned `rot` quarter turns about +Y.
    Part { part: u16, rot: u8 },
    /// A plain box of the given cell size (no studs), used for terrain fill.
    Filler { size: [u8; 3] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, state_hash::StateHash)]
pub struct StudBrick {
    /// Min cell `[stud x, plate y, stud z]`.
    pub min: [u8; 3],
    pub shape: StudShape,
    /// Material id (`stud_material.rs`): a plastic of an LDraw colour, or a natural material.
    pub material: u16,
    /// Free per-brick tag: 0 for plain terrain and builds, a tree id for wood and leaves.
    pub data: u16,
}

/// Rotate a cell of an unrotated part of `size` (UNROTATED) by `rot` quarter turns.
pub fn rotate_cell(rot: u8, c: [u8; 3], size: [u8; 3]) -> [u8; 3] {
    let [x, y, z] = c;
    let [sx, _, sz] = size;
    match rot & 3 {
        0 => [x, y, z],
        1 => [sz - 1 - z, y, x],
        2 => [sx - 1 - x, y, sz - 1 - z],
        _ => [z, y, sx - 1 - x],
    }
}

/// Rotate a continuous point (LDU, relative to the unrotated bbox min) the same way.
/// `size_ldu` is the UNROTATED footprint `[sx*20, _, sz*20]`.
pub fn rotate_point(rot: u8, p: [i32; 3], size_ldu: [i32; 3]) -> [i32; 3] {
    let [x, y, z] = p;
    let [sx, _, sz] = size_ldu;
    match rot & 3 {
        0 => [x, y, z],
        1 => [sz - z, y, x],
        2 => [sx - x, y, sz - z],
        _ => [z, y, sx - x],
    }
}

impl StudBrick {
    /// A catalog part with `data` 0.
    pub fn part_at(min: [u8; 3], part: u16, rot: u8, material: u16) -> Self {
        Self { min, shape: StudShape::Part { part, rot }, material, data: 0 }
    }

    /// A filler box with `data` 0.
    pub fn filler(min: [u8; 3], size: [u8; 3], material: u16) -> Self {
        Self { min, shape: StudShape::Filler { size }, material, data: 0 }
    }

    fn unrotated_size(&self) -> [u8; 3] {
        match self.shape {
            StudShape::Part { part, .. } => STUD_PARTS.get(part as usize).map_or([0; 3], |p| p.size),
            StudShape::Filler { size } => size,
        }
    }

    fn rot(&self) -> u8 {
        match self.shape {
            StudShape::Part { rot, .. } => rot,
            StudShape::Filler { .. } => 0,
        }
    }

    /// Footprint after rotation (x/z swap for odd turns).
    pub fn size(&self) -> [u8; 3] {
        let [sx, sy, sz] = self.unrotated_size();
        if self.rot() & 1 == 1 { [sz, sy, sx] } else { [sx, sy, sz] }
    }

    /// Every chunk cell the brick occupies.
    pub fn cells(&self) -> impl Iterator<Item = [u8; 3]> {
        let [mx, my, mz] = self.min;
        let [sx, sy, sz] = self.size();
        (0..sx).flat_map(move |x| {
            (0..sy).flat_map(move |y| (0..sz).map(move |z| [mx + x, my + y, mz + z]))
        })
    }

    /// Chunk cells carrying a top stud (the cell above the top, so `y` may be `STUD_CHUNK_PLATES`).
    pub fn stud_cells(&self) -> Vec<[u8; 3]> {
        let StudShape::Part { rot, .. } = self.shape else { return Vec::new() };
        let Some(def) = self.part() else { return Vec::new() };
        let size = def.size;
        def.studs
            .iter()
            .map(|&c| {
                let r = rotate_cell(rot, c, size);
                [self.min[0] + r[0], self.min[1] + r[1], self.min[2] + r[2]]
            })
            .collect()
    }

    pub fn part(&self) -> Option<&'static StudPartDef> {
        match self.shape {
            StudShape::Part { part, .. } => STUD_PARTS.get(part as usize),
            StudShape::Filler { .. } => None,
        }
    }

    /// Collides as its cell box: a filler or a part with no hull.
    pub fn is_box(&self) -> bool {
        self.part().is_none_or(|p| p.hull.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudError {
    OutOfChunk,
    Overlap,
    UnknownPart,
    UnknownMaterial,
    BadRotation,
    EmptyFiller,
}

/// Bitset over the chunk's 32 x 90 x 32 cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudOccupancy {
    bits: Vec<u64>,
}

const OCC_CELLS: usize = STUD_CHUNK_STUDS * STUD_CHUNK_PLATES * STUD_CHUNK_STUDS;

impl Default for StudOccupancy {
    fn default() -> Self {
        Self { bits: vec![0; OCC_CELLS.div_ceil(64)] }
    }
}

impl StudOccupancy {
    fn index(c: [i32; 3]) -> Option<usize> {
        let [x, y, z] = c;
        let in_range = (0..STUD_CHUNK_STUDS as i32).contains(&x)
            && (0..STUD_CHUNK_PLATES as i32).contains(&y)
            && (0..STUD_CHUNK_STUDS as i32).contains(&z);
        in_range.then(|| (y as usize * STUD_CHUNK_STUDS + z as usize) * STUD_CHUNK_STUDS + x as usize)
    }

    /// Out of range reads as empty.
    pub fn get(&self, c: [i32; 3]) -> bool {
        Self::index(c).is_some_and(|i| self.bits[i / 64] >> (i % 64) & 1 == 1)
    }

    /// Out of range writes are ignored.
    pub fn set(&mut self, c: [i32; 3], v: bool) {
        if let Some(i) = Self::index(c) {
            if v {
                self.bits[i / 64] |= 1 << (i % 64);
            } else {
                self.bits[i / 64] &= !(1 << (i % 64));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|&w| w == 0)
    }
}

/// The bricks of one chunk, keyed by min cell. Non-overlapping by construction.
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct StudChunk {
    pub(crate) bricks: BTreeMap<[u8; 3], StudBrick>,
}

fn validate(b: &StudBrick) -> Result<(), StudError> {
    match b.shape {
        StudShape::Part { part, rot } => {
            if rot >= 4 {
                return Err(StudError::BadRotation);
            }
            if part as usize >= STUD_PARTS.len() {
                return Err(StudError::UnknownPart);
            }
        }
        StudShape::Filler { size } => {
            if size.contains(&0) {
                return Err(StudError::EmptyFiller);
            }
        }
    }
    if stud_material(b.material).is_none() {
        return Err(StudError::UnknownMaterial);
    }
    let [sx, sy, sz] = b.size();
    let fits = b.min[0] as usize + sx as usize <= STUD_CHUNK_STUDS
        && b.min[1] as usize + sy as usize <= STUD_CHUNK_PLATES
        && b.min[2] as usize + sz as usize <= STUD_CHUNK_STUDS;
    if fits { Ok(()) } else { Err(StudError::OutOfChunk) }
}

impl StudChunk {
    /// Builds a chunk from bricks in any order; the result is canonical.
    pub fn new(bricks: impl IntoIterator<Item = StudBrick>) -> Result<Self, StudError> {
        let mut chunk = Self::default();
        let mut occ = StudOccupancy::default();
        for b in bricks {
            validate(&b)?;
            if b.cells().any(|c| occ.get(c.map(i32::from))) {
                return Err(StudError::Overlap);
            }
            for c in b.cells() {
                occ.set(c.map(i32::from), true);
            }
            chunk.bricks.insert(b.min, b);
        }
        Ok(chunk)
    }

    /// Builds a chunk from bricks the caller guarantees valid, in-chunk and disjoint
    /// (generators; checked against [`Self::new`] in debug builds).
    pub(crate) fn from_disjoint(bricks: Vec<StudBrick>) -> Self {
        debug_assert!(Self::new(bricks.iter().copied()).is_ok(), "generated bricks must be valid and disjoint");
        Self { bricks: bricks.into_iter().map(|b| (b.min, b)).collect() }
    }

    /// Bricks in min-cell order.
    pub fn bricks(&self) -> impl Iterator<Item = &StudBrick> {
        self.bricks.values()
    }

    pub fn len(&self) -> usize {
        self.bricks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    /// The brick occupying `cell`, if any.
    pub fn brick_at(&self, cell: [u8; 3]) -> Option<&StudBrick> {
        // Linear scan over every brick (chunks hold few bricks; no spatial index yet).
        self.bricks.values().find(|b| {
            let [sx, sy, sz] = b.size();
            (0..3).all(|i| cell[i] >= b.min[i] && (cell[i] as usize) < b.min[i] as usize + [sx, sy, sz][i] as usize)
        })
    }

    pub fn occupancy(&self) -> StudOccupancy {
        let mut occ = StudOccupancy::default();
        for b in self.bricks.values() {
            for c in b.cells() {
                occ.set(c.map(i32::from), true);
            }
        }
        occ
    }

    pub fn place(&mut self, b: StudBrick) -> Result<(), StudError> {
        validate(&b)?;
        let overlaps = self.bricks.values().any(|o| {
            let (bs, os) = (b.size(), o.size());
            (0..3).all(|i| {
                (b.min[i] as usize) < o.min[i] as usize + os[i] as usize
                    && (o.min[i] as usize) < b.min[i] as usize + bs[i] as usize
            })
        });
        if overlaps {
            return Err(StudError::Overlap);
        }
        self.bricks.insert(b.min, b);
        Ok(())
    }

    /// [`Self::place`] with the room check done against `occ`, this chunk's occupancy
    /// kept by the caller (updated here): the cost is the brick's cells, not the
    /// chunk's bricks. For laying many bricks into one chunk.
    pub fn place_into(&mut self, b: StudBrick, occ: &mut StudOccupancy) -> Result<(), StudError> {
        validate(&b)?;
        if b.cells().any(|c| occ.get(c.map(i32::from))) {
            return Err(StudError::Overlap);
        }
        for c in b.cells() {
            occ.set(c.map(i32::from), true);
        }
        self.bricks.insert(b.min, b);
        Ok(())
    }

    /// Removes the whole brick covering `cell`.
    pub fn remove_at(&mut self, cell: [u8; 3]) -> Option<StudBrick> {
        let min = self.brick_at(cell)?.min;
        self.bricks.remove(&min)
    }

    /// One past the highest occupied plate in stud column `(sx, sz)`.
    pub fn top_plate(&self, sx: u8, sz: u8) -> Option<u8> {
        self.bricks
            .values()
            .filter(|b| {
                let [w, _, d] = b.size();
                (b.min[0]..b.min[0] + w).contains(&sx) && (b.min[2]..b.min[2] + d).contains(&sz)
            })
            .map(|b| b.min[1] + b.size()[1])
            .max()
    }
}

/// What a build-mode ray met: the occupied cell, the face it entered through
/// (the unit step back toward the ray origin; all zero if the ray started inside
/// the cell) and the distance along the ray.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StudHit {
    pub chunk: StudChunkCoords,
    pub cell: [u8; 3],
    pub normal: [i8; 3],
    pub t: f32,
}

/// Cell edge lengths in sim units: a stud wide, a plate tall.
fn stud_cell_size() -> [f64; 3] {
    [STUD_UNITS as f64, 16.0 / 5.0, STUD_UNITS as f64]
}

/// What [`raycast_studs_within`] met: like [`StudHit`], with the chunk in signed
/// chunk coordinates `[x, layer, z]` of the caller's frame (it may lie past the region's
/// edge: a halo chunk).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StudRayHit {
    pub chunk: [i64; 3],
    pub cell: [u8; 3],
    pub normal: [i8; 3],
    pub t: f32,
}

/// Amanatides-Woo walk over the stud cell grid, region-local units, crossing chunks.
/// `chunks` looks a chunk up by coordinates; each chunk's occupancy is built at most
/// once per call. Stops at `reach` (sim units) and once the ray leaves the region's
/// x/z extent (or the legal layers) after having been inside it.
pub fn raycast_studs<'a>(
    chunks: impl Fn(StudChunkCoords) -> Option<&'a StudChunk>,
    origin: Vector,
    dir: Vector,
    reach: f32,
) -> Option<StudHit> {
    let n = crate::REGION_CHUNKS as i64;
    let hit = raycast_studs_within(
        |[x, y, z]| chunks(StudChunkCoords { x: x as usize, y: y as i32, z: z as usize }),
        origin,
        dir,
        reach,
        0..n,
    )?;
    let chunk = StudChunkCoords { x: hit.chunk[0] as usize, y: hit.chunk[1] as i32, z: hit.chunk[2] as usize };
    Some(StudHit { chunk, cell: hit.cell, normal: hit.normal, t: hit.t })
}

/// [`raycast_studs`] over the chunk columns `span` on x and on z (the region's frame;
/// wider than `0..REGION_CHUNKS` to walk into the halo band, `stud_halo.rs`). `chunks`
/// takes signed chunk coordinates `[x, layer, z]` inside that span and the legal layers.
pub fn raycast_studs_within<'a>(
    chunks: impl Fn([i64; 3]) -> Option<&'a StudChunk>,
    origin: Vector,
    dir: Vector,
    reach: f32,
    span: std::ops::Range<i64>,
) -> Option<StudRayHit> {
    let len = (dir.x as f64).hypot(dir.y as f64).hypot(dir.z as f64);
    if len == 0.0 || !len.is_finite() {
        return None;
    }
    let d = [dir.x as f64 / len, dir.y as f64 / len, dir.z as f64 / len];
    let o = [origin.x as f64, origin.y as f64, origin.z as f64];
    let s = stud_cell_size();
    let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
    let lo = [span.start * cw, -(STUD_LAYER_LIMIT as i64) * ch, span.start * cw];
    let hi = [span.end * cw, (STUD_LAYER_LIMIT as i64 + 1) * ch, span.end * cw];

    let mut cell = [0i64; 3];
    let mut step = [0i64; 3];
    let mut t_max = [f64::INFINITY; 3];
    let mut t_delta = [f64::INFINITY; 3];
    for i in 0..3 {
        cell[i] = (o[i] / s[i]).floor() as i64;
        if d[i] > 0.0 {
            step[i] = 1;
            t_delta[i] = s[i] / d[i];
            t_max[i] = ((cell[i] + 1) as f64 * s[i] - o[i]) / d[i];
        } else if d[i] < 0.0 {
            step[i] = -1;
            t_delta[i] = -s[i] / d[i];
            t_max[i] = (cell[i] as f64 * s[i] - o[i]) / d[i];
        }
    }

    let mut cache: BTreeMap<[i64; 3], Option<StudOccupancy>> = BTreeMap::new();
    let (mut t, mut normal, mut entered) = (0.0f64, [0i8; 3], false);
    loop {
        if t > reach as f64 {
            return None;
        }
        let inside = (0..3).all(|i| (lo[i]..hi[i]).contains(&cell[i]));
        if inside {
            entered = true;
            let coords = [cell[0].div_euclid(cw), cell[1].div_euclid(ch), cell[2].div_euclid(cw)];
            let occ = cache.entry(coords).or_insert_with(|| chunks(coords).map(StudChunk::occupancy));
            let local = [cell[0].rem_euclid(cw), cell[1].rem_euclid(ch), cell[2].rem_euclid(cw)];
            if occ.as_ref().is_some_and(|o| o.get(local.map(|v| v as i32))) {
                return Some(StudRayHit { chunk: coords, cell: local.map(|v| v as u8), normal, t: t as f32 });
            }
        } else if entered {
            return None;
        }
        let axis = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] {
            0
        } else if t_max[1] <= t_max[2] {
            1
        } else {
            2
        };
        if !t_max[axis].is_finite() {
            return None;
        }
        t = t_max[axis];
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
        normal = [0; 3];
        normal[axis] = -step[axis] as i8;
    }
}

/// The brick a build click would place against `hit`: its min cell is the cell in
/// front of the struck face, shifted so the footprint is centred under the cursor on
/// x/z (and, for a face struck from below, so the brick hangs down from that cell).
/// `None` for an unknown part, or a target outside the region / legal layers.
pub fn placement_for(hit: &StudHit, part: u16, rot: u8, material: u16) -> Option<(StudChunkCoords, StudBrick)> {
    let n = crate::REGION_CHUNKS as i64;
    let from = StudRayHit {
        chunk: [hit.chunk.x as i64, hit.chunk.y as i64, hit.chunk.z as i64],
        cell: hit.cell,
        normal: hit.normal,
        t: hit.t,
    };
    let ([x, y, z], b) = placement_for_ray(&from, part, rot, material)?;
    if !(0..n).contains(&x) || !(0..n).contains(&z) {
        return None;
    }
    Some((StudChunkCoords { x: x as usize, y: y as i32, z: z as usize }, b))
}

/// [`placement_for`] in signed chunk coordinates `[x, layer, z]`: the target chunk may
/// lie past the region's edge (a neighbour's — placing there is an intent to its owner,
/// `Order::PlaceStudAt`). `None` for an unknown part or a target outside the legal layers.
pub fn placement_for_ray(hit: &StudRayHit, part: u16, rot: u8, material: u16) -> Option<([i64; 3], StudBrick)> {
    let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
    let g = [
        hit.chunk[0] * cw + hit.cell[0] as i64 + hit.normal[0] as i64,
        hit.chunk[1] * ch + hit.cell[1] as i64 + hit.normal[1] as i64,
        hit.chunk[2] * cw + hit.cell[2] as i64 + hit.normal[2] as i64,
    ];
    let coords = [g[0].div_euclid(cw), g[1].div_euclid(ch), g[2].div_euclid(cw)];
    if !i32::try_from(coords[1]).is_ok_and(stud_layer_ok) {
        return None;
    }
    let mut b = StudBrick::part_at([0; 3], part, rot, material);
    b.part()?;
    let size = b.size();
    let cell = [g[0].rem_euclid(cw) as i32, g[1].rem_euclid(ch) as i32, g[2].rem_euclid(cw) as i32];
    let down = if hit.normal[1] < 0 { size[1] as i32 - 1 } else { 0 };
    b.min = [
        (cell[0] - (size[0] as i32 - 1) / 2).max(0) as u8,
        (cell[1] - down).max(0) as u8,
        (cell[2] - (size[2] as i32 - 1) / 2).max(0) as u8,
    ];
    Some((coords, b))
}
