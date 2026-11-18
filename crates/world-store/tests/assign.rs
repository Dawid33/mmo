//! Elastic 3: the coordinator gives a free (or absent) super-region to a live host inside its
//! transition (`StoreDecision::AssignLand`, `world-store/src/assign.rs`): first writer wins, an
//! owned row is never touched (nor waited for), and the assignment commits or rolls back with
//! its transition. Postgres only (SQLite: `coordinator_rows::a_decision_on_sqlite_is_unsupported`).

use std::time::{Duration, Instant};

use crate::common::hid;
use game::coordinator::{AssignLand, Assigned, Declare, DecisionAnswer, ReplicaId, StoreDecision};
use game::{CutId, HOST_DEAD, SuccessionKind};
use world_store::{Declared, StoreError, StoreHandle, TransitionWrite};

use crate::succession::{World, boot_of, part};
use crate::two_phase::exec;

/// A super-region no host of [`World`] holds.
const FREE: (i32, i32) = (7, 7);

/// `sr`'s row: `(owner, epoch)`, `None` when absent.
fn row(h: &StoreHandle, sr: (i32, i32)) -> Option<(i64, u64)> {
    h.owners().unwrap().into_iter().find(|(s, _, _)| *s == sr).map(|(_, owner, epoch)| (owner, epoch))
}

fn assign(sr: (i32, i32), host: game::HostId) -> AssignLand {
    AssignLand { sr, host, boot: boot_of(host) }
}

/// A transition of replica 1 at the row's current version carrying `decisions`.
fn transition_now(h: &StoreHandle, decisions: Vec<StoreDecision>) -> TransitionWrite {
    let version = h.load_coordinator().unwrap().expect("bootstrapped").version;
    TransitionWrite { version, core: vec![version as u8], decisions, replica: ReplicaId(1), ..Default::default() }
}

#[test]
fn a_free_or_absent_row_is_assigned_at_the_next_epoch() {
    let Some(w) = World::new(2) else { return };
    assert_eq!(row(&w.h, FREE), None);
    assert_eq!(w.h.assign_land(assign(FREE, hid(0))).unwrap(), Assigned::Done { epoch: 1 });
    assert_eq!(row(&w.h, FREE), Some((0, 1)), "an absent row: the host's at epoch 1");
    // A row released at epoch 3.
    let released = (8, 8);
    exec(&w.w, &format!("INSERT INTO super_regions (sx, sz, owner, epoch) VALUES (8, 8, {}, 3)", game::NO_OWNER));
    assert_eq!(w.h.assign_land(assign(released, hid(1))).unwrap(), Assigned::Done { epoch: 4 });
    assert_eq!(row(&w.h, released), Some((1, 4)), "a free row: the host's at epoch + 1");
    w.shutdown();
}

#[test]
fn an_owned_row_is_refused_and_untouched() {
    let Some(w) = World::new(2) else { return };
    let (&sr, &epoch) = w.fences[0].epochs.iter().next().expect("host 0 holds land");
    assert_eq!(w.h.assign_land(assign(sr, hid(1))).unwrap(), Assigned::Owned);
    assert_eq!(row(&w.h, sr), Some((0, epoch)), "owner and epoch unchanged");
    w.shutdown();
}

#[test]
fn a_dead_cleanly_left_or_draining_assignee_gets_nothing() {
    let Some(w) = World::new(4) else { return };
    // Host 0: a dead row.
    exec(&w.w, &format!("UPDATE hosts SET state = {HOST_DEAD} WHERE id = 0"));
    assert_eq!(w.h.assign_land(assign(FREE, hid(0))).unwrap(), Assigned::HostNotLive);
    // Host 1: a clean row (it left).
    exec(&w.w, "UPDATE hosts SET clean = 1 WHERE id = 1");
    assert_eq!(w.h.assign_land(assign(FREE, hid(1))).unwrap(), Assigned::HostNotLive);
    // Host 2: a drain mark.
    w.h.drain_host(hid(2), 60_000).unwrap();
    assert_eq!(w.h.assign_land(assign(FREE, hid(2))).unwrap(), Assigned::HostDraining);
    // Host 3: another boot of its id.
    let other_boot = AssignLand { boot: boot_of(hid(3)) + 1, ..assign(FREE, hid(3)) };
    assert_eq!(w.h.assign_land(other_boot).unwrap(), Assigned::HostNotLive);
    assert_eq!(row(&w.h, FREE), None, "the row stays free (absent) in all four");
    w.shutdown();
}

