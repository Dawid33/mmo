//! Sim tiles on either kind of terrain (stud world plan P6a).
//!
//! A `TilePos` is one nav / designation cell. On voxel terrain it is a 1 m block; on
//! stud terrain it is [`TILE_STUDS`] x [`TILE_STUDS`] studs wide and [`TILE_PLATES`]
//! plates tall (1 m x 1.2 m x 1 m): tile `(tx, ty, tz)` covers studs
//! `[2tx, 2tx + 2)`, plates `[6ty, 6ty + 6)`, studs `[2tz, 2tz + 2)` of the region's
//! stud-cell frame (plate 0 is the bottom of layer 0).
//!
//! This file is the ONE place that
//! - says which kind a region is ([`Rollback::terrain_kind`]),
//! - converts tiles to sim units and back ([`TerrainKind`]'s methods), and
//! - derives a stud tile's [`TileFill`] and material from stud cells ([`StudTile`]),
//!   over materialised chunks or the recipe's implied ground, own region ∪ stud halo.
//!
//! Nothing here is state: a [`StudTile`] is a pure function of hashed state, computed
//! on demand. [`TileView`] memoises whole chunks for the length of ONE borrow of the
//! `Rollback` (a path search), so it cannot outlive an edit or a rollback.

use std::cell::RefCell;
use std::collections::BTreeMap;

use rapier3d::math::Vector;

use crate::halo::in_halo_band;
use crate::movement::{BodyScale, STUD_PLAYER_HEIGHT_M, STUD_STEP_HEIGHT_M, UNITS_PER_METER};
use crate::stud::{
    plate_units, stud_layer_ok, StudBrick, StudChunk, StudChunkCoords, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS,
    STUD_UNITS, TILE_PLATES, TILE_STUDS,
};
use crate::stud_ground::StudColumnGround;
use crate::stud_material::stud_material;
use crate::{
    BlockId, ColumnCoords, Rollback, SlopeShape, TilePos, TileSource, WorldChunkPos, BLOCK_VOXELS, REGION_CHUNKS,
};

/// What a region's terrain is made of. Decided in one place: [`Rollback::terrain_kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerrainKind {
    /// 1 m blocks: a tile is a block.
    #[default]
    Voxel,
    /// Toy bricks: a tile is 2 x 2 studs x 6 plates.
    Stud,
}

/// Metres per plate.
pub const PLATE_M: f32 = 0.2;
const _: () = assert!(16.0 / 5.0 / UNITS_PER_METER == PLATE_M);
/// Stud tiles per chunk along x and z.
pub const STUD_CHUNK_TILES: usize = STUD_CHUNK_STUDS / TILE_STUDS;
/// Stud tiles per chunk layer.
pub const STUD_LAYER_TILES: usize = STUD_CHUNK_PLATES / TILE_PLATES;
// A stud tile is as wide as a block, so tile columns, goals and region edges mean the
// same place on both terrains.
const _: () = assert!(TILE_STUDS * STUD_UNITS == BLOCK_VOXELS);

/// Plates a stud-world body walks up without jumping: the whole plates within
/// `STUD_STEP_HEIGHT_M` (one brick course).
pub const STUD_STEP_PLATES: i32 = 3;
const _: () = assert!(
    STUD_STEP_PLATES as f32 * PLATE_M <= STUD_STEP_HEIGHT_M && (STUD_STEP_PLATES + 1) as f32 * PLATE_M > STUD_STEP_HEIGHT_M
);
/// Plates a stud-world body is tall (`STUD_PLAYER_HEIGHT_M`, rounded up to whole plates).
pub const STUD_BODY_PLATES: i32 = 11;
const _: () = assert!(
    (STUD_BODY_PLATES - 1) as f32 * PLATE_M < STUD_PLAYER_HEIGHT_M
        && STUD_PLAYER_HEIGHT_M < STUD_BODY_PLATES as f32 * PLATE_M + 0.001
);
/// Farthest (plates) a path steps DOWN between neighbouring tiles: one tile, as a
/// voxel path drops one block.
pub const STUD_DROP_PLATES: i32 = TILE_PLATES as i32;
/// Lowest floor (plates above a tile's bottom, so negative: in the tile below) a body
/// standing in the tile may rest on. With [`STUD_STEP_PLATES`] as the highest, every
/// floor height belongs to exactly one tile.
pub const STUD_FLOOR_MIN_PLATES: i32 = STUD_STEP_PLATES + 1 - TILE_PLATES as i32;

