//! Fortress orders: what a player may designate on a region's tiles and the
//! pure rules for which tiles accept which designation. An order covers
//! exactly the box between the two corners the player picked
//! ([`TileBox::new`]). Engine-free and state-free; the sim state and the
//! client tools build on these names.
//!
//! A tile is one 1 m block; `TilePos` is region-local block coordinates and a
//! level is a block `y`.

use std::collections::BTreeMap;

use crate::edit::{raycast_blocks, BlockHit, BlockPos};
use crate::{
    Block, BlockId, ClientId, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback,
    BLOCK_VOXELS, REGION_BLOCKS, UNDERGROUND_DEPTH,
};
use rapier3d::math::Vector;
use state_hash::StateHash;

/// Region-local block coordinates `[x, y, z]`.
pub type TilePos = BlockPos;

/// Priority of an order that does not choose one.
pub const DEFAULT_PRIORITY: u8 = 4;
/// Priorities run `1..=MAX_PRIORITY`.
pub const MAX_PRIORITY: u8 = 7;
/// A `Designate`/`Erase` whose clipped area exceeds this many tiles is
/// rejected whole.
pub const MAX_ORDER_TILES: u64 = 65_536;
/// Lowest and highest level an order may touch: a box reaching outside is
/// rejected whole, which keeps chunk coordinates (`level / CHUNK_BLOCKS`,
/// times the chunk edge in voxels) far from `i32` overflow.
pub const MIN_ORDER_LEVEL: i32 = i16::MIN as i32;
pub const MAX_ORDER_LEVEL: i32 = i16::MAX as i32;

/// Identifies one order: the issuing client and its running sequence number.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize, StateHash,
)]
pub struct OpId {
    pub client: ClientId,
    pub seq: u32,
}

// `Default` only because the rollback map wrapper requires it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum DesignationKind {
    #[default]
    Dig,
    Deconstruct,
    Build(BlockId),
    /// Fell the part of a tree beyond this log or branch block
    /// (`trees::falling_sets`).
    Chop,
}

impl DesignationKind {
    /// Same tool, ignoring the material: every `Build(_)` matches.
    pub fn same_tool(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Dig, Self::Dig)
                | (Self::Deconstruct, Self::Deconstruct)
                | (Self::Build(_), Self::Build(_))
                | (Self::Chop, Self::Chop)
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Designation {
    pub kind: DesignationKind,
    pub op: OpId,
    pub priority: u8,
}

/// An inclusive axis-aligned box of tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct TileBox {
    pub min: TilePos,
    pub max: TilePos,
}

impl TileBox {
    /// The box spanning two corners, in any order.
    pub fn new(a: TilePos, b: TilePos) -> Self {
        Self {
            min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        }
    }

    /// Saturating: an inverted axis counts 0 tiles, a huge box `u64::MAX`.
    pub fn tile_count(&self) -> u64 {
        (0..3)
            .map(|i| (i64::from(self.max[i]) - i64::from(self.min[i]) + 1).max(0) as u64)
            .fold(1, u64::saturating_mul)
    }

    /// Whether `min > max` on any axis (only a box built without
    /// [`TileBox::new`], e.g. off the wire).
    pub fn is_inverted(&self) -> bool {
        (0..3).any(|i| self.min[i] > self.max[i])
    }

    pub fn contains(&self, p: TilePos) -> bool {
        (0..3).all(|i| p[i] >= self.min[i] && p[i] <= self.max[i])
    }

    /// Every tile, `y` outermost, then `z`, then `x` ascending.
    pub fn tiles(&self) -> impl Iterator<Item = TilePos> {
        let (min, max) = (self.min, self.max);
        (min[1]..=max[1]).flat_map(move |y| {
            (min[2]..=max[2]).flat_map(move |z| (min[0]..=max[0]).map(move |x| [x, y, z]))
        })
    }

