//! The coordinator's replicas (Elastic 2b): several coordinator processes over ONE
//! `coordinator` row, each a cache of the row that writes its transitions with a
//! compare-and-set and, when the row moved under it, rebases onto the newer row and replays
//! what it applied since its last commit (`input.rs`). Until the live set arrives (Task 4) a
//! replica is only its id.

use serde::{Deserialize, Serialize};

/// A coordinator replica: one process start, never reused, nothing derived from its number
/// (Elastic 2b replaces Elastic 2a's process boot with it). The runtime draws a random one at
/// start; tests pick one. An attachment stamped with another replica is not this replica's (a
/// restart's projection holds the dead process's), and a store call handed out by another
/// replica is not in flight here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReplicaId(pub u64);
