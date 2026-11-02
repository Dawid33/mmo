//! The client's predicted regions, run like the server's: each region is
//! driven only through a message pair ([`runner::RegionCmd`] in,
//! [`runner::RegionReport`] out), on the caller's thread (the inline host:
//! harnesses, wasm) or on one thread per region (native play).
//! Plan: `docs/superpowers/plans/2026-11-02-client-region-threads.md`.
pub mod clock;
pub mod runner;
pub mod host;
