//! The world coordinator core (R2b Task 4): requests validated and answered once, durable
//! before visible, claims, bounced directives, reattach after a restart, the saved state,
//! and the inline link's seeded delays. Pure `Coordinator` — the host is played by sending
//! `HostMsg`s directly.

use game::coordinator::*;
use game::history::*;
use game::{EmbarkRequest, GameEventKind, InlineSchedule, PlayerId, RegionCoords};

use crate::history_handover::{land_map, start, years_before};

const A: PlayerId = PlayerId(1);
const B: PlayerId = PlayerId(2);
/// The hop a player is placed at by [`online`] and `report`, and so the hop its requests
/// carry in most tests.
const PLACED: u64 = 1;
/// Seeds per seeded test.
const SEEDS: u64 = 32;

fn rc(x: i32, z: i32) -> RegionCoords {
    RegionCoords::new(x, z)
}

fn rid(seq: u64) -> RequestId {
    RequestId { host: 0, boot: 7, seq }
}

/// The messages free to leave, whatever host each is for (one host in most tests).
trait TakeMsgs {
    fn take_msgs(&mut self) -> Vec<CoordMsg>;
}

impl TakeMsgs for Coordinator {
    fn take_msgs(&mut self) -> Vec<CoordMsg> {
        self.take_out().into_iter().map(|(_, m)| m).collect()
    }
}

fn grid() -> SocietyGrid {
    SocietyGrid::from_world_map(&land_map(16, 3))
}

/// Site 0 at (0, 0) with six people of one civ, on a 16 × 3 land map.
fn world() -> (History, CivId, Vec<PersonId>) {
    let s = start();
    let mut h = History::new(5, s, grid());
    let civ = h.add_civ(Race::Dwarf);
    let people: Vec<PersonId> = (0..6)
        .map(|i| h.add_founder(if i % 2 == 0 { Sex::Female } else { Sex::Male }, years_before(s, 20)))
        .collect();
    h.found_site(civ, rc(0, 0), &people, None, false);
    (h, civ, people)
}

/// A fresh coordinator over [`world`], its opening `View` drained.
fn coordinator() -> (Coordinator, CivId, Vec<PersonId>) {
    let (h, civ, people) = world();
    let mut c = Coordinator::new(h, 0);
    let first = c.take_msgs();
    assert!(matches!(first.as_slice(), [CoordMsg::View { .. }]), "a new coordinator opens with its view: {first:?}");
    (c, civ, people)
}

fn embark(id: u64, player: PlayerId, civ: CivId, people: &[PersonId], region: RegionCoords) -> HostMsg {
    HostMsg::Request { id: rid(id), player, hop: PLACED, req: WorldRequest::Embark(EmbarkRequest { civ, people: people.to_vec(), region }) }
}

fn request(id: u64, player: PlayerId, req: WorldRequest) -> HostMsg {
    HostMsg::Request { id: rid(id), player, hop: PLACED, req }
}

/// Save as a store that commits at once would, then take what may leave.
fn commit(c: &mut Coordinator) -> Vec<CoordMsg> {
    if c.take_save().is_some() {
        c.saved();
    }
    c.take_msgs()
}

fn replies(msgs: &[CoordMsg]) -> Vec<(RequestId, WorldReply)> {
    msgs.iter()
        .filter_map(|m| match m {
            CoordMsg::Reply { id, reply } => Some((*id, reply.clone())),
            _ => None,
        })
        .collect()
}

fn reply_to(msgs: &[CoordMsg], id: u64) -> WorldReply {
    let found: Vec<WorldReply> = replies(msgs).into_iter().filter(|(i, _)| *i == rid(id)).map(|(_, r)| r).collect();
    assert_eq!(found.len(), 1, "one reply to request {id}: {msgs:?}");
    found.into_iter().next().unwrap()
}

/// Every event in `msgs` (catch-ups and directives) that names `person`.
fn events_about(msgs: &[CoordMsg], person: PersonId) -> Vec<GameEventKind> {
    let names = |e: &GameEventKind| match e {
        GameEventKind::SpawnPerson(s) => s.person == person,
        GameEventKind::DespawnPerson(p) | GameEventKind::WalkOut(p, _) => *p == person,
        _ => false,
    };
    let mut out = Vec::new();
    for m in msgs {
        match m {
            CoordMsg::SpawnPeople { events, .. } => out.extend(events.iter().filter(|e| names(e)).cloned()),
            CoordMsg::Directive { directive: d, .. } if names(&d.event) => out.push(d.event.clone()),
            _ => {}
        }
    }
    out
}

#[test]
fn embark_validates_and_claims_once() {
    let (mut c, civ, people) = coordinator();
    let sites = c.history().sites().count();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    c.handle(0, embark(2, B, civ, &people[2..4], rc(8, 0)), 0);
    let out = commit(&mut c);
    let WorldReply::Embarked { site, region } = reply_to(&out, 1) else { panic!("A's embark founds: {out:?}") };
    assert_eq!(region, rc(8, 0));
    assert!(
        matches!(
            reply_to(&out, 2),
            WorldReply::EmbarkRefused(EmbarkRefusal::Occupied | EmbarkRefusal::TooClose { .. })
        ),
        "B's embark onto the same tile is refused: {out:?}"
    );
    assert_eq!(replies(&out)[0].0, rid(1), "replies leave in request order");
    assert_eq!(c.history().sites().count(), sites + 1, "one site founded");
    assert_eq!(c.claim_of(A), Some(Claim::Fortress(site)));
    assert_eq!(c.claim_of(B), None);
    // One claim per player.
    c.handle(0, embark(3, A, civ, &people[4..6], rc(14, 0)), 0);
    let out = commit(&mut c);
    assert_eq!(reply_to(&out, 3), WorldReply::EmbarkRefused(EmbarkRefusal::AlreadyHasFortress));
    c.handle(0, request(4, A, WorldRequest::StartAdventure(SiteId(0))), 0);
    let out = commit(&mut c);
    assert_eq!(reply_to(&out, 4), WorldReply::AdventureRefused(AdventureRefusal::AlreadyPlaying));
}

#[test]
fn embark_reply_waits_for_save() {
    let (mut c, civ, people) = coordinator();
    // A refusal grants nothing: it leaves at once.
    c.handle(0, embark(1, B, civ, &people[0..2], rc(0, 0)), 0);
    assert_eq!(reply_to(&c.take_msgs(), 1), WorldReply::EmbarkRefused(EmbarkRefusal::Occupied));
    c.handle(0, embark(2, A, civ, &people[0..2], rc(8, 0)), 0);
    c.maintain(0);
    assert!(replies(&c.take_msgs()).is_empty(), "no claim-granting reply before a save");
    // Anything decided after it waits behind it, in order.
    c.handle(0, request(3, B, WorldRequest::PersonInfo(people[3])), 0);
    assert!(c.take_msgs().is_empty(), "later messages keep their place behind the waiting reply");
    let state = c.take_save().expect("a save is due while a reply waits");
    let saved = game::history::persist::decode(&state.history, grid()).expect("the saved history loads");
    assert_eq!(saved.sites().count(), 2, "the save holds the founded site");
    assert_eq!(state.claims, vec![(A, Claim::Fortress(SiteId(1)))], "and the claim");
    assert!(c.take_save().is_none(), "one save at a time");
    assert!(c.take_msgs().is_empty(), "nothing leaves before the save commits");
    c.saved();
    let out = c.take_msgs();
    let r = replies(&out);
    assert_eq!(r.len(), 2, "{out:?}");
    assert!(matches!(r[0], (id, WorldReply::Embarked { .. }) if id == rid(2)), "{r:?}");
    assert!(matches!(r[1], (id, WorldReply::PersonInfo(Some(_))) if id == rid(3)), "{r:?}");
}

#[test]
fn failed_save_keeps_reply_waiting() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    assert!(c.take_save().is_some());
    c.save_failed();
    assert!(c.take_msgs().is_empty(), "a failed save releases nothing");
    assert!(c.take_save().is_some(), "and the next take_save retries");
    c.saved();
    assert!(matches!(reply_to(&c.take_msgs(), 1), WorldReply::Embarked { .. }));
}

#[test]
fn request_replay_answers_from_memo() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let first = reply_to(&commit(&mut c), 1);
    assert!(matches!(first, WorldReply::Embarked { .. }), "{first:?}");
    let sites = c.history().sites().count();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), first, "the replay is answered identically");
    assert_eq!(c.history().sites().count(), sites, "and founds nothing");
    // The memo is saved with the state: a restarted coordinator answers the replay too.
    c.request_save();
    let state = c.take_save().expect("asked for");
    c.saved();
    let mut c2 = Coordinator::from_state(state, grid(), 5_000).expect("state loads");
    c2.take_msgs();
    c2.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 5_000);
    assert_eq!(reply_to(&commit(&mut c2), 1), first, "the restarted coordinator answers from its memo");
    assert_eq!(c2.history().sites().count(), sites, "and founds nothing");
}

#[test]
fn return_in_use_only_while_online() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, region } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(0, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    c.handle(0, request(3, B, WorldRequest::ReturnTo(SiteId(0))), 0);
    assert_eq!(
        reply_to(&commit(&mut c), 3),
        WorldReply::EmbarkRefused(EmbarkRefusal::NotPlayerFounded),
        "only player-founded sites"
    );
    c.handle(0, HostMsg::PlayerOffline { player: A, hop: 1 }, 0);
    c.handle(0, request(4, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 4), WorldReply::Returned { site, region });
    assert_eq!(c.claim_of(B), Some(Claim::Fortress(site)));
    assert_eq!(c.claim_of(A), None, "the offline claimant lost it");
    // A comes back holding the old claim in its record: it is B's now. (Its placement is
    // numbered 2, and its requests from here carry that.)
    c.handle(0, online_at(A, Some(Claim::Fortress(site)), false, 2), 0);
    assert_eq!(c.claim_of(A), None);
    assert!(
        c.take_msgs().iter().any(|m| matches!(m, CoordMsg::ClaimIs { player: A, claim: None, .. })),
        "A's host is told to drop the fortress B plays"
    );
    c.handle(0, HostMsg::Request { id: rid(5), player: A, hop: 2, req: WorldRequest::ReturnTo(site) }, 0);
    assert_eq!(reply_to(&commit(&mut c), 5), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    // B retires: the site is free again.
    c.handle(0, HostMsg::ReleaseClaim { player: B }, 0);
    c.handle(0, HostMsg::Request { id: rid(6), player: A, hop: 2, req: WorldRequest::ReturnTo(site) }, 0);
    assert_eq!(reply_to(&commit(&mut c), 6), WorldReply::Returned { site, region });
}

#[test]
fn directive_bounce_is_redelivered_once() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit(&mut c);
    assert!(matches!(out.as_slice(), [CoordMsg::SpawnPeople { incarnation: 1, events, .. }] if events.is_empty()));
    // An embark into the running region: its party spawns there by directive.
    let party = &people[0..2];
    c.handle(0, embark(1, A, civ, party, r), 0);
    let out = commit(&mut c);
    let spawns: Vec<_> = out
        .iter()
        .filter_map(|m| match m {
            CoordMsg::Directive { directive, incarnation } if directive.region == r => {
                assert_eq!(*incarnation, 1, "stamped with the region's running incarnation");
                Some(directive.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(spawns.len(), party.len(), "{out:?}");

    // The host was stopping `r`: the first spawn bounces, `r` starts again before parking.
    c.handle(0, HostMsg::RegionStopping { rc: r }, 0);
    c.handle(0, HostMsg::DirectiveBounced { directive: spawns[0].clone(), incarnation: 1 }, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    let out = commit(&mut c);
    assert_eq!(events_about(&out, party[0]).len(), 1, "the bounced spawn is delivered once: {out:?}");
    assert!(matches!(events_about(&out, party[0])[0], GameEventKind::SpawnPerson(_)));
    assert!(events_about(&out, party[1]).is_empty(), "the delivered one is left alone: {out:?}");

    // Now both spawns bounce and `r` parks before it starts again: the catch-up is the one body.
    c.handle(0, HostMsg::RegionStopping { rc: r }, 0);
    for d in &spawns {
        c.handle(0, HostMsg::DirectiveBounced { directive: d.clone(), incarnation: 2 }, 0);
    }
    c.handle(0, HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 3 }, 0);
    c.maintain(0);
    let out = commit(&mut c);
    for p in party {
        let ev = events_about(&out, *p);
        assert_eq!(ev.len(), 1, "{p:?} spawns once after the restart: {out:?}");
        assert!(matches!(ev[0], GameEventKind::SpawnPerson(_)));
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r));
    }
    c.maintain(game::TICK_RATE);
    assert!(events_about(&commit(&mut c), party[0]).is_empty(), "and never again");
}

#[test]
fn reattach_keeps_bodies_and_despawns_the_dead() {
    let (mut h, _civ, people) = world();
    let q = people[0];
    // A 170-year-old member of site 0: dies on the first advance.
    let p = h.add_founder(Sex::Male, years_before(start(), 170));
    h.sites.get_mut(&SiteId(0)).unwrap().members.insert(p);
    let pr = h.people.get_mut(&p).unwrap();
    pr.site = Some(SiteId(0));
    pr.location = Location::AtSite(SiteId(0));
    let r = rc(0, 0);
    let mut c = Coordinator::new(h, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit(&mut c);
    assert_eq!(events_about(&out, p).len(), 1, "P has a body");
    assert_eq!(events_about(&out, q).len(), 1, "Q has a body");
    // P dies: the despawn waits for a save; the save commits, then the coordinator crashes
    // before the despawn leaves.
    c.maintain(0);
    assert!(!c.history().person(p).unwrap().alive());
    assert!(events_about(&c.take_msgs(), p).is_empty());
    let state = c.take_save().expect("the despawn waits on a save");
    drop(c);

    let mut c = Coordinator::from_state(state, grid(), 60_000).expect("state loads");
    assert!(matches!(c.take_msgs().as_slice(), [CoordMsg::View { .. }]));
    let running = RunningRegion { rc: r, incarnation: 1, people: vec![(p, [1.0, 2.0, 3.0]), (q, [4.0, 5.0, 6.0])] };
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running: vec![running], online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 60_000);
    let out = commit(&mut c);
    assert!(!out.iter().any(|m| matches!(m, CoordMsg::SpawnPeople { .. })), "a reattach spawns nothing: {out:?}");
    assert_eq!(events_about(&out, q), Vec::<GameEventKind>::new(), "Q's body stands");
    assert_eq!(events_about(&out, p), vec![GameEventKind::DespawnPerson(p)], "P's body goes");
    assert!(out.iter().any(|m| matches!(m, CoordMsg::View { .. })), "the attached host gets the view");
    let qr = c.history().person(q).unwrap();
    assert_eq!(qr.location, Location::InRegion(r));
    assert_eq!(qr.abstract_location(), Location::AtSite(SiteId(0)));
    assert_eq!(c.history().noted_position(q), Some((r, [4.0, 5.0, 6.0])));
    // `r` runs again: a park returns Q to the abstract history.
    c.handle(0, HostMsg::RegionStopping { rc: r }, 60_000);
    c.handle(0, HostMsg::RegionParked { rc: r, people: vec![(q, [4.0, 5.0, 6.0])] }, 60_000);
    assert_eq!(c.history().person(q).unwrap().location, Location::AtSite(SiteId(0)));
}

#[test]
fn state_round_trips() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    c.handle(0, request(2, B, WorldRequest::StartAdventure(SiteId(0))), 0);
    let out = commit(&mut c);
    let WorldReply::Embarked { site, region } = reply_to(&out, 1) else { panic!("embarks") };
    c.request_save();
    let state = c.take_save().expect("asked for");
    c.saved();
    assert_eq!(state.claims, vec![(A, Claim::Fortress(site)), (B, Claim::Adventure(SiteId(0)))]);
    assert_eq!(state.memo.len(), 2);
    let bytes = state.encode();
    assert_eq!(bytes[0], COORDINATOR_STATE_VERSION);
    let back = CoordinatorState::decode(&bytes).expect("decodes");
    assert_eq!(back, state);
    assert!(CoordinatorState::decode(&[COORDINATOR_STATE_VERSION + 1]).is_err(), "an unknown format is refused");
    assert!(CoordinatorState::decode(&[]).is_err());

    let mut c2 = Coordinator::from_state(back, grid(), 0).expect("loads");
    c2.request_save();
    assert_eq!(c2.take_save().expect("asked for"), state, "a loaded state saves back unchanged");
    c2.saved();
    assert_eq!(c2.claim_of(A), Some(Claim::Fortress(site)));
    assert_eq!(c2.claim_of(B), Some(Claim::Adventure(SiteId(0))));
    // Claimants load offline: another player may return to A's fortress.
    let p3 = PlayerId(3);
    c2.handle(0, request(3, p3, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c2), 3), WorldReply::Returned { site, region });
}

#[test]
fn periodic_save_after_changes() {
    let (mut c, _civ, _people) = coordinator();
    assert!(c.take_save().is_none(), "nothing changed, nothing waits");
    c.maintain(1_000);
    assert!(c.take_save().is_none(), "a change saves only once the period passed");
    c.maintain(COORDINATOR_SAVE_SECS * 1000);
    assert!(c.take_save().is_some(), "the period passed with the state changed");
    c.saved();
    assert!(c.take_save().is_none());
}

