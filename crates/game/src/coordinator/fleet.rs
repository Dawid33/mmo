//! Elastic 4: the hosts the world wants — the coordinator decides how many exist. The table is
//! `desired_hosts` (`world_store` `fleet.rs`), written only by the store decisions
//! [`super::AddHost`], [`super::DrainHost`] and [`super::RemoveHost`] inside the coordinator's
//! transition; the reconciler reads it and makes one host Deployment per row.

use serde::{Deserialize, Serialize};

/// One `desired_hosts` row: a host the coordinator wants to exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredHost {
    /// `h<n>`, never issued twice (`world.next_desired_host`): the host's Deployment name.
    pub name: String,
    /// The lowest slot free at its add; reused once its row is removed.
    pub slot: u32,
    /// Marked draining ([`super::DrainHost`]): it leaves, and still holds its slot until removed.
    pub draining: bool,
    /// The store's clock (ms since the epoch) at its add.
    pub created_ms: u64,
}
