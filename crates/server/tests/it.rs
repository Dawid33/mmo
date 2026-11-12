//! The server's integration tests that need no process of their own, in ONE
//! binary (`--test it <file>::`). The other files under `tests/` keep their
//! own `[[test]]` targets (see `Cargo.toml`).

mod auth_edge;
mod hooks;
mod jwt_admission;
mod stud_world;
