use crate::parry::bounding_volume::Aabb;
use crate::parry::math::{Isometry, Real};
use crate::parry::shape::Voxels;

impl Voxels {
    /// Computes the world-space Aabb of this set of voxels, transformed by `pos`.
    #[inline]
    pub fn aabb(&self, pos: &Isometry<Real>) -> Aabb {
        self.chunk_bvh().root_aabb().transform_by(pos)
    }

    /// Computes the local-space Aabb of this set of voxels.
    #[inline]
    pub fn local_aabb(&self) -> Aabb {
        self.chunk_bvh().root_aabb()
    }
}
