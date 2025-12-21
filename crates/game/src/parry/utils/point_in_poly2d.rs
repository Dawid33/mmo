use crate::parry::math::Real;
use na::Point2;
use num::Zero;

/// Tests if the given point is inside a convex polygon with arbitrary orientation.
///
/// This function uses a faster algorithm than [`point_in_poly2d`] but only works for
/// convex polygons. It checks if the point is on the same side of all edges of the polygon.
///
/// The polygon is assumed to be closed, i.e., first and last point of the polygon are implicitly
/// assumed to be connected by an edge.
///
/// # Arguments
///
/// * `pt` - The point to test
/// * `poly` - A slice of points defining the convex polygon vertices in any order (clockwise or counter-clockwise)
///
/// # Returns
///
/// `true` if the point is inside or on the boundary of the polygon, `false` otherwise.
///
/// # Examples
///
/// ## Point Inside a Square
///
/// ```
/// # #[cfg(all(feature = "dim2", feature = "f32"))] {
/// use parry2d::utils::point_in_convex_poly2d;
/// use parry2d::na::Point2;
///
/// let square = vec![
///     Point2::origin(),
///     Point2::new(2.0, 0.0),
///     Point2::new(2.0, 2.0),
///     Point2::new(0.0, 2.0),
/// ];
///
/// let inside = Point2::new(1.0, 1.0);
/// let outside = Point2::new(3.0, 1.0);
///
/// assert!(point_in_convex_poly2d(&inside, &square));
/// assert!(!point_in_convex_poly2d(&outside, &square));
/// # }
/// ```
///
/// ## Point on the Boundary
///
/// ```
/// # #[cfg(all(feature = "dim2", feature = "f32"))] {
/// use parry2d::utils::point_in_convex_poly2d;
/// use parry2d::na::Point2;
///
/// let triangle = vec![
///     Point2::origin(),
///     Point2::new(2.0, 0.0),
///     Point2::new(1.0, 2.0),
/// ];
///
/// let on_edge = Point2::new(1.0, 0.0);
/// assert!(point_in_convex_poly2d(&on_edge, &triangle));
/// # }
/// ```
///
/// ## Empty Polygon
///
/// ```
/// # #[cfg(all(feature = "dim2", feature = "f32"))] {
/// use parry2d::utils::point_in_convex_poly2d;
/// use parry2d::na::Point2;
///
/// let empty: Vec<Point2<f32>> = vec![];
/// let point = Point2::new(1.0, 1.0);
///
/// // An empty polygon contains no points
/// assert!(!point_in_convex_poly2d(&point, &empty));
/// # }
/// ```
pub fn point_in_convex_poly2d(pt: &Point2<Real>, poly: &[Point2<Real>]) -> bool {
    if poly.is_empty() {
        false
    } else {
        let mut sign = Real::from(0.0);

        for i1 in 0..poly.len() {
            let i2 = (i1 + 1) % poly.len();
            let seg_dir = poly[i2] - poly[i1];
            let dpt = pt - poly[i1];
            let perp = dpt.perp(&seg_dir);

            if sign.is_zero() {
                sign = perp;
            } else if sign * perp < 0.0.into() {
                return false;
            }
        }

        true
    }
}