#[test]
fn two_assignments_of_one_row_one_wins() {
    let Some(w) = World::new(2) else { return };
    let write = |d| w.h.write_transition_later(transition_now(&w.h, vec![d])).recv().unwrap().unwrap();
    let first = write(StoreDecision::AssignLand(assign(FREE, hid(0))));
    let second = write(StoreDecision::AssignLand(assign(FREE, hid(1))));
    let outcome = |a: &[DecisionAnswer]| match a {
        [DecisionAnswer::Assigned { outcome, .. }] => outcome.clone(),
        other => panic!("one assignment answered: {other:?}"),
    };
    assert_eq!(outcome(&first.answers), Ok(Assigned::Done { epoch: 1 }));
    assert_eq!(outcome(&second.answers), Ok(Assigned::Owned));
    assert_eq!(row(&w.h, FREE), Some((0, 1)), "the first writer holds it");
    w.shutdown();
}

#[test]
fn an_assignment_never_waits_for_a_staged_part() {
    let Some(w) = World::new(2) else { return };
    w.h.set_cut_lock_timeout_ms(1_000);
    // Host 0's part of cut 1 is staged: its super-regions are held FOR SHARE.
    w.hosts[0].prepare_cut(part(1, hid(0), vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    let (&sr, &epoch) = w.fences[0].epochs.iter().next().expect("host 0 holds land");
    let started = Instant::now();
    assert_eq!(w.h.assign_land(assign(sr, hid(1))).unwrap(), Assigned::Owned);
    let took = started.elapsed();
    assert!(took < Duration::from_millis(500), "the assignment waited {took:?} for a staged part");
    assert_eq!(row(&w.h, sr), Some((0, epoch)));
    assert!(!w.h.abort_cut(CutId(1), 100).unwrap());
    w.shutdown();
}

#[test]
fn an_assignment_rolls_back_with_its_conflicting_transition() {
    let Some(w) = World::new(2) else { return };
    let stale = transition_now(&w.h, vec![StoreDecision::AssignLand(assign(FREE, hid(0)))]);
    // Another writer moves the row first.
    w.h.write_transition_later(transition_now(&w.h, vec![])).recv().unwrap().unwrap();
    assert_eq!(w.h.write_transition_later(stale).recv().unwrap().unwrap_err(), StoreError::Conflict);
    assert_eq!(row(&w.h, FREE), None, "the row is still free");
    w.shutdown();
}

#[test]
fn assigned_then_declared_in_one_transition_moves_with_the_rest() {
    let Some(w) = World::new(2) else { return };
    w.lapse(hid(1));
    let declare = Declare { dead: vec![(hid(1), boot_of(hid(1)))], heirs: vec![hid(0)], kind: SuccessionKind::Crash, generation: 0 };
    let t = transition_now(&w.h, vec![StoreDecision::AssignLand(assign(FREE, hid(1))), StoreDecision::Declare(declare)]);
    let written = w.h.write_transition_later(t).recv().unwrap().unwrap();
    let [DecisionAnswer::Assigned { outcome: assigned, .. }, DecisionAnswer::Declared { outcome: Ok(Declared::Done(list)), .. }] =
        written.answers.as_slice()
    else {
        panic!("an assignment and a declaration answered: {:?}", written.answers);
    };
    assert_eq!(assigned, &Ok(Assigned::Done { epoch: 1 }));
    assert_eq!(list.len(), 1);
    assert!(list[0].moved.contains(&(FREE, 2)), "the assigned row moved with the rest: {:?}", list[0].moved);
    assert_eq!(row(&w.h, FREE), Some((0, 2)), "the heir's, assigned at 1, moved at 2");
    w.shutdown();
}
