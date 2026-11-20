//! Deterministic tree shape and rasterisation shared by worldgen and simulation.

pub mod raster;
pub mod skeleton;
pub mod species;

pub use raster::{TreeBlock, TreePart, rasterize};
pub use skeleton::{CrownShape, Node, ShapeParams, TreeSkeleton, skeleton};
pub use species::{Niche, SPECIES, Species};

/// Oldest tree age (the age is a growth stage in `0..=MAX_AGE`).
pub const MAX_AGE: u16 = 100;

/// Splitmix-style deterministic hash used by placement and the shape.
pub fn tree_hash64(seed: u64, a: u64, b: u64) -> u64 {
    let mut z =
        seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
