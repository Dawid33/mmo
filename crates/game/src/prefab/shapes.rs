//! The catalogue of named block shapes, as voxel occupancy inside one block.

use serde::{Deserialize, Serialize};

use super::turn::turn_voxel;
use crate::registry::BlockId;
use crate::{ChiselData, BLOCK_VOXELS};

const H: usize = BLOCK_VOXELS / 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Facing {
    North,
    East,
    South,
    West,
}

impl Facing {
    fn quarter_turns(self) -> u8 {
        match self {
            Facing::North => 0,
            Facing::East => 1,
            Facing::South => 2,
            Facing::West => 3,
        }
    }

    fn from_quarter_turns(n: u8) -> Self {
        match n % 4 {
            0 => Facing::North,
            1 => Facing::East,
            2 => Facing::South,
            _ => Facing::West,
        }
    }

    fn turned(self, quarter_turns: u8) -> Self {
        Self::from_quarter_turns(self.quarter_turns() + quarter_turns % 4)
    }

    fn mirrored_x(self) -> Self {
        match self {
            Facing::East => Facing::West,
            Facing::West => Facing::East,
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Half {
    Bottom,
    Top,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StairCorner {
    Straight,
    InnerLeft,
    InnerRight,
    OuterLeft,
    OuterRight,
}

impl StairCorner {
    fn mirrored(self) -> Self {
        match self {
            StairCorner::InnerLeft => StairCorner::InnerRight,
            StairCorner::InnerRight => StairCorner::InnerLeft,
            StairCorner::OuterLeft => StairCorner::OuterRight,
            StairCorner::OuterRight => StairCorner::OuterLeft,
            StairCorner::Straight => StairCorner::Straight,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Sides {
    #[serde(default)]
    pub north: bool,
    #[serde(default)]
    pub east: bool,
    #[serde(default)]
    pub south: bool,
    #[serde(default)]
    pub west: bool,
}

impl Sides {
    fn turned(self, quarter_turns: u8) -> Self {
        let mut s = self;
        for _ in 0..quarter_turns % 4 {
            s = Sides { north: s.west, east: s.north, south: s.east, west: s.south };
        }
        s
    }

    fn mirrored_x(self) -> Self {
        Sides { east: self.west, west: self.east, ..self }
    }

    /// The arm of a set side, from the block's centre to that edge, `across` wide
    /// (a range of the perpendicular axis).
    fn arm(self, x: usize, z: usize, across: std::ops::Range<usize>) -> bool {
        (self.north && across.contains(&x) && z < H)
            || (self.south && across.contains(&x) && z >= H)
            || (self.west && across.contains(&z) && x < H)
            || (self.east && across.contains(&z) && x >= H)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Shape {
    Slab { half: Half },
    Stairs { facing: Facing, half: Half, corner: StairCorner },
    Wall { sides: Sides, post: bool },
    Fence { sides: Sides },
    Pane { sides: Sides },
    Trapdoor { facing: Facing, half: Half, open: bool },
    Carpet,
    /// 1..=8 layers, each two voxels thick.
    Layer { layers: u8 },
}

/// The voxel of a bottom/top-half shape in the bottom frame.
fn flip_y(half: Half, y: usize) -> usize {
    match half {
        Half::Bottom => y,
        Half::Top => BLOCK_VOXELS - 1 - y,
    }
}

impl Shape {
    /// False only for a `Layer` outside 1..=8.
    pub fn is_valid(self) -> bool {
        match self {
            Shape::Layer { layers } => (1..=8).contains(&layers),
            _ => true,
        }
    }

    /// Voxel occupancy; each coordinate in `0..BLOCK_VOXELS`.
    pub fn filled(self, x: usize, y: usize, z: usize) -> bool {
        const N: usize = BLOCK_VOXELS;
        match self {
            Shape::Slab { half } => flip_y(half, y) < H,
            Shape::Stairs { facing, half, corner } => {
                // Work in the north-facing, bottom frame.
                let [x, _, z] = turn_voxel([x, y, z], (4 - facing.quarter_turns()) % 4);
                let y = flip_y(half, y);
                if y < H {
                    return true;
                }
                match corner {
                    StairCorner::Straight => z < H,
                    StairCorner::OuterLeft => x < H && z < H,
                    StairCorner::OuterRight => x >= H && z < H,
                    StairCorner::InnerLeft => z < H || x < H,
                    StairCorner::InnerRight => z < H || x >= H,
                }
            }
            Shape::Fence { sides } => {
                let post = (6..10).contains(&x) && (6..10).contains(&z);
                let rails = (6..9).contains(&y) || (12..15).contains(&y);
                post || (rails && sides.arm(x, z, 7..9))
            }
            Shape::Wall { sides, post } => {
                (post && (4..12).contains(&x) && (4..12).contains(&z))
                    || (y < 14 && sides.arm(x, z, 5..11))
            }
            Shape::Pane { sides } => {
                let none = !(sides.north || sides.east || sides.south || sides.west);
                (none && (7..9).contains(&x) && (7..9).contains(&z)) || sides.arm(x, z, 7..9)
            }
            Shape::Trapdoor { facing, half, open } => {
                if open {
                    let [_, _, z] = turn_voxel([x, y, z], (4 - facing.quarter_turns()) % 4);
                    z >= N - 3
                } else {
                    flip_y(half, y) < 3
                }
            }
            Shape::Carpet => y < 1,
            Shape::Layer { layers } => y < 2 * layers as usize,
        }
    }

    /// The same shape after `quarter_turns` clockwise turns seen from above.
    pub fn turned(self, quarter_turns: u8) -> Shape {
        let t = quarter_turns % 4;
        match self {
            Shape::Stairs { facing, half, corner } => {
                Shape::Stairs { facing: facing.turned(t), half, corner }
            }
            Shape::Wall { sides, post } => Shape::Wall { sides: sides.turned(t), post },
            Shape::Fence { sides } => Shape::Fence { sides: sides.turned(t) },
            Shape::Pane { sides } => Shape::Pane { sides: sides.turned(t) },
            Shape::Trapdoor { facing, half, open } => {
                Shape::Trapdoor { facing: facing.turned(t), half, open }
            }
            other => other,
        }
    }

    /// The shape reflected x -> BLOCK_VOXELS-1-x.
    pub fn mirrored_x(self) -> Shape {
        match self {
            Shape::Stairs { facing, half, corner } => {
                Shape::Stairs { facing: facing.mirrored_x(), half, corner: corner.mirrored() }
            }
            Shape::Wall { sides, post } => Shape::Wall { sides: sides.mirrored_x(), post },
            Shape::Fence { sides } => Shape::Fence { sides: sides.mirrored_x() },
            Shape::Pane { sides } => Shape::Pane { sides: sides.mirrored_x() },
            Shape::Trapdoor { facing, half, open } => {
                Shape::Trapdoor { facing: facing.mirrored_x(), half, open }
            }
            other => other,
        }
    }

    /// This shape as a canonical chisel of one material.
    pub fn chisel(self, material: BlockId) -> ChiselData {
        ChiselData::from_fn(vec![material], |x, y, z| self.filled(x, y, z).then_some(0))
    }
}
