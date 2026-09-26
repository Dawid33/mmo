//! Upstream nalgebra fixes carried in the vendored fork.
use game::na::Matrix4;

/// nalgebra #1109 / PR #1210: the 2×2 deflation basis lost all precision
/// when λ0 ≈ d1, so `recompose()` no longer reproduced the input.
#[test]
fn symmetric_eigen_regression_issue_1109() {
    #[rustfmt::skip]
    let m = Matrix4::new(
        -19884.07f64, -10.07188, 11.277279, -188560.63,
        -10.07188, 12.518197, 1.3770627, -102.97504,
        11.277279, 1.3770627, 14.587362, 113.26099,
        -188560.63, -102.97504, 113.26099, -1788112.3,
    );
    let eig = m.symmetric_eigen();
    let recomposed = eig.recompose().lower_triangle();
    // Upstream asserts `relative_eq!(.., epsilon = 1.0e-5)`: per element,
    // absolute 1e-5 or relative f64::EPSILON. Before the fix the error is ~11.6.
    for (a, b) in m.lower_triangle().iter().zip(recomposed.iter()) {
        let d = (a - b).abs();
        assert!(
            d <= 1.0e-5 || d <= f64::EPSILON * a.abs().max(b.abs()),
            "recompose error {d:e} at {a} vs {b}"
        );
    }
}
