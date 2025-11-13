use crate::parry::bounding_volume;
use crate::parry::bounding_volume::BoundingSphere;
use crate::parry::math::{Isometry, Real};
use crate::parry::shape::ConvexPolygon;

impl ConvexPolygon {
    /// Computes the world-space bounding sphere of this convex polygon, transformed by `pos`.
    #[inline]
    pub fn bounding_sphere(&self, pos: &Isometry<Real>) -> BoundingSphere {
        let bv: BoundingSphere = self.local_bounding_sphere();
        bv.transform_by(pos)
    }

    /// Computes the local-space bounding sphere of this convex polygon.
    #[inline]
    pub fn local_bounding_sphere(&self) -> BoundingSphere {
        bounding_volume::details::point_cloud_bounding_sphere(self.points())
    }
}
