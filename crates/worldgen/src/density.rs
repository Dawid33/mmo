//! Composable density functions evaluated at world-meter (x, y, z). Pure `f64`.

use crate::noise::OctaveNoise;
use crate::spline::Spline;

/// The set of named noises a `DensityFn::Noise{idx}` indexes into. The pipeline
/// owns the index assignment (see `pipeline.rs`).
pub struct NoiseSet(pub Vec<OctaveNoise>);

// The tree-walk evaluator below is now the pipeline's test oracle only:
// `TerrainPipeline::column`/`final_density` (crates/worldgen/src/pipeline.rs)
// fuse the graph into flat FlatSpline arithmetic at runtime instead of
// walking `DensityFn`, so outside `#[cfg(test)]` nothing constructs most
// variants or calls most of `impl DensityFn` below anymore. Kept (not
// deleted) because it's still the spec `fused_column_matches_density_graphs_
// bit_exact` checks the fused path against, and per the `Min`/`Max`/
// `RangeChoice` note just below, the vocabulary future graph features
// (caves, continent/island split) will build on. allow-dead accordingly.
#[allow(dead_code)]
pub enum DensityFn {
    Const(f64),
    /// 2D noise `idx`, sampled at `(x, z) * xz_scale` (meters).
    Noise {
        idx: usize,
        xz_scale: f64,
    },
    /// 2D noise `idx` sampled at `((x + shift_x) * xz_scale, (z + shift_z) *
    /// xz_scale)`: a domain warp, shifts in meters. Shifting by another
    /// low-frequency noise bends round noise blobs into long coherent lines
    /// (the pipeline's mountain ranges).
    WarpedNoise {
        idx: usize,
        xz_scale: f64,
        shift_x: Box<DensityFn>,
        shift_z: Box<DensityFn>,
    },
    /// Linear ramp in `y` from `from_v` at `from_y` to `to_v` at `to_y`, clamped.
    YGradient {
        from_y: f64,
        to_y: f64,
        from_v: f64,
        to_v: f64,
    },
    Add(Box<DensityFn>, Box<DensityFn>),
    Mul(Box<DensityFn>, Box<DensityFn>),
    // `Min`/`Max`/`RangeChoice` are engine vocabulary the v1 pipeline doesn't use
    // yet: the deferred features fold in through them (caves as `min(terrain,
    // caves)`, the continent/island split as a `RangeChoice`). Kept + tested so
    // those specs don't re-add primitives; allow-dead until a consumer lands.
    #[allow(dead_code)]
    Min(Box<DensityFn>, Box<DensityFn>),
    #[allow(dead_code)]
    Max(Box<DensityFn>, Box<DensityFn>),
    Abs(Box<DensityFn>),
    Clamp {
        input: Box<DensityFn>,
        min: f64,
        max: f64,
    },
    SplineOf {
        coord: Box<DensityFn>,
        spline: Spline,
    },
    #[allow(dead_code)]
    RangeChoice {
        input: Box<DensityFn>,
        min: f64,
        max: f64,
        in_range: Box<DensityFn>,
        out_range: Box<DensityFn>,
    },
    /// The island layer (`pipeline::island_terms`) at the point: sampled at
    /// sample coordinates minus the pipeline's sample offsets, exactly as
    /// `TerrainPipeline::column` computes them.
    Island {
        field: std::sync::Arc<crate::islands::IslandField>,
        value: IslandValue,
    },
}

/// Which island term a `DensityFn::Island` yields (test oracle only, like
/// the rest of the tree-walk evaluator).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandValue {
    /// Pseudo-|continentalness| (0 deep sea … `ISLAND_CORE_C` at the centre).
    Continent,
    /// Land relief multiplier in `ISLAND_MIN_RELIEF..=1`.
    Relief,
}

impl Clone for DensityFn {
    fn clone(&self) -> Self {
        match self {
            DensityFn::Const(v) => DensityFn::Const(*v),
            DensityFn::Noise { idx, xz_scale } => DensityFn::Noise {
                idx: *idx,
                xz_scale: *xz_scale,
            },
            DensityFn::WarpedNoise {
                idx,
                xz_scale,
                shift_x,
                shift_z,
            } => DensityFn::WarpedNoise {
                idx: *idx,
                xz_scale: *xz_scale,
                shift_x: shift_x.clone(),
                shift_z: shift_z.clone(),
            },
            DensityFn::YGradient {
                from_y,
                to_y,
                from_v,
                to_v,
            } => DensityFn::YGradient {
                from_y: *from_y,
                to_y: *to_y,
                from_v: *from_v,
                to_v: *to_v,
            },
            DensityFn::Add(a, b) => DensityFn::Add(a.clone(), b.clone()),
            DensityFn::Mul(a, b) => DensityFn::Mul(a.clone(), b.clone()),
            DensityFn::Min(a, b) => DensityFn::Min(a.clone(), b.clone()),
            DensityFn::Max(a, b) => DensityFn::Max(a.clone(), b.clone()),
            DensityFn::Abs(a) => DensityFn::Abs(a.clone()),
            DensityFn::Clamp { input, min, max } => DensityFn::Clamp {
                input: input.clone(),
                min: *min,
                max: *max,
            },
            DensityFn::SplineOf { coord, spline } => DensityFn::SplineOf {
                coord: coord.clone(),
                spline: spline.clone(),
            },
            DensityFn::RangeChoice {
                input,
                min,
                max,
                in_range,
                out_range,
            } => DensityFn::RangeChoice {
                input: input.clone(),
                min: *min,
                max: *max,
                in_range: in_range.clone(),
                out_range: out_range.clone(),
            },
            DensityFn::Island { field, value } => DensityFn::Island {
                field: field.clone(),
                value: *value,
            },
        }
    }
}

