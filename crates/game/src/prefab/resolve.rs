//! A [`PrefabFile`] resolved into block ids with a spatial index, so "what is in this cell / this
//! range" is answered from one small bucket and never by walking the prefab.

use std::collections::BTreeMap;
use std::fmt;

use super::file::{FileShape, GroundFit, Marker, PrefabFile, PrefabMeta, PREFAB_FORMAT};
use super::Shape;
use crate::block::BLOCK_VOXELS;
use crate::registry::BlockId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Builtin,
    Untrusted,
}

pub const UNTRUSTED_MAX_EXTENT: u32 = 256;
pub const UNTRUSTED_MAX_ITEMS: usize = 65_536;
/// The spatial index's cell; a prefab-internal constant, not `CHUNK_BLOCKS`.
pub const PREFAB_BUCKET_BLOCKS: i32 = 16;

/// Overflow guard only, not a cost bound: no axis may exceed this, so every cell coordinate and bucket
/// edge fits an `i32` with room to spare. Builtin files are trusted and otherwise uncapped.
const MAX_AXIS: u32 = 1 << 30;
/// Allocation guard only, not a cost bound: the column profile holds `size.x * size.z` entries.
const MAX_PROFILE_COLUMNS: u64 = 1 << 26;
/// Untrusted work budget: summed bucket overlaps of all boxes and carve boxes.
pub const UNTRUSTED_MAX_BUCKET_REFS: u64 = 1 << 20;
/// Untrusted work budget: summed column footprints (x extent times z extent) of all boxes.
pub const UNTRUSTED_MAX_COLUMN_WORK: u64 = 1 << 24;
/// A marker may lie this many cells outside the box on any axis (connectors, entrances).
pub const MARKER_MARGIN: i32 = 64;
const MAX_CUBOID_MATS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    BadFormat(u16),
    EmptyName,
    EmptySize,
    UnknownBlock(String),
    BadPaletteIndex { list: &'static str, index: usize },
    OutsideSize { list: &'static str, index: usize },
    BadShape { index: usize },
    DuplicateShaped { at: [i32; 3] },
    BadTurn { index: usize },
    TooLarge { what: &'static str, limit: usize },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::BadFormat(v) => write!(f, "unsupported prefab format {v}"),
            Problem::EmptyName => write!(f, "the prefab has no name"),
            Problem::EmptySize => write!(f, "the prefab has an empty size"),
            Problem::UnknownBlock(n) => write!(f, "unknown block `{n}`"),
            Problem::BadPaletteIndex { list, index } => write!(f, "{list}[{index}] uses a palette index out of range"),
            Problem::OutsideSize { list, index } => write!(f, "{list}[{index}] lies outside the prefab"),
            Problem::BadShape { index } => write!(f, "shaped[{index}] is not a valid shape"),
            Problem::DuplicateShaped { at } => write!(f, "two shaped cells at {at:?}"),
            Problem::BadTurn { index } => write!(f, "markers[{index}] has a turn above 3"),
            Problem::TooLarge { what, limit } => write!(f, "too large: {what} exceeds {limit}"),
        }
    }
}

/// What one cell of a prefab holds. Precedence, lowest first: Keep, carve boxes, boxes in file order
/// (a later box wins), shaped cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell<'a> {
    Keep,
    Carve,
    Block(BlockId),
    Shaped(&'a ResolvedShape),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedShape {
    Named { shape: Shape, mat: BlockId },
    Cuboids { cuboids: Vec<([u8; 3], [u8; 3], u8)>, mats: Vec<BlockId> },
}

