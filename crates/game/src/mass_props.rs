//! Deterministic explicit mass properties for dynamic bodies.
//!
//! A dynamic body's mass is never derived from its colliders: they are built
//! with density 0, and the body carries [`mass_properties`] of its
//! [`ColliderSpec`] as rapier's ADDITIONAL mass properties. With massless
//! colliders parry's `Add` short-circuits on the zero side, so rapier's
//! recompute hands those properties back bit-unchanged — no glamx
//! `SymmetricEigen3` (std `acos`/`cos`, which differ between native and
//! wasm32) on any sim path. The same value then crosses a seam inside
//! [`crate::BodyState`] and is re-applied as-is.
//!
//! Primitives are analytic (the formulas parry uses, in the same operation
//! order); a compound sums its parts' inertia tensors with the parallel-axis
//! shift in part order and takes its principal frame from
//! [`jacobi_eigen_sym3`]: a fixed number of cyclic Jacobi sweeps using only
//! `+ − × ÷` and `libm` square roots. Never `MassProperties::with_inertia_matrix`
//! or `Add` of two non-zero properties — both diagonalise through glamx.
use parry3d::mass_properties::MassProperties;
use rapier3d::math::{Rotation, Vector};

use crate::ColliderSpec;

/// Density of a dynamic body when nothing more specific is known, in kg per
/// cubic world unit: a wood-like ~600 kg/m³ at 16 units per metre
/// (600 / 16³).
pub const BODY_DEFAULT_DENSITY: f32 = 600.0 / (16.0 * 16.0 * 16.0);

/// Cyclic Jacobi sweeps over the three off-diagonal pairs. Fixed, so the
/// result never depends on a convergence test; a 3×3 converges
/// quadratically and is at f64 round-off well before 8.
pub const JACOBI_SWEEPS: usize = 8;

/// The mass properties of `spec` at uniform `density`, in the body frame (a
/// compound's parts at their poses). Pure and cross-target deterministic.
///
/// # Panics
/// On a compound nested in a compound (parts are primitives only).
pub fn mass_properties(spec: &ColliderSpec, density: f32) -> MassProperties {
    match spec {
        ColliderSpec::Compound(parts) => compound(parts, density),
        _ => {
            let (mass, inertia) = primitive(spec, density);
            MassProperties::new(Vector::ZERO, mass, inertia)
        }
    }
}

/// Mass and principal inertia of a primitive about its own centre, along
/// its own axes (a Y capsule's, a cuboid's and a ball's principal frame is
/// the identity). Same formulas and operation order as parry's.
fn primitive(spec: &ColliderSpec, density: f32) -> (f32, Vector) {
    use core::f32::consts::PI;
    match *spec {
        ColliderSpec::Cuboid { half_extents: h } => {
            let volume = h.x * h.y * h.z * 8.0;
            let ix = (h.x * h.x) / 3.0;
            let iy = (h.y * h.y) / 3.0;
            let iz = (h.z * h.z) / 3.0;
            let mass = volume * density;
            (mass, Vector::new(iy + iz, ix + iz, ix + iy) * mass)
        }
        ColliderSpec::Ball { radius: r } => {
            let volume = PI * r * r * r * 4.0 / 3.0;
            let i = r * r * 2.0 / 5.0;
            let mass = volume * density;
            (mass, Vector::splat(i) * mass)
        }
        ColliderSpec::CapsuleY {
            half_height,
            radius: r,
        } => {
            let cyl_vol = half_height * r * r * PI * 2.0;
            let sq_radius = r * r;
            let sq_height = half_height * half_height * 4.0;
            let off_principal = (sq_radius * 3.0 + sq_height) / 12.0;
            let cyl_unit_i = Vector::new(off_principal, sq_radius / 2.0, off_principal);
            let ball_vol = PI * r * r * r * 4.0 / 3.0;
            let ball_unit_i = Vector::splat(r * r * 2.0 / 5.0);
            let mass = (cyl_vol + ball_vol) * density;
            let mut i = (cyl_unit_i * cyl_vol + ball_unit_i * ball_vol) * density;
            let h = half_height * 2.0;
            let extra = (h * h * 0.25 + h * r * 3.0 / 8.0) * ball_vol * density;
            i.x += extra;
            i.z += extra;
            (mass, i)
        }
        ColliderSpec::Compound(_) => panic!("mass_props: a compound's parts must be primitives"),
    }
}