#[test]
fn inline_link_without_delay_answers_on_the_pump() {
    let (c, _civ, people) = coordinator();
    let mut link: Box<dyn WorldLink + Send> = Box::new(InlineWorldLink::new(c));
    assert!(link.is_up());
    link.send(request(1, A, WorldRequest::PersonInfo(people[0])));
    assert!(link.recv().is_none(), "nothing before the pump");
    link.pump(0);
    let got: Vec<CoordMsg> = std::iter::from_fn(|| link.recv()).collect();
    assert_eq!(replies(&got).len(), 1, "answered on the same pump: {got:?}");
    let inline = link.as_inline_mut().expect("the inline link");
    assert_eq!(inline.coordinator_mut().history().sites().count(), 1);
}

/// Sends a person-info request before each of 20 pumps, then pumps until all are read.
/// Every reply arrives exactly once, in request order, at most `max` pumps after the
/// pump that answered it; with a nonzero `max` some replies are late.
#[test]
fn inline_link_delay_keeps_order() {
    const MAX: u32 = 3;
    let mut any_late = false;
    for seed in 0..SEEDS {
        let (c, _civ, people) = coordinator();
        let mut link = InlineWorldLink::new(c).with_schedule(&InlineSchedule::shuffled(seed).coordinator_delay(MAX));
        let mut got: Vec<(u64, u64)> = Vec::new(); // (request seq, pump read after)
        for step in 1..=40u64 {
            if step <= 20 {
                link.send(request(step, A, WorldRequest::PersonInfo(people[(step % 6) as usize])));
            }
            link.pump(step * game::TICK_RATE);
            while let Some(m) = link.recv() {
                if let CoordMsg::Reply { id, .. } = m {
                    got.push((id.seq, step));
                }
            }
        }
        let seqs: Vec<u64> = got.iter().map(|(s, _)| *s).collect();
        assert_eq!(seqs, (1..=20).collect::<Vec<_>>(), "seed {seed}: every reply once, in order");
        for (seq, read) in &got {
            assert!(read - seq <= u64::from(MAX), "seed {seed}: reply {seq} read at pump {read}, past the delay");
            any_late |= read > seq;
        }
        assert_eq!(link.in_transit(), 0, "seed {seed}");
    }
    assert!(any_late, "the delay delays something");
}

/// The host replays an embark (it saw no answer yet) at a seeded pump while the link
/// delays answers: one site is founded, and both answers are the same.
#[test]
fn replayed_embark_under_delay_founds_once() {
    for seed in 0..SEEDS {
        let (c, civ, people) = coordinator();
        let sites = c.history().sites().count();
        let mut link = InlineWorldLink::new(c).with_delay(seed, 3);
        let replay_at = 1 + seed % 4;
        let mut answers = Vec::new();
        for step in 0..12u64 {
            if step == 0 || step == replay_at {
                link.send(embark(1, A, civ, &people[0..2], rc(8, 0)));
            }
            link.pump(step * game::TICK_RATE);
            while let Some(m) = link.recv() {
                if let CoordMsg::Reply { reply, .. } = m {
                    answers.push(reply);
                }
            }
        }
        assert_eq!(answers.len(), 2, "seed {seed}: both requests answered: {answers:?}");
        assert_eq!(answers[0], answers[1], "seed {seed}: the replay is answered from the memo");
        assert!(matches!(answers[0], WorldReply::Embarked { .. }), "seed {seed}: {answers:?}");
        assert_eq!(link.coordinator().history().sites().count(), sites + 1, "seed {seed}: one site founded");
    }
}

/// The person's body count as the host would apply `msgs` (spawns +1, despawns -1).
fn body_delta(msgs: &[CoordMsg], person: PersonId) -> i32 {
    events_about(msgs, person)
        .iter()
        .map(|e| match e {
            GameEventKind::SpawnPerson(_) => 1,
            GameEventKind::DespawnPerson(_) => -1,
            _ => 0,
        })
        .sum()
}

/// The directives for `r` in `msgs`, with their incarnations.
fn directives_for(msgs: &[CoordMsg], r: RegionCoords) -> Vec<(Directive, u64)> {
    msgs.iter()
        .filter_map(|m| match m {
            CoordMsg::Directive { directive, incarnation } if directive.region == r => {
                Some((directive.clone(), *incarnation))
            }
            _ => None,
        })
        .collect()
}

/// A spawn decided for incarnation 1 reaches the host after the region parked and
/// started again (incarnation 2): its bounce is stale and dropped, so a later stop and
/// start without a park cannot hand the person a second body.
#[test]
fn stale_bounce_is_dropped() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(0, embark(1, A, civ, &[p], r), 0);
    let out = commit(&mut c);
    let spawn = directives_for(&out, r);
    assert_eq!(spawn.len(), 1, "{out:?}");
    assert_eq!(spawn[0].1, 1);
    let mut bodies = 0; // P's bodies on the host: the spawn has not been applied yet
    // The host parks `r` (no body of P in it) and starts it again before the spawn lands.
    c.handle(0, HostMsg::RegionStopping { rc: r }, 0);
    c.handle(0, HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    let out = commit(&mut c);
    bodies += body_delta(&out, p);
    assert_eq!(bodies, 1, "incarnation 2's catch-up spawns P: {out:?}");
    // The delayed incarnation-1 spawn is bounced by the host (wrong incarnation).
    c.handle(0, HostMsg::DirectiveBounced { directive: spawn[0].0.clone(), incarnation: 1 }, 0);
    // Stop and start again without a park (P's body stays).
    c.handle(0, HostMsg::RegionStopping { rc: r }, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 3 }, 0);
    c.maintain(0);
    let out = commit(&mut c);
    bodies += body_delta(&out, p);
    assert_eq!(bodies, 1, "P keeps exactly one body: {out:?}");
}

/// A region that started while the link was down (its `RegionStarted` lost) is listed
/// by the next `Attach` with its exact bodies — none: its residents get the start's
/// catch-up then (spawned once each, after the echo), not at the region's next start; a
/// later attach listing their bodies spawns nobody again.
#[test]
fn attach_of_an_unannounced_region_spawns_its_residents_once() {
    let (mut c, _, people) = coordinator();
    let r = rc(0, 0);
    let running = |bodies: Vec<(PersonId, [f32; 3])>| RunningRegion { rc: r, incarnation: 1, people: bodies };
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running: vec![running(Vec::new())], online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 0);
    let out = commit(&mut c);
    let attached = out.iter().position(|m| matches!(m, CoordMsg::Attached { epoch: 1 })).expect("the echo");
    for p in &people {
        assert_eq!(body_delta(&out[attached..], *p), 1, "{p:?} spawned once: {out:?}");
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r));
    }
    // Re-attached with every body there: nothing more.
    let bodies: Vec<(PersonId, [f32; 3])> = people.iter().map(|p| (*p, [0.0; 3])).collect();
    c.handle(0, HostMsg::Attach { host: 0, epoch: 2, running: vec![running(bodies)], online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 0);
    let out = commit(&mut c);
    assert!(directives_for(&out, r).is_empty(), "{out:?}");
}

/// A person crossing a seam while the host gathers its `Attach` is in neither region's
/// own list; the host puts them in exactly one (`WorldManager`'s exact-across-seams rule):
/// delivered into listed B → B's list, absorbed at the seam → the list of A they left.
/// With the held `PersonCrossed` following the `Attach`, P keeps exactly one body — the
/// attach's catch-up spawns nobody beside it — and an absorbed P none.
#[test]
fn crossing_held_behind_the_attach_keeps_one_body() {
    let (mut c, _, people) = coordinator();
    let (a, b, x) = (rc(0, 0), rc(1, 0), rc(2, 0));
    c.handle(0, HostMsg::RegionStarted { rc: a, incarnation: 1 }, 0);
    c.handle(0, HostMsg::RegionStarted { rc: b, incarnation: 1 }, 0);
    let out = commit(&mut c);
    let (p, q) = (people[0], people[1]);
    // Bodies on the host: everyone spawned into A once.
    let mut count = |p, out: &[CoordMsg], n: &mut i32| *n += body_delta(out, p);
    let (mut p_bodies, mut q_bodies) = (0, 0);
    count(p, &out, &mut p_bodies);
    count(q, &out, &mut q_bodies);
    assert_eq!((p_bodies, q_bodies), (1, 1), "{out:?}");
    // P crosses into B (delivered, in flight), Q is absorbed at the seam into X: the
    // host lists P in B and Q still in A; their crossings follow the attach.
    q_bodies -= 1;
    let at = [0.0; 3];
    let a_list: Vec<(PersonId, [f32; 3])> = people.iter().filter(|&&r| r != p).map(|r| (*r, at)).collect();
    let running = vec![
        RunningRegion { rc: a, incarnation: 1, people: a_list },
        RunningRegion { rc: b, incarnation: 1, people: vec![(p, at)] },
    ];
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running, online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 0);
    let out = commit(&mut c);
    count(p, &out, &mut p_bodies);
    count(q, &out, &mut q_bodies);
    assert_eq!((p_bodies, q_bodies), (1, 0), "nobody spawned beside a crossing: {out:?}");
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(b));
    c.handle(0, HostMsg::PersonCrossed { person: p, to: b, delivered: true }, 0);
    c.handle(0, HostMsg::PersonCrossed { person: q, to: x, delivered: false }, 0);
    c.maintain(0);
    let out = commit(&mut c);
    count(p, &out, &mut p_bodies);
    count(q, &out, &mut q_bodies);
    assert_eq!((p_bodies, q_bodies), (1, 0), "{out:?}");
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(b));
    assert!(!matches!(c.history().person(q).unwrap().location, Location::InRegion(_)), "Q is abstract");
}

/// A directive waiting on a save when the host re-attaches is superseded: the attach
/// re-decides the region from its bodies — the stale spawn never leaves, and P, a
/// resident of the running region with no body there, gets one fresh spawn decided after
/// the attach (its catch-up), not one per decision.
#[test]
fn attach_supersedes_unsent_directives() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(0, embark(1, A, civ, &[p], r), 0);
    // A later ungated message waits behind the spawn.
    c.handle(0, request(2, B, WorldRequest::PersonInfo(people[3])), 0);
    assert!(c.take_msgs().is_empty());
    // The link dropped; the host re-attaches running `r` with no body of P.
    let running = RunningRegion { rc: r, incarnation: 1, people: Vec::new() };
    let online = vec![OnlinePlayer { player: A, claim: None, claim_pending: true, report: 1, hop: 1 }];
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running: vec![running], online, generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 0);
    let out = commit(&mut c);
    let attached = out.iter().position(|m| matches!(m, CoordMsg::Attached { epoch: 1 })).expect("the echo");
    assert!(directives_for(&out[..attached], r).is_empty(), "the stale spawn is dropped: {out:?}");
    assert!(matches!(reply_to(&out, 1), WorldReply::Embarked { .. }), "the reply still leaves");
    assert!(matches!(reply_to(&out, 2), WorldReply::PersonInfo(Some(_))));
    assert_eq!(body_delta(&out[attached..], p), 1, "one spawn of P, decided by the attach: {out:?}");
    assert_eq!(directives_for(&out[attached..], r).iter().map(|d| d.1).collect::<Vec<_>>(), vec![1]);
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(r), "P is embodied in r");
    // `r` parks with P's body and starts again: P is spawned once by its catch-up.
    c.handle(0, HostMsg::RegionStopping { rc: r }, 0);
    c.handle(0, HostMsg::RegionParked { rc: r, people: vec![(p, [0.0; 3])] }, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    assert_eq!(body_delta(&commit(&mut c), p), 1);
}

/// A body that crossed between two running regions while the link was down is listed
/// in its new region only: whichever region is reattached first, it keeps that body.
#[test]
fn reattach_keeps_a_body_that_crossed_between_listed_regions() {
    for order in [false, true] {
        let (mut c, _civ, people) = coordinator();
        let (r0, r1) = (rc(0, 0), rc(1, 0));
        c.handle(0, HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
        c.handle(0, HostMsg::RegionStarted { rc: r1, incarnation: 1 }, 0);
        commit(&mut c);
        let p = people[0];
        assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(r0));
        let others: Vec<_> = people[1..].iter().map(|q| (*q, [0.0; 3])).collect();
        let mut running = vec![
            RunningRegion { rc: r0, incarnation: 1, people: others },
            RunningRegion { rc: r1, incarnation: 1, people: vec![(p, [0.0; 3])] },
        ];
        if order {
            running.reverse();
        }
        c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running, online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 0);
        let out = commit(&mut c);
        assert_eq!(body_delta(&out, p), 0, "order {order}: no despawn of P's only body: {out:?}");
        assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(r1), "order {order}");
    }
}

/// The host delivered a crossing body into a region the history did not have running:
/// the host wins, and the history has the person embodied there.
#[test]
fn delivered_crossing_is_adopted() {
    let (mut c, _civ, people) = coordinator();
    let (r0, r1) = (rc(0, 0), rc(1, 0));
    c.handle(0, HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(0, HostMsg::PersonCrossed { person: p, to: r1, delivered: true }, 0);
    let out = commit(&mut c);
    assert_eq!(body_delta(&out, p), 0, "{out:?}");
    let pr = c.history().person(p).unwrap();
    assert_eq!(pr.location, Location::InRegion(r1));
    // Parking `r1` hands P back to the abstract history.
    c.handle(0, HostMsg::RegionStopping { rc: r1 }, 0);
    c.handle(0, HostMsg::RegionParked { rc: r1, people: vec![(p, [0.0; 3])] }, 0);
    assert_ne!(c.history().person(p).unwrap().location, Location::InRegion(r1));
}

/// The same bounce twice (a resent message) before the region starts again: the spawn is
/// redelivered once.
#[test]
fn duplicated_bounce_is_redelivered_once() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(0, embark(1, A, civ, &[p], r), 0);
    let spawn = directives_for(&commit(&mut c), r);
    assert_eq!(spawn.len(), 1);
    c.handle(0, HostMsg::RegionStopping { rc: r }, 0);
    for _ in 0..2 {
        c.handle(0, HostMsg::DirectiveBounced { directive: spawn[0].0.clone(), incarnation: 1 }, 0);
    }
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    let out = commit(&mut c);
    assert_eq!(body_delta(&out, p), 1, "one spawn of P: {out:?}");
}

fn online(player: PlayerId, claim: Option<Claim>, claim_pending: bool) -> HostMsg {
    online_at(player, claim, claim_pending, 1)
}

/// [`online`] at placement `hop`.
fn online_at(player: PlayerId, claim: Option<Claim>, claim_pending: bool, hop: u64) -> HostMsg {
    HostMsg::PlayerOnline(OnlinePlayer { player, claim, claim_pending, report: 1, hop })
}

/// The host's report is the truth for an online player without an unsettled request: a
/// claim it does not hold is dropped (a release lost to a crash), one it holds and the
/// coordinator lacks is taken when free (a grant lost to a crash), and a fortress another
/// online player holds is refused back to the host. With a request unsettled the
/// coordinator's claim stands.
#[test]
fn host_report_reconciles_claims() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    // A request of A unsettled on the host: the claim stands.
    c.handle(0, online(A, None, true), 0);
    assert_eq!(c.claim_of(A), Some(Claim::Fortress(site)));
    // Settled, and the host holds nothing: the claim goes (and is saved at once).
    commit(&mut c);
    c.handle(0, online(A, None, false), 0);
    assert_eq!(c.claim_of(A), None);
    assert!(c.save_due(), "a dropped claim is saved at once");
    commit(&mut c);
    // The host holds the fortress the coordinator lost: taken back.
    c.handle(0, online(A, Some(Claim::Fortress(site)), false), 0);
    assert_eq!(c.claim_of(A), Some(Claim::Fortress(site)));
    // B's host claims the same fortress while A plays it: refused, never two holders.
    c.handle(0, online(B, Some(Claim::Fortress(site)), false), 0);
    assert_eq!(c.claim_of(B), None);
    assert!(commit(&mut c).iter().any(|m| matches!(m, CoordMsg::ClaimIs { player: B, claim: None, .. })));
    // A's host reports an adventure instead: the fortress is freed, the adventure taken.
    c.handle(0, online(A, Some(Claim::Adventure(SiteId(0))), false), 0);
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(SiteId(0))));
    c.handle(0, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert!(matches!(reply_to(&commit(&mut c), 2), WorldReply::Returned { .. }), "the fortress is free");
}

/// A release is saved at once: the state saved right after it holds no claim.
#[test]
fn release_claim_forces_a_save() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    commit(&mut c);
    assert!(!c.save_due());
    c.handle(0, HostMsg::ReleaseClaim { player: A }, 0);
    let state = c.take_save().expect("a release forces a save");
    assert!(state.claims.is_empty(), "{:?}", state.claims);
}

/// A timed-out request is settled, never executed: unapplied, it is refused (and the
/// refusal memoised, so a late copy of the request founds nothing); applied, its memo
/// answers.
#[test]
fn settle_answers_without_executing() {
    let (mut c, civ, people) = coordinator();
    let sites = c.history().sites().count();
    let req = WorldRequest::Embark(EmbarkRequest { civ, people: people[0..2].to_vec(), region: rc(8, 0) });
    c.handle(0, HostMsg::Settle { id: rid(1), req: req.clone() }, 0);
    assert_eq!(reply_to(&commit(&mut c), 1), WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable));
    c.handle(0, request(1, A, req.clone()), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable));
    assert_eq!(c.history().sites().count(), sites, "nothing founded");
    assert_eq!(c.claim_of(A), None);
    // Applied before the settle: the memo answers.
    c.handle(0, request(2, A, req.clone()), 0);
    let first = reply_to(&commit(&mut c), 2);
    assert!(matches!(first, WorldReply::Embarked { .. }));
    c.handle(0, HostMsg::Settle { id: rid(2), req }, 0);
    assert_eq!(reply_to(&commit(&mut c), 2), first);
    assert_eq!(c.history().sites().count(), sites + 1);
}

