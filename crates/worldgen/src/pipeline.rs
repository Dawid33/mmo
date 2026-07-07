//! Our tuned density graph: continentalness/erosion/ridges -> offset/factor/
//! jaggedness -> depth -> final_density. Numbers here are hand-tuned, not
//! ported from Tectonic (see the design spec).

use crate::density::{DensityFn, NoiseSet};
use crate::noise::OctaveNoise;
use crate::spline::Spline;

pub const SEA_LEVEL: f64 = 64.0;

// Noise indices into NoiseSet.
const N_CONTINENTS: usize = 0;
const N_EROSION: usize = 1;
const N_RIDGES: usize = 2;

// Tuning constants (hand-picked, not ported).
const OCEAN_OFFSET: f64 = -0.5;
const MIN_OFFSET: f64 = -1.0;
const MAX_OFFSET: f64 = 2.0;
const HEIGHT_SCALE: f64 = 96.0; // meters of surface swing per offset unit
const DEPTH_PER_BLOCK: f64 = 1.0 / 32.0; // density falloff per meter of height

// Vertical density ramp (the `neg_y` term): carries `-y` across the world band,
// clamped at the ends. Used by BOTH the density graph node and ColumnProfile so
// they stay bit-identical.
const RAMP_FROM_Y: f64 = -64.0;
const RAMP_TO_Y: f64 = 320.0;
const RAMP_FROM_V: f64 = 64.0;
const RAMP_TO_V: f64 = -320.0;

fn continentalness() -> DensityFn {
    DensityFn::Noise { idx: N_CONTINENTS, xz_scale: 0.13 }
}
fn erosion() -> DensityFn {
    DensityFn::Noise { idx: N_EROSION, xz_scale: 0.25 }
}
fn ridges() -> DensityFn {
    DensityFn::Noise { idx: N_RIDGES, xz_scale: 0.5 }
}

/// abs(continentalness) pushed through a rising curve, minus the ocean bias.
/// Negative over most of the map (ocean); positive where |continents| is large.
fn raw_continents() -> DensityFn {
    let s = Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(0.4, Spline::constant(0.6), 1.0),
            Spline::point(1.0, Spline::constant(1.2), 0.0),
        ],
    };
    DensityFn::add(
        DensityFn::Const(OCEAN_OFFSET),
        DensityFn::spline_of(DensityFn::abs(continentalness()), s),
    )
}

/// Surface height offset (in offset units) as a spline on raw_continents,
/// its inland high end pulled down by erosion (flat plains vs mountains).
fn offset() -> DensityFn {
    let erosion_mod = Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(1.0), 0.0), // low erosion -> tall
            Spline::point(0.2, Spline::constant(0.5), 0.0),
            Spline::point(1.0, Spline::constant(0.15), 0.0), // high erosion -> flat
        ],
    };
    let base = Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(-0.9), 0.0), // deep ocean
            Spline::point(-0.2, Spline::constant(-0.3), 0.5), // shelf
            Spline::point(0.0, Spline::constant(0.02), 0.6),  // coast ~ sea level
            Spline::point(0.6, Spline::constant(0.9), 1.0),   // inland
            Spline::point(1.2, Spline::constant(1.8), 0.5),   // highland
        ],
    };
    // offset = base(raw) * erosion_mod(erosion), clamped.
    DensityFn::clamp(
        DensityFn::mul(
            DensityFn::spline_of(raw_continents(), base),
            DensityFn::spline_of(erosion(), erosion_mod),
        ),
        MIN_OFFSET,
        MAX_OFFSET,
    )
}

/// Vertical stretch factor (always positive): tall in low-erosion inland.
fn factor() -> DensityFn {
    let s = Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(0.6), 0.0),
            Spline::point(0.0, Spline::constant(1.2), 0.0),
            Spline::point(1.0, Spline::constant(3.0), 0.0),
        ],
    };
    DensityFn::spline_of(offset(), s)
}

/// Jaggedness: a ridges-driven bump, nonzero mainly at high offset.
fn jaggedness() -> DensityFn {
    let gate = Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(1.0, Spline::constant(0.6), 0.0),
        ],
    };
    DensityFn::mul(
        DensityFn::spline_of(offset(), gate),
        DensityFn::abs(ridges()),
    )
}

/// final_density = (surface_height - y) * DEPTH_PER_BLOCK * factor + jaggedness.
/// `surface_height = SEA_LEVEL + offset*HEIGHT_SCALE` (meters); the value is
/// positive below the surface, negative above. `neg_y` is a wide linear ramp
/// carrying `-y` across the whole world band.
fn final_density_graph() -> DensityFn {
    let neg_y = DensityFn::YGradient {
        from_y: RAMP_FROM_Y,
        to_y: RAMP_TO_Y,
        from_v: RAMP_FROM_V,
        to_v: RAMP_TO_V,
    };
    let surface = DensityFn::add(
        DensityFn::mul(offset(), DensityFn::Const(HEIGHT_SCALE)),
        DensityFn::add(DensityFn::Const(SEA_LEVEL), neg_y),
    );
    DensityFn::add(
        DensityFn::mul(
            DensityFn::mul(surface, DensityFn::Const(DEPTH_PER_BLOCK)),
            factor(),
        ),
        jaggedness(),
    )
}

