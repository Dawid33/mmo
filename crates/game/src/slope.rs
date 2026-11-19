//! Terrain slopes: one block whose geometry is a [`ChiselData`] in the block's
//! own material, shaped like a Minecraft stair (spec
//! `docs/superpowers/specs/2026-11-06-terrain-slopes-design.md`). Every shape
//! is octant-aligned: the bottom half plus one, two or three top quadrants.
use crate::block::{ChiselData, HALF_BLOCK_VOXELS};
use crate::block_props::block_props;
use crate::registry::BlockId;

/// A horizontal side of a block, by the axis direction it faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub enum Side {
    PosX,
    NegX,
    PosZ,
    NegZ,
}

impl Side {
    /// `[dx, dz]` toward this side.
    pub fn offset(self) -> [i32; 2] {
        match self {
            Side::PosX => [1, 0],
            Side::NegX => [-1, 0],
            Side::PosZ => [0, 1],
            Side::NegZ => [0, -1],
        }
    }
}

/// The geometry of a slope block. `low_x` is `PosX` or `NegX`, `low_z` is
/// `PosZ` or `NegZ`; an outer corner is low on both cardinals, an inner corner
/// only toward that diagonal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub enum SlopeShape {
    Straight { low: Side },
    Outer { low_x: Side, low_z: Side },
    Inner { low_x: Side, low_z: Side },
}

const H: usize = HALF_BLOCK_VOXELS;

/// Is voxel coordinate `v` on the high half of an axis whose low side is `low_pos`
/// (low toward +) or its opposite?
fn high_half(v: usize, low_pos: bool) -> bool {
    if low_pos { v < H } else { v >= H }
}

fn top_solid(shape: SlopeShape, x: usize, z: usize) -> bool {
    let hx = |s: Side| {
        debug_assert!(matches!(s, Side::PosX | Side::NegX));
        high_half(x, s == Side::PosX)
    };
    let hz = |s: Side| {
        debug_assert!(matches!(s, Side::PosZ | Side::NegZ));
        high_half(z, s == Side::PosZ)
    };
    match shape {
        SlopeShape::Straight { low } => match low {
            Side::PosX | Side::NegX => hx(low),
            Side::PosZ | Side::NegZ => hz(low),
        },
        SlopeShape::Outer { low_x, low_z } => hx(low_x) && hz(low_z),
        SlopeShape::Inner { low_x, low_z } => hx(low_x) || hz(low_z),
    }
}

/// The slope as a canonical chisel of `material`.
///
/// Changing this geometry changes generated terrain, but the worldgen disk
/// cache key hashes only `crates/worldgen/src`: run `just worldgen-cache-clear`
/// (wipes `target/worldgen-cache`) after such a change, or stale regions load.
pub fn slope_chisel(material: BlockId, shape: SlopeShape) -> ChiselData {
    ChiselData::from_fn(vec![material], |x, y, z| (y < H || top_solid(shape, x, z)).then_some(0))
}

/// One cuboid of a slope geometry: (min, exclusive max), material-free.
type Bounds = ([usize; 3], [usize; 3]);

/// The twelve slope geometries as cuboid lists, built once from
/// [`slope_chisel`] so [`classify`] cannot drift from it.
fn shape_table() -> &'static [(SlopeShape, Vec<Bounds>)] {
    static TABLE: std::sync::OnceLock<Vec<(SlopeShape, Vec<Bounds>)>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        all_shapes()
            .map(|shape| (shape, slope_chisel(BlockId(1), shape).cuboids().map(|(min, max, _)| (min, max)).collect()))
            .collect()
    })
}

/// All twelve shapes.
fn all_shapes() -> impl Iterator<Item = SlopeShape> {
    let sides = [Side::PosX, Side::NegX, Side::PosZ, Side::NegZ];
    let xs = [Side::PosX, Side::NegX];
    let zs = [Side::PosZ, Side::NegZ];
    let straight = sides.into_iter().map(|low| SlopeShape::Straight { low });
    let outer = xs.into_iter().flat_map(move |low_x| zs.into_iter().map(move |low_z| SlopeShape::Outer { low_x, low_z }));
    let inner = xs.into_iter().flat_map(move |low_x| zs.into_iter().map(move |low_z| SlopeShape::Inner { low_x, low_z }));
    straight.chain(outer).chain(inner)
}

/// `Some(shape)` only when `chisel` is exactly one of the twelve slope
/// geometries, in a single material; half blocks and other chisels give `None`.
/// Allocation-free: it runs inside A* and on every client chunk ingest.
pub fn classify(chisel: &ChiselData) -> Option<SlopeShape> {
    let material = chisel.cuboids().next()?.2;
    if chisel.cuboids().any(|c| c.2 != material) {
        return None;
    }
    let count = chisel.cuboids().count();
    shape_table().iter().find_map(|(shape, bounds)| {
        (bounds.len() == count && chisel.cuboids().map(|(min, max, _)| (min, max)).eq(bounds.iter().copied()))
            .then_some(*shape)
    })
}

/// Can a mover walk up onto a slope tile from the neighbour at `from` (`[dx, dz]`
/// from the slope tile to the tile the mover comes from)?
pub fn walk_up(shape: SlopeShape, from: [i32; 2]) -> bool {
    match shape {
        SlopeShape::Straight { low } => from == low.offset(),
        SlopeShape::Outer { low_x, low_z } => {
            let (a, b) = (low_x.offset(), low_z.offset());
            from == a || from == b || from == [a[0] + b[0], a[1] + b[1]]
        }
        SlopeShape::Inner { low_x, low_z } => {
            let (a, b) = (low_x.offset(), low_z.offset());
            from == [a[0] + b[0], a[1] + b[1]]
        }
    }
}

/// The manifest name with spaces and a capital ("Red sand"), plus " slope"
/// when the chisel is a slope.
pub fn display_name(id: BlockId, chisel: Option<&ChiselData>) -> String {
    let raw = block_props().name_of(id).map_or_else(|| format!("block {}", id.0), |n| n.replace('_', " "));
    let mut chars = raw.chars();
    let mut name: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
    if chisel.and_then(classify).is_some() {
        name.push_str(" slope");
    }
    name
}

