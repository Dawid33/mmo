//! The peer journal (`game::peer::journal`, R4): the state messages a host exchanged with
//! each peer since the last committed world cut, truncated by cut ownership read
//! backwards. The seeded property over real hosts is
//! `peer_schedule::journal_explains_every_difference_from_the_last_cut`.
use std::collections::BTreeSet;

use game::{CutId, EntityId, JournalEvent, PeerJournal, PlayerId, RegionCoords, Reliable, Subject};

const A: u32 = 1;
const B: u32 = 2;
const K: CutId = CutId(5);
const NOW: u64 = 900;

fn rc(x: i32, z: i32) -> RegionCoords {
    RegionCoords::new(x, z)
}

fn id(serial: u64) -> EntityId {
    EntityId { birth_region: rc(0, 0), serial }
}

fn transfer(serial: u64, stamp: u64) -> Reliable {
    let pose = game::Pose::from_translation(game::rapier3d::math::Vector::new(1.0, 2.0, 3.0));
    let collider = game::ColliderSpec::Ball { radius: 0.5 };
    let zero = game::rapier3d::math::Vector::ZERO;
    let bundle = game::EntityBundle {
        id: id(serial),
        kind: game::EntityKind::Slime { size: 1 },
        body: game::body::body_state(pose, &collider, zero, zero),
        collider,
        carried: Default::default(),
        client: None,
        pending_inputs: Vec::new(),
    };
    Reliable::Transfer { target: rc(1, 0), stamp, bundle: Box::new(bundle), player: None }
}

fn handoff(player: u64) -> game::PlayerHandoff {
    game::PlayerHandoff { client: 7, player: PlayerId(player), claim: None, login: None, park: false, hop: 1 }
}

fn player_move(player: u64) -> Reliable {
    let record = game::PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(game::rapier3d::math::Vector::ZERO),
        region: rc(1, 0),
        fortress: None,
        adventure: None,
    };
    Reliable::PlayerMove { handoff: handoff(player), record: Box::new(record), target: rc(1, 0) }
}

fn wrapped(cut: CutId, msg: Reliable) -> Reliable {
    Reliable::InCut { cut, body: Box::new(msg) }
}

fn participants(hosts: &[u32]) -> BTreeSet<u32> {
    hosts.iter().copied().collect()
}

/// `(subject, sent?)` of every entry with `peer`, in order.
fn kept(j: &PeerJournal, peer: u32) -> Vec<(Subject, bool)> {
    j.entries(peer).map(|(e, _)| (e.subject, matches!(e.event, JournalEvent::Sent(_)))).collect()
}

/// A transfer that went bare before this host's mark of K is in the receiver's part of
/// K, and one that came bare before the peer's mark is in this host's: both are forgotten
/// when K commits. What went or came after a mark stays.
#[test]
fn bare_before_the_mark_is_forgotten_at_commit() {
    let mut j = PeerJournal::new(CutId(4));
    j.sent(A, 0, &transfer(1, 10), None, NOW);
    j.received(A, &transfer(2, 11), None, NOW);
    j.mark_out(A, K);
    j.received(A, &transfer(3, 12), None, NOW);
    j.mark_in(A, K);
    j.sent(A, 2, &transfer(4, 13), None, NOW);
    j.received(A, &transfer(5, 14), None, NOW);
    assert_eq!(j.len(), 5);
    j.committed(K, &participants(&[A]));
    assert_eq!(
        kept(&j, A),
        vec![(Subject::Entity(id(4)), true), (Subject::Entity(id(5)), false)],
        "only what crossed after the mark of its direction stays (id 3 came after this host's mark, \
         but before the peer's: the receiving direction reads the peer's mark)"
    );
    assert_eq!(j.base(), K);
    // The sent entry keeps its queue id and the message itself, never a wrapper.
    let (entry, queue_id) = j.entries(A).find(|(e, _)| matches!(e.event, JournalEvent::Sent(_))).expect("sent");
    assert_eq!(queue_id, Some(2));
    assert_eq!(entry.stamp, 13);
    assert!(matches!(&entry.event, JournalEvent::Sent(m) if matches!(**m, Reliable::Transfer { .. })));
    // The next commit forgets those too (they all went before its marks).
    j.mark_out(A, CutId(6));
    j.mark_in(A, CutId(6));
    j.committed(CutId(6), &participants(&[A]));
    assert!(j.is_empty(), "everything before both marks of the next cut is explained by it: {:?}", kept(&j, A));
}

/// A message wrapped `InCut { K }` was still its sender's at K: it stays in both
/// directions though it crossed before the marks; a wrapper of another (an aborted) cut
/// does not keep it.
#[test]
fn wrapped_in_the_cut_survives_its_commit() {
    let mut j = PeerJournal::new(CutId(3));
    j.sent(A, 0, &wrapped(K, transfer(1, 10)), None, NOW);
    j.sent(A, 1, &transfer(2, 10), Some(K), NOW);
    j.received(A, &wrapped(K, player_move(9)), None, NOW);
    j.sent(A, 2, &wrapped(CutId(4), transfer(3, 10)), None, NOW);
    j.received(A, &transfer(4, 10), Some(CutId(4)), NOW);
    j.mark_out(A, K);
    j.mark_in(A, K);
    j.committed(K, &participants(&[A]));
    assert_eq!(
        kept(&j, A),
        vec![(Subject::Entity(id(1)), true), (Subject::Entity(id(2)), true), (Subject::Player(PlayerId(9)), false)]
    );
    let (entry, _) = j.entries(A).next().expect("an entry");
    assert!(
        matches!(&entry.event, JournalEvent::Sent(m) if !matches!(**m, Reliable::InCut { .. })),
        "a sent entry holds the message, never its wrapper"
    );
    // A player move is stamped with the world tick it was journalled at.
    let (mv, _) = j.entries(A).find(|(e, _)| e.subject == Subject::Player(PlayerId(9))).expect("the move");
    assert_eq!(mv.stamp, NOW);
}

