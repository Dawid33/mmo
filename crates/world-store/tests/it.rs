//! All of `world-store`'s integration tests in ONE binary (`--test it <file>::`).
//! Postgres tests run only with `MMO_TEST_DATABASE_URL` set (`just pg-up`, `just test-pg`).

mod clock;
mod counters;
mod codec;
mod cut;
mod fence;
mod handle;
mod hosts;
mod notify;
mod common;
mod forged;
mod open;
mod players;
mod schema;
mod studs;
mod two_phase;
mod voxel_refusal;
mod worldctl;
