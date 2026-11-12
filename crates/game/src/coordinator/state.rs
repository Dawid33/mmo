//! The coordinator's saved state: the history, the claims and the answer memo — one blob,
//! committed as a unit, so a reply is never durable without the state it reports.

use serde::{Deserialize, Serialize};

use super::{Claim, RequestId, WorldReply};
use crate::history::persist::HistoryLoadError;
use crate::PlayerId;

/// The format byte leading an encoded [`CoordinatorState`]. An older format is migrated on
/// load and written back in the current one at the next save; a newer or unknown one
/// refuses the start (`Corrupt`, fatal in `ensure_coordinator_state`) — the state holds the
/// world's people, claims and replies, so it is never replaced. Bumping it needs a frozen
/// copy of the old layout (`CoordinatorStateV<n>`), a `migrate_v<n>`, a rung in
/// [`CoordinatorState::decode`] and a committed fixture of the old blob.
pub const COORDINATOR_STATE_VERSION: u8 = 1;
const _: () = assert!(
    COORDINATOR_STATE_VERSION == 1,
    "add the current-format arm to CoordinatorState::decode and a migrate step"
);

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

    /// Decodes through the version ladder: every format this build knows loads (older ones
    /// migrated to the current shape); another one is `Corrupt`.
    pub fn decode(bytes: &[u8]) -> Result<Self, HistoryLoadError> {
        let corrupt = |e: bincode::Error| HistoryLoadError::Corrupt(e.to_string());
        match bytes.split_first() {
            None => Err(HistoryLoadError::Corrupt("empty coordinator state".into())),
            Some((&1, body)) => bincode::deserialize::<CoordinatorStateV1>(body).map(migrate_v1).map_err(corrupt),
            // At the first bump: add `COORDINATOR_STATE_VERSION => decode_current(body)` and keep `1 => … migrate_v1`.
            Some((v, _)) => Err(HistoryLoadError::Corrupt(format!(
                "coordinator state format {v}; this build reads 1..={COORDINATOR_STATE_VERSION}"
            ))),
        }
    }
}

/// Format 1, frozen: a copy of the layout, not an alias of [`CoordinatorState`] (which may
/// change). Pinned by `tests/fixtures/coordinator_state_v1.bin`. Only the top level is frozen:
/// a layout change to `PlayerId`, `Claim`, `RequestId` or `WorldReply` is a format bump and
/// needs frozen copies of those types here.
#[derive(Deserialize)]
struct CoordinatorStateV1 {
    history: Vec<u8>,
    claims: Vec<(PlayerId, Claim)>,
    memo: Vec<(RequestId, WorldReply)>,
}

fn migrate_v1(v1: CoordinatorStateV1) -> CoordinatorState {
    let CoordinatorStateV1 { history, claims, memo } = v1;
    CoordinatorState { history, claims, memo }
}