/// The saved state's format ladder: a blob of every older format still loads.
mod state {
    use super::*;

    /// A small real state: one claim, one memo entry.
    fn small_state() -> CoordinatorState {
        let (mut c, civ, people) = coordinator();
        c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
        commit(&mut c);
        c.request_save();
        let state = c.take_save().expect("asked for");
        c.saved();
        state
    }

    // The fixture was made by `small_state().encode()` at commit b46a6aee; never regenerate it.
    #[test]
    fn v1_blob_still_decodes() {
        let state = CoordinatorState::decode(include_bytes!("fixtures/coordinator_state_v1.bin")).expect("a v1 blob decodes");
        let expected = small_state();
        assert_eq!(state.claims, expected.claims);
        assert_eq!(state.memo, expected.memo);
        game::history::persist::decode(&state.history, grid()).expect("its history loads");
    }

    #[test]
    fn unknown_version_is_refused() {
        let err = CoordinatorState::decode(&[200, 0, 0]).expect_err("an unknown format is refused");
        assert!(matches!(err, game::history::persist::HistoryLoadError::Corrupt(ref m) if m.contains("200")), "{err:?}");
    }
}

// ---- Many hosts (R3 Task 10) ----------------------------------------------------------

/// Save as a store that commits at once would, then take what may leave, with targets.
fn commit_routed(c: &mut Coordinator) -> Vec<(Target, CoordMsg)> {
    if c.take_save().is_some() {
        c.saved();
    }
    c.take_out()
}

fn attach(host: u32, epoch: u64, running: Vec<RunningRegion>, online: Vec<OnlinePlayer>) -> HostMsg {
    HostMsg::Attach { host, epoch, running, online, generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }
}

fn report(player: PlayerId, claim: Option<Claim>, claim_pending: bool) -> OnlinePlayer {
    OnlinePlayer { player, claim, claim_pending, report: 1, hop: 1 }
}

/// Host 0 runs site 0's region with its people and has A online in A's fortress; host 1
/// then attaches (running another region, B online): host 0's regions keep their bodies
/// and A stays online (another player's return to A's fortress is refused), the echo and
/// view go to host 1 only. A re-attach of host 1 running nothing parks only host 1's
/// region.
#[test]
fn attach_from_host_1_keeps_host_0_regions_and_players() {
    let (mut c, civ, people) = coordinator();
    let (r0, r1) = (rc(0, 0), rc(14, 0));
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    let here: Vec<(PersonId, [f32; 3])> = people[2..].iter().map(|p| (*p, [0.0; 3])).collect();
    let running0 = vec![RunningRegion { rc: r0, incarnation: 1, people: here }];
    c.handle(0, attach(0, 1, running0, vec![report(A, Some(Claim::Fortress(site)), false)]), 0);
    commit(&mut c);
    let running1 = vec![RunningRegion { rc: r1, incarnation: 1, people: Vec::new() }];
    c.handle(1, attach(1, 1, running1, vec![report(B, None, false)]), 0);
    let out = commit_routed(&mut c);
    for (to, m) in &out {
        if matches!(m, CoordMsg::Attached { .. } | CoordMsg::View { .. }) {
            assert_eq!(*to, Target::Host(1), "host 1's echo and view go to host 1: {m:?}");
        }
        if let CoordMsg::Directive { directive, .. } = m {
            assert_ne!(directive.region, r0, "host 0's region is not re-decided: {m:?}");
        }
    }
    for p in &people[2..] {
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r0), "{p:?} keeps its body");
    }
    assert_eq!((c.region_host(r0), c.region_host(r1)), (Some(0), Some(1)));
    assert_eq!((c.online_on(A), c.online_on(B)), (Some(0), Some(1)));
    c.handle(1, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::EmbarkRefused(EmbarkRefusal::InUse), "A is still online");
    // Host 1 re-attaches running nothing, B gone: only its region parks, only B goes offline.
    c.handle(1, attach(1, 2, Vec::new(), Vec::new()), 0);
    commit(&mut c);
    assert_eq!((c.region_host(r0), c.region_host(r1)), (Some(0), None), "only host 1's region parked");
    for p in &people[2..] {
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r0), "{p:?} keeps its body");
    }
    assert_eq!((c.online_on(A), c.online_on(B)), (Some(0), None));
    c.handle(1, request(3, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 3), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
}

/// A hand-off: A's `PlayerOnline` from the new host (1) arrives before its
/// `PlayerOffline` from the old one (0): the late offline is ignored — A's fortress stays
/// in use; A going offline on host 1 frees it.
#[test]
fn player_offline_from_the_old_host_after_online_on_the_new_is_ignored() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, region } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(0, online(A, Some(Claim::Fortress(site)), false), 0);
    assert_eq!(c.online_on(A), Some(0));
    c.handle(1, online_at(A, Some(Claim::Fortress(site)), false, 2), 0);
    c.handle(0, HostMsg::PlayerOffline { player: A, hop: 1 }, 0);
    assert_eq!(c.online_on(A), Some(1), "online on the new host");
    assert!(!commit(&mut c).iter().any(|m| matches!(m, CoordMsg::ClaimIs { .. })), "nothing to correct");
    c.handle(0, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    c.handle(1, HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
    assert_eq!(c.online_on(A), None);
    c.handle(0, request(3, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 3), WorldReply::Returned { site, region });
}

/// The `ClaimIs` messages for `player` in `out`, with their targets.
fn claim_is(out: &[(Target, CoordMsg)], player: PlayerId) -> Vec<(Target, Option<Claim>)> {
    out.iter()
        .filter_map(|(to, m)| match m {
            CoordMsg::ClaimIs { player: p, claim, .. } if *p == player => Some((*to, *claim)),
            _ => None,
        })
        .collect()
}

/// A host's report that differs from the coordinator's claim is answered with the claim
/// the coordinator ends with, to the reporting host only: (1) the coordinator holds none
/// and cannot take the reported fortress (another online player plays it); (2) it holds
/// another fortress and the report comes with a request unsettled (its own stands); (3) the
/// report holds nothing with a request unsettled whose grant the coordinator already made.
/// A report that agrees gets nothing. The host applies the answer as is (see
/// `world_manager::claim_is_sets_the_host_claim`).
#[test]
fn claim_is_answers_a_differing_report() {
    let c_player = PlayerId(3);
    // (1) Another online player (on host 9) plays the fortress A's host reports.
    let (mut c, civ, people) = coordinator();
    c.handle(9, embark(1, c_player, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(2, online(A, Some(Claim::Fortress(site)), false), 0);
    let out = commit_routed(&mut c);
    assert_eq!(c.claim_of(A), None);
    assert_eq!(claim_is(&out, A), vec![(Target::Host(2), None)], "{out:?}");

    // (2) A holds fortress s1; its host reports s2 with a request unsettled: s1 stands.
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site: s1, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(0, embark(2, B, civ, &people[2..4], rc(14, 0)), 0);
    let WorldReply::Embarked { site: s2, .. } = reply_to(&commit(&mut c), 2) else { panic!("embarks") };
    c.handle(0, HostMsg::ReleaseClaim { player: B }, 0);
    c.handle(1, online(A, Some(Claim::Fortress(s2)), true), 0);
    let out = commit_routed(&mut c);
    assert_eq!(c.claim_of(A), Some(Claim::Fortress(s1)));
    assert_eq!(claim_is(&out, A), vec![(Target::Host(1), Some(Claim::Fortress(s1)))], "{out:?}");

    // (3) The embark is granted; the host still reports nothing with it unsettled.
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(0, online(A, None, true), 0);
    let out = commit_routed(&mut c);
    assert_eq!(claim_is(&out, A), vec![(Target::Host(0), Some(Claim::Fortress(site)))], "{out:?}");
    // A report that agrees: nothing.
    c.handle(0, online(A, Some(Claim::Fortress(site)), false), 0);
    assert!(claim_is(&commit_routed(&mut c), A).is_empty());
}

/// A directive for a region no host has announced is never broadcast: it is held as a
/// bounce and delivered by the region's next start, to the host that starts it.
#[test]
fn directive_for_an_unannounced_region_is_deferred_not_broadcast() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(1, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    // Host 1 parks `r` (gone from the coordinator's view) while an embark into it founds a
    // site whose party would spawn there.
    let party = &people[0..2];
    c.handle(1, HostMsg::RegionStopping { rc: r }, 0);
    c.handle(1, HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    assert_eq!(c.region_host(r), None);
    c.history_mut().region_started(r); // the history alone believes `r` runs
    c.handle(0, embark(1, A, civ, party, r), 0);
    let out = commit_routed(&mut c);
    assert!(
        !out.iter().any(|(_, m)| matches!(m, CoordMsg::Directive { directive, .. } if directive.region == r)),
        "no directive for an unannounced region leaves: {out:?}"
    );
    // Host 0 starts `r`: the held spawns come with its catch-up, to host 0.
    c.handle(0, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit_routed(&mut c);
    let spawns: Vec<&(Target, CoordMsg)> =
        out.iter().filter(|(_, m)| matches!(m, CoordMsg::SpawnPeople { rc, .. } if *rc == r)).collect();
    assert_eq!(spawns.len(), 1, "{out:?}");
    assert_eq!(spawns[0].0, Target::Host(0));
    let msgs: Vec<CoordMsg> = out.into_iter().map(|(_, m)| m).collect();
    for p in party {
        assert_eq!(body_delta(&msgs, *p), 1, "{p:?} spawns once, on host 0: {msgs:?}");
    }
}

/// A hands P (holding fortress S) to B: P stays online — in transit — so another player's
/// return to S is refused before B says anything; B's `PlayerOnline` confirms (the claim
/// P's, online on B); B's `PlayerOffline` frees S.
#[test]
fn handed_player_stays_online_until_the_new_host_reports() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, region } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(0, HostMsg::PlayerHandedTo { player: A, to: 1, hop: 1 }, 0);
    assert_eq!(c.online_on(A), Some(1), "in transit to host 1");
    c.handle(0, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::EmbarkRefused(EmbarkRefusal::InUse), "in use while in transit");
    c.handle(1, online_at(A, Some(Claim::Fortress(site)), false, 2), 0);
    assert!(claim_is(&commit_routed(&mut c), A).is_empty(), "nothing to correct");
    assert_eq!((c.online_on(A), c.claim_of(A)), (Some(1), Some(Claim::Fortress(site))));
    c.handle(0, request(3, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 3), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    c.handle(1, HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
    c.handle(0, request(4, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 4), WorldReply::Returned { site, region });
}

/// A `PlayerHandedTo` at the player's latest hop from a host the player is not online
/// on (a stale or a stray message) changes nothing; nor does one for a player online
/// nowhere. Only the equal-hop stray: one at a higher hop is taken as the end of a
/// placement on its sender not heard yet (only a holder hands a player off).
#[test]
fn handed_to_from_a_host_that_does_not_have_the_player_is_ignored() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, region } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(0, online(A, Some(Claim::Fortress(site)), false), 0);
    c.handle(2, HostMsg::PlayerHandedTo { player: A, to: 3, hop: 1 }, 0);
    assert_eq!(c.online_on(A), Some(0));
    c.handle(0, HostMsg::PlayerOffline { player: A, hop: 1 }, 0);
    assert_eq!(c.online_on(A), None);
    c.handle(0, HostMsg::PlayerHandedTo { player: A, to: 1, hop: 1 }, 0);
    assert_eq!(c.online_on(A), None, "online nowhere: nothing to hand");
    c.handle(0, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::Returned { site, region });
}

/// P in transit to host 1, which (re)attaches without it: the transit ends (offline). And
/// host 1 parking P on arrival (its `PlayerOffline`, at the hop after host 0's) heard
/// before host 0's `PlayerHandedTo` takes P offline at once; the hand-over, older, then
/// changes nothing.
#[test]
fn attach_of_the_new_host_without_the_player_ends_the_transit() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(0, HostMsg::PlayerHandedTo { player: A, to: 1, hop: 1 }, 0);
    c.handle(1, attach(1, 1, Vec::new(), Vec::new()), 0);
    assert_eq!(c.online_on(A), None);
    c.handle(0, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert!(matches!(reply_to(&commit(&mut c), 2), WorldReply::Returned { .. }));
    c.handle(0, HostMsg::ReleaseClaim { player: B }, 0);
    // A, online again on host 0 with the fortress, is handed to host 1 — whose park on
    // arrival is heard first.
    c.handle(0, online_at(A, Some(Claim::Fortress(site)), false, 2), 0);
    assert_eq!(c.claim_of(A), Some(Claim::Fortress(site)));
    c.handle(1, HostMsg::PlayerOffline { player: A, hop: 3 }, 0);
    assert_eq!(c.online_on(A), None, "parked on arrival: offline");
    c.handle(0, HostMsg::PlayerHandedTo { player: A, to: 1, hop: 2 }, 0);
    assert_eq!(c.online_on(A), None, "the older hand-over changes nothing");
    c.handle(0, request(3, B, WorldRequest::ReturnTo(site)), 0);
    assert!(matches!(reply_to(&commit(&mut c), 3), WorldReply::Returned { .. }));
}

/// A region's messages go to the host that runs it, not the requester's: an embark by
/// host 0 into a region host 1 runs sends the founders' spawns and the founding notice to
/// host 1, the reply to host 0; a notice for a region nobody runs goes to every host.
#[test]
fn notice_and_directive_go_to_the_regions_host() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(1, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    c.handle(0, embark(1, A, civ, &people[0..2], r), 0);
    c.maintain(0);
    let out = commit_routed(&mut c);
    let to_of = |want: fn(&CoordMsg) -> bool| -> Vec<Target> { out.iter().filter(|(_, m)| want(m)).map(|(t, _)| *t).collect() };
    let directives = to_of(|m| matches!(m, CoordMsg::Directive { .. }));
    assert!(!directives.is_empty() && directives.iter().all(|t| *t == Target::Host(1)), "{out:?}");
    assert_eq!(to_of(|m| matches!(m, CoordMsg::Reply { .. })), vec![Target::Host(0)]);
    let notices = to_of(|m| matches!(m, CoordMsg::Notice { .. }));
    assert!(!notices.is_empty() && notices.iter().all(|t| *t == Target::Host(1)), "{out:?}");
    // Into a region nobody runs: its notice (the founding, for the player) to every host.
    c.handle(0, embark(2, B, civ, &people[2..4], rc(14, 0)), 0);
    c.maintain(0);
    let out = commit_routed(&mut c);
    let notices: Vec<Target> =
        out.iter().filter(|(_, m)| matches!(m, CoordMsg::Notice { region, .. } if *region == rc(14, 0))).map(|(t, _)| *t).collect();
    assert_eq!(notices, vec![Target::Every], "{out:?}");
}

// ---- The answer memo, pruned by acknowledgement (R3 Task 11) -------------------------

/// The memoised request ids, oldest first.
fn memo_ids(c: &Coordinator) -> Vec<RequestId> {
    c.state().memo.into_iter().map(|(id, _)| id).collect()
}

fn request_as(id: RequestId, player: PlayerId, req: WorldRequest) -> HostMsg {
    HostMsg::Request { id, player, hop: PLACED, req }
}

/// More answers than the old count cap, none settled: every one is kept, so a replay of
/// the first still finds its `Embarked` and founds nothing.
#[test]
fn memo_keeps_unsettled_answers_past_the_old_cap() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let first = reply_to(&commit(&mut c), 1);
    assert!(matches!(first, WorldReply::Embarked { .. }), "{first:?}");
    let sites = c.history().sites().count();
    for seq in 2..=ANSWERED_MEMO_MAX as u64 + 1 {
        c.handle(0, embark(seq, A, civ, &people[2..4], rc(14, 0)), 0);
        c.take_out();
    }
    commit(&mut c);
    assert_eq!(memo_ids(&c).len(), ANSWERED_MEMO_MAX + 1, "nothing unsettled is forgotten");
    c.handle(0, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), first, "the first answer is replayed");
    assert_eq!(c.history().sites().count(), sites, "and founds nothing");
}

/// `Settled { boot, below }` from a host removes exactly that host's entries of that boot
/// below the watermark; an `Attach`'s `settled_below` does the same.
#[test]
fn settled_watermark_prunes_exactly_below() {
    let (mut c, civ, people) = coordinator();
    for seq in 1..=4 {
        c.handle(0, embark(seq, A, civ, &people[0..2], rc(8, 0)), 0);
    }
    let other_host = RequestId { host: 1, boot: 7, seq: 1 };
    c.handle(1, request_as(other_host, B, WorldRequest::StartAdventure(SiteId(0))), 0);
    commit(&mut c);
    assert_eq!(memo_ids(&c).len(), 5);
    c.handle(0, HostMsg::Settled { boot: 7, below: 3 }, 0);
    assert_eq!(memo_ids(&c), vec![rid(3), rid(4), other_host], "seq 1 and 2 of host 0 go");
    // From host 1: its own entries only (none of boot 7 below 1 … then all of them).
    c.handle(1, HostMsg::Settled { boot: 7, below: 1 }, 0);
    assert_eq!(memo_ids(&c), vec![rid(3), rid(4), other_host]);
    c.handle(0, attach(0, 1, Vec::new(), Vec::new()), 0);
    assert_eq!(memo_ids(&c), vec![rid(3), rid(4), other_host], "an attach watermark of 0 prunes nothing");
    c.handle(0, HostMsg::Attach { host: 0, epoch: 2, running: Vec::new(), online: Vec::new(), generation: 0, boot: 7, settled_below: 4, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 0);
    assert_eq!(memo_ids(&c), vec![rid(4), other_host], "the attach's watermark prunes below it");
    c.handle(1, HostMsg::Settled { boot: 7, below: 2 }, 0);
    assert_eq!(memo_ids(&c), vec![rid(4)]);
}

/// A host's `Attach` with a boot the coordinator has not seen: that host's old process
/// is gone, so its entries of every other boot go; other hosts' entries and the new
/// boot's stay (a re-attach of the same boot keeps them).
#[test]
fn new_boot_attach_drops_the_old_boots_memo() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, attach(0, 1, Vec::new(), Vec::new()), 0);
    for seq in 1..=2 {
        c.handle(0, embark(seq, A, civ, &people[0..2], rc(8, 0)), 0);
    }
    let other_host = RequestId { host: 1, boot: 7, seq: 1 };
    c.handle(1, request_as(other_host, B, WorldRequest::StartAdventure(SiteId(0))), 0);
    commit(&mut c);
    let boot = |seq| RequestId { host: 0, boot: 9, seq };
    let attach_boot_9 = |epoch| HostMsg::Attach {
        host: 0,
        epoch,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot: 9,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
    };
    c.handle(0, attach_boot_9(2), 0);
    assert_eq!(memo_ids(&c), vec![other_host], "boot 7 of host 0 is gone");
    c.handle(0, request_as(boot(0), B, WorldRequest::ReturnTo(SiteId(0))), 0);
    commit(&mut c);
    c.handle(0, attach_boot_9(3), 0);
    assert_eq!(memo_ids(&c), vec![other_host, boot(0)], "a re-attach of the same boot keeps its answers");
}

