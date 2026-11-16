//! The world coordinator's row (Elastic 2a): its projection — `core` and `history` — written
//! with a compare-and-set on `version` under the claim's `epoch`. Both engines.

use world_store::{StoreError, StoredCoordinator};

use crate::handle::{each_backend, open};

fn stored(version: u64, core: &[u8], history: &[u8]) -> Option<StoredCoordinator> {
    Some(StoredCoordinator { version, core: core.to_vec(), history: history.to_vec() })
}

/// A write naming a version the row has moved past is refused `Conflict` (the epoch is
/// still this claim's: a second writer), and writes nothing; the current version writes.
#[test]
fn a_write_with_a_stale_version_conflicts() {
    each_backend("coord_rows_version", false, |url| {
        let h = open(url);
        let fence = h.claim_coordinator().unwrap();
        let first = h.write_coordinator_later(fence, 0, vec![1], Some(vec![10])).recv().unwrap().unwrap();
        assert_eq!(first.version, 1);
        assert!(first.store_ms > 1_600_000_000_000, "the database clock, ms since the epoch: {}", first.store_ms);
        let second = h.write_coordinator_later(fence, 1, vec![2], None).recv().unwrap().unwrap();
        assert_eq!(second.version, 2);
        // A writer that read version 1 (the other wrote 2 since).
        let err = h.write_coordinator_later(fence, 1, vec![3], Some(vec![30])).recv().unwrap().unwrap_err();
        assert_eq!(err, StoreError::Conflict);
        assert_eq!(h.load_coordinator().unwrap(), stored(2, &[2], &[10]), "the conflicting write wrote nothing");
        // A version from the future conflicts too.
        assert_eq!(h.write_coordinator_later(fence, 9, vec![4], None).recv().unwrap().unwrap_err(), StoreError::Conflict);
        h.write_coordinator_later(fence, 2, vec![5], None).recv().unwrap().unwrap();
        assert_eq!(h.load_coordinator().unwrap(), stored(3, &[5], &[10]));
        h.shutdown();
    });
}

/// A write under a claim another claim replaced is `Fenced`, whatever version it names —
/// the current one included — and writes nothing.
#[test]
fn a_write_with_a_stale_epoch_is_fenced() {
    each_backend("coord_rows_epoch", false, |url| {
        let h = open(url);
        let old = h.claim_coordinator().unwrap();
        h.write_coordinator_later(old, 0, vec![1], Some(vec![10])).recv().unwrap().unwrap();
        let new = h.claim_coordinator().unwrap();
        assert_eq!(new.epoch, old.epoch + 1);
        assert_eq!(h.load_coordinator().unwrap(), stored(1, &[1], &[10]), "a claim keeps the projection and its version");
        for version in [0, 1, 2] {
            let err = h.write_coordinator_later(old, version, vec![2], Some(vec![20])).recv().unwrap().unwrap_err();
            assert_eq!(err, StoreError::Fenced { sr: None }, "version {version}");
        }
        assert_eq!(h.load_coordinator().unwrap(), stored(1, &[1], &[10]), "the fenced writes wrote nothing");
        let w = h.write_coordinator_later(new, 1, vec![3], None).recv().unwrap().unwrap();
        assert_eq!(w.version, 2);
        h.shutdown();
    });
}

/// A write without a history keeps the stored one (`COALESCE`); one with a history replaces it.
#[test]
fn history_is_kept_when_not_written() {
    each_backend("coord_rows_history", false, |url| {
        let h = open(url);
        let fence = h.claim_coordinator().unwrap();
        h.write_coordinator_later(fence, 0, vec![1], Some(vec![10, 11])).recv().unwrap().unwrap();
        h.write_coordinator_later(fence, 1, vec![2], None).recv().unwrap().unwrap();
        h.write_coordinator_later(fence, 2, vec![3], None).recv().unwrap().unwrap();
        assert_eq!(h.load_coordinator().unwrap(), stored(3, &[3], &[10, 11]), "the core moves on, the history stays");
        h.write_coordinator_later(fence, 3, vec![4], Some(vec![40])).recv().unwrap().unwrap();
        assert_eq!(h.load_coordinator().unwrap(), stored(4, &[4], &[40]));
        h.shutdown();
    });
}
