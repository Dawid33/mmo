use crate::parry::math::{Isometry, Real, RawReal};
use crate::parry::query::ClosestPoints;
use crate::parry::shape::Cuboid;

/// Distance between two cuboids.
#[inline]
pub fn distance_cuboid_cuboid(pos12: &Isometry<Real>, cuboid1: &Cuboid, cuboid2: &Cuboid) -> Real {
    match crate::parry::query::details::closest_points_cuboid_cuboid(pos12, cuboid1, cuboid2, Real::from(RawReal::MAX)) {
        ClosestPoints::WithinMargin(p1, p2) => na::distance(&p1, &(pos12 * p2)),
        _ => Real::from(0.0),
    }
}
