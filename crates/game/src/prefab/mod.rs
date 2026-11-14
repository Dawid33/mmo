//! Prefabs: pre-built structures stamped into the voxel world. This module
//! holds the shape catalogue and the one rotation convention.

pub mod shapes;
pub mod turn;

pub use shapes::{Facing, Half, Shape, Sides, StairCorner};
pub use turn::{turn_voxel, turn_xz};