// ---- Player hops order presence across hosts (R3 Task 11) ----------------------------

const HOST_A: u32 = 0;
const HOST_B: u32 = 1;

/// A coordinator where player A has embarked (fortress `site`, its region) and is online
/// on host A at hop 1.
fn a_online_on_host_a() -> (Coordinator, SiteId, RegionCoords) {
    let (mut c, site, region) = a_embarked_on_host_a();
    c.handle(HOST_A, online_at(A, Some(Claim::Fortress(site)), false, 1), 0);
    commit(&mut c);
    (c, site, region)
}

/// Player A has embarked through host A (fortress `site`); no presence report yet.
fn a_embarked_on_host_a() -> (Coordinator, SiteId, RegionCoords) {
    let (mut c, civ, people) = coordinator();
    c.handle(HOST_A, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, region } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    (c, site, region)
}

/// Whether another player (B) may return to `site` now: the claim is offline. A granted
/// return is given back at once, so the probe changes nothing.
fn returnable(c: &mut Coordinator, site: SiteId, id: u64) -> bool {
    c.handle(HOST_A, request(id, B, WorldRequest::ReturnTo(site)), 0);
    match reply_to(&commit(c), id) {
        WorldReply::Returned { .. } => {
            c.handle(HOST_A, HostMsg::ReleaseClaim { player: B }, 0);
            commit(c);
            true
        }
        WorldReply::EmbarkRefused(EmbarkRefusal::InUse) => false,
        other => panic!("unexpected answer to the probe: {other:?}"),
    }
}

/// A → B → A, then a disconnect on A, as hosts A and B send it (in send order per host):
/// A's placement at hop 1 and its hand-over, its placement at hop 3 and its end; B's
/// placement at hop 2 and its hand-over back.
fn round_trip(site: SiteId, disconnect: bool) -> (Vec<HostMsg>, Vec<HostMsg>) {
    let fortress = Some(Claim::Fortress(site));
    let mut a = vec![
        online_at(A, fortress, false, 1),
        HostMsg::PlayerHandedTo { player: A, to: HOST_B, hop: 1 },
        online_at(A, fortress, false, 3),
    ];
    if disconnect {
        a.push(HostMsg::PlayerOffline { player: A, hop: 3 });
    }
    let b = vec![online_at(A, fortress, false, 2), HostMsg::PlayerHandedTo { player: A, to: HOST_A, hop: 2 }];
    (a, b)
}

/// The reviewer's order: B's link lags behind all of A's messages. B's placement and
/// hand-over back, older than A's last placement, change nothing: A ends offline and the
/// fortress is free for another player.
#[test]
fn round_trip_then_disconnect_with_a_lagging_link_ends_offline() {
    let (mut c, site, _) = a_embarked_on_host_a();
    let (a, b) = round_trip(site, true);
    for m in a {
        c.handle(HOST_A, m, 0);
    }
    for m in b {
        c.handle(HOST_B, m, 0);
    }
    assert_eq!(c.online_on(A), None, "offline after the disconnect");
    assert_eq!(c.hop_of(A), 3);
    assert!(returnable(&mut c, site, 10), "the fortress is free for another player");
}

/// Every interleaving of A's and B's messages for A → B → A (with and without the final
/// disconnect) that keeps each host's own order ends as the in-order delivery does.
#[test]
fn every_arrival_order_of_a_round_trip_converges() {
    for disconnect in [false, true] {
        let (a_len, b_len) = {
            let (a, b) = round_trip(SiteId(0), disconnect);
            (a.len(), b.len())
        };
        let n = a_len + b_len;
        let mut orders = 0;
        // Each mask with `b_len` bits set picks the arrival slots of B's messages.
        for mask in 0u32..(1 << n) {
            if mask.count_ones() as usize != b_len {
                continue;
            }
            orders += 1;
            let (mut c, site, _) = a_embarked_on_host_a();
            let (a, b) = round_trip(site, disconnect);
            let (mut a, mut b) = (a.into_iter(), b.into_iter());
            let mut hop_is = 0;
            for slot in 0..n {
                if mask & (1 << slot) != 0 {
                    c.handle(HOST_B, b.next().unwrap(), 0);
                } else {
                    c.handle(HOST_A, a.next().unwrap(), 0);
                }
                hop_is += c.take_out().iter().filter(|(_, m)| matches!(m, CoordMsg::HopIs { .. })).count();
            }
            // In order: A online, hands to B; B online, hands back; A online (… offline).
            if mask == 0b1100 {
                assert_eq!(hop_is, 0, "disconnect {disconnect}: the in-order run tells nobody anything");
            }
            let want_on = if disconnect { None } else { Some(HOST_A) };
            assert_eq!(c.online_on(A), want_on, "disconnect {disconnect}, order {mask:b}");
            assert_eq!(c.hop_of(A), 3, "disconnect {disconnect}, order {mask:b}");
            assert_eq!(c.claim_of(A), Some(Claim::Fortress(site)), "disconnect {disconnect}, order {mask:b}");
            assert_eq!(returnable(&mut c, site, 10), disconnect, "disconnect {disconnect}, order {mask:b}");
        }
        assert_eq!(orders, if disconnect { 15 } else { 10 });
    }
}

/// A's `PlayerHandedTo` (A → B) is never delivered — lost with A's connection — and A
/// re-attaches without the player but with it in `handed`: the claim stays online (in
/// transit to B) until B reports. (a) B attaches without the player: offline. (b) B
/// reports it online, then offline: in use until the offline.
#[test]
fn reattach_mid_crossing_keeps_the_claim_online() {
    for b_attaches in [true, false] {
        let (mut c, site, _) = a_online_on_host_a();
        let reattach = HostMsg::Attach {
            host: HOST_A,
            epoch: 2,
            running: Vec::new(),
            online: Vec::new(),
            generation: 0,
            boot: 7,
            settled_below: 0,
            handed: vec![(A, HOST_B, 1)],
            pending_cut: None,
            successions: Vec::new(),
        };
        c.handle(HOST_A, reattach, 0);
        assert_eq!(c.online_on(A), Some(HOST_B), "b_attaches {b_attaches}: in transit to B");
        assert!(!returnable(&mut c, site, 10), "b_attaches {b_attaches}: in use while in transit");
        if b_attaches {
            c.handle(HOST_B, attach(HOST_B, 1, Vec::new(), Vec::new()), 0);
        } else {
            c.handle(HOST_B, online_at(A, Some(Claim::Fortress(site)), false, 2), 0);
            assert!(!returnable(&mut c, site, 11), "online on B");
            c.handle(HOST_B, HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
        }
        assert_eq!(c.online_on(A), None, "b_attaches {b_attaches}");
        assert!(returnable(&mut c, site, 12), "b_attaches {b_attaches}: free once B says so");
    }
}

/// B's offline of an older placement (hop 2), delayed on B's link, arrives while the
/// player is in transit to B again (A handed it at hop 3): it cannot end that transit.
#[test]
fn stale_offline_cannot_cancel_a_later_transit() {
    let (mut c, site, _) = a_online_on_host_a();
    let fortress = Some(Claim::Fortress(site));
    c.handle(HOST_A, HostMsg::PlayerHandedTo { player: A, to: HOST_B, hop: 1 }, 0);
    c.handle(HOST_B, online_at(A, fortress, false, 2), 0);
    // B's placement ends (its offline delayed); the player logs in on A again (hop 3) and
    // is handed to B once more.
    c.handle(HOST_A, online_at(A, fortress, false, 3), 0);
    c.handle(HOST_A, HostMsg::PlayerHandedTo { player: A, to: HOST_B, hop: 3 }, 0);
    c.handle(HOST_B, HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
    assert_eq!(c.online_on(A), Some(HOST_B), "still in transit to B");
    assert!(!returnable(&mut c, site, 10), "the claim stays in use");
    c.handle(HOST_B, online_at(A, fortress, false, 4), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(HOST_B), 4));
}

/// `ClaimIs` (and `HopIs`) answers for a host still waiting to leave — behind a gated
/// message, or free to leave — are dropped at that host's `Attach`, which reports
/// everyone afresh; answers for other hosts stay.
#[test]
fn claim_is_queued_for_a_host_is_dropped_at_its_attach() {
    let c_player = PlayerId(3);
    for gated in [true, false] {
        let (mut c, civ, people) = coordinator();
        c.handle(HOST_A, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
        let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
        if gated {
            // A grant waits for its save: everything after it waits too.
            c.handle(2, embark(2, c_player, civ, &people[2..4], rc(14, 0)), 0);
        }
        // Host B reports B with A's fortress, which A plays online: `ClaimIs(None)` for B,
        // and a stale report of C on host 2: `HopIs` for host 2… after a higher hop.
        c.handle(HOST_B, online(B, Some(Claim::Fortress(site)), false), 0);
        c.handle(2, online_at(PlayerId(4), None, false, 5), 0);
        c.handle(2, online_at(PlayerId(4), None, false, 4), 0);
        c.handle(HOST_B, online_at(PlayerId(4), None, false, 3), 0);
        c.handle(HOST_B, attach(HOST_B, 1, Vec::new(), Vec::new()), 0);
        let out = commit_routed(&mut c);
        let answers: Vec<(Target, &CoordMsg)> = out
            .iter()
            .filter(|(_, m)| matches!(m, CoordMsg::ClaimIs { .. } | CoordMsg::HopIs { .. }))
            .map(|(t, m)| (*t, m))
            .collect();
        assert!(answers.iter().all(|(t, _)| *t != Target::Host(HOST_B)), "gated {gated}: none for B: {answers:?}");
        assert_eq!(answers.len(), 1, "gated {gated}: host 2's answer stays: {answers:?}");
        assert!(matches!(answers[0].1, CoordMsg::HopIs { reported: 4, hop: 5, .. }), "{answers:?}");
    }
}

/// An `Attach` of a newer world generation (every host rolled back, its hops starting
/// over) forgets the hops: a report at hop 1 counts again. An attach of the same
/// generation does not.
#[test]
fn higher_generation_attach_resets_hops() {
    let (mut c, site, _) = a_online_on_host_a();
    let fortress = Some(Claim::Fortress(site));
    c.handle(HOST_A, online_at(A, fortress, false, 5), 0);
    c.handle(HOST_A, HostMsg::PlayerOffline { player: A, hop: 5 }, 0);
    let attach_gen = |host, epoch, generation, online| HostMsg::Attach {
        host,
        epoch,
        running: Vec::new(),
        online,
        generation,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
    };
    c.handle(HOST_B, attach_gen(HOST_B, 1, 0, vec![report(A, fortress, false)]), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (None, 5), "the same generation: hop 1 is stale");
    c.handle(HOST_B, attach_gen(HOST_B, 2, 1, vec![report(A, fortress, false)]), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(HOST_B), 1), "a newer generation: hops start over");
    c.handle(HOST_A, attach_gen(HOST_A, 1, 1, Vec::new()), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(HOST_B), 1), "a second attach of it keeps them");
}

// ---- Fix round 1: a displaced host is told (R3 Task 11) ------------------------------

const HOST_G: u32 = 2;
const HOST_H: u32 = 3;

/// One run of the login race, driven by `choices` (0: deliver G's next in-flight
/// message, 1: deliver H's next message, 2: deliver the coordinator's next message to
/// H). Returns the choices enabled at the end of the prefix (empty: the run is over),
/// and the coordinator.
///
/// P's last placement the coordinator knows is on G at hop 3, ended. G's later
/// placements (`g_flight`, hops above 3) are still on G's link. P logs in on H, which
/// knows nothing of it (hop 1) and answers every `HopIs` of its current placement by
/// renumbering above it and reporting again.
///
/// `grant`: P gave its fortress up at hop 3 (H's login holds nothing), and G's in-flight
/// placement at hop 4 asks to return to it — granted — between its `Online` and its
/// `Offline` (`g_flight` is then ignored).
fn login_race(choices: &[u8], g_flight: &[HostMsg], grant: bool) -> (Vec<u8>, Coordinator, SiteId) {
    let (mut c, civ, people) = coordinator();
    c.handle(HOST_G, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    let fortress = Some(Claim::Fortress(site));
    c.handle(HOST_G, online_at(A, fortress, false, 3), 0);
    if grant {
        c.handle(HOST_G, HostMsg::ReleaseClaim { player: A }, 0);
    }
    c.handle(HOST_G, HostMsg::PlayerOffline { player: A, hop: 3 }, 0);
    commit_routed(&mut c);
    let fortress = if grant { None } else { fortress };
    let g_flight = if grant {
        vec![
            online_at(A, None, false, 4),
            request(2, A, WorldRequest::ReturnTo(site)),
            HostMsg::PlayerOffline { player: A, hop: 4 },
        ]
    } else {
        g_flight.to_vec()
    };
    let mut g: std::collections::VecDeque<HostMsg> = g_flight.into_iter().collect();
    let mut h_hop = 1;
    let mut h_out = std::collections::VecDeque::from([online_at(A, fortress, false, h_hop)]);
    let mut h_in: std::collections::VecDeque<CoordMsg> = Default::default();
    let mut route = |c: &mut Coordinator, h_in: &mut std::collections::VecDeque<CoordMsg>| {
        for (to, m) in commit_routed(c) {
            if to == Target::Host(HOST_H) && matches!(m, CoordMsg::HopIs { player: A, .. }) {
                h_in.push_back(m);
            }
        }
    };
    for &choice in choices {
        match choice {
            0 => c.handle(HOST_G, g.pop_front().unwrap(), 0),
            1 => c.handle(HOST_H, h_out.pop_front().unwrap(), 0),
            _ => {
                if let Some(CoordMsg::HopIs { reported, hop, .. }) = h_in.pop_front() {
                    if reported == h_hop {
                        h_hop = hop + 1;
                        h_out.push_back(online_at(A, fortress, false, h_hop));
                    }
                }
            }
        }
        route(&mut c, &mut h_in);
    }
    let enabled = [(0, !g.is_empty()), (1, !h_out.is_empty()), (2, !h_in.is_empty())]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(ch, _)| ch)
        .collect();
    (enabled, c, site)
}

/// Every arrival order (each host's own order kept, `HopIs` round trips causal) of G's
/// in-flight placements and H's login ends with P online on H — the host that holds it —
/// and another player's return to P's fortress refused. With G's in-flight placements at
/// hop 4, and at hops 5 and 6 (several in flight: H's first renumbering lands below them).
#[test]
fn login_racing_an_older_placements_messages_ends_online_on_the_login_host() {
    let placements = |hops: &[u64]| -> Vec<HostMsg> {
        hops.iter()
            .flat_map(|&hop| {
                // The fortress claim does not matter to the order: report none.
                [online_at(A, None, false, hop), HostMsg::PlayerOffline { player: A, hop }]
            })
            .collect()
    };
    for g_flight in [placements(&[4]), placements(&[5, 6])] {
        let mut stack: Vec<Vec<u8>> = vec![Vec::new()];
        let mut runs = 0;
        while let Some(prefix) = stack.pop() {
            let (enabled, mut c, site) = login_race(&prefix, &g_flight, false);
            if enabled.is_empty() {
                runs += 1;
                assert_eq!(c.online_on(A), Some(HOST_H), "order {prefix:?}: online on H");
                assert!(!returnable(&mut c, site, 10), "order {prefix:?}: the fortress stays in use");
                continue;
            }
            for ch in enabled {
                let mut next = prefix.clone();
                next.push(ch);
                stack.push(next);
            }
        }
        assert!(runs > 1, "{runs} orders");
    }
    // G's in-flight placement is granted a claim (a return to P's old fortress) between its
    // `Online` and `Offline`. The grant moves P onto G: H is told, re-asserts, and its
    // report (no claim, nothing unsettled) is the truth — the grant is given back. Every
    // order ends with P online on H holding nothing, the fortress free.
    let mut stack: Vec<Vec<u8>> = vec![Vec::new()];
    let mut runs = 0;
    while let Some(prefix) = stack.pop() {
        let (enabled, mut c, site) = login_race(&prefix, &[], true);
        if enabled.is_empty() {
            runs += 1;
            assert_eq!(c.online_on(A), Some(HOST_H), "grant, order {prefix:?}: online on H");
            assert_eq!(c.claim_of(A), None, "grant, order {prefix:?}: the grant is given back");
            assert!(returnable(&mut c, site, 10), "grant, order {prefix:?}: the fortress is free");
            continue;
        }
        for ch in enabled {
            let mut next = prefix.clone();
            next.push(ch);
            stack.push(next);
        }
    }
    assert!(runs > 1, "{runs} orders");
}

