use bevy::math::{Quat, Vec3, Vec4};
use bevy::prelude::{PerspectiveProjection, Transform};
use game::camera::Perspective3;
use game::Pose;

/// Sim pose (right-handed, Y-up — same convention as bevy) → bevy Transform.
/// The sim's glam (0.33, through rapier's glamx) is not Bevy's (0.32): the
/// types only look alike, so convert through arrays.
pub fn iso_to_transform(iso: &Pose) -> Transform {
    Transform {
        translation: Vec3::from_array(iso.translation.to_array()),
        rotation: Quat::from_array(iso.rotation.to_array()),
        scale: Vec3::ONE,
    }
}

pub fn perspective_to_projection(p: &Perspective3) -> PerspectiveProjection {
    let near = p.znear();
    PerspectiveProjection {
        fov: p.fovy(),
        aspect_ratio: p.aspect(),
        near,
        far: p.zfar(),
        near_clip_plane: Vec4::new(0.0, 0.0, -1.0, -near),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_iso_is_identity_transform() {
        let iso = game::Pose::IDENTITY;
        let t = iso_to_transform(&iso);
        assert_eq!(t, Transform::IDENTITY);
    }

    #[test]
    fn translation_maps_componentwise() {
        let iso = game::Pose::translation(1.0, 2.0, -3.0);
        let t = iso_to_transform(&iso);
        assert_eq!(t.translation, Vec3::new(1.0, 2.0, -3.0));
    }

    #[test]
    fn rotation_maps_to_equivalent_quat() {
        let iso = game::Pose::from_rotation(game::yaw_quat(1.0));
        let t = iso_to_transform(&iso);
        let expected = Quat::from_axis_angle(Vec3::Y, 1.0);
        assert!(t.rotation.angle_between(expected) < 1e-6);
    }

    #[test]
    fn perspective_fields_carry_over() {
        let p = Perspective3::new(16.0 / 9.0, 1.2, 0.1, 100.0);
        let proj = perspective_to_projection(&p);
        // Note: znear/zfar are computed from the projection matrix, introducing precision loss
        assert!((proj.fov - 1.2).abs() < 1e-4);
        assert!((proj.near - 0.1).abs() < 1e-4);
        assert!((proj.far - 100.0).abs() < 1e-4);
        assert!((proj.aspect_ratio - 16.0 / 9.0).abs() < 1e-4);
    }
}