/// How far (plates) under a floor the feet of a body standing on it may be measured:
/// float noise and the controller's skin, well under the half plate of a stud cap.
pub const STUD_FEET_SLACK_PLATES: f32 = 0.25;

/// Height of the stud caps on a studded floor, sim units (half a plate: one fine
/// collider cell, `stud_collider.rs`). A body placed on a stud tile starts on them.
pub fn stud_cap_units() -> f32 {
    crate::stud_collider::STUD_FINE_CELL.y
}

impl TerrainKind {
    /// The terrain of a region whose bodies have `scale` (`GameData::body_scale`): the
    /// decision behind [`Rollback::terrain_kind`], for code that holds only the scale.
    pub fn of(scale: BodyScale) -> Self {
        if scale == BodyScale::STUD { TerrainKind::Stud } else { TerrainKind::Voxel }
    }

    /// A tile's width (x and z), sim units.
    pub fn tile_width_units(self) -> f32 {
        match self {
            TerrainKind::Voxel => BLOCK_VOXELS as f32,
            TerrainKind::Stud => (TILE_STUDS * STUD_UNITS) as f32,
        }
    }

    /// A tile's height, sim units.
    pub fn tile_height_units(self) -> f32 {
        match self {
            TerrainKind::Voxel => BLOCK_VOXELS as f32,
            TerrainKind::Stud => plate_units(TILE_PLATES as i32),
        }
    }

    /// How far below and above a tile's bottom (in tiles) the feet of a body standing
    /// in it may be: half a block either way on voxels (the level nearest the feet);
    /// on studs the floors [`STUD_FLOOR_MIN_PLATES`]`..=`[`STUD_STEP_PLATES`], each
    /// taking feet from [`STUD_FEET_SLACK_PLATES`] below it to a plate less that above
    /// (feet rest on the stud caps, half a plate over the floor, or on a smooth top).
    fn level_window(self) -> (f32, f32) {
        match self {
            TerrainKind::Voxel => (-0.5, 0.5),
            TerrainKind::Stud => (
                (STUD_FLOOR_MIN_PLATES as f32 - STUD_FEET_SLACK_PLATES) / TILE_PLATES as f32,
                (STUD_STEP_PLATES as f32 + 1.0 - STUD_FEET_SLACK_PLATES) / TILE_PLATES as f32,
            ),
        }
    }

    /// The whole plate feet at height `feet_y` (sim units) stand on: the plate at or
    /// just under them, [`STUD_FEET_SLACK_PLATES`] of slack below.
    pub fn feet_plate(feet_y: f32) -> i64 {
        (feet_y / plate_units(1) + STUD_FEET_SLACK_PLATES).floor() as i64
    }

    /// The tile feet at `feet` (region-local sim units) occupy: the column under them,
    /// at the level whose [window](Self::level_window) holds them.
    pub fn feet_tile(self, feet: Vector) -> TilePos {
        let (w, h) = (self.tile_width_units(), self.tile_height_units());
        let (lo, _) = self.level_window();
        [(feet.x / w).floor() as i32, (feet.y / h - lo).floor() as i32, (feet.z / w).floor() as i32]
    }

    /// Whether feet at height `feet_y` (sim units) are on `level`.
    pub fn on_level(self, level: i32, feet_y: f32) -> bool {
        let (lo, hi) = self.level_window();
        let d = feet_y / self.tile_height_units() - level as f32;
        lo < d && d < hi
    }

    /// The centre of tile column `[x, z]`, region-local sim units.
    pub fn column_centre(self, column: [i32; 2]) -> (f32, f32) {
        let w = self.tile_width_units();
        ((column[0] as f32 + 0.5) * w, (column[1] as f32 + 0.5) * w)
    }

    /// The bottom of `level`, sim units.
    pub fn level_bottom_units(self, level: i32) -> f32 {
        level as f32 * self.tile_height_units()
    }
}

/// How much of a tile is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileFill {
    /// Nothing in it.
    Open,
    /// Partly filled: the highest filled plate over the tile's four stud columns is
    /// this many plates above its bottom (`1..=TILE_PLATES`; `TILE_PLATES` only when
    /// part of the footprint stays lower, or a non-box part is in it).
    Floor(u8),
    /// The whole footprint is filled to the tile's top. (A voxel block that collides.)
    Solid,
}

