//! The world coordinator's row (Elastic 2a, 2b): its projection — `core` and `history` —
//! bootstrapped once (first writer wins) and written with a compare-and-set on `version`; the
//! store decisions a transition carries run inside that same transaction (Elastic 2b Task 3),
//! each in a savepoint after the row's update. Both engines unless noted; decisions need
//! Postgres.

use crate::common::hid;
use game::coordinator::{CallStamp, CutDecide, Declare, DecisionAnswer, ReplicaId, StoreDecision};
use game::{CutId, SuccessionKind};
use world_store::{AfterCommit, Bootstrapped, Declared, HOST_LIVE, StoreError, StoreHandle, StoredCoordinator, TransitionWrite, TransitionWritten};

use crate::handle::{each_backend, open};
use crate::succession::{World, boot_of, part};
use crate::two_phase::prepared;

/// The row as `(version, core, history)` (its `history_version` checked on its own).
fn stored(version: u64, core: &[u8], history: &[u8]) -> Option<(u64, Vec<u8>, Vec<u8>)> {
    Some((version, core.to_vec(), history.to_vec()))
}

fn row(s: StoredCoordinator) -> (u64, Vec<u8>, Vec<u8>) {
    (s.version, s.core, s.history)
}

/// A transition of replica 1 at `version`: `core`, `history`, `decisions`.
fn transition(version: u64, core: &[u8], history: Option<&[u8]>, decisions: Vec<StoreDecision>) -> TransitionWrite {
    TransitionWrite {
        version,
        core: core.to_vec(),
        history: history.map(<[u8]>::to_vec),
        decisions,
        replica: ReplicaId(1),
        ..Default::default()
    }
}

fn write(h: &StoreHandle, w: TransitionWrite) -> Result<TransitionWritten, StoreError> {
    h.write_transition_later(w).recv().unwrap()
}

/// The first replica to bootstrap the row writes it at version 1 (epoch 1); every later one
/// finds it there and writes nothing.
#[test]
fn bootstrap_is_first_writer_wins() {
    each_backend("coord_rows_bootstrap", true, |url| {
        let h = open(url);
        assert_eq!(h.load_coordinator().unwrap(), None);
        assert_eq!(h.bootstrap_coordinator(vec![1], vec![10]).unwrap(), Bootstrapped::Written { version: 1, epoch: 1 });
        // Another process (SQLite: after this one, its file is locked).
        h.shutdown();
        let other = open(url);
        assert_eq!(other.bootstrap_coordinator(vec![2], vec![20]).unwrap(), Bootstrapped::Exists);
        assert_eq!(other.load_coordinator().unwrap().map(row), stored(1, &[1], &[10]), "the second bootstrap wrote nothing");
        other.shutdown();
    });
}

/// A write naming a version the row has moved past is refused `Conflict` (a second writer),
/// and writes nothing; the current version writes.
#[test]
fn a_write_with_a_stale_version_conflicts() {
    each_backend("coord_rows_version", false, |url| {
        let h = open(url);
        h.bootstrap_coordinator(vec![0], vec![10]).unwrap();
        let first = write(&h, transition(1, &[1], None, vec![])).unwrap();
        assert_eq!(first.version, 2);
        assert!(first.store_ms > 1_600_000_000_000, "the database clock, ms since the epoch: {}", first.store_ms);
        assert!(first.answers.is_empty() && first.after_commit.is_empty());
        let second = write(&h, transition(2, &[2], None, vec![])).unwrap();
        assert_eq!(second.version, 3);
        // A writer that read version 2 (the other wrote 3 since).
        assert_eq!(write(&h, transition(2, &[3], Some(&[30]), vec![])).unwrap_err(), StoreError::Conflict);
        assert_eq!(h.load_coordinator().unwrap().map(row), stored(3, &[2], &[10]), "the conflicting write wrote nothing");
        // A version from the future conflicts too.
        assert_eq!(write(&h, transition(9, &[4], None, vec![])).unwrap_err(), StoreError::Conflict);
        write(&h, transition(3, &[5], None, vec![])).unwrap();
        assert_eq!(h.load_coordinator().unwrap().map(row), stored(4, &[5], &[10]));
        h.shutdown();
    });
}

