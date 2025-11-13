use crate::parry::math::{Point, Real};
use crate::parry::query::gjk::VoronoiSimplex;
use crate::parry::query::{PointProjection, PointQuery};
use crate::parry::shape::{FeatureId, RoundShape, SupportMap};

// TODO: if PointQuery had a `project_point_with_normal` method, we could just
// call this and adjust the projected point accordingly.
impl<S: SupportMap> PointQuery for RoundShape<S> {
    #[inline]
    fn project_local_point(&self, point: &Point<Real>, solid: bool) -> PointProjection {
        #[cfg(not(feature = "alloc"))]
        return unimplemented!(
            "The projection of points on a round shape isn't supported without alloc yet."
        );

        #[cfg(feature = "alloc")]
        return crate::parry::query::details::local_point_projection_on_support_map(
            self,
            &mut VoronoiSimplex::new(),
            point,
            solid,
        );
    }

    #[inline]
    fn project_local_point_and_get_feature(
        &self,
        point: &Point<Real>,
    ) -> (PointProjection, FeatureId) {
        (self.project_local_point(point, false), FeatureId::Unknown)
    }
}