/// The fill a voxel block gives its tile: `Solid` when it collides, else `Open`.
pub fn voxel_tile_fill(block: BlockId) -> TileFill {
    if block != BlockId::AIR && crate::block_props().is_solid(block) { TileFill::Solid } else { TileFill::Open }
}

/// One stud tile, derived from its 2 x 2 x 6 stud cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StudTile {
    /// One past the highest filled plate of each stud column, plates above the tile's
    /// bottom (0 = the column is empty in this tile). Index `z * TILE_STUDS + x`.
    pub tops: [u8; TILE_STUDS * TILE_STUDS],
    /// The material of each column's highest filled cell (meaningless where `tops` is 0).
    mats: [u16; TILE_STUDS * TILE_STUDS],
    /// A non-box part (a slope…) lies in the tile: it is never `Solid`.
    pub partial: bool,
}

impl StudTile {
    /// The highest filled plate over the four columns.
    pub fn floor_plates(&self) -> u8 {
        self.tops.iter().copied().max().unwrap_or(0)
    }

    pub fn fill(&self) -> TileFill {
        let top = self.floor_plates();
        if top == 0 {
            TileFill::Open
        } else if !self.partial && self.tops.iter().all(|&t| t as usize == TILE_PLATES) {
            TileFill::Solid
        } else {
            TileFill::Floor(top)
        }
    }

    /// The tile's dominant material: that of its topmost filled cell (on a tie, the
    /// lowest stud index `z * TILE_STUDS + x`). `None` for an open tile.
    pub fn material(&self) -> Option<u16> {
        let top = self.floor_plates();
        (top > 0).then(|| self.mats[self.tops.iter().position(|&t| t == top).unwrap_or(0)])
    }

    /// Filled-to-the-top cells counted per column: the cells under each column's top.
    /// (An upper bound of the filled cells: a hollow under a column's top counts.)
    pub fn cells_under_tops(&self) -> u32 {
        self.tops.iter().map(|&t| u32::from(t)).sum()
    }

    /// Whether nothing in the tile can be dug: its dominant material is undiggable.
    pub fn is_bedrock(&self) -> bool {
        self.material().and_then(stud_material).is_some_and(|m| m.hardness == u16::MAX)
    }

    /// Adds the part of `b` (a brick of the chunk) lying in the tile whose min cell,
    /// chunk-local, is `tmin`.
    fn add_brick(&mut self, tmin: [i32; 3], b: &StudBrick) {
        let size = b.size();
        let (ts, tp) = (TILE_STUDS as i32, TILE_PLATES as i32);
        let lo = [b.min[0] as i32, b.min[1] as i32, b.min[2] as i32];
        let hi = [lo[0] + size[0] as i32, lo[1] + size[1] as i32, lo[2] + size[2] as i32];
        if hi[1] <= tmin[1] || lo[1] >= tmin[1] + tp {
            return;
        }
        let top = (hi[1].min(tmin[1] + tp) - tmin[1]) as u8;
        let mut touched = false;
        for z in lo[2].max(tmin[2])..hi[2].min(tmin[2] + ts) {
            for x in lo[0].max(tmin[0])..hi[0].min(tmin[0] + ts) {
                touched = true;
                let col = ((z - tmin[2]) * ts + (x - tmin[0])) as usize;
                if top > self.tops[col] {
                    self.tops[col] = top;
                    self.mats[col] = b.material;
                }
            }
        }
        if touched && !b.is_box() {
            self.partial = true;
        }
    }

    /// The tile at chunk-local tile `local` of a materialised chunk.
    pub fn of_chunk(chunk: &StudChunk, local: [usize; 3]) -> StudTile {
        let tmin = tile_min_cell(local);
        let mut t = StudTile::default();
        for b in chunk.bricks() {
            t.add_brick(tmin, b);
        }
        t
    }

