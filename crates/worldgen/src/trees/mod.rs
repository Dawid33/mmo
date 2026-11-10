//! Trees: species with climate niches, deterministic placement, and the
//! skeleton of a placed tree (laid as bricks by `stud_trees.rs`).

pub mod placement;
pub mod skeleton;
pub mod species;

pub use placement::{
    biome_tree_density, trees_in, TreeSeed, BIOME_TREE_DENSITY, NATIVE_ONLY_BIOMES, LEAF_REACH_PAST_CROWN_BLOCKS, MAX_AGE, MAX_CROWN_RADIUS_BLOCKS, MAX_TREE_HEIGHT_BLOCKS, TREE_LATTICE_BLOCKS,
};
pub use species::{Niche, Species, SPECIES};
pub use skeleton::{skeleton, CrownShape, Node, ShapeParams, TreeSkeleton};
