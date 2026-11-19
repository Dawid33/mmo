//! Cubic-Hermite splines whose control-point values may themselves be splines
//! (recursion is load-bearing). Pure `f64` (see plan Global Constraints).

/// A control point: `value` is itself a spline (may be `Const`), evaluated at
/// the outer coordinate; `derivative` is the slope used for Hermite blending.
#[derive(Clone)]
pub struct Point {
    pub location: f64,
    pub value: Box<Spline>,
    pub derivative: f64,
}

#[derive(Clone)]
pub enum Spline {
    Const(f64),
    Curve { points: Vec<Point> },
}

impl Spline {
    pub fn constant(v: f64) -> Spline {
        Spline::Const(v)
    }

    pub fn point(location: f64, value: Spline, derivative: f64) -> Point {
        Point { location, value: Box::new(value), derivative }
    }

    // Boxed-recursive eval: the runtime path uses `FlatSpline::eval` instead
    // (see pipeline.rs's fused `column()`); this stays as the oracle
    // `Spline::eval`/`FlatSpline::eval` bit-exactness tests compare against,
    // and for `DensityFn::SplineOf` in the (now test-only) density graph.
    #[allow(dead_code)]
    pub fn eval(&self, coord: f64) -> f64 {
        let points = match self {
            Spline::Const(v) => return *v,
            Spline::Curve { points } => points,
        };
        if points.is_empty() {
            return 0.0;
        }
        // Below the first point: linear by its derivative.
        let first = &points[0];
        if coord <= first.location {
            return first.value.eval(coord) + first.derivative * (coord - first.location);
        }
        // Above the last point: linear by its derivative.
        let last = &points[points.len() - 1];
        if coord >= last.location {
            return last.value.eval(coord) + last.derivative * (coord - last.location);
        }
        // Find the bracketing segment [p0, p1].
        let mut i = 0;
        while coord >= points[i + 1].location {
            i += 1;
        }
        let p0 = &points[i];
        let p1 = &points[i + 1];
        let (l0, l1) = (p0.location, p1.location);
        let v0 = p0.value.eval(coord);
        let v1 = p1.value.eval(coord);
        let span = l1 - l0;
        let t = (coord - l0) / span;
        // Cubic Hermite basis (Minecraft's spline interpolation).
        let k0 = v0;
        let k1 = v1;
        let m0 = p0.derivative * span;
        let m1 = p1.derivative * span;
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * k0
            + (t3 - 2.0 * t2 + t) * m0
            + (-2.0 * t3 + 3.0 * t2) * k1
            + (t3 - t2) * m1
    }
}

/// `Spline::Curve` flattened to parallel arrays for the common case where
/// every control-point value is `Const`. Arithmetic is copied verbatim from
/// `Spline::eval` so results are bit-identical.
pub struct FlatSpline {
    locations: Box<[f64]>,
    values: Box<[f64]>,
    derivatives: Box<[f64]>,
}

impl FlatSpline {
    /// Panics if any control-point value is not `Spline::Const` — the v1
    /// pipeline never nests curves; revisit if a nested spline ever lands.
    /// An empty `Curve` yields a `FlatSpline` that evaluates to `0.0` for
    /// every coord, matching `Spline::eval`.
    pub fn from_spline(s: &Spline) -> FlatSpline {
        let points = match s {
            Spline::Const(v) => {
                return FlatSpline {
                    locations: Box::new([0.0]),
                    values: Box::new([*v]),
                    derivatives: Box::new([0.0]),
                }
            }
            Spline::Curve { points } => points,
        };
        let values = points
            .iter()
            .map(|p| match p.value.as_ref() {
                Spline::Const(v) => *v,
                Spline::Curve { .. } => panic!("FlatSpline: nested curve control value"),
            })
            .collect();
        FlatSpline {
            locations: points.iter().map(|p| p.location).collect(),
            values,
            derivatives: points.iter().map(|p| p.derivative).collect(),
        }
    }

    pub fn eval(&self, coord: f64) -> f64 {
        // Empty curve: Spline::eval returns 0.0.
        if self.locations.is_empty() {
            return 0.0;
        }
        let (locs, vals, ders) = (&self.locations, &self.values, &self.derivatives);
        let n = locs.len();
        if coord <= locs[0] {
            return vals[0] + ders[0] * (coord - locs[0]);
        }
        if coord >= locs[n - 1] {
            return vals[n - 1] + ders[n - 1] * (coord - locs[n - 1]);
        }
        let mut i = 0;
        while coord >= locs[i + 1] {
            i += 1;
        }
        let (l0, l1) = (locs[i], locs[i + 1]);
        let (k0, k1) = (vals[i], vals[i + 1]);
        let span = l1 - l0;
        let t = (coord - l0) / span;
        let m0 = ders[i] * span;
        let m1 = ders[i + 1] * span;
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * k0
            + (t3 - 2.0 * t2 + t) * m0
            + (-2.0 * t3 + 3.0 * t2) * k1
            + (t3 - t2) * m1
    }
}

