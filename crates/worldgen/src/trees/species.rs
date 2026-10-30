//! The three tree species and the habitat niches that decide where each grows.

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
        crown_radius_blocks: 5,
        max_height_blocks: 18,
    },
    Species {
        name: "pine",
        temperature_c: Niche::new(-6.0, 0.0, 12.0, 20.0),
        moisture: Niche::new(0.3, 0.6, 1.6, 2.5),
        slope: Niche::new(0.0, 0.0, 0.8, 1.2),
        soil_depth: Niche::new(0.0, 1.0, 3.0, 3.0),
        salinity: Niche::new(0.0, 0.0, 0.4, 0.8),
        max_density: 0.8,
        crown_radius_blocks: 3,
        max_height_blocks: 24,
    },
    Species {
        name: "birch",
        temperature_c: Niche::new(-2.0, 4.0, 14.0, 22.0),
        moisture: Niche::new(0.5, 0.9, 2.0, 3.0),
        slope: Niche::new(0.0, 0.0, 0.5, 0.9),
        soil_depth: Niche::new(1.0, 1.0, 3.0, 3.0),
        salinity: Niche::new(0.0, 0.0, 0.2, 0.4),
        max_density: 0.5,
        crown_radius_blocks: 3,
        max_height_blocks: 16,
    },
];
