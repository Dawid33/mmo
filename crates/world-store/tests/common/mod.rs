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
