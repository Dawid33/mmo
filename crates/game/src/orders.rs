//! Fortress orders: what a player may designate on a region's tiles and the
//! pure rules for which tiles accept which designation. An order covers
//! exactly the box between the two corners the player picked
//! ([`TileBox::new`]). Engine-free and state-free; the sim state and the
//! client tools build on these names.
//!
//! A tile is one 1 m block; `TilePos` is region-local block coordinates and a
//! level is a block `y`.

use std::collections::BTreeMap;

use crate::edit::{raycast_blocks, BlockPos};
use crate::{
    BlockId, ClientId, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback,
    BLOCK_UNITS, REGION_BLOCKS, UNDERGROUND_DEPTH,
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
    /// Stud world: place a brick in a stud chunk of this region (invalid = no-op).
    PlaceStud { chunk: crate::StudChunkCoords, brick: crate::StudBrick },
    /// Stud world: remove the brick covering `cell` of a stud chunk (none = no-op).
    RemoveStud { chunk: crate::StudChunkCoords, cell: [u8; 3] },
    /// Stud world, dev: carve the box of region stud cells `lo .. hi` (`[stud x, plate y,
    /// stud z]`, `hi` exclusive; plate 0 is the bottom of layer 0). Too big or empty = no-op.
    /// The box may reach past the region's edge: that part is carved by the neighbour
    /// owning it (`Intent::CarveStud`, one tick later).
    CarveStud { lo: [i32; 3], hi: [i32; 3] },
    /// Switch the issuing client's play mode; `at` is read only when
    /// entering Adventure (the tile to put the body on).
    SetMode { mode: crate::PlayMode, at: Option<TilePos> },
    /// Walk the issuing client's adventurer to a world goal, any number of
    /// regions away (`path_plan.rs`).
    MoveTo(crate::Goal),
    /// Dev / test front end of `Intent::BreakBlock` (`intents.rs`): the
    /// issuing client's body sends one at this world block, wherever it is.
    /// (Survival mining sends the same intent from `mining.rs`.)
    DevBreakBlock(crate::WorldBlockPos),
    /// Dev panel: drop a 1 m dynamic body (`EntityKind::Body`) above this
    /// region-local tile (`Rollback::dev_drop_body_safe`).
    DevDropBody { at: TilePos },
    /// Stud world: place a brick in the stud chunk at world chunk `chunk` (`y` = stud
    /// layer), this region's or — seen through the stud halo — a neighbour's, which
    /// gets it as an `Intent::PlaceStud` (`intents.rs`). Invalid = no-op.
    PlaceStudAt { chunk: crate::WorldChunkPos, brick: crate::StudBrick },
    /// Stud world, dev: fill the empty cells of the box of region stud cells `lo .. hi`
    /// (as [`Order::CarveStud`]'s) with bricks of stud material `material`. Too big,
    /// empty or an unknown material = no-op. The part past the region's edge is filled
    /// by the neighbour owning it (`Intent::FillStud`, one tick later).
    FillStud { lo: [i32; 3], hi: [i32; 3], material: u16 },
    /// Stud world: remove the brick covering `cell` of the stud chunk at world chunk
    /// `chunk` (`y` = stud layer), this region's or — seen through the stud halo — a
    /// neighbour's, which gets it as an `Intent::RemoveStud`. None there = no-op.
    /// The world-addressed form of [`Order::RemoveStud`], as `PlaceStudAt` is of
    /// `PlaceStud`. Appended.
    RemoveStudAt { chunk: crate::WorldChunkPos, cell: [u8; 3] },
}

/// Why a tile refuses a designation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Invalid {
    Bedrock,
    NothingToDig,
    Built,
    Occupied,
    NotBuilt,
    /// Chop on anything but a solid block of a tree (a log or branch).
    NotTree,
}

/// What `validate` and the path search need to know about the world's tiles.
pub trait TileSource {
    /// Whether nothing in tile `p` can be dug (`StudTile::is_bedrock`).
    fn is_bedrock(&self, p: TilePos) -> bool;
    fn is_constructed(&self, p: TilePos) -> bool;
    /// The tree whose wood is in tile `p`; 0 when none or unknown.
    fn tree_id(&self, p: TilePos) -> u32;
    /// The lowest and highest tile level worth scanning in tile column `[x, z]`
    /// (materialised chunks and the recipe's surface); `None` when unknown.
    /// Pathfinding scans this band instead of the fixed
    /// `COLUMN_SCAN_Y_MIN..=MAX` constants, which is what lets ground at any
    /// height be stood on.
    fn column_band(&self, _column: [i32; 2]) -> Option<(i32, i32)> {
        None
    }
    /// Water units in the cell at `p` ([`crate::WATER_FULL`] = full); 0 when
    /// dry or unknown.
    fn water_units(&self, _p: TilePos) -> u16 {
        0
    }
    /// How much of tile `p` is filled (`tiles.rs`); `Open` when unknown.
    fn tile_fill(&self, p: TilePos) -> crate::TileFill;
    /// The dominant stud material of tile `p` (`StudTile::material`); `None`
    /// for an open or unknown tile.
    fn tile_material(&self, _p: TilePos) -> Option<u16> {
        None
    }
}

