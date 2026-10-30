//! Trees: species with climate niches, and deterministic placement.

pub mod placement;
pub mod species;

pub use placement::{
    trees_in, TreeSeed, MAX_AGE, MAX_CROWN_RADIUS_BLOCKS, MAX_TREE_HEIGHT_BLOCKS, TREE_LATTICE_BLOCKS,
};
pub use species::{Niche, Species, SPECIES};
