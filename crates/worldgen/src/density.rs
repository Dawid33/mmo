//! Composable density functions evaluated at world-meter (x, y, z). Pure `f64`.

use crate::noise::OctaveNoise;
use crate::spline::Spline;

/// The set of named noises a `DensityFn::Noise{idx}` indexes into. The pipeline
/// owns the index assignment (see `pipeline.rs`).
pub struct NoiseSet(pub Vec<OctaveNoise>);

pub enum DensityFn {
    Const(f64),
    /// 2D noise `idx`, sampled at `(x, z) * xz_scale` (meters).
    Noise { idx: usize, xz_scale: f64 },
    /// Linear ramp in `y` from `from_v` at `from_y` to `to_v` at `to_y`, clamped.
    YGradient { from_y: f64, to_y: f64, from_v: f64, to_v: f64 },
    Add(Box<DensityFn>, Box<DensityFn>),
    Mul(Box<DensityFn>, Box<DensityFn>),
    Min(Box<DensityFn>, Box<DensityFn>),
    Max(Box<DensityFn>, Box<DensityFn>),
    Abs(Box<DensityFn>),
    Clamp { input: Box<DensityFn>, min: f64, max: f64 },
    SplineOf { coord: Box<DensityFn>, spline: Spline },
    RangeChoice {
        input: Box<DensityFn>,
        min: f64,
        max: f64,
        in_range: Box<DensityFn>,
        out_range: Box<DensityFn>,
    },
}

impl Clone for DensityFn {
    fn clone(&self) -> Self {
        match self {
            DensityFn::Const(v) => DensityFn::Const(*v),
            DensityFn::Noise { idx, xz_scale } => DensityFn::Noise { idx: *idx, xz_scale: *xz_scale },
            DensityFn::YGradient { from_y, to_y, from_v, to_v } => {
                DensityFn::YGradient { from_y: *from_y, to_y: *to_y, from_v: *from_v, to_v: *to_v }
            }
            DensityFn::Add(a, b) => DensityFn::Add(a.clone(), b.clone()),
            DensityFn::Mul(a, b) => DensityFn::Mul(a.clone(), b.clone()),
            DensityFn::Min(a, b) => DensityFn::Min(a.clone(), b.clone()),
            DensityFn::Max(a, b) => DensityFn::Max(a.clone(), b.clone()),
            DensityFn::Abs(a) => DensityFn::Abs(a.clone()),
            DensityFn::Clamp { input, min, max } => {
                DensityFn::Clamp { input: input.clone(), min: *min, max: *max }
            }
            DensityFn::SplineOf { coord, spline } => {
                DensityFn::SplineOf { coord: coord.clone(), spline: spline.clone() }
            }
            DensityFn::RangeChoice { input, min, max, in_range, out_range } => DensityFn::RangeChoice {
                input: input.clone(),
                min: *min,
                max: *max,
                in_range: in_range.clone(),
                out_range: out_range.clone(),
            },
        }
    }
}

impl DensityFn {
    pub fn add(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Add(Box::new(a), Box::new(b)) }
    pub fn mul(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Mul(Box::new(a), Box::new(b)) }
    pub fn min(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Min(Box::new(a), Box::new(b)) }
    pub fn max(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Max(Box::new(a), Box::new(b)) }
    pub fn abs(a: DensityFn) -> DensityFn { DensityFn::Abs(Box::new(a)) }
    pub fn clamp(a: DensityFn, min: f64, max: f64) -> DensityFn {
        DensityFn::Clamp { input: Box::new(a), min, max }
    }
    pub fn spline_of(coord: DensityFn, spline: Spline) -> DensityFn {
        DensityFn::SplineOf { coord: Box::new(coord), spline }
    }
    pub fn range_choice(input: DensityFn, min: f64, max: f64, in_range: DensityFn, out_range: DensityFn) -> DensityFn {
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
            DensityFn::Noise { idx, xz_scale } => noises.0[*idx].sample2(x * xz_scale, z * xz_scale),
            DensityFn::YGradient { from_y, to_y, from_v, to_v } => {
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
            DensityFn::RangeChoice { input, min, max, in_range, out_range } => {
                let v = input.eval(x, y, z, noises);
                if v >= *min && v < *max {
                    in_range.eval(x, y, z, noises)
                } else {
                    out_range.eval(x, y, z, noises)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::OctaveNoise;
    use crate::spline::Spline;

    fn empty_noises() -> NoiseSet {
        NoiseSet(vec![])
    }

    #[test]
    fn arithmetic_nodes() {
        let n = empty_noises();
        let two = DensityFn::Const(2.0);
        let three = DensityFn::Const(3.0);
        assert_eq!(DensityFn::add(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 5.0);
        assert_eq!(DensityFn::mul(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 6.0);
        assert_eq!(DensityFn::min(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 2.0);
        assert_eq!(DensityFn::max(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 3.0);
        assert_eq!(DensityFn::abs(DensityFn::Const(-4.0)).eval(0.0, 0.0, 0.0, &n), 4.0);
    }

    #[test]
    fn clamp_and_gradient() {
        let n = empty_noises();
        let c = DensityFn::clamp(DensityFn::Const(9.0), -1.0, 1.0);
        assert_eq!(c.eval(0.0, 0.0, 0.0, &n), 1.0);
        // Gradient: at from_y -> from_v, at to_y -> to_v, clamped outside.
        let g = DensityFn::YGradient { from_y: 0.0, to_y: 100.0, from_v: 0.0, to_v: 10.0 };
        assert_eq!(g.eval(0.0, 0.0, 0.0, &n), 0.0);
        assert_eq!(g.eval(0.0, 100.0, 0.0, &n), 10.0);
        assert_eq!(g.eval(0.0, 50.0, 0.0, &n), 5.0);
        assert_eq!(g.eval(0.0, -20.0, 0.0, &n), 0.0);
        assert_eq!(g.eval(0.0, 200.0, 0.0, &n), 10.0);
    }

    #[test]
    fn range_choice_selects() {
        let n = empty_noises();
        let rc = DensityFn::range_choice(
            DensityFn::Const(0.5),
            0.0,
            1.0,
            DensityFn::Const(100.0), // in range
            DensityFn::Const(-100.0), // out of range
        );
        assert_eq!(rc.eval(0.0, 0.0, 0.0, &n), 100.0);
    }

    #[test]
    fn spline_of_uses_input_as_coord() {
        let n = empty_noises();
        let s = Spline::Curve {
            points: vec![
                crate::spline::Spline::point(0.0, Spline::constant(0.0), 0.0),
                crate::spline::Spline::point(1.0, Spline::constant(10.0), 0.0),
            ],
        };
        let d = DensityFn::spline_of(DensityFn::Const(1.0), s);
        assert_eq!(d.eval(0.0, 0.0, 0.0, &n), 10.0);
    }

    #[test]
    fn noise_node_reads_noiseset() {
        let noises = NoiseSet(vec![OctaveNoise::new(1, -2, vec![1.0])]);
        let d = DensityFn::Noise { idx: 0, xz_scale: 1.0 };
        assert_eq!(d.eval(5.0, 0.0, 6.0, &noises), d.eval(5.0, 0.0, 6.0, &noises));
    }
}
