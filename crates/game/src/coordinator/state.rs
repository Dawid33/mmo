//! The coordinator's saved state: the history, the claims and the answer memo — one blob,
//! committed as a unit, so a reply is never durable without the state it reports.

use serde::{Deserialize, Serialize};

use super::{Claim, RequestId, WorldReply};
use crate::history::persist::HistoryLoadError;
use crate::PlayerId;

/// The format byte leading an encoded [`CoordinatorState`]. A stored state of another
/// format is NOT migrated or replaced: the server refuses to start (`Corrupt`, fatal in
/// `ensure_coordinator_state`) — the state holds the world's people, claims and replies.
/// Bumping it therefore needs a migration of the old format (decode it, re-encode), or
/// every world saved before stops loading.
pub const COORDINATOR_STATE_VERSION: u8 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CoordinatorState {
    /// [`crate::history::persist::encode`] of the history.
    pub history: Vec<u8>,
    /// What each player plays.
    pub claims: Vec<(PlayerId, Claim)>,
    /// Answers of state-changing requests, oldest first.
    pub memo: Vec<(RequestId, WorldReply)>,
}

impl CoordinatorState {
    /// The format byte, then the bincode of the state.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![COORDINATOR_STATE_VERSION];
        out.extend(bincode::serialize(self).expect("coordinator state serializes"));
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HistoryLoadError> {
        match bytes.split_first() {
            None => Err(HistoryLoadError::Corrupt("empty coordinator state".into())),
            Some((&COORDINATOR_STATE_VERSION, body)) => {
                bincode::deserialize(body).map_err(|e| HistoryLoadError::Corrupt(e.to_string()))
            }
            Some((v, _)) => Err(HistoryLoadError::Corrupt(format!(
                "coordinator state format {v}, expected {COORDINATOR_STATE_VERSION}"
            ))),
        }
    }
}
