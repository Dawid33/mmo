//! All of `world-store`'s integration tests in ONE binary (`--test it <file>::`).
//! Postgres tests run only with `MMO_TEST_DATABASE_URL` set (`just pg-up`, `just test-pg`).

mod clock;
mod counters;
mod codec;
mod cut;
mod entity_key;
mod fence;
mod directory;
mod handle;
mod hosts;
mod lease;
mod notify;
mod common;
mod forged;
mod open;
mod players;
mod schema;
mod stud_refusal;
mod succession;
mod two_phase;
mod worldctl;
