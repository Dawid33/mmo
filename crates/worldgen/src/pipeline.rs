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
    let neg_y = DensityFn::YGradient { from_y: -64.0, to_y: 320.0, from_v: 64.0, to_v: -320.0 };
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
        Self { noises, graph: final_density_graph() }
    }

    pub fn final_density(&self, x: f64, y: f64, z: f64) -> f64 {
        self.graph.eval(x, y, z, &self.noises)
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
}
