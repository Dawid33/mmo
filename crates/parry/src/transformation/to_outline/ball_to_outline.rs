use crate::math::{Isometry, Point, Real, RawReal, Vector};
use crate::shape::Ball;
use crate::transformation::utils;
use alloc::vec::Vec;
use na::{self, Point3, RealField};

impl Ball {
    /// Outlines this ball’s shape using polylines.
    pub fn to_outline(&self, nsubdiv: u32) -> (Vec<Point3<Real>>, Vec<[u32; 2]>) {
        let diameter = self.radius * 2.0;
        let (vtx, idx) = unit_sphere_outline(nsubdiv);
        (utils::scaled(vtx, Vector::repeat(diameter)), idx)
    }
}

fn unit_sphere_outline(nsubdiv: u32) -> (Vec<Point3<Real>>, Vec<[u32; 2]>) {
    let two_pi = Real::two_pi();
    let dtheta = two_pi / Real::from(nsubdiv as RawReal);
    let mut coords = Vec::new();
    let mut indices = Vec::new();

    utils::push_circle(0.5.into(), nsubdiv, dtheta, 0.0.into(), &mut coords);
    utils::push_circle(0.5.into(), nsubdiv, dtheta, 0.0.into(), &mut coords);
    utils::push_circle(0.5.into(), nsubdiv, dtheta, 0.0.into(), &mut coords);

    let n = nsubdiv as usize;
    utils::transform(
        &mut coords[n..n * 2],
        Isometry::rotation(Vector::x() * Real::frac_pi_2()),
    );
    utils::transform(
        &mut coords[n * 2..n * 3],
        Isometry::rotation(Vector::z() * Real::frac_pi_2()),
    );

    utils::push_circle_outline_indices(&mut indices, 0..nsubdiv);
    utils::push_circle_outline_indices(&mut indices, nsubdiv..nsubdiv * 2);
    utils::push_circle_outline_indices(&mut indices, nsubdiv * 2..nsubdiv * 3);

    (coords, indices)
}

/// Creates an hemisphere with a radius of 0.5.
pub(crate) fn push_unit_hemisphere_outline(
    nsubdiv: u32,
    pts: &mut Vec<Point<Real>>,
    idx: &mut Vec<[u32; 2]>,
) {
    let base_idx = pts.len() as u32;
    let dtheta = Real::pi() / Real::from(nsubdiv as RawReal);
    let npoints = nsubdiv + 1;

    utils::push_circle(0.5.into(), npoints, dtheta, 0.0.into(), pts);
    utils::push_circle(0.5.into(), npoints, dtheta, 0.0.into(), pts);

    let n = npoints as usize;
    utils::transform(
        &mut pts[base_idx as usize..base_idx as usize + n],
        Isometry::rotation(Vector::x() * -Real::frac_pi_2()),
    );
    utils::transform(
        &mut pts[base_idx as usize + n..base_idx as usize + n * 2],
        Isometry::rotation(Vector::z() * Real::frac_pi_2())
            * Isometry::rotation(Vector::y() * Real::frac_pi_2()),
    );

    utils::push_open_circle_outline_indices(idx, base_idx..base_idx + npoints);
    utils::push_open_circle_outline_indices(idx, base_idx + npoints..base_idx + npoints * 2);
}
