//! The in-memory standalone host: players with tags, a chest, a namespace store with latency and failure
//! injection ([`StubHost`]), its host-API description ([`stub_api`]) and a [`Runner`] that drives a
//! `quest_vm::Vm` tick by tick. No filesystem, clock or thread: it builds for wasm32.
#![forbid(unsafe_code)]

mod api;
mod host;
mod namespace;
mod runner;

pub use api::*;
pub use host::{FailureKind, FailureRule, StubConfig, StubHost};
pub use runner::{RunOutcome, Runner};
