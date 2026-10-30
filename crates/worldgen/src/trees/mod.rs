//! Trees: species with climate niches, deterministic placement, and the
//! skeleton + rasteriser that turn a placed tree into blocks.

pub mod placement;
pub mod raster;
pub mod skeleton;
pub mod species;

pub use placement::{
    trees_in, TreeSeed, LEAF_REACH_PAST_CROWN_BLOCKS, MAX_AGE, MAX_CROWN_RADIUS_BLOCKS, MAX_TREE_HEIGHT_BLOCKS, TREE_LATTICE_BLOCKS,
};
pub use species::{Niche, Species, SPECIES};
pub use raster::{rasterize, TreeBlock, TreePart};
pub use skeleton::{skeleton, CrownShape, Node, ShapeParams, TreeSkeleton};
