use crate::parry::math::{Isometry, Point, Real, Rotation, Translation, Vector, DIM, RawReal};
use crate::parry::shape::Cuboid;

/// Computes an oriented bounding box for the given set of points.
///
/// The returned OBB is not guaranteed to be the smallest enclosing OBB.
/// Though it should be a pretty good on for most purposes.
pub fn obb(pts: &[Point<Real>]) -> (Isometry<Real>, Cuboid) {
    let cov = crate::parry::utils::cov(pts);
    let mut eigv = cov.symmetric_eigen().eigenvectors;

    if eigv.determinant() < 0.0.into() {
        eigv = -eigv;
    }

    let mut mins = Vector::repeat(Real::from(RawReal::MAX));
    let mut maxs = Vector::repeat(Real::from(-RawReal::MAX));

    for pt in pts {
        for i in 0..DIM {
            let dot = eigv.column(i).dot(&pt.coords);
            mins[i] = mins[i].min(dot);
            maxs[i] = maxs[i].max(dot);
        }
    }

    #[cfg(feature = "dim2")]
    let rot = Rotation::from_rotation_matrix(&na::Rotation2::from_matrix_unchecked(eigv));
    #[cfg(feature = "dim3")]
    let rot = Rotation::from_rotation_matrix(&na::Rotation3::from_matrix_unchecked(eigv));

    (
        rot * Translation::from((maxs + mins) / Real::from(2.0)),
        Cuboid::new((maxs - mins) / Real::from(2.0)),
    )
}