// See the "tree-walk evaluator" note on the enum above: outside `#[cfg(test)]`
// this impl block is exercised only by the pipeline's test oracle and this
// module's own unit tests.
#[allow(dead_code)]
impl DensityFn {
    pub fn add(a: DensityFn, b: DensityFn) -> DensityFn {
        DensityFn::Add(Box::new(a), Box::new(b))
    }
    pub fn mul(a: DensityFn, b: DensityFn) -> DensityFn {
        DensityFn::Mul(Box::new(a), Box::new(b))
    }
    #[allow(dead_code)] // see the Min/Max/RangeChoice note on the enum
    pub fn min(a: DensityFn, b: DensityFn) -> DensityFn {
        DensityFn::Min(Box::new(a), Box::new(b))
    }
    #[allow(dead_code)] // see the Min/Max/RangeChoice note on the enum
    pub fn max(a: DensityFn, b: DensityFn) -> DensityFn {
        DensityFn::Max(Box::new(a), Box::new(b))
    }
    pub fn abs(a: DensityFn) -> DensityFn {
        DensityFn::Abs(Box::new(a))
    }
    pub fn clamp(a: DensityFn, min: f64, max: f64) -> DensityFn {
        DensityFn::Clamp {
            input: Box::new(a),
            min,
            max,
        }
    }
    pub fn spline_of(coord: DensityFn, spline: Spline) -> DensityFn {
        DensityFn::SplineOf {
            coord: Box::new(coord),
            spline,
        }
    }
    #[allow(dead_code)] // see the Min/Max/RangeChoice note on the enum
    pub fn range_choice(
        input: DensityFn,
        min: f64,
        max: f64,
        in_range: DensityFn,
        out_range: DensityFn,
    ) -> DensityFn {
        DensityFn::RangeChoice {
            input: Box::new(input),
            min,
            max,
            in_range: Box::new(in_range),
            out_range: Box::new(out_range),
        }
    }

    pub fn eval(&self, x: f64, y: f64, z: f64, noises: &NoiseSet) -> f64 {
        match self {
            DensityFn::Const(v) => *v,
            DensityFn::Noise { idx, xz_scale } => {
                noises.0[*idx].sample2(x * xz_scale, z * xz_scale)
            }
            DensityFn::WarpedNoise {
                idx,
                xz_scale,
                shift_x,
                shift_z,
            } => {
                let wx = x + shift_x.eval(x, y, z, noises);
                let wz = z + shift_z.eval(x, y, z, noises);
                noises.0[*idx].sample2(wx * xz_scale, wz * xz_scale)
            }
            DensityFn::YGradient {
                from_y,
                to_y,
                from_v,
                to_v,
            } => {
                let t = ((y - from_y) / (to_y - from_y)).clamp(0.0, 1.0);
                from_v + t * (to_v - from_v)
            }
            DensityFn::Add(a, b) => a.eval(x, y, z, noises) + b.eval(x, y, z, noises),
            DensityFn::Mul(a, b) => a.eval(x, y, z, noises) * b.eval(x, y, z, noises),
            DensityFn::Min(a, b) => a.eval(x, y, z, noises).min(b.eval(x, y, z, noises)),
            DensityFn::Max(a, b) => a.eval(x, y, z, noises).max(b.eval(x, y, z, noises)),
            DensityFn::Abs(a) => a.eval(x, y, z, noises).abs(),
            DensityFn::Clamp { input, min, max } => input.eval(x, y, z, noises).clamp(*min, *max),
            DensityFn::SplineOf { coord, spline } => spline.eval(coord.eval(x, y, z, noises)),
            DensityFn::RangeChoice {
                input,
                min,
                max,
                in_range,
                out_range,
            } => {
                let v = input.eval(x, y, z, noises);
                if v >= *min && v < *max {
                    in_range.eval(x, y, z, noises)
                } else {
                    out_range.eval(x, y, z, noises)
                }
            }
            DensityFn::Island { field, value } => {
                let (c, k) = crate::pipeline::island_terms(field, noises, x, z);
                match value {
                    IslandValue::Continent => c,
                    IslandValue::Relief => k,
                }
            }
        }
    }
}
