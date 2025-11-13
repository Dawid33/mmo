use crate::parry::math::Real;
use crate::parry::query::gjk::VoronoiSimplex;
use crate::parry::query::{Ray, RayCast, RayIntersection};
use crate::parry::shape::{RoundShape, SupportMap};

impl<S: SupportMap> RayCast for RoundShape<S> {
    fn cast_local_ray_and_get_normal(
        &self,
        ray: &Ray,
        max_time_of_impact: Real,
        solid: bool,
    ) -> Option<RayIntersection> {
        crate::parry::query::details::local_ray_intersection_with_support_map_with_params(
            self,
            &mut VoronoiSimplex::new(),
            ray,
            max_time_of_impact,
            solid,
        )
    }
}