/// Parts' tensors summed about the common centre of mass (f64, in part
/// order), then diagonalised by [`jacobi_eigen_sym3`].
fn compound(parts: &[(rapier3d::math::Pose, ColliderSpec)], density: f32) -> MassProperties {
    let prims: Vec<(f64, [f64; 3], [[f64; 3]; 3])> = parts
        .iter()
        .map(|(pose, spec)| {
            let (mass, principal) = primitive(spec, density);
            // R · diag · Rᵀ: the part's tensor about its centre, body axes.
            let r = rotation_matrix(pose.rotation);
            let d = [principal.x, principal.y, principal.z].map(f64::from);
            let mut t = [[0.0f64; 3]; 3];
            for (i, row) in t.iter_mut().enumerate() {
                for (j, v) in row.iter_mut().enumerate() {
                    *v = r[i][0] * d[0] * r[j][0]
                        + r[i][1] * d[1] * r[j][1]
                        + r[i][2] * d[2] * r[j][2];
                }
            }
            let c = pose.translation;
            (f64::from(mass), [c.x, c.y, c.z].map(f64::from), t)
        })
        .collect();
    let total: f64 = prims.iter().map(|p| p.0).sum();
    if total <= 0.0 {
        return MassProperties::default();
    }
    let mut com = [0.0f64; 3];
    for (m, c, _) in &prims {
        for a in 0..3 {
            com[a] += m * c[a];
        }
    }
    let com = com.map(|v| v / total);
    let mut tensor = [[0.0f64; 3]; 3];
    for (m, c, t) in &prims {
        let d = [c[0] - com[0], c[1] - com[1], c[2] - com[2]];
        let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        for i in 0..3 {
            for j in 0..3 {
                let delta = if i == j { dd } else { 0.0 };
                tensor[i][j] += t[i][j] + m * (delta - d[i] * d[j]);
            }
        }
    }
    let (values, frame) = jacobi_f64(tensor);
    MassProperties::with_principal_inertia_frame(
        Vector::new(com[0] as f32, com[1] as f32, com[2] as f32),
        total as f32,
        values,
        frame,
    )
}

/// Row-major rotation matrix of a unit quaternion (arithmetic only).
fn rotation_matrix(q: Rotation) -> [[f64; 3]; 3] {
    let (x, y, z, w) = (
        f64::from(q.x),
        f64::from(q.y),
        f64::from(q.z),
        f64::from(q.w),
    );
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

/// Eigen-decomposition of a symmetric 3×3 (row-major `m[row][col]`; only
/// the upper triangle is read): `(principal values, frame)` with
/// `m = R · diag(values) · Rᵀ`, `R` the frame's matrix, right-handed.
/// Deterministic on every target: [`JACOBI_SWEEPS`] cyclic sweeps in the
/// fixed pair order (0,1), (0,2), (1,2), `+ − × ÷` and `libm::sqrt` only.
pub fn jacobi_eigen_sym3(m: [[f32; 3]; 3]) -> (Vector, Rotation) {
    jacobi_f64(m.map(|row| row.map(f64::from)))
}

fn jacobi_f64(m: [[f64; 3]; 3]) -> (Vector, Rotation) {
    let mut a = [
        [m[0][0], m[0][1], m[0][2]],
        [m[0][1], m[1][1], m[1][2]],
        [m[0][2], m[1][2], m[2][2]],
    ];
    let mut v = [[1.0f64, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..JACOBI_SWEEPS {
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            let apq = a[p][q];
            if apq == 0.0 {
                continue;
            }
            // Rotation zeroing a[p][q] (Numerical Recipes' stable form).
            let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
            let sign = if theta < 0.0 { -1.0 } else { 1.0 };
            let t = sign / (theta * sign + libm::sqrt(theta * theta + 1.0));
            let c = 1.0 / libm::sqrt(t * t + 1.0);
            let s = t * c;
            // A <- Jᵀ A J, with J the identity but J[p][p] = J[q][q] = c,
            // J[p][q] = s, J[q][p] = −s.
            for k in 0..3 {
                let (akp, akq) = (a[k][p], a[k][q]);
                a[k][p] = c * akp - s * akq;
                a[k][q] = s * akp + c * akq;
            }
            for k in 0..3 {
                let (apk, aqk) = (a[p][k], a[q][k]);
                a[p][k] = c * apk - s * aqk;
                a[q][k] = s * apk + c * aqk;
            }
            a[p][q] = 0.0;
            a[q][p] = 0.0;
            // V <- V J: V's columns are the eigenvectors.
            for row in &mut v {
                let (vp, vq) = (row[p], row[q]);
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    let det = v[0][0] * (v[1][1] * v[2][2] - v[1][2] * v[2][1])
        - v[0][1] * (v[1][0] * v[2][2] - v[1][2] * v[2][0])
        + v[0][2] * (v[1][0] * v[2][1] - v[1][1] * v[2][0]);
    if det < 0.0 {
        for row in &mut v {
            row[2] = -row[2];
        }
    }
    let col =
        |j: usize| rapier3d::math::Vector::new(v[0][j] as f32, v[1][j] as f32, v[2][j] as f32);
    let frame = Rotation::from_mat3(&glamx::Mat3::from_cols(col(0), col(1), col(2)));
    (
        Vector::new(a[0][0] as f32, a[1][1] as f32, a[2][2] as f32),
        frame,
    )
}