pub struct TerrainPipeline {
    noises: NoiseSet,
    graph: DensityFn,
    offset_g: DensityFn,
    factor_g: DensityFn,
    jaggedness_g: DensityFn,
}

impl TerrainPipeline {
    pub fn new(seed: u64) -> Self {
        // Per-noise sub-seeds derived from the world seed; amplitudes are our
        // own (long-wavelength continents, medium erosion, short ridges).
        let noises = NoiseSet(vec![
            OctaveNoise::new(seed ^ 0x00C0, -10, vec![1.75, 1.0, 2.0, 3.0, 2.0, 2.0, 1.0]),
            OctaveNoise::new(seed ^ 0x00E0, -10, vec![2.0, 1.75, 1.5, 1.5, 1.3, 1.0]),
            OctaveNoise::new(seed ^ 0x00D0, -8, vec![1.0, 2.0, 1.0]),
        ]);
        Self {
            noises,
            graph: final_density_graph(),
            offset_g: offset(),
            factor_g: factor(),
            jaggedness_g: jaggedness(),
        }
    }

    pub fn final_density(&self, x: f64, y: f64, z: f64) -> f64 {
        self.graph.eval(x, y, z, &self.noises)
    }

    /// Precompute the 2D profile at (x, z). The sub-graphs ignore `y`, so any y
    /// (here 0.0) yields the same result.
    pub fn column(&self, x: f64, z: f64) -> ColumnProfile {
        let offset_val = self.offset_g.eval(x, 0.0, z, &self.noises);
        ColumnProfile {
            offset_h: offset_val * HEIGHT_SCALE,
            factor: self.factor_g.eval(x, 0.0, z, &self.noises),
            jaggedness: self.jaggedness_g.eval(x, 0.0, z, &self.noises),
        }
    }
}

/// The 2D (x,z-only) part of the density field for one column, precomputed so a
/// whole vertical column can be evaluated with cheap arithmetic. `density_at(y)`
/// reproduces `TerrainPipeline::final_density(x, y, z)` bit-for-bit.
pub struct ColumnProfile {
    offset_h: f64, // offset value * HEIGHT_SCALE
    factor: f64,
    jaggedness: f64,
}

impl ColumnProfile {
    pub fn density_at(&self, y: f64) -> f64 {
        // Must match final_density_graph's arithmetic EXACTLY, including float
        // grouping/associativity (f64 add/mul are not associative). The graph does:
        //   surface  = (offset*HEIGHT_SCALE) + (SEA_LEVEL + neg_y)
        //   density  = ((surface * DEPTH_PER_BLOCK) * factor) + jaggedness
        let t = ((y - RAMP_FROM_Y) / (RAMP_TO_Y - RAMP_FROM_Y)).clamp(0.0, 1.0);
        let neg_y = RAMP_FROM_V + t * (RAMP_TO_V - RAMP_FROM_V);
        let surface = self.offset_h + (SEA_LEVEL + neg_y);
        (surface * DEPTH_PER_BLOCK) * self.factor + self.jaggedness
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let p = TerrainPipeline::new(12345);
        assert_eq!(p.final_density(10.0, 64.0, 20.0), p.final_density(10.0, 64.0, 20.0));
    }

    #[test]
    fn density_decreases_with_height() {
        // Same column: deep underground is denser (more solid) than the sky.
        let p = TerrainPipeline::new(1);
        let low = p.final_density(0.0, 0.0, 0.0);
        let high = p.final_density(0.0, 300.0, 0.0);
        assert!(low > high, "low={low} high={high}");
    }

    #[test]
    fn ocean_is_the_majority() {
        // Over a wide area at sea level, most columns' surface is below sea
        // level (ocean): sample density just above sea level and expect mostly air.
        let p = TerrainPipeline::new(7);
        let mut air = 0;
        let mut total = 0;
        let step = 47.0; // meters, coprime-ish with wavelengths to spread samples
        for i in 0..40 {
            for j in 0..40 {
                let x = i as f64 * step;
                let z = j as f64 * step;
                if p.final_density(x, SEA_LEVEL + 2.0, z) <= 0.0 {
                    air += 1;
                }
                total += 1;
            }
        }
        assert!(air * 2 > total, "expected majority air above sea level, got {air}/{total}");
    }

    #[test]
    fn column_profile_matches_graph_bit_exact() {
        let p = TerrainPipeline::new(999);
        for i in 0..20 {
            for j in 0..20 {
                let x = i as f64 * 13.0;
                let z = j as f64 * 17.0;
                let prof = p.column(x, z);
                for k in 0..12 {
                    let y = -64.0 + k as f64 * 34.0; // spans the world band incl. ends
                    assert_eq!(
                        prof.density_at(y),
                        p.final_density(x, y, z),
                        "profile/graph mismatch at ({x}, {y}, {z})"
                    );
                }
            }
        }
    }
}