    /// The part inside the region's columns (`x`, `z` in `0..REGION_BLOCKS`);
    /// `y` is left alone. `None` when nothing is left.
    pub fn clip_to_region(&self) -> Option<TileBox> {
        let (min_x, max_x) = (self.min[0].max(0), self.max[0].min(REGION_BLOCKS as i32 - 1));
        let (min_z, max_z) = (self.min[2].max(0), self.max[2].min(REGION_BLOCKS as i32 - 1));
        (min_x <= max_x && min_z <= max_z).then_some(TileBox {
            min: [min_x, self.min[1], min_z],
            max: [max_x, self.max[1], max_z],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum Order {
    Designate { op: OpId, kind: DesignationKind, priority: u8, area: TileBox },
    /// Remove designations in `area`; `None` = every kind.
    Erase { area: TileBox, only: Option<DesignationKind> },
    CancelOp(OpId),
    DevCompleteAll,
    /// Switch the issuing client's play mode; `at` is read only when
    /// entering Adventure (the tile to put the body on).
    SetMode { mode: crate::PlayMode, at: Option<TilePos> },
    /// Walk the issuing client's adventurer to a goal.
    MoveTo(crate::Goal),
}

/// Why a tile refuses a designation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Invalid {
    Bedrock,
    NothingToDig,
    Built,
    RegionEdge,
    Occupied,
    NotBuilt,
    /// Chop on anything but a solid block of a tree (a log or branch).
    NotTree,
}

/// What `validate` needs to know about the world.
pub trait TileSource {
    /// The block at `p`; `AIR` when unknown.
    fn block(&self, p: TilePos) -> BlockId;
    fn is_bedrock(&self, p: TilePos) -> bool;
    /// The outer ring below the surface that cannot be dug (see `edit.rs`).
    fn edge_locked(&self, p: TilePos) -> bool;
    fn is_constructed(&self, p: TilePos) -> bool;
    /// The tree id of the block at `p` (`Block::tree_id`); 0 when none or
    /// unknown.
    fn tree_id(&self, p: TilePos) -> u32;
    /// The tree limb of the block at `p` (`Block::limb`; 0 = the trunk, or
    /// none).
    fn tree_limb(&self, p: TilePos) -> u8;
    /// The lowest and highest materialised (or implied-surface) block `y` of
    /// block column `[x, z]`; `None` when unknown. Pathfinding scans this
    /// band instead of the fixed `COLUMN_SCAN_Y_MIN..=MAX` constants, which
    /// is what lets ground at any height be stood on.
    fn column_band(&self, _column: [i32; 2]) -> Option<(i32, i32)> {
        None
    }
}

/// Whether `kind` may be designated on tile `p`.
pub fn validate(src: &impl TileSource, kind: DesignationKind, p: TilePos) -> Result<(), Invalid> {
    match kind {
        DesignationKind::Dig => {
            if src.block(p) == BlockId::AIR {
                Err(Invalid::NothingToDig)
            } else if src.is_constructed(p) {
                Err(Invalid::Built)
            } else if src.is_bedrock(p) {
                Err(Invalid::Bedrock)
            } else if src.edge_locked(p) {
                Err(Invalid::RegionEdge)
            } else {
                Ok(())
            }
        }
        DesignationKind::Deconstruct => src.is_constructed(p).then_some(()).ok_or(Invalid::NotBuilt),
        DesignationKind::Build(_) => {
            if src.block(p) == BlockId::AIR {
                Ok(())
            } else {
                Err(Invalid::Occupied)
            }
        }
        DesignationKind::Chop => {
            let is_wood = crate::block_props().is_solid(src.block(p)) && src.tree_id(p) != 0;
            is_wood.then_some(()).ok_or(Invalid::NotTree)
        }
    }
}

/// Tiles of an area that accept an order, and why the rest do not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OrderSummary {
    pub valid: u64,
    pub invalid: BTreeMap<Invalid, u64>,
}

pub fn summarize(src: &impl TileSource, kind: DesignationKind, area: TileBox) -> OrderSummary {
    let mut out = OrderSummary::default();
    for p in area.tiles() {
        match validate(src, kind, p) {
            Ok(()) => out.valid += 1,
            Err(why) => *out.invalid.entry(why).or_default() += 1,
        }
    }
    out
}

/// The part of an order's `area` it acts on: clipped to the region, `None`
/// (reject) when inverted, reaching outside
/// [`MIN_ORDER_LEVEL`]`..=`[`MAX_ORDER_LEVEL`], empty after clipping, or over
/// [`MAX_ORDER_TILES`].
fn order_area(area: TileBox) -> Option<TileBox> {
    if area.is_inverted() || area.min[1] < MIN_ORDER_LEVEL || area.max[1] > MAX_ORDER_LEVEL {
        return None;
    }
    area.clip_to_region().filter(|a| a.tile_count() <= MAX_ORDER_TILES)
}

/// The shared `column_band` probe (sim and client mirror): walks the chunk
/// stack with plain `has_chunk(cy)` lookups (no iteration order) from the
/// lower of the band base and the bedrock chunk (`UNDERGROUND_DEPTH` below the
/// surface: dug space can be materialised anywhere above it) up to
/// [`BAND_HEADROOM_CHUNKS`] past the surface chunk, or over the fallback scan
/// range without a recipe. The band spans the implied base, the surface and
/// every chunk found; `None` with no recipe and no chunk.
pub fn probe_column_band(
    column: [i32; 2],
    ground: Option<&crate::ColumnGround>,
    has_chunk: impl Fn(i32) -> bool,
) -> Option<(i32, i32)> {
    let cb = crate::CHUNK_BLOCKS as i32;
    let (probe_lo, probe_hi, base) = match ground {
        Some(g) => {
            let i = column[0].rem_euclid(cb) as usize * crate::CHUNK_BLOCKS + column[1].rem_euclid(cb) as usize;
            let surface = i32::from(g.surface[i]);
            let bedrock_sub = (surface - UNDERGROUND_DEPTH).div_euclid(cb);
            (
                g.band_base_sub_y.min(bedrock_sub),
                surface.div_euclid(cb) + BAND_HEADROOM_CHUNKS,
                Some((g.band_base_sub_y * cb, surface)),
            )
        }
        None => (crate::COLUMN_SCAN_Y_MIN.div_euclid(cb), crate::COLUMN_SCAN_Y_MAX.div_euclid(cb), None),
    };
    let mut band = base;
    for cy in probe_lo..=probe_hi {
        if has_chunk(cy) {
            let (lo, hi) = (cy * cb, cy * cb + cb - 1);
            band = Some(band.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        }
    }
    band
}

/// Chunks above a column's surface chunk that `column_band` probes (trees,
/// buildings).
pub const BAND_HEADROOM_CHUNKS: i32 = 4;

impl TileSource for Rollback {
    /// See [`probe_column_band`].
    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        let cb = crate::CHUNK_BLOCKS as i32;
        let (cx, cz) = (column[0].div_euclid(cb), column[1].div_euclid(cb));
        let n = crate::REGION_CHUNKS as i32;
        if !(0..n).contains(&cx) || !(0..n).contains(&cz) {
            return None;
        }
        let ground = self.data.ground.get(&crate::ColumnCoords::new(cx as usize, cz as usize));
        probe_column_band(column, ground, |cy| {
            self.chunk_key_at(crate::ChunkCoords::new(cx as usize, cy, cz as usize)).is_some()
        })
    }

    fn block(&self, p: TilePos) -> BlockId {
        crate::edit::locate(p)
            .and_then(|(coords, local)| self.current_block(coords, local))
            .map_or(BlockId::AIR, |b| b.id)
    }

    fn is_bedrock(&self, p: TilePos) -> bool {
        let Some((coords, local)) = crate::edit::locate(p) else { return false };
        self.current_block(coords, local).is_some_and(|b| {
            crate::edit::is_bedrock_at(self.data.ground.get(&crate::ColumnCoords::new(coords.x, coords.z)), b)
        })
    }

    fn edge_locked(&self, p: TilePos) -> bool {
        let Some((coords, _)) = crate::edit::locate(p) else { return false };
        crate::edit::edge_ring_locked(p, self.data.ground.get(&crate::ColumnCoords::new(coords.x, coords.z)))
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.data.constructed.contains_key(&p)
    }

    fn tree_id(&self, p: TilePos) -> u32 {
        crate::edit::locate(p)
            .and_then(|(coords, local)| self.current_block(coords, local))
            .map_or(0, |b| b.tree_id())
    }

    fn tree_limb(&self, p: TilePos) -> u8 {
        crate::edit::locate(p)
            .and_then(|(coords, local)| self.current_block(coords, local))
            .map_or(0, |b| b.limb())
    }
}

impl Rollback {
    /// Applies one client's order, undo-safely. `false` = rejected, nothing
    /// changed: an op not issued by `client`, a priority outside
    /// `1..=MAX_PRIORITY`, a build of anything but
    /// [`crate::edit::PLACE_BLOCK`], or an area that is inverted, leaves the
    /// order level range, misses the region or clips to more than
    /// [`MAX_ORDER_TILES`]. A designation goes only on
    /// tiles that [`validate`], replacing what was there.
    pub fn apply_order_safe(&mut self, client: ClientId, order: Order) -> bool {
        match order {
            Order::Designate { op, kind, priority, area } => {
                if op.client != client || !(1..=MAX_PRIORITY).contains(&priority) {
                    return false;
                }
                if matches!(kind, DesignationKind::Build(id) if id != crate::edit::PLACE_BLOCK) {
                    return false;
                }
                let Some(area) = order_area(area) else { return false };
                let d = Designation { kind, op, priority };
                let changes: Vec<_> = area
                    .tiles()
                    .filter(|&p| validate(self, kind, p).is_ok())
                    .filter(|p| self.data.designations.get(p) != Some(&d))
                    .map(|p| (p, Some(d)))
                    .collect();
                self.set_designations_safe(changes);
            }
            Order::Erase { area, only } => {
                let Some(area) = order_area(area) else { return false };
                let changes: Vec<_> = self
                    .data
                    .designations
                    .iter()
                    .filter(|(p, d)| area.contains(**p) && only.is_none_or(|k| k.same_tool(d.kind)))
                    .map(|(p, _)| (*p, None))
                    .collect();
                self.set_designations_safe(changes);
            }
            Order::CancelOp(op) => {
                if op.client != client {
                    return false;
                }
                let changes: Vec<_> = self
                    .data
                    .designations
                    .iter()
                    .filter(|(_, d)| d.op == op)
                    .map(|(p, _)| (*p, None))
                    .collect();
                self.set_designations_safe(changes);
            }
            Order::DevCompleteAll => self.dev_complete_all_safe(),
            Order::SetMode { mode, at } => return self.set_play_mode_safe(client, mode, at),
            // Needs the region's coordinates: `Region::handle_event` routes
            // it to `move_to_safe`.
            Order::MoveTo(_) => return false,
        }
        true
    }

    /// Dev shortcut: carries out every designation at once — builds bottom
    /// up, then digs, deconstructions and chops top down — skipping tiles
    /// that no longer validate, and clears them all. A chop clears its block
    /// and detaches each falling part of the tree into a voxel body that
    /// despawns at rest.
    fn dev_complete_all_safe(&mut self) {
        let all: Vec<(TilePos, Designation)> =
            self.data.designations.iter().map(|(p, d)| (*p, *d)).collect();
        let is_build = |d: &Designation| matches!(d.kind, DesignationKind::Build(_));
        let mut builds: Vec<_> = all.iter().copied().filter(|(_, d)| is_build(d)).collect();
        builds.sort_by_key(|(p, _)| p[1]);
        let mut removals: Vec<_> = all.iter().copied().filter(|(_, d)| !is_build(d)).collect();
        removals.sort_by_key(|(p, _)| std::cmp::Reverse(p[1]));
        // Writes go through `fill_blocks_safe` in batches (one undo entry per
        // chunk, not per tile): under `HashMode::Full` every entry re-hashes
        // its whole field, and a per-tile loop over a few hundred tiles took
        // seconds on a play-size region — on the server, in the client's
        // prediction and again on every rollback and replay of it.
        // Validity is checked tile by tile in the old order: a tile's
        // validity never depends on another dig or build, only on a chop
        // (which can fell blocks below it), so pending digs are flushed
        // before each chop.
        let mut constructed = Vec::new();
        let mut placing: BTreeMap<BlockId, Vec<TilePos>> = BTreeMap::new();
        for (p, d) in builds {
            if let DesignationKind::Build(id) = d.kind {
                if validate(self, d.kind, p).is_ok() {
                    placing.entry(id).or_default().push(p);
                }
            }
        }
        for (id, tiles) in placing {
            constructed.extend(self.fill_blocks_safe(&tiles, Block::new(id)).into_iter().map(|p| (p, true)));
        }
        let mut clearing = Vec::new();
        for (p, d) in removals {
            if d.kind == DesignationKind::Chop {
                self.clear_tiles_safe(&mut clearing, &mut constructed);
            }
            if validate(self, d.kind, p).is_err() {
                continue;
            }
            match d.kind {
                DesignationKind::Build(_) => unreachable!("builds were done first"),
                DesignationKind::Dig | DesignationKind::Deconstruct => clearing.push(p),
                DesignationKind::Chop => self.chop_safe(p),
            }
        }
        self.clear_tiles_safe(&mut clearing, &mut constructed);
        self.set_constructed_safe(constructed);
        self.set_designations_safe(all.into_iter().map(|(p, _)| (p, None)).collect());
    }

    /// Clears the pending dig / deconstruct tiles in one batch and drops
    /// their constructed flags. A refused write (e.g. the tile became
    /// bedrock) leaves the construction standing, flag included.
    fn clear_tiles_safe(&mut self, tiles: &mut Vec<TilePos>, constructed: &mut Vec<(TilePos, bool)>) {
        if tiles.is_empty() {
            return;
        }
        let cleared = self.fill_blocks_safe(tiles, Block::new(BlockId::AIR));
        constructed.extend(cleared.into_iter().filter(|p| self.data.constructed.contains_key(p)).map(|p| (p, false)));
        tiles.clear();
    }

    /// Fells the tree at `p` (`trees::fell`, computed first): clears the cut
    /// blocks and detaches every falling piece with its tip spin, turning
    /// about the cut (`trees::tip_pivot`). A tree
    /// `fell` refuses (over `MAX_TREE_BLOCKS`) is left whole.
    fn chop_safe(&mut self, p: TilePos) {
        let tree = TileSource::tree_id(self, p);
        let Some(felling) = crate::trees::fell(self, p) else { return };
        if self.clear_blocks_safe(&felling.cut) == 0 {
            return;
        }
        let tick = *self.data.tick as u64;
        for set in felling.falling {
            let spin = crate::trees::tip_angvel(tree, p, &set, tick);
            let pivot = crate::trees::tip_pivot(p);
            self.detach_blocks_pivoting_safe(&set, crate::SettlePolicy::Despawn, spin, Some(pivot));
        }
    }

    /// Writes designation changes and tells the renderer, both ways.
    fn set_designations_safe(&mut self, changes: Vec<(TilePos, Option<Designation>)>) {
        if changes.is_empty() {
            return;
        }
        let prior = changes.iter().map(|(p, _)| (*p, self.data.designations.get(p).copied())).collect();
        // LIFO: registered first, so the prior values are re-sent after the
        // map is restored.
        self.data.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetDesignations(prior),
        ));
        for (p, d) in &changes {
            match d {
                Some(d) => self.data.designations.insert(*p, *d),
                None => self.data.designations.remove(p),
            };
        }
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetDesignations(changes),
        ));
    }

    /// As [`Self::set_designations_safe`], for the constructed-tile set.
    fn set_constructed_safe(&mut self, changes: Vec<(TilePos, bool)>) {
        if changes.is_empty() {
            return;
        }
        let prior = changes.iter().map(|(p, _)| (*p, self.data.constructed.contains_key(p))).collect();
        self.data.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetConstructed(prior),
        ));
        for (p, built) in &changes {
            if *built {
                self.data.constructed.insert(*p, ());
            } else {
                self.data.constructed.remove(p);
            }
        }
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetConstructed(changes),
        ));
    }
}

