use crate::parry::math::{Isometry, Real};
use crate::parry::query::ClosestPoints;
use crate::parry::shape::Segment;

/// Distance between two segments.
#[inline]
pub fn distance_segment_segment(
    pos12: &Isometry<Real>,
    segment1: &Segment,
    segment2: &Segment,
) -> Real {
    match crate::parry::query::details::closest_points_segment_segment(
        pos12,
        segment1,
        segment2,
        Real::MAX,
    ) {
        ClosestPoints::WithinMargin(p1, p2) => na::distance(&p1, &(pos12 * p2)),
        _ => 0.0,
    }
}
