//! Successions (R4): which hosts lost their lease. The coordinator declares a host dead
//! only from what it read itself — the same `seen_ms` over [`HOST_LEASE_MS`] of its own
//! clock, every read in between a successful one — so a database outage never looks like
//! every host dying. Pure: the runtime reads the `hosts` rows and passes its clock in.

use std::collections::BTreeMap;

use crate::{HOST_LEASE_MS, HOST_LIVE};

/// One `hosts` row as the lease watch needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSeen {
    pub ordinal: u32,
    pub boot: u64,
    pub clean: bool,
    /// `HOST_LIVE` or `HOST_DEAD`.
    pub state: i64,
    /// The database clock at its last beat; 0 = vouched dead by a boot of its ordinal.
    pub seen_ms: u64,
}

/// What the watch remembers of one ordinal's row.
#[derive(Clone, Debug)]
struct Watched {
    row: HostSeen,
    /// The coordinator's clock at the first read of this boot with this `seen_ms`.
    first_ms: u64,
    /// The coordinator's clock at the latest read of it.
    last_ms: u64,
}

/// The coordinator's lapse watch over the `hosts` rows (fed every
/// `WORLD_CUT_HOSTS_REFRESH_MS` by the runtime).
#[derive(Clone, Debug, Default)]
pub struct LeaseWatch {
    rows: BTreeMap<u32, Watched>,
}

impl LeaseWatch {
    /// A successful read of every `hosts` row at the coordinator's `now_ms`. A row whose
    /// boot or `seen_ms` changed starts its count again; a row no longer read is forgotten.
    pub fn observe(&mut self, rows: &[HostSeen], now_ms: u64) {
        let mut next = BTreeMap::new();
        for row in rows {
            let first_ms = match self.rows.get(&row.ordinal) {
                Some(w) if w.row.boot == row.boot && w.row.seen_ms == row.seen_ms => w.first_ms,
                _ => now_ms,
            };
            next.insert(row.ordinal, Watched { row: row.clone(), first_ms, last_ms: now_ms });
        }
        self.rows = next;
    }

    /// A read that failed: every observation is forgotten, so a lapse needs a whole lease
    /// of successful reads again (an outage is no evidence of anyone's death).
    pub fn observe_failed(&mut self) {
        self.rows.clear();
    }

    /// The `(ordinal, boot)` of every dirty live row that lapsed by the reads made up to
    /// `now_ms`: vouched (`seen_ms == 0`), or read with one `seen_ms` over reads spanning
    /// at least [`HOST_LEASE_MS`]. Time without a read counts for nothing. By ordinal.
    pub fn lapsed(&self, now_ms: u64) -> Vec<(u32, u64)> {
        self.rows
            .values()
            .filter(|w| !w.row.clean && w.row.state == HOST_LIVE)
            .filter(|w| w.row.seen_ms == 0 || w.last_ms.min(now_ms).saturating_sub(w.first_ms) >= HOST_LEASE_MS)
            .map(|w| (w.row.ordinal, w.row.boot))
            .collect()
    }
}

impl super::Coordinator {
    /// A successful read of the `hosts` rows at `now_ms` ([`LeaseWatch::observe`]).
    pub fn observe_leases(&mut self, rows: &[HostSeen], now_ms: u64) {
        self.leases.observe(rows, now_ms);
    }

    /// A failed read of the `hosts` rows ([`LeaseWatch::observe_failed`]).
    pub fn leases_unread(&mut self) {
        self.leases.observe_failed();
    }

    /// The lease watch (Task 8 declares successions from [`LeaseWatch::lapsed`]).
    pub fn leases(&self) -> &LeaseWatch {
        &self.leases
    }
}
