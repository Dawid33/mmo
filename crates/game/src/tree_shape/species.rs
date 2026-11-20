//! The tree species and the habitat niches that decide where each grows.
//!
//! Oak, pine and birch grow in any biome their niches allow (the procedural
//! world has no biomes); the rest grow only in a forged world, in the biomes
//! they list (DF-scale terrain spec §5b).

use super::skeleton::{CrownShape, ShapeParams};
use crate::Biome;

/// A trapezoidal tolerance range: 0 outside `min..=max`, 1 inside
/// `opt_lo..=opt_hi`, linear in between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Niche {
    pub min: f32,
    pub opt_lo: f32,
    pub opt_hi: f32,
    pub max: f32,
}

impl Niche {
    pub const fn new(min: f32, opt_lo: f32, opt_hi: f32, max: f32) -> Niche {
        Niche {
            min,
            opt_lo,
            opt_hi,
            max,
        }
    }

    /// Suitability of `v`, 0..=1.
    pub fn response(&self, v: f32) -> f32 {
        if v < self.min || v > self.max {
            0.0
        } else if v < self.opt_lo {
            (v - self.min) / (self.opt_lo - self.min)
        } else if v <= self.opt_hi {
            1.0
        } else {
            (self.max - v) / (self.max - self.opt_hi)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Species {
    pub name: &'static str,
    pub temperature_c: Niche,
    pub moisture: Niche,
    pub slope: Niche,
    pub soil_depth: Niche,
    pub salinity: Niche,
    /// Chance that a lattice cell holds this species under ideal habitat.
    pub max_density: f32,
    /// Widest reach of the crown from the trunk, blocks.
    pub crown_radius_blocks: i32,
    /// Tallest a tree of this species grows, blocks.
    pub max_height_blocks: i32,
    /// Skeleton shape (`skeleton.rs`); fitted inside the two limits above.
    pub shape: ShapeParams,
    /// The biomes it grows in. Empty = any biome, and the procedural world
    /// (no biome) too; otherwise only a forged column whose biome is listed.
    pub biomes: &'static [Biome],
    /// Made of one block named after the species (a cactus), not
    /// `<species>_log` / `_branch` / `_leaves`; such a species is leafless.
    pub single_block: bool,
}

impl Species {
    /// Whether this species may grow on a column of `biome` (`None` = the
    /// procedural world, which has no biomes).
    pub fn grows_in(&self, biome: Option<Biome>) -> bool {
        self.biomes.is_empty() || biome.is_some_and(|b| self.biomes.contains(&b))
    }
}

/// Index in this table is the `species: u8` of a `TreeSeed`: append only.
pub const SPECIES: [Species; 8] = [
    Species {
        name: "oak",
        // The warm end only matters in forged worlds (the procedural world
        // never passes BASE_TEMPERATURE_C = 14 C, under `opt_hi`): oak thins
        // out in the subtropics and is gone in the tropics.
        temperature_c: Niche::new(4.0, 10.0, 20.0, 24.0),
        moisture: Niche::new(0.6, 1.0, 2.5, 3.5),
        slope: Niche::new(0.0, 0.0, 0.4, 0.8),
        soil_depth: Niche::new(1.0, 2.0, 3.0, 3.0),
        salinity: Niche::new(0.0, 0.0, 0.2, 0.5),
        max_density: 0.7,
        crown_radius_blocks: 8,
        max_height_blocks: 20,
        shape: ShapeParams {
            trunk_height_blocks: (10, 12),
            trunk_radius: 13,
            taper_pct: 45,
            trunk_lean: 6,
            limbs: (4, 6),
            whorl_spacing: 0,
            limb_start_pct: 50,
            limb_angle: (12, 50),
            limb_bend: -2,
            limb_length_pct: 80,
            limb_radius_pct: 65,
            crown: CrownShape::Even,
            branch_levels: 2,
            child_angle: 40,
            child_length_pct: 50,
            cluster_radius: 20,
            trunk_leaves: false,
            clump: (3, 5),
            clump_spread_pct: 70,
            clump_droop: 0,
            leaves_along_pct: 45,
            asymmetry_pct: 30,
            leafless: false,
            tip_radius_pct: 0,
            base_flare_pct: 0,
        },
        biomes: &[],
        single_block: false,
    },
    Species {
        name: "pine",
        temperature_c: Niche::new(-6.0, 0.0, 12.0, 20.0),
        moisture: Niche::new(0.3, 0.6, 1.6, 2.5),
        slope: Niche::new(0.0, 0.0, 0.8, 1.2),
        soil_depth: Niche::new(0.0, 1.0, 3.0, 3.0),
        salinity: Niche::new(0.0, 0.0, 0.4, 0.8),
        max_density: 0.8,
        crown_radius_blocks: 4,
        max_height_blocks: 24,
        shape: ShapeParams {
            trunk_height_blocks: (20, 23),
            trunk_radius: 9,
            taper_pct: 2,
            trunk_lean: 1,
            limbs: (4, 6),
            whorl_spacing: 36,
            limb_start_pct: 15,
            limb_angle: (-6, -2),
            limb_bend: -3,
            limb_length_pct: 18,
            limb_radius_pct: 35,
            crown: CrownShape::Conical,
            branch_levels: 2,
            child_angle: 40,
            child_length_pct: 45,
            cluster_radius: 10,
            trunk_leaves: true,
            clump: (1, 2),
            clump_spread_pct: 40,
            clump_droop: 3,
            leaves_along_pct: 100,
            asymmetry_pct: 10,
            leafless: false,
            tip_radius_pct: 0,
            base_flare_pct: 0,
        },
        biomes: &[],
        single_block: false,
    },
    Species {
        name: "birch",
        temperature_c: Niche::new(-2.0, 4.0, 14.0, 22.0),
        moisture: Niche::new(0.5, 0.9, 2.0, 3.0),
        slope: Niche::new(0.0, 0.0, 0.5, 0.9),
        soil_depth: Niche::new(1.0, 1.0, 3.0, 3.0),
        salinity: Niche::new(0.0, 0.0, 0.2, 0.4),
        max_density: 0.5,
        crown_radius_blocks: 4,
        max_height_blocks: 16,
        shape: ShapeParams {
            trunk_height_blocks: (12, 15),
            trunk_radius: 5,
            taper_pct: 25,
            trunk_lean: 4,
            limbs: (8, 12),
            whorl_spacing: 0,
            limb_start_pct: 30,
            limb_angle: (44, 56),
            limb_bend: -6,
            limb_length_pct: 40,
            limb_radius_pct: 50,
            crown: CrownShape::Flame,
            branch_levels: 2,
            child_angle: 30,
            child_length_pct: 50,
            cluster_radius: 12,
            trunk_leaves: false,
            clump: (2, 3),
            clump_spread_pct: 60,
            clump_droop: 10,
            leaves_along_pct: 60,
            asymmetry_pct: 25,
            leafless: false,
            tip_radius_pct: 0,
            base_flare_pct: 0,
        },
        biomes: &[],
        single_block: false,
    },
    SPRUCE,
    ACACIA,
    PALM,
    JUNGLE,
    CACTUS,
];

/// Spruce: the taiga's dense, narrow cone, branches to the ground.
const SPRUCE: Species = Species {
    name: "spruce",
    temperature_c: Niche::new(-12.0, -6.0, 4.0, 12.0),
    moisture: Niche::new(0.4, 0.8, 2.5, 4.0),
    slope: Niche::new(0.0, 0.0, 0.8, 1.2),
    soil_depth: Niche::new(0.0, 0.5, 3.0, 3.0),
    salinity: Niche::new(0.0, 0.0, 0.3, 0.6),
    max_density: 0.9,
    crown_radius_blocks: 5,
    max_height_blocks: 20,
    shape: ShapeParams {
        trunk_height_blocks: (16, 18),
        trunk_radius: 9,
        taper_pct: 30,
        trunk_lean: 1,
        limbs: (5, 7),
        whorl_spacing: 26,
        limb_start_pct: 18,
        limb_angle: (2, 6),
        limb_bend: -1,
        limb_length_pct: 40,
        limb_radius_pct: 35,
        crown: CrownShape::Conical,
        branch_levels: 2,
        child_angle: 40,
        child_length_pct: 45,
        cluster_radius: 6,
        trunk_leaves: true,
        clump: (2, 4),
        clump_spread_pct: 40,
        clump_droop: 3,
        leaves_along_pct: 100,
        asymmetry_pct: 5,
        leafless: false,
        tip_radius_pct: 0,
        base_flare_pct: 0,
    },
    biomes: &[Biome::Taiga, Biome::ConiferForest, Biome::Mountain],
    single_block: false,
};

/// Acacia: a flat umbrella crown on a bare, forked trunk (savanna).
const ACACIA: Species = Species {
    name: "acacia",
    temperature_c: Niche::new(14.0, 20.0, 32.0, 40.0),
    moisture: Niche::new(0.15, 0.4, 1.3, 2.0),
    slope: Niche::new(0.0, 0.0, 0.5, 0.9),
    soil_depth: Niche::new(0.5, 1.0, 3.0, 3.0),
    salinity: Niche::new(0.0, 0.0, 0.3, 0.6),
    max_density: 0.8,
    crown_radius_blocks: 7,
    max_height_blocks: 10,
    shape: ShapeParams {
        trunk_height_blocks: (6, 8),
        trunk_radius: 9,
        taper_pct: 60,
        trunk_lean: 10,
        limbs: (3, 5),
        whorl_spacing: 0,
        limb_start_pct: 45,
        limb_angle: (36, 44),
        limb_bend: -5,
        limb_length_pct: 110,
        limb_radius_pct: 70,
        crown: CrownShape::Even,
        branch_levels: 2,
        child_angle: 50,
        child_length_pct: 55,
        cluster_radius: 12,
        trunk_leaves: false,
        clump: (3, 5),
        clump_spread_pct: 90,
        clump_droop: 0,
        leaves_along_pct: 15,
        asymmetry_pct: 20,
        leafless: false,
        tip_radius_pct: 0,
        base_flare_pct: 0,
    },
    biomes: &[Biome::Savanna, Biome::Shrubland, Biome::Grassland],
    single_block: false,
};

/// Palm: a tall, curved bare trunk under a fan of drooping fronds. Salt
/// tolerant, and only near the sea (its salinity niche is 0 inland): the
/// tropical coast.
const PALM: Species = Species {
    name: "palm",
    temperature_c: Niche::new(18.0, 22.0, 35.0, 42.0),
    moisture: Niche::new(0.2, 0.5, 4.0, 6.0),
    slope: Niche::new(0.0, 0.0, 0.5, 0.9),
    soil_depth: Niche::new(0.0, 0.0, 3.0, 3.0),
    salinity: Niche::new(0.0, 0.25, 1.0, 1.0),
    max_density: 0.6,
    crown_radius_blocks: 5,
    max_height_blocks: 16,
    shape: ShapeParams {
        trunk_height_blocks: (11, 14),
        trunk_radius: 6,
        taper_pct: 70,
        trunk_lean: 14,
        limbs: (6, 9),
        whorl_spacing: 0,
        limb_start_pct: 96,
        limb_angle: (24, 24),
        limb_bend: -10,
        limb_length_pct: 40,
        limb_radius_pct: 50,
        crown: CrownShape::Even,
        branch_levels: 1,
        child_angle: 0,
        child_length_pct: 0,
        cluster_radius: 14,
        trunk_leaves: false,
        clump: (2, 3),
        clump_spread_pct: 40,
        clump_droop: 1,
        leaves_along_pct: 100,
        asymmetry_pct: 10,
        leafless: false,
        tip_radius_pct: 0,
        base_flare_pct: 0,
    },
    biomes: &[
        Biome::TropicalBroadleafForest,
        Biome::TropicalConiferForest,
        Biome::Savanna,
        Biome::Grassland,
        Biome::Shrubland,
        Biome::Swamp,
        Biome::MangroveSwamp,
    ],
    single_block: false,
};

/// Jungle broadleaf: a very tall emergent with a flared, buttressed base and
/// a broad canopy at the top.
const JUNGLE: Species = Species {
    name: "jungle",
    temperature_c: Niche::new(16.0, 21.0, 32.0, 40.0),
    moisture: Niche::new(0.9, 1.3, 4.0, 6.0),
    slope: Niche::new(0.0, 0.0, 0.6, 1.0),
    soil_depth: Niche::new(0.5, 1.5, 3.0, 3.0),
    salinity: Niche::new(0.0, 0.0, 0.3, 0.6),
    max_density: 0.8,
    crown_radius_blocks: 8,
    max_height_blocks: 32,
    shape: ShapeParams {
        trunk_height_blocks: (26, 29),
        trunk_radius: 20,
        taper_pct: 40,
        trunk_lean: 3,
        limbs: (5, 7),
        whorl_spacing: 0,
        limb_start_pct: 68,
        limb_angle: (10, 20),
        limb_bend: -4,
        limb_length_pct: 38,
        limb_radius_pct: 55,
        crown: CrownShape::Even,
        branch_levels: 2,
        child_angle: 45,
        child_length_pct: 55,
        cluster_radius: 22,
        trunk_leaves: false,
        clump: (3, 5),
        clump_spread_pct: 80,
        clump_droop: 2,
        leaves_along_pct: 50,
        asymmetry_pct: 25,
        leafless: false,
        tip_radius_pct: 0,
        base_flare_pct: 90,
    },
    biomes: &[
        Biome::TropicalBroadleafForest,
        Biome::TropicalConiferForest,
        Biome::Swamp,
        Biome::MangroveSwamp,
    ],
    single_block: false,
};

/// Cactus: a saguaro-like column with upturned arms, one `cactus` block, no
/// leaves (hot deserts).
const CACTUS: Species = Species {
    name: "cactus",
    temperature_c: Niche::new(8.0, 16.0, 40.0, 48.0),
    moisture: Niche::new(0.0, 0.0, 0.5, 0.9),
    slope: Niche::new(0.0, 0.0, 0.6, 1.0),
    soil_depth: Niche::new(0.0, 0.0, 3.0, 3.0),
    salinity: Niche::new(0.0, 0.0, 0.5, 0.8),
    max_density: 1.0,
    crown_radius_blocks: 3,
    max_height_blocks: 9,
    shape: ShapeParams {
        trunk_height_blocks: (7, 8),
        trunk_radius: 8,
        taper_pct: 80,
        trunk_lean: 2,
        limbs: (1, 4),
        whorl_spacing: 0,
        limb_start_pct: 35,
        limb_angle: (2, 8),
        limb_bend: 22,
        limb_length_pct: 60,
        limb_radius_pct: 80,
        crown: CrownShape::Even,
        branch_levels: 1,
        child_angle: 0,
        child_length_pct: 0,
        cluster_radius: 0,
        trunk_leaves: false,
        clump: (1, 1),
        clump_spread_pct: 0,
        clump_droop: 0,
        leaves_along_pct: 0,
        asymmetry_pct: 0,
        leafless: true,
        tip_radius_pct: 85,
        base_flare_pct: 0,
    },
    biomes: &[Biome::SandDesert, Biome::RockyWasteland, Biome::Badlands],
    single_block: true,
};