/// An aborted cut forgets its marks and nothing else: the next committed cut truncates
/// by its own marks only.
#[test]
fn an_abort_forgets_nothing() {
    let mut j = PeerJournal::new(CutId(3));
    j.sent(A, 0, &transfer(1, 10), None, NOW);
    j.received(A, &transfer(2, 10), None, NOW);
    j.mark_out(A, CutId(4));
    j.mark_in(A, CutId(4));
    j.sent(A, 2, &transfer(3, 10), None, NOW);
    j.aborted(CutId(4));
    assert_eq!(j.len(), 3, "an abort keeps every entry");
    assert_eq!(j.base(), CutId(3));
    // Committing K without marks of it (a direction never marked) keeps everything; the
    // aborted cut's marks no longer count.
    j.committed(CutId(4), &participants(&[A]));
    assert_eq!(j.len(), 3, "the aborted cut's marks were forgotten");
    j.mark_out(A, K);
    j.sent(A, 4, &transfer(4, 10), None, NOW);
    j.mark_in(A, K);
    j.committed(K, &participants(&[A]));
    assert_eq!(kept(&j, A), vec![(Subject::Entity(id(4)), true)]);
}

/// Entries toward a host that was not a participant of the committed cut stay whole; a
/// one-phase cut (no participants) only moves the base.
#[test]
fn entries_toward_an_absent_host_survive() {
    let mut j = PeerJournal::new(CutId(0));
    for peer in [A, B] {
        j.sent(peer, 0, &transfer(u64::from(peer) * 10, 10), None, NOW);
        j.received(peer, &transfer(u64::from(peer) * 10 + 1, 10), None, NOW);
        j.mark_out(peer, K);
        j.mark_in(peer, K);
    }
    j.committed(K, &participants(&[A]));
    assert_eq!(kept(&j, A), Vec::new());
    assert_eq!(kept(&j, B).len(), 2, "host {B} was not in the cut");
    j.committed(CutId(6), &BTreeSet::new());
    assert_eq!(j.base(), CutId(6));
    assert_eq!(kept(&j, B).len(), 2, "a one-phase cut truncates nothing");
    // `take` hands over everything with that peer and forgets it.
    let taken = j.take(B);
    assert_eq!(taken.len(), 2);
    assert!(j.is_empty());
}

/// A transfer handing over a player is journalled for both its entity and its player;
/// intents, replies, orders, marks and `Owns` are never journalled.
#[test]
fn intents_and_orders_are_never_journalled() {
    let mut j = PeerJournal::new(CutId(0));
    let cell = game::WorldBlockPos::from_local(rc(1, 0), game::BlockPos::new(1, 1, 1));
    let stack = game::ItemStack { item: game::ItemId(2), count: 3 };
    let intent = game::Intent::DepositItems { cell, stack, expire: 50, pickup: 10, by: EntityId::of_region(rc(0, 0)) };
    let sent = game::SentIntent { target: game::Target::Pile(cell), intent: intent.clone(), from: rc(0, 0), due: 10 };
    let reply = game::SentReply {
        reply: game::IntentReply::Refused(intent, game::IntentRefusal::PileFull),
        hint: rc(0, 0),
        due: 11,
        hops: 0,
    };
    let none = [
        Reliable::Intent(game::IntentMsg::Intent(sent)),
        Reliable::Intent(game::IntentMsg::Reply(reply.clone())),
        wrapped(K, Reliable::Intent(game::IntentMsg::Reply(reply))),
        Reliable::SpawnOrder {
            origin: 0,
            hops: 0,
            player_token: "t".into(),
            session_epoch: 1,
            player: PlayerId(3),
            record_region: None,
        },
        Reliable::ObserverOrder { client: 7, player: PlayerId(3), nonce: game::Nonce(1), ttl_secs: 5 },
        Reliable::Kick { player_token: "t".into(), session_epoch: 1, reason: "r".into() },
        Reliable::CutMark { cut: K },
        Reliable::Owns { rows: Vec::new() },
        Reliable::PlayerParked { client: 7, player: PlayerId(3) },
    ];
    for (i, msg) in none.iter().enumerate() {
        j.sent(A, i as u64, msg, None, NOW);
        j.received(A, msg, None, NOW);
    }
    assert!(j.is_empty(), "not state: {:?}", kept(&j, A));
    let Reliable::Transfer { target, stamp, bundle, .. } = transfer(1, 10) else { unreachable!() };
    let with_player = Reliable::Transfer { target, stamp, bundle, player: Some(handoff(9)) };
    j.sent(A, 20, &with_player, None, NOW);
    assert_eq!(kept(&j, A), vec![(Subject::Entity(id(1)), true), (Subject::Player(PlayerId(9)), true)]);
}