/// Whether `kind` may be designated on tile `p`: a dig needs something in the tile
/// (any fill), not built and not bedrock (its dominant material cannot be dug); a
/// build needs room (anything but a solid tile — it fills the tile's empty cells); a
/// deconstruct a built tile; a chop a tile holding tree wood
/// (`Rollback::stud_tree_at`).
pub fn validate(src: &impl TileSource, kind: DesignationKind, p: TilePos) -> Result<(), Invalid> {
    use crate::TileFill;
    match kind {
        DesignationKind::Dig => {
            if src.tile_fill(p) == TileFill::Open {
                Err(Invalid::NothingToDig)
            } else if src.is_constructed(p) {
                Err(Invalid::Built)
            } else if src.is_bedrock(p) {
                Err(Invalid::Bedrock)
            } else {
                Ok(())
            }
        }
        DesignationKind::Deconstruct => src.is_constructed(p).then_some(()).ok_or(Invalid::NotBuilt),
        DesignationKind::Build(_) => {
            if src.tile_fill(p) == TileFill::Solid { Err(Invalid::Occupied) } else { Ok(()) }
        }
        DesignationKind::Chop => (src.tree_id(p) != 0).then_some(()).ok_or(Invalid::NotTree),
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

/// Region ∪ stud halo: tiles outside the region read the stud halo
/// (`stud_halo.rs`); never constructions there. Walks plan over all of it
/// (`path_plan.rs`).
impl TileSource for Rollback {
    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        self.stud_column_band(column)
    }

    /// Own ∪ halo, through [`Rollback::water_units_at`].
    fn water_units(&self, p: TilePos) -> u16 {
        self.water_units_at(crate::WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(p[0], p[1], p[2])))
    }

    fn tile_fill(&self, p: TilePos) -> crate::TileFill {
        self.stud_tile(p).fill()
    }

    fn tile_material(&self, p: TilePos) -> Option<u16> {
        self.stud_tile(p).material()
    }

    fn is_bedrock(&self, p: TilePos) -> bool {
        self.stud_tile(p).is_bedrock()
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.data.constructed.contains_key(&p)
    }

    /// The tree whose wood is in the tile (`stud_tree_at`).
    fn tree_id(&self, p: TilePos) -> u32 {
        self.stud_tree_at(p).map_or(0, u32::from)
    }
}

