//! Shared test helpers: a throwaway Postgres database per test (`pg-test-support`).
#![allow(dead_code)]

pub use pg_test_support::PgTestDb;

/// Whether pg tests are on (`MMO_TEST_DATABASE_URL` set).
pub fn pg_admin_url() -> Option<String> {
    pg_test_support::admin_url()
}

pub fn skip_msg() {
    eprintln!("{}", pg_test_support::SKIP_MSG);
}

/// The fixture RSA key (PKCS#8 PEM) and the JWKS publishing its public half as kid `test-1`.
pub const OIDC_KEY_PEM: &str = include_str!("../fixtures/oidc_rsa.pem");
pub const OIDC_JWKS: &str = include_str!("../fixtures/oidc_jwks.json");

/// Signs `claims` with the fixture key (RS256) under key id `kid`.
pub fn oidc_token(claims: serde_json::Value, kid: &str) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    jsonwebtoken::encode(&header, &claims, &jsonwebtoken::EncodingKey::from_rsa_pem(OIDC_KEY_PEM.as_bytes()).unwrap()).unwrap()
}
