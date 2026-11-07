//! All of `world-store`'s integration tests in ONE binary (`--test it <file>::`).
//! Postgres tests run only with `MMO_TEST_DATABASE_URL` set (`just pg-up`, `just test-pg`).

mod codec;
mod handle;
mod common;
mod open;
mod schema;
