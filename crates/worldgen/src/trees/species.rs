//! The three tree species and the habitat niches that decide where each grows.

use super::skeleton::{CrownShape, ShapeParams};

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
        Niche { min, opt_lo, opt_hi, max }
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
}

/// Index in this table is the `species: u8` of a `TreeSeed`.
pub const SPECIES: [Species; 3] = [
    Species {
        name: "oak",
        temperature_c: Niche::new(4.0, 10.0, 20.0, 28.0),
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
        },
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
        },
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
        },
    },
];