    /// The tile at chunk-local tile `local` of layer `layer` as the recipe `g` implies
    /// it (no chunk materialised there).
    pub fn of_ground(g: &StudColumnGround, layer: i32, local: [usize; 3]) -> StudTile {
        let base = i64::from(layer) * STUD_CHUNK_PLATES as i64 + (local[1] * TILE_PLATES) as i64;
        let mut t = StudTile::default();
        for dz in 0..TILE_STUDS {
            for dx in 0..TILE_STUDS {
                let (sx, sz) = (local[0] * TILE_STUDS + dx, local[2] * TILE_STUDS + dz);
                let top = (i64::from(g.surface(sx, sz)) - base).clamp(0, TILE_PLATES as i64);
                if top > 0 {
                    let col = dz * TILE_STUDS + dx;
                    t.tops[col] = top as u8;
                    t.mats[col] = g.material_at(sx, (base + top - 1) as i32, sz).unwrap_or_default();
                }
            }
        }
        t
    }
}

/// Min cell, chunk-local, of chunk-local tile `local`.
fn tile_min_cell(local: [usize; 3]) -> [i32; 3] {
    [(local[0] * TILE_STUDS) as i32, (local[1] * TILE_PLATES) as i32, (local[2] * TILE_STUDS) as i32]
}

fn table_index(local: [usize; 3]) -> usize {
    (local[1] * STUD_CHUNK_TILES + local[2]) * STUD_CHUNK_TILES + local[0]
}

/// Every tile of a materialised chunk, by [`table_index`]: one pass over its bricks.
fn chunk_tiles(chunk: &StudChunk) -> Box<[StudTile]> {
    let mut out = vec![StudTile::default(); STUD_CHUNK_TILES * STUD_LAYER_TILES * STUD_CHUNK_TILES].into_boxed_slice();
    for b in chunk.bricks() {
        let size = b.size();
        let first = [b.min[0] as usize / TILE_STUDS, b.min[1] as usize / TILE_PLATES, b.min[2] as usize / TILE_STUDS];
        let last = [
            (b.min[0] as usize + size[0] as usize - 1) / TILE_STUDS,
            (b.min[1] as usize + size[1] as usize - 1) / TILE_PLATES,
            (b.min[2] as usize + size[2] as usize - 1) / TILE_STUDS,
        ];
        for ty in first[1]..=last[1] {
            for tz in first[2]..=last[2] {
                for tx in first[0]..=last[0] {
                    let local = [tx, ty, tz];
                    out[table_index(local)].add_brick(tile_min_cell(local), b);
                }
            }
        }
    }
    out
}

/// Every tile of a materialised chunk in one pass over its bricks, indexed by
/// [`stud_tile_table_index`]: what [`TileView`] memoises (for the client's tile mirror).
pub fn stud_chunk_tile_table(chunk: &StudChunk) -> Box<[StudTile]> {
    chunk_tiles(chunk)
}

/// Index of chunk-local tile `local` in a [`stud_chunk_tile_table`].
pub fn stud_tile_table_index(local: [usize; 3]) -> usize {
    table_index(local)
}

/// The chunk `[x, layer, z]` (region-local, may lie past the edge) holding stud tile
/// `p`, and the tile inside it; `None` outside the legal layers.
pub fn split_stud_tile(p: TilePos) -> Option<([i32; 3], [usize; 3])> {
    split_tile(p)
}

/// The chunk `[x, layer, z]` (region-local, may lie past the edge) holding tile `p`,
/// and the tile inside it. `None` outside the legal layers.
fn split_tile(p: TilePos) -> Option<([i32; 3], [usize; 3])> {
    let (n, l) = (STUD_CHUNK_TILES as i32, STUD_LAYER_TILES as i32);
    let layer = p[1].div_euclid(l);
    stud_layer_ok(layer).then(|| {
        (
            [p[0].div_euclid(n), layer, p[2].div_euclid(n)],
            [p[0].rem_euclid(n) as usize, p[1].rem_euclid(l) as usize, p[2].rem_euclid(n) as usize],
        )
    })
}

/// The stud cell box of tile `p` in the region's stud-cell frame: `(lo, hi)`, `hi`
/// exclusive, `[stud x, plate y, stud z]`. `None` when it leaves `i32`.
pub fn stud_tile_box(p: TilePos) -> Option<([i32; 3], [i32; 3])> {
    let (s, t) = (TILE_STUDS as i32, TILE_PLATES as i32);
    let lo = [p[0].checked_mul(s)?, p[1].checked_mul(t)?, p[2].checked_mul(s)?];
    Some((lo, [lo[0].checked_add(s)?, lo[1].checked_add(t)?, lo[2].checked_add(s)?]))
}

