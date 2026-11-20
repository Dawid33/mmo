//! The world panel (R5b, plan `docs/superpowers/plans/2026-11-15-r5b-world-panel.md`): an
//! operator page over the game worlds of one cluster — each world's state, pods,
//! health and metrics, its release history and forge Jobs — and the actions the
//! `world-*` recipes run: start, stop, forge.

pub mod forge;
pub mod kube;
pub mod metrics;
pub mod ops;
pub mod web;
pub mod world;