/// DF's "tile under the cursor at the working level": the ray (region-local
/// sim units, `dir` normalised) is cut at the top of `level`
/// (`y = (level + 1) * BLOCK_VOXELS`) and walked down. A hit on `level` is
/// that cell, a hit on `level - 1` is the floor shown at `level`, anything
/// deeper or nothing is open space, clamped to the level where the ray
/// crosses `y = level * BLOCK_VOXELS`. `None` for a ray that points up or
/// runs parallel to the cut.
pub fn pick_level_cell(
    origin: Vector,
    dir: Vector,
    level: i32,
    solid: impl Fn(TilePos) -> bool,
) -> Option<TilePos> {
    let size = BLOCK_VOXELS as f32;
    let cut_y = (level + 1) as f32 * size;
    if dir.y >= 0.0 {
        return None;
    }
    let t = (cut_y - origin.y) / dir.y;
    if t < 0.0 {
        // Camera already below the cut: start from the camera.
        return pick_from(origin, dir, level, solid);
    }
    let start = Vector::new(origin.x + dir.x * t, cut_y, origin.z + dir.z * t);
    pick_from(start, dir, level, solid)
}

/// The tile under the cursor with nothing cut away: the first block `solid`
/// accepts along the ray (region-local sim units, `dir` normalised) within
/// `reach` blocks, on whatever level it lies. With `face` the cell in front
/// of the face the ray entered through instead — the air a build fills
/// (the hit block itself when the ray starts inside one). `None` when
/// nothing is hit.
pub fn pick_visible_cell(
    origin: Vector,
    dir: Vector,
    reach: f32,
    face: bool,
    solid: impl Fn(TilePos) -> bool,
) -> Option<TilePos> {
    let hit = raycast_blocks(origin, dir, reach, solid)?;
    let [x, y, z] = hit.block;
    let n = if face { hit.normal } else { [0; 3] };
    Some([x + n[0], y + n[1], z + n[2]])
}