/// What a region knows of one stud chunk position.
enum SeenChunk<'a> {
    /// Materialised (its own, or a halo mirror): the bricks are the truth.
    Chunk(&'a StudChunk),
    /// Not materialised: implied by this recipe.
    Ground(&'a StudColumnGround),
    /// Nothing known: air.
    Nothing,
}

impl Rollback {
    /// What this region's terrain is made of — THE region kind switch: a stud region
    /// is one whose bodies are stud bodies (`BodyScale::STUD`, set by every stud
    /// genesis and by `restore_portable` for a region with stud terrain).
    pub fn terrain_kind(&self) -> TerrainKind {
        TerrainKind::of(*self.data.body_scale)
    }

    /// The stud chunk at region-local chunk `[x, layer, z]` as this region sees it, by
    /// the rules of `stud_material_seen`: its own materialised chunk, else its recipe;
    /// past the edge the halo's chunk, else the neighbour's recipe.
    fn stud_chunk_for_tiles(&self, chunk: [i32; 3]) -> SeenChunk<'_> {
        let n = REGION_CHUNKS as i32;
        let [cx, layer, cz] = chunk;
        if (0..n).contains(&cx) && (0..n).contains(&cz) {
            let c = StudChunkCoords { x: cx as usize, y: layer, z: cz as usize };
            if let Some(chunk) = self.stud_chunk_at(c) {
                return SeenChunk::Chunk(chunk);
            }
            return match self.data.stud_ground.get(&ColumnCoords::new(c.x, c.z)) {
                Some(g) => SeenChunk::Ground(g),
                None => SeenChunk::Nothing,
            };
        }
        if !in_halo_band(cx, cz) {
            return SeenChunk::Nothing;
        }
        let key = WorldChunkPos::from_local(*self.data.region, chunk);
        if let Some(c) = self.data.stud_halo.get(&key).and_then(|h| h.chunk.as_ref()) {
            return SeenChunk::Chunk(c);
        }
        match self.data.stud_halo_ground.get(&WorldChunkPos::new(key.x, 0, key.z)) {
            Some(g) => SeenChunk::Ground(g),
            None => SeenChunk::Nothing,
        }
    }

    /// Stud tile `p` (region-local; it may lie past the region's edge), own ∪ stud
    /// halo, implied ground included. Unknown reads as open. Computed from the bricks
    /// on every call: a search that reads many tiles goes through [`Self::tiles`].
    pub fn stud_tile(&self, p: TilePos) -> StudTile {
        let Some((chunk, local)) = split_tile(p) else { return StudTile::default() };
        match self.stud_chunk_for_tiles(chunk) {
            SeenChunk::Chunk(c) => StudTile::of_chunk(c, local),
            SeenChunk::Ground(g) => StudTile::of_ground(g, chunk[1], local),
            SeenChunk::Nothing => StudTile::default(),
        }
    }

    /// The lowest and highest tile level worth scanning in stud tile column `[x, z]`:
    /// the layers of its materialised chunks and the recipe's surface there (own ∪
    /// halo). `None` when nothing is known of the column.
    pub(crate) fn stud_column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        let (n, l) = (STUD_CHUNK_TILES as i32, STUD_LAYER_TILES as i32);
        let (cx, cz) = (column[0].div_euclid(n), column[1].div_euclid(n));
        let (lx, lz) = (column[0].rem_euclid(n) as usize, column[1].rem_euclid(n) as usize);
        let mut band: Option<(i32, i32)> = None;
        let mut add = |lo: i32, hi: i32| band = Some(band.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        let add_ground = |g: &StudColumnGround, add: &mut dyn FnMut(i32, i32)| {
            let tp = TILE_PLATES as i32;
            let tops = (0..TILE_STUDS)
                .flat_map(|dz| (0..TILE_STUDS).map(move |dx| (dx, dz)))
                .map(|(dx, dz)| g.surface(lx * TILE_STUDS + dx, lz * TILE_STUDS + dz));
            let (lo, hi) = tops.fold((i32::MAX, i32::MIN), |(lo, hi), s| (lo.min(s), hi.max(s)));
            // The tiles holding the lowest and the highest top plate, and the one above.
            add(lo.saturating_sub(1).div_euclid(tp), hi.saturating_sub(1).div_euclid(tp).saturating_add(1));
        };
        let rn = REGION_CHUNKS as i32;
        if (0..rn).contains(&cx) && (0..rn).contains(&cz) {
            let (x, z) = (cx as usize, cz as usize);
            let range = StudChunkCoords { x, y: i32::MIN, z: 0 }..=StudChunkCoords { x, y: i32::MAX, z: usize::MAX };
            for c in self.data.stud_index.range(range).map(|(c, _)| c).filter(|c| c.z == z) {
                add(c.y * l, c.y * l + l - 1);
            }
            if let Some(g) = self.data.stud_ground.get(&ColumnCoords::new(x, z)) {
                add_ground(g, &mut add);
            }
        } else if in_halo_band(cx, cz) {
            let key = WorldChunkPos::from_local(*self.data.region, [cx, 0, cz]);
            for (k, _) in self.data.stud_halo.iter().filter(|(k, h)| (k.x, k.z) == (key.x, key.z) && h.chunk.is_some()) {
                add(k.y * l, k.y * l + l - 1);
            }
            if let Some(g) = self.data.stud_halo_ground.get(&key) {
                add_ground(g, &mut add);
            }
        }
        band
    }

    /// This region's tiles for a search that reads many of them: the same answers as
    /// `impl TileSource for Rollback`, with each stud chunk's tiles derived once.
    pub fn tiles(&self) -> TileView<'_> {
        TileView { data: self, kind: self.terrain_kind(), tables: RefCell::new(BTreeMap::new()) }
    }
}

