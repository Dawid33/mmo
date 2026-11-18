//! The in-memory standalone host: players with tags, a chest, a namespace store with latency and failure
//! injection ([`StubHost`]), its host-API description ([`stub_api`]), a [`Runner`] that drives a
//! `quest_vm::Vm` tick by tick, and a [`World`] that runs a quest's instances (its home and parts) with
//! [`People`], conversations and scenes. No filesystem, clock or thread: it builds for wasm32.
#![forbid(unsafe_code)]

mod api;
mod host;
mod namespace;
mod people;
mod runner;
mod world;

pub use api::*;
pub use host::{FailureKind, FailureRule, StubConfig, StubHost};
pub use people::{
    Entity, EntityKind, Input, People, id_of, parse_people, parse_script, parse_wire,
};
pub use runner::{RUNNER_EVENTS, RunOutcome, Runner, vm_config};
pub use world::{EditFn, Outcome, StepReport, World};