/// An `Online` at the known hop from a host other than the one the player is online on
/// is stale (two hosts are never online at one hop): ignored, answered `HopIs`. The same
/// report from the host it is online on (a re-report) is applied.
#[test]
fn equal_hop_online_from_another_host_is_stale() {
    let (mut c, site, _) = a_online_on_host_a();
    commit_routed(&mut c);
    let fortress = Some(Claim::Fortress(site));
    c.handle(HOST_B, online_at(A, fortress, false, 1), 0);
    assert_eq!(c.online_on(A), Some(HOST_A));
    let out = commit_routed(&mut c);
    assert!(
        out.iter().any(|(t, m)| *t == Target::Host(HOST_B) && matches!(m, CoordMsg::HopIs { player: A, reported: 1, hop: 1 })),
        "{out:?}"
    );
    c.handle(HOST_A, online_at(A, fortress, false, 1), 0);
    assert_eq!(c.online_on(A), Some(HOST_A));
    assert!(!commit_routed(&mut c).iter().any(|(_, m)| matches!(m, CoordMsg::HopIs { .. })), "a re-report is fine");
}

/// Two hosts that both hold P (a duplicate body) and answer every `HopIs` of their
/// placement by renumbering and reporting again: each displaces the other, but a host
/// renumbers one player at most `HOP_RENUMBER_MAX_PER_PLAYER` times — the exchange
/// stops, with one error logged, and P ends online on one of them.
#[test]
fn two_holders_do_not_ping_pong_for_ever() {
    let (mut c, site, _) = a_embarked_on_host_a();
    commit_routed(&mut c);
    let fortress = Some(Claim::Fortress(site));
    let hosts = [HOST_G, HOST_H];
    let mut hop = [1u64, 1];
    let mut budget = [RenumberBudget::default(), RenumberBudget::default()];
    let mut inbox: std::collections::VecDeque<(usize, HostMsg)> =
        hosts.iter().enumerate().map(|(i, _)| (i, online_at(A, fortress, false, 1))).collect();
    let mut steps = 0;
    while let Some((i, m)) = inbox.pop_front() {
        steps += 1;
        assert!(steps < 100, "the exchange does not stop");
        c.handle(hosts[i], m, 0);
        for (to, m) in commit_routed(&mut c) {
            let CoordMsg::HopIs { player: A, reported, hop: known } = m else { continue };
            let j = hosts.iter().position(|h| Target::Host(*h) == to).expect("one of the two");
            if reported == hop[j] && budget[j].take(A) {
                hop[j] = known + 1;
                inbox.push_back((j, online_at(A, fortress, false, hop[j])));
            }
        }
    }
    let errors: u32 = budget.iter().map(|b| b.errors_logged()).sum();
    assert_eq!(errors, 1, "one host ran out, once");
    assert!(hosts.iter().any(|h| c.online_on(A) == Some(*h)), "online on one of them");
    assert_eq!(c.hop_of(A), hop.iter().copied().max().unwrap());
}

// --- World cuts (R3 Task 13) ---------------------------------------------------------

/// The world-cut period in these tests.
const CUT_EVERY_MS: u64 = 1_000;

/// A coordinator with world cuts every [`CUT_EVERY_MS`] of hosts 0 and 1, both attached,
/// its outputs drained.
fn cutting() -> Coordinator {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(3);
    c.set_expected_hosts([0, 1].into_iter().collect());
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_world_tick(100);
    c.handle(0, attach(0, 1, Vec::new(), Vec::new()), 0);
    c.handle(1, attach(1, 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    c
}

/// The `BeginCut` each host got: `(host, cut, tick)`.
fn begun(out: &[(Target, CoordMsg)]) -> Vec<(u32, game::CutId, u64)> {
    out.iter()
        .filter_map(|(to, m)| match (to, m) {
            (Target::Host(h), CoordMsg::BeginCut { cut, tick, .. }) => Some((*h, *cut, *tick)),
            _ => None,
        })
        .collect()
}

/// The `CutDone` each host got: `(host, cut, committed)`.
fn done(out: &[(Target, CoordMsg)]) -> Vec<(u32, game::CutId, bool)> {
    out.iter()
        .filter_map(|(to, m)| match (to, m) {
            (Target::Host(h), CoordMsg::CutDone { cut, committed }) => Some((*h, *cut, *committed)),
            _ => None,
        })
        .collect()
}

/// A world cut begins a period after the cuts were turned on, at the world tick plus the
/// lead, with an id of the coordinator's epoch; when both parts are staged it is decided
/// commit and both hosts are told.
#[test]
fn world_cut_begins_and_commits_with_every_part() {
    let mut c = cutting();
    c.maintain(CUT_EVERY_MS - 1);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "not before the period");
    c.maintain(CUT_EVERY_MS);
    let out = commit_routed(&mut c);
    let cut = game::CutId((3 << 32) | 1);
    assert_eq!(begun(&out), [(0, cut, 100 + game::WORLD_CUT_LEAD_TICKS), (1, cut, 100 + game::WORLD_CUT_LEAD_TICKS)]);
    c.maintain(3 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "one cut at a time");
    c.handle(0, HostMsg::CutPrepared { cut }, 2 * CUT_EVERY_MS);
    assert_eq!(c.take_decision(), None, "host 1's part is missing");
    c.handle(1, HostMsg::CutPrepared { cut }, 2 * CUT_EVERY_MS);
    let d = c.take_decision().expect("decided");
    assert_eq!((d.cut, d.commit, d.hosts.clone()), (cut, true, vec![0, 1]));
    assert_eq!(c.take_decision(), None, "handed out once");
    c.decided(cut, true);
    assert_eq!(done(&commit_routed(&mut c)), [(0, cut, true), (1, cut, true)]);
    assert_eq!(c.world_cut_running(), None);
}

/// Criterion F at the coordinator: no new cut while one is undecided — also once its
/// decision was handed to the runtime and not yet recorded.
#[test]
fn no_second_world_cut_while_one_is_undecided() {
    let mut c = cutting();
    c.maintain(CUT_EVERY_MS);
    let (_, cut, _) = begun(&commit_routed(&mut c))[0];
    c.handle(0, HostMsg::CutFailed { cut, why: CutFailure::Aborted }, CUT_EVERY_MS);
    assert!(!c.take_decision().expect("an abort").commit);
    c.maintain(10 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "the decision is not recorded yet");
    c.decided(cut, false);
    commit_routed(&mut c);
    c.maintain(11 * CUT_EVERY_MS);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 2, "the next cut a period after the last decision");
}

/// A host that re-attaches naming the cut it took part in learns the outcome: the cut it
/// was in when its link dropped is aborted (it detached) and every participant told; on
/// its `Attach` it is answered from what the coordinator remembers; a cut the coordinator
/// does not know (an older run's) is looked up by the runtime and answered from that.
#[test]
fn reattach_with_a_pending_cut_gets_its_outcome() {
    let mut c = cutting();
    c.maintain(CUT_EVERY_MS);
    let (_, cut, _) = begun(&commit_routed(&mut c))[0];
    c.handle(0, HostMsg::CutPrepared { cut }, CUT_EVERY_MS);
    c.host_detached(1);
    let d = c.take_decision().expect("the detach decides");
    assert!(!d.commit, "a detached participant aborts the cut");
    c.decided(cut, false);
    assert_eq!(done(&commit_routed(&mut c)), [(0, cut, false), (1, cut, false)]);
    // Host 1 never read that: it re-attaches naming the cut.
    let reattach = HostMsg::Attach {
        host: 1,
        epoch: 2,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: Some(cut),
        successions: Vec::new(),
    };
    c.handle(1, reattach, CUT_EVERY_MS);
    assert_eq!(done(&commit_routed(&mut c)), [(1, cut, false)], "answered from the outcome remembered");
    // A cut of an earlier coordinator run: the runtime looks it up.
    let old = game::CutId((2 << 32) | 9);
    c.handle(0, HostMsg::CutPrepared { cut: old }, CUT_EVERY_MS);
    assert!(done(&commit_routed(&mut c)).is_empty());
    assert_eq!(c.take_cut_lookups(), [(0, old)]);
    c.cut_outcome(0, old, true);
    assert_eq!(done(&commit_routed(&mut c)), [(0, old, true)]);
    // A participant re-attaching mid-cut aborts it (its link dropped: marks may be lost).
    c.maintain(5 * CUT_EVERY_MS);
    let (_, next, _) = begun(&commit_routed(&mut c))[0];
    c.handle(1, attach(1, 3, Vec::new(), Vec::new()), 5 * CUT_EVERY_MS);
    assert_eq!(c.take_decision().map(|d| (d.cut, d.commit)), Some((next, false)));
}

/// A `Missed` doubles the lead up to the maximum; a committed cut resets it; the deadline
/// aborts a cut a part never came for.
#[test]
fn missed_world_cut_doubles_the_lead_and_the_deadline_aborts() {
    let mut c = cutting();
    let mut now = 0;
    let mut leads = Vec::new();
    for _ in 0..6 {
        now += CUT_EVERY_MS;
        c.maintain(now);
        let (_, cut, tick) = begun(&commit_routed(&mut c))[0];
        leads.push(tick - 100);
        c.handle(1, HostMsg::CutFailed { cut, why: CutFailure::Missed }, now);
        c.take_decision().expect("aborted");
        c.decided(cut, false);
        commit_routed(&mut c);
    }
    assert_eq!(leads, [40, 80, 160, 320, 640, 640]);
    now += CUT_EVERY_MS;
    c.maintain(now);
    let (_, cut, _) = begun(&commit_routed(&mut c))[0];
    c.handle(0, HostMsg::CutPrepared { cut }, now);
    c.handle(1, HostMsg::CutPrepared { cut }, now);
    c.take_decision().expect("commit");
    c.decided(cut, true);
    assert_eq!(c.world_cut_lead(), game::WORLD_CUT_LEAD_TICKS);
    now += CUT_EVERY_MS;
    c.maintain(now);
    let (_, cut, _) = begun(&commit_routed(&mut c))[0];
    c.maintain(now + game::WORLD_CUT_DEADLINE_MS - 1);
    assert_eq!(c.take_decision(), None);
    c.maintain(now + game::WORLD_CUT_DEADLINE_MS);
    assert_eq!(c.take_decision().map(|d| (d.cut, d.commit)), Some((cut, false)), "the deadline aborts");
}

/// Important 6: no world cut while an attached host is not among the expected ones (it
/// registered since the hosts were last read), nor after a failed read until a good one.
#[test]
fn no_cut_while_an_attached_host_is_not_expected() {
    let mut c = cutting();
    c.handle(2, attach(2, 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    c.maintain(CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "host 2 is attached and not expected");
    c.set_expected_hosts([0, 1, 2].into_iter().collect());
    c.expected_hosts_unknown();
    c.maintain(2 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "the hosts could not be read");
    c.set_expected_hosts([0, 1, 2].into_iter().collect());
    c.maintain(3 * CUT_EVERY_MS);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 3, "all three take part");
}

/// Task 14: leave requests are collected for `LEAVE_BATCH_MS`, then a cut begins at once
/// (before the period) with them `leaving` (among its hosts); once it commits the leaver
/// is not expected — the next cut is the others' alone, though its process is still
/// attached — until another boot of it attaches.
#[test]
fn leave_requests_begin_one_cut_and_a_leaver_is_not_expected_until_its_next_boot() {
    let mut c = cutting();
    c.handle(1, HostMsg::LeaveRequest, 100);
    c.maintain(100 + game::LEAVE_BATCH_MS - 1);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "collected for the batch");
    c.maintain(100 + game::LEAVE_BATCH_MS);
    let out = commit_routed(&mut c);
    let begins: Vec<(Vec<u32>, Vec<u32>)> = out
        .iter()
        .filter_map(|(_, m)| match m {
            CoordMsg::BeginCut { hosts, leaving, .. } => Some((hosts.clone(), leaving.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(begins, vec![(vec![0, 1], vec![1]); 2], "both hosts, host 1 leaving, before the period");
    let cut = begun(&out)[0].1;
    c.handle(0, HostMsg::CutPrepared { cut }, 700);
    c.handle(1, HostMsg::CutPrepared { cut }, 700);
    let d = c.take_decision().expect("decided");
    assert!(d.commit && d.hosts == vec![0, 1] && d.leaving == vec![1], "{d:?}");
    c.decided(cut, true);
    assert_eq!(done(&commit_routed(&mut c)).len(), 2, "one CutDone per host");
    assert_eq!(c.left_hosts(), vec![1]);
    // Its stale row (same boot) does not bring it back.
    c.set_host_boots([(0, 7), (1, 7)].into_iter().collect());
    c.maintain(700 + CUT_EVERY_MS);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![0], "host 0 alone");
    let cut = next[0].1;
    c.handle(0, HostMsg::CutPrepared { cut }, 1800);
    let d = c.take_decision().expect("decided");
    assert!(d.commit && d.hosts == vec![0] && d.leaving.is_empty(), "{d:?}");
    c.decided(cut, true);
    commit_routed(&mut c);
    // Another boot of it attaches: expected again.
    c.handle(1, HostMsg::Attach { host: 1, epoch: 2, running: Vec::new(), online: Vec::new(), generation: 0, boot: 8, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() }, 1900);
    assert!(c.left_hosts().is_empty());
    commit_routed(&mut c);
    c.maintain(1900 + CUT_EVERY_MS);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 2, "both take part again");
}

/// Final review I1: a host attaching while a cut it is not in runs (back from a clean
/// leave, or new) aborts that cut — it may exchange entities with the participants before
/// the cut's tick, which no part would save — and the next cut takes it in.
#[test]
fn attach_of_a_host_outside_the_running_cut_aborts_it() {
    let mut c = cutting();
    // Host 1 leaves in a committed cut.
    c.handle(1, HostMsg::LeaveRequest, 100);
    c.maintain(100 + game::LEAVE_BATCH_MS);
    let cut = begun(&commit_routed(&mut c))[0].1;
    c.handle(0, HostMsg::CutPrepared { cut }, 700);
    c.handle(1, HostMsg::CutPrepared { cut }, 700);
    assert!(c.take_decision().expect("decided").commit);
    c.decided(cut, true);
    commit_routed(&mut c);
    // A cut of host 0 alone begins; host 1 comes back (another boot) before it is decided.
    c.maintain(700 + CUT_EVERY_MS);
    let alone = begun(&commit_routed(&mut c));
    assert_eq!(alone.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![0]);
    let cut = alone[0].1;
    let back = HostMsg::Attach { host: 1, epoch: 2, running: Vec::new(), online: Vec::new(), generation: 0, boot: 8, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() };
    c.handle(1, back, 1800);
    assert!(c.left_hosts().is_empty(), "another boot: back");
    commit_routed(&mut c);
    c.handle(0, HostMsg::CutPrepared { cut }, 1800);
    let d = c.take_decision().expect("the attach decides");
    assert_eq!((d.cut, d.commit), (cut, false), "the cut that excludes host 1 is aborted");
    c.decided(cut, false);
    commit_routed(&mut c);
    // The next cut takes both.
    c.maintain(1800 + CUT_EVERY_MS);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 2, "both take part");
    // A host never seen before attaching mid-cut aborts it too.
    let cut = c.world_cut_running().expect("running").0;
    c.handle(2, attach(2, 1, Vec::new(), Vec::new()), 1900);
    assert_eq!(c.take_decision().map(|d| (d.cut, d.commit)), Some((cut, false)));
}

/// Final review I2: a due cut blocked by a dirty row with no attached host is reported
/// (after the grace, once per period, naming the missing host and saying the world is not
/// being saved); a cut begins as soon as the hosts match.
#[test]
fn blocked_cuts_are_reported_once_a_period_and_begin_when_the_hosts_match() {
    let mut c = cutting();
    c.set_expected_hosts([0, 1, 2].into_iter().collect());
    c.maintain(CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "host 2 is expected and not attached");
    assert_eq!(c.take_cuts_blocked(), None, "within the grace");
    let first = CUT_EVERY_MS + game::WORLD_CUT_BLOCKED_GRACE_MS;
    c.maintain(first);
    let blocked = c.take_cuts_blocked().expect("reported");
    assert_eq!(blocked, game::CutsBlocked { missing: vec![2], unexpected: vec![], successions: vec![] });
    let text = blocked.to_string();
    assert!(text.contains("NOT being saved") && text.contains("[2]") && text.contains("worldctl"), "{text}");
    c.maintain(first + CUT_EVERY_MS - 1);
    assert_eq!(c.take_cuts_blocked(), None, "once per period");
    c.maintain(first + CUT_EVERY_MS);
    assert!(c.take_cuts_blocked().is_some(), "again a period later");
    assert!(begun(&commit_routed(&mut c)).is_empty());
    c.handle(2, attach(2, 1, Vec::new(), Vec::new()), first + CUT_EVERY_MS + 1);
    commit_routed(&mut c);
    c.maintain(first + CUT_EVERY_MS + 2);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 3, "at once, all three");
    assert_eq!(c.take_cuts_blocked(), None);
}

