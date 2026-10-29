//! Fortress orders: what a player may designate on a region's tiles, the
//! pure rules for which tiles accept which designation, and how a mouse drag
//! resolves to a box. Engine-free and state-free; the sim state and the
//! client tools build on these names.
//!
//! A tile is one 1 m block; `TilePos` is region-local block coordinates and a
//! level is a block `y`.

use std::collections::BTreeMap;

use crate::edit::BlockPos;
use crate::{BlockId, ClientId, CHUNK_BLOCKS, REGION_CHUNKS};
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
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize, StateHash,
)]
pub struct OpId {
    pub client: ClientId,
    pub seq: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum DesignationKind {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
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

    pub fn tile_count(&self) -> u64 {
        (0..3).map(|i| (i64::from(self.max[i]) - i64::from(self.min[i]) + 1) as u64).product()
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