/// A write without a history keeps the stored one (`COALESCE`); one with a history replaces it.
#[test]
fn history_is_kept_when_not_written() {
    each_backend("coord_rows_history", false, |url| {
        let h = open(url);
        h.bootstrap_coordinator(vec![1], vec![10, 11]).unwrap();
        write(&h, transition(1, &[2], None, vec![])).unwrap();
        write(&h, transition(2, &[3], None, vec![])).unwrap();
        assert_eq!(h.load_coordinator().unwrap().map(row), stored(3, &[3], &[10, 11]), "the core moves on, the history stays");
        write(&h, transition(3, &[4], Some(&[40]), vec![])).unwrap();
        assert_eq!(h.load_coordinator().unwrap().map(row), stored(4, &[4], &[40]));
        h.shutdown();
    });
}

/// SQLite runs one host and no decision: a transition carrying one is refused `Unsupported`,
/// and nothing is written.
#[test]
fn a_decision_on_sqlite_is_unsupported() {
    let h = open(&world_store::StoreUrl::Memory);
    h.bootstrap_coordinator(vec![1], vec![10]).unwrap();
    let begin = StoreDecision::BeginCut { cut: CutId(1) };
    let err = write(&h, transition(1, &[2], None, vec![begin])).unwrap_err();
    assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
    assert_eq!(h.load_coordinator().unwrap().map(row), stored(1, &[1], &[10]), "nothing written");
    h.shutdown();
}

/// Review Focus 1: a decision rides in its transition's transaction — a transition at a stale
/// version is a `Conflict`, and its declaration wrote nothing: no `successions` row, no dead
/// `hosts` row, no `super_regions` moved. At the row's version the same declaration commits
/// with the core, its answer stamped with the version it committed as.
#[test]
fn a_decision_rolls_back_with_its_conflicting_transition() {
    let Some(w) = World::new(2) else { return };
    w.lapse(hid(1));
    let read = w.h.load_coordinator().unwrap().expect("bootstrapped").version;
    // Another writer moved the row.
    write(&w.h, TransitionWrite { replica: ReplicaId(2), ..transition(read, &[9], None, vec![]) }).unwrap();
    let declare = || {
        let (dead, heirs) = (vec![(hid(1), boot_of(hid(1)))], vec![hid(0)]);
        StoreDecision::Declare(Declare { dead, heirs, kind: SuccessionKind::Crash, generation: 0 })
    };
    assert_eq!(write(&w.h, transition(read, &[7], None, vec![declare()])).unwrap_err(), StoreError::Conflict);
    assert!(w.h.open_successions().unwrap().is_empty(), "no successions row");
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, false), "no dead hosts row");
    assert_eq!(w.h.owned_by(hid(1)).unwrap(), w.fences[1], "no super_regions moved");
    assert_eq!(w.h.load_coordinator().unwrap().unwrap().core, vec![9], "the other writer's core stands");
    // At the row's version: the core and the declaration commit together.
    let written = write(&w.h, transition(read + 1, &[7], None, vec![declare()])).unwrap();
    assert_eq!(written.version, read + 2);
    let stamp = CallStamp { replica: ReplicaId(1), version: read + 2 };
    let [DecisionAnswer::Declared { stamp: got, outcome: Ok(Declared::Done(list)) }] = written.answers.as_slice() else {
        panic!("one declaration answered: {:?}", written.answers);
    };
    assert_eq!((*got, list.len()), (stamp, 1));
    assert_eq!(w.h.open_successions().unwrap(), list.clone(), "the succession is in the store with the core");
    assert_eq!(w.h.load_coordinator().unwrap().unwrap().core, vec![7]);
    w.shutdown();
}

