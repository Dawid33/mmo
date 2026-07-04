use bevy::math::{Quat, Vec3, Vec4};
use bevy::prelude::{PerspectiveProjection, Transform};
use game::na::Perspective3;
use game::parry::math::Real;
use game::IsometryReal;

/// Sim isometry (right-handed, Y-up — same convention as bevy) → bevy Transform.
pub fn iso_to_transform(iso: &IsometryReal) -> Transform {
    Transform {
        translation: Vec3::new(
            iso.translation.vector.x.into_inner(),
            iso.translation.vector.y.into_inner(),
            iso.translation.vector.z.into_inner(),
        ),
        rotation: Quat::from_xyzw(
            iso.rotation.i.into_inner(),
            iso.rotation.j.into_inner(),
            iso.rotation.k.into_inner(),
            iso.rotation.w.into_inner(),
        ),
        scale: Vec3::ONE,
    }
}

pub fn perspective_to_projection(p: &Perspective3<Real>) -> PerspectiveProjection {
    let near = p.znear().into_inner();
    PerspectiveProjection {
        fov: p.fovy().into_inner(),
        aspect_ratio: p.aspect().into_inner(),
        near,
        far: p.zfar().into_inner(),
        near_clip_plane: Vec4::new(0.0, 0.0, -1.0, -near),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::na::{Perspective3, Translation3, UnitQuaternion, Vector3};
    use game::parry::math::Real;
    use ordered_float::OrderedFloat;

    #[test]
    fn identity_iso_is_identity_transform() {
        let iso = game::IsometryReal::identity();
        let t = iso_to_transform(&iso);
        assert_eq!(t, Transform::IDENTITY);
    }

    #[test]
    fn translation_maps_componentwise() {
        let iso = game::IsometryReal::from_parts(
            Translation3::new(OrderedFloat(1.0), OrderedFloat(2.0), OrderedFloat(-3.0)),
            UnitQuaternion::identity(),
        );
        let t = iso_to_transform(&iso);
        assert_eq!(t.translation, Vec3::new(1.0, 2.0, -3.0));
    }

    #[test]
    fn rotation_maps_to_equivalent_quat() {
        let rot = UnitQuaternion::from_axis_angle(&Vector3::y_axis(), Real::from(1.0));
        let iso = game::IsometryReal::from_parts(Translation3::identity(), rot);
        let t = iso_to_transform(&iso);
        let expected = Quat::from_axis_angle(Vec3::Y, 1.0);
        assert!(t.rotation.angle_between(expected) < 1e-6);
    }

    #[test]
    fn perspective_fields_carry_over() {
        let p = Perspective3::new(
            Real::from(16.0 / 9.0),
            Real::from(1.2),
            Real::from(0.1),
            Real::from(100.0),
        );
        let proj = perspective_to_projection(&p);
        // Note: znear/zfar are computed from the projection matrix, introducing precision loss
        assert!((proj.fov - 1.2).abs() < 1e-4);
        assert!((proj.near - 0.1).abs() < 1e-4);
        assert!((proj.far - 100.0).abs() < 1e-4);
        assert!((proj.aspect_ratio - 16.0 / 9.0).abs() < 1e-4);
    }
}
