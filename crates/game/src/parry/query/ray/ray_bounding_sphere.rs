use crate::parry::bounding_volume::BoundingSphere;
use crate::parry::math::Real;
use crate::parry::query::{Ray, RayCast, RayIntersection};
use crate::parry::shape::Ball;

impl RayCast for BoundingSphere {
    #[inline]
    fn cast_local_ray(&self, ray: &Ray, max_time_of_impact: Real, solid: bool) -> Option<Real> {
        let centered_ray = ray.translate_by(-self.center().coords);
        Ball::new(self.radius()).cast_local_ray(&centered_ray, max_time_of_impact, solid)
    }

    #[inline]
    fn cast_local_ray_and_get_normal(
        &self,
        ray: &Ray,
        max_time_of_impact: Real,
        solid: bool,
    ) -> Option<RayIntersection> {
        let centered_ray = ray.translate_by(-self.center().coords);
        Ball::new(self.radius()).cast_local_ray_and_get_normal(
            &centered_ray,
            max_time_of_impact,
            solid,
        )
    }

    #[inline]
    fn intersects_local_ray(&self, ray: &Ray, max_time_of_impact: Real) -> bool {
        let centered_ray = ray.translate_by(-self.center().coords);
        Ball::new(self.radius()).intersects_local_ray(&centered_ray, max_time_of_impact)
    }
}
