use bevy::math::{Quat, Vec3, Vec4};
use bevy::prelude::{PerspectiveProjection, Transform};
use game::Pose;
use game::camera::Perspective3;

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
