//! Cubic-Hermite splines whose control-point values may themselves be splines
//! (recursion is load-bearing). Pure `f64` (see plan Global Constraints).

/// A control point: `value` is itself a spline (may be `Const`), evaluated at
/// the outer coordinate; `derivative` is the slope used for Hermite blending.
pub struct Point {
    pub location: f64,
    pub value: Box<Spline>,
    pub derivative: f64,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_ignores_coord() {
        assert_eq!(Spline::constant(3.0).eval(-99.0), 3.0);
        assert_eq!(Spline::constant(3.0).eval(99.0), 3.0);
    }

    #[test]
    fn passes_through_control_points() {
        // Two flat points at (0 -> 0) and (1 -> 10).
        let s = Spline::Curve {
            points: vec![
                Point { location: 0.0, value: Box::new(Spline::constant(0.0)), derivative: 0.0 },
                Point { location: 1.0, value: Box::new(Spline::constant(10.0)), derivative: 0.0 },
            ],
        };
        assert_eq!(s.eval(0.0), 0.0);
        assert_eq!(s.eval(1.0), 10.0);
        // Monotone between, and flat-derivative midpoint sits at the Hermite mean.
        let mid = s.eval(0.5);
        assert!(mid > 0.0 && mid < 10.0, "mid={mid}");
    }

    #[test]
    fn clamps_outside_range() {
        let s = Spline::Curve {
            points: vec![
                Point { location: 0.0, value: Box::new(Spline::constant(2.0)), derivative: 0.0 },
                Point { location: 1.0, value: Box::new(Spline::constant(8.0)), derivative: 0.0 },
            ],
        };
        // Linear extrapolation by the endpoint derivative (0 here) => clamps flat.
        assert_eq!(s.eval(-5.0), 2.0);
        assert_eq!(s.eval(5.0), 8.0);
    }

    #[test]
    fn nested_value_is_evaluated_at_same_coord() {
        // Outer point's value is itself a spline; eval it at the same coord.
        let inner = Spline::Curve {
            points: vec![
                Point { location: 0.0, value: Box::new(Spline::constant(0.0)), derivative: 0.0 },
                Point { location: 1.0, value: Box::new(Spline::constant(100.0)), derivative: 0.0 },
            ],
        };
        let outer = Spline::Curve {
            points: vec![Point { location: 0.0, value: Box::new(inner), derivative: 0.0 }],
        };
        // Single point => returns that point's value, evaluated at the coord.
        assert_eq!(outer.eval(1.0), 100.0);
    }
}