/// [`Rollback::tiles`]: a `TileSource` over a region that memoises stud chunks' tiles
/// while it lives. It borrows the `Rollback`, so no edit or rollback can happen under
/// it; it is never stored, hashed or serialized.
pub struct TileView<'a> {
    data: &'a Rollback,
    kind: TerrainKind,
    tables: RefCell<BTreeMap<[i32; 3], Box<[StudTile]>>>,
}

impl TileView<'_> {
    /// [`Rollback::stud_tile`], memoised per materialised chunk.
    pub fn stud_tile(&self, p: TilePos) -> StudTile {
        let Some((chunk, local)) = split_tile(p) else { return StudTile::default() };
        match self.data.stud_chunk_for_tiles(chunk) {
            SeenChunk::Chunk(c) => {
                self.tables.borrow_mut().entry(chunk).or_insert_with(|| chunk_tiles(c))[table_index(local)]
            }
            SeenChunk::Ground(g) => StudTile::of_ground(g, chunk[1], local),
            SeenChunk::Nothing => StudTile::default(),
        }
    }
}

impl TileSource for TileView<'_> {
    fn block(&self, p: TilePos) -> BlockId {
        TileSource::block(self.data, p)
    }
    fn is_bedrock(&self, p: TilePos) -> bool {
        match self.kind {
            TerrainKind::Voxel => TileSource::is_bedrock(self.data, p),
            TerrainKind::Stud => self.stud_tile(p).is_bedrock(),
        }
    }
    fn is_constructed(&self, p: TilePos) -> bool {
        TileSource::is_constructed(self.data, p)
    }
    fn tree_id(&self, p: TilePos) -> u32 {
        TileSource::tree_id(self.data, p)
    }
    fn tree_limb(&self, p: TilePos) -> u8 {
        TileSource::tree_limb(self.data, p)
    }
    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        TileSource::column_band(self.data, column)
    }
    fn slope(&self, p: TilePos) -> Option<SlopeShape> {
        TileSource::slope(self.data, p)
    }
    fn water_units(&self, p: TilePos) -> u16 {
        TileSource::water_units(self.data, p)
    }
    fn terrain(&self) -> TerrainKind {
        self.kind
    }
    fn tile_fill(&self, p: TilePos) -> TileFill {
        match self.kind {
            TerrainKind::Voxel => voxel_tile_fill(TileSource::block(self.data, p)),
            TerrainKind::Stud => self.stud_tile(p).fill(),
        }
    }
    fn tile_material(&self, p: TilePos) -> Option<u16> {
        match self.kind {
            TerrainKind::Voxel => None,
            TerrainKind::Stud => self.stud_tile(p).material(),
        }
    }
}
