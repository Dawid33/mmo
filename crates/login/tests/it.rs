//! All of `login`'s integration tests in ONE binary (`--test it <file>::`).
//! Postgres tests run only with `MMO_TEST_DATABASE_URL` set (`just pg-up`, `just test-login-pg`).

mod api;
mod common;
mod ctl;
mod kicks;
mod oidc;
mod schema;
mod store;
