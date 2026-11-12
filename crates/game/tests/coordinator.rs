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
    c.handle(0, HostMsg::PlayerOffline { player: A }, 0);
    c.handle(0, request(4, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 4), WorldReply::Returned { site, region });
    assert_eq!(c.claim_of(B), Some(Claim::Fortress(site)));
    assert_eq!(c.claim_of(A), None, "the offline claimant lost it");
    // A comes back holding the old claim in its record: it is B's now.
    c.handle(0, online(A, Some(Claim::Fortress(site)), false), 0);
    assert_eq!(c.claim_of(A), None);
    assert!(
        c.take_msgs().contains(&CoordMsg::ClaimIs { player: A, claim: None }),
        "A's host is told to drop the fortress B plays"
    );
    c.handle(0, request(5, A, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 5), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    // B retires: the site is free again.
    c.handle(0, HostMsg::ReleaseClaim { player: B }, 0);
    c.handle(0, request(6, A, WorldRequest::ReturnTo(site)), 0);
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
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running: vec![running], online: Vec::new(), generation: 0 }, 60_000);
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
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running: vec![running(Vec::new())], online: Vec::new(), generation: 0 }, 0);
    let out = commit(&mut c);
    let attached = out.iter().position(|m| matches!(m, CoordMsg::Attached { epoch: 1 })).expect("the echo");
    for p in &people {
        assert_eq!(body_delta(&out[attached..], *p), 1, "{p:?} spawned once: {out:?}");
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r));
    }
    // Re-attached with every body there: nothing more.
    let bodies: Vec<(PersonId, [f32; 3])> = people.iter().map(|p| (*p, [0.0; 3])).collect();
    c.handle(0, HostMsg::Attach { host: 0, epoch: 2, running: vec![running(bodies)], online: Vec::new(), generation: 0 }, 0);
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
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running, online: Vec::new(), generation: 0 }, 0);
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
    let online = vec![OnlinePlayer { player: A, claim: None, claim_pending: true }];
    c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running: vec![running], online, generation: 0 }, 0);
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
        c.handle(0, HostMsg::Attach { host: 0, epoch: 1, running, online: Vec::new(), generation: 0 }, 0);
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
    HostMsg::PlayerOnline(OnlinePlayer { player, claim, claim_pending })
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
    assert!(commit(&mut c).contains(&CoordMsg::ClaimIs { player: B, claim: None }));
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
    HostMsg::Attach { host, epoch, running, online, generation: 0 }
}

fn report(player: PlayerId, claim: Option<Claim>, claim_pending: bool) -> OnlinePlayer {
    OnlinePlayer { player, claim, claim_pending }
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
    c.handle(1, online(A, Some(Claim::Fortress(site)), false), 0);
    c.handle(0, HostMsg::PlayerOffline { player: A }, 0);
    assert_eq!(c.online_on(A), Some(1), "online on the new host");
    assert!(!commit(&mut c).iter().any(|m| matches!(m, CoordMsg::ClaimIs { .. })), "nothing to correct");
    c.handle(0, request(2, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), WorldReply::EmbarkRefused(EmbarkRefusal::InUse));
    c.handle(1, HostMsg::PlayerOffline { player: A }, 0);
    assert_eq!(c.online_on(A), None);
    c.handle(0, request(3, B, WorldRequest::ReturnTo(site)), 0);
    assert_eq!(reply_to(&commit(&mut c), 3), WorldReply::Returned { site, region });
}

/// The `ClaimIs` messages for `player` in `out`, with their targets.
fn claim_is(out: &[(Target, CoordMsg)], player: PlayerId) -> Vec<(Target, Option<Claim>)> {
    out.iter()
        .filter_map(|(to, m)| match m {
            CoordMsg::ClaimIs { player: p, claim } if *p == player => Some((*to, *claim)),
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
