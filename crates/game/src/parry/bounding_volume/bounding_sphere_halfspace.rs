use crate::parry::bounding_volume::BoundingSphere;
use crate::parry::math::{Isometry, Point, Real};
use crate::parry::shape::HalfSpace;

use num::Bounded;

impl HalfSpace {
    /// Computes the world-space bounding sphere of this half-space, transformed by `pos`.
    #[inline]
    pub fn bounding_sphere(&self, pos: &Isometry<Real>) -> BoundingSphere {
        let bv: BoundingSphere = self.local_bounding_sphere();
        bv.transform_by(pos)
    }

    /// Computes the local-space bounding sphere of this half-space.
    #[inline]
    pub fn local_bounding_sphere(&self) -> BoundingSphere {
        let radius = Real::max_value();

        BoundingSphere::new(Point::origin(), radius)
    }
}
