//! The coordinator's durable projection (Elastic 2a, decision 5): everything a coordinator
//! process holds that is not a link, a cache or an observation timer, as two blobs the
//! runtime writes in ONE compare-and-set of the `coordinator` row — `core` (this module's
//! format) and `history` ([`crate::history::persist`], written only when it changed). A
//! process restarted from them, or a cache reloaded from them, behaves as the one that wrote
//! them ([`super::Coordinator::load`]). The projection's fields are listed in `mod.rs`'s
//! `projection!`; among them, beside the history's companions (claims, the memo, the calendar
//! anchor) and the hosts' view (`region_host`, `online_on`, hops), `boots_seen` (the boots each
//! host's `Attach`es named: a new one forgets the answers of the others) and
//! `attached_generation` (the generation of the latest first-of-its-generation `Attach`: the
//! next higher one finds every older host gone).
//!
//! Outside it, and so lost by a restart (each loss only delays a decision): the lease watch
//! ([`super::LeaseWatch`], on the process's monotonic clock, re-counted from zero), the counts
//! of its reads that refusals are stamped with, the store-retry and report pacing, the memo's
//! warning rate limit, the outgoing messages (a transition carries them; see [`Transition`]),
//! the input log (`input.rs`: what was applied since the last commit, replayed by a rebase)
//! and the login relay. The runtime's configuration (the cut period, the crash policy, the
//! lease dials, the hosts a cut expects, the world tick) is set again by every runtime that
//! loads it; the cut ids' high half (`WorldCuts.epoch`) is in the projection (Elastic 2b).

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{CoordMsg, StoreDecision, Target};
use crate::history::persist::HistoryLoadError;

/// The format byte leading an encoded core ([`encode_core`]). Worlds are re-forged, never
/// migrated (Dawid, 2026-11-15): a core of an older format is refused `OldVersion` (the
/// server refuses the start beside saved regions and says to re-forge); a newer or unknown one
/// is `Corrupt`. Formats 1 and 2 were the whole state in one blob (history, claims, memo); 3
/// still held the fortress claims (site → player); 4 stamped with a process boot and kept the
/// cut ids' high half out of the projection (Elastic 2b: a [`super::ReplicaId`], and
/// `WorldCuts.epoch` durable).
pub const COORDINATOR_STATE_VERSION: u8 = 5;

/// One transition of the core ([`super::Coordinator::take_transition`]): the projection as
/// bytes — `core`, and `history` when it changed — the messages it released, and the store
/// decisions it issues (Elastic 2b Task 3). The runtime writes the bytes (compare-and-set) and
/// runs the decisions in that same transaction, each in a savepoint after the row's update; it
/// sends `out` only once that write committed: every output, a store decision included, is
/// durable before it leaves.
#[derive(Debug, Clone)]
pub struct Transition {
    pub core: Vec<u8>,
    pub history: Option<Vec<u8>>,
    pub out: Vec<(Target, CoordMsg)>,
    pub decisions: Vec<StoreDecision>,
}

/// The bincode of the projection `body` (what a transition compares against the last one).
pub(super) fn raw_core(body: &impl Serialize) -> Vec<u8> {
    bincode::serialize(body).expect("the coordinator core serializes")
}

/// The core blob of `raw` ([`raw_core`]): the format byte, then its lz4 (as the history).
pub(super) fn seal_core(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![COORDINATOR_STATE_VERSION];
    out.extend(lz4_flex::compress_prepend_size(raw));
    out
}

/// A core sealed by [`seal_core`]: an older format is `OldVersion` (re-forge), any other
/// byte or a body that does not decode is `Corrupt`.
pub(super) fn decode_core<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, HistoryLoadError> {
    let corrupt = |e: String| HistoryLoadError::Corrupt(format!("coordinator core: {e}"));
    match bytes.split_first() {
        None => Err(corrupt("empty".into())),
        Some((&COORDINATOR_STATE_VERSION, body)) => {
            let raw = lz4_flex::decompress_size_prepended(body).map_err(|e| corrupt(e.to_string()))?;
            bincode::deserialize(&raw).map_err(|e| corrupt(e.to_string()))
        }
        Some((&v, _)) if v < COORDINATOR_STATE_VERSION => Err(HistoryLoadError::OldVersion { found: u32::from(v) }),
        Some((v, _)) => Err(corrupt(format!("format {v}; this build reads {COORDINATOR_STATE_VERSION}"))),
    }
}
