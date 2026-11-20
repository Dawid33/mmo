//! Trees: species with climate niches, deterministic placement, and the
//! skeleton + rasteriser that turn a placed tree into blocks.

pub mod placement;
pub mod raster;
pub mod skeleton;
pub mod species;

pub use placement::{
    BIOME_TREE_DENSITY, LEAF_REACH_PAST_CROWN_BLOCKS, LatticeWrap, MAX_AGE,
    MAX_CROWN_RADIUS_BLOCKS, MAX_TREE_HEIGHT_BLOCKS, NATIVE_ONLY_BIOMES, TREE_LATTICE_BLOCKS,
    TreeSeed, biome_tree_density, trees_in,
};
pub use raster::{TreeBlock, TreePart, rasterize};
pub use skeleton::{CrownShape, Node, ShapeParams, TreeSkeleton, skeleton};
pub use species::{Niche, SPECIES, Species};