/// A decision the store refuses is that decision's answer, not the write's: a commit of a cut
/// with a part missing is recorded as an ABORT (`committed: Ok(false)`), and the core commits
/// all the same; finishing the staged parts is the post-commit effect.
#[test]
fn a_refused_decision_still_commits_the_core() {
    let Some(w) = World::new(2) else { return };
    w.hosts[0].prepare_cut(part(1, hid(0), vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    let read = w.h.load_coordinator().unwrap().expect("bootstrapped").version;
    let decide = StoreDecision::DecideCut(CutDecide {
        cut: CutId(1),
        tick: 100,
        commit: true,
        hosts: vec![hid(0), hid(1)],
        leaving: vec![],
        generation: 0,
        settles: vec![],
    });
    let written = write(&w.h, transition(read, &[5], None, vec![decide])).unwrap();
    assert_eq!(written.version, read + 1, "the core committed");
    let stamp = CallStamp { replica: ReplicaId(1), version: read + 1 };
    assert_eq!(written.answers, vec![DecisionAnswer::Cut { stamp, cut: CutId(1), committed: Ok(false) }], "an abort recorded");
    assert_eq!(w.h.cut_decision(CutId(1)).unwrap(), Some(false));
    assert_eq!(written.after_commit, vec![AfterCommit::FinishCut { cut: CutId(1), tick: 100, committed: false }]);
    assert_eq!(prepared(&w.w), 1, "the staged part waits for the post-commit effect");
    for a in written.after_commit {
        w.h.after_commit_later(a).recv().unwrap().unwrap();
    }
    assert_eq!(prepared(&w.w), 0, "rolled back as decided");
    w.shutdown();
}

/// Task 3 fix round 1: a committed cut is announced to the terrain listeners only once its
/// staged parts are committed — the notice is the post-commit finish's own last step, never a
/// separate effect that could run beside it (a listener reading before `COMMIT PREPARED` would
/// see the old versions and wait for its poll).
#[test]
fn a_committed_cut_is_announced_only_after_its_parts_are_finished() {
    let Some(w) = World::new(2) else { return };
    let notices = w.h.listen_cuts(world_store::NotifyMode::Listen);
    for o in 0..2 {
        w.hosts[o].prepare_cut(part(1, hid(o as u64), vec![]), w.fences[o].clone(), 0).recv().unwrap().unwrap();
    }
    let read = w.h.load_coordinator().unwrap().expect("bootstrapped").version;
    let decide = StoreDecision::DecideCut(CutDecide {
        cut: CutId(1),
        tick: 100,
        commit: true,
        hosts: vec![hid(0), hid(1)],
        leaving: vec![],
        generation: 0,
        settles: vec![],
    });
    let written = write(&w.h, transition(read, &[5], None, vec![decide])).unwrap();
    assert_eq!(
        written.after_commit,
        vec![AfterCommit::FinishCut { cut: CutId(1), tick: 100, committed: true }],
        "one effect: the finish, which announces"
    );
    assert_eq!(prepared(&w.w), 2, "nothing finished before the effect");
    for a in written.after_commit {
        w.h.after_commit_later(a).recv().unwrap().unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let left = deadline.checked_duration_since(std::time::Instant::now()).expect("the committed cut was announced");
        match notices.recv_timeout(left) {
            Ok(world_store::CutNotice::Cut { tick }) => {
                assert_eq!(tick, 100);
                break;
            }
            Ok(world_store::CutNotice::Resync) => continue,
            Err(_) => panic!("the committed cut was never announced"),
        }
    }
    assert_eq!(prepared(&w.w), 0, "its parts were committed before the notice");
    w.shutdown();
}

/// A declaration that waits out its lock timeout behind a staged part of the predecessor is
/// refused — that is ITS answer (`Declared` with an error, nothing of it written); the core it
/// rode in commits all the same.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_decision_lock_timeout_is_its_answer_not_the_writes() {
    let Some(w) = World::new(2) else { return };
    w.h.set_cut_lock_timeout_ms(300);
    // Host 1's part of cut 1 is staged (it holds its super-regions FOR SHARE), then it dies.
    w.hosts[1].prepare_cut(part(1, hid(1), vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    w.vouch(hid(1));
    let read = w.h.load_coordinator().unwrap().expect("bootstrapped").version;
    let (dead, heirs) = (vec![(hid(1), boot_of(hid(1)))], vec![hid(0)]);
    let declare = StoreDecision::Declare(Declare { dead, heirs, kind: SuccessionKind::Crash, generation: 0 });
    let written = write(&w.h, transition(read, &[6], None, vec![declare])).unwrap();
    assert_eq!(written.version, read + 1, "the core committed");
    let [DecisionAnswer::Declared { outcome: Err(e), .. }] = written.answers.as_slice() else {
        panic!("the declaration's own refusal: {:?}", written.answers);
    };
    assert!(e.contains("lock_timeout"), "a lock timeout: {e}");
    assert_eq!(w.h.load_coordinator().unwrap().unwrap().core, vec![6]);
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, false), "nothing of the declaration written");
    assert_eq!(w.h.owned_by(hid(1)).unwrap(), w.fences[1]);
    assert!(w.h.open_successions().unwrap().is_empty());
    assert!(!w.h.abort_cut(CutId(1), 100).unwrap());
    assert_eq!(prepared(&w.w), 0);
    w.shutdown();
}

// ---- the outbox (Elastic 2b Task 5) -------------------------------------------------------

/// A fresh Postgres store (the outbox is Postgres only: one replica on SQLite), its row
/// bootstrapped at version 1.
fn pg_store(tag: &str) -> Option<(crate::common::PgTestDb, StoreHandle)> {
    if crate::common::pg_admin_url().is_none() {
        crate::common::skip_msg();
        return None;
    }
    let db = crate::common::PgTestDb::create(&format!("mmo_test_outbox_{tag}")).expect("pg env var set");
    let h = open(&world_store::StoreUrl::Postgres(db.url.clone()));
    h.bootstrap_coordinator(vec![0], vec![10]).unwrap();
    Some((db, h))
}

/// A transition of replica `replica` at `version` (core `[version]`) leaving `outbox` for other
/// replicas' hosts, taking the rows of `take` (its own hosts, with their links' boots) and
/// discarding the rows of `discard` (the hosts whose `Attach` it applied).
fn relay(version: u64, replica: u64, outbox: &[(u64, u64, u8)], take: &[(u64, u64)], discard: &[u64]) -> TransitionWrite {
    TransitionWrite {
        version,
        core: vec![version as u8],
        replica: ReplicaId(replica),
        outbox: outbox.iter().map(|(h, b, m)| (hid(*h), *b, vec![*m])).collect(),
        take_for: take.iter().map(|(h, b)| (hid(*h), *b)).collect(),
        discard_for: discard.iter().map(|h| hid(*h)).collect(),
        ..Default::default()
    }
}

/// The rows as `(version, host, boot, msg)`, in their order (`seq` rising).
fn rows_of(rows: &[world_store::OutboxRow]) -> Vec<(u64, u64, u64, Vec<u8>)> {
    assert!(rows.windows(2).all(|w| w[0].seq < w[1].seq), "in seq order: {rows:?}");
    rows.iter().map(|r| (r.version, r.host.0, r.boot, r.msg.clone())).collect()
}

/// Review Focus 2 (decision 5): the transition that applies a host's `Attach` discards every
/// outbox row for it — they were decided before the `Attach`, which reconciles what they
/// carried — before it takes its hosts' rows: none is delivered. A row a later transition leaves
/// for it is delivered.
#[test]
fn outbox_rows_older_than_an_attach_are_discarded() {
    let Some((_db, h)) = pg_store("attach") else { return };
    write(&h, relay(1, 1, &[(5, 9, 1), (5, 9, 2), (6, 3, 7)], &[], &[])).unwrap();
    let attached = write(&h, relay(2, 2, &[], &[(5, 9)], &[5])).unwrap();
    assert!(attached.taken.is_empty(), "the rows older than the Attach are discarded, never taken: {:?}", attached.taken);
    write(&h, relay(3, 1, &[(5, 9, 3)], &[], &[])).unwrap();
    let rows = h.take_outbox_later(vec![(hid(5), 9), (hid(6), 3)], 4).recv().unwrap().unwrap();
    assert_eq!(rows_of(&rows), vec![(2, 6, 3, vec![7]), (4, 5, 9, vec![3])], "host 6's row stands; host 5's newer row is delivered");
    assert!(h.take_outbox_later(vec![(hid(5), 9), (hid(6), 3)], 4).recv().unwrap().unwrap().is_empty(), "taken once");
    h.shutdown();
}

/// Decision 5: a write takes the rows of its own hosts only (the ones it names, at its links'
/// boots), in commit order (`seq`); a row whose boot is not its link's is deleted unsent; the
/// other hosts' rows stay for the replica that links them.
#[test]
fn a_write_takes_only_its_own_hosts_rows_in_seq_order() {
    let Some((_db, h)) = pg_store("take") else { return };
    write(&h, relay(1, 1, &[(1, 10, 1), (2, 20, 2), (1, 10, 3)], &[], &[])).unwrap();
    write(&h, relay(2, 1, &[(3, 30, 4), (1, 11, 5)], &[], &[])).unwrap();
    let written = write(&h, relay(3, 2, &[(2, 20, 6)], &[(1, 10), (3, 30)], &[])).unwrap();
    assert_eq!(written.version, 4);
    assert_eq!(
        rows_of(&written.taken),
        vec![(2, 1, 10, vec![1]), (2, 1, 10, vec![3]), (3, 3, 30, vec![4])],
        "hosts 1 and 3's rows at their links' boots, in seq order"
    );
    let rows = h.take_outbox_later(vec![(hid(2), 20)], 4).recv().unwrap().unwrap();
    assert_eq!(rows_of(&rows), vec![(2, 2, 20, vec![2]), (4, 2, 20, vec![6])], "host 2's rows stayed for its replica");
    assert!(h.take_outbox_later(vec![(hid(1), 11)], 4).recv().unwrap().unwrap().is_empty(), "host 1's row of another boot was deleted unsent");
    h.shutdown();
}

/// Final review T3: the epoch a core replacing an unreadable stored one makes its cut ids under is
/// raised past the row's own and past the high half of every recorded cut decision's id — never an
/// id the replaced core made (its stored decision would be returned for a new cut, unchecked).
#[test]
fn a_raised_epoch_is_past_every_recorded_cut_id() {
    let Some((_db, h)) = pg_store("raise") else { return };
    assert_eq!(h.raise_coordinator_epoch().unwrap(), Some(2), "past the bootstrapped row's epoch 1");
    let old = game::CutId((5 << 32) | 3);
    assert!(!h.abort_cut(old, 1).unwrap(), "an abort recorded at cut epoch 5");
    assert_eq!(h.raise_coordinator_epoch().unwrap(), Some(6), "past the recorded decisions' epoch 5");
    assert_eq!(h.raise_coordinator_epoch().unwrap(), Some(7), "and past its own again");
    assert_eq!(h.load_coordinator().unwrap().map(|r| r.version), Some(1), "the version is untouched");
    h.shutdown();
}

/// Final review S-I2: a take between transitions takes only the rows at or below the version of
/// the cache that named its hosts. Host 5 is attached to replica A as of version 3; a row decided
/// at version 4 (after host 5 attached to B, say — A's cache cannot know) stays in the table for B,
/// and B (at version 4) takes it.
#[test]
fn a_take_between_transitions_stops_at_its_cache_version() {
    let Some((_db, h)) = pg_store("upto") else { return };
    write(&h, relay(1, 1, &[(5, 9, 1)], &[], &[])).unwrap();
    write(&h, relay(2, 1, &[], &[], &[])).unwrap();
    write(&h, relay(3, 3, &[(5, 9, 2)], &[], &[])).unwrap();
    let stale = h.take_outbox_later(vec![(hid(5), 9)], 3).recv().unwrap().unwrap();
    assert_eq!(rows_of(&stale), vec![(2, 5, 9, vec![1])], "only the row at or below the taker's cache");
    let newer = h.take_outbox_later(vec![(hid(5), 9)], 4).recv().unwrap().unwrap();
    assert_eq!(rows_of(&newer), vec![(4, 5, 9, vec![2])], "the newer row stayed for a cache that knows it");
    h.shutdown();
}

/// Decision 4: a transition that meets a moved row (`Conflict`) inserts no outbox row, takes
/// none and notifies nothing; a committed one notifies `coordinator_row` with its version.
#[test]
fn a_conflicting_write_inserts_no_outbox_row() {
    let Some((_db, h)) = pg_store("conflict") else { return };
    let notices = h.listen_channel(world_store::COORDINATOR_ROW_CHANNEL, world_store::NotifyMode::Listen);
    // The listener is up once it says Resync.
    assert_eq!(notices.recv_timeout(std::time::Duration::from_secs(5)), Ok(world_store::Notice::Resync));
    write(&h, relay(1, 1, &[(7, 1, 1)], &[], &[])).unwrap();
    assert_eq!(write(&h, relay(1, 2, &[(7, 1, 2)], &[(7, 1)], &[7])).unwrap_err(), StoreError::Conflict);
    let next_payload = || loop {
        match notices.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(world_store::Notice::Payload(p)) => return Some(p),
            Ok(world_store::Notice::Resync) => continue,
            Err(_) => return None,
        }
    };
    assert_eq!(next_payload(), Some("2".to_string()), "the committed write's version");
    write(&h, relay(2, 1, &[], &[], &[])).unwrap();
    assert_eq!(next_payload(), Some("3".to_string()), "no notice of the conflicting write came between");
    let rows = h.take_outbox_later(vec![(hid(7), 1)], 3).recv().unwrap().unwrap();
    assert_eq!(rows_of(&rows), vec![(2, 7, 1, vec![1])], "only the committed write's row; the conflicting one discarded and took nothing");
    h.shutdown();
}
