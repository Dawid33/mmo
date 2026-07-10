//! Our tuned density graph: continentalness/erosion/ridges -> offset/factor/
//! jaggedness -> depth -> final_density. Numbers here are hand-tuned, not
//! ported from Tectonic (see the design spec).

use crate::density::NoiseSet;
#[cfg(test)]
use crate::density::DensityFn;
use crate::noise::OctaveNoise;
use crate::spline::{FlatSpline, Spline};

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

// Noise-sampling offset (meters), applied to (x, z) before every noise read.
// The world origin must NOT sit on the noise lattice zero: Perlin noise is 0
// at lattice points for every seed, which pinned spawn (region (0,0)) to the
// seed-independent "noise = 0" terrain — mid-ocean. Any large non-special
// values work; they just have to be applied consistently.
const SAMPLE_OFFSET_X_M: f64 = 100_000.0;
const SAMPLE_OFFSET_Z_M: f64 = 50_000.0;

// Vertical density ramp (the `neg_y` term): carries `-y` across the world band,
// clamped at the ends. Used by BOTH the density graph node and ColumnProfile so
// they stay bit-identical.
const RAMP_FROM_Y: f64 = -64.0;
const RAMP_TO_Y: f64 = 320.0;
const RAMP_FROM_V: f64 = 64.0;
const RAMP_TO_V: f64 = -320.0;

// Noise xz scales (used by both the graph builders and the fused path).
const XZ_SCALE_CONTINENTS: f64 = 0.13;
const XZ_SCALE_EROSION: f64 = 0.25;
const XZ_SCALE_RIDGES: f64 = 0.5;

// Graph-tree builders below (`continentalness`/`erosion`/`ridges` through
// `final_density_graph`) are the test oracle only: `TerrainPipeline::column`/
// `final_density` no longer walk the `DensityFn` tree at runtime (see the
// fused evaluation in `impl TerrainPipeline`), so these are `#[cfg(test)]`
// and exist to keep `fused_column_matches_density_graphs_bit_exact` (and
// `column_profile_matches_graph_bit_exact`) honest. They also stay as the
// spec for future graph features (caves, continent/island `RangeChoice` —
// see the roadmap note on `DensityFn::Min`/`Max`/`RangeChoice` in density.rs).
#[cfg(test)]
fn continentalness() -> DensityFn {
    DensityFn::Noise { idx: N_CONTINENTS, xz_scale: XZ_SCALE_CONTINENTS }
}
#[cfg(test)]
fn erosion() -> DensityFn {
    DensityFn::Noise { idx: N_EROSION, xz_scale: XZ_SCALE_EROSION }
}
#[cfg(test)]
fn ridges() -> DensityFn {
    DensityFn::Noise { idx: N_RIDGES, xz_scale: XZ_SCALE_RIDGES }
}

fn continents_curve_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(0.4, Spline::constant(0.6), 1.0),
            Spline::point(1.0, Spline::constant(1.2), 0.0),
        ],
    }
}

fn erosion_mod_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(1.0), 0.0),
            Spline::point(0.2, Spline::constant(0.5), 0.0),
            Spline::point(1.0, Spline::constant(0.15), 0.0),
        ],
    }
}

fn offset_base_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(-0.9), 0.0),
            Spline::point(-0.2, Spline::constant(-0.3), 0.5),
            Spline::point(0.0, Spline::constant(0.02), 0.6),
            Spline::point(0.6, Spline::constant(0.9), 1.0),
            Spline::point(1.2, Spline::constant(1.8), 0.5),
        ],
    }
}

fn factor_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(0.6), 0.0),
            Spline::point(0.0, Spline::constant(1.2), 0.0),
            Spline::point(1.0, Spline::constant(3.0), 0.0),
        ],
    }
}

fn jaggedness_gate_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(1.0, Spline::constant(0.6), 0.0),
        ],
    }
}

/// abs(continentalness) pushed through a rising curve, minus the ocean bias.
/// Negative over most of the map (ocean); positive where |continents| is large.
#[cfg(test)]
fn raw_continents() -> DensityFn {
    DensityFn::add(
        DensityFn::Const(OCEAN_OFFSET),
        DensityFn::spline_of(DensityFn::abs(continentalness()), continents_curve_spline()),
    )
}

/// Surface height offset (in offset units) as a spline on raw_continents,
/// its inland high end pulled down by erosion (flat plains vs mountains).
#[cfg(test)]
fn offset() -> DensityFn {
    // offset = base(raw) * erosion_mod(erosion), clamped.
    DensityFn::clamp(
        DensityFn::mul(
            DensityFn::spline_of(raw_continents(), offset_base_spline()),
            DensityFn::spline_of(erosion(), erosion_mod_spline()),
        ),
        MIN_OFFSET,
        MAX_OFFSET,
    )
}

/// Vertical stretch factor (always positive): tall in low-erosion inland.
#[cfg(test)]
fn factor() -> DensityFn {
    DensityFn::spline_of(offset(), factor_spline())
}

/// Jaggedness: a ridges-driven bump, nonzero mainly at high offset.
#[cfg(test)]
fn jaggedness() -> DensityFn {
    DensityFn::mul(
        DensityFn::spline_of(offset(), jaggedness_gate_spline()),
        DensityFn::abs(ridges()),
    )
}