// ---- the lease watch (R4) ----------------------------------------------------------------

/// Host `ordinal`'s row as the coordinator reads it: dirty and live, last seen at `seen_ms`.
fn seen(ordinal: u32, seen_ms: u64) -> HostSeen {
    HostSeen { ordinal, boot: 100 + u64::from(ordinal), clean: false, state: game::HOST_LIVE, seen_ms }
}

/// A host is lapsed only once the coordinator itself read one `seen_ms` over a whole lease
/// of its own clock; a beat in between restarts the count; clean or dead rows never lapse.
#[test]
fn lease_watch_needs_a_full_lease_of_unchanged_reads() {
    let mut w = LeaseWatch::default();
    let mut clean = seen(2, 7);
    clean.clean = true;
    let mut dead = seen(3, 7);
    dead.state = game::HOST_DEAD;
    for t in (0..game::HOST_LEASE_MS).step_by(1_000) {
        w.observe(&[seen(1, 500), clean.clone(), dead.clone()], t, t);
        assert!(w.lapsed(t).is_empty(), "lapsed at {t} ms: {:?}", w.lapsed(t));
    }
    w.observe(&[seen(1, 500), clean.clone(), dead.clone()], game::HOST_LEASE_MS, game::HOST_LEASE_MS);
    assert_eq!(w.lapsed(game::HOST_LEASE_MS), vec![(1, 101)]);
    // It beat: the count starts again from this read.
    let t0 = game::HOST_LEASE_MS + 1_000;
    w.observe(&[seen(1, 2_000)], t0, t0);
    assert!(w.lapsed(t0).is_empty());
    w.observe(&[seen(1, 2_000)], t0 + game::HOST_LEASE_MS - 1, t0 + game::HOST_LEASE_MS - 1);
    assert!(w.lapsed(t0 + game::HOST_LEASE_MS - 1).is_empty());
    w.observe(&[seen(1, 2_000)], t0 + game::HOST_LEASE_MS, t0 + game::HOST_LEASE_MS);
    assert_eq!(w.lapsed(t0 + game::HOST_LEASE_MS), vec![(1, 101)]);
    // Time alone, without a read, lapses nobody new.
    let mut fresh = LeaseWatch::default();
    fresh.observe(&[seen(1, 2_000)], 0, 0);
    assert!(fresh.lapsed(10 * game::HOST_LEASE_MS).is_empty());
}

/// Reads that fail for longer than a lease (a database outage) declare nobody: the watch
/// forgets everything and needs a whole lease of good reads again.
#[test]
fn outage_longer_than_the_lease_declares_nobody() {
    let mut w = LeaseWatch::default();
    w.observe(&[seen(0, 500), seen(1, 500)], 0, 0);
    for _ in 0..20 {
        w.observe_failed();
    }
    let back = 20_000;
    w.observe(&[seen(0, 500), seen(1, 500)], back, back);
    assert!(w.lapsed(back).is_empty(), "{:?}", w.lapsed(back));
    w.observe(&[seen(0, 500), seen(1, 500)], back + game::HOST_LEASE_MS - 1, back + game::HOST_LEASE_MS - 1);
    assert!(w.lapsed(back + game::HOST_LEASE_MS - 1).is_empty());
    w.observe(&[seen(0, 500), seen(1, 500)], back + game::HOST_LEASE_MS, back + game::HOST_LEASE_MS);
    assert_eq!(w.lapsed(back + game::HOST_LEASE_MS), vec![(0, 100), (1, 101)]);
}

/// A row a booting host of the same ordinal vouched for (`seen_ms = 0`) is lapsed at once.
#[test]
fn a_vouched_row_is_lapsed_at_once() {
    let mut w = LeaseWatch::default();
    w.observe(&[seen(0, 500), seen(1, 0)], 42, 42);
    assert_eq!(w.lapsed(42), vec![(1, 101)]);
    // Unless it is clean (it left) or dead (already declared).
    let mut clean = seen(1, 0);
    clean.clean = true;
    w.observe(&[clean], 43, 43);
    assert!(w.lapsed(43).is_empty());
}

/// A read stamped by when it was taken, not when it was handled: an answer issued at 1 s
/// that the coordinator only got to at 12 s (a stall) shows the same beat as a read received
/// at 0 — that is 1 s of evidence, not 12, so nobody lapses.
#[test]
fn a_late_answer_does_not_stretch_the_lease() {
    let mut w = LeaseWatch::default();
    w.observe(&[seen(1, 500)], 0, 0);
    w.observe(&[seen(1, 500)], 1_000, 12_000);
    assert!(w.lapsed(12_000).is_empty(), "{:?}", w.lapsed(12_000));
    // A read issued a whole lease after the first one was received still shows it: lapsed.
    w.observe(&[seen(1, 500)], game::HOST_LEASE_MS, game::HOST_LEASE_MS + 50);
    assert_eq!(w.lapsed(game::HOST_LEASE_MS + 50), vec![(1, 101)]);
    // A changed value counts from its first read's RECEIVE.
    w.observe(&[seen(1, 900)], 20_000, 21_000);
    w.observe(&[seen(1, 900)], 21_000 + game::HOST_LEASE_MS - 1, 40_000);
    assert!(w.lapsed(40_000).is_empty());
}

// ---- successions (R4 Task 8) ---------------------------------------------------------------

/// Hosts `0..n` attached and expected by world cuts every [`CUT_EVERY_MS`], the crash policy
/// `Succession`, every output drained.
fn succeeding(n: u32) -> Coordinator {
    let mut c = cutting();
    for h in 2..n {
        c.handle(h, attach(h, 1, Vec::new(), Vec::new()), 0);
    }
    c.set_expected_hosts((0..n).collect());
    c.set_crash_policy(game::CrashPolicy::Succession);
    commit_routed(&mut c);
    c
}

/// A read of hosts `0..n` at `now`: `dead` vouched dead by a boot of their ordinal
/// (`seen_ms = 0`: lapsed at once), every other one beating; `declared` rows are dead.
fn read_leases(c: &mut Coordinator, n: u32, dead: &[u32], declared: &[u32], now: u64) {
    let rows: Vec<HostSeen> = (0..n)
        .map(|h| {
            let mut row = seen(h, if dead.contains(&h) { 0 } else { now + 1 });
            if declared.contains(&h) {
                row.state = game::HOST_DEAD;
            }
            row
        })
        .collect();
    c.observe_leases(&rows, now, now);
}

/// What the store answers a declaration of `d` (ids from `first`): survivors every host of
/// `0..n` but the dead, one super-region moved per predecessor, from cut `base`.
fn store_declares(d: &Declare, n: u32, first: u64, base: game::CutId) -> Vec<game::Succession> {
    let dead: Vec<u32> = d.dead.iter().map(|(h, _)| *h).collect();
    d.dead
        .iter()
        .zip(&d.heirs)
        .enumerate()
        .map(|(i, (&(predecessor, predecessor_boot), &heir))| game::Succession {
            id: game::SuccessionId(first + i as u64),
            predecessor,
            predecessor_boot,
            heir,
            kind: d.kind,
            base_cut: base,
            moved: vec![((predecessor as i32, 0), 2)],
            survivors: (0..n).filter(|h| !dead.contains(h)).collect(),
        })
        .collect()
}

/// The succession orders in `out`: `(host, id)`.
fn orders(out: &[(Target, CoordMsg)]) -> Vec<(u32, game::SuccessionId)> {
    out.iter()
        .filter_map(|(to, m)| match (to, m) {
            (Target::Host(h), CoordMsg::Succession { id, .. }) => Some((*h, *id)),
            _ => None,
        })
        .collect()
}

/// The `SuccessionDone`s in `out`: `(target, id, settled)`.
fn succession_done(out: &[(Target, CoordMsg)]) -> Vec<(Target, game::SuccessionId, bool)> {
    out.iter()
        .filter_map(|(to, m)| match m {
            CoordMsg::SuccessionDone { id, settled } => Some((*to, *id, *settled)),
            _ => None,
        })
        .collect()
}

/// Host `dead` of `0..n` lapses and is declared at `now`: the succession, its orders out.
fn declare_one(c: &mut Coordinator, n: u32, dead: u32, now: u64) -> game::Succession {
    read_leases(c, n, &[dead], &[], now);
    let d = c.declare_wanted(now).expect("a lapsed host is declared");
    let list = store_declares(&d, n, 1, game::CutId(0));
    c.declared(game::Declared::Done(list.clone()), now);
    c.set_expected_hosts((0..n).filter(|h| *h != dead).collect());
    list.into_iter().next().unwrap()
}

/// Every lapsed host goes in ONE batch, each with the heir `heir_of` picks over the live
/// attached hosts that did not lapse; nothing while a declaration is in flight, nothing
/// with no live host attached, nothing under the `Rollback` policy (`Static`).
#[test]
fn lapsed_hosts_are_declared_in_one_batch_between_cuts() {
    let mut c = succeeding(4);
    read_leases(&mut c, 4, &[], &[], 10);
    assert_eq!(c.declare_wanted(10), None, "nobody lapsed");
    read_leases(&mut c, 4, &[2, 3], &[], 20);
    let d = c.declare_wanted(20).expect("two lapsed hosts");
    let live: std::collections::BTreeSet<u32> = [0, 1].into_iter().collect();
    assert_eq!(d.dead, vec![(2, 102), (3, 103)], "both, in one batch");
    assert_eq!(d.heirs, vec![game::heir_of(2, &live).unwrap(), game::heir_of(3, &live).unwrap()]);
    assert_eq!(d.kind, game::SuccessionKind::Crash);
    assert_eq!(c.declare_wanted(21), None, "one declaration in flight at a time");
    // The store found them beating again: forgotten — not declared again before a NEWER
    // read of the rows (the watch's same observation would loop every runtime pass).
    c.declared(game::Declared::NotLapsed(vec![2, 3]), 22);
    assert_eq!(c.declare_wanted(23), None, "no second declaration without a fresh read");
    assert_eq!(c.declare_wanted(500), None, "time alone is no fresh read");
    read_leases(&mut c, 4, &[2, 3], &[], 1_000);
    assert_eq!(c.declare_wanted(1_000).map(|d| d.dead), Some(vec![(2, 102), (3, 103)]), "a fresh read: asked again");
    c.declared(game::Declared::Gone(vec![2]), 1_001);
    assert_eq!(c.declare_wanted(1_002), None, "`Gone` waits for a fresh read too");
    read_leases(&mut c, 4, &[2, 3], &[], 2_000);
    assert!(c.declare_wanted(2_000).is_some(), "a vouched row is lapsed at once on the next read");
    c.declare_failed();
    // Under `Rollback` (`Static`) nothing is ever declared.
    let mut r = succeeding(4);
    r.set_crash_policy(game::CrashPolicy::Rollback);
    read_leases(&mut r, 4, &[2, 3], &[], 20);
    assert_eq!(r.declare_wanted(20), None, "Static never declares");
    // A total crash (no live host attached) is the boots' business.
    let mut t = succeeding(2);
    read_leases(&mut t, 2, &[0, 1], &[], 20);
    assert_eq!(t.declare_wanted(20), None, "no live attached host");
}

/// A participant of the running cut lapses: the cut is decided aborted first, and the
/// declaration comes only once that decision is recorded.
#[test]
fn a_running_cut_with_a_lapsed_participant_is_aborted_first() {
    let mut c = succeeding(3);
    c.maintain(CUT_EVERY_MS);
    let begun_now = begun(&commit_routed(&mut c));
    assert_eq!(begun_now.len(), 3, "a cut of every host runs");
    let cut = begun_now[0].1;
    c.handle(0, HostMsg::CutPrepared { cut }, CUT_EVERY_MS);
    read_leases(&mut c, 3, &[2], &[], CUT_EVERY_MS + 1);
    assert_eq!(c.declare_wanted(CUT_EVERY_MS + 1), None, "not while the cut runs");
    let d = c.take_decision().expect("the lapse decides the cut");
    assert_eq!((d.cut, d.commit), (cut, false), "aborted");
    assert_eq!(c.declare_wanted(CUT_EVERY_MS + 2), None, "not before the abort is recorded");
    c.decided(cut, false);
    commit_routed(&mut c);
    let d = c.declare_wanted(CUT_EVERY_MS + 3).expect("between cuts now");
    assert_eq!(d.dead, vec![(2, 102)]);
}

/// After a declaration no world cut begins — however long the period passed — until every
/// survivor applied and the heir adopted; the blocked report names the succession. Then the
/// next cut begins with the survivors only.
#[test]
fn no_cut_begins_until_every_survivor_applied_and_the_heir_adopted() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, 2, 100);
    let out = commit_routed(&mut c);
    assert_eq!(orders(&out), vec![(0, s.id), (1, s.id)], "the order goes to every survivor");
    let mut now = 100 + CUT_EVERY_MS;
    c.maintain(now);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "nobody answered");
    now += game::WORLD_CUT_BLOCKED_GRACE_MS;
    c.maintain(now);
    let blocked = c.take_cuts_blocked().expect("reported after the grace");
    assert_eq!(blocked.successions, vec![s.id]);
    assert!(blocked.to_string().contains("successions"), "{blocked}");
    c.handle(0, HostMsg::SuccessionApplied { id: s.id }, now);
    c.handle(1, HostMsg::SuccessionApplied { id: s.id }, now);
    c.maintain(now + 1);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "every survivor applied; the heir has not adopted");
    // An adoption from a host that is not the heir counts for nothing.
    let other = 1 - s.heir;
    c.handle(other, HostMsg::SuccessionAdopted { id: s.id }, now + 2);
    c.maintain(now + 3);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "only the heir adopts");
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, now + 4);
    c.maintain(now + 5);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![0, 1], "the survivors' cut begins");
}

/// The first cut after every answer names the succession in its decision; an abort keeps
/// it open (the next cut names it again), a commit settles it — every survivor told — and
/// the cut after that settles nothing.
#[test]
fn the_commit_after_a_succession_names_it() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, 2, 100);
    for h in [0, 1] {
        c.handle(h, HostMsg::SuccessionApplied { id: s.id }, 100);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    commit_routed(&mut c);
    let mut now = 100 + CUT_EVERY_MS;
    let run_cut = |c: &mut Coordinator, now: u64, commit: bool| -> CutDecide {
        c.maintain(now);
        let (_, cut, _) = begun(&commit_routed(c))[0];
        if commit {
            c.handle(0, HostMsg::CutPrepared { cut }, now);
            c.handle(1, HostMsg::CutPrepared { cut }, now);
        } else {
            c.handle(1, HostMsg::CutFailed { cut, why: CutFailure::Aborted }, now);
        }
        let d = c.take_decision().expect("decided");
        c.decided(cut, d.commit);
        d
    };
    let aborted = run_cut(&mut c, now, false);
    assert_eq!(aborted.settles, vec![s.id], "the cut names the open succession");
    assert!(succession_done(&commit_routed(&mut c)).is_empty(), "an abort settles nothing");
    assert_eq!(c.succession_progress().len(), 1, "still open");
    now += CUT_EVERY_MS;
    let d = run_cut(&mut c, now, true);
    assert_eq!((d.commit, d.settles.clone()), (true, vec![s.id]));
    let done = succession_done(&commit_routed(&mut c));
    assert_eq!(done, vec![(Target::Host(0), s.id, true), (Target::Host(1), s.id, true)], "every survivor is told");
    assert!(c.succession_progress().is_empty(), "settled");
    now += CUT_EVERY_MS;
    assert!(run_cut(&mut c, now, true).settles.is_empty(), "the next cut settles nothing");
}

/// A second host lapsing while a succession is open (not settled), or the store finding
/// one open, asks for a world rollback: nothing is declared and no cut begins meanwhile;
/// once rolled back every host is told the generation and the open succession abandoned.
#[test]
fn a_second_lapse_before_the_settle_asks_for_a_rollback() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, 2, 100);
    for h in [0, 1] {
        c.handle(h, HostMsg::SuccessionApplied { id: s.id }, 100);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    commit_routed(&mut c);
    // The predecessor's own row (dead now, then lapsed in the watch) is no second lapse.
    read_leases(&mut c, 3, &[2], &[], 150);
    c.maintain(150);
    assert!(!c.rollback_wanted(), "the declared predecessor is not a second lapse");
    read_leases(&mut c, 3, &[1], &[2], 200);
    c.maintain(200);
    assert!(c.rollback_wanted(), "host 1 lapsed before the settle");
    assert_eq!(c.declare_wanted(200), None, "nothing more is declared");
    c.maintain(200 + CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut while a rollback is wanted");
    c.rolled_back(5);
    let out = commit_routed(&mut c);
    assert_eq!(succession_done(&out), vec![(Target::Every, s.id, false)], "abandoned");
    assert!(out.iter().any(|(to, m)| *to == Target::Every && *m == CoordMsg::Generation(5)), "{out:?}");
    assert!(!c.rollback_wanted());
    // The store answering `SuccessionOpen` (another coordinator's, or a lost answer's) too.
    let mut c = succeeding(3);
    read_leases(&mut c, 3, &[2], &[], 10);
    c.declare_wanted(10).expect("declared");
    c.declared(game::Declared::SuccessionOpen, 11);
    assert!(c.rollback_wanted());
}

/// A survivor that never answers (it cannot reach the base cut, it lost its journal): at
/// [`game::SUCCESSION_DEADLINE_MS`] after the declaration the world rolls back — never
/// before, and never once everyone answered.
#[test]
fn a_silent_survivor_past_the_deadline_asks_for_a_rollback() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, 2, 100);
    c.handle(s.heir, HostMsg::SuccessionApplied { id: s.id }, 100);
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    let silent = 1 - s.heir;
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS - 1);
    assert!(!c.rollback_wanted(), "within the deadline");
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS);
    assert!(c.rollback_wanted(), "host {silent} stayed silent past the deadline");
    // Everyone answered: the deadline does not matter (the settle cut may take its time).
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, 2, 100);
    for h in [0, 1] {
        c.handle(h, HostMsg::SuccessionApplied { id: s.id }, 100);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    c.maintain(100 + 2 * game::SUCCESSION_DEADLINE_MS);
    assert!(!c.rollback_wanted(), "answered in time");
}

