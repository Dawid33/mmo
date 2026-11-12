//! A `jwt` host without the login link's service token refuses to start unless the
//! operator names the dev-only fallback.

use server::login_link::{jwt_admission, JwtAdmission};

#[test]
fn token_means_spawn_orders() {
    assert_eq!(jwt_admission(true, None), Ok(JwtAdmission::SpawnOrders));
}

#[test]
fn no_token_and_newest_is_the_dev_fallback() {
    assert_eq!(jwt_admission(false, Some("newest")), Ok(JwtAdmission::Newest));
}

#[test]
fn no_token_and_nothing_else_is_refused() {
    let err = jwt_admission(false, None).unwrap_err();
    assert!(err.contains("VOXEL_SERVICE_TOKEN") && err.contains("VOXEL_JWT_ADMISSION=newest"), "{err}");
}

#[test]
fn token_with_newest_is_a_conflict() {
    let err = jwt_admission(true, Some("newest")).unwrap_err();
    assert!(err.contains("VOXEL_JWT_ADMISSION") && err.contains("VOXEL_SERVICE_TOKEN"), "{err}");
}