/// The block written for a `Cuboids` shape is `mats[0]` (layout writes `mats.first()`); the material the
/// column profile reports for it ([`ResolvedShape::top_material`], so `column_top`) is that of its highest
/// voxel instead.
impl ResolvedShape {
    /// The material of the highest voxel (the later cuboid wins a tie).
    pub fn top_material(&self) -> Option<BlockId> {
        match self {
            ResolvedShape::Named { mat, .. } => Some(*mat),
            ResolvedShape::Cuboids { cuboids, mats } => cuboids
                .iter()
                .enumerate()
                .max_by_key(|(i, (_, max, _))| (max[1], *i))
                .and_then(|(_, (_, _, m))| mats.get(*m as usize).copied())
                .or_else(|| mats.first().copied()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResBox {
    min: [i32; 3],
    /// Exclusive.
    max: [i32; 3],
    mat: BlockId,
}

impl ResBox {
    fn contains(&self, at: [i32; 3]) -> bool {
        (0..3).all(|a| self.min[a] <= at[a] && at[a] < self.max[a])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResShaped {
    at: [i32; 3],
    shape: ResolvedShape,
}

/// Indices of what overlaps one index cell. A box covering the whole bucket drops everything below it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Bucket {
    /// File order, only those that can still show.
    boxes: Vec<u32>,
    carve: Vec<u32>,
    /// Ascending by `at`.
    shaped: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPrefab {
    pub name: String,
    pub name_key: u32,
    pub meta: PrefabMeta,
    pub size: [u32; 3],
    pub anchor: [i32; 3],
    pub ground: GroundFit,
    pub tags: Vec<String>,
    pub markers: Vec<Marker>,
    boxes: Vec<ResBox>,
    carve: Vec<ResBox>,
    /// Sorted by `at`.
    shaped: Vec<ResShaped>,
    /// Keyed `[bz, by, bx]` so one bucket row is a key range.
    buckets: BTreeMap<[i32; 3], Bucket>,
    /// `x + z * size.x`.
    profile: Vec<Option<(i32, BlockId)>>,
    /// Lowest Block or Shaped cell per column, same indexing.
    bottoms: Vec<Option<i32>>,
    solid: u64,
}

pub fn prefab_name_key(name: &str) -> u32 {
    crc32fast::hash(name.as_bytes())
}

/// A file box as `[min, exclusive max)`, or `None` when it leaves `0..size` (overflow-safe).
fn bounds(min: [i32; 3], size: [u32; 3], dims: [u32; 3]) -> Option<([i32; 3], [i32; 3])> {
    let mut max = [0i32; 3];
    for a in 0..3 {
        let hi = min[a] as i64 + size[a] as i64;
        if min[a] < 0 || hi > dims[a] as i64 {
            return None;
        }
        max[a] = hi as i32;
    }
    Some((min, max))
}

fn inside(at: [i32; 3], dims: [u32; 3]) -> bool {
    (0..3).all(|a| at[a] >= 0 && (at[a] as i64) < dims[a] as i64)
}

/// Within `MARKER_MARGIN` cells of `0..size` on every axis.
fn near_box(at: [i32; 3], dims: [u32; 3]) -> bool {
    (0..3).all(|a| at[a] as i64 >= -(MARKER_MARGIN as i64) && (at[a] as i64) < dims[a] as i64 + MARKER_MARGIN as i64)
}

fn is_empty_box(b: &ResBox) -> bool {
    (0..3).any(|a| b.min[a] >= b.max[a])
}

pub fn resolve(
    file: &PrefabFile,
    block_named: &dyn Fn(&str) -> Option<BlockId>,
    trust: Trust,
) -> Result<ResolvedPrefab, Vec<Problem>> {
    let mut problems = Vec::new();
    let dims = file.size;
    if file.format != PREFAB_FORMAT {
        problems.push(Problem::BadFormat(file.format));
    }
    if file.name.is_empty() {
        problems.push(Problem::EmptyName);
    }
    if dims.contains(&0) {
        problems.push(Problem::EmptySize);
    }
    let extent_limit = match trust {
        Trust::Untrusted => UNTRUSTED_MAX_EXTENT,
        Trust::Builtin => MAX_AXIS,
    };
    if dims.iter().any(|&d| d > extent_limit) {
        problems.push(Problem::TooLarge { what: "extent", limit: extent_limit as usize });
    }
    let items = file.boxes.len() + file.shaped.len() + file.carve.len() + file.markers.len();
    if trust == Trust::Untrusted && items > UNTRUSTED_MAX_ITEMS {
        problems.push(Problem::TooLarge { what: "items", limit: UNTRUSTED_MAX_ITEMS });
    }
    let columns = dims[0] as u64 * dims[2] as u64;
    if columns > MAX_PROFILE_COLUMNS && !dims.iter().any(|&d| d > extent_limit) {
        problems.push(Problem::TooLarge { what: "columns", limit: MAX_PROFILE_COLUMNS as usize });
    }
    // An untrusted file over a cap is refused before any of its content is looked at.
    if trust == Trust::Untrusted && !problems.is_empty() {
        return Err(problems);
    }

    let mut palette = Vec::with_capacity(file.palette.len());
    for name in &file.palette {
        match block_named(name) {
            Some(id) => palette.push(Some(id)),
            None => {
                problems.push(Problem::UnknownBlock(name.clone()));
                palette.push(None);
            }
        }
    }
    // `None`: the index is out of range (reported here) or names an unknown block (reported once above).
    let pal = |index: u16, list: &'static str, at: usize, problems: &mut Vec<Problem>| -> Option<BlockId> {
        match palette.get(index as usize) {
            Some(id) => *id,
            None => {
                problems.push(Problem::BadPaletteIndex { list, index: at });
                None
            }
        }
    };

    let mut boxes = Vec::with_capacity(file.boxes.len());
    for (i, b) in file.boxes.iter().enumerate() {
        let mat = pal(b.mat, "boxes", i, &mut problems);
        match bounds(b.min, b.size, dims) {
            Some((min, max)) => {
                if let Some(mat) = mat {
                    boxes.push(ResBox { min, max, mat });
                }
            }
            None => problems.push(Problem::OutsideSize { list: "boxes", index: i }),
        }
    }
    let mut carve = Vec::with_capacity(file.carve.len());
    for (i, c) in file.carve.iter().enumerate() {
        match bounds(c.min, c.size, dims) {
            Some((min, max)) => carve.push(ResBox { min, max, mat: BlockId(0) }),
            None => problems.push(Problem::OutsideSize { list: "carve", index: i }),
        }
    }

    if trust == Trust::Untrusted {
        let (mut refs, mut cols) = (0u64, 0u64);
        for (i, b) in boxes.iter().chain(carve.iter()).enumerate() {
            if is_empty_box(b) {
                continue;
            }
            refs += (0..3)
                .map(|a| ((b.max[a] - 1) / PREFAB_BUCKET_BLOCKS - b.min[a] / PREFAB_BUCKET_BLOCKS + 1) as u64)
                .product::<u64>();
            if i < boxes.len() {
                cols += (b.max[0] - b.min[0]) as u64 * (b.max[2] - b.min[2]) as u64;
            }
            if refs > UNTRUSTED_MAX_BUCKET_REFS {
                problems.push(Problem::TooLarge { what: "box overlap", limit: UNTRUSTED_MAX_BUCKET_REFS as usize });
                return Err(problems);
            }
            if cols > UNTRUSTED_MAX_COLUMN_WORK {
                problems.push(Problem::TooLarge { what: "box overlap", limit: UNTRUSTED_MAX_COLUMN_WORK as usize });
                return Err(problems);
            }
        }
    }

    let mut shaped: Vec<ResShaped> = Vec::with_capacity(file.shaped.len());
    for (i, s) in file.shaped.iter().enumerate() {
        if !inside(s.at, dims) {
            problems.push(Problem::OutsideSize { list: "shaped", index: i });
        }
        let resolved = match &s.shape {
            FileShape::Named { shape, mat } => {
                if !shape.is_valid() || shape.is_empty() {
                    problems.push(Problem::BadShape { index: i });
                }
                pal(*mat, "shaped", i, &mut problems).map(|mat| ResolvedShape::Named { shape: *shape, mat })
            }
            FileShape::Cuboids { cuboids, mats } => {
                let ok = !cuboids.is_empty()
                    && mats.len() <= MAX_CUBOID_MATS
                    && cuboids.iter().all(|(min, max, m)| {
                        (*m as usize) < mats.len()
                            && (0..3).all(|a| min[a] < max[a] && (max[a] as usize) <= BLOCK_VOXELS)
                    });
                if !ok {
                    problems.push(Problem::BadShape { index: i });
                }
                let ids: Vec<Option<BlockId>> =
                    mats.iter().take(MAX_CUBOID_MATS).map(|m| pal(*m, "shaped", i, &mut problems)).collect();
                if ok && ids.iter().all(Option::is_some) {
                    Some(ResolvedShape::Cuboids {
                        cuboids: cuboids.clone(),
                        mats: ids.into_iter().flatten().collect(),
                    })
                } else {
                    None
                }
            }
        };
        if let Some(shape) = resolved {
            shaped.push(ResShaped { at: s.at, shape });
        }
    }
    shaped.sort_by_key(|s| s.at);
    // Each duplicated position once, whether or not its twins resolved: one sorted pass over the file.
    let mut ats: Vec<[i32; 3]> = file.shaped.iter().map(|s| s.at).collect();
    ats.sort_unstable();
    for run in ats.chunk_by(|a, b| a == b) {
        if run.len() > 1 {
            problems.push(Problem::DuplicateShaped { at: run[0] });
        }
    }

    for (i, m) in file.markers.iter().enumerate() {
        if m.turn > 3 {
            problems.push(Problem::BadTurn { index: i });
        }
        if !near_box(m.at, dims) {
            problems.push(Problem::OutsideSize { list: "markers", index: i });
        }
    }

    if !problems.is_empty() {
        return Err(problems);
    }

    let mut buckets: BTreeMap<[i32; 3], Bucket> = BTreeMap::new();
    for (list, is_carve) in [(&boxes, false), (&carve, true)] {
        for (i, b) in list.iter().enumerate() {
            if is_empty_box(b) {
                continue;
            }
            let lo = b.min.map(|v| v / PREFAB_BUCKET_BLOCKS);
            let hi = b.max.map(|v| (v - 1) / PREFAB_BUCKET_BLOCKS);
            for bz in lo[2]..=hi[2] {
                for by in lo[1]..=hi[1] {
                    for bx in lo[0]..=hi[0] {
                        let bucket = buckets.entry([bz, by, bx]).or_default();
                        if is_carve {
                            bucket.carve.push(i as u32);
                        } else {
                            bucket.boxes.push(i as u32);
                        }
                    }
                }
            }
        }
    }
    for (i, s) in shaped.iter().enumerate() {
        let k = s.at.map(|v| v / PREFAB_BUCKET_BLOCKS);
        buckets.entry([k[2], k[1], k[0]]).or_default().shaped.push(i as u32);
    }

    let mut solid = 0u64;
    for (key, bucket) in buckets.iter_mut() {
        let (lo, hi) = bucket_range(*key, dims);
        let covering = bucket.boxes.iter().rposition(|&i| {
            let b = &boxes[i as usize];
            (0..3).all(|a| b.min[a] <= lo[a] && b.max[a] >= hi[a])
        });
        if let Some(p) = covering {
            bucket.boxes.drain(..p);
            bucket.carve.clear();
            solid += (0..3).map(|a| (hi[a] - lo[a]) as u64).product::<u64>();
        } else {
            solid += occupied_cells(bucket, &boxes, &shaped, lo, hi);
        }
    }

    let (profile, bottoms) = column_profile(dims, &boxes, &shaped);

    Ok(ResolvedPrefab {
        name_key: prefab_name_key(&file.name),
        name: file.name.clone(),
        meta: file.meta.clone(),
        size: dims,
        anchor: file.anchor,
        ground: file.ground,
        tags: file.tags.clone(),
        markers: file.markers.clone(),
        boxes,
        carve,
        shaped,
        buckets,
        profile,
        bottoms,
        solid,
    })
}

/// `[lo, hi)` of a bucket clipped to the prefab.
fn bucket_range(key: [i32; 3], dims: [u32; 3]) -> ([i32; 3], [i32; 3]) {
    let b = [key[2], key[1], key[0]];
    let lo = b.map(|v| v * PREFAB_BUCKET_BLOCKS);
    let hi = [0, 1, 2].map(|a| (lo[a] + PREFAB_BUCKET_BLOCKS).min(dims[a] as i32));
    (lo, hi)
}

/// Cells of one bucket held by a box or a shaped cell (the bucket is not covered by one box).
fn occupied_cells(bucket: &Bucket, boxes: &[ResBox], shaped: &[ResShaped], lo: [i32; 3], hi: [i32; 3]) -> u64 {
    const N: usize = PREFAB_BUCKET_BLOCKS as usize;
    let mut grid = vec![false; N * N * N];
    let idx = |x: i32, y: i32, z: i32| ((z - lo[2]) as usize * N + (y - lo[1]) as usize) * N + (x - lo[0]) as usize;
    for &i in &bucket.boxes {
        let b = &boxes[i as usize];
        let from = [0, 1, 2].map(|a| b.min[a].max(lo[a]));
        let to = [0, 1, 2].map(|a| b.max[a].min(hi[a]));
        for z in from[2]..to[2] {
            for y in from[1]..to[1] {
                for x in from[0]..to[0] {
                    grid[idx(x, y, z)] = true;
                }
            }
        }
    }
    for &i in &bucket.shaped {
        let at = shaped[i as usize].at;
        grid[idx(at[0], at[1], at[2])] = true;
    }
    grid.iter().filter(|&&c| c).count() as u64
}

/// Top solid cell (and material) and bottom solid cell per column, from box bounds (never per cell).
/// Shaped wins a tie at the same height, a later box wins over an earlier one.
#[allow(clippy::type_complexity)]
fn column_profile(
    dims: [u32; 3],
    boxes: &[ResBox],
    shaped: &[ResShaped],
) -> (Vec<Option<(i32, BlockId)>>, Vec<Option<i32>>) {
    let (w, d) = (dims[0] as usize, dims[2] as usize);
    let mut profile: Vec<Option<(i32, BlockId)>> = vec![None; w * d];
    let mut bottoms: Vec<Option<i32>> = vec![None; w * d];
    for b in boxes {
        if is_empty_box(b) {
            continue;
        }
        let top = b.max[1] - 1;
        for z in b.min[2]..b.max[2] {
            let row = z as usize * w;
            for slot in &mut profile[row + b.min[0] as usize..row + b.max[0] as usize] {
                if slot.is_none_or(|(y, _)| top >= y) {
                    *slot = Some((top, b.mat));
                }
            }
            for slot in &mut bottoms[row + b.min[0] as usize..row + b.max[0] as usize] {
                if slot.is_none_or(|y| b.min[1] < y) {
                    *slot = Some(b.min[1]);
                }
            }
        }
    }
    for s in shaped {
        let Some(mat) = s.shape.top_material() else { continue };
        let i = s.at[2] as usize * w + s.at[0] as usize;
        if profile[i].is_none_or(|(y, _)| s.at[1] >= y) {
            profile[i] = Some((s.at[1], mat));
        }
        if bottoms[i].is_none_or(|y| s.at[1] < y) {
            bottoms[i] = Some(s.at[1]);
        }
    }
    (profile, bottoms)
}

impl ResolvedPrefab {
    fn in_bounds(&self, at: [i32; 3]) -> bool {
        inside(at, self.size)
    }

    fn in_bucket(&self, bucket: &Bucket, at: [i32; 3]) -> Cell<'_> {
        if !bucket.shaped.is_empty() {
            if let Ok(p) = bucket.shaped.binary_search_by(|&i| self.shaped[i as usize].at.cmp(&at)) {
                return Cell::Shaped(&self.shaped[bucket.shaped[p] as usize].shape);
            }
        }
        for &i in bucket.boxes.iter().rev() {
            let b = &self.boxes[i as usize];
            if b.contains(at) {
                return Cell::Block(b.mat);
            }
        }
        if bucket.carve.iter().any(|&i| self.carve[i as usize].contains(at)) {
            return Cell::Carve;
        }
        Cell::Keep
    }

    /// Keep outside `0..size`.
    pub fn cell(&self, at: [i32; 3]) -> Cell<'_> {
        if !self.in_bounds(at) {
            return Cell::Keep;
        }
        let key = [at[2], at[1], at[0]].map(|v| v / PREFAB_BUCKET_BLOCKS);
        match self.buckets.get(&key) {
            Some(bucket) => self.in_bucket(bucket, at),
            None => Cell::Keep,
        }
    }

    /// Every non-Keep cell with `min <= cell < max` (clamped to the prefab), in ascending (z, y, x)
    /// order. Cost is bounded by the buckets the range overlaps, not by the prefab.
    pub fn for_each_cell(&self, min: [i32; 3], max: [i32; 3], f: &mut dyn FnMut([i32; 3], Cell<'_>)) {
        let lo = [0, 1, 2].map(|a| min[a].max(0));
        let hi = [0, 1, 2].map(|a| max[a].min(self.size[a] as i32));
        if (0..3).any(|a| lo[a] >= hi[a]) {
            return;
        }
        let bl = lo.map(|v| v / PREFAB_BUCKET_BLOCKS);
        let bh = hi.map(|v| (v - 1) / PREFAB_BUCKET_BLOCKS);
        let span = |b: i32, a: usize| (lo[a].max(b * PREFAB_BUCKET_BLOCKS), hi[a].min((b + 1) * PREFAB_BUCKET_BLOCKS));
        for bz in bl[2]..=bh[2] {
            let (z0, z1) = span(bz, 2);
            // The bucket rows of this z layer, one per by: (y range, [(bx, bucket)]).
            let rows: Vec<((i32, i32), Vec<(i32, &Bucket)>)> = (bl[1]..=bh[1])
                .map(|by| {
                    let row = self
                        .buckets
                        .range([bz, by, bl[0]]..=[bz, by, bh[0]])
                        .map(|(k, b)| (k[2], b))
                        .collect();
                    (span(by, 1), row)
                })
                .collect();
            for z in z0..z1 {
                for ((y0, y1), row) in &rows {
                    for y in *y0..*y1 {
                        for &(bx, bucket) in row {
                            let (x0, x1) = span(bx, 0);
                            for x in x0..x1 {
                                let at = [x, y, z];
                                match self.in_bucket(bucket, at) {
                                    Cell::Keep => {}
                                    c => f(at, c),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Highest Block or Shaped cell of column (x, z): (y, top material); None for an empty column or outside.
    pub fn column_top(&self, x: i32, z: i32) -> Option<(i32, BlockId)> {
        if !self.in_bounds([x, 0, z]) {
            return None;
        }
        self.profile[z as usize * self.size[0] as usize + x as usize]
    }

    /// Lowest Block or Shaped cell of column (x, z); None for an empty column or outside.
    pub fn column_bottom(&self, x: i32, z: i32) -> Option<i32> {
        if !self.in_bounds([x, 0, z]) {
            return None;
        }
        self.bottoms[z as usize * self.size[0] as usize + x as usize]
    }

    /// Block + Shaped cells.
    pub fn solid_count(&self) -> u64 {
        self.solid
    }
}