/// A coordinator restarted mid-succession: nothing is declared and no cut begins until it
/// read the open successions; then each survivor gets the order when it attaches unless
/// its `Attach` lists it as answered (from the host's memory), the heir's listing it
/// adopted counts, and the settle cut follows.
#[test]
fn restart_resends_open_successions_and_hosts_answer_again() {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_world_tick(100);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.successions_unknown();
    c.set_expected_hosts([0, 1].into_iter().collect());
    let live: std::collections::BTreeSet<u32> = [0, 1].into_iter().collect();
    let heir = game::heir_of(2, &live).unwrap();
    let s = game::Succession {
        id: game::SuccessionId(7),
        predecessor: 2,
        predecessor_boot: 102,
        heir,
        kind: game::SuccessionKind::Crash,
        base_cut: game::CutId(3),
        moved: vec![((2, 0), 2)],
        survivors: vec![0, 1],
    };
    let attach_listing = |host: u32, epoch: u64, successions: Vec<(game::SuccessionId, bool)>| HostMsg::Attach {
        host,
        epoch,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions,
    };
    let other = 1 - heir;
    c.handle(other, attach_listing(other, 1, Vec::new()), 0);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "not read yet");
    c.handle(heir, attach_listing(heir, 1, vec![(s.id, true)]), 0);
    c.maintain(CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut before the open successions are read");
    c.successions_open(vec![s.clone()], CUT_EVERY_MS);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(0, s.id), (1, s.id)], "re-sent to the survivors attached");
    // The heir attached listing it applied and adopted before the read came: re-sent all
    // the same, and its next attach (from memory) counts.
    c.handle(heir, attach_listing(heir, 2, vec![(s.id, true)]), CUT_EVERY_MS);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "the heir answered from memory");
    assert_eq!(c.succession_progress(), vec![(s.id, vec![heir], true)]);
    // The other survivor re-attaches without having answered: the order again.
    c.handle(other, attach_listing(other, 2, Vec::new()), CUT_EVERY_MS);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(other, s.id)]);
    c.handle(other, HostMsg::SuccessionApplied { id: s.id }, CUT_EVERY_MS);
    c.maintain(3 * CUT_EVERY_MS);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.len(), 2, "every answer in: the settle cut begins");
    let cut = next[0].1;
    c.handle(0, HostMsg::CutPrepared { cut }, 3 * CUT_EVERY_MS);
    c.handle(1, HostMsg::CutPrepared { cut }, 3 * CUT_EVERY_MS);
    assert_eq!(c.take_decision().expect("decided").settles, vec![s.id]);
    c.decided(cut, true);
    commit_routed(&mut c);
    // A host that missed the `SuccessionDone` (its link dropped) lists the id again: the
    // coordinator no longer has it open — it was settled — and says so.
    c.handle(other, attach_listing(other, 3, vec![(s.id, false)]), 4 * CUT_EVERY_MS);
    let out = commit_routed(&mut c);
    assert_eq!(succession_done(&out), vec![(Target::Host(other), s.id, true)], "{out:?}");
}

/// The succeeded host's history-live regions park and its online players go offline, as
/// for an attach of it with nothing: a fortress its player held there is free for a
/// return (an offline claimant's), its directives and claim answers are dropped, and the
/// regions and players of the other hosts stand.
#[test]
fn host_succeeded_parks_its_regions_and_takes_its_players_offline() {
    let (mut c, civ, people) = coordinator();
    c.set_cut_epoch(3);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([0, 1].into_iter().collect());
    let (r0, r1) = (rc(0, 0), rc(14, 0));
    c.handle(1, embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, region } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    let here: Vec<(PersonId, [f32; 3])> = people[2..].iter().map(|p| (*p, [0.0; 3])).collect();
    c.handle(1, attach(1, 1, vec![RunningRegion { rc: r0, incarnation: 1, people: here }], vec![report(A, Some(Claim::Fortress(site)), false)]), 0);
    c.handle(0, attach(0, 1, vec![RunningRegion { rc: r1, incarnation: 1, people: Vec::new() }], vec![report(B, None, false)]), 0);
    commit(&mut c);
    assert_eq!((c.region_host(r0), c.online_on(A)), (Some(1), Some(1)));
    c.handle(0, HostMsg::Request { id: RequestId { host: 0, boot: 7, seq: 2 }, player: B, hop: PLACED, req: WorldRequest::ReturnTo(site) }, 0);
    let out = commit(&mut c);
    assert!(replies(&out).iter().any(|(_, r)| *r == WorldReply::EmbarkRefused(EmbarkRefusal::InUse)), "A plays it: {out:?}");
    // Host 1 lapses and is succeeded by host 0.
    read_leases(&mut c, 2, &[1], &[], 10);
    let d = c.declare_wanted(10).expect("declared");
    assert_eq!(d.heirs, vec![0]);
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(0))), 10);
    commit(&mut c);
    assert_eq!(c.region_host(r0), None, "host 1's region is no longer its");
    for p in &people[2..] {
        assert_ne!(c.history().person(*p).unwrap().location, Location::InRegion(r0), "{p:?}'s body is back in the history");
    }
    assert_eq!(c.online_on(A), None, "A went offline with its host");
    assert_eq!((c.region_host(r1), c.online_on(B)), (Some(0), Some(0)), "host 0's stand");
    c.handle(0, HostMsg::Request { id: RequestId { host: 0, boot: 7, seq: 3 }, player: B, hop: PLACED, req: WorldRequest::ReturnTo(site) }, 20);
    let out = commit(&mut c);
    assert!(
        replies(&out).iter().any(|(_, r)| *r == WorldReply::Returned { site, region }),
        "the fortress of the succeeded host's player is free for a return: {out:?}"
    );
}

/// A zombie of a succeeded boot (still linked to the coordinator, its process hung or cut
/// off) changes nothing: its presence, region and attach messages are dropped, a request
/// of it is refused `WorldUnavailable` (memoised); another boot of the ordinal is a new host.
#[test]
fn a_succeeded_boots_messages_are_dropped() {
    let (mut c, civ, people) = coordinator();
    c.set_cut_epoch(3);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([0, 1].into_iter().collect());
    let attach_as = |host: u32, epoch: u64, boot: u64, online: Vec<OnlinePlayer>| HostMsg::Attach {
        host,
        epoch,
        running: Vec::new(),
        online,
        generation: 0,
        boot,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
    };
    c.handle(0, attach_as(0, 1, 100, Vec::new()), 0);
    c.handle(1, attach_as(1, 1, 101, Vec::new()), 0);
    commit(&mut c);
    read_leases(&mut c, 2, &[1], &[], 10);
    let d = c.declare_wanted(10).expect("declared");
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(0))), 10);
    commit(&mut c);
    let r = rc(14, 0);
    c.handle(1, HostMsg::RegionStarted { rc: r, incarnation: 1 }, 20);
    c.handle(1, online(A, None, false), 20);
    c.handle(1, attach_as(1, 2, 101, vec![report(B, None, false)]), 20);
    let id = RequestId { host: 1, boot: 101, seq: 1 };
    let req = WorldRequest::Embark(EmbarkRequest { civ, people: people[0..2].to_vec(), region: rc(8, 0) });
    c.handle(1, HostMsg::Request { id, player: A, hop: PLACED, req: req.clone() }, 20);
    let out = commit_routed(&mut c);
    assert_eq!(c.region_host(r), None, "the zombie's region start is dropped");
    assert_eq!((c.online_on(A), c.online_on(B)), (None, None), "its players are not online");
    assert_eq!(c.claim_of(A), None, "no claim granted to its player");
    let refused: Vec<&WorldReply> = out
        .iter()
        .filter_map(|(to, m)| match m {
            CoordMsg::Reply { id: i, reply } if *i == id => {
                assert_eq!(*to, Target::Host(1));
                Some(reply)
            }
            _ => None,
        })
        .collect();
    assert_eq!(refused, vec![&WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable)], "{out:?}");
    // Memoised: a resend gets the same refusal, never an embark.
    c.handle(1, HostMsg::Request { id, player: A, hop: PLACED, req }, 21);
    assert!(replies(&commit(&mut c)).iter().all(|(_, r)| *r == WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable)));
    // Another boot of the ordinal is a new host.
    c.handle(1, attach_as(1, 3, 999, vec![report(B, None, false)]), 30);
    commit(&mut c);
    assert_eq!(c.online_on(B), Some(1), "a new boot is applied");
}

/// After a rollback the coordinator runs the new generation: a host attaching with an
/// older one (it missed the broadcast) is told the generation (and exits) — its attach is
/// not applied.
#[test]
fn a_host_attaching_after_a_rollback_is_told_the_generation() {
    let mut c = succeeding(3);
    c.rolled_back(4);
    commit_routed(&mut c);
    assert_eq!(c.world_generation(), 4);
    let stale = HostMsg::Attach {
        host: 2,
        epoch: 9,
        running: Vec::new(),
        online: vec![report(A, None, false)],
        generation: 0,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
    };
    c.handle(2, stale, 50);
    let out = commit_routed(&mut c);
    assert!(out.contains(&(Target::Host(2), CoordMsg::Generation(4))), "{out:?}");
    assert_eq!(c.online_on(A), None, "the stale attach is not applied");
}

/// R4 Task 9 (fix round 1): a person's crossing reported TWICE across a succession. The
/// dead host delivered P into its region `b` and reported it (`delivered: true`) before it
/// died; its succession parks `b` in the history (P abstract again, its body gone with the
/// host); then the heir, resolving the survivor's `Sent` entry for P, reports the same
/// crossing absorbed (`delivered: false`). The second step never makes a body: P stays
/// abstract — no spawn of P anywhere — exactly once.
#[test]
fn a_crossing_reported_delivered_then_absorbed_by_the_heir_is_one_person() {
    let (mut c, _civ, people) = coordinator();
    c.set_cut_epoch(3);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([0, 1].into_iter().collect());
    let (a, b) = (rc(0, 0), rc(1, 0));
    c.handle(0, attach(0, 1, Vec::new(), Vec::new()), 0);
    c.handle(1, attach(1, 1, Vec::new(), Vec::new()), 0);
    c.handle(0, HostMsg::RegionStarted { rc: a, incarnation: 1 }, 0);
    c.handle(1, HostMsg::RegionStarted { rc: b, incarnation: 1 }, 0);
    let out = commit(&mut c);
    let p = people[0];
    assert_eq!(body_delta(&out, p), 1, "P's body in a: {out:?}");
    // P walks from host 0's `a` into host 1's `b`, which reports it delivered.
    c.handle(1, HostMsg::PersonCrossed { person: p, to: b, delivered: true }, 0);
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(b));
    // Host 1 dies and host 0 succeeds it: `b` parks in the history with nobody.
    read_leases(&mut c, 2, &[1], &[], 10);
    let d = c.declare_wanted(10).expect("declared");
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(0))), 10);
    let mut spawned = body_delta(&commit(&mut c), p);
    assert!(!matches!(c.history().person(p).unwrap().location, Location::InRegion(_)), "P is abstract");
    let before = c.history().person(p).unwrap().location;
    // The heir reports the same crossing absorbed.
    c.handle(0, HostMsg::PersonCrossed { person: p, to: b, delivered: false }, 20);
    c.maintain(20);
    spawned += body_delta(&commit(&mut c), p);
    let after = c.history().person(p).unwrap().location;
    assert_eq!(spawned, 0, "no body of P is made by either step");
    assert!(!matches!(after, Location::InRegion(_)), "P stays abstract: {before:?} → {after:?}");
}

// ---- drain: a departed host's land on demand (R4 Task 11) ----------------------------------

/// The super-region host 0 asks for (`LandWanted`): one of the departed host's.
const WANTED: game::SuperRegion = (4, 4);

/// Hosts `0..n` succeeding; host `leaver` asks to leave, leaves in a committed cut and its
/// process is gone (detached). Every output drained; the clock at the leave's decision.
fn departed(n: u32, leaver: u32) -> (Coordinator, u64) {
    let mut c = succeeding(n);
    c.handle(leaver, HostMsg::LeaveRequest, 100);
    c.maintain(100 + game::LEAVE_BATCH_MS);
    let cut = begun(&commit_routed(&mut c))[0].1;
    let now = 100 + game::LEAVE_BATCH_MS + 1;
    for h in 0..n {
        c.handle(h, HostMsg::CutPrepared { cut }, now);
    }
    assert!(c.take_decision().expect("decided").commit);
    c.decided(cut, true);
    c.host_detached(leaver);
    commit_routed(&mut c);
    (c, now)
}

/// A read of hosts `0..n` at `now`: every host beating, `left` clean (its `hosts` row after
/// its leave cut), `back` (another boot of `left`) dirty again.
fn read_departed(c: &mut Coordinator, n: u32, left: u32, back: Option<u64>, now: u64) {
    let rows: Vec<HostSeen> = (0..n)
        .map(|h| {
            let mut row = seen(h, now + 1);
            if h == left {
                match back {
                    Some(boot) => row.boot = boot,
                    None => row.clean = true,
                }
            }
            row
        })
        .collect();
    c.observe_leases(&rows, now, now);
}

/// Host 0 asks for `WANTED` at `now`; the runtime looks it up (`owner`'s) and says so.
fn land_asked(c: &mut Coordinator, owner: u32, now: u64) {
    c.handle(0, HostMsg::LandWanted { sr: WANTED }, now);
    assert_eq!(c.take_land_lookups(), vec![WANTED], "the runtime looks the super-region up");
    c.land_wanted(owner, now);
}

/// Host 1 left cleanly; host 0 needs its land. Nothing is declared for `HEIR_WAIT_MS` from
/// the first request (asking again does not restart it); then ALL host 1's land goes to the
/// heir `heir_of` picks over the live hosts, as a `Leave` succession of the boot that left,
/// and its order goes to the survivors. Nothing under `Rollback` (`Static`).
#[test]
fn land_of_a_departed_host_waits_heir_wait_ms_then_is_inherited() {
    let (mut c, t) = departed(3, 1);
    read_departed(&mut c, 3, 1, None, t);
    land_asked(&mut c, 1, t);
    assert_eq!(c.declare_wanted(t), None, "it may come back: waited for");
    let half = t + game::HEIR_WAIT_MS / 2;
    read_departed(&mut c, 3, 1, None, half);
    land_asked(&mut c, 1, half);
    assert_eq!(c.take_land_lookups(), Vec::<game::SuperRegion>::new(), "each request looked up once");
    let last = t + game::HEIR_WAIT_MS - 1;
    read_departed(&mut c, 3, 1, None, last);
    assert_eq!(c.declare_wanted(last), None, "not before HEIR_WAIT_MS from the FIRST request");
    let due = t + game::HEIR_WAIT_MS;
    read_departed(&mut c, 3, 1, None, due);
    let d = c.declare_wanted(due).expect("its land is given away");
    let live: std::collections::BTreeSet<u32> = [0, 2].into_iter().collect();
    assert_eq!(d.dead, vec![(1, 101)], "the boot that left");
    assert_eq!(d.heirs, vec![game::heir_of(1, &live).unwrap()]);
    assert_eq!(d.kind, game::SuccessionKind::Leave);
    assert_eq!(c.declare_wanted(due), None, "one declaration in flight");
    let list = store_declares(&d, 3, 1, game::CutId(5));
    c.declared(game::Declared::Done(list.clone()), due);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(0, list[0].id), (2, list[0].id)], "the order goes to the survivors");
    read_departed(&mut c, 3, 1, None, due + 1);
    assert_eq!(c.declare_wanted(due + 1), None, "declared once");
    // Under `Rollback` (`Static`) a request is ignored.
    let (mut r, t) = departed(3, 1);
    r.set_crash_policy(game::CrashPolicy::Rollback);
    read_departed(&mut r, 3, 1, None, t);
    r.handle(0, HostMsg::LandWanted { sr: WANTED }, t);
    assert!(r.take_land_lookups().is_empty(), "Static looks nothing up");
    r.land_wanted(1, t);
    read_departed(&mut r, 3, 1, None, t + game::HEIR_WAIT_MS);
    assert_eq!(r.declare_wanted(t + game::HEIR_WAIT_MS), None, "Static never declares");
}

/// An ordinal at or beyond the configured host count (`VOXEL_HOSTS`) will not come back:
/// its land is given away at the first request; one below the count waits.
#[test]
fn land_of_an_ordinal_beyond_the_host_count_is_inherited_at_once() {
    let (mut c, t) = departed(3, 2);
    c.set_host_count(2);
    read_departed(&mut c, 3, 2, None, t);
    land_asked(&mut c, 2, t);
    let d = c.declare_wanted(t).expect("at once");
    assert_eq!((d.dead.clone(), d.kind), (vec![(2, 102)], game::SuccessionKind::Leave));
    let live: std::collections::BTreeSet<u32> = [0, 1].into_iter().collect();
    assert_eq!(d.heirs, vec![game::heir_of(2, &live).unwrap()]);
    // Within the count: waited for.
    let (mut w, t) = departed(3, 1);
    w.set_host_count(2);
    read_departed(&mut w, 3, 1, None, t);
    land_asked(&mut w, 1, t);
    assert_eq!(w.declare_wanted(t), None, "ordinal 1 of 2 may come back");
}