impl Rollback {
    /// Applies one client's order, undo-safely. `false` = rejected, nothing
    /// changed: an op not issued by `client`, a priority outside
    /// `1..=MAX_PRIORITY`, a build of air, or an area that is inverted, leaves the
    /// order level range, misses the region or clips to more than
    /// [`MAX_ORDER_TILES`]. A designation goes only on
    /// tiles that [`validate`], replacing what was there.
    pub fn apply_order_safe(&mut self, client: ClientId, order: Order) -> bool {
        match order {
            Order::Designate { op, kind, priority, area } => {
                if op.client != client || !(1..=MAX_PRIORITY).contains(&priority) {
                    return false;
                }
                // A build names its material by block (`stud_material_of_block`):
                // any block but air.
                if matches!(kind, DesignationKind::Build(id) if id == BlockId::AIR) {
                    return false;
                }
                let Some(area) = order_area(area) else { return false };
                let d = Designation { kind, op, priority };
                // Through the memoising view: a large area reads each stud chunk once.
                let changes: Vec<_> = {
                    let src = self.tiles();
                    area.tiles()
                        .filter(|&p| validate(&src, kind, p).is_ok())
                        .filter(|p| self.data.designations.get(p) != Some(&d))
                        .map(|p| (p, Some(d)))
                        .collect()
                };
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
            // Sends an intent: `Region::handle_event` routes it too.
            Order::DevBreakBlock(_) => return false,
            Order::DevDropBody { at } => return self.dev_drop_body_safe(at),
            Order::PlaceStud { chunk, brick } => return self.place_stud_safe(chunk, brick),
            Order::RemoveStud { chunk, cell } => return self.remove_stud_safe(chunk, cell),
            // This region's part only: `Region::handle_event` routes these to
            // `carve_stud_order_safe` / `fill_stud_order_safe` / `place_stud_order_safe` /
            // `remove_stud_order_safe`,
            // which also send the neighbours' part as intents.
            Order::CarveStud { lo, hi } => {
                return self.carve_stud_boxes_dropping_safe(&[(lo, hi)]).pop().is_some_and(|r| !r.is_empty());
            }
            Order::FillStud { lo, hi, material } => return self.fill_stud_box_safe(lo, hi, material) > 0,
            Order::PlaceStudAt { chunk, brick } => {
                let (rc, c) = chunk.to_local();
                return rc == *self.data.region
                    && self.place_stud_safe(crate::StudChunkCoords { x: c.x, y: c.y, z: c.z }, brick);
            }
            Order::RemoveStudAt { chunk, cell } => {
                let (rc, c) = chunk.to_local();
                return rc == *self.data.region
                    && self.remove_stud_safe(crate::StudChunkCoords { x: c.x, y: c.y, z: c.z }, cell);
            }
        }
        true
    }

    /// Dev shortcut: carries out every designation at once. Builds bottom up (each fills
    /// the empty cells of its tile box with bricks of its block's material,
    /// `stud_material_of_block`, and marks the tile constructed), then digs and
    /// deconstructions top down (each carves its tile box; a deconstructed tile loses
    /// its constructed mark), skipping tiles that no longer validate, and clears
    /// every designation. Each chunk is written once per batch. Chops come last, top
    /// down, each felling its tree (`fell_stud_safe`) if its tile still holds tree
    /// wood. Everything removed yields its items: a carve's
    /// as piles at its tile (`carve_stud_boxes_dropping_safe`), a felled part's where
    /// it comes to rest (`SettlePolicy::DropItems`).
    fn dev_complete_all_safe(&mut self) {
        let all: Vec<(TilePos, Designation)> =
            self.data.designations.iter().map(|(p, d)| (*p, *d)).collect();
        let mut builds: Vec<(TilePos, BlockId)> = all
            .iter()
            .filter_map(|(p, d)| match d.kind {
                DesignationKind::Build(id) => Some((*p, id)),
                _ => None,
            })
            .collect();
        builds.sort_by_key(|(p, _)| p[1]);
        let mut removals: Vec<TilePos> = all
            .iter()
            .filter(|(_, d)| matches!(d.kind, DesignationKind::Dig | DesignationKind::Deconstruct))
            .map(|(p, _)| *p)
            .collect();
        removals.sort_by_key(|p| std::cmp::Reverse(p[1]));
        let mut constructed = Vec::new();
        // A tile's validity never depends on another tile's dig or build, so each
        // batch is validated against the state before it.
        let (mut tiles, mut boxes) = (Vec::new(), Vec::new());
        // A build of the chest block puts a chest in the tile (`place_stud_chest_safe`:
        // an open tile only), after the plain builds.
        let chest = crate::chest_block();
        let mut chests = Vec::new();
        {
            let src = self.tiles();
            for (p, id) in builds {
                if validate(&src, DesignationKind::Build(id), p).is_ok() {
                    if chest == Some(id) {
                        chests.push(p);
                    } else if let Some((lo, hi)) = crate::stud_tile_box(p) {
                        tiles.push(p);
                        boxes.push((lo, hi, crate::stud_material_of_block(id)));
                    }
                }
            }
        }
        let filled = self.fill_stud_boxes_safe(&boxes);
        constructed.extend(tiles.iter().zip(filled).filter(|(_, cells)| *cells > 0).map(|(p, _)| (*p, true)));
        for p in chests {
            if self.place_stud_chest_safe(p) {
                constructed.push((p, true));
            }
        }
        let (mut tiles, mut boxes) = (Vec::new(), Vec::new());
        {
            let src = self.tiles();
            for p in removals {
                let kind = self.data.designations.get(&p).map_or(DesignationKind::Dig, |d| d.kind);
                if validate(&src, kind, p).is_ok() {
                    if let Some((lo, hi)) = crate::stud_tile_box(p) {
                        tiles.push(p);
                        boxes.push((lo, hi));
                    }
                }
            }
        }
        let removed = self.carve_stud_boxes_dropping_safe(&boxes);
        constructed.extend(
            tiles
                .iter()
                .zip(removed)
                .filter(|(p, cells)| !cells.is_empty() && self.data.constructed.contains_key(*p))
                .map(|(p, _)| (*p, false)),
        );
        let mut chops: Vec<TilePos> =
            all.iter().filter(|(_, d)| d.kind == DesignationKind::Chop).map(|(p, _)| *p).collect();
        chops.sort_by_key(|p| std::cmp::Reverse(p[1]));
        for p in chops {
            // Validated against the state now: an earlier chop may have felled it.
            let _ = self.fell_stud_safe(p, crate::SettlePolicy::DropItems);
        }
        self.set_constructed_safe(constructed);
        self.set_designations_safe(all.into_iter().map(|(p, _)| (p, None)).collect());
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
/// (`y = (level + 1) * BLOCK_UNITS`) and walked down. A hit on `level` is
/// that cell, a hit on `level - 1` is the floor shown at `level`, anything
/// deeper or nothing is open space, clamped to the level where the ray
/// crosses `y = level * BLOCK_UNITS`. `None` for a ray that points up or
/// runs parallel to the cut.
pub fn pick_level_cell(
    origin: Vector,
    dir: Vector,
    level: i32,
    solid: impl Fn(TilePos) -> bool,
) -> Option<TilePos> {
    let size = BLOCK_UNITS as f32;
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

fn pick_from(
    start: Vector,
    dir: Vector,
    level: i32,
    solid: impl Fn(TilePos) -> bool,
) -> Option<TilePos> {
    let size = BLOCK_UNITS as f32;
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