/// Tests if the given point is inside an arbitrary closed polygon with arbitrary orientation,
/// using a winding number algorithm.
///
/// This function works with both convex and concave polygons, and even self-intersecting
/// polygons. It uses a winding number algorithm to determine if a point is inside.
///
/// The polygon is assumed to be closed, i.e., first and last point of the polygon are implicitly
/// assumed to be connected by an edge.
///
/// This handles concave polygons. For a faster function dedicated to convex polygons, see [`point_in_convex_poly2d`].
///
/// # Arguments
///
/// * `pt` - The point to test
/// * `poly` - A slice of points defining the polygon vertices in order (clockwise or counter-clockwise)
///
/// # Returns
///
/// `true` if the point is inside the polygon, `false` otherwise.
///
/// # Examples
///
/// ## Convex Polygon (Square)
///
/// ```
/// # #[cfg(all(feature = "dim2", feature = "f32"))] {
/// use parry2d::utils::point_in_poly2d;
/// use parry2d::na::Point2;
///
/// let square = vec![
///     Point2::origin(),
///     Point2::new(2.0, 0.0),
///     Point2::new(2.0, 2.0),
///     Point2::new(0.0, 2.0),
/// ];
///
/// let inside = Point2::new(1.0, 1.0);
/// let outside = Point2::new(3.0, 1.0);
///
/// assert!(point_in_poly2d(&inside, &square));
/// assert!(!point_in_poly2d(&outside, &square));
/// # }
/// ```
///
/// ## Concave Polygon (L-Shape)
///
/// ```
/// # #[cfg(all(feature = "dim2", feature = "f32"))] {
/// use parry2d::utils::point_in_poly2d;
/// use parry2d::na::Point2;
///
/// // L-shaped polygon (concave)
/// let l_shape = vec![
///     Point2::origin(),
///     Point2::new(2.0, 0.0),
///     Point2::new(2.0, 1.0),
///     Point2::new(1.0, 1.0),
///     Point2::new(1.0, 2.0),
///     Point2::new(0.0, 2.0),
/// ];
///
/// let inside_corner = Point2::new(0.5, 0.5);
/// let outside_corner = Point2::new(1.5, 1.5);
///
/// assert!(point_in_poly2d(&inside_corner, &l_shape));
/// assert!(!point_in_poly2d(&outside_corner, &l_shape));
/// # }
/// ```
///
/// ## Complex Polygon with Holes
///
/// ```
/// # #[cfg(all(feature = "dim2", feature = "f32"))] {
/// use parry2d::utils::point_in_poly2d;
/// use parry2d::na::Point2;
///
/// // A star shape (self-intersecting creates a complex winding pattern)
/// let star = vec![
///     Point2::new(0.0, 1.0),
///     Point2::new(0.5, 0.5),
///     Point2::new(1.0, 1.0),
///     Point2::new(0.7, 0.3),
///     Point2::new(1.0, -0.5),
///     Point2::origin(),
///     Point2::new(-1.0, -0.5),
///     Point2::new(-0.7, 0.3),
///     Point2::new(-1.0, 1.0),
///     Point2::new(-0.5, 0.5),
/// ];
///
/// let center = Point2::new(0.0, 0.5);
/// assert!(point_in_poly2d(&center, &star));
/// # }
/// ```
///
/// ## Empty Polygon
///
/// ```
/// # #[cfg(all(feature = "dim2", feature = "f32"))] {
/// use parry2d::utils::point_in_poly2d;
/// use parry2d::na::Point2;
///
/// let empty: Vec<Point2<f32>> = vec![];
/// let point = Point2::new(1.0, 1.0);
///
/// // An empty polygon contains no points
/// assert!(!point_in_poly2d(&point, &empty));
/// # }
/// ```
pub fn point_in_poly2d(pt: &Point2<Real>, poly: &[Point2<Real>]) -> bool {
    if poly.is_empty() {
        return false;
    }

    let mut winding = 0i32;

    for (i, a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        let seg_dir = b - a;
        let dpt = pt - a;
        let perp = dpt.perp(&seg_dir);
        winding += match (dpt.y >= 0.0.into(), b.y > pt.y) {
            (true, true) if perp < 0.0.into() => 1,
            (false, false) if perp > 0.0.into() => 1,
            _ => 0,
        };
    }

    winding % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_in_poly2d_self_intersecting() {
        let poly = [
            [Real::from(-1.0), Real::from(-1.0)],
            [Real::from(0.0), Real::from(-1.0)],
            [Real::from(0.0), Real::from(1.0)],
            [Real::from(-2.0), Real::from(1.0)],
            [Real::from(-2.0), Real::from(-2.0)],
            [Real::from(1.0), Real::from(-2.0)],
            [Real::from(1.0), Real::from(2.0)],
            [Real::from(-1.0), Real::from(2.0)],
        ]
        .map(Point2::from);
        assert!(!point_in_poly2d(&[Real::from(-0.5), Real::from(-0.5)].into(), &poly));
        assert!(point_in_poly2d(&[Real::from(0.5), Real::from(-0.5)].into(), &poly));
    }

    #[test]
    fn point_in_poly2d_concave() {
        let poly= [
            [Real::from(615.4741821289063), Real::from(279.4120788574219)],
            [Real::from(617.95947265625), Real::from(281.8973693847656)],
            [Real::from(624.1727294921875), Real::from(288.73193359375)],
            [Real::from(626.6580200195313), Real::from(292.4598693847656)],
            [Real::from(634.7352294921875), Real::from(302.40106201171875)],
            [Real::from(637.8418579101563), Real::from(306.7503356933594)],
            [Real::from(642.8124389648438), Real::from(312.96356201171875)],
            [Real::from(652.7536010742188), Real::from(330.98193359375)],
            [Real::from(654.6176147460938), Real::from(334.7098693847656)],
            [Real::from(661.4521484375), Real::from(349.0003356933594)],
            [Real::from(666.4227294921875), Real::from(360.18414306640625)],
            [Real::from(670.1506958007813), Real::from(367.6400451660156)],
            [Real::from(675.1212768554688), Real::from(381.30914306640625)],
            [Real::from(678.2279052734375), Real::from(391.2503356933594)],
            [Real::from(681.33447265625), Real::from(402.43414306640625)],
            [Real::from(683.81982421875), Real::from(414.23931884765625)],
            [Real::from(685.0624389648438), Real::from(422.3165283203125)],
            [Real::from(685.6837768554688), Real::from(431.0150146484375)],
            [Real::from(686.3051147460938), Real::from(442.8201904296875)],
            [Real::from(685.6837768554688), Real::from(454.0040283203125)],
            [Real::from(683.81982421875), Real::from(460.83856201171875)],
            [Real::from(679.4705200195313), Real::from(470.77972412109375)],
            [Real::from(674.4999389648438), Real::from(480.720947265625)],
            [Real::from(670.1506958007813), Real::from(486.93414306640625)],
            [Real::from(662.073486328125), Real::from(497.49664306640625)],
            [Real::from(659.5881958007813), Real::from(499.36065673828125)],
            [Real::from(653.3749389648438), Real::from(503.70989990234375)],
            [Real::from(647.7830200195313), Real::from(506.1951904296875)],
            [Real::from(642.8124389648438), Real::from(507.43780517578125)],
            [Real::from(631.6286010742188), Real::from(508.05914306640625)],
            [Real::from(621.0661010742188), Real::from(508.05914306640625)],
            [Real::from(605.5330200195313), Real::from(508.05914306640625)],
            [Real::from(596.2131958007813), Real::from(508.05914306640625)],
            [Real::from(586.893310546875), Real::from(508.05914306640625)],
            [Real::from(578.8161010742188), Real::from(508.05914306640625)],
            [Real::from(571.3602294921875), Real::from(506.1951904296875)],
            [Real::from(559.5551147460938), Real::from(499.36065673828125)],
            [Real::from(557.0697631835938), Real::from(497.49664306640625)],
            [Real::from(542.1580200195313), Real::from(484.4488525390625)],
            [Real::from(534.7021484375), Real::from(476.37164306640625)],
            [Real::from(532.8381958007813), Real::from(473.8863525390625)],
            [Real::from(527.2462768554688), Real::from(466.43048095703125)],
            [Real::from(522.2756958007813), Real::from(450.89739990234375)],
            [Real::from(521.6543579101563), Real::from(444.06280517578125)],
            [Real::from(521.0330200195313), Real::from(431.6363525390625)],
            [Real::from(521.6543579101563), Real::from(422.93780517578125)],
            [Real::from(523.518310546875), Real::from(409.26873779296875)],
            [Real::from(527.2462768554688), Real::from(397.46356201171875)],
            [Real::from(532.8381958007813), Real::from(385.6584167480469)],
            [Real::from(540.9154052734375), Real::from(373.23193359375)],
            [Real::from(547.1286010742188), Real::from(365.77606201171875)],
            [Real::from(559.5551147460938), Real::from(354.59222412109375)],
            [Real::from(573.2241821289063), Real::from(342.165771484375)],
            [Real::from(575.70947265625), Real::from(339.68048095703125)],
            [Real::from(584.4080200195313), Real::from(331.603271484375)],
            [Real::from(597.455810546875), Real::from(317.3128356933594)],
            [Real::from(601.8051147460938), Real::from(311.7209167480469)],
            [Real::from(607.39697265625), Real::from(303.6437072753906)],
            [Real::from(611.7462768554688), Real::from(296.1878356933594)],
            [Real::from(614.2315673828125), Real::from(288.1106262207031)],
            [Real::from(615.4741821289063), Real::from(280.65472412109375)],
            [Real::from(615.4741821289063), Real::from(279.4120788574219)],
        ]
        .map(Point2::from);
        let pt = Point2::from([Real::from(596.0181884765625), Real::from(427.9162902832031)]);
        assert!(point_in_poly2d(&pt, &poly));
    }

    #[test]
    #[cfg(all(feature = "dim2", feature = "alloc"))]
    fn point_in_poly2d_concave_exact_vertex_bug() {
        let poly = crate::parry::shape::Ball::new(1.0).to_polyline(10);
        assert!(point_in_poly2d(&Point2::origin(), &poly));
        assert!(point_in_poly2d(&Point2::new(-0.25, 0.0), &poly));
        assert!(point_in_poly2d(&Point2::new(0.25, 0.0), &poly));
        assert!(point_in_poly2d(&Point2::new(0.0, -0.25), &poly));
        assert!(point_in_poly2d(&Point2::new(0.0, 0.25), &poly));
        assert!(!point_in_poly2d(&Point2::new(-2.0, 0.0), &poly));
        assert!(!point_in_poly2d(&Point2::new(2.0, 0.0), &poly));
        assert!(!point_in_poly2d(&Point2::new(0.0, -2.0), &poly));
        assert!(!point_in_poly2d(&Point2::new(0.0, 2.0), &poly));
    }
}