/// The departed host registers again (another boot: its row dirty) or attaches before
/// `HEIR_WAIT_MS`: the request is forgotten — nothing is declared, then or later; the store
/// refusing a declaration that raced the registration (`Gone`) declares nothing either.
#[test]
fn a_returning_host_cancels_the_wait() {
    let (mut c, t) = departed(2, 1);
    read_departed(&mut c, 2, 1, None, t);
    land_asked(&mut c, 1, t);
    // Its next boot registers: the hosts read shows it dirty.
    let mid = t + game::HEIR_WAIT_MS / 2;
    read_departed(&mut c, 2, 1, Some(901), mid);
    assert_eq!(c.declare_wanted(mid), None);
    for at in [t + game::HEIR_WAIT_MS, t + 3 * game::HEIR_WAIT_MS] {
        read_departed(&mut c, 2, 1, Some(901), at);
        assert_eq!(c.declare_wanted(at), None, "forgotten: never declared ({at} ms)");
    }
    // Its next boot attaches before the hosts are read again: forgotten too.
    let (mut a, t) = departed(2, 1);
    read_departed(&mut a, 2, 1, None, t);
    land_asked(&mut a, 1, t);
    let back = HostMsg::Attach { host: 1, epoch: 2, running: Vec::new(), online: Vec::new(), generation: 0, boot: 902, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new() };
    a.handle(1, back, t + 10);
    commit_routed(&mut a);
    // The read still shows the clean row of the boot that left (it lags the attach).
    read_departed(&mut a, 2, 1, None, t + game::HEIR_WAIT_MS);
    assert_eq!(a.declare_wanted(t + game::HEIR_WAIT_MS), None, "it is back: never declared");
    // It registered just as the wait ran out: the store refuses the declaration (`Gone`)
    // and the next read, showing the new boot, forgets the request.
    let (mut g, t) = departed(2, 1);
    read_departed(&mut g, 2, 1, None, t);
    land_asked(&mut g, 1, t);
    read_departed(&mut g, 2, 1, None, t + game::HEIR_WAIT_MS);
    assert!(g.declare_wanted(t + game::HEIR_WAIT_MS).is_some());
    g.declared(game::Declared::Gone(vec![1]), t + game::HEIR_WAIT_MS);
    assert!(orders(&commit_routed(&mut g)).is_empty(), "nothing declared");
    read_departed(&mut g, 2, 1, Some(903), t + game::HEIR_WAIT_MS + 1_000);
    assert_eq!(g.declare_wanted(t + game::HEIR_WAIT_MS + 1_000), None, "forgotten once the new boot is read");
}

/// Fix round 1 (minor 4): an owners lookup answered just after its owner's declaration
/// records the request again; it is pruned (the owner is declared), never kept for ever.
#[test]
fn a_request_for_land_already_declared_is_pruned() {
    let (mut c, t) = departed(2, 1);
    read_departed(&mut c, 2, 1, None, t);
    land_asked(&mut c, 1, t);
    let due = t + game::HEIR_WAIT_MS;
    read_departed(&mut c, 2, 1, None, due);
    let d = c.declare_wanted(due).expect("declared");
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(5))), due);
    assert!(c.lands_wanted().is_empty(), "the declared owner's request is done");
    // A late lookup answer for the same owner: not recorded again.
    c.land_wanted(1, due + 1);
    assert!(c.lands_wanted().is_empty(), "pruned");
    read_departed(&mut c, 2, 1, None, due + 2);
    assert_eq!(c.declare_wanted(due + 2), None, "declared once");
}

/// The counts relayed between participants of a running world cut.
fn counts(out: &[(Target, CoordMsg)]) -> Vec<(u32, game::CutId, u32, u64, u64)> {
    out.iter()
        .filter_map(|(to, m)| match (to, m) {
            (Target::Host(h), CoordMsg::CutPeerCount { cut, from, received_through, boot }) => {
                Some((*h, *cut, *from, *received_through, *boot))
            }
            _ => None,
        })
        .collect()
}

/// R4 Task 12: a participant's count of what it received from another participant of the
/// running cut goes to that participant (as from the reporter), unchanged; a count naming
/// the reporter itself, a host outside the cut, coming from a host outside it, or of
/// another cut is dropped — and once the cut is decided (aborted for a lapsed participant
/// here, as the coordinator does before any declaration) no count of it is relayed.
#[test]
fn cut_peer_counts_are_relayed_between_participants_only() {
    let mut c = succeeding(3);
    c.maintain(CUT_EVERY_MS);
    let begun_now = begun(&commit_routed(&mut c));
    assert_eq!(begun_now.len(), 3, "a cut of every host runs");
    let cut = begun_now[0].1;
    let now = CUT_EVERY_MS;
    c.handle(0, HostMsg::CutPeerCount { cut, peer: 1, received_through: 7, boot: 101 }, now);
    c.handle(1, HostMsg::CutPeerCount { cut, peer: 0, received_through: 0, boot: 0 }, now);
    assert_eq!(counts(&commit_routed(&mut c)), [(1, cut, 0, 7, 101), (0, cut, 1, 0, 0)], "relayed to the peer, as from the reporter");
    c.handle(0, HostMsg::CutPeerCount { cut, peer: 0, received_through: 3, boot: 100 }, now);
    c.handle(0, HostMsg::CutPeerCount { cut, peer: 5, received_through: 3, boot: 105 }, now);
    c.handle(5, HostMsg::CutPeerCount { cut, peer: 0, received_through: 3, boot: 100 }, now);
    c.handle(0, HostMsg::CutPeerCount { cut: game::CutId(cut.0 + 1), peer: 2, received_through: 3, boot: 102 }, now);
    assert!(counts(&commit_routed(&mut c)).is_empty(), "nothing relayed outside the cut's participants");
    // Host 2 lapses: the cut is decided aborted before anything is declared, and a count
    // sent after that is not relayed.
    read_leases(&mut c, 3, &[2], &[], now + 1);
    assert_eq!(c.declare_wanted(now + 1), None, "not while the cut runs");
    assert_eq!(c.take_decision().map(|d| (d.cut, d.commit)), Some((cut, false)), "aborted first");
    c.handle(0, HostMsg::CutPeerCount { cut, peer: 2, received_through: 4, boot: 102 }, now + 1);
    c.handle(2, HostMsg::CutPeerCount { cut, peer: 0, received_through: 4, boot: 100 }, now + 1);
    assert!(counts(&commit_routed(&mut c)).is_empty(), "no count of a decided cut is relayed");
    c.decided(cut, false);
    commit_routed(&mut c);
    c.handle(1, HostMsg::CutPeerCount { cut, peer: 0, received_through: 4, boot: 100 }, now + 2);
    assert!(counts(&commit_routed(&mut c)).is_empty(), "nor once it is over");
}

/// An `Embark` request of `player` made at placement `hop`.
fn embark_at(id: u64, player: PlayerId, hop: u64, civ: CivId, people: &[PersonId], region: RegionCoords) -> HostMsg {
    HostMsg::Request {
        id: rid(id),
        player,
        hop,
        req: WorldRequest::Embark(EmbarkRequest { civ, people: people.to_vec(), region }),
    }
}

/// A places P at hop 3, P moves to B at hop 4, and A's `Embark`, queued before the move,
/// arrives late: nothing is founded and P's claim is as it was; the refusal is memoised
/// (the same id, resent after P has moved on again, gets the same answer).
#[test]
fn a_request_from_an_older_placement_is_refused_not_granted() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, online_at(A, None, false, 3), 0);
    c.handle(1, online_at(A, None, false, 4), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(1), 4));
    let sites = c.history().sites().count();
    c.handle(0, embark_at(1, A, 3, civ, &people[0..2], rc(8, 0)), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable));
    assert_eq!(c.history().sites().count(), sites, "no site is founded");
    assert_eq!(c.claim_of(A), None, "the claim is unchanged");
    assert_eq!(c.online_on(A), Some(1), "A stays on host 1");
    // At the same hop as the known one, from a host A is not on: refused as well.
    c.handle(0, embark_at(2, A, 4, civ, &people[0..2], rc(8, 0)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable));
    assert_eq!(c.claim_of(A), None);
    // A query of the old placement is no state change: answered.
    c.handle(0, request(3, A, WorldRequest::EmbarkOptions), 0);
    assert!(matches!(reply_to(&commit(&mut c), 3), WorldReply::EmbarkOptions(_)));
    // Memoised: A is offline again at a higher hop, and the first request's id still
    // gets the refusal, never a grant.
    c.handle(1, HostMsg::PlayerOffline { player: A, hop: 5 }, 0);
    c.handle(0, embark_at(1, A, 3, civ, &people[0..2], rc(8, 0)), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable));
    assert_eq!(c.claim_of(A), None);
}

/// A request at the player's current hop, from the host it is online on, is granted.
#[test]
fn a_request_at_the_current_hop_is_granted() {
    let (mut c, civ, people) = coordinator();
    c.handle(0, online_at(A, None, false, 3), 0);
    c.handle(0, embark_at(1, A, 3, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, .. } = reply_to(&commit(&mut c), 1) else { panic!("granted") };
    assert_eq!(c.claim_of(A), Some(Claim::Fortress(site)));
}

// ---- R4 Task 14: a succession the operator opened offline; a rollback elsewhere --------------

/// A coordinator with world cuts and successions on, host 0 attached and expected, the open
/// successions known (none), its outputs drained.
fn heir_alone() -> Coordinator {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_world_tick(100);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([0].into_iter().collect());
    c.handle(0, attach(0, 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    c
}

/// The succession `worldctl retire-host` opens with no host alive: host 1 → heir 0, no
/// survivors.
fn offline_succession() -> game::Succession {
    game::Succession {
        id: game::SuccessionId(9),
        predecessor: 1,
        predecessor_boot: 101,
        heir: 0,
        kind: game::SuccessionKind::Crash,
        base_cut: game::CutId(3),
        moved: vec![((1, 0), 2)],
        survivors: Vec::new(),
    }
}

/// `worldctl retire-host` opened a succession while no host ran (no survivors): a running
/// coordinator learns it from its next `hosts` read — a dead row it never declared makes it
/// read the open successions again (once; no cut meanwhile) — and its order goes to the HEIR,
/// which is no survivor, at once if attached, else when it attaches. The heir's answers are
/// all a cut waits for; the settle cut names it, and its `SuccessionDone` reaches the heir.
#[test]
fn an_offline_succession_goes_to_its_heir_and_its_settle_tells_it() {
    let mut c = heir_alone();
    let s = offline_succession();
    let mut dead = seen(1, 50);
    dead.state = game::HOST_DEAD;
    c.observe_leases(&[seen(0, 60), dead.clone()], 60, 60);
    assert!(c.take_successions_reread(), "a dead row nobody here declared: the open successions are read again");
    assert!(!c.take_successions_reread(), "asked once");
    c.observe_leases(&[seen(0, 70), dead.clone()], 70, 70);
    assert!(!c.take_successions_reread(), "the same dead row asks once");
    c.maintain(2 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut until the open successions are read");
    c.successions_open(vec![s.clone()], 2 * CUT_EVERY_MS);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(0, s.id)], "the order goes to the heir, no survivor");
    let mut now = 3 * CUT_EVERY_MS;
    c.maintain(now);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "the heir has not answered");
    c.handle(0, HostMsg::SuccessionApplied { id: s.id }, now);
    c.handle(0, HostMsg::SuccessionAdopted { id: s.id }, now);
    assert_eq!(c.succession_progress(), vec![(s.id, vec![0], true)]);
    now += CUT_EVERY_MS;
    c.maintain(now);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![0], "the heir's cut begins");
    let cut = next[0].1;
    c.handle(0, HostMsg::CutPrepared { cut }, now);
    let d = c.take_decision().expect("decided");
    assert_eq!((d.commit, d.settles.clone()), (true, vec![s.id]), "the cut settles it");
    c.decided(cut, true);
    assert_eq!(succession_done(&commit_routed(&mut c)), vec![(Target::Host(0), s.id, true)], "the heir is told");
    assert!(c.succession_progress().is_empty());
    // A later read of the settled row (clean now) asks nothing.
    dead.clean = true;
    c.observe_leases(&[seen(0, 90), dead], 90, 90);
    assert!(!c.take_successions_reread());
    // Read before the heir attached: the order comes with its attach.
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.successions_unknown();
    c.successions_open(vec![s.clone()], 0);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "the heir is not attached");
    c.handle(0, attach(0, 1, Vec::new(), Vec::new()), 10);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(0, s.id)], "sent at its attach");
    // An heir that answered from memory (its next attach) is not ordered again.
    c.handle(0, HostMsg::SuccessionApplied { id: s.id }, 20);
    let listing = HostMsg::Attach {
        host: 0,
        epoch: 2,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: vec![(s.id, false)],
    };
    c.handle(0, listing, 30);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "applied already");
}

/// A world rollback the coordinator did not order (`worldctl rollback`): the store abandoned
/// the open succession behind its back. A host attaching at the newer generation makes it read
/// the open successions again; the run that is no longer open is dropped (abandoned, every
/// host told), and declarations go on as usual.
#[test]
fn a_rollback_elsewhere_drops_the_abandoned_runs() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, 2, 100);
    commit_routed(&mut c);
    assert!(!c.take_successions_reread());
    let newer = HostMsg::Attach {
        host: 0,
        epoch: 2,
        running: Vec::new(),
        online: Vec::new(),
        generation: 1,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
    };
    c.handle(0, newer, 200);
    assert!(c.take_successions_reread(), "a newer generation: the open successions are read again");
    c.maintain(200 + CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut until they are read");
    c.successions_open(Vec::new(), 300);
    let out = commit_routed(&mut c);
    assert_eq!(succession_done(&out), vec![(Target::Every, s.id, false)], "abandoned: {out:?}");
    assert!(c.succession_progress().is_empty());
    assert!(!c.rollback_wanted(), "nothing for this coordinator to roll back");
    // The runtime's store refusal of a decision at the old generation asks the same.
    c.successions_reread();
    assert!(c.take_successions_reread());
}

/// An offline succession waits for its heir to boot: its deadline starts with the heir's
/// order, and the lapses of boots that were dead already when the world was retired (the
/// heir's old boot, another host never restarted) are no second death. A boot seen beating
/// since it opened that then lapses is: the world rolls back.
#[test]
fn an_offline_run_waits_for_its_heir_and_ignores_the_dead_already() {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    let s = offline_succession();
    let mut dead = seen(1, 50);
    dead.state = game::HOST_DEAD;
    // Hosts 0 (the heir's old boot) and 2 died with host 1; the coordinator lives on.
    c.observe_leases(&[seen(0, 40), dead.clone(), seen(2, 40)], 0, 0);
    c.successions_open(vec![s.clone()], 0);
    let long = 10 * game::SUCCESSION_DEADLINE_MS;
    for now in [game::HOST_LEASE_MS, long] {
        c.observe_leases(&[seen(0, 40), dead.clone(), seen(2, 40)], now, now);
        c.maintain(now);
        assert!(!c.rollback_wanted(), "at {now}: no heir booted yet, the dead were dead already");
    }
    assert_eq!(c.leases().lapsed(long), vec![(0, 100), (2, 102)], "both lapsed");
    // The heir boots (a new boot), attaches, beats; host 2 stays down.
    let heir_row = |seen_ms: u64| HostSeen { boot: 900, ..seen(0, seen_ms) };
    c.handle(0, attach_as(0, 1, 900), long);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(0, s.id)]);
    c.observe_leases(&[heir_row(long), dead.clone(), seen(2, 40)], long, long);
    c.observe_leases(&[heir_row(long + 500), dead.clone(), seen(2, 40)], long + 500, long + 500);
    c.maintain(long + game::SUCCESSION_DEADLINE_MS - 1);
    assert!(!c.rollback_wanted(), "within the deadline from the heir's order");
    c.handle(0, HostMsg::SuccessionApplied { id: s.id }, long + 600);
    c.handle(0, HostMsg::SuccessionAdopted { id: s.id }, long + 600);
    c.maintain(long + 2 * game::SUCCESSION_DEADLINE_MS);
    assert!(!c.rollback_wanted(), "answered: the deadline no longer matters");
    // The heir's new boot, seen beating since the run opened, dies before the settle.
    let late = long + 2 * game::SUCCESSION_DEADLINE_MS + game::HOST_LEASE_MS;
    c.observe_leases(&[heir_row(long + 500), dead.clone(), seen(2, 40)], late, late);
    c.maintain(late);
    assert!(c.rollback_wanted(), "a death during the succession");
    // Unanswered past the deadline counted from the heir's order: a rollback too.
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.successions_open(vec![s.clone()], 0);
    c.handle(0, attach_as(0, 1, 7), 100);
    commit_routed(&mut c);
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS - 1);
    assert!(!c.rollback_wanted());
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS);
    assert!(c.rollback_wanted(), "the heir never answered");
}

/// Host `host`'s `Attach` of boot `boot` (epoch `epoch`, nothing running or online).
fn attach_as(host: u32, epoch: u64, boot: u64) -> HostMsg {
    HostMsg::Attach {
        host,
        epoch,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
    }
}
