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
/// Seeds per seeded test.
const SEEDS: u64 = 32;

fn rc(x: i32, z: i32) -> RegionCoords {
    RegionCoords::new(x, z)
}

fn rid(seq: u64) -> RequestId {
    RequestId { host: 0, boot: 7, seq }
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
    let first = c.take_out();
    assert!(matches!(first.as_slice(), [CoordMsg::View { .. }]), "a new coordinator opens with its view: {first:?}");
    (c, civ, people)
}

fn embark(id: u64, player: PlayerId, civ: CivId, people: &[PersonId], region: RegionCoords) -> HostMsg {
    HostMsg::Request { id: rid(id), player, req: WorldRequest::Embark(EmbarkRequest { civ, people: people.to_vec(), region }) }
}

fn request(id: u64, player: PlayerId, req: WorldRequest) -> HostMsg {
    HostMsg::Request { id: rid(id), player, req }
}

/// Save as a store that commits at once would, then take what may leave.
fn commit(c: &mut Coordinator) -> Vec<CoordMsg> {
    if c.take_save().is_some() {
        c.saved();
    }
    c.take_out()
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
    c.handle(embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    c.handle(embark(2, B, civ, &people[2..4], rc(8, 0)), 0);
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
    c.handle(embark(3, A, civ, &people[4..6], rc(14, 0)), 0);
    let out = commit(&mut c);
    assert_eq!(reply_to(&out, 3), WorldReply::EmbarkRefused(EmbarkRefusal::AlreadyHasFortress));
    c.handle(request(4, A, WorldRequest::StartAdventure(SiteId(0))), 0);
    let out = commit(&mut c);
    assert_eq!(reply_to(&out, 4), WorldReply::AdventureRefused(AdventureRefusal::AlreadyPlaying));
}

#[test]
fn embark_reply_waits_for_save() {
    let (mut c, civ, people) = coordinator();
    // A refusal grants nothing: it leaves at once.
    c.handle(embark(1, B, civ, &people[0..2], rc(0, 0)), 0);
    assert_eq!(reply_to(&c.take_out(), 1), WorldReply::EmbarkRefused(EmbarkRefusal::Occupied));
    c.handle(embark(2, A, civ, &people[0..2], rc(8, 0)), 0);
    c.maintain(0);
    assert!(replies(&c.take_out()).is_empty(), "no claim-granting reply before a save");
    // Anything decided after it waits behind it, in order.
    c.handle(request(3, B, WorldRequest::PersonInfo(people[3])), 0);
    assert!(c.take_out().is_empty(), "later messages keep their place behind the waiting reply");
    let state = c.take_save().expect("a save is due while a reply waits");
    let saved = game::history::persist::decode(&state.history, grid()).expect("the saved history loads");
    assert_eq!(saved.sites().count(), 2, "the save holds the founded site");
    assert_eq!(state.claims, vec![(A, Claim::Fortress(SiteId(1)))], "and the claim");
    assert!(c.take_save().is_none(), "one save at a time");
    assert!(c.take_out().is_empty(), "nothing leaves before the save commits");
    c.saved();
    let out = c.take_out();
    let r = replies(&out);
    assert_eq!(r.len(), 2, "{out:?}");
    assert!(matches!(r[0], (id, WorldReply::Embarked { .. }) if id == rid(2)), "{r:?}");
    assert!(matches!(r[1], (id, WorldReply::PersonInfo(Some(_))) if id == rid(3)), "{r:?}");
}

#[test]
fn failed_save_keeps_reply_waiting() {
    let (mut c, civ, people) = coordinator();
    c.handle(embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    assert!(c.take_save().is_some());
    c.save_failed();
    assert!(c.take_out().is_empty(), "a failed save releases nothing");
    assert!(c.take_save().is_some(), "and the next take_save retries");
    c.saved();
    assert!(matches!(reply_to(&c.take_out(), 1), WorldReply::Embarked { .. }));
}

#[test]
fn request_replay_answers_from_memo() {
    let (mut c, civ, people) = coordinator();
    c.handle(embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let first = reply_to(&commit(&mut c), 1);
    assert!(matches!(first, WorldReply::Embarked { .. }), "{first:?}");
    let sites = c.history().sites().count();
    c.handle(embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), first, "the replay is answered identically");
    assert_eq!(c.history().sites().count(), sites, "and founds nothing");
    // The memo is saved with the state: a restarted coordinator answers the replay too.
    c.request_save();
    let state = c.take_save().expect("asked for");
    c.saved();
    let mut c2 = Coordinator::from_state(state, grid(), 5_000).expect("state loads");
    c2.take_out();
    c2.handle(embark(1, A, civ, &people[0..2], rc(8, 0)), 5_000);
    assert_eq!(reply_to(&commit(&mut c2), 1), first, "the restarted coordinator answers from its memo");
    assert_eq!(c2.history().sites().count(), sites, "and founds nothing");
}

#[test]
fn return_in_use_only_while_online() {
    let (mut c, civ, people) = coordinator();
    c.handle(embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    let WorldReply::Embarked { site, region } = reply_to(&commit(&mut c), 1) else { panic!("embarks") };
    c.handle(request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    c.handle(request(3, B, WorldRequest::ReturnTo(SiteId(0))), 0);
    assert_eq!(
        reply_to(&commit(&mut c), 3),
        WorldReply::EmbarkRefused(EmbarkRefusal::NotPlayerFounded),
        "only player-founded sites"
    );
    c.handle(HostMsg::PlayerOffline { player: A }, 0);
    c.handle(request(4, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 4), WorldReply::Returned { site, region });
    assert_eq!(c.claim_of(B), Some(Claim::Fortress(site)));
    assert_eq!(c.claim_of(A), None, "the offline claimant lost it");
    // A comes back holding the old claim in its record: it is B's now.
    c.handle(HostMsg::PlayerOnline { player: A, claim: Some(Claim::Fortress(site)) }, 0);
    assert_eq!(c.claim_of(A), None);
    c.handle(request(5, A, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 5), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    // B retires: the site is free again.
    c.handle(HostMsg::ReleaseClaim { player: B }, 0);
    c.handle(request(6, A, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 6), WorldReply::Returned { site, region });
}

#[test]
fn directive_bounce_is_redelivered_once() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit(&mut c);
    assert!(matches!(out.as_slice(), [CoordMsg::SpawnPeople { incarnation: 1, events, .. }] if events.is_empty()));
    // An embark into the running region: its party spawns there by directive.
    let party = &people[0..2];
    c.handle(embark(1, A, civ, party, r), 0);
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
    c.handle(HostMsg::RegionStopping { rc: r }, 0);
    c.handle(HostMsg::DirectiveBounced { directive: spawns[0].clone(), incarnation: 1 }, 0);
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    let out = commit(&mut c);
    assert_eq!(events_about(&out, party[0]).len(), 1, "the bounced spawn is delivered once: {out:?}");
    assert!(matches!(events_about(&out, party[0])[0], GameEventKind::SpawnPerson(_)));
    assert!(events_about(&out, party[1]).is_empty(), "the delivered one is left alone: {out:?}");

    // Now both spawns bounce and `r` parks before it starts again: the catch-up is the one body.
    c.handle(HostMsg::RegionStopping { rc: r }, 0);
    for d in &spawns {
        c.handle(HostMsg::DirectiveBounced { directive: d.clone(), incarnation: 2 }, 0);
    }
    c.handle(HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 3 }, 0);
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
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit(&mut c);
    assert_eq!(events_about(&out, p).len(), 1, "P has a body");
    assert_eq!(events_about(&out, q).len(), 1, "Q has a body");
    // P dies: the despawn waits for a save; the save commits, then the coordinator crashes
    // before the despawn leaves.
    c.maintain(0);
    assert!(!c.history().person(p).unwrap().alive());
    assert!(events_about(&c.take_out(), p).is_empty());
    let state = c.take_save().expect("the despawn waits on a save");
    drop(c);

    let mut c = Coordinator::from_state(state, grid(), 60_000).expect("state loads");
    assert!(matches!(c.take_out().as_slice(), [CoordMsg::View { .. }]));
    let running = RunningRegion { rc: r, incarnation: 1, people: vec![(p, [1.0, 2.0, 3.0]), (q, [4.0, 5.0, 6.0])] };
    c.handle(HostMsg::Attach { host: 0, epoch: 1, running: vec![running], online: Vec::new() }, 60_000);
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
    c.handle(HostMsg::RegionStopping { rc: r }, 60_000);
    c.handle(HostMsg::RegionParked { rc: r, people: vec![(q, [4.0, 5.0, 6.0])] }, 60_000);
    assert_eq!(c.history().person(q).unwrap().location, Location::AtSite(SiteId(0)));
}

#[test]
fn state_round_trips() {
    let (mut c, civ, people) = coordinator();
    c.handle(embark(1, A, civ, &people[0..2], rc(8, 0)), 0);
    c.handle(request(2, B, WorldRequest::StartAdventure(SiteId(0))), 0);
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
    c2.handle(request(3, p3, WorldRequest::ReturnTo(site)), 0);
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
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(embark(1, A, civ, &[p], r), 0);
    let out = commit(&mut c);
    let spawn = directives_for(&out, r);
    assert_eq!(spawn.len(), 1, "{out:?}");
    assert_eq!(spawn[0].1, 1);
    let mut bodies = 0; // P's bodies on the host: the spawn has not been applied yet
    // The host parks `r` (no body of P in it) and starts it again before the spawn lands.
    c.handle(HostMsg::RegionStopping { rc: r }, 0);
    c.handle(HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    let out = commit(&mut c);
    bodies += body_delta(&out, p);
    assert_eq!(bodies, 1, "incarnation 2's catch-up spawns P: {out:?}");
    // The delayed incarnation-1 spawn is bounced by the host (wrong incarnation).
    c.handle(HostMsg::DirectiveBounced { directive: spawn[0].0.clone(), incarnation: 1 }, 0);
    // Stop and start again without a park (P's body stays).
    c.handle(HostMsg::RegionStopping { rc: r }, 0);
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 3 }, 0);
    c.maintain(0);
    let out = commit(&mut c);
    bodies += body_delta(&out, p);
    assert_eq!(bodies, 1, "P keeps exactly one body: {out:?}");
}

/// A directive waiting on a save when the host re-attaches is superseded: the attach
/// re-decides the region from its bodies, and the stale spawn never leaves.
#[test]
fn attach_supersedes_unsent_directives() {
    let (mut c, civ, people) = coordinator();
    let r = rc(8, 0);
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(embark(1, A, civ, &[p], r), 0);
    // A later ungated message waits behind the spawn.
    c.handle(request(2, B, WorldRequest::PersonInfo(people[3])), 0);
    assert!(c.take_out().is_empty());
    // The link dropped; the host re-attaches running `r` with no body of P.
    let running = RunningRegion { rc: r, incarnation: 1, people: Vec::new() };
    c.handle(HostMsg::Attach { host: 0, epoch: 1, running: vec![running], online: vec![(A, None)] }, 0);
    let out = commit(&mut c);
    assert!(directives_for(&out, r).is_empty(), "the stale spawn is dropped: {out:?}");
    assert!(matches!(reply_to(&out, 1), WorldReply::Embarked { .. }), "the reply still leaves");
    assert!(matches!(reply_to(&out, 2), WorldReply::PersonInfo(Some(_))));
    let pr = c.history().person(p).unwrap();
    assert_ne!(pr.location, Location::InRegion(r), "P has no body, so the history has P abstract");
    // `r` starts again: P is spawned once by its catch-up.
    c.handle(HostMsg::RegionStopping { rc: r }, 0);
    c.handle(HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    c.handle(HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    assert_eq!(body_delta(&commit(&mut c), p), 1);
}

/// A body that crossed between two running regions while the link was down is listed
/// in its new region only: whichever region is reattached first, it keeps that body.
#[test]
fn reattach_keeps_a_body_that_crossed_between_listed_regions() {
    for order in [false, true] {
        let (mut c, _civ, people) = coordinator();
        let (r0, r1) = (rc(0, 0), rc(1, 0));
        c.handle(HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
        c.handle(HostMsg::RegionStarted { rc: r1, incarnation: 1 }, 0);
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
        c.handle(HostMsg::Attach { host: 0, epoch: 1, running, online: Vec::new() }, 0);
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
    c.handle(HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(HostMsg::PersonCrossed { person: p, to: r1, delivered: true }, 0);
    let out = commit(&mut c);
    assert_eq!(body_delta(&out, p), 0, "{out:?}");
    let pr = c.history().person(p).unwrap();
    assert_eq!(pr.location, Location::InRegion(r1));
    // Parking `r1` hands P back to the abstract history.
    c.handle(HostMsg::RegionStopping { rc: r1 }, 0);
    c.handle(HostMsg::RegionParked { rc: r1, people: vec![(p, [0.0; 3])] }, 0);
    assert_ne!(c.history().person(p).unwrap().location, Location::InRegion(r1));
}
