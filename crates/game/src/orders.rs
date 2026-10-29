//! Fortress orders: what a player may designate on a region's tiles, the
//! pure rules for which tiles accept which designation, and how a mouse drag
//! resolves to a box. Engine-free and state-free; the sim state and the
//! client tools build on these names.
//!
//! A tile is one 1 m block; `TilePos` is region-local block coordinates and a
//! level is a block `y`.

use std::collections::BTreeMap;

use crate::edit::BlockPos;
use crate::{
    Block, BlockId, ClientId, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback,
    CHUNK_BLOCKS, REGION_CHUNKS,
};
use state_hash::StateHash;

/// Region-local block coordinates `[x, y, z]`.
pub type TilePos = BlockPos;

/// The one race's height in levels: what a wall drag on a single level covers.
pub const DWARF_HEIGHT_LEVELS: i32 = 2;
/// Priority of an order that does not choose one.
pub const DEFAULT_PRIORITY: u8 = 4;
/// Priorities run `1..=MAX_PRIORITY`.
pub const MAX_PRIORITY: u8 = 7;
/// A `Designate`/`Erase` whose clipped area exceeds this many tiles is
/// rejected whole.
pub const MAX_ORDER_TILES: u64 = 65_536;
/// A region's edge in blocks.
pub const REGION_BLOCKS: i32 = (REGION_CHUNKS * CHUNK_BLOCKS) as i32;

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
}

impl DesignationKind {
    /// Same tool, ignoring the material: every `Build(_)` matches.
    pub fn same_tool(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Dig, Self::Dig) | (Self::Deconstruct, Self::Deconstruct) | (Self::Build(_), Self::Build(_))
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
        let (min_x, max_x) = (self.min[0].max(0), self.max[0].min(REGION_BLOCKS - 1));
        let (min_z, max_z) = (self.min[2].max(0), self.max[2].min(REGION_BLOCKS - 1));
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
}

/// What `validate` needs to know about the world.
pub trait TileSource {
    /// The block at `p`; `AIR` when unknown.
    fn block(&self, p: TilePos) -> BlockId;
    fn is_bedrock(&self, p: TilePos) -> bool;
    /// The outer ring below the surface that cannot be dug (see `edit.rs`).
    fn edge_locked(&self, p: TilePos) -> bool;
    fn is_constructed(&self, p: TilePos) -> bool;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragShape {
    Box,
    /// One level high becomes dwarf height; an explicit `y` range is kept.
    Wall,
    /// A single layer under the level you stand on.
    Floor,
}

/// The box a drag from `anchor` to `corner` covers.
pub fn resolve_drag(shape: DragShape, anchor: TilePos, corner: TilePos) -> TileBox {
    let mut b = TileBox::new(anchor, corner);
    match shape {
        DragShape::Box => {}
        DragShape::Wall => {
            if anchor[1] == corner[1] {
                b.max[1] = b.min[1] + DWARF_HEIGHT_LEVELS - 1;
            }
        }
        DragShape::Floor => {
            let y = anchor[1].min(corner[1]) - 1;
            b.min[1] = y;
            b.max[1] = y;
        }
    }
    b
}

/// The part of an order's `area` it acts on: clipped to the region, `None`
/// (reject) when inverted, empty after clipping, or over [`MAX_ORDER_TILES`].
fn order_area(area: TileBox) -> Option<TileBox> {
    if area.is_inverted() {
        return None;
    }
    area.clip_to_region().filter(|a| a.tile_count() <= MAX_ORDER_TILES)
}

impl TileSource for Rollback {
    fn block(&self, p: TilePos) -> BlockId {
        crate::edit::locate(p)
            .and_then(|(coords, local)| self.current_block(coords, local))
            .map_or(BlockId::AIR, |b| b.id)
    }

    fn is_bedrock(&self, p: TilePos) -> bool {
        let Some((coords, local)) = crate::edit::locate(p) else { return false };
        self.data.cells.cell_chunks == 0
            && self.current_block(coords, local).is_some_and(|b| {
                crate::edit::is_bedrock_at(
                    self.data.ground.get(&crate::ColumnCoords::new(coords.x, coords.z)),
                    b,
                )
            })
    }

    fn edge_locked(&self, p: TilePos) -> bool {
        let Some((coords, _)) = crate::edit::locate(p) else { return false };
        self.data.cells.cell_chunks == 0
            && crate::edit::edge_ring_locked(
                p,
                self.data.ground.get(&crate::ColumnCoords::new(coords.x, coords.z)),
            )
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.data.constructed.contains_key(&p)
    }
}

impl Rollback {
    /// Applies one client's order, undo-safely. `false` = rejected, nothing
    /// changed: an op not issued by `client`, a priority outside
    /// `1..=MAX_PRIORITY`, or an area that is inverted, misses the region or
    /// clips to more than [`MAX_ORDER_TILES`]. A designation goes only on
    /// tiles that [`validate`], replacing what was there.
    pub fn apply_order_safe(&mut self, client: ClientId, order: Order) -> bool {
        match order {
            Order::Designate { op, kind, priority, area } => {
                if op.client != client || !(1..=MAX_PRIORITY).contains(&priority) {
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
        }
        true
    }

    /// Dev shortcut: carries out every designation at once — builds bottom
    /// up, then digs and deconstructions top down — skipping tiles that no
    /// longer validate, and clears them all.
    fn dev_complete_all_safe(&mut self) {
        let all: Vec<(TilePos, Designation)> =
            self.data.designations.iter().map(|(p, d)| (*p, *d)).collect();
        let is_build = |d: &Designation| matches!(d.kind, DesignationKind::Build(_));
        let mut builds: Vec<_> = all.iter().copied().filter(|(_, d)| is_build(d)).collect();
        builds.sort_by_key(|(p, _)| p[1]);
        let mut removals: Vec<_> = all.iter().copied().filter(|(_, d)| !is_build(d)).collect();
        removals.sort_by_key(|(p, _)| std::cmp::Reverse(p[1]));
        let mut constructed = Vec::new();
        for (p, d) in builds.into_iter().chain(removals) {
            if validate(self, d.kind, p).is_err() {
                continue;
            }
            match d.kind {
                DesignationKind::Build(id) => {
                    if self.set_block_safe(p, Block::new(id)) {
                        constructed.push((p, true));
                    }
                }
                DesignationKind::Dig | DesignationKind::Deconstruct => {
                    self.set_block_safe(p, Block::new(BlockId::AIR));
                    if self.data.constructed.contains_key(&p) {
                        constructed.push((p, false));
                    }
                }
            }
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
