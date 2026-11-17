//! The coordinator's replicas' rows (Elastic 2b Task 4, migration 0017): ids issued from the
//! `coordinator` row and never reused; a registration is `alone` when no other replica was
//! beaten within the lease (those rows are marked dead); a beat of an ended replica answers
//! "not live". Registration on both engines; beats and reads need Postgres.

use game::coordinator::{ReplicaId, StoreDecision};
use world_store::{StoreHandle, TransitionWrite};

use crate::handle::{each_backend, open};

/// Every replica's row, as `(id, live)`.
fn rows(h: &StoreHandle) -> Vec<(ReplicaId, bool)> {
    h.replicas().unwrap().into_iter().map(|r| (r.replica, r.live)).collect()
}

#[test]
fn ids_are_never_reused() {
    each_backend("replicas_ids", true, |url| {
        let h = open(url);
        let a = h.register_replica("a").unwrap();
        // The bootstrap fills the row the registration made (empty): epoch 1, version 1.
        assert_eq!(h.bootstrap_coordinator(vec![1], vec![10]).unwrap(), world_store::Bootstrapped::Written { version: 1, epoch: 1 });
        let b = h.register_replica("b").unwrap();
        h.shutdown();
        let h = open(url);
        let c = h.register_replica("c").unwrap();
        assert!(a.replica < b.replica && b.replica < c.replica, "{a:?} {b:?} {c:?}");
        assert_eq!(h.load_coordinator().unwrap().map(|s| (s.version, s.core)), Some((1, vec![1])), "the row untouched");
        h.shutdown();
    });
}

#[test]
fn a_registration_beside_a_live_replica_is_not_alone() {
    each_backend("replicas_beside", false, |url| {
        let h = open(url);
        let a = h.register_replica("a").unwrap();
        assert!(a.alone, "the first replica is alone");
        let b = h.register_replica("b").unwrap();
        assert!(!b.alone, "A was beaten (registered) just now");
        assert_eq!(rows(&h), vec![(a.replica, true), (b.replica, true)]);
        assert!(h.beat_replica(a.replica).unwrap(), "A is live");
        h.shutdown();
    });
}

#[test]
fn a_registration_after_every_replica_lapsed_is_alone_and_marks_them_dead() {
    each_backend("replicas_lapsed", false, |url| {
        let h = open(url);
        let a = h.register_replica("a").unwrap();
        let b = h.register_replica("b").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        // Both lapsed (a lease of 10 ms, both silent for longer).
        h.set_replica_lease_ms(10);
        let c = h.register_replica("c").unwrap();
        assert!(c.alone, "no other replica beaten within the lease");
        assert_eq!(rows(&h), vec![(a.replica, false), (b.replica, false), (c.replica, true)]);
        assert!(!h.beat_replica(a.replica).unwrap(), "A's row is dead: its beat answers not live");
        assert!(h.beat_replica(c.replica).unwrap());
        h.shutdown();
    });
}

#[test]
fn a_beat_of_an_ended_replica_answers_not_live() {
    each_backend("replicas_ended", false, |url| {
        // A store decision needs Postgres (SQLite never ends a replica).
        if !matches!(url, world_store::StoreUrl::Postgres(_)) {
            return;
        }
        let h = open(url);
        let a = h.register_replica("a").unwrap();
        let b = h.register_replica("b").unwrap();
        h.bootstrap_coordinator(vec![1], vec![10]).unwrap();
        let version = h.load_coordinator().unwrap().unwrap().version;
        // B's transition ends A (Elastic 2b: the decision inside B's compare-and-set).
        let w = TransitionWrite { version, core: vec![2], history: None, decisions: vec![StoreDecision::EndReplica(a.replica)], replica: b.replica };
        let written = h.write_transition_later(w).recv().unwrap().unwrap();
        assert_eq!(written.answers, vec![game::coordinator::DecisionAnswer::ReplicaEnded { replica: a.replica }]);
        assert!(!h.beat_replica(a.replica).unwrap(), "ended: not live");
        assert!(h.beat_replica(b.replica).unwrap(), "B beats on");
        assert_eq!(rows(&h), vec![(a.replica, false), (b.replica, true)]);
        // The history stayed at its version; a read since it returns no history.
        let since = h.load_coordinator_since(1).unwrap().unwrap();
        assert_eq!((since.version, since.history_version, since.history.is_empty()), (version + 1, 1, true));
        assert_eq!(h.load_coordinator_since(0).unwrap().unwrap().history, vec![10]);
        h.shutdown();
    });
}
