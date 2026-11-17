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

fn stored(version: u64, core: &[u8], history: &[u8]) -> Option<StoredCoordinator> {
    Some(StoredCoordinator { version, core: core.to_vec(), history: history.to_vec() })
}

/// A transition of replica 1 at `version`: `core`, `history`, `decisions`.
fn transition(version: u64, core: &[u8], history: Option<&[u8]>, decisions: Vec<StoreDecision>) -> TransitionWrite {
    TransitionWrite { version, core: core.to_vec(), history: history.map(<[u8]>::to_vec), decisions, replica: ReplicaId(1) }
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
        assert_eq!(other.load_coordinator().unwrap(), stored(1, &[1], &[10]), "the second bootstrap wrote nothing");
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
        assert_eq!(h.load_coordinator().unwrap(), stored(3, &[2], &[10]), "the conflicting write wrote nothing");
        // A version from the future conflicts too.
        assert_eq!(write(&h, transition(9, &[4], None, vec![])).unwrap_err(), StoreError::Conflict);
        write(&h, transition(3, &[5], None, vec![])).unwrap();
        assert_eq!(h.load_coordinator().unwrap(), stored(4, &[5], &[10]));
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
        assert_eq!(h.load_coordinator().unwrap(), stored(3, &[3], &[10, 11]), "the core moves on, the history stays");
        write(&h, transition(3, &[4], Some(&[40]), vec![])).unwrap();
        assert_eq!(h.load_coordinator().unwrap(), stored(4, &[4], &[40]));
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
    assert_eq!(h.load_coordinator().unwrap(), stored(1, &[1], &[10]), "nothing written");
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