/// Whether air at block y `y` is part of a DUG space: strictly below its
/// column's original worldgen surface block `surface` (`ColumnGround`'s, which
/// digging never changes). Strictly: a half block's empty upper half is air
/// inside the surface block, not a dug space. The cut-off x-ray view opens
/// exactly these spaces (client `meshing/interior.rs` marks the faces that
/// border them; [`xray_hit`] is the picking side of the same rule).
pub fn is_dug_air(y: i32, surface: i32) -> bool {
    y < surface
}

/// What a ray SEES with nothing cut away and the dug-space x-ray on: terrain
/// in front of a dug space is not drawn, so once the ray (region-local sim
/// units, `dir` normalised) enters a cell `dug` accepts within `reach` blocks
/// (only asked of cells `solid` refuses), the hit is the first solid block
/// AFTER it — the floor or far wall seen through the opening. With no dug
/// air on the ray, the first solid block, as [`pick_visible_cell`].
pub fn xray_hit(
    origin: Vector,
    dir: Vector,
    reach: f32,
    solid: impl Fn(TilePos) -> bool,
    dug: impl Fn(TilePos) -> bool,
) -> Option<BlockHit> {
    match xray_through(origin, dir, reach, &solid, dug) {
        Some(hit) => Some(hit),
        None => raycast_blocks(origin, dir, reach, solid),
    }
}