/// final_density = (surface_height - y) * DEPTH_PER_BLOCK * factor + jaggedness.
/// `surface_height = SEA_LEVEL + offset*HEIGHT_SCALE` (meters); the value is
/// positive below the surface, negative above. `neg_y` is a wide linear ramp
/// carrying `-y` across the whole world band.
#[cfg(test)]
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

/// Noise construction shared by `TerrainPipeline::new` and the oracle test.
/// Per-noise sub-seeds derived from the world seed; amplitudes are our own
/// (long-wavelength continents, medium erosion, short ridges).
fn pipeline_noises(seed: u64) -> NoiseSet {
    NoiseSet(vec![
        OctaveNoise::new(seed ^ 0x00C0, -10, vec![1.75, 1.0, 2.0, 3.0, 2.0, 2.0, 1.0]),
        OctaveNoise::new(seed ^ 0x00E0, -10, vec![2.0, 1.75, 1.5, 1.5, 1.3, 1.0]),
        OctaveNoise::new(seed ^ 0x00D0, -8, vec![1.0, 2.0, 1.0]),
    ])
}

pub struct TerrainPipeline {
    noises: NoiseSet,
    continents_curve: FlatSpline,
    offset_base: FlatSpline,
    erosion_mod: FlatSpline,
    factor_curve: FlatSpline,
    jaggedness_gate: FlatSpline,
}

impl TerrainPipeline {
    pub fn new(seed: u64) -> Self {
        Self {
            noises: pipeline_noises(seed),
            continents_curve: FlatSpline::from_spline(&continents_curve_spline()),
            offset_base: FlatSpline::from_spline(&offset_base_spline()),
            erosion_mod: FlatSpline::from_spline(&erosion_mod_spline()),
            factor_curve: FlatSpline::from_spline(&factor_spline()),
            jaggedness_gate: FlatSpline::from_spline(&jaggedness_gate_spline()),
        }
    }

    pub fn final_density(&self, x: f64, y: f64, z: f64) -> f64 {
        // ColumnProfile::density_at reproduces final_density_graph bit-for-bit
        // (see `column_profile_matches_graph_bit_exact`), so the 3D entry point
        // is just the 2D profile plus the cheap y arithmetic.
        self.column(x, z).density_at(y)
    }

    /// Fused evaluation of the offset/factor/jaggedness graphs: each noise is
    /// sampled ONCE (the graphs share the whole `offset` subtree, which the
    /// tree-walk evaluator recomputed three times). Every f64 op below mirrors
    /// the graph's op order exactly — guarded bit-for-bit by
    /// `fused_column_matches_density_graphs_bit_exact`.
    pub fn column(&self, x: f64, z: f64) -> ColumnProfile {
        let (x, z) = (x + SAMPLE_OFFSET_X_M, z + SAMPLE_OFFSET_Z_M);
        let n = &self.noises.0;
        let cont = n[N_CONTINENTS].sample2(x * XZ_SCALE_CONTINENTS, z * XZ_SCALE_CONTINENTS);
        let ero = n[N_EROSION].sample2(x * XZ_SCALE_EROSION, z * XZ_SCALE_EROSION);
        let ridge = n[N_RIDGES].sample2(x * XZ_SCALE_RIDGES, z * XZ_SCALE_RIDGES);

        // raw_continents = OCEAN_OFFSET + curve(|continents|)   (Add(Const, SplineOf))
        let raw = OCEAN_OFFSET + self.continents_curve.eval(cont.abs());
        // offset = clamp(base(raw) * erosion_mod(erosion))
        let offset = (self.offset_base.eval(raw) * self.erosion_mod.eval(ero))
            .clamp(MIN_OFFSET, MAX_OFFSET);
        let factor = self.factor_curve.eval(offset);
        let jaggedness = self.jaggedness_gate.eval(offset) * ridge.abs();

        ColumnProfile { offset_h: offset * HEIGHT_SCALE, factor, jaggedness }
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

    fn worldgen_test_seed() -> u64 { 0x00D5_C0DE_8D78 } // test helper, next to the test

    #[test]
    fn fused_column_matches_density_graphs_bit_exact() {
        let p = TerrainPipeline::new(worldgen_test_seed());
        let noises = pipeline_noises(worldgen_test_seed());
        let (graph, offset_g, factor_g, jaggedness_g) =
            (final_density_graph(), offset(), factor(), jaggedness());
        for i in 0..64 {
            for j in 0..64 {
                let (x, z) = (i as f64 * 7.3 - 200.0, j as f64 * 11.1 - 150.0);
                let (sx, sz) = (x + SAMPLE_OFFSET_X_M, z + SAMPLE_OFFSET_Z_M);
                let prof = p.column(x, z);
                assert_eq!(prof.offset_h, offset_g.eval(sx, 0.0, sz, &noises) * HEIGHT_SCALE);
                assert_eq!(prof.factor, factor_g.eval(sx, 0.0, sz, &noises));
                assert_eq!(prof.jaggedness, jaggedness_g.eval(sx, 0.0, sz, &noises));
                for y in [-64.0, 0.0, 63.5, 64.5, 200.0, 320.0] {
                    assert_eq!(p.final_density(x, y, z), graph.eval(sx, y, sz, &noises));
                }
            }
        }
    }
}
