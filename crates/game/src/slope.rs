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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_VOXELS;
    use crate::{install_block_props, BlockRegistry};

    const STONE: BlockId = BlockId(2);

    fn all_voxels() -> impl Iterator<Item = (usize, usize, usize)> {
        (0..BLOCK_VOXELS).flat_map(|x| (0..BLOCK_VOXELS).flat_map(move |y| (0..BLOCK_VOXELS).map(move |z| (x, y, z))))
    }

    #[test]
    fn straight_fills_bottom_half_and_back_top_half() {
        let c = slope_chisel(STONE, SlopeShape::Straight { low: Side::PosX });
        for (x, y, z) in all_voxels() {
            assert_eq!(c.is_solid(x, y, z), y < H || x < H, "{x} {y} {z}");
        }
    }

    #[test]
    fn outer_keeps_one_top_quadrant_inner_keeps_three() {
        let o = slope_chisel(STONE, SlopeShape::Outer { low_x: Side::PosX, low_z: Side::PosZ });
        let i = slope_chisel(STONE, SlopeShape::Inner { low_x: Side::PosX, low_z: Side::PosZ });
        for (x, y, z) in all_voxels().filter(|v| v.1 >= H) {
            assert_eq!(o.is_solid(x, y, z), x < H && z < H);
            assert_eq!(i.is_solid(x, y, z), !(x >= H && z >= H));
        }
    }

    #[test]
    fn classify_round_trips_all_twelve_shapes() {
        assert_eq!(all_shapes().count(), 12);
        for shape in all_shapes() {
            for m in [STONE, BlockId(28)] {
                assert_eq!(classify(&slope_chisel(m, shape)), Some(shape), "{shape:?}");
            }
        }
    }

    #[test]
    fn classify_rejects_half_blocks_and_full_chisels() {
        assert_eq!(classify(&ChiselData::cuboid(STONE, [0, 0, 0], [BLOCK_VOXELS, H, BLOCK_VOXELS])), None);
        assert_eq!(classify(&ChiselData::cuboid(STONE, [0, 0, 0], [BLOCK_VOXELS; 3])), None);
        assert_eq!(classify(&ChiselData::default()), None);
    }

    #[test]
    fn classify_table_matches_slope_chisel() {
        let table = shape_table();
        assert_eq!(table.len(), 12);
        for (shape, bounds) in table {
            let want: Vec<Bounds> = slope_chisel(STONE, *shape).cuboids().map(|(min, max, _)| (min, max)).collect();
            assert_eq!(bounds, &want, "{shape:?}");
        }
    }

    #[test]
    fn classify_rejects_other_multi_cuboid_chisels() {
        // A voxel-masked log: a round-ish trunk core, several cuboids, one material.
        let log = ChiselData::from_fn(vec![STONE], |x, _, z| {
            let (dx, dz) = (x as i32 * 2 - 15, z as i32 * 2 - 15);
            (dx * dx + dz * dz <= 12 * 12).then_some(0)
        });
        assert!(log.cuboids().count() > 1);
        assert_eq!(classify(&log), None);
        // A slope's geometry in two materials is not a slope.
        let two = ChiselData::from_fn(vec![STONE, BlockId(28)], |x, y, z| {
            (y < H || x < H).then_some(u8::from(z < H))
        });
        assert_eq!(classify(&two), None);
    }

    #[test]
    fn slope_chisels_are_canonical() {
        for shape in all_shapes() {
            let c = slope_chisel(STONE, shape);
            assert_eq!(c, ChiselData::from_grid(vec![STONE], &c.grid()), "{shape:?}");
        }
    }

    #[test]
    fn walk_up_only_from_the_low_side() {
        let s = SlopeShape::Straight { low: Side::NegZ };
        assert!(walk_up(s, [0, -1]));
        for from in [[0, 1], [1, 0], [-1, 0], [1, -1], [-1, -1]] {
            assert!(!walk_up(s, from));
        }
        let o = SlopeShape::Outer { low_x: Side::PosX, low_z: Side::PosZ };
        for from in [[1, 0], [0, 1], [1, 1]] {
            assert!(walk_up(o, from));
        }
        assert!(!walk_up(o, [-1, 0]));
        let i = SlopeShape::Inner { low_x: Side::PosX, low_z: Side::PosZ };
        assert!(walk_up(i, [1, 1]));
        assert!(!walk_up(i, [1, 0]) && !walk_up(i, [0, 1]));
    }

    #[test]
    fn display_names() {
        let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
        install_block_props(reg.props());
        let (dirt, red_sand) = (BlockId(1), BlockId(28));
        let slope = slope_chisel(red_sand, SlopeShape::Straight { low: Side::PosX });
        assert_eq!(display_name(red_sand, Some(&slope)), "Red sand slope");
        assert_eq!(display_name(dirt, None), "Dirt");
        let half = ChiselData::cuboid(dirt, [0, 0, 0], [BLOCK_VOXELS, H, BLOCK_VOXELS]);
        assert_eq!(display_name(dirt, Some(&half)), "Dirt");
    }
}