/// The half of [`xray_hit`] that differs from a plain pick: the first solid
/// block past the first dug cell on the ray, `None` when the ray meets no
/// dug cell (or nothing solid after it) within `reach`.
pub fn xray_through(
    origin: Vector,
    dir: Vector,
    reach: f32,
    solid: impl Fn(TilePos) -> bool,
    dug: impl Fn(TilePos) -> bool,
) -> Option<BlockHit> {
    let opening = raycast_blocks(origin, dir, reach, |p| !solid(p) && dug(p))?;
    // The same walk again, accepting solids only once past the opening.
    let past = std::cell::Cell::new(false);
    raycast_blocks(origin, dir, reach, |p| {
        if p == opening.block {
            past.set(true);
        }
        past.get() && solid(p)
    })
}

/// [`pick_visible_cell`] under the dug-space x-ray ([`xray_hit`]).
pub fn pick_xray_cell(
    origin: Vector,
    dir: Vector,
    reach: f32,
    face: bool,
    solid: impl Fn(TilePos) -> bool,
    dug: impl Fn(TilePos) -> bool,
) -> Option<TilePos> {
    let hit = xray_hit(origin, dir, reach, solid, dug)?;
    let [x, y, z] = hit.block;
    let n = if face { hit.normal } else { [0; 3] };
    Some([x + n[0], y + n[1], z + n[2]])
}

fn pick_from(
    start: Vector,
    dir: Vector,
    level: i32,
    solid: impl Fn(TilePos) -> bool,
) -> Option<TilePos> {
    let size = BLOCK_VOXELS as f32;
    // Blocks above the cut are not visible at this level (the walk starts in
    // the cell above the cut plane, so they must not stop it).
    // Reach is in blocks (the walk's parameter is in block units).
    let reach = (level + 1 + UNDERGROUND_DEPTH) as f32;
    if let Some(hit) = raycast_blocks(start, dir, reach, |p| p[1] <= level && solid(p)) {
        if hit.block[1] == level {
            return Some(hit.block);
        }
        if hit.block[1] == level - 1 {
            return Some([hit.block[0], level, hit.block[2]]);
        }
    }
    // Open space: where the ray crosses the bottom of the level.
    let t = (level as f32 * size - start.y) / dir.y;
    let (x, z) = (start.x + dir.x * t, start.z + dir.z * t);
    Some([(x / size).floor() as i32, level, (z / size).floor() as i32])
}
