//! The world coordinator core (R2b Task 4): requests validated and answered once, durable
//! before visible, claims, bounced directives, reattach after a restart, the saved state,
//! and the inline link's seeded delays. Pure `Coordinator` — the host is played by sending
//! `HostMsg`s directly.

use crate::common::{h, populated_site, start_request, CoreInputs};
use game::coordinator::*;
use game::history::*;
use game::{HostId, GameEventKind, InlineSchedule, PlayerId, RegionCoords};

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
    RequestId { host: h(0), boot: 7, seq }
}

/// What leaves once a store that commits at once wrote the pending transition (if any):
/// whatever host each message is for (one host in most tests).
trait TakeMsgs {
    fn take_msgs(&mut self) -> Vec<CoordMsg>;
}

impl TakeMsgs for Coordinator {
    fn take_msgs(&mut self) -> Vec<CoordMsg> {
        commit_routed(self).into_iter().map(|(_, m)| m).collect()
    }
}

/// The pending transition written by a store that commits at once, then what may leave,
/// with targets.
fn commit_routed(c: &mut Coordinator) -> Vec<(Target, CoordMsg)> {
    let mut out = written(c).map(|t| t.out).unwrap_or_default();
    out.extend(c.take_out());
    out
}

/// The pending transition, written by a store that commits at once (acknowledged).
fn written(c: &mut Coordinator) -> Option<Transition> {
    let t = c.take_transition();
    if t.is_some() {
        c.transition_written(c.version() + 1);
    }
    t
}

/// A core restarted from transition `t` (`history`: the history the store holds, when `t`
/// carried none) as replica `process`, at store time `now_ms` — its first input its
/// registration alone (Elastic 2b Task 4: the restart of the only replica; every replica
/// registered before it is gone).
fn reload(t: &Transition, history: &[u8], process: ReplicaId, now_ms: u64) -> Coordinator {
    let mut c = Coordinator::load(&t.core, t.history.as_deref().unwrap_or(history), grid(), process, now_ms).expect("the projection loads");
    c.apply(Input::ReplicaJoined { me: process, alone: true }, now_ms);
    c
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
    h.found_site(civ, rc(0, 0), &people, None);
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

fn request(id: u64, player: PlayerId, req: WorldRequest) -> HostMsg {
    HostMsg::Request { id: rid(id), player, hop: PLACED, req }
}

/// A `StartAdventure` of `player` at `site`, made at [`PLACED`].
fn adventure(id: u64, player: PlayerId, site: SiteId) -> HostMsg {
    request(id, player, start_request(site))
}

/// The site an adventure may start at in `c`'s history.
fn site_of(c: &Coordinator) -> SiteId {
    populated_site(c.history())
}

/// A fresh coordinator over [`world`] with a second populated site (at (14, 0), founders of
/// its own), its opening `View` drained: the two sites.
fn coordinator_two_sites() -> (Coordinator, SiteId, SiteId) {
    let (mut h, civ, _) = world();
    let s = start();
    let people: Vec<PersonId> = (0..2).map(|_| h.add_founder(Sex::Female, years_before(s, 20))).collect();
    let s2 = h.found_site(civ, rc(14, 0), &people, None);
    let s1 = populated_site(&h);
    assert_ne!(s1, s2);
    let mut c = Coordinator::new(h, 0);
    c.take_msgs();
    (c, s1, s2)
}

/// The history founds a site of `civ` in `region` by its own path (an NPC founding, from the
/// populated site), with `n` new founders: their spawns, if `region` runs, are directives
/// that leave at the next `handle` / `maintain`. The founders.
fn npc_founding(c: &mut Coordinator, civ: CivId, region: RegionCoords, n: usize) -> Vec<PersonId> {
    let from = site_of(c);
    let h = c.history_mut();
    let founders: Vec<PersonId> = (0..n).map(|_| h.add_founder(Sex::Male, years_before(start(), 20))).collect();
    h.found_site(civ, region, &founders, Some(from));
    founders
}

/// Write as a store that commits at once would, then take what may leave.
fn commit(c: &mut Coordinator) -> Vec<CoordMsg> {
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

/// Elastic 2a: nothing a durable change decided leaves before its transition — not even what
/// was decided with it that changes nothing durable (the order is kept) — and the transition
/// holds the state that decided it. A query decided while nothing durable changed leaves at
/// once ([`Coordinator::take_out`]).
#[test]
fn no_output_is_released_without_its_transition() {
    let (mut c, _civ, people) = coordinator();
    // A query changes nothing durable: its answer leaves at once.
    c.handle(h(0), request(1, B, WorldRequest::PersonInfo(people[2])), 0);
    assert!(c.take_transition().is_none(), "nothing durable changed");
    assert!(matches!(reply_to(&c.take_out().into_iter().map(|(_, m)| m).collect::<Vec<_>>(), 1), WorldReply::PersonInfo(Some(_))));
    let site = site_of(&c);
    let history = c.encode_projection().1;
    c.handle(h(0), adventure(2, A, site), 0);
    c.maintain(0);
    assert!(c.take_out().is_empty(), "no message leaves before the transition holding its state");
    // Anything decided after it waits behind it, in order.
    c.handle(h(0), request(3, B, WorldRequest::PersonInfo(people[3])), 0);
    assert!(c.take_out().is_empty(), "later messages keep their place behind the transition");
    let t = c.take_transition().expect("a durable change is pending");
    let saved = Coordinator::load(&t.core, t.history.as_deref().unwrap_or(&history), grid(), ReplicaId(1), 0)
        .expect("the transition loads");
    assert_eq!(saved.claims(), vec![(A, Claim::Adventure(site))], "the transition holds the claim");
    assert!(c.take_transition().is_none(), "nothing changed since");
    assert!(c.take_out().is_empty(), "the transition's messages are its own");
    let out: Vec<CoordMsg> = t.out.into_iter().map(|(_, m)| m).collect();
    let r = replies(&out);
    assert_eq!(r.len(), 2, "{out:?}");
    assert!(matches!(r[0], (id, WorldReply::AdventureStarted { site: s, .. }) if id == rid(2) && s == site), "{r:?}");
    assert!(matches!(r[1], (id, WorldReply::PersonInfo(Some(_))) if id == rid(3)), "{r:?}");
}

/// A transition whose write failed is handed back: its messages wait (none is lost or
/// doubled), and the next transition writes everything again, the history included.
#[test]
fn a_failed_write_keeps_its_messages_and_writes_again() {
    let (mut c, _civ, people) = coordinator();
    c.take_transition();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    // The history changed with it (as a day's turn changes it): the transition carries it.
    c.history_mut();
    let t = c.take_transition().expect("pending");
    assert!(t.history.is_some());
    c.handle(h(0), request(2, B, WorldRequest::PersonInfo(people[3])), 0);
    c.transition_failed(t);
    assert!(c.take_out().is_empty(), "a failed write releases nothing");
    let again = c.take_transition().expect("and the next transition writes again");
    assert!(again.history.is_some(), "the history with it");
    let r = replies(&again.out.iter().map(|(_, m)| m.clone()).collect::<Vec<_>>());
    assert!(matches!(r.as_slice(), [(a, WorldReply::AdventureStarted { .. }), (b, WorldReply::PersonInfo(_))] if *a == rid(1) && *b == rid(2)), "once each, in order: {r:?}");
    assert_eq!(reload(&again, &[], ReplicaId(1), 0).claims(), vec![(A, Claim::Adventure(site))], "the rewrite holds the claim");
}

#[test]
fn request_replay_answers_from_memo() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    let first = reply_to(&commit(&mut c), 1);
    assert!(matches!(first, WorldReply::AdventureStarted { site: s, .. } if s == site), "{first:?}");
    // Executed again, the request would be refused `AlreadyPlaying`: the same answer is the memo's.
    c.handle(h(0), adventure(1, A, site), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), first, "the replay is answered identically");
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)), "and grants nothing more");
    // The memo is in the projection: a restarted coordinator answers the replay too.
    let (core, history) = c.encode_projection();
    let mut c2 = Coordinator::load(&core, &history, grid(), ReplicaId(1), 5_000).expect("the projection loads");
    assert!(c2.take_msgs().is_empty(), "a load sends nothing");
    c2.handle(h(0), adventure(1, A, site), 5_000);
    assert_eq!(reply_to(&commit(&mut c2), 1), first, "the restarted coordinator answers from its memo");
    assert_eq!(c2.claim_of(A), Some(Claim::Adventure(site)), "and grants nothing more");
}

#[test]
fn directive_bounce_is_redelivered_once() {
    let (mut c, civ, _people) = coordinator();
    let r = rc(8, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit(&mut c);
    assert!(matches!(out.as_slice(), [CoordMsg::SpawnPeople { incarnation: 1, events, .. }] if events.is_empty()));
    // A site founded into the running region: its party spawns there by directive.
    let founders = npc_founding(&mut c, civ, r, 2);
    let party = &founders[..];
    c.maintain(0);
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
    c.handle(h(0), HostMsg::RegionStopping { rc: r }, 0);
    c.handle(h(0), HostMsg::DirectiveBounced { directive: spawns[0].clone(), incarnation: 1 }, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    let out = commit(&mut c);
    assert_eq!(events_about(&out, party[0]).len(), 1, "the bounced spawn is delivered once: {out:?}");
    assert!(matches!(events_about(&out, party[0])[0], GameEventKind::SpawnPerson(_)));
    assert!(events_about(&out, party[1]).is_empty(), "the delivered one is left alone: {out:?}");

    // Now both spawns bounce and `r` parks before it starts again: the catch-up is the one body.
    c.handle(h(0), HostMsg::RegionStopping { rc: r }, 0);
    for d in &spawns {
        c.handle(h(0), HostMsg::DirectiveBounced { directive: d.clone(), incarnation: 2 }, 0);
    }
    c.handle(h(0), HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 3 }, 0);
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
    let (mut hist, _civ, people) = world();
    let q = people[0];
    // A 170-year-old member of site 0: dies on the first advance.
    let p = hist.add_founder(Sex::Male, years_before(start(), 170));
    hist.sites.get_mut(&SiteId(0)).unwrap().members.insert(p);
    let pr = hist.people.get_mut(&p).unwrap();
    pr.site = Some(SiteId(0));
    pr.location = Location::AtSite(SiteId(0));
    let r = rc(0, 0);
    let mut c = Coordinator::new(hist, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit(&mut c);
    assert_eq!(events_about(&out, p).len(), 1, "P has a body");
    assert_eq!(events_about(&out, q).len(), 1, "Q has a body");
    // P dies: the despawn waits for its transition; the write commits, then the coordinator
    // crashes before the despawn leaves.
    c.maintain(0);
    assert!(!c.history().person(p).unwrap().alive());
    assert!(c.take_out().is_empty());
    let t = c.take_transition().expect("the despawn waits on a transition");
    assert!(!events_about(&t.out.iter().map(|(_, m)| m.clone()).collect::<Vec<_>>(), p).is_empty(), "it holds the despawn");
    drop(c);

    let mut c = reload(&t, &[], ReplicaId(1), 60_000);
    assert!(c.take_msgs().is_empty(), "a load sends nothing");
    let running = RunningRegion { rc: r, incarnation: 1, people: vec![(p, [1.0, 2.0, 3.0]), (q, [4.0, 5.0, 6.0])] };
    c.handle(h(0), HostMsg::Attach { host: h(0), epoch: 1, running: vec![running], online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 60_000);
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
    c.handle(h(0), HostMsg::RegionStopping { rc: r }, 60_000);
    c.handle(h(0), HostMsg::RegionParked { rc: r, people: vec![(q, [4.0, 5.0, 6.0])] }, 60_000);
    assert_eq!(c.history().person(q).unwrap().location, Location::AtSite(SiteId(0)));
}

/// The projection round-trips: a core loaded from a transition holds the claims, the memo and
/// the presence as written (a claimant online stays online until its host's `Attach` says
/// otherwise), writes nothing until something changes, and the format is checked.
#[test]
fn state_round_trips() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    let history = c.encode_projection().1;
    c.handle(h(0), adventure(1, A, site), 0);
    c.handle(h(0), adventure(2, B, site), 0);
    let t = c.take_transition().expect("pending");
    let WorldReply::AdventureStarted { site: started, .. } = reply_to(&t.out.iter().map(|(_, m)| m.clone()).collect::<Vec<_>>(), 1)
    else {
        panic!("starts")
    };
    assert_eq!(started, site);
    assert_eq!(t.core[0], COORDINATOR_STATE_VERSION);
    assert_eq!(COORDINATOR_STATE_VERSION, 5, "core v5: replica stamps, the cut ids' high half durable");
    let mut c2 = reload(&t, &history, ReplicaId(1), 0);
    assert_eq!(c2.claims(), vec![(A, Claim::Adventure(site)), (B, Claim::Adventure(site))]);
    assert_eq!(c2.memo_ids().len(), 2);
    // Its registration (Elastic 2b Task 4: the live set) is its first transition.
    assert!(written(&mut c2).is_some_and(|t| t.out.is_empty() && t.decisions.is_empty()), "the join alone");
    assert!(c2.take_transition().is_none(), "a loaded projection writes nothing until something changes");
    let refuse = |bytes: &[u8]| Coordinator::load(bytes, &history, grid(), ReplicaId(1), 0).err();
    assert!(matches!(refuse(&[COORDINATOR_STATE_VERSION + 1]), Some(game::history::persist::HistoryLoadError::Corrupt(_))), "an unknown format is refused");
    assert!(matches!(refuse(&[]), Some(game::history::persist::HistoryLoadError::Corrupt(_))));
    for old in [1, 2, 3, 4] {
        assert!(
            matches!(refuse(&[old, 0, 0]), Some(game::history::persist::HistoryLoadError::OldVersion { found }) if found == u32::from(old)),
            "format {old} is refused as old: re-forge"
        );
    }
    // A's claim loads online on host 0 (its presence is in the projection), and stays online
    // until host 0's Attach says A is not there; the claim outlives the presence.
    assert_eq!((c2.online_on(A), c2.online_on(B)), (Some(h(0)), Some(h(0))));
    c2.handle(h(0), HostMsg::Attach { host: h(0), epoch: 1, running: Vec::new(), online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 0);
    commit(&mut c2);
    assert_eq!((c2.online_on(A), c2.online_on(B)), (None, None), "offline once the attach says so");
    assert_eq!(c2.claim_of(A), Some(Claim::Adventure(site)));
}

/// Advancing the clock with no step due changes nothing durable: no transition. A day's turn
/// is a transition.
#[test]
fn advancing_with_nothing_due_writes_nothing() {
    let (hist, _, _) = world();
    let mut c = Coordinator::new(hist, 0);
    assert!(written(&mut c).is_some_and(|t| t.history.is_some()), "a new core writes itself first, history and all");
    assert!(written(&mut c).is_none(), "nothing changed, nothing waits");
    c.maintain(1_000);
    assert!(written(&mut c).is_none(), "the clock moved, nothing durable changed");
    c.maintain(game::history::calendar::DAY_MS);
    assert!(written(&mut c).is_some(), "a day turned");
    assert!(written(&mut c).is_none());
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

/// The host replays a start (it saw no answer yet) at a seeded pump while the link delays
/// answers: one claim is granted, and both answers are the same (executed twice, the second
/// would be refused `AlreadyPlaying`).
#[test]
fn replayed_start_under_delay_grants_once() {
    for seed in 0..SEEDS {
        let (c, _civ, _people) = coordinator();
        let site = site_of(&c);
        let mut link = InlineWorldLink::new(c).with_delay(seed, 3);
        let replay_at = 1 + seed % 4;
        let mut answers = Vec::new();
        for step in 0..12u64 {
            if step == 0 || step == replay_at {
                link.send(adventure(1, A, site));
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
        assert!(matches!(answers[0], WorldReply::AdventureStarted { site: s, .. } if s == site), "seed {seed}: {answers:?}");
        assert_eq!(link.coordinator().claim_of(A), Some(Claim::Adventure(site)), "seed {seed}: one claim granted");
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
    let (mut c, civ, _people) = coordinator();
    let r = rc(8, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    let p = npc_founding(&mut c, civ, r, 1)[0];
    c.maintain(0);
    let out = commit(&mut c);
    let spawn = directives_for(&out, r);
    assert_eq!(spawn.len(), 1, "{out:?}");
    assert_eq!(spawn[0].1, 1);
    let mut bodies = 0; // P's bodies on the host: the spawn has not been applied yet
    // The host parks `r` (no body of P in it) and starts it again before the spawn lands.
    c.handle(h(0), HostMsg::RegionStopping { rc: r }, 0);
    c.handle(h(0), HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    let out = commit(&mut c);
    bodies += body_delta(&out, p);
    assert_eq!(bodies, 1, "incarnation 2's catch-up spawns P: {out:?}");
    // The delayed incarnation-1 spawn is bounced by the host (wrong incarnation).
    c.handle(h(0), HostMsg::DirectiveBounced { directive: spawn[0].0.clone(), incarnation: 1 }, 0);
    // Stop and start again without a park (P's body stays).
    c.handle(h(0), HostMsg::RegionStopping { rc: r }, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 3 }, 0);
    c.maintain(0);
    let out = commit(&mut c);
    bodies += body_delta(&out, p);
    assert_eq!(bodies, 1, "P keeps exactly one body: {out:?}");
}

/// A region one host announced is announced by ANOTHER host (R4: the heir of a host
/// declared dead in the store may start a moved region before the coordinator has heard
/// the declaration's answer and parked the dead host's regions): a region runs on one host,
/// so the first host's incarnation is lost — the history takes back everyone it had there
/// and the new start embodies them again, once each. Without that they would stay embodied
/// in a region whose new host holds no body of theirs.
#[test]
fn a_region_started_on_another_host_takes_its_people_over() {
    let (mut c, _, people) = coordinator();
    let r = rc(0, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit(&mut c);
    for p in &people {
        assert_eq!(body_delta(&out, *p), 1, "{p:?} spawned on host 0: {out:?}");
    }
    c.handle(h(1), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit_routed(&mut c);
    let for_host_1: Vec<CoordMsg> = out.iter().filter(|(to, _)| *to == Target::Host(h(1))).map(|(_, m)| m.clone()).collect();
    for p in &people {
        assert_eq!(body_delta(&for_host_1, *p), 1, "{p:?} spawned again for host 1's start: {out:?}");
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r));
    }
    // Host 0's late park of its dead incarnation changes nothing (the region runs elsewhere).
    c.handle(h(0), HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    for p in &people {
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r), "{p:?} stays embodied on host 1");
    }
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
    c.handle(h(0), HostMsg::Attach { host: h(0), epoch: 1, running: vec![running(Vec::new())], online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 0);
    let out = commit(&mut c);
    let attached = out.iter().position(|m| matches!(m, CoordMsg::Attached { epoch: 1 })).expect("the echo");
    for p in &people {
        assert_eq!(body_delta(&out[attached..], *p), 1, "{p:?} spawned once: {out:?}");
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r));
    }
    // Re-attached with every body there: nothing more.
    let bodies: Vec<(PersonId, [f32; 3])> = people.iter().map(|p| (*p, [0.0; 3])).collect();
    c.handle(h(0), HostMsg::Attach { host: h(0), epoch: 2, running: vec![running(bodies)], online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 0);
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
    c.handle(h(0), HostMsg::RegionStarted { rc: a, incarnation: 1 }, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: b, incarnation: 1 }, 0);
    let out = commit(&mut c);
    let (p, q) = (people[0], people[1]);
    // Bodies on the host: everyone spawned into A once.
    let count = |p, out: &[CoordMsg], n: &mut i32| *n += body_delta(out, p);
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
    c.handle(h(0), HostMsg::Attach { host: h(0), epoch: 1, running, online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 0);
    let out = commit(&mut c);
    count(p, &out, &mut p_bodies);
    count(q, &out, &mut q_bodies);
    assert_eq!((p_bodies, q_bodies), (1, 0), "nobody spawned beside a crossing: {out:?}");
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(b));
    c.handle(h(0), HostMsg::PersonCrossed { person: p, to: b, delivered: true }, 0);
    c.handle(h(0), HostMsg::PersonCrossed { person: q, to: x, delivered: false }, 0);
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
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    // A site founded into `r` (P's spawn there) as A's start is granted.
    let p = npc_founding(&mut c, civ, r, 1)[0];
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    // A later message waits behind the spawn (no transition taken yet).
    c.handle(h(0), request(2, B, WorldRequest::PersonInfo(people[3])), 0);
    assert!(c.take_out().is_empty());
    // The link dropped; the host re-attaches running `r` with no body of P.
    let running = RunningRegion { rc: r, incarnation: 1, people: Vec::new() };
    let online = vec![OnlinePlayer { player: A, claim: None, claim_pending: true, report: 1, hop: 1 }];
    c.handle(h(0), HostMsg::Attach { host: h(0), epoch: 1, running: vec![running], online, generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 0);
    let out = commit(&mut c);
    let attached = out.iter().position(|m| matches!(m, CoordMsg::Attached { epoch: 1 })).expect("the echo");
    assert!(directives_for(&out[..attached], r).is_empty(), "the stale spawn is dropped: {out:?}");
    assert!(matches!(reply_to(&out, 1), WorldReply::AdventureStarted { .. }), "the reply still leaves");
    assert!(matches!(reply_to(&out, 2), WorldReply::PersonInfo(Some(_))));
    assert_eq!(body_delta(&out[attached..], p), 1, "one spawn of P, decided by the attach: {out:?}");
    assert_eq!(directives_for(&out[attached..], r).iter().map(|d| d.1).collect::<Vec<_>>(), vec![1]);
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(r), "P is embodied in r");
    // `r` parks with P's body and starts again: P is spawned once by its catch-up.
    c.handle(h(0), HostMsg::RegionStopping { rc: r }, 0);
    c.handle(h(0), HostMsg::RegionParked { rc: r, people: vec![(p, [0.0; 3])] }, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
    assert_eq!(body_delta(&commit(&mut c), p), 1);
}

/// A body that crossed between two running regions while the link was down is listed
/// in its new region only: whichever region is reattached first, it keeps that body.
#[test]
fn reattach_keeps_a_body_that_crossed_between_listed_regions() {
    for order in [false, true] {
        let (mut c, _civ, people) = coordinator();
        let (r0, r1) = (rc(0, 0), rc(1, 0));
        c.handle(h(0), HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
        c.handle(h(0), HostMsg::RegionStarted { rc: r1, incarnation: 1 }, 0);
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
        c.handle(h(0), HostMsg::Attach { host: h(0), epoch: 1, running, online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 0);
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
    c.handle(h(0), HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
    commit(&mut c);
    let p = people[0];
    c.handle(h(0), HostMsg::PersonCrossed { person: p, to: r1, delivered: true }, 0);
    let out = commit(&mut c);
    assert_eq!(body_delta(&out, p), 0, "{out:?}");
    let pr = c.history().person(p).unwrap();
    assert_eq!(pr.location, Location::InRegion(r1));
    // Parking `r1` hands P back to the abstract history.
    c.handle(h(0), HostMsg::RegionStopping { rc: r1 }, 0);
    c.handle(h(0), HostMsg::RegionParked { rc: r1, people: vec![(p, [0.0; 3])] }, 0);
    assert_ne!(c.history().person(p).unwrap().location, Location::InRegion(r1));
}

/// The same bounce twice (a resent message) before the region starts again: the spawn is
/// redelivered once.
#[test]
fn duplicated_bounce_is_redelivered_once() {
    let (mut c, civ, _people) = coordinator();
    let r = rc(8, 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    let p = npc_founding(&mut c, civ, r, 1)[0];
    c.maintain(0);
    let spawn = directives_for(&commit(&mut c), r);
    assert_eq!(spawn.len(), 1);
    c.handle(h(0), HostMsg::RegionStopping { rc: r }, 0);
    for _ in 0..2 {
        c.handle(h(0), HostMsg::DirectiveBounced { directive: spawn[0].0.clone(), incarnation: 1 }, 0);
    }
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 2 }, 0);
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
/// coordinator lacks is taken (a grant lost to a crash), and one that differs replaces the
/// coordinator's. With a request unsettled the coordinator's claim stands.
#[test]
fn host_report_reconciles_claims() {
    let (mut c, s1, s2) = coordinator_two_sites();
    c.handle(h(0), adventure(1, A, s1), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    // A request of A unsettled on the host: the claim stands.
    c.handle(h(0), online(A, None, true), 0);
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(s1)));
    // Settled, and the host holds nothing: the claim goes (and is saved at once).
    commit(&mut c);
    c.handle(h(0), online(A, None, false), 0);
    assert_eq!(c.claim_of(A), None);
    assert!(written(&mut c).is_some_and(|t| t.out.is_empty()), "a dropped claim is a transition of its own");
    // The host holds the adventure the coordinator lost: taken back, nothing to correct.
    c.handle(h(0), online(A, Some(Claim::Adventure(s1)), false), 0);
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(s1)));
    assert!(!commit(&mut c).iter().any(|m| matches!(m, CoordMsg::ClaimIs { .. })));
    // A's host reports an adventure at another site: the report replaces the claim.
    c.handle(h(0), online(A, Some(Claim::Adventure(s2)), false), 0);
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(s2)));
    assert!(!commit(&mut c).iter().any(|m| matches!(m, CoordMsg::ClaimIs { .. })), "the host's report is taken as is");
}

/// A release is a transition: the projection right after it holds no claim.
#[test]
fn release_claim_forces_a_save() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    commit(&mut c);
    assert!(c.take_transition().is_none());
    c.handle(h(0), HostMsg::ReleaseClaim { player: A }, 0);
    let t = c.take_transition().expect("a release is a transition");
    let loaded = reload(&t, &c.encode_projection().1, ReplicaId(1), 0);
    assert!(loaded.claims().is_empty(), "{:?}", loaded.claims());
}

/// A timed-out request is settled, never executed: unapplied, it is refused (and the
/// refusal memoised, so a late copy of the request grants nothing); applied, its memo
/// answers.
#[test]
fn settle_answers_without_executing() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    let req = start_request(site);
    c.handle(h(0), HostMsg::Settle { id: rid(1), req: req.clone() }, 0);
    assert_eq!(reply_to(&commit(&mut c), 1), WorldReply::AdventureRefused(AdventureRefusal::WorldUnavailable));
    c.handle(h(0), request(1, A, req.clone()), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), WorldReply::AdventureRefused(AdventureRefusal::WorldUnavailable));
    assert_eq!(c.claim_of(A), None, "nothing granted");
    // Applied before the settle: the memo answers.
    c.handle(h(0), request(2, A, req.clone()), 0);
    let first = reply_to(&commit(&mut c), 2);
    assert!(matches!(first, WorldReply::AdventureStarted { .. }));
    c.handle(h(0), HostMsg::Settle { id: rid(2), req }, 0);
    assert_eq!(reply_to(&commit(&mut c), 2), first);
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)), "granted once");
}

// ---- Many hosts (R3 Task 10) ----------------------------------------------------------

fn attach(host: HostId, epoch: u64, running: Vec<RunningRegion>, online: Vec<OnlinePlayer>) -> HostMsg {
    HostMsg::Attach { host, epoch, running, online, generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }
}

fn report(player: PlayerId, claim: Option<Claim>, claim_pending: bool) -> OnlinePlayer {
    OnlinePlayer { player, claim, claim_pending, report: 1, hop: 1 }
}

/// Host 0 runs site 0's region with its people and has A online on an adventure; host 1
/// then attaches (running another region, B online): host 0's regions keep their bodies
/// and A stays online with its claim, the echo and view go to host 1 only. A re-attach of
/// host 1 running nothing parks only host 1's region.
#[test]
fn attach_from_host_1_keeps_host_0_regions_and_players() {
    let (mut c, _civ, people) = coordinator();
    let (r0, r1) = (rc(0, 0), rc(14, 0));
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    let here: Vec<(PersonId, [f32; 3])> = people[2..].iter().map(|p| (*p, [0.0; 3])).collect();
    let running0 = vec![RunningRegion { rc: r0, incarnation: 1, people: here }];
    c.handle(h(0), attach(h(0), 1, running0, vec![report(A, Some(Claim::Adventure(site)), false)]), 0);
    commit(&mut c);
    let running1 = vec![RunningRegion { rc: r1, incarnation: 1, people: Vec::new() }];
    c.handle(h(1), attach(h(1), 1, running1, vec![report(B, None, false)]), 0);
    let out = commit_routed(&mut c);
    for (to, m) in &out {
        if matches!(m, CoordMsg::Attached { .. } | CoordMsg::View { .. }) {
            assert_eq!(*to, Target::Host(h(1)), "host 1's echo and view go to host 1: {m:?}");
        }
        if let CoordMsg::Directive { directive, .. } = m {
            assert_ne!(directive.region, r0, "host 0's region is not re-decided: {m:?}");
        }
    }
    for p in &people[2..] {
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r0), "{p:?} keeps its body");
    }
    assert_eq!((c.region_host(r0), c.region_host(r1)), (Some(h(0)), Some(h(1))));
    assert_eq!((c.online_on(A), c.online_on(B)), (Some(h(0)), Some(h(1))));
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)), "A is still online with its claim");
    // Host 1 re-attaches running nothing, B gone: only its region parks, only B goes offline.
    c.handle(h(1), attach(h(1), 2, Vec::new(), Vec::new()), 0);
    commit(&mut c);
    assert_eq!((c.region_host(r0), c.region_host(r1)), (Some(h(0)), None), "only host 1's region parked");
    for p in &people[2..] {
        assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r0), "{p:?} keeps its body");
    }
    assert_eq!((c.online_on(A), c.online_on(B)), (Some(h(0)), None));
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)));
}

/// A hand-off: A's `PlayerOnline` from the new host (1) arrives before its
/// `PlayerOffline` from the old one (0): the late offline is ignored — A stays online with
/// its claim; A going offline on host 1 takes it offline.
#[test]
fn player_offline_from_the_old_host_after_online_on_the_new_is_ignored() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    let claim = Some(Claim::Adventure(site));
    c.handle(h(0), adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    c.handle(h(0), online(A, claim, false), 0);
    assert_eq!(c.online_on(A), Some(h(0)));
    c.handle(h(1), online_at(A, claim, false, 2), 0);
    c.handle(h(0), HostMsg::PlayerOffline { player: A, hop: 1 }, 0);
    assert_eq!(c.online_on(A), Some(h(1)), "online on the new host");
    assert!(!commit(&mut c).iter().any(|m| matches!(m, CoordMsg::ClaimIs { .. })), "nothing to correct");
    assert_eq!(c.claim_of(A), claim);
    c.handle(h(1), HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
    assert_eq!(c.online_on(A), None);
    assert_eq!(c.claim_of(A), claim, "offline, the claim kept");
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
/// the coordinator ends with, to the reporting host only: (1) it holds an adventure at
/// another site and the report comes with a request unsettled (its own stands); (2) the
/// report holds nothing with a request unsettled whose grant the coordinator already made.
/// A report that agrees gets nothing. The host applies the answer as is (see
/// `world_manager::claim_is_sets_the_host_claim`).
#[test]
fn claim_is_answers_a_differing_report() {
    // (1) A holds an adventure at s1; its host reports s2 with a request unsettled: s1 stands.
    let (mut c, s1, s2) = coordinator_two_sites();
    c.handle(h(0), adventure(1, A, s1), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    c.handle(h(1), online(A, Some(Claim::Adventure(s2)), true), 0);
    let out = commit_routed(&mut c);
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(s1)));
    assert_eq!(claim_is(&out, A), vec![(Target::Host(h(1)), Some(Claim::Adventure(s1)))], "{out:?}");

    // (2) The start is granted; the host still reports nothing with it unsettled.
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    c.handle(h(0), online(A, None, true), 0);
    let out = commit_routed(&mut c);
    assert_eq!(claim_is(&out, A), vec![(Target::Host(h(0)), Some(Claim::Adventure(site)))], "{out:?}");
    // A report that agrees: nothing.
    c.handle(h(0), online(A, Some(Claim::Adventure(site)), false), 0);
    assert!(claim_is(&commit_routed(&mut c), A).is_empty());
}

/// A directive for a region no host has announced is never broadcast: it is held as a
/// bounce and delivered by the region's next start, to the host that starts it.
#[test]
fn directive_for_an_unannounced_region_is_deferred_not_broadcast() {
    let (mut c, civ, _people) = coordinator();
    let r = rc(8, 0);
    c.handle(h(1), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    // Host 1 parks `r` (gone from the coordinator's view) while the history founds a site
    // into it whose party would spawn there.
    c.handle(h(1), HostMsg::RegionStopping { rc: r }, 0);
    c.handle(h(1), HostMsg::RegionParked { rc: r, people: Vec::new() }, 0);
    assert_eq!(c.region_host(r), None);
    c.history_mut().region_started(r); // the history alone believes `r` runs
    let founders = npc_founding(&mut c, civ, r, 2);
    let party = &founders[..];
    c.maintain(0);
    let out = commit_routed(&mut c);
    assert!(
        !out.iter().any(|(_, m)| matches!(m, CoordMsg::Directive { directive, .. } if directive.region == r)),
        "no directive for an unannounced region leaves: {out:?}"
    );
    // Host 0 starts `r`: the held spawns come with its catch-up, to host 0.
    c.handle(h(0), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    let out = commit_routed(&mut c);
    let spawns: Vec<&(Target, CoordMsg)> =
        out.iter().filter(|(_, m)| matches!(m, CoordMsg::SpawnPeople { rc, .. } if *rc == r)).collect();
    assert_eq!(spawns.len(), 1, "{out:?}");
    assert_eq!(spawns[0].0, Target::Host(h(0)));
    let msgs: Vec<CoordMsg> = out.into_iter().map(|(_, m)| m).collect();
    for p in party {
        assert_eq!(body_delta(&msgs, *p), 1, "{p:?} spawns once, on host 0: {msgs:?}");
    }
}

/// A hands P (on an adventure at S) to B: P stays online — in transit to B, its claim
/// kept — before B says anything; B's `PlayerOnline` confirms (the claim P's, online on B);
/// B's `PlayerOffline` takes P offline.
#[test]
fn handed_player_stays_online_until_the_new_host_reports() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    c.handle(h(0), HostMsg::PlayerHandedTo { player: A, to: h(1), hop: 1 }, 0);
    assert_eq!((c.online_on(A), c.claim_of(A)), (Some(h(1)), Some(Claim::Adventure(site))), "in transit to host 1");
    commit(&mut c);
    c.handle(h(1), online_at(A, Some(Claim::Adventure(site)), false, 2), 0);
    assert!(claim_is(&commit_routed(&mut c), A).is_empty(), "nothing to correct");
    assert_eq!((c.online_on(A), c.claim_of(A)), (Some(h(1)), Some(Claim::Adventure(site))));
    c.handle(h(1), HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
    assert_eq!(c.online_on(A), None, "offline once host 1 says so");
}

/// A `PlayerHandedTo` at the player's latest hop from a host the player is not online
/// on (a stale or a stray message) changes nothing; nor does one for a player online
/// nowhere. Only the equal-hop stray: one at a higher hop is taken as the end of a
/// placement on its sender not heard yet (only a holder hands a player off).
#[test]
fn handed_to_from_a_host_that_does_not_have_the_player_is_ignored() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    c.handle(h(0), online(A, Some(Claim::Adventure(site)), false), 0);
    c.handle(h(2), HostMsg::PlayerHandedTo { player: A, to: h(3), hop: 1 }, 0);
    assert_eq!(c.online_on(A), Some(h(0)));
    c.handle(h(0), HostMsg::PlayerOffline { player: A, hop: 1 }, 0);
    assert_eq!(c.online_on(A), None);
    c.handle(h(0), HostMsg::PlayerHandedTo { player: A, to: h(1), hop: 1 }, 0);
    assert_eq!(c.online_on(A), None, "online nowhere: nothing to hand");
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)));
}

/// P in transit to host 1, which (re)attaches without it: the transit ends (offline). And
/// host 1 parking P on arrival (its `PlayerOffline`, at the hop after host 0's) heard
/// before host 0's `PlayerHandedTo` takes P offline at once; the hand-over, older, then
/// changes nothing.
#[test]
fn attach_of_the_new_host_without_the_player_ends_the_transit() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    let claim = Some(Claim::Adventure(site));
    c.handle(h(0), adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    c.handle(h(0), HostMsg::PlayerHandedTo { player: A, to: h(1), hop: 1 }, 0);
    c.handle(h(1), attach(h(1), 1, Vec::new(), Vec::new()), 0);
    assert_eq!((c.online_on(A), c.claim_of(A)), (None, claim), "offline, the claim kept");
    // A, online again on host 0 with the claim, is handed to host 1 — whose park on
    // arrival is heard first.
    c.handle(h(0), online_at(A, claim, false, 2), 0);
    assert_eq!((c.online_on(A), c.claim_of(A)), (Some(h(0)), claim));
    c.handle(h(1), HostMsg::PlayerOffline { player: A, hop: 3 }, 0);
    assert_eq!(c.online_on(A), None, "parked on arrival: offline");
    c.handle(h(0), HostMsg::PlayerHandedTo { player: A, to: h(1), hop: 2 }, 0);
    assert_eq!(c.online_on(A), None, "the older hand-over changes nothing");
    assert_eq!(c.hop_of(A), 3);
}

/// A region's messages go to the host that runs it, not the requester's: a site founded
/// by the history (an NPC founding) into a region host 1 runs, while host 0 asks a
/// request, sends the founders' spawns and the founding notice to host 1, the reply to
/// host 0; a notice for a region no host announced goes to every host.
#[test]
fn notice_and_directive_go_to_the_regions_host() {
    let (mut c, civ, _people) = coordinator();
    let r = rc(8, 0);
    let site = site_of(&c);
    c.handle(h(1), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 0);
    commit(&mut c);
    npc_founding(&mut c, civ, r, 2);
    c.handle(h(0), adventure(1, A, site), 0);
    c.maintain(0);
    let out = commit_routed(&mut c);
    let to_of = |want: fn(&CoordMsg) -> bool| -> Vec<Target> { out.iter().filter(|(_, m)| want(m)).map(|(t, _)| *t).collect() };
    let directives = to_of(|m| matches!(m, CoordMsg::Directive { .. }));
    assert!(!directives.is_empty() && directives.iter().all(|t| *t == Target::Host(h(1))), "{out:?}");
    assert_eq!(to_of(|m| matches!(m, CoordMsg::Reply { .. })), vec![Target::Host(h(0))]);
    let notices = to_of(|m| matches!(m, CoordMsg::Notice { .. }));
    assert!(!notices.is_empty() && notices.iter().all(|t| *t == Target::Host(h(1))), "{out:?}");
    // Into a region no host announced (the history alone believes it runs): its notice to
    // every host.
    c.history_mut().region_started(rc(14, 0));
    npc_founding(&mut c, civ, rc(14, 0), 2);
    c.maintain(0);
    let out = commit_routed(&mut c);
    let notices: Vec<Target> =
        out.iter().filter(|(_, m)| matches!(m, CoordMsg::Notice { region, .. } if *region == rc(14, 0))).map(|(t, _)| *t).collect();
    assert_eq!(notices, vec![Target::Every], "{out:?}");
}

// ---- The answer memo, pruned by acknowledgement (R3 Task 11) -------------------------

/// The memoised request ids, oldest first.
fn memo_ids(c: &Coordinator) -> Vec<RequestId> {
    c.memo_ids()
}

fn request_as(id: RequestId, player: PlayerId, req: WorldRequest) -> HostMsg {
    HostMsg::Request { id, player, hop: PLACED, req }
}

/// More answers than the old count cap, none settled: every one is kept, so a replay of
/// the first still finds its `AdventureStarted` (executed again it would be refused
/// `AlreadyPlaying`) and grants nothing.
#[test]
fn memo_keeps_unsettled_answers_past_the_old_cap() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    let first = reply_to(&commit(&mut c), 1);
    assert!(matches!(first, WorldReply::AdventureStarted { .. }), "{first:?}");
    for seq in 2..=ANSWERED_MEMO_MAX as u64 + 1 {
        c.handle(h(0), adventure(seq, A, site), 0);
        c.take_out();
    }
    commit(&mut c);
    assert_eq!(memo_ids(&c).len(), ANSWERED_MEMO_MAX + 1, "nothing unsettled is forgotten");
    c.handle(h(0), adventure(1, A, site), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), first, "the first answer is replayed");
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)), "and grants nothing more");
}

/// `Settled { boot, below }` from a host removes exactly that host's entries of that boot
/// below the watermark; an `Attach`'s `settled_below` does the same.
#[test]
fn settled_watermark_prunes_exactly_below() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    for seq in 1..=4 {
        c.handle(h(0), adventure(seq, A, site), 0);
    }
    let other_host = RequestId { host: h(1), boot: 7, seq: 1 };
    c.handle(h(1), request_as(other_host, B, start_request(site)), 0);
    commit(&mut c);
    assert_eq!(memo_ids(&c).len(), 5);
    c.handle(h(0), HostMsg::Settled { boot: 7, below: 3 }, 0);
    assert_eq!(memo_ids(&c), vec![rid(3), rid(4), other_host], "seq 1 and 2 of host 0 go");
    // From host 1: its own entries only (none of boot 7 below 1 … then all of them).
    c.handle(h(1), HostMsg::Settled { boot: 7, below: 1 }, 0);
    assert_eq!(memo_ids(&c), vec![rid(3), rid(4), other_host]);
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), 0);
    assert_eq!(memo_ids(&c), vec![rid(3), rid(4), other_host], "an attach watermark of 0 prunes nothing");
    c.handle(h(0), HostMsg::Attach { host: h(0), epoch: 2, running: Vec::new(), online: Vec::new(), generation: 0, boot: 7, settled_below: 4, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 0);
    assert_eq!(memo_ids(&c), vec![rid(4), other_host], "the attach's watermark prunes below it");
    c.handle(h(1), HostMsg::Settled { boot: 7, below: 2 }, 0);
    assert_eq!(memo_ids(&c), vec![rid(4)]);
}

/// A host's `Attach` with a boot the coordinator has not seen: that host's old process
/// is gone, so its entries of every other boot go; other hosts' entries and the new
/// boot's stay (a re-attach of the same boot keeps them).
#[test]
fn new_boot_attach_drops_the_old_boots_memo() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), 0);
    for seq in 1..=2 {
        c.handle(h(0), adventure(seq, A, site), 0);
    }
    let other_host = RequestId { host: h(1), boot: 7, seq: 1 };
    c.handle(h(1), request_as(other_host, B, start_request(site)), 0);
    commit(&mut c);
    let boot = |seq| RequestId { host: h(0), boot: 9, seq };
    let attach_boot_9 = |epoch| HostMsg::Attach {
        host: h(0),
        epoch,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot: 9,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
        policy: game::CrashPolicy::Rollback,
    };
    c.handle(h(0), attach_boot_9(2), 0);
    assert_eq!(memo_ids(&c), vec![other_host], "boot 7 of host 0 is gone");
    c.handle(h(0), request_as(boot(0), B, start_request(site)), 0);
    commit(&mut c);
    c.handle(h(0), attach_boot_9(3), 0);
    assert_eq!(memo_ids(&c), vec![other_host, boot(0)], "a re-attach of the same boot keeps its answers");
}

// ---- Player hops order presence across hosts (R3 Task 11) ----------------------------

const HOST_A: game::HostId = game::HostId(0);
const HOST_B: game::HostId = game::HostId(1);

/// A coordinator where player A has started an adventure (at `site`, its region) and is
/// online on host A at hop 1.
fn a_online_on_host_a() -> (Coordinator, SiteId, RegionCoords) {
    let (mut c, site, region) = a_adventuring_on_host_a();
    c.handle(HOST_A, online_at(A, Some(Claim::Adventure(site)), false, 1), 0);
    commit(&mut c);
    (c, site, region)
}

/// Player A has started an adventure through host A (at `site`); no presence report yet.
fn a_adventuring_on_host_a() -> (Coordinator, SiteId, RegionCoords) {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(HOST_A, adventure(1, A, site), 0);
    let WorldReply::AdventureStarted { site, region } = reply_to(&commit(&mut c), 1) else { panic!("starts") };
    (c, site, region)
}

/// Whether A's claim is offline now: online on no host. A keeps its claim offline.
fn a_offline(c: &Coordinator, site: SiteId) -> bool {
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)), "A keeps its claim");
    c.online_on(A).is_none()
}

/// A → B → A, then a disconnect on A, as hosts A and B send it (in send order per host):
/// A's placement at hop 1 and its hand-over, its placement at hop 3 and its end; B's
/// placement at hop 2 and its hand-over back.
fn round_trip(site: SiteId, disconnect: bool) -> (Vec<HostMsg>, Vec<HostMsg>) {
    let claim = Some(Claim::Adventure(site));
    let mut a = vec![
        online_at(A, claim, false, 1),
        HostMsg::PlayerHandedTo { player: A, to: HOST_B, hop: 1 },
        online_at(A, claim, false, 3),
    ];
    if disconnect {
        a.push(HostMsg::PlayerOffline { player: A, hop: 3 });
    }
    let b = vec![online_at(A, claim, false, 2), HostMsg::PlayerHandedTo { player: A, to: HOST_A, hop: 2 }];
    (a, b)
}

/// The reviewer's order: B's link lags behind all of A's messages. B's placement and
/// hand-over back, older than A's last placement, change nothing: A ends offline, its
/// claim kept.
#[test]
fn round_trip_then_disconnect_with_a_lagging_link_ends_offline() {
    let (mut c, site, _) = a_adventuring_on_host_a();
    let (a, b) = round_trip(site, true);
    for m in a {
        c.handle(HOST_A, m, 0);
    }
    for m in b {
        c.handle(HOST_B, m, 0);
    }
    assert_eq!(c.online_on(A), None, "offline after the disconnect");
    assert_eq!(c.hop_of(A), 3);
    assert!(a_offline(&c, site), "the claim is offline");
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
            let (mut c, site, _) = a_adventuring_on_host_a();
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
            assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)), "disconnect {disconnect}, order {mask:b}");
            assert_eq!(a_offline(&c, site), disconnect, "disconnect {disconnect}, order {mask:b}");
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
            policy: game::CrashPolicy::Rollback,
        };
        c.handle(HOST_A, reattach, 0);
        assert_eq!(c.online_on(A), Some(HOST_B), "b_attaches {b_attaches}: in transit to B");
        assert!(!a_offline(&c, site), "b_attaches {b_attaches}: online while in transit");
        if b_attaches {
            c.handle(HOST_B, attach(HOST_B, 1, Vec::new(), Vec::new()), 0);
        } else {
            c.handle(HOST_B, online_at(A, Some(Claim::Adventure(site)), false, 2), 0);
            assert_eq!(c.online_on(A), Some(HOST_B), "online on B");
            c.handle(HOST_B, HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
        }
        assert_eq!(c.online_on(A), None, "b_attaches {b_attaches}");
        assert!(a_offline(&c, site), "b_attaches {b_attaches}: offline once B says so");
    }
}

/// B's offline of an older placement (hop 2), delayed on B's link, arrives while the
/// player is in transit to B again (A handed it at hop 3): it cannot end that transit.
#[test]
fn stale_offline_cannot_cancel_a_later_transit() {
    let (mut c, site, _) = a_online_on_host_a();
    let claim = Some(Claim::Adventure(site));
    c.handle(HOST_A, HostMsg::PlayerHandedTo { player: A, to: HOST_B, hop: 1 }, 0);
    c.handle(HOST_B, online_at(A, claim, false, 2), 0);
    // B's placement ends (its offline delayed); the player logs in on A again (hop 3) and
    // is handed to B once more.
    c.handle(HOST_A, online_at(A, claim, false, 3), 0);
    c.handle(HOST_A, HostMsg::PlayerHandedTo { player: A, to: HOST_B, hop: 3 }, 0);
    c.handle(HOST_B, HostMsg::PlayerOffline { player: A, hop: 2 }, 0);
    assert_eq!(c.online_on(A), Some(HOST_B), "still in transit to B");
    assert!(!a_offline(&c, site), "the claim stays online");
    c.handle(HOST_B, online_at(A, claim, false, 4), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(HOST_B), 4));
}

/// `ClaimIs` (and `HopIs`) answers for a host still waiting to leave — behind a gated
/// message, or free to leave — are dropped at that host's `Attach`, which reports
/// everyone afresh; answers for other hosts stay.
#[test]
fn claim_is_queued_for_a_host_is_dropped_at_its_attach() {
    let c_player = PlayerId(3);
    for gated in [true, false] {
        let (mut c, _civ, _people) = coordinator();
        let site = site_of(&c);
        c.handle(HOST_B, request_as(RequestId { host: HOST_B, boot: 7, seq: 1 }, B, start_request(site)), 0);
        commit(&mut c);
        assert_eq!(c.claim_of(B), Some(Claim::Adventure(site)));
        if gated {
            // A grant waits for its save: everything after it waits too.
            c.handle(h(2), request_as(RequestId { host: h(2), boot: 7, seq: 2 }, c_player, start_request(site)), 0);
        }
        // Host B reports B with nothing and its request unsettled, which the coordinator
        // granted: `ClaimIs(Some)` for B, and a stale report of C on host 2: `HopIs` for
        // host 2… after a higher hop.
        c.handle(HOST_B, online(B, None, true), 0);
        c.handle(h(2), online_at(PlayerId(4), None, false, 5), 0);
        c.handle(h(2), online_at(PlayerId(4), None, false, 4), 0);
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
    let claim = Some(Claim::Adventure(site));
    c.handle(HOST_A, online_at(A, claim, false, 5), 0);
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
        policy: game::CrashPolicy::Rollback,
    };
    c.handle(HOST_B, attach_gen(HOST_B, 1, 0, vec![report(A, claim, false)]), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (None, 5), "the same generation: hop 1 is stale");
    c.handle(HOST_B, attach_gen(HOST_B, 2, 1, vec![report(A, claim, false)]), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(HOST_B), 1), "a newer generation: hops start over");
    c.handle(HOST_A, attach_gen(HOST_A, 1, 1, Vec::new()), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(HOST_B), 1), "a second attach of it keeps them");
}

// ---- Fix round 1: a displaced host is told (R3 Task 11) ------------------------------

const HOST_G: game::HostId = game::HostId(2);
const HOST_H: game::HostId = game::HostId(3);

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
/// `grant`: P gave its adventure up at hop 3 (H's login holds nothing), and G's in-flight
/// placement at hop 4 asks to start one again — granted — between its `Online` and its
/// `Offline` (`g_flight` is then ignored).
fn login_race(choices: &[u8], g_flight: &[HostMsg], grant: bool) -> (Vec<u8>, Coordinator, SiteId) {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(HOST_G, adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    let claim = Some(Claim::Adventure(site));
    c.handle(HOST_G, online_at(A, claim, false, 3), 0);
    if grant {
        c.handle(HOST_G, HostMsg::ReleaseClaim { player: A }, 0);
    }
    c.handle(HOST_G, HostMsg::PlayerOffline { player: A, hop: 3 }, 0);
    commit_routed(&mut c);
    let claim = if grant { None } else { claim };
    let g_flight = if grant {
        vec![
            online_at(A, None, false, 4),
            adventure(2, A, site),
            HostMsg::PlayerOffline { player: A, hop: 4 },
        ]
    } else {
        g_flight.to_vec()
    };
    let mut g: std::collections::VecDeque<HostMsg> = g_flight.into_iter().collect();
    let mut h_hop = 1;
    let mut h_out = std::collections::VecDeque::from([online_at(A, claim, false, h_hop)]);
    let mut h_in: std::collections::VecDeque<CoordMsg> = Default::default();
    let route = |c: &mut Coordinator, h_in: &mut std::collections::VecDeque<CoordMsg>| {
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
                        h_out.push_back(online_at(A, claim, false, h_hop));
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
/// its claim kept. With G's in-flight placements at
/// hop 4, and at hops 5 and 6 (several in flight: H's first renumbering lands below them).
#[test]
fn login_racing_an_older_placements_messages_ends_online_on_the_login_host() {
    let placements = |hops: &[u64]| -> Vec<HostMsg> {
        hops.iter()
            .flat_map(|&hop| {
                // The claim does not matter to the order: report none.
                [online_at(A, None, false, hop), HostMsg::PlayerOffline { player: A, hop }]
            })
            .collect()
    };
    for g_flight in [placements(&[4]), placements(&[5, 6])] {
        let mut stack: Vec<Vec<u8>> = vec![Vec::new()];
        let mut runs = 0;
        while let Some(prefix) = stack.pop() {
            let (enabled, c, site) = login_race(&prefix, &g_flight, false);
            if enabled.is_empty() {
                runs += 1;
                assert_eq!(c.online_on(A), Some(HOST_H), "order {prefix:?}: online on H");
                assert!(!a_offline(&c, site), "order {prefix:?}: the claim stays online");
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
    // G's in-flight placement is granted a claim (a new adventure) between its `Online` and
    // `Offline`. The grant moves P onto G: H is told, re-asserts, and its report (no claim,
    // nothing unsettled) is the truth — the grant is given back. Every order ends with P
    // online on H holding nothing.
    let mut stack: Vec<Vec<u8>> = vec![Vec::new()];
    let mut runs = 0;
    while let Some(prefix) = stack.pop() {
        let (enabled, c, _site) = login_race(&prefix, &[], true);
        if enabled.is_empty() {
            runs += 1;
            assert_eq!(c.online_on(A), Some(HOST_H), "grant, order {prefix:?}: online on H");
            assert_eq!(c.claim_of(A), None, "grant, order {prefix:?}: the grant is given back");
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
    let claim = Some(Claim::Adventure(site));
    c.handle(HOST_B, online_at(A, claim, false, 1), 0);
    assert_eq!(c.online_on(A), Some(HOST_A));
    let out = commit_routed(&mut c);
    assert!(
        out.iter().any(|(t, m)| *t == Target::Host(HOST_B) && matches!(m, CoordMsg::HopIs { player: A, reported: 1, hop: 1 })),
        "{out:?}"
    );
    c.handle(HOST_A, online_at(A, claim, false, 1), 0);
    assert_eq!(c.online_on(A), Some(HOST_A));
    assert!(!commit_routed(&mut c).iter().any(|(_, m)| matches!(m, CoordMsg::HopIs { .. })), "a re-report is fine");
}

/// Two hosts that both hold P (a duplicate body) and answer every `HopIs` of their
/// placement by renumbering and reporting again: each displaces the other, but a host
/// renumbers one player at most `HOP_RENUMBER_MAX_PER_PLAYER` times — the exchange
/// stops, with one error logged, and P ends online on one of them.
#[test]
fn two_holders_do_not_ping_pong_for_ever() {
    let (mut c, site, _) = a_adventuring_on_host_a();
    commit_routed(&mut c);
    let claim = Some(Claim::Adventure(site));
    let hosts = [HOST_G, HOST_H];
    let mut hop = [1u64, 1];
    let mut budget = [RenumberBudget::default(), RenumberBudget::default()];
    let mut inbox: std::collections::VecDeque<(usize, HostMsg)> =
        hosts.iter().enumerate().map(|(i, _)| (i, online_at(A, claim, false, 1))).collect();
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
                inbox.push_back((j, online_at(A, claim, false, hop[j])));
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
    c.set_expected_hosts([0, 1].into_iter().map(h).collect());
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_world_tick(100);
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), 0);
    c.handle(h(1), attach(h(1), 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    c
}

/// The `BeginCut` each host got: `(host, cut, tick)`.
fn begun(out: &[(Target, CoordMsg)]) -> Vec<(HostId, game::CutId, u64)> {
    out.iter()
        .filter_map(|(to, m)| match (to, m) {
            (Target::Host(h), CoordMsg::BeginCut { cut, tick, .. }) => Some((*h, *cut, *tick)),
            _ => None,
        })
        .collect()
}

/// The `CutDone` each host got: `(host, cut, committed)`.
fn done(out: &[(Target, CoordMsg)]) -> Vec<(HostId, game::CutId, bool)> {
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
    assert_eq!(begun(&out), [(h(0), cut, 100 + game::WORLD_CUT_LEAD_TICKS), (h(1), cut, 100 + game::WORLD_CUT_LEAD_TICKS)]);
    c.maintain(3 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "one cut at a time");
    c.handle(h(0), HostMsg::CutPrepared { cut }, 2 * CUT_EVERY_MS);
    assert_eq!(c.take_decision(), None, "host 1's part is missing");
    c.handle(h(1), HostMsg::CutPrepared { cut }, 2 * CUT_EVERY_MS);
    let d = c.take_decision().expect("decided");
    assert_eq!((d.cut, d.commit, d.hosts.clone()), (cut, true, vec![h(0), h(1)]));
    assert_eq!(c.take_decision(), None, "handed out once");
    c.decided(cut, true);
    assert_eq!(done(&commit_routed(&mut c)), [(h(0), cut, true), (h(1), cut, true)]);
    assert_eq!(c.world_cut_running(), None);
}

/// Criterion F at the coordinator: no new cut while one is undecided — also once its
/// decision was handed to the runtime and not yet recorded.
#[test]
fn no_second_world_cut_while_one_is_undecided() {
    let mut c = cutting();
    c.maintain(CUT_EVERY_MS);
    let (_, cut, _) = begun(&commit_routed(&mut c))[0];
    c.handle(h(0), HostMsg::CutFailed { cut, why: CutFailure::Aborted }, CUT_EVERY_MS);
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
    c.handle(h(0), HostMsg::CutPrepared { cut }, CUT_EVERY_MS);
    c.host_detached(h(1));
    let d = c.take_decision().expect("the detach decides");
    assert!(!d.commit, "a detached participant aborts the cut");
    c.decided(cut, false);
    assert_eq!(done(&commit_routed(&mut c)), [(h(0), cut, false), (h(1), cut, false)]);
    // Host 1 never read that: it re-attaches naming the cut.
    let reattach = HostMsg::Attach {
        host: h(1),
        epoch: 2,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: Some(cut),
        successions: Vec::new(),
        policy: game::CrashPolicy::Rollback,
    };
    c.handle(h(1), reattach, CUT_EVERY_MS);
    assert_eq!(done(&commit_routed(&mut c)), [(h(1), cut, false)], "answered from the outcome remembered");
    // A cut of an earlier coordinator run: the runtime looks it up.
    let old = game::CutId((2 << 32) | 9);
    c.handle(h(0), HostMsg::CutPrepared { cut: old }, CUT_EVERY_MS);
    assert!(done(&commit_routed(&mut c)).is_empty());
    assert_eq!(c.take_cut_lookups(), [(h(0), old)]);
    c.cut_outcome(h(0), old, true);
    assert_eq!(done(&commit_routed(&mut c)), [(h(0), old, true)]);
    // A participant re-attaching mid-cut aborts it (its link dropped: marks may be lost).
    c.maintain(5 * CUT_EVERY_MS);
    let (_, next, _) = begun(&commit_routed(&mut c))[0];
    c.handle(h(1), attach(h(1), 3, Vec::new(), Vec::new()), 5 * CUT_EVERY_MS);
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
        c.handle(h(1), HostMsg::CutFailed { cut, why: CutFailure::Missed }, now);
        c.take_decision().expect("aborted");
        c.decided(cut, false);
        commit_routed(&mut c);
    }
    assert_eq!(leads, [40, 80, 160, 320, 640, 640]);
    now += CUT_EVERY_MS;
    c.maintain(now);
    let (_, cut, _) = begun(&commit_routed(&mut c))[0];
    c.handle(h(0), HostMsg::CutPrepared { cut }, now);
    c.handle(h(1), HostMsg::CutPrepared { cut }, now);
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
    c.handle(h(2), attach(h(2), 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    c.maintain(CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "host 2 is attached and not expected");
    c.set_expected_hosts([0, 1, 2].into_iter().map(h).collect());
    c.expected_hosts_unknown();
    c.maintain(2 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "the hosts could not be read");
    c.set_expected_hosts([0, 1, 2].into_iter().map(h).collect());
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
    c.handle(h(1), HostMsg::LeaveRequest, 100);
    c.maintain(100 + game::LEAVE_BATCH_MS - 1);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "collected for the batch");
    c.maintain(100 + game::LEAVE_BATCH_MS);
    let out = commit_routed(&mut c);
    let begins: Vec<(Vec<HostId>, Vec<HostId>)> = out
        .iter()
        .filter_map(|(_, m)| match m {
            CoordMsg::BeginCut { hosts, leaving, .. } => Some((hosts.clone(), leaving.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(begins, vec![(vec![h(0), h(1)], vec![h(1)]); 2], "both hosts, host 1 leaving, before the period");
    let cut = begun(&out)[0].1;
    c.handle(h(0), HostMsg::CutPrepared { cut }, 700);
    c.handle(h(1), HostMsg::CutPrepared { cut }, 700);
    let d = c.take_decision().expect("decided");
    assert!(d.commit && d.hosts == vec![h(0), h(1)] && d.leaving == vec![h(1)], "{d:?}");
    c.decided(cut, true);
    assert_eq!(done(&commit_routed(&mut c)).len(), 2, "one CutDone per host");
    assert_eq!(c.left_hosts(), vec![h(1)]);
    // Its stale row (same boot) does not bring it back.
    c.set_host_boots([(h(0), 7), (h(1), 7)].into_iter().collect());
    c.maintain(700 + CUT_EVERY_MS);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![h(0)], "host 0 alone");
    let cut = next[0].1;
    c.handle(h(0), HostMsg::CutPrepared { cut }, 1800);
    let d = c.take_decision().expect("decided");
    assert!(d.commit && d.hosts == vec![h(0)] && d.leaving.is_empty(), "{d:?}");
    c.decided(cut, true);
    commit_routed(&mut c);
    // Another boot of it attaches: expected again.
    c.handle(h(1), HostMsg::Attach { host: h(1), epoch: 2, running: Vec::new(), online: Vec::new(), generation: 0, boot: 8, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Rollback }, 1900);
    assert!(c.left_hosts().is_empty());
    commit_routed(&mut c);
    c.maintain(1900 + CUT_EVERY_MS);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 2, "both take part again");
}

/// Elastic 1 Task 4 (was Final review I1, which aborted): a host attaching while a cut it is
/// not in runs — new, registered after the cut began — leaves that cut alone (the
/// participants freeze their sessions with it until the decision; the store lets the decision
/// leave out only a host registered after the begin was recorded); the cut commits without it
/// and the next cut takes it in. A participant re-attaching still aborts.
#[test]
fn attach_of_a_host_outside_the_running_cut_leaves_it_alone() {
    let mut c = cutting();
    // Host 1 leaves in a committed cut.
    c.handle(h(1), HostMsg::LeaveRequest, 100);
    c.maintain(100 + game::LEAVE_BATCH_MS);
    let cut = begun(&commit_routed(&mut c))[0].1;
    c.handle(h(0), HostMsg::CutPrepared { cut }, 700);
    c.handle(h(1), HostMsg::CutPrepared { cut }, 700);
    assert!(c.take_decision().expect("decided").commit);
    c.decided(cut, true);
    commit_routed(&mut c);
    // A cut of host 0 alone begins; a new host (id 2) attaches before it is decided.
    c.maintain(700 + CUT_EVERY_MS);
    let alone = begun(&commit_routed(&mut c));
    assert_eq!(alone.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![h(0)]);
    let cut = alone[0].1;
    c.handle(h(2), attach(h(2), 1, Vec::new(), Vec::new()), 1800);
    commit_routed(&mut c);
    assert_eq!(c.take_decision(), None, "the attach decides nothing");
    c.handle(h(0), HostMsg::CutPrepared { cut }, 1800);
    let d = c.take_decision().expect("host 0's part decides");
    assert_eq!((d.cut, d.commit, d.hosts.clone()), (cut, true, vec![h(0)]), "the cut without the newcomer commits");
    c.decided(cut, true);
    commit_routed(&mut c);
    // The next cut takes the newcomer once its dirty row is read.
    c.set_expected_hosts([h(0), h(2)].into_iter().collect());
    c.maintain(1800 + CUT_EVERY_MS);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![h(0), h(2)], "both take part");
    // A participant re-attaching (its link dropped) still aborts.
    let cut = c.world_cut_running().expect("running").0;
    c.handle(h(2), attach(h(2), 1, Vec::new(), Vec::new()), 1900);
    assert_eq!(c.take_decision().map(|d| (d.cut, d.commit)), Some((cut, false)));
}

/// Final review I2: a due cut blocked by a dirty row with no attached host is reported
/// (after the grace, once per period, naming the missing host and saying the world is not
/// being saved); a cut begins as soon as the hosts match.
#[test]
fn blocked_cuts_are_reported_once_a_period_and_begin_when_the_hosts_match() {
    let mut c = cutting();
    c.set_expected_hosts([0, 1, 2].into_iter().map(h).collect());
    c.maintain(CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "host 2 is expected and not attached");
    assert_eq!(c.take_cuts_blocked(), None, "within the grace");
    let first = CUT_EVERY_MS + game::WORLD_CUT_BLOCKED_GRACE_MS;
    c.maintain(first);
    let blocked = c.take_cuts_blocked().expect("reported");
    assert_eq!(blocked, game::CutsBlocked { missing: vec![h(2)], unexpected: vec![], successions: vec![], awaiting_heir: vec![] });
    let text = blocked.to_string();
    assert!(text.contains("NOT being saved") && text.contains("[2]") && text.contains("worldctl"), "{text}");
    c.maintain(first + CUT_EVERY_MS - 1);
    assert_eq!(c.take_cuts_blocked(), None, "once per period");
    c.maintain(first + CUT_EVERY_MS);
    assert!(c.take_cuts_blocked().is_some(), "again a period later");
    assert!(begun(&commit_routed(&mut c)).is_empty());
    c.handle(h(2), attach(h(2), 1, Vec::new(), Vec::new()), first + CUT_EVERY_MS + 1);
    commit_routed(&mut c);
    c.maintain(first + CUT_EVERY_MS + 2);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 3, "at once, all three");
    assert_eq!(c.take_cuts_blocked(), None);
}

// ---- the lease watch (R4) ----------------------------------------------------------------

/// Host `ordinal`'s row as the coordinator reads it: dirty and live, last seen at `seen_ms`.
fn seen(ordinal: HostId, seen_ms: u64) -> HostSeen {
    HostSeen { ordinal, boot: 100 + ordinal.0, clean: false, state: game::HOST_LIVE, seen_ms, draining: false, landed: true }
}

/// A host is lapsed only once the coordinator itself read one `seen_ms` over a whole lease
/// of its own clock; a beat in between restarts the count; clean or dead rows never lapse.
#[test]
fn lease_watch_needs_a_full_lease_of_unchanged_reads() {
    let mut w = LeaseWatch::default();
    let mut clean = seen(h(2), 7);
    clean.clean = true;
    let mut dead = seen(h(3), 7);
    dead.state = game::HOST_DEAD;
    for t in (0..game::HOST_LEASE_MS).step_by(1_000) {
        w.observe(&[seen(h(1), 500), clean.clone(), dead.clone()], t, t);
        assert!(w.lapsed(t).is_empty(), "lapsed at {t} ms: {:?}", w.lapsed(t));
    }
    w.observe(&[seen(h(1), 500), clean.clone(), dead.clone()], game::HOST_LEASE_MS, game::HOST_LEASE_MS);
    assert_eq!(w.lapsed(game::HOST_LEASE_MS), vec![(h(1), 101)]);
    // It beat: the count starts again from this read.
    let t0 = game::HOST_LEASE_MS + 1_000;
    w.observe(&[seen(h(1), 2_000)], t0, t0);
    assert!(w.lapsed(t0).is_empty());
    w.observe(&[seen(h(1), 2_000)], t0 + game::HOST_LEASE_MS - 1, t0 + game::HOST_LEASE_MS - 1);
    assert!(w.lapsed(t0 + game::HOST_LEASE_MS - 1).is_empty());
    w.observe(&[seen(h(1), 2_000)], t0 + game::HOST_LEASE_MS, t0 + game::HOST_LEASE_MS);
    assert_eq!(w.lapsed(t0 + game::HOST_LEASE_MS), vec![(h(1), 101)]);
    // Time alone, without a read, lapses nobody new.
    let mut fresh = LeaseWatch::default();
    fresh.observe(&[seen(h(1), 2_000)], 0, 0);
    assert!(fresh.lapsed(10 * game::HOST_LEASE_MS).is_empty());
}

/// Reads that fail for longer than a lease (a database outage) declare nobody: the watch
/// forgets everything and needs a whole lease of good reads again.
#[test]
fn outage_longer_than_the_lease_declares_nobody() {
    let mut w = LeaseWatch::default();
    w.observe(&[seen(h(0), 500), seen(h(1), 500)], 0, 0);
    for _ in 0..20 {
        w.observe_failed();
    }
    let back = 20_000;
    w.observe(&[seen(h(0), 500), seen(h(1), 500)], back, back);
    assert!(w.lapsed(back).is_empty(), "{:?}", w.lapsed(back));
    w.observe(&[seen(h(0), 500), seen(h(1), 500)], back + game::HOST_LEASE_MS - 1, back + game::HOST_LEASE_MS - 1);
    assert!(w.lapsed(back + game::HOST_LEASE_MS - 1).is_empty());
    w.observe(&[seen(h(0), 500), seen(h(1), 500)], back + game::HOST_LEASE_MS, back + game::HOST_LEASE_MS);
    assert_eq!(w.lapsed(back + game::HOST_LEASE_MS), vec![(h(0), 100), (h(1), 101)]);
}

/// A row a booting host of the same ordinal vouched for (`seen_ms = 0`) is lapsed at once.
#[test]
fn a_vouched_row_is_lapsed_at_once() {
    let mut w = LeaseWatch::default();
    w.observe(&[seen(h(0), 500), seen(h(1), 0)], 42, 42);
    assert_eq!(w.lapsed(42), vec![(h(1), 101)]);
    // Unless it is clean (it left) or dead (already declared).
    let mut clean = seen(h(1), 0);
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
    w.observe(&[seen(h(1), 500)], 0, 0);
    w.observe(&[seen(h(1), 500)], 1_000, 12_000);
    assert!(w.lapsed(12_000).is_empty(), "{:?}", w.lapsed(12_000));
    // A read issued a whole lease after the first one was received still shows it: lapsed.
    w.observe(&[seen(h(1), 500)], game::HOST_LEASE_MS, game::HOST_LEASE_MS + 50);
    assert_eq!(w.lapsed(game::HOST_LEASE_MS + 50), vec![(h(1), 101)]);
    // A changed value counts from its first read's RECEIVE.
    w.observe(&[seen(h(1), 900)], 20_000, 21_000);
    w.observe(&[seen(h(1), 900)], 21_000 + game::HOST_LEASE_MS - 1, 40_000);
    assert!(w.lapsed(40_000).is_empty());
}

// ---- successions (R4 Task 8) ---------------------------------------------------------------

/// Hosts `0..n` attached and expected by world cuts every [`CUT_EVERY_MS`], the crash policy
/// `Succession`, every output drained.
fn succeeding(n: u32) -> Coordinator {
    let mut c = cutting();
    for n in 2..u64::from(n) {
        c.handle(h(n), attach(h(n), 1, Vec::new(), Vec::new()), 0);
    }
    c.set_expected_hosts((0..u64::from(n)).map(h).collect());
    c.set_crash_policy(game::CrashPolicy::Succession);
    commit_routed(&mut c);
    c
}

/// A read of hosts `0..n` at `now`: `dead` vouched dead by a boot of their ordinal
/// (`seen_ms = 0`: lapsed at once), every other one beating; `declared` rows are dead.
fn read_leases(c: &mut Coordinator, n: u32, dead: &[HostId], declared: &[HostId], now: u64) {
    let rows: Vec<HostSeen> = (0..u64::from(n)).map(h)
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
    let dead: Vec<HostId> = d.dead.iter().map(|(h, _)| *h).collect();
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
            moved: vec![((predecessor.0 as i32, 0), 2)],
            survivors: (0..u64::from(n)).map(h).filter(|h| !dead.contains(h)).collect(),
        })
        .collect()
}

/// The succession orders in `out`: `(host, id)`.
fn orders(out: &[(Target, CoordMsg)]) -> Vec<(HostId, game::SuccessionId)> {
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
fn declare_one(c: &mut Coordinator, n: u32, dead: HostId, now: u64) -> game::Succession {
    read_leases(c, n, &[dead], &[], now);
    let d = c.declare_wanted(now).expect("a lapsed host is declared");
    let list = store_declares(&d, n, 1, game::CutId(0));
    c.declared(game::Declared::Done(list.clone()), now);
    c.set_expected_hosts((0..u64::from(n)).map(h).filter(|o| *o != dead).collect());
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
    read_leases(&mut c, 4, &[h(2), h(3)], &[], 20);
    let d = c.declare_wanted(20).expect("two lapsed hosts");
    let live: std::collections::BTreeSet<HostId> = [0, 1].into_iter().map(h).collect();
    assert_eq!(d.dead, vec![(h(2), 102), (h(3), 103)], "both, in one batch");
    assert_eq!(d.heirs, vec![game::heir_of(h(2), &live).unwrap(), game::heir_of(h(3), &live).unwrap()]);
    assert_eq!(d.kind, game::SuccessionKind::Crash);
    assert_eq!(c.declare_wanted(21), None, "one declaration in flight at a time");
    // The store found them beating again: forgotten — not declared again before a NEWER
    // read of the rows (the watch's same observation would loop every runtime pass).
    c.declared(game::Declared::NotLapsed(vec![h(2), h(3)]), 22);
    assert_eq!(c.declare_wanted(23), None, "no second declaration without a fresh read");
    assert_eq!(c.declare_wanted(500), None, "time alone is no fresh read");
    read_leases(&mut c, 4, &[h(2), h(3)], &[], 1_000);
    assert_eq!(c.declare_wanted(1_000).map(|d| d.dead), Some(vec![(h(2), 102), (h(3), 103)]), "a fresh read: asked again");
    c.declared(game::Declared::Gone(vec![h(2)]), 1_001);
    assert_eq!(c.declare_wanted(1_002), None, "`Gone` waits for a fresh read too");
    read_leases(&mut c, 4, &[h(2), h(3)], &[], 2_000);
    assert!(c.declare_wanted(2_000).is_some(), "a vouched row is lapsed at once on the next read");
    c.declare_failed();
    // Under `Rollback` (`Static`) nothing is ever declared.
    let mut r = succeeding(4);
    r.set_crash_policy(game::CrashPolicy::Rollback);
    read_leases(&mut r, 4, &[h(2), h(3)], &[], 20);
    assert_eq!(r.declare_wanted(20), None, "Static never declares");
    // A total crash (no live host attached) is the boots' business.
    let mut t = succeeding(2);
    read_leases(&mut t, 2, &[h(0), h(1)], &[], 20);
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
    c.handle(h(0), HostMsg::CutPrepared { cut }, CUT_EVERY_MS);
    read_leases(&mut c, 3, &[h(2)], &[], CUT_EVERY_MS + 1);
    assert_eq!(c.declare_wanted(CUT_EVERY_MS + 1), None, "not while the cut runs");
    let d = c.take_decision().expect("the lapse decides the cut");
    assert_eq!((d.cut, d.commit), (cut, false), "aborted");
    assert_eq!(c.declare_wanted(CUT_EVERY_MS + 2), None, "not before the abort is recorded");
    c.decided(cut, false);
    commit_routed(&mut c);
    let d = c.declare_wanted(CUT_EVERY_MS + 3).expect("between cuts now");
    assert_eq!(d.dead, vec![(h(2), 102)]);
}

/// After a declaration no world cut begins — however long the period passed — until every
/// survivor applied and the heir adopted; the blocked report names the succession. Then the
/// next cut begins with the survivors only.
#[test]
fn no_cut_begins_until_every_survivor_applied_and_the_heir_adopted() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, h(2), 100);
    let out = commit_routed(&mut c);
    assert_eq!(orders(&out), vec![(h(0), s.id), (h(1), s.id)], "the order goes to every survivor");
    let mut now = 100 + CUT_EVERY_MS;
    c.maintain(now);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "nobody answered");
    now += game::WORLD_CUT_BLOCKED_GRACE_MS;
    c.maintain(now);
    let blocked = c.take_cuts_blocked().expect("reported after the grace");
    assert_eq!(blocked.successions, vec![s.id]);
    assert!(blocked.to_string().contains("successions"), "{blocked}");
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, now);
    c.handle(h(1), HostMsg::SuccessionApplied { id: s.id }, now);
    c.maintain(now + 1);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "every survivor applied; the heir has not adopted");
    // An adoption from a host that is not the heir counts for nothing.
    let other = h(1 - s.heir.0);
    c.handle(other, HostMsg::SuccessionAdopted { id: s.id }, now + 2);
    c.maintain(now + 3);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "only the heir adopts");
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, now + 4);
    c.maintain(now + 5);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![h(0), h(1)], "the survivors' cut begins");
}

/// The first cut after every answer names the succession in its decision; an abort keeps
/// it open (the next cut names it again), a commit settles it — every survivor told — and
/// the cut after that settles nothing.
#[test]
fn the_commit_after_a_succession_names_it() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, h(2), 100);
    for hn in [0, 1] {
        c.handle(h(hn), HostMsg::SuccessionApplied { id: s.id }, 100);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    commit_routed(&mut c);
    let mut now = 100 + CUT_EVERY_MS;
    let run_cut = |c: &mut Coordinator, now: u64, commit: bool| -> CutDecide {
        c.maintain(now);
        let (_, cut, _) = begun(&commit_routed(c))[0];
        if commit {
            c.handle(h(0), HostMsg::CutPrepared { cut }, now);
            c.handle(h(1), HostMsg::CutPrepared { cut }, now);
        } else {
            c.handle(h(1), HostMsg::CutFailed { cut, why: CutFailure::Aborted }, now);
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
    assert_eq!(done, vec![(Target::Host(h(0)), s.id, true), (Target::Host(h(1)), s.id, true)], "every survivor is told");
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
    let s = declare_one(&mut c, 3, h(2), 100);
    for hn in [0, 1] {
        c.handle(h(hn), HostMsg::SuccessionApplied { id: s.id }, 100);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    commit_routed(&mut c);
    // The predecessor's own row (dead now, then lapsed in the watch) is no second lapse.
    read_leases(&mut c, 3, &[h(2)], &[], 150);
    c.maintain(150);
    assert!(!c.rollback_wanted(), "the declared predecessor is not a second lapse");
    read_leases(&mut c, 3, &[h(1)], &[h(2)], 200);
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
    read_leases(&mut c, 3, &[h(2)], &[], 10);
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
    let s = declare_one(&mut c, 3, h(2), 100);
    c.handle(s.heir, HostMsg::SuccessionApplied { id: s.id }, 100);
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    let silent = h(1 - s.heir.0);
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS - 1);
    assert!(!c.rollback_wanted(), "within the deadline");
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS);
    assert!(c.rollback_wanted(), "host {silent} stayed silent past the deadline");
    // Everyone answered: the deadline does not matter (the settle cut may take its time).
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, h(2), 100);
    for hn in [0, 1] {
        c.handle(h(hn), HostMsg::SuccessionApplied { id: s.id }, 100);
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
    c.set_expected_hosts([0, 1].into_iter().map(h).collect());
    let live: std::collections::BTreeSet<HostId> = [0, 1].into_iter().map(h).collect();
    let heir = game::heir_of(h(2), &live).unwrap();
    let s = game::Succession {
        id: game::SuccessionId(7),
        predecessor: h(2),
        predecessor_boot: 102,
        heir,
        kind: game::SuccessionKind::Crash,
        base_cut: game::CutId(3),
        moved: vec![((2, 0), 2)],
        survivors: vec![h(0), h(1)],
    };
    let attach_listing = |host: HostId, epoch: u64, successions: Vec<(game::SuccessionId, bool)>| HostMsg::Attach {
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
        policy: game::CrashPolicy::Rollback,
    };
    let other = h(1 - heir.0);
    c.handle(other, attach_listing(other, 1, Vec::new()), 0);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "not read yet");
    c.handle(heir, attach_listing(heir, 1, vec![(s.id, true)]), 0);
    c.maintain(CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut before the open successions are read");
    c.successions_open(vec![s.clone()], CUT_EVERY_MS);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id), (h(1), s.id)], "re-sent to the survivors attached");
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
    c.handle(h(0), HostMsg::CutPrepared { cut }, 3 * CUT_EVERY_MS);
    c.handle(h(1), HostMsg::CutPrepared { cut }, 3 * CUT_EVERY_MS);
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
/// for an attach of it with nothing: a claim its player held there is an offline
/// claimant's, its directives and claim answers are dropped, and the regions and players
/// of the other hosts stand.
#[test]
fn host_succeeded_parks_its_regions_and_takes_its_players_offline() {
    let (mut c, _civ, people) = coordinator();
    c.set_cut_epoch(3);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([0, 1].into_iter().map(h).collect());
    let (r0, r1) = (rc(0, 0), rc(14, 0));
    let site = site_of(&c);
    c.handle(h(1), adventure(1, A, site), 0);
    assert!(matches!(reply_to(&commit(&mut c), 1), WorldReply::AdventureStarted { .. }));
    let here: Vec<(PersonId, [f32; 3])> = people[2..].iter().map(|p| (*p, [0.0; 3])).collect();
    c.handle(h(1), attach(h(1), 1, vec![RunningRegion { rc: r0, incarnation: 1, people: here }], vec![report(A, Some(Claim::Adventure(site)), false)]), 0);
    c.handle(h(0), attach(h(0), 1, vec![RunningRegion { rc: r1, incarnation: 1, people: Vec::new() }], vec![report(B, None, false)]), 0);
    commit(&mut c);
    assert_eq!((c.region_host(r0), c.online_on(A)), (Some(h(1)), Some(h(1))), "A plays on host 1");
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)));
    // Host 1 lapses and is succeeded by host 0.
    read_leases(&mut c, 2, &[h(1)], &[], 10);
    let d = c.declare_wanted(10).expect("declared");
    assert_eq!(d.heirs, vec![h(0)]);
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(0))), 10);
    commit(&mut c);
    assert_eq!(c.region_host(r0), None, "host 1's region is no longer its");
    for p in &people[2..] {
        assert_ne!(c.history().person(*p).unwrap().location, Location::InRegion(r0), "{p:?}'s body is back in the history");
    }
    assert_eq!(c.online_on(A), None, "A went offline with its host");
    assert_eq!((c.region_host(r1), c.online_on(B)), (Some(h(0)), Some(h(0))), "host 0's stand");
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)), "the succeeded host's player keeps its claim, offline");
}

/// A zombie of a succeeded boot (still linked to the coordinator, its process hung or cut
/// off) changes nothing: its presence, region and attach messages are dropped, a request
/// of it is refused `WorldUnavailable` (memoised); another boot of the ordinal is a new host.
#[test]
fn a_succeeded_boots_messages_are_dropped() {
    let (mut c, _civ, _people) = coordinator();
    c.set_cut_epoch(3);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([0, 1].into_iter().map(h).collect());
    let attach_as = |host: HostId, epoch: u64, boot: u64, online: Vec<OnlinePlayer>| HostMsg::Attach {
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
        policy: game::CrashPolicy::Rollback,
    };
    c.handle(h(0), attach_as(h(0), 1, 100, Vec::new()), 0);
    c.handle(h(1), attach_as(h(1), 1, 101, Vec::new()), 0);
    commit(&mut c);
    read_leases(&mut c, 2, &[h(1)], &[], 10);
    let d = c.declare_wanted(10).expect("declared");
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(0))), 10);
    commit(&mut c);
    let r = rc(14, 0);
    c.handle(h(1), HostMsg::RegionStarted { rc: r, incarnation: 1 }, 20);
    c.handle(h(1), online(A, None, false), 20);
    c.handle(h(1), attach_as(h(1), 2, 101, vec![report(B, None, false)]), 20);
    let id = RequestId { host: h(1), boot: 101, seq: 1 };
    let req = start_request(site_of(&c));
    c.handle(h(1), HostMsg::Request { id, player: A, hop: PLACED, req: req.clone() }, 20);
    let out = commit_routed(&mut c);
    assert_eq!(c.region_host(r), None, "the zombie's region start is dropped");
    assert_eq!((c.online_on(A), c.online_on(B)), (None, None), "its players are not online");
    assert_eq!(c.claim_of(A), None, "no claim granted to its player");
    let refused: Vec<&WorldReply> = out
        .iter()
        .filter_map(|(to, m)| match m {
            CoordMsg::Reply { id: i, reply } if *i == id => {
                assert_eq!(*to, Target::Host(h(1)));
                Some(reply)
            }
            _ => None,
        })
        .collect();
    assert_eq!(refused, vec![&WorldReply::AdventureRefused(AdventureRefusal::WorldUnavailable)], "{out:?}");
    // Memoised: a resend gets the same refusal, never a grant.
    c.handle(h(1), HostMsg::Request { id, player: A, hop: PLACED, req }, 21);
    assert!(replies(&commit(&mut c)).iter().all(|(_, r)| *r == WorldReply::AdventureRefused(AdventureRefusal::WorldUnavailable)));
    assert_eq!(c.claim_of(A), None);
    // Another boot of the ordinal is a new host.
    c.handle(h(1), attach_as(h(1), 3, 999, vec![report(B, None, false)]), 30);
    commit(&mut c);
    assert_eq!(c.online_on(B), Some(h(1)), "a new boot is applied");
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
        host: h(2),
        epoch: 9,
        running: Vec::new(),
        online: vec![report(A, None, false)],
        generation: 0,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
        policy: game::CrashPolicy::Rollback,
    };
    c.handle(h(2), stale, 50);
    let out = commit_routed(&mut c);
    assert!(out.contains(&(Target::Host(h(2)), CoordMsg::Generation(4))), "{out:?}");
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
    c.set_expected_hosts([0, 1].into_iter().map(h).collect());
    let (a, b) = (rc(0, 0), rc(1, 0));
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), 0);
    c.handle(h(1), attach(h(1), 1, Vec::new(), Vec::new()), 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: a, incarnation: 1 }, 0);
    c.handle(h(1), HostMsg::RegionStarted { rc: b, incarnation: 1 }, 0);
    let out = commit(&mut c);
    let p = people[0];
    assert_eq!(body_delta(&out, p), 1, "P's body in a: {out:?}");
    // P walks from host 0's `a` into host 1's `b`, which reports it delivered.
    c.handle(h(1), HostMsg::PersonCrossed { person: p, to: b, delivered: true }, 0);
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(b));
    // Host 1 dies and host 0 succeeds it: `b` parks in the history with nobody.
    read_leases(&mut c, 2, &[h(1)], &[], 10);
    let d = c.declare_wanted(10).expect("declared");
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(0))), 10);
    let mut spawned = body_delta(&commit(&mut c), p);
    assert!(!matches!(c.history().person(p).unwrap().location, Location::InRegion(_)), "P is abstract");
    let before = c.history().person(p).unwrap().location;
    // The heir reports the same crossing absorbed.
    c.handle(h(0), HostMsg::PersonCrossed { person: p, to: b, delivered: false }, 20);
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
fn departed(n: u32, leaver: HostId) -> (Coordinator, u64) {
    let mut c = succeeding(n);
    c.handle(leaver, HostMsg::LeaveRequest, 100);
    c.maintain(100 + game::LEAVE_BATCH_MS);
    let cut = begun(&commit_routed(&mut c))[0].1;
    let now = 100 + game::LEAVE_BATCH_MS + 1;
    for hn in 0..u64::from(n) {
        c.handle(h(hn), HostMsg::CutPrepared { cut }, now);
    }
    assert!(c.take_decision().expect("decided").commit);
    c.decided(cut, true);
    c.host_detached(leaver);
    commit_routed(&mut c);
    (c, now)
}

/// A read of hosts `0..n` at `now`: every host beating, `left` clean (its `hosts` row after
/// its leave cut), `back` (another boot of `left`) dirty again.
fn read_departed(c: &mut Coordinator, n: u32, left: HostId, back: Option<u64>, now: u64) {
    let rows: Vec<HostSeen> = (0..u64::from(n)).map(h)
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
fn land_asked(c: &mut Coordinator, owner: HostId, now: u64) {
    c.handle(h(0), HostMsg::LandWanted { sr: WANTED }, now);
    assert_eq!(c.take_land_lookups(), vec![WANTED], "the runtime looks the super-region up");
    c.land_wanted(owner, now);
}

/// Host 1 left cleanly; host 0 needs its land. Elastic 1: a cleanly departed host never
/// returns as itself (a restarted process is a new host), so nothing waits for it — the
/// runtime's lookup and the next `declare_wanted` at the SAME clock hand out ONE `Leave`
/// succession giving ALL host 1's land to the heir `heir_of` picks over the live hosts, of the
/// boot that left; its order goes to the survivors. Asking again declares nothing more.
/// Nothing under `Rollback` (`Static`).
#[test]
fn land_of_a_host_that_left_is_given_at_once() {
    let (mut c, t) = departed(3, h(1));
    read_departed(&mut c, 3, h(1), None, t);
    land_asked(&mut c, h(1), t);
    let d = c.declare_wanted(t).expect("its land is given away at once");
    let live: std::collections::BTreeSet<HostId> = [0, 2].into_iter().map(h).collect();
    assert_eq!(d.dead, vec![(h(1), 101)], "the boot that left");
    assert_eq!(d.heirs, vec![game::heir_of(h(1), &live).unwrap()]);
    assert_eq!(d.kind, game::SuccessionKind::Leave);
    assert_eq!(c.declare_wanted(t), None, "one declaration in flight");
    let list = store_declares(&d, 3, 1, game::CutId(5));
    c.declared(game::Declared::Done(list.clone()), t);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), list[0].id), (h(2), list[0].id)], "the order goes to the survivors");
    read_departed(&mut c, 3, h(1), None, t + 1);
    land_asked(&mut c, h(1), t + 1);
    assert_eq!(c.declare_wanted(t + 1), None, "declared once");
    // Under `Rollback` (`Static`) a request is ignored.
    let (mut r, t) = departed(3, h(1));
    r.set_crash_policy(game::CrashPolicy::Rollback);
    read_departed(&mut r, 3, h(1), None, t);
    r.handle(h(0), HostMsg::LandWanted { sr: WANTED }, t);
    assert!(r.take_land_lookups().is_empty(), "Static looks nothing up");
    r.land_wanted(h(1), t);
    assert_eq!(r.declare_wanted(t), None, "Static never declares");
}

/// A `Leave` declaration is only made between cuts and for a row the latest read shows clean
/// and live: a running cut holds it back (declared once that cut is decided), and a store
/// refusal (`Gone`: the row changed under it) waits for a fresh read, then asks again.
#[test]
fn a_leave_declaration_waits_for_the_cut_and_a_refusal_for_a_fresh_read() {
    let (mut c, t) = departed(3, h(1));
    read_departed(&mut c, 3, h(1), None, t);
    // A cut of hosts 0 and 2 runs.
    c.set_expected_hosts([0, 2].into_iter().map(h).collect());
    c.maintain(t + CUT_EVERY_MS);
    let cut = begun(&commit_routed(&mut c));
    assert_eq!(cut.len(), 2, "a cut of hosts 0 and 2 runs");
    land_asked(&mut c, h(1), t + CUT_EVERY_MS);
    assert_eq!(c.declare_wanted(t + CUT_EVERY_MS), None, "only between cuts");
    let now = t + CUT_EVERY_MS + 1;
    for hn in [0, 2] {
        c.handle(h(hn), HostMsg::CutPrepared { cut: cut[0].1 }, now);
    }
    assert!(c.take_decision().expect("decided").commit);
    c.decided(cut[0].1, true);
    commit_routed(&mut c);
    let d = c.declare_wanted(now).expect("declared once the cut is decided");
    assert_eq!((d.dead.clone(), d.kind), (vec![(h(1), 101)], game::SuccessionKind::Leave));
    // The store refuses it: nothing declared, nothing asked again before a newer read.
    c.declared(game::Declared::Gone(vec![h(1)]), now);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "nothing declared");
    assert_eq!(c.declare_wanted(now + 1), None, "no fresh read yet");
    read_departed(&mut c, 3, h(1), None, now + 2);
    assert!(c.declare_wanted(now + 2).is_some(), "asked again after a fresh read");
}

/// Fix round 1 (minor 4): an owners lookup answered just after its owner's declaration
/// records the request again; it is pruned (the owner is declared), never kept for ever.
#[test]
fn a_request_for_land_already_declared_is_pruned() {
    let (mut c, t) = departed(2, h(1));
    read_departed(&mut c, 2, h(1), None, t);
    land_asked(&mut c, h(1), t);
    let d = c.declare_wanted(t).expect("declared");
    c.declared(game::Declared::Done(store_declares(&d, 2, 1, game::CutId(5))), t);
    assert!(c.lands_wanted().is_empty(), "the declared owner's request is done");
    // A late lookup answer for the same owner: not recorded again.
    c.land_wanted(h(1), t + 1);
    assert!(c.lands_wanted().is_empty(), "pruned");
    read_departed(&mut c, 2, h(1), None, t + 2);
    assert_eq!(c.declare_wanted(t + 2), None, "declared once");
}

// ---- no host count (Elastic 1 Task 5) -----------------------------------------------------

/// A `claim` coordinator (world cuts, `Succession`) with ONE attached host, host 0; host 5
/// (an issued id: no count bounds it) has a dirty row and is not attached.
fn one_host_attached() -> Coordinator {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(3);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_world_tick(100);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    c
}

/// A `claim` coordinator declares successions whatever the number of hosts: one attached host,
/// another lapses → a crash succession with the attached host as heir.
#[test]
fn a_claim_coordinator_declares_successions_with_one_host() {
    let mut c = one_host_attached();
    c.set_expected_hosts([0, 5].into_iter().map(h).collect());
    let rows = [seen(h(0), 11), seen(h(5), 0)];
    c.observe_leases(&rows, 10, 10);
    let d = c.declare_wanted(10).expect("host 5 lapsed: declared");
    assert_eq!((d.dead, d.heirs, d.kind), (vec![(h(5), 105)], vec![h(0)], game::SuccessionKind::Crash));
}

/// Final review B-I2: hosts 0..3 attached, host 3 lapses; `marked` (the heir `heir_of` would
/// pick over the other three) is marked draining in the same read. The declaration.
fn declare_beside_a_draining_heir(alone: bool) -> (Declare, HostId) {
    let n = if alone { 2 } else { 4 };
    let mut c = succeeding(n);
    let dead = h(u64::from(n) - 1);
    let live: std::collections::BTreeSet<HostId> = (0..u64::from(n) - 1).map(h).collect();
    let marked = game::heir_of(dead, &live).unwrap();
    let rows: Vec<HostSeen> = (0..u64::from(n))
        .map(h)
        .map(|o| {
            let mut row = seen(o, if o == dead { 0 } else { 11 });
            row.draining = o == marked;
            row
        })
        .collect();
    c.observe_leases(&rows, 10, 10);
    (c.declare_wanted(10).expect("host lapsed: declared"), marked)
}

/// A draining host is not chosen as heir while another live attached host can be: it would
/// adopt a dead host's whole land minutes before it leaves (its players moved twice, a second
/// `Leave` succession). The heir is `heir_of` over the hosts that are not going.
#[test]
fn a_draining_host_is_not_heir_while_another_can_be() {
    let (d, marked) = declare_beside_a_draining_heir(false);
    let staying: std::collections::BTreeSet<HostId> = [0, 1, 2].into_iter().map(h).filter(|o| *o != marked).collect();
    assert_eq!(d.heirs, vec![game::heir_of(h(3), &staying).unwrap()], "not the draining host {marked}");
    assert_ne!(d.heirs, vec![marked]);
}

/// The only live attached host is draining: it IS the heir — no land is left without one.
#[test]
fn a_draining_host_is_heir_when_it_is_the_only_one() {
    let (d, marked) = declare_beside_a_draining_heir(true);
    assert_eq!(marked, h(0));
    assert_eq!((d.dead, d.heirs), (vec![(h(1), 101)], vec![h(0)]));
}

/// A host that asked to leave (SIGTERM: its leave request waits) is not chosen as heir while
/// another can be — it would have to adopt inside `SUCCESSION_DEADLINE_MS` while exiting; it
/// is when it is the only one.
#[test]
fn a_leaving_host_is_heir_only_when_it_is_the_only_one() {
    for n in [4u32, 2] {
        let mut c = succeeding(n);
        let dead = h(u64::from(n) - 1);
        let live: std::collections::BTreeSet<HostId> = (0..u64::from(n) - 1).map(h).collect();
        let leaving = game::heir_of(dead, &live).unwrap();
        c.handle(leaving, HostMsg::LeaveRequest, 5);
        read_leases(&mut c, n, &[dead], &[], 10);
        let d = c.declare_wanted(10).expect("host lapsed: declared");
        if n == 2 {
            assert_eq!(d.heirs, vec![leaving], "the only host left: it is the heir");
        } else {
            let staying: std::collections::BTreeSet<HostId> = live.iter().copied().filter(|o| *o != leaving).collect();
            assert_eq!(d.heirs, vec![game::heir_of(dead, &staying).unwrap()], "not the leaving host {leaving}");
        }
    }
}

/// The same for a `Leave` succession (a departed host's land wanted): the heir is not the
/// draining host while another can be.
#[test]
fn a_departed_hosts_land_does_not_go_to_a_draining_host() {
    let (mut c, t) = departed(4, h(1));
    let live: std::collections::BTreeSet<HostId> = [0, 2, 3].into_iter().map(h).collect();
    let marked = game::heir_of(h(1), &live).unwrap();
    let rows: Vec<HostSeen> = (0..4)
        .map(h)
        .map(|o| {
            let mut row = seen(o, t + 1);
            row.clean = o == h(1);
            row.draining = o == marked;
            row
        })
        .collect();
    c.observe_leases(&rows, t, t);
    land_asked(&mut c, h(1), t);
    let d = c.declare_wanted(t).expect("its land is given away");
    let staying: std::collections::BTreeSet<HostId> = live.iter().copied().filter(|o| *o != marked).collect();
    assert_eq!(d.kind, game::SuccessionKind::Leave);
    assert_eq!(d.heirs, vec![game::heir_of(h(1), &staying).unwrap()], "not the draining host {marked}");
}

/// Ruling 6's degenerate case: the ONLY host lapses and nobody else is attached — no heir
/// exists, so nothing is declared (no panic), and the blocked-cuts report names the host: the
/// next registration finds nobody alive and its total-crash rule takes the land.
#[test]
fn the_only_host_lapsing_declares_nothing_and_blocks_cuts() {
    let mut c = one_host_attached();
    c.set_expected_hosts([0].into_iter().map(h).collect());
    c.host_detached(h(0));
    commit_routed(&mut c);
    for now in [10, 10 + game::HOST_LEASE_MS, 10 + 3 * game::HOST_LEASE_MS] {
        c.observe_leases(&[seen(h(0), 0)], now, now);
        assert_eq!(c.declare_wanted(now), None, "no heir: nothing declared ({now} ms)");
    }
    assert_eq!(c.successions_open_count(), 0);
    let due = 10 + 3 * game::HOST_LEASE_MS + CUT_EVERY_MS;
    c.maintain(due);
    c.maintain(due + game::WORLD_CUT_BLOCKED_GRACE_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut without the host");
    let blocked = c.take_cuts_blocked().expect("reported");
    assert_eq!(blocked.missing, vec![h(0)]);
    let text = blocked.to_string();
    assert!(text.contains("NOT being saved") && text.contains("[0]"), "{text}");
    assert!(!text.contains("VOXEL_HOST_ORDINAL"), "no ordinal to configure: {text}");
    assert!(text.contains("nobody else alive"), "the total-crash rule is named: {text}");
}

/// The counts relayed between participants of a running world cut.
fn counts(out: &[(Target, CoordMsg)]) -> Vec<(HostId, game::CutId, HostId, u64, u64)> {
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
    c.handle(h(0), HostMsg::CutPeerCount { cut, peer: h(1), received_through: 7, boot: 101 }, now);
    c.handle(h(1), HostMsg::CutPeerCount { cut, peer: h(0), received_through: 0, boot: 0 }, now);
    assert_eq!(counts(&commit_routed(&mut c)), [(h(1), cut, h(0), 7, 101), (h(0), cut, h(1), 0, 0)], "relayed to the peer, as from the reporter");
    c.handle(h(0), HostMsg::CutPeerCount { cut, peer: h(0), received_through: 3, boot: 100 }, now);
    c.handle(h(0), HostMsg::CutPeerCount { cut, peer: h(5), received_through: 3, boot: 105 }, now);
    c.handle(h(5), HostMsg::CutPeerCount { cut, peer: h(0), received_through: 3, boot: 100 }, now);
    c.handle(h(0), HostMsg::CutPeerCount { cut: game::CutId(cut.0 + 1), peer: h(2), received_through: 3, boot: 102 }, now);
    assert!(counts(&commit_routed(&mut c)).is_empty(), "nothing relayed outside the cut's participants");
    // Host 2 lapses: the cut is decided aborted before anything is declared, and a count
    // sent after that is not relayed.
    read_leases(&mut c, 3, &[h(2)], &[], now + 1);
    assert_eq!(c.declare_wanted(now + 1), None, "not while the cut runs");
    assert_eq!(c.take_decision().map(|d| (d.cut, d.commit)), Some((cut, false)), "aborted first");
    c.handle(h(0), HostMsg::CutPeerCount { cut, peer: h(2), received_through: 4, boot: 102 }, now + 1);
    c.handle(h(2), HostMsg::CutPeerCount { cut, peer: h(0), received_through: 4, boot: 100 }, now + 1);
    assert!(counts(&commit_routed(&mut c)).is_empty(), "no count of a decided cut is relayed");
    c.decided(cut, false);
    commit_routed(&mut c);
    c.handle(h(1), HostMsg::CutPeerCount { cut, peer: h(0), received_through: 4, boot: 100 }, now + 2);
    assert!(counts(&commit_routed(&mut c)).is_empty(), "nor once it is over");
}

/// A `StartAdventure` request of `player` at `site` made at placement `hop`.
fn start_at(id: u64, player: PlayerId, hop: u64, site: SiteId) -> HostMsg {
    HostMsg::Request { id: rid(id), player, hop, req: start_request(site) }
}

/// A places P at hop 3, P moves to B at hop 4, and A's `StartAdventure`, queued before the
/// move, arrives late: nothing is granted and P's claim is as it was; the refusal is
/// memoised (the same id, resent after P has moved on again, gets the same answer).
#[test]
fn a_request_from_an_older_placement_is_refused_not_granted() {
    let (mut c, _civ, people) = coordinator();
    let site = site_of(&c);
    let unavailable = WorldReply::AdventureRefused(AdventureRefusal::WorldUnavailable);
    c.handle(h(0), online_at(A, None, false, 3), 0);
    c.handle(h(1), online_at(A, None, false, 4), 0);
    assert_eq!((c.online_on(A), c.hop_of(A)), (Some(h(1)), 4));
    c.handle(h(0), start_at(1, A, 3, site), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), unavailable);
    assert_eq!(c.claim_of(A), None, "the claim is unchanged");
    assert_eq!(c.online_on(A), Some(h(1)), "A stays on host 1");
    // At the same hop as the known one, from a host A is not on: refused as well.
    c.handle(h(0), start_at(2, A, 4, site), 0);
    assert_eq!(reply_to(&commit(&mut c), 2), unavailable);
    assert_eq!(c.claim_of(A), None);
    // A query of the old placement is no state change: answered.
    c.handle(h(0), request(3, A, WorldRequest::PersonInfo(people[0])), 0);
    assert!(matches!(reply_to(&commit(&mut c), 3), WorldReply::PersonInfo(Some(_))));
    // Memoised: A is offline again at a higher hop, and the first request's id still
    // gets the refusal, never a grant.
    c.handle(h(1), HostMsg::PlayerOffline { player: A, hop: 5 }, 0);
    c.handle(h(0), start_at(1, A, 3, site), 0);
    assert_eq!(reply_to(&commit(&mut c), 1), unavailable);
    assert_eq!(c.claim_of(A), None);
}

/// A request at the player's current hop, from the host it is online on, is granted.
#[test]
fn a_request_at_the_current_hop_is_granted() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), online_at(A, None, false, 3), 0);
    c.handle(h(0), start_at(1, A, 3, site), 0);
    let WorldReply::AdventureStarted { site: started, .. } = reply_to(&commit(&mut c), 1) else { panic!("granted") };
    assert_eq!(started, site);
    assert_eq!(c.claim_of(A), Some(Claim::Adventure(site)));
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
    c.set_expected_hosts([0].into_iter().map(h).collect());
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    c
}

/// The succession `worldctl retire-host` opens with no host alive: host 1 → heir 0, no
/// survivors.
fn offline_succession() -> game::Succession {
    game::Succession {
        id: game::SuccessionId(9),
        predecessor: h(1),
        predecessor_boot: 101,
        heir: h(0),
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
    let mut dead = seen(h(1), 50);
    dead.state = game::HOST_DEAD;
    c.observe_leases(&[seen(h(0), 60), dead.clone()], 60, 60);
    assert!(c.take_successions_reread(), "a dead row nobody here declared: the open successions are read again");
    assert!(!c.take_successions_reread(), "asked once");
    c.observe_leases(&[seen(h(0), 70), dead.clone()], 70, 70);
    assert!(!c.take_successions_reread(), "the same dead row asks once");
    c.maintain(2 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut until the open successions are read");
    c.successions_open(vec![s.clone()], 2 * CUT_EVERY_MS);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id)], "the order goes to the heir, no survivor");
    let mut now = 3 * CUT_EVERY_MS;
    c.maintain(now);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "the heir has not answered");
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, now);
    c.handle(h(0), HostMsg::SuccessionAdopted { id: s.id }, now);
    assert_eq!(c.succession_progress(), vec![(s.id, vec![h(0)], true)]);
    now += CUT_EVERY_MS;
    c.maintain(now);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![h(0)], "the heir's cut begins");
    let cut = next[0].1;
    c.handle(h(0), HostMsg::CutPrepared { cut }, now);
    let d = c.take_decision().expect("decided");
    assert_eq!((d.commit, d.settles.clone()), (true, vec![s.id]), "the cut settles it");
    c.decided(cut, true);
    assert_eq!(succession_done(&commit_routed(&mut c)), vec![(Target::Host(h(0)), s.id, true)], "the heir is told");
    assert!(c.succession_progress().is_empty());
    // A later read of the settled row (clean now) asks nothing.
    dead.clean = true;
    c.observe_leases(&[seen(h(0), 90), dead], 90, 90);
    assert!(!c.take_successions_reread());
    // Read before the heir attached: the order comes with its attach.
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.successions_unknown();
    c.successions_open(vec![s.clone()], 0);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "the heir is not attached");
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), 10);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id)], "sent at its attach");
    // An heir that answered from memory (its next attach) is not ordered again.
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, 20);
    let listing = HostMsg::Attach {
        host: h(0),
        epoch: 2,
        running: Vec::new(),
        online: Vec::new(),
        generation: 0,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: vec![(s.id, false)],
        policy: game::CrashPolicy::Rollback,
    };
    c.handle(h(0), listing, 30);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "applied already");
}

/// A world rollback the coordinator did not order (`worldctl rollback`): the store abandoned
/// the open succession behind its back. A host attaching at the newer generation makes it read
/// the open successions again; the run that is no longer open is dropped (abandoned, every
/// host told), and declarations go on as usual.
#[test]
fn a_rollback_elsewhere_drops_the_abandoned_runs() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, h(2), 100);
    commit_routed(&mut c);
    assert!(!c.take_successions_reread());
    let newer = HostMsg::Attach {
        host: h(0),
        epoch: 2,
        running: Vec::new(),
        online: Vec::new(),
        generation: 1,
        boot: 7,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut: None,
        successions: Vec::new(),
        policy: game::CrashPolicy::Rollback,
    };
    c.handle(h(0), newer, 200);
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
    let mut dead = seen(h(1), 50);
    dead.state = game::HOST_DEAD;
    // Hosts 0 (the heir's old boot) and 2 died with host 1; the coordinator lives on.
    c.observe_leases(&[seen(h(0), 40), dead.clone(), seen(h(2), 40)], 0, 0);
    c.successions_open(vec![s.clone()], 0);
    let long = 10 * game::SUCCESSION_DEADLINE_MS;
    for now in [game::HOST_LEASE_MS, long] {
        c.observe_leases(&[seen(h(0), 40), dead.clone(), seen(h(2), 40)], now, now);
        c.maintain(now);
        assert!(!c.rollback_wanted(), "at {now}: no heir booted yet, the dead were dead already");
    }
    assert_eq!(c.leases().lapsed(long), vec![(h(0), 100), (h(2), 102)], "both lapsed");
    // The heir boots (a new boot), attaches, beats; host 2 stays down.
    let heir_row = |seen_ms: u64| HostSeen { boot: 900, ..seen(h(0), seen_ms) };
    c.handle(h(0), attach_as(h(0), 1, 900), long);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id)]);
    c.observe_leases(&[heir_row(long), dead.clone(), seen(h(2), 40)], long, long);
    c.observe_leases(&[heir_row(long + 500), dead.clone(), seen(h(2), 40)], long + 500, long + 500);
    c.maintain(long + game::SUCCESSION_DEADLINE_MS - 1);
    assert!(!c.rollback_wanted(), "within the deadline from the heir's order");
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, long + 600);
    c.handle(h(0), HostMsg::SuccessionAdopted { id: s.id }, long + 600);
    c.maintain(long + 2 * game::SUCCESSION_DEADLINE_MS);
    assert!(!c.rollback_wanted(), "answered: the deadline no longer matters");
    // The heir's new boot, seen beating since the run opened, dies before the settle.
    let late = long + 2 * game::SUCCESSION_DEADLINE_MS + game::HOST_LEASE_MS;
    c.observe_leases(&[heir_row(long + 500), dead.clone(), seen(h(2), 40)], late, late);
    c.maintain(late);
    assert!(c.rollback_wanted(), "a death during the succession");
    // Unanswered past the deadline counted from the heir's order: a rollback too.
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.successions_open(vec![s.clone()], 0);
    c.handle(h(0), attach_as(h(0), 1, 7), 100);
    commit_routed(&mut c);
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS - 1);
    assert!(!c.rollback_wanted());
    c.maintain(100 + game::SUCCESSION_DEADLINE_MS);
    assert!(c.rollback_wanted(), "the heir never answered");
}

/// Host `host`'s `Attach` of boot `boot` (epoch `epoch`, nothing running or online).
fn attach_as(host: HostId, epoch: u64, boot: u64) -> HostMsg {
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
        policy: game::CrashPolicy::Rollback,
    }
}

// ---- R4 final fix wave: the coordinator ----------------------------------------------------------

/// Final review C-I3: a succession every recipient answered makes its settle cut due at once —
/// not a whole save period later (the window in which a second failure rolls the world back,
/// and for which the predecessor's next boot waits in `register_host`). An aborted settle cut
/// is tried again after `SETTLE_CUT_RETRY_MS`; once settled, cuts follow their period again.
#[test]
fn an_answered_succession_makes_its_settle_cut_due_at_once() {
    const PERIOD: u64 = 60_000;
    let retry = game::SETTLE_CUT_RETRY_MS;
    assert!(retry < PERIOD);
    let mut c = succeeding(3);
    c.set_world_cuts(PERIOD);
    let s = declare_one(&mut c, 3, h(2), 100);
    commit_routed(&mut c);
    let mut now = 100 + retry;
    c.maintain(now);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "not answered: no cut");
    for hn in [0, 1] {
        c.handle(h(hn), HostMsg::SuccessionApplied { id: s.id }, now);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, now);
    c.maintain(now);
    let first = begun(&commit_routed(&mut c));
    assert_eq!(first.len(), 2, "answered: the settle cut begins at once, {} ms into a {PERIOD} ms period", now);
    let cut = first[0].1;
    // It aborts (a pinned region not loaded yet): tried again after the retry wait, not at once.
    c.handle(h(1), HostMsg::CutFailed { cut, why: CutFailure::Aborted }, now);
    let d = c.take_decision().expect("decided");
    assert_eq!((d.commit, d.settles.clone()), (false, vec![s.id]));
    c.decided(cut, false);
    commit_routed(&mut c);
    c.maintain(now + retry - 1);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "an aborted settle cut is not tried again at once");
    now += retry;
    c.maintain(now);
    let again = begun(&commit_routed(&mut c));
    assert_eq!(again.len(), 2, "tried again after the retry wait");
    let cut = again[0].1;
    for hn in [0, 1] {
        c.handle(h(hn), HostMsg::CutPrepared { cut }, now);
    }
    let d = c.take_decision().expect("decided");
    assert_eq!((d.commit, d.settles.clone()), (true, vec![s.id]));
    c.decided(cut, true);
    commit_routed(&mut c);
    assert!(c.succession_progress().is_empty(), "settled");
    // Nothing open: the period rules again.
    c.maintain(now + 2 * retry);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no succession open: the next cut waits its period");
    c.maintain(now + PERIOD);
    assert_eq!(begun(&commit_routed(&mut c)).len(), 2);
}

/// Final review C-I4 (b): while only an offline run is open, the lapse of a boot that was
/// dead already (never seen beating since the run opened) is no second death — but a
/// VOUCHED row (`seen_ms == 0`: `worldctl retire-host` beside live hosts, or a boot of that
/// ordinal) is a statement made now: it counts, the world rolls back (the run abandoned, its
/// land staying with the heir), and the next pass can declare that host — else its dirty row
/// blocks every cut and the open run blocks its declaration, for ever.
#[test]
fn a_vouched_lapse_counts_while_only_an_offline_run_is_open() {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    let s = offline_succession();
    let mut dead = seen(h(1), 50);
    dead.state = game::HOST_DEAD;
    c.observe_leases(&[seen(h(0), 40), dead.clone(), seen(h(2), 40)], 0, 0);
    c.successions_open(vec![s.clone()], 0);
    let t = 10 * game::HOST_LEASE_MS;
    let heir_row = |seen_ms: u64| HostSeen { boot: 900, ..seen(h(0), seen_ms) };
    c.handle(h(0), attach_as(h(0), 1, 900), t);
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, t);
    c.handle(h(0), HostMsg::SuccessionAdopted { id: s.id }, t);
    commit_routed(&mut c);
    c.observe_leases(&[heir_row(t), dead.clone(), seen(h(2), 40)], t, t);
    c.maintain(t);
    assert!(!c.rollback_wanted(), "host 2 was dead already when the world was retired: no second death");
    // The operator vouches host 2 dead.
    c.observe_leases(&[heir_row(t + 500), dead.clone(), seen(h(2), 0)], t + 500, t + 500);
    c.maintain(t + 500);
    assert!(c.rollback_wanted(), "a vouched row counts for an offline run too");
}

/// Final review C-I4 (a): an offline run whose heir never started has no deadline — the
/// blocked-cuts report says so, names the heir and the way out (`worldctl rollback`), and no
/// longer promises a rollback "within the succession deadline".
#[test]
fn blocked_cuts_name_an_offline_runs_absent_heir() {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_world_tick(100);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([2].into_iter().map(h).collect());
    c.handle(h(2), attach(h(2), 1, Vec::new(), Vec::new()), 0);
    let s = offline_succession();
    c.successions_open(vec![s.clone()], 0);
    commit_routed(&mut c);
    c.maintain(CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "the run is not answered: no cut");
    c.maintain(CUT_EVERY_MS + game::WORLD_CUT_BLOCKED_GRACE_MS);
    let blocked = c.take_cuts_blocked().expect("reported");
    assert_eq!(
        blocked,
        game::CutsBlocked { missing: vec![], unexpected: vec![], successions: vec![s.id], awaiting_heir: vec![(s.id, s.heir)] }
    );
    let text = blocked.to_string();
    assert!(text.contains("heir, host 0") && text.contains("worldctl rollback"), "{text}");
    assert!(!text.contains("within the succession deadline"), "no deadline runs before the heir's first attach: {text}");
    // A declared run (survivors) still has its deadline, and says so.
    let declared = game::CutsBlocked { missing: vec![], unexpected: vec![], successions: vec![s.id], awaiting_heir: vec![] };
    assert!(declared.to_string().contains("within the succession deadline"));
}

/// Final review C minor 3: a rollback this coordinator ordered starts the hops over, as a
/// newer-generation `Attach` does — the hosts restart with the cut's hops, below the ones
/// known here (a state-changing request would be refused `WorldUnavailable` until renumbered).
#[test]
fn a_rollback_ordered_here_starts_the_hops_over() {
    let mut c = succeeding(3);
    let mut online = report(A, None, false);
    online.hop = 5;
    c.handle(h(0), attach(h(0), 2, Vec::new(), vec![online]), 10);
    commit_routed(&mut c);
    assert_eq!(c.hop_of(A), 5);
    c.rolled_back(7);
    assert_eq!(c.hop_of(A), 0, "hops start over at the rollback's generation");
}

/// Final review C minor 4 (ii): a hand-over to a host that was succeeded (its `PlayerHandedTo`
/// handled after the declaration) leaves the player offline — never "online on" a dead
/// host.
#[test]
fn a_player_handed_to_a_succeeded_host_is_offline() {
    let mut c = succeeding(3);
    c.handle(h(0), attach(h(0), 2, Vec::new(), vec![report(A, None, false)]), 10);
    commit_routed(&mut c);
    assert_eq!(c.online_on(A), Some(h(0)));
    let s = declare_one(&mut c, 3, h(2), 100);
    assert_eq!(s.predecessor, h(2));
    c.handle(h(0), HostMsg::PlayerHandedTo { player: A, to: h(2), hop: PLACED }, 110);
    assert_eq!(c.online_on(A), None, "handed to the dead host: offline, not online on it");
    assert_eq!(c.hop_of(A), PLACED);
}

/// Final review C minor 6: a declaration's answer that arrives when none is in flight any
/// more (a rollback cleared it: its transaction abandoned every open run) opens nothing.
#[test]
fn a_declaration_answered_after_a_rollback_opens_no_run() {
    let mut c = succeeding(3);
    read_leases(&mut c, 3, &[h(2)], &[], 100);
    let d = c.declare_wanted(100).expect("a lapsed host is declared");
    let list = store_declares(&d, 3, 1, game::CutId(0));
    c.rolled_back(5);
    commit_routed(&mut c);
    c.declared(game::Declared::Done(list), 120);
    assert!(c.succession_progress().is_empty(), "the rollback abandoned it in the store: not run here");
    assert!(orders(&commit_routed(&mut c)).is_empty(), "no order goes out");
}

/// Ledger T14: the heir of an offline run adopts, dies, and its next boot attaches long after
/// the first order's deadline: the answers are reset AND the deadline starts again with the
/// new boot's order — not a rollback at once.
#[test]
fn an_offline_runs_deadline_starts_again_with_a_rebooted_heirs_order() {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    let s = offline_succession();
    c.successions_open(vec![s.clone()], 0);
    c.handle(h(0), attach_as(h(0), 1, 7), 100);
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, 110);
    c.handle(h(0), HostMsg::SuccessionAdopted { id: s.id }, 110);
    commit_routed(&mut c);
    let later = 100 + 5 * game::SUCCESSION_DEADLINE_MS;
    c.handle(h(0), attach_as(h(0), 1, 8), later);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id)], "the order goes to the new boot");
    c.maintain(later + game::SUCCESSION_DEADLINE_MS - 1);
    assert!(!c.rollback_wanted(), "the new boot has a whole deadline from its order");
    c.maintain(later + game::SUCCESSION_DEADLINE_MS);
    assert!(c.rollback_wanted(), "and no longer");
}

// ---- R4 Task 14 fix round 1: answers are per boot ---------------------------------------------

/// The heir of an offline succession adopts, then its process dies and boots again before
/// the settle cut (a new boot, nothing listed in its `Attach`): the old boot's answers do not
/// count for the new one. The order goes to the new boot, and no cut begins — none names the
/// succession — until the NEW boot has applied and adopted it.
#[test]
fn an_heir_that_reboots_after_adopting_must_adopt_again_before_any_settle() {
    let mut c = heir_alone();
    let s = offline_succession();
    c.successions_unknown();
    c.successions_open(vec![s.clone()], 10);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id)]);
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, 20);
    c.handle(h(0), HostMsg::SuccessionAdopted { id: s.id }, 20);
    // The heir's process dies before any cut began and boots again.
    c.handle(h(0), attach_as(h(0), 2, 900), 30);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id)], "the order goes to the new boot");
    for now in [2 * CUT_EVERY_MS, 3 * CUT_EVERY_MS] {
        c.maintain(now);
        assert!(begun(&commit_routed(&mut c)).is_empty(), "at {now}: the new boot has not adopted");
        assert!(c.take_decision().is_none());
    }
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, 3 * CUT_EVERY_MS);
    c.maintain(4 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "applied, not adopted");
    c.handle(h(0), HostMsg::SuccessionAdopted { id: s.id }, 4 * CUT_EVERY_MS);
    c.maintain(5 * CUT_EVERY_MS);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.len(), 1, "the new boot adopted: the settle cut begins");
    let cut = next[0].1;
    c.handle(h(0), HostMsg::CutPrepared { cut }, 5 * CUT_EVERY_MS);
    assert_eq!(c.take_decision().expect("decided").settles, vec![s.id]);
    // An attach of the SAME boot listing it (a reconnect) keeps its answers: no order again.
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.handle(h(0), attach_as(h(0), 1, 900), 0);
    c.successions_open(vec![s.clone()], 0);
    commit_routed(&mut c);
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, 1);
    c.handle(h(0), HostMsg::SuccessionAdopted { id: s.id }, 1);
    let mut again = attach_as(h(0), 2, 900);
    if let HostMsg::Attach { successions, .. } = &mut again {
        *successions = vec![(s.id, true)];
    }
    c.handle(h(0), again, 2);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "the same boot answered already");
    assert_eq!(c.succession_progress(), vec![(s.id, vec![h(0)], true)]);
}

/// A survivor of a declared succession attaches as ANOTHER boot before the settle: its journal
/// is gone, so the run can never be answered — no order goes to the new boot (it would report
/// an empty journal) and the world rolls back.
#[test]
fn a_survivor_that_reboots_during_a_declared_run_rolls_the_world_back() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, h(2), 100);
    commit_routed(&mut c);
    let other = h(1 - s.heir.0);
    c.handle(other, HostMsg::SuccessionApplied { id: s.id }, 150);
    c.handle(other, attach_as(other, 2, 900), 200);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "no order to a boot that lost the journal");
    c.maintain(201);
    assert!(c.rollback_wanted(), "a survivor's journal is gone");
}

/// The coordinator's own declaration seen by a `hosts` read before its answer was handled (a
/// dead row it has not noted yet) asks for no re-read; and a re-read that lists a run already
/// open never re-opens it — its answers are kept, its order is not sent twice.
#[test]
fn a_declared_run_learned_twice_is_opened_once() {
    let mut c = succeeding(3);
    read_leases(&mut c, 3, &[h(2)], &[], 100);
    let d = c.declare_wanted(100).expect("declared");
    let list = store_declares(&d, 3, 1, game::CutId(0));
    let s = list[0].clone();
    read_leases(&mut c, 3, &[], &[h(2)], 110);
    assert!(!c.take_successions_reread(), "the declaration in flight explains the dead row");
    c.declared(game::Declared::Done(list.clone()), 113);
    assert_eq!(orders(&commit_routed(&mut c)).len(), 2, "one order per survivor");
    c.handle(h(0), HostMsg::SuccessionApplied { id: s.id }, 114);
    c.successions_reread();
    assert!(c.take_successions_reread());
    c.successions_open(list, 115);
    assert!(orders(&commit_routed(&mut c)).is_empty(), "not re-opened: no second order");
    assert_eq!(c.succession_progress(), vec![(s.id, vec![h(0)], false)], "host 0's answer kept");
}

/// A run a decision in flight settles is no "abandoned" run: a re-read that no longer lists it
/// (the commit already stored) does not announce it abandoned; the decision's answer settles it.
#[test]
fn a_run_settling_is_not_dropped_as_abandoned() {
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, h(2), 100);
    for hn in [0, 1] {
        c.handle(h(hn), HostMsg::SuccessionApplied { id: s.id }, 100);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    commit_routed(&mut c);
    let now = 100 + CUT_EVERY_MS;
    c.maintain(now);
    let (_, cut, _) = begun(&commit_routed(&mut c))[0];
    c.handle(h(0), HostMsg::CutPrepared { cut }, now);
    c.handle(h(1), HostMsg::CutPrepared { cut }, now);
    let d = c.take_decision().expect("decided");
    assert_eq!(d.settles, vec![s.id]);
    // The store committed it; before its answer is handled a re-read comes back without it.
    c.successions_reread();
    assert!(c.take_successions_reread());
    c.successions_open(Vec::new(), now + 1);
    assert!(succession_done(&commit_routed(&mut c)).is_empty(), "not announced abandoned");
    c.decided(cut, true);
    let done = succession_done(&commit_routed(&mut c));
    assert!(done.iter().all(|(_, id, settled)| *id == s.id && *settled) && !done.is_empty(), "{done:?}");
}

// ---- R4 Task 14 fix round 2 ------------------------------------------------------------------

/// A coordinator starting (open successions not read yet) buffers what an `Attach` lists. The
/// heir's boot A lists the run adopted, dies, and its boot B attaches listing nothing; then the
/// read comes: A's listing is not credited to B — B gets the order and no cut begins until B
/// itself has adopted.
#[test]
fn an_early_listing_of_a_replaced_boot_is_not_credited() {
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_world_tick(100);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts([0].into_iter().map(h).collect());
    c.successions_unknown();
    let s = offline_succession();
    let mut listing = attach_as(h(0), 1, 7);
    if let HostMsg::Attach { successions, .. } = &mut listing {
        *successions = vec![(s.id, true)];
    }
    c.handle(h(0), listing, 0);
    c.handle(h(0), attach_as(h(0), 2, 900), 10);
    commit_routed(&mut c);
    c.successions_open(vec![s.clone()], 20);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(0), s.id)], "the order goes to boot B");
    assert_eq!(c.succession_progress(), vec![(s.id, Vec::new(), false)], "boot A's listing is not B's answer");
    c.maintain(2 * CUT_EVERY_MS);
    assert!(begun(&commit_routed(&mut c)).is_empty(), "no cut before boot B adopted");
    // A listing of the boot still attached when the read comes counts.
    let (mut c, _, _) = coordinator();
    c.set_cut_epoch(4);
    c.set_world_cuts(CUT_EVERY_MS);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.successions_unknown();
    let mut listing = attach_as(h(0), 1, 900);
    if let HostMsg::Attach { successions, .. } = &mut listing {
        *successions = vec![(s.id, true)];
    }
    c.handle(h(0), listing, 0);
    c.successions_open(vec![s.clone()], 20);
    commit_routed(&mut c);
    assert_eq!(c.succession_progress(), vec![(s.id, vec![h(0)], true)], "the listing boot's answers count");
}

/// A survivor detached when the run is declared (its next boot registered, not attached yet):
/// its stale previous boot is not recorded — its new boot's first `Attach` is recorded and
/// ordered, no rollback.
#[test]
fn a_survivor_detached_at_the_declaration_is_recorded_at_its_attach() {
    let mut c = succeeding(3);
    c.host_detached(h(1));
    commit_routed(&mut c);
    let s = declare_one(&mut c, 3, h(2), 100);
    assert!(s.survivors.contains(&h(1)));
    commit_routed(&mut c);
    c.handle(h(1), attach_as(h(1), 2, 900), 200);
    assert_eq!(orders(&commit_routed(&mut c)), vec![(h(1), s.id)], "its new boot gets the order");
    c.maintain(201);
    assert!(!c.rollback_wanted(), "no journal was counted on from a boot that was not attached");
}

// --- Elastic 1 Task 6: the login service's orders go through the coordinator ----------------

fn spawn_order(epoch: u64) -> game::LoginOrder {
    game::LoginOrder::Spawn { player_token: "tok-secret-login".into(), session_epoch: epoch }
}

/// An order of the login service goes to the lowest attached host that neither left nor was
/// succeeded (the gate host of that moment), and nothing about it is kept: a resend after the
/// gate changed goes to the new gate. A host asking to leave is never the gate.
#[test]
fn a_login_order_goes_to_the_lowest_live_host() {
    let mut c = succeeding(3);
    assert_eq!(c.gate_host(), Some(h(0)));
    assert_eq!(c.login_order(spawn_order(1)), Some((h(0), CoordMsg::Login(spawn_order(1)))));
    assert!(commit_routed(&mut c).iter().all(|(_, m)| !matches!(m, CoordMsg::Login(_))), "returned, not queued");
    // Host 0 is lost (detached, then succeeded): the resend goes to host 1.
    c.host_detached(h(0));
    assert_eq!(c.login_order(spawn_order(1)), Some((h(1), CoordMsg::Login(spawn_order(1)))), "the resend: the gate of its moment");
    let _ = declare_one(&mut c, 3, h(0), 100);
    commit_routed(&mut c);
    assert_eq!(c.gate_host(), Some(h(1)));
    // Host 1 asks to leave: host 2 takes the orders.
    c.handle(h(1), HostMsg::LeaveRequest, 200);
    assert_eq!(c.gate_host(), Some(h(2)), "a leaving host is not the gate");
    let kick = game::LoginOrder::Kick { player_token: "tok-secret-login".into(), session_epoch: 1, reason: "bye".into() };
    assert_eq!(c.login_order(kick.clone()), Some((h(2), CoordMsg::Login(kick))));
    // The answers come back untouched, in order, and are kept only until taken.
    let ack = game::LoginOut::KickAck { player_token: "tok-secret-login".into(), session_epoch: 1 };
    let end = game::LoginOut::SessionEnded { player_token: "tok-secret-login".into(), session_epoch: 1 };
    c.handle(h(2), HostMsg::Login(ack.clone()), 300);
    c.handle(h(1), HostMsg::Login(end.clone()), 300);
    assert_eq!(c.take_login_out(), [ack, end]);
    assert!(c.take_login_out().is_empty());
    assert!(commit_routed(&mut c).is_empty(), "a host's answer changes nothing of the coordinator's");
    // The token never shows in a log line of the order or the answer.
    let shown = format!("{:?} {:?}", spawn_order(1), HostMsg::Login(game::LoginOut::SessionEnded { player_token: "tok-secret-login".into(), session_epoch: 1 }));
    assert!(!shown.contains("tok-secret") && shown.contains("<redacted>"), "{shown}");
}

/// With no host attached the order is dropped: no gate host, nothing queued for anyone (the
/// login service times out and answers 503; the client retries).
#[test]
fn a_login_order_with_no_host_is_dropped() {
    let (mut c, _, _) = coordinator();
    commit_routed(&mut c);
    assert_eq!(c.gate_host(), None);
    assert_eq!(c.login_order(spawn_order(1)), None);
    assert!(commit_routed(&mut c).is_empty(), "nothing queued");
    // A host attaches: the next order (the client's retry) goes to it.
    c.handle(h(4), attach(h(4), 1, Vec::new(), Vec::new()), 0);
    commit_routed(&mut c);
    assert_eq!(c.login_order(spawn_order(2)), Some((h(4), CoordMsg::Login(spawn_order(2)))));
    c.host_detached(h(4));
    assert_eq!(c.login_order(spawn_order(3)), None, "detached: none again");
}

// --- Elastic 1 Task 7: a draining host is never the gate ------------------------------------

/// A read of hosts `0..n` at `now`, every one beating; those of `draining` marked draining.
fn read_draining(c: &mut Coordinator, n: u32, draining: &[HostId], now: u64) {
    let rows: Vec<HostSeen> =
        (0..u64::from(n)).map(h).map(|h| HostSeen { draining: draining.contains(&h), ..seen(h, now + 1) }).collect();
    c.observe_leases(&rows, now, now);
}

/// A host the operator marked draining (`worldctl drain`) is not the gate host: the orders go
/// to the lowest attached host that is not draining, from the hosts read that shows the mark
/// on (no read of its own).
#[test]
fn a_draining_host_is_not_the_gate() {
    let mut c = succeeding(3);
    read_draining(&mut c, 3, &[], 100);
    assert_eq!(c.gate_host(), Some(h(0)));
    read_draining(&mut c, 3, &[h(0)], 200);
    assert_eq!(c.gate_host(), Some(h(1)), "host 0 drains");
    assert_eq!(c.login_order(spawn_order(1)), Some((h(1), CoordMsg::Login(spawn_order(1)))));
    read_draining(&mut c, 3, &[h(0), h(1)], 300);
    assert_eq!(c.gate_host(), Some(h(2)), "hosts 0 and 1 drain");
    assert!(commit_routed(&mut c).iter().all(|(_, m)| !matches!(m, CoordMsg::Login(_))), "nothing queued");
}

/// Every attached live host draining: no gate host — the order is dropped (as with no host
/// attached: the login service answers 503 at its timeout).
#[test]
fn every_host_draining_means_no_gate() {
    let mut c = succeeding(3);
    read_draining(&mut c, 3, &[h(0), h(1), h(2)], 100);
    assert_eq!(c.gate_host(), None);
    assert_eq!(c.login_order(spawn_order(1)), None);
    assert!(commit_routed(&mut c).is_empty(), "nothing queued");
    // A failed hosts read keeps the marks: still no gate.
    c.leases_unread();
    assert_eq!(c.gate_host(), None, "the marks outlive a failed read");
    // A new host attaches (not draining): it takes the orders.
    c.handle(h(3), attach(h(3), 1, Vec::new(), Vec::new()), 150);
    commit_routed(&mut c);
    read_draining(&mut c, 4, &[h(0), h(1), h(2)], 200);
    assert_eq!(c.login_order(spawn_order(2)), Some((h(3), CoordMsg::Login(spawn_order(2)))));
}

// --- Elastic 2a: recipients own land; a landless death is ended -----------------------------

/// A read of hosts `0..n` at `now`, every one beating but those of `dead` (vouched, lapsed at
/// once) and of `declared` (dead rows); those of `landless` hold no land.
fn read_land(c: &mut Coordinator, n: u32, dead: &[HostId], declared: &[HostId], landless: &[HostId], now: u64) {
    let rows: Vec<HostSeen> = (0..u64::from(n))
        .map(h)
        .map(|h| {
            let mut row = seen(h, if dead.contains(&h) { 0 } else { now + 1 });
            row.landed = !landless.contains(&h);
            if declared.contains(&h) {
                row.state = game::HOST_DEAD;
            }
            row
        })
        .collect();
    c.observe_leases(&rows, now, now);
}

/// The store's survivors of `d`: every host of `0..n` but the predecessors and `landless`,
/// plus the heirs.
fn store_declares_landed(d: &Declare, n: u32, landless: &[HostId]) -> Vec<game::Succession> {
    let mut list = store_declares(d, n, 1, game::CutId(0));
    for s in &mut list {
        s.survivors.retain(|h| !landless.contains(h) || d.heirs.contains(h));
    }
    list
}

/// A landless host lapsing — with no run open, or while one is open and it is no recipient of
/// it — is ended, not succeeded and not a second failure: no rollback, the run settles with
/// its survivors only, and the ended host is out of the view (not the gate, not expected).
#[test]
fn a_lapsed_landless_non_recipient_is_ended_not_a_second_failure() {
    // No run open: a landless lapse is ended instead of declared.
    let mut c = succeeding(3);
    read_land(&mut c, 3, &[h(2)], &[], &[h(2)], 10);
    assert_eq!(c.declare_wanted(10), None, "a landless lapse is no succession");
    let end = c.take_end_landless(10).expect("ended instead");
    assert_eq!(end, EndLandless { host: h(2), boot: 102 });
    assert_eq!(c.take_end_landless(11), None, "one end in flight");
    c.landless_ended(end, game::LandlessEnd::Ended, 12);
    assert!(!c.rollback_wanted() && c.successions_open_count() == 0);
    read_land(&mut c, 3, &[h(2)], &[h(2)], &[h(2)], 20);
    assert_eq!((c.declare_wanted(20), c.take_end_landless(20)), (None, None), "never again");
    assert_ne!(c.gate_host(), Some(h(2)));

    // A run open (host 3 succeeded, survivors 0 and 1): landless host 2 lapses.
    let mut c = succeeding(4);
    read_land(&mut c, 4, &[h(3)], &[], &[h(2)], 100);
    let d = c.declare_wanted(100).expect("host 3 is declared");
    assert_eq!(d.dead, vec![(h(3), 103)]);
    assert_ne!(d.heirs[0], h(2), "the landless host is no heir while a landed one takes logins");
    let s = store_declares_landed(&d, 4, &[h(2)]).remove(0);
    assert_eq!(s.survivors, vec![h(0), h(1)]);
    c.declared(game::Declared::Done(vec![s.clone()]), 100);
    c.set_expected_hosts([0, 1, 2].into_iter().map(h).collect());
    commit_routed(&mut c);
    read_land(&mut c, 4, &[h(2)], &[h(3)], &[h(2)], 200);
    c.maintain(200);
    assert!(!c.rollback_wanted(), "a landless non-recipient's lapse is no second failure");
    let end = c.take_end_landless(200).expect("ended while the run is open");
    assert_eq!(end.host, h(2));
    c.landless_ended(end, game::LandlessEnd::Ended, 201);
    c.maintain(201);
    assert!(!c.rollback_wanted());
    // The run settles with its survivors; the cut does not wait for the ended host.
    for hn in [0, 1] {
        c.handle(h(hn), HostMsg::SuccessionApplied { id: s.id }, 300);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 300);
    commit_routed(&mut c);
    c.maintain(300 + CUT_EVERY_MS);
    let next = begun(&commit_routed(&mut c));
    assert_eq!(next.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![h(0), h(1)], "the settle cut begins without host 2");
    assert_ne!(c.gate_host(), Some(h(2)));
}

/// A LANDED host lapsing while a run is open is still a second failure (the residual), and so
/// is a landless recipient (a landless heir); the store answering `Recipient` to an end rolls
/// back too. `OwnsLand` (it won land after the read) waits for a newer read.
#[test]
fn a_lapsed_landed_host_during_a_run_still_rolls_back() {
    let mut c = succeeding(3);
    declare_one(&mut c, 3, h(2), 100);
    commit_routed(&mut c);
    read_land(&mut c, 3, &[h(1)], &[h(2)], &[], 200);
    assert_eq!(c.take_end_landless(200), None, "a landed host is not ended");
    c.maintain(200);
    assert!(c.rollback_wanted(), "a landed lapse before the settle rolls back");

    // A landless survivor (it held land at the declaration; the read shows none now).
    let mut c = succeeding(3);
    let s = declare_one(&mut c, 3, h(2), 100);
    let survivor = *s.survivors.iter().find(|h| **h != s.heir).expect("a survivor that is not the heir");
    read_land(&mut c, 3, &[survivor], &[h(2)], &[survivor], 200);
    assert_eq!(c.take_end_landless(200), None, "a recipient is not ended");
    c.maintain(200);
    assert!(c.rollback_wanted(), "a recipient's lapse rolls back");

    // The store answers `Recipient` to an end handed out while a run is open (the run the
    // core holds does not name the host; the store's does): a rollback.
    let mut c = succeeding(4);
    read_land(&mut c, 4, &[h(3)], &[], &[h(2)], 100);
    let d = c.declare_wanted(100).expect("host 3 is declared");
    c.declared(game::Declared::Done(store_declares_landed(&d, 4, &[h(2)])), 100);
    commit_routed(&mut c);
    read_land(&mut c, 4, &[h(2)], &[h(3)], &[h(2)], 200);
    let end = c.take_end_landless(200).expect("host 2 is no recipient here");
    c.landless_ended(end, game::LandlessEnd::Recipient, 201);
    assert!(c.rollback_wanted(), "the store's run names it: a second failure");
    // Without a run: `OwnsLand` waits for a newer read, `Recipient` asks for a re-read.
    let mut c = succeeding(3);
    read_land(&mut c, 3, &[h(2)], &[], &[h(2)], 10);
    let end = c.take_end_landless(10).unwrap();
    c.landless_ended(end, game::LandlessEnd::OwnsLand, 11);
    assert_eq!(c.take_end_landless(12), None, "not before a newer read");
    read_land(&mut c, 3, &[h(2)], &[], &[], 20);
    assert_eq!(c.take_end_landless(20), None, "landed now");
    assert_eq!(c.declare_wanted(20).map(|d| d.dead), Some(vec![(h(2), 102)]), "a landed lapse is declared");
    let mut c = succeeding(3);
    read_land(&mut c, 3, &[h(2)], &[], &[h(2)], 10);
    let end = c.take_end_landless(10).unwrap();
    c.landless_ended(end, game::LandlessEnd::Recipient, 11);
    assert!(c.take_successions_reread(), "a run the core does not know: re-read");
    assert!(!c.rollback_wanted());
}

/// Heirs are tried in order: landed hosts that take logins, then hosts that take logins, then
/// every attached live host.
#[test]
fn the_heir_prefers_landed_hosts_that_take_logins() {
    let heir = |landless: &[HostId], draining: &[HostId]| {
        let mut c = succeeding(4);
        let rows: Vec<HostSeen> = (0..4)
            .map(h)
            .map(|o| HostSeen {
                landed: !landless.contains(&o),
                draining: draining.contains(&o),
                ..seen(o, if o == h(3) { 0 } else { 11 })
            })
            .collect();
        c.observe_leases(&rows, 10, 10);
        c.declare_wanted(10).expect("host 3 lapsed").heirs[0]
    };
    let all = |of: &[u64]| of.iter().copied().map(h).collect::<std::collections::BTreeSet<_>>();
    // Host 2 is the only landed host taking logins (0 is landless, 1 drains).
    assert_eq!(heir(&[h(0)], &[h(1)]), h(2));
    // No landed host takes logins: the one that takes logins (landless 0).
    assert_eq!(heir(&[h(0)], &[h(1), h(2)]), h(0));
    // Nobody takes logins: any live host.
    assert_eq!(heir(&[h(0)], &[h(0), h(1), h(2)]), game::heir_of(h(3), &all(&[0, 1, 2])).unwrap());
    // Every candidate landed and taking logins: the rendezvous among all of them, as before.
    assert_eq!(heir(&[], &[]), game::heir_of(h(3), &all(&[0, 1, 2])).unwrap());
}

/// An id absent from a successful `hosts` read was pruned: nothing of it is kept (B-M5) — not
/// as succeeded, not its boot, not as declared, not as a dead row asking for a re-read.
#[test]
fn an_id_pruned_from_the_read_is_forgotten() {
    let mut c = succeeding(4);
    let s = declare_one(&mut c, 4, h(2), 100);
    c.host_detached(h(2));
    for hn in [0, 1, 3] {
        c.handle(h(hn), HostMsg::SuccessionApplied { id: s.id }, 100);
    }
    c.handle(s.heir, HostMsg::SuccessionAdopted { id: s.id }, 100);
    commit_routed(&mut c);
    let mentioned = c.host_mentions(h(2));
    assert!(mentioned.contains(&"succeeded") && mentioned.contains(&"declared") && mentioned.contains(&"boots"), "{mentioned:?}");
    // Host 3's row turns dead behind the coordinator's back: a re-read is asked once.
    read_land(&mut c, 4, &[], &[h(2), h(3)], &[], 200);
    assert!(c.host_mentions(h(3)).contains(&"reread_for"));
    assert!(c.take_successions_reread());
    c.successions_open(vec![s.clone()], 201);
    c.host_detached(h(3));
    // Both rows pruned (a registration's prune): nothing is kept of either.
    let rows: Vec<HostSeen> = [0, 1].into_iter().map(h).map(|o| seen(o, 301)).collect();
    c.observe_leases(&rows, 300, 300);
    assert_eq!(c.host_mentions(h(2)), Vec::<&str>::new());
    assert_eq!(c.host_mentions(h(3)), Vec::<&str>::new());
    // An attached host absent from the read (registered after it was issued) keeps its boot.
    assert!(c.host_mentions(h(0)).contains(&"boots"));
    let rows: Vec<HostSeen> = [1].into_iter().map(h).map(|o| seen(o, 401)).collect();
    c.observe_leases(&rows, 400, 400);
    assert!(c.host_mentions(h(0)).contains(&"boots"), "attached: kept");
}

/// Elastic 2a Task 3: the core's clock is the store's, and downtime is not caught up. A core run
/// to day 3 on store time, then fed a store clock an hour later (the coordinator was paused, or
/// gone and rebuilt): the calendar re-anchors at the history's own `now` instead of running the
/// hour's 72 000 ticks at once — at most one `maintain` step moves it. Afterwards it runs on as
/// before: one tick per `TICK_RATE`, a day's turn every `DAY_MS`.
#[test]
fn downtime_is_not_caught_up() {
    use game::history::calendar::DAY_MS;
    const T0: u64 = 1_763_000_000_000;
    let (hist, _, _) = world();
    let mut c = Coordinator::new(hist, T0);
    let start = c.history().now();
    let mut now = T0;
    while c.history().now().0 < start.0 + 3 * TICKS_PER_DAY {
        now += 1_000;
        c.maintain(now);
    }
    let written = c.take_transition().expect("days turned");
    let history = written.history.clone().expect("the history advanced");
    c.take_msgs();
    let at_save = c.history().now();
    let later = now + 3_600_000;

    // The same core, its store clock an hour on.
    c.maintain(later);
    let moved = c.history().now().0 - at_save.0;
    assert!(moved <= 1, "an hour of downtime is not caught up: the calendar moved {moved} ticks");
    assert!(c.calendar_now(later).0 - at_save.0 <= 1);
    // It runs on from there.
    c.maintain(later + 7 * game::TICK_RATE);
    assert_eq!(c.history().now().0, at_save.0 + moved + 7, "one tick per TICK_RATE after the re-anchor");
    let day = c.history().now().0 / TICKS_PER_DAY;
    let mut t = later + 7 * game::TICK_RATE;
    while c.history().now().0 / TICKS_PER_DAY == day {
        t += 1_000;
        c.maintain(t);
    }
    assert!(t - later <= DAY_MS + 1_000, "the next day turns within a day of the re-anchor");

    // Rebuilt from the written projection, the hour later: its stored anchor is an hour old —
    // the gap rule re-anchors it, nothing is caught up.
    let mut c2 = reload(&written, &history, ReplicaId(1), later);
    let loaded = c2.history().now();
    c2.maintain(later);
    assert!(c2.history().now().0 - loaded.0 <= 1, "a rebuilt core does not catch up either");
}

/// Elastic 2a (moved here from Task 3): a coordinator rebuilt from its written projection two
/// seconds after the write runs its calendar on from the STORED anchor — the ticks of those two
/// seconds are there at once (no skip back to the history's own `now`, no re-anchor), and the
/// day turn the dead process broadcast is not broadcast again.
#[test]
fn the_calendar_continues_across_a_quick_restart() {
    use game::history::calendar::DAY_MS;
    const T0: u64 = 1_763_000_000_000;
    let (hist, _, _) = world();
    let mut c = Coordinator::new(hist, T0);
    let mut now = T0;
    let mut history = Vec::new();
    let mut last = None;
    // Two days, every transition written.
    let start = c.history().now().0;
    while c.history().now().0 < start + 2 * TICKS_PER_DAY + 2 {
        now += 250;
        c.maintain(now);
        if let Some(t) = written(&mut c) {
            if let Some(h) = &t.history {
                history = h.clone();
            }
            last = Some((t, now));
        }
    }
    let (t, written_at) = last.expect("written");
    let day = c.history().now().0 / TICKS_PER_DAY;
    let restart = now + 2_000;
    let mut c2 = reload(&t, &history, ReplicaId(1), restart);
    assert_eq!(c2.calendar_anchor(), c.calendar_anchor(), "the anchor is the stored one");
    let expected = c.calendar_now(restart);
    assert_eq!(c2.calendar_now(restart), expected, "the calendar continues from the stored anchor");
    assert!(expected.0 >= c.history().now().0 + 2_000 / game::TICK_RATE, "two seconds on: no skip");
    c2.maintain(restart);
    assert_eq!(c2.history().now(), expected, "one step brings the history there");
    let out = c2.take_msgs();
    assert!(
        !out.iter().any(|m| matches!(m, CoordMsg::Calendar(_))),
        "the day turn written at {written_at} is not broadcast again: {out:?}"
    );
    // The next day turns once, a day after the last.
    let mut t2 = restart;
    let mut turns = 0;
    while c2.history().now().0 / TICKS_PER_DAY <= day {
        t2 += 250;
        c2.maintain(t2);
        turns += c2.take_msgs().iter().filter(|m| matches!(m, CoordMsg::Calendar(_))).count();
    }
    assert_eq!(turns, 1, "the next day's turn, once");
    assert!(t2 - written_at <= DAY_MS + 1_000, "within a day of the last turn");
}

/// Elastic 2a (moved here from Task 3): a world cut running when the projection was written,
/// its deadline more than [`CALENDAR_GAP_MS`] past when the core is loaded, restarts its count
/// at the load (never earlier than stored); one within the gap stands and fires as it would have.
#[test]
fn a_cut_deadline_older_than_the_gap_restarts_its_count() {
    const T0: u64 = 1_763_000_000_000;
    let begin = |c: &mut Coordinator| -> game::CutId {
        let attach = |o: u64| HostMsg::Attach { host: h(o), epoch: 1, running: Vec::new(), online: Vec::new(), generation: 0, boot: 7, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Succession };
        c.set_cut_epoch(1);
        c.set_world_cuts(1_000);
        c.set_expected_hosts([h(0), h(1)].into_iter().collect());
        c.handle(h(0), attach(0), T0);
        c.handle(h(1), attach(1), T0);
        c.maintain(T0 + 1_000);
        c.world_cut_running().expect("a cut begins").0
    };
    let (hist, _, _) = world();
    let mut c = Coordinator::new(hist, T0);
    let cut = begin(&mut c);
    let t = c.take_transition().expect("the cut began");
    let history = t.history.clone().expect("a new core writes its history");
    // Loaded an hour on: the deadline is far past — its count starts over at the load.
    let later = T0 + 3_600_000;
    let mut late = reload(&t, &history, ReplicaId(1), later);
    late.set_world_cuts(1_000);
    late.maintain(later + game::WORLD_CUT_DEADLINE_MS - 1);
    assert_eq!(late.world_cut_progress().map(|(id, _, v)| (id, v)), Some((cut, None)), "not aborted at once");
    late.maintain(later + game::WORLD_CUT_DEADLINE_MS);
    assert_eq!(late.world_cut_progress().map(|(id, _, v)| (id, v)), Some((cut, Some(false))), "aborted a whole deadline after the load");
    // Loaded within the gap past its deadline: it stands, and the first upkeep aborts the cut.
    let soon = T0 + 1_000 + game::WORLD_CUT_DEADLINE_MS + CALENDAR_GAP_MS;
    let mut quick = reload(&t, &history, ReplicaId(1), soon);
    quick.set_world_cuts(1_000);
    quick.maintain(soon);
    assert_eq!(quick.world_cut_progress().map(|(id, _, v)| (id, v)), Some((cut, Some(false))), "the stored deadline fired");
}

// ---- The durable projection (Elastic 2a Task 5) ---------------------------------------

/// What one input does to a core, at store time `now`.
type LifeStep = Box<dyn Fn(&mut Coordinator, u64)>;

/// A host's `Attach` under `claim` (boot 7 unless given), running `running`, `online` online.
fn claim_attach(host: HostId, boot: u64, running: Vec<RunningRegion>, online: Vec<OnlinePlayer>) -> HostMsg {
    HostMsg::Attach { host, epoch: 1, running, online, generation: 0, boot, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Succession }
}

/// The coordinator inputs of a short world life over [`world`] — three hosts attach, regions
/// start and park, people cross and bounce, requests (state-changing and queries), presence
/// (online, a hand-off, offline), the calendar across day turns, a world cut decided, a
/// succession declared, answered and settled by the next cut — each with its store time.
/// `gone` is set once host 2 is no longer expected by the cuts (the runtime's next read after
/// its declaration): what a reloaded core is configured with.
fn a_world_life(gone: std::rc::Rc<std::cell::Cell<bool>>) -> Vec<(u64, LifeStep)> {
    const T0: u64 = 1_763_000_000_000;
    let (r0, r1, r2) = (rc(0, 0), rc(1, 0), rc(8, 0));
    let mut life: Vec<(u64, LifeStep)> = Vec::new();
    let mut t = T0;
    let mut at = |life: &mut Vec<(u64, LifeStep)>, dt: u64, f: LifeStep| {
        t += dt;
        life.push((t, f));
    };
    for o in 0..3 {
        at(&mut life, 10, Box::new(move |c, now| c.handle(h(o), claim_attach(h(o), 7, Vec::new(), Vec::new()), now)));
    }
    at(&mut life, 10, Box::new(|c, now| {
        let site = site_of(c);
        c.handle(h(0), HostMsg::Request { id: rid(1), player: A, hop: PLACED, req: start_request(site) }, now)
    }));
    at(&mut life, 10, Box::new(move |c, now| c.handle(h(0), HostMsg::RegionStarted { rc: r0, incarnation: 1 }, now)));
    at(&mut life, 10, Box::new(move |c, now| c.handle(h(1), HostMsg::RegionStarted { rc: r1, incarnation: 1 }, now)));
    at(&mut life, 10, Box::new(|c, now| {
        let p = *c.history().site(SiteId(0)).unwrap().members.iter().nth(3).unwrap();
        c.handle(h(1), HostMsg::Request { id: RequestId { host: h(1), boot: 7, seq: 1 }, player: B, hop: PLACED, req: WorldRequest::PersonInfo(p) }, now)
    }));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), online(A, Some(Claim::Adventure(site_of(c))), false), now)));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), HostMsg::PlayerHandedTo { player: A, to: h(1), hop: PLACED }, now)));
    at(&mut life, 10, Box::new(|c, now| {
        let claim = Some(Claim::Adventure(site_of(c)));
        c.handle(h(1), HostMsg::PlayerOnline(OnlinePlayer { player: A, claim, claim_pending: false, report: 2, hop: PLACED + 1 }), now)
    }));
    // A stale end of A's older placement on host 0 (its hop is below A's): ignored.
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), HostMsg::PlayerOffline { player: A, hop: PLACED }, now)));
    at(&mut life, 10, Box::new(|c, now| assert_eq!(c.online_on(A), Some(h(1)), "A stays online on host 1 at {now}")));
    at(&mut life, 10, Box::new(move |c, now| {
        let p = *c.history().site(SiteId(0)).unwrap().members.iter().next().unwrap();
        c.handle(h(0), HostMsg::PersonCrossed { person: p, to: r1, delivered: false }, now)
    }));
    at(&mut life, 10, Box::new(move |c, now| c.handle(h(2), HostMsg::RegionStarted { rc: r2, incarnation: 1 }, now)));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), HostMsg::Settled { boot: 7, below: 1 }, now)));
    // The calendar runs (day turns), a world cut of all three begins.
    for _ in 0..30 {
        at(&mut life, 1_000, Box::new(|c, now| c.maintain(now)));
    }
    // Every part staged; the decision handed out, then recorded.
    for o in 0..3 {
        at(&mut life, 10, Box::new(move |c, now| {
            if let Some((cut, _)) = c.world_cut_running() {
                c.handle(h(o), HostMsg::CutPrepared { cut }, now);
            }
        }));
    }
    at(&mut life, 10, Box::new(|c, _| {
        let d = c.take_decision().expect("every part staged: a decision");
        assert!(d.commit, "{d:?}");
    }));
    at(&mut life, 10, Box::new(|c, _| {
        let (cut, _) = c.world_cut_running().expect("still running until recorded");
        c.decided(cut, true);
    }));
    // Host 2 is declared dead: host 0 its heir, host 1 a survivor.
    at(&mut life, 10, Box::new(|c, _| c.declaring_by_hand()));
    at(&mut life, 10, Box::new(|c, now| {
        let s = game::Succession {
            id: game::SuccessionId(1),
            predecessor: h(2),
            predecessor_boot: 7,
            heir: h(0),
            kind: game::SuccessionKind::Crash,
            base_cut: game::CutId((3 << 32) | 1),
            moved: vec![((0, 0), 2)],
            survivors: vec![h(0), h(1)],
        };
        c.declared(game::Declared::Done(vec![s]), now);
    }));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(1), HostMsg::SuccessionApplied { id: game::SuccessionId(1) }, now)));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), HostMsg::SuccessionApplied { id: game::SuccessionId(1) }, now)));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), HostMsg::SuccessionAdopted { id: game::SuccessionId(1) }, now)));
    at(&mut life, 10, Box::new(move |c, _| {
        gone.set(true);
        c.set_expected_hosts([h(0), h(1)].into_iter().collect());
    }));
    for _ in 0..4 {
        at(&mut life, 1_000, Box::new(|c, now| c.maintain(now)));
    }
    for o in 0..2 {
        at(&mut life, 10, Box::new(move |c, now| {
            if let Some((cut, _)) = c.world_cut_running() {
                c.handle(h(o), HostMsg::CutPrepared { cut }, now);
            }
        }));
    }
    at(&mut life, 10, Box::new(|c, _| {
        let d = c.take_decision().expect("the settle cut's decision");
        assert_eq!(d.settles, vec![game::SuccessionId(1)]);
    }));
    at(&mut life, 10, Box::new(|c, _| {
        let (cut, _) = c.world_cut_running().expect("recorded next");
        c.decided(cut, true);
    }));
    at(&mut life, 10, Box::new(move |c, now| c.handle(h(1), HostMsg::RegionStopping { rc: r1 }, now)));
    at(&mut life, 10, Box::new(move |c, now| c.handle(h(1), HostMsg::RegionParked { rc: r1, people: Vec::new() }, now)));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(1), HostMsg::PlayerOffline { player: A, hop: PLACED + 1 }, now)));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), HostMsg::ReleaseClaim { player: A }, now)));
    for _ in 0..12 {
        at(&mut life, 1_000, Box::new(|c, now| c.maintain(now)));
    }
    // A lease read showing a dead row nobody here declared (host 8): the open successions are
    // read again (`reread_for`, `reread`, `known`), and the read's answer.
    at(&mut life, 10, Box::new(|c, now| {
        let row = |o: u64, state: i64| HostSeen { ordinal: h(o), boot: 7 + o, clean: false, state, seen_ms: now, draining: false, landed: true };
        c.observe_leases(&[row(0, game::HOST_LIVE), row(1, game::HOST_LIVE), row(8, game::HOST_DEAD)], now, now);
    }));
    at(&mut life, 10, Box::new(|c, now| {
        assert!(c.take_successions_reread(), "a dead row nobody declared asks for a re-read");
        c.successions_open(Vec::new(), now);
    }));
    // A host asks for land of a departed owner (`land_lookups`), the lookup's answer (`land_wanted`).
    at(&mut life, 10, Box::new(|c, now| c.handle(h(1), HostMsg::LandWanted { sr: (3, 3) }, now)));
    at(&mut life, 10, Box::new(|c, now| {
        assert_eq!(c.take_land_lookups(), vec![(3, 3)]);
        c.land_wanted(h(9), now);
    }));
    // A host asks about a cut nobody remembers (`lookups`), and the store's answer.
    at(&mut life, 10, Box::new(|c, now| c.handle(h(0), HostMsg::CutPrepared { cut: game::CutId(77) }, now)));
    at(&mut life, 10, Box::new(|c, _| {
        assert_eq!(c.take_cut_lookups(), vec![(h(0), game::CutId(77))]);
        c.cut_outcome(h(0), game::CutId(77), false);
    }));
    // Host 1's link drops and it attaches again.
    at(&mut life, 10, Box::new(|c, _| c.host_detached(h(1))));
    at(&mut life, 10, Box::new(|c, now| c.handle(h(1), claim_attach(h(1), 7, Vec::new(), Vec::new()), now)));
    // Host 1 asks to leave (`leave_requests`): its leave cut begins, both parts staged, committed.
    at(&mut life, 10, Box::new(|c, now| c.handle(h(1), HostMsg::LeaveRequest, now)));
    for _ in 0..2 {
        at(&mut life, 1_000, Box::new(|c, now| c.maintain(now)));
    }
    for o in 0..2 {
        at(&mut life, 10, Box::new(move |c, now| {
            if let Some((cut, _)) = c.world_cut_running() {
                c.handle(h(o), HostMsg::CutPrepared { cut }, now);
            }
        }));
    }
    at(&mut life, 10, Box::new(|c, _| {
        let d = c.take_decision().expect("the leave cut's decision");
        assert_eq!(d.leaving, vec![h(1)]);
    }));
    at(&mut life, 10, Box::new(|c, _| {
        let (cut, _) = c.world_cut_running().expect("recorded next");
        c.decided(cut, true);
        assert_eq!(c.left_hosts(), vec![h(1)]);
    }));
    // The store refuses a declaration with a succession open elsewhere: a rollback is wanted
    // (`rollback`), executed, and a host of the new generation attaches.
    at(&mut life, 10, Box::new(|c, _| c.declaring_by_hand()));
    at(&mut life, 10, Box::new(|c, now| c.declared(game::Declared::SuccessionOpen, now)));
    at(&mut life, 10, Box::new(|c, _| {
        assert_eq!(c.rollback_from(), Some(0));
        c.rolled_back(1);
    }));
    at(&mut life, 10, Box::new(|c, now| {
        let mut attach = claim_attach(h(7), 70, Vec::new(), Vec::new());
        if let HostMsg::Attach { generation, .. } = &mut attach {
            *generation = 1;
        }
        c.handle(h(7), attach, now);
    }));
    for _ in 0..3 {
        at(&mut life, 1_000, Box::new(|c, now| c.maintain(now)));
    }
    life
}

/// The configuration a runtime sets on every core it starts or loads (not in the projection):
/// cuts of hosts 0..3 every 20 s, or of hosts 0 and 1 once host 2 is `gone`.
fn configure(c: &mut Coordinator, gone: bool) {
    c.set_cut_epoch(3);
    c.set_world_cuts(CUT_EVERY_MS * 20);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.set_expected_hosts((0..if gone { 2 } else { 3 }).map(h).collect());
    c.set_world_tick(100);
}

/// Elastic 2a: the whole core is its projection. A world's inputs run into core A, and into
/// core B, which is loaded again from its own last written transition after EVERY input (the same
/// process boot: a cache reloaded) — each input as one runtime pass (the input, then the
/// upkeep): their outputs are equal, in order, input by input, and so are their final bytes.
#[test]
fn a_core_reloaded_after_every_input_is_identical() {
    let (hist, _, _) = world();
    let gone = std::rc::Rc::new(std::cell::Cell::new(false));
    let life = a_world_life(gone.clone());
    let start = life[0].0 - 10;
    let process = ReplicaId(0x5EED);
    let mut a = Coordinator::new_for(hist, start, process);
    configure(&mut a, false);
    let t0 = written(&mut a).expect("a new core writes itself");
    let mut history = t0.history.clone().expect("with its history");
    let load = |t: &Transition, history: &[u8], now: u64| {
        let mut b = Coordinator::load(&t.core, history, grid(), process, now).expect("the projection loads");
        configure(&mut b, gone.get());
        b
    };
    let mut b = load(&t0, &history, start);
    // Loaded at the row's version (the marks of its decisions are stamped by it).
    b.set_version(a.version());
    // Elastic 2b Task 6: a loaded ticker owes a read of the open successions (the process's to
    // know): answered as the store holds them (an ask the projection carries stays asked).
    b.successions_open(Vec::new(), start);
    let mut last = t0;
    let (mut sent, mut transitions) = (Vec::new(), 0usize);
    // One runtime pass per input: the input, then the upkeep at the pass's store time.
    for (i, (now, input)) in life.iter().enumerate() {
        for c in [&mut a, &mut b] {
            input(c, *now);
            c.maintain(*now);
        }
        let (ta, tb) = (written(&mut a), written(&mut b));
        assert_eq!(ta.is_some(), tb.is_some(), "input {i}: a transition in one core only");
        let mut out_a = ta.as_ref().map(|t| t.out.clone()).unwrap_or_default();
        out_a.extend(a.take_out());
        let mut out_b = tb.as_ref().map(|t| t.out.clone()).unwrap_or_default();
        out_b.extend(b.take_out());
        assert_eq!(out_a, out_b, "input {i}: the outputs differ");
        sent.extend(out_a.into_iter().map(|(_, m)| m));
        if let (Some(ta), Some(tb)) = (ta, tb) {
            transitions += 1;
            if ta.core != tb.core {
                let (da, db) = (a.projection_debug(), b.projection_debug());
                let diff: Vec<(usize, &str, &str)> =
                    da.lines().zip(db.lines()).enumerate().filter(|(_, (x, y))| x != y).map(|(n, (x, y))| (n, x, y)).collect();
                panic!("input {i}: the projections differ: {diff:#?}");
            }
            assert_eq!(ta.history, tb.history, "input {i}: the histories differ");
            if let Some(hb) = &tb.history {
                history = hb.clone();
            }
            last = tb;
        }
        // Reloaded after EVERY input, from the last transition written: what is not in it is lost.
        b = load(&last, &history, *now);
        b.set_version(a.version());
        // Elastic 2b Task 6: a loaded ticker owes a read of the open successions (they are the
        // process's to know); the store holds what A holds.
        b.successions_open(a.successions_listed(), *now);
    }
    let kinds = |f: fn(&CoordMsg) -> bool| sent.iter().filter(|m| f(m)).count();
    assert!(transitions > 20, "a life worth comparing: {transitions} transitions");
    assert!(kinds(|m| matches!(m, CoordMsg::SpawnPeople { .. })) >= 3, "regions started: {sent:?}");
    assert!(kinds(|m| matches!(m, CoordMsg::Reply { reply: WorldReply::AdventureStarted { .. }, .. })) == 1, "the start granted once: {:?}", replies(&sent));
    assert!(kinds(|m| matches!(m, CoordMsg::Calendar(_))) >= 3, "days turned");
    assert_eq!(kinds(|m| matches!(m, CoordMsg::CutDone { committed: true, .. })), 7, "three cuts committed (3 + 2 + 2 participants)");
    assert_eq!(kinds(|m| matches!(m, CoordMsg::Generation(1))), 1, "the rollback told once");
    assert_eq!(kinds(|m| matches!(m, CoordMsg::Succession { .. })), 2, "the order to both recipients");
    assert_eq!(kinds(|m| matches!(m, CoordMsg::SuccessionDone { settled: true, .. })), 2, "settled, both told");
    // The final core bytes; the histories were compared as written by every transition (a clean
    // advance of the history's `now` is not written: the calendar anchor carries the time).
    assert_eq!(a.encode_projection().0, b.encode_projection().0, "the final bytes");
    assert_eq!(a.successions_open_count(), 0, "the succession settled");
}

/// Elastic 2a: an attachment stamped with another process boot counts as detached — a core
/// loaded by a new process has no host attached (no gate host, no cut begins, no order goes
/// out) until each host attaches again, and each `Attach` corrects what the projection holds
/// of that host alone: its live regions it does not list park, another host's stand until
/// that host attaches. A decision the dead process handed out (it committed with that process's
/// transition, its answer lost) is looked up, never handed out again (Elastic 2b Task 3). Loaded
/// by the same process boot (a cache reloaded) every attachment stands.
#[test]
fn an_attachment_of_another_process_boot_is_detached() {
    let (hist, _, people) = world();
    let (old, new) = (ReplicaId(11), ReplicaId(12));
    let mut c = Coordinator::new_for(hist, 0, old);
    configure(&mut c, true);
    let (r0, r1) = (rc(0, 0), rc(1, 0));
    c.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 0);
    c.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 0);
    c.handle(h(0), HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
    c.handle(h(1), HostMsg::RegionStarted { rc: r1, incarnation: 1 }, 0);
    c.maintain(20 * CUT_EVERY_MS);
    let (cut, _) = c.world_cut_running().expect("both attached: a cut begins");
    for o in 0..2 {
        c.handle(h(o), HostMsg::CutPrepared { cut }, 20 * CUT_EVERY_MS);
    }
    assert!(c.take_decision().is_some_and(|d| d.commit), "handed to the old process");
    assert_eq!(c.gate_host(), Some(h(0)));
    let t = c.take_transition().expect("pending");
    let history = t.history.clone().expect("a new core writes its history");
    assert!(c.history().live_regions().contains(&r0));

    // The same process boot: the attachments and the handed decision stand.
    let mut same = reload(&t, &history, old, 20 * CUT_EVERY_MS);
    assert_eq!(same.gate_host(), Some(h(0)), "a cache reloaded keeps its hosts attached");
    assert!(same.take_decision().is_none(), "the decision is in flight in this process");

    // A new process boot: nobody is attached, the decision is looked up, never handed out again.
    let mut c = reload(&t, &history, new, 20 * CUT_EVERY_MS);
    configure(&mut c, true);
    assert_eq!(c.gate_host(), None, "no host attached to the new process");
    assert!(c.take_decision().is_none(), "never handed out again");
    assert_eq!(c.take_decision_lookup().map(|(c, _)| c), Some(cut), "looked up instead");
    assert_eq!(c.take_decision_lookup(), None, "once");
    c.decision_looked_up(cut, true);
    c.maintain(60 * CUT_EVERY_MS);
    assert!(c.world_cut_running().is_none(), "no cut begins with no host attached");
    // Host 1 attaches again running nothing: ITS region parks; host 0's stands.
    c.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 60 * CUT_EVERY_MS);
    let live = c.history().live_regions();
    assert!(!live.contains(&r1), "host 1 does not run {r1:?}: parked");
    assert!(live.contains(&r0), "host 0's region stands until host 0 attaches");
    assert_eq!(c.region_host(r0), Some(h(0)));
    assert_eq!(c.gate_host(), Some(h(1)), "host 1 is attached to the new process");
    // Host 0 attaches again as a NEW boot listing r0 with one body: kept exactly, nothing spawned.
    let p = people[0];
    let running = RunningRegion { rc: r0, incarnation: 1, people: vec![(p, [1.0, 2.0, 3.0])] };
    c.handle(h(0), claim_attach(h(0), 70, vec![running], Vec::new()), 60 * CUT_EVERY_MS);
    let out = commit(&mut c);
    assert!(!out.iter().any(|m| matches!(m, CoordMsg::SpawnPeople { .. })), "a reattach spawns nothing: {out:?}");
    assert!(events_about(&out, p).is_empty(), "P's body stands: {out:?}");
    assert_eq!(c.history().person(p).unwrap().location, Location::InRegion(r0));
    assert_eq!(c.gate_host(), Some(h(0)));
}

/// Elastic 2a (found by r4 `heir_killed_before_the_settle_rolls_the_world_back_once`, once every
/// output waits for its transition): after the rollback it ordered, no host of the old
/// generation counts as attached — while its `Generation` is still on its way, a lapse is not
/// declared (no old host inherits land it is about to drop), no host is the gate and no cut
/// begins. A host of the new generation attaching counts again.
#[test]
fn after_a_rollback_no_old_host_inherits_or_takes_logins() {
    let mut c = succeeding(3);
    let _ = declare_one(&mut c, 3, h(2), 100);
    commit_routed(&mut c);
    // The heir lapses with the run open: a second failure, the world rolls back.
    read_leases(&mut c, 3, &[h(0)], &[h(2)], 200);
    c.maintain(200);
    assert!(c.rollback_wanted());
    c.rolled_back(9);
    assert_eq!(c.gate_host(), None, "no old host takes logins");
    read_leases(&mut c, 3, &[h(0)], &[h(2)], 300);
    assert_eq!(c.declare_wanted(300), None, "no old host inherits the lapsed one's land");
    c.maintain(300 + 10 * CUT_EVERY_MS);
    assert!(c.world_cut_running().is_none(), "no cut begins");
    // A new process (a new host id) attaches at the new generation.
    c.handle(h(5), HostMsg::Attach { host: h(5), epoch: 1, running: Vec::new(), online: Vec::new(), generation: 9, boot: 50, settled_below: 0, handed: Vec::new(), pending_cut: None, successions: Vec::new(), policy: game::CrashPolicy::Succession }, 400);
    assert_eq!(c.gate_host(), Some(h(5)));
}

/// Elastic 2a (the restart undo, WATCH): the first host to attach at a higher world generation
/// (the world restarted from a cut: a rollback, a total crash) finds every host of an older
/// generation gone — a restarted process is a new host, so those ids never attach again. What
/// the projection still holds of them is undone as a predecessor's is: their live regions park
/// (the people return to the abstract history), their players go offline. Before Elastic 2a a
/// restart of the coordinator forgot every host; now the generation says so, restarted or not.
#[test]
fn the_first_attach_of_a_new_generation_parks_every_older_hosts_regions() {
    let (hist, _, people) = world();
    for restarted in [false, true] {
        let mut c = Coordinator::new_for(hist.clone(), 0, ReplicaId(1));
        let r0 = rc(0, 0);
        c.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 0);
        c.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 0);
        c.handle(h(0), HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 0);
        c.handle(h(1), online(A, None, false), 0);
        commit(&mut c);
        assert_eq!(c.history().person(people[0]).unwrap().location, Location::InRegion(r0));
        assert_eq!(c.online_on(A), Some(h(1)));
        if restarted {
            let (core, history) = c.encode_projection();
            c = Coordinator::load(&core, &history, grid(), ReplicaId(2), 10).expect("loads");
        }
        // Host 5, a new process, attaches at generation 1, running nothing.
        let mut attach = claim_attach(h(5), 50, Vec::new(), Vec::new());
        if let HostMsg::Attach { generation, .. } = &mut attach {
            *generation = 1;
        }
        c.handle(h(5), attach, 20);
        commit(&mut c);
        assert!(!c.history().live_regions().contains(&r0), "restarted {restarted}: {r0:?} parks");
        assert_eq!(c.region_host(r0), None);
        assert_ne!(c.history().person(people[0]).unwrap().location, Location::InRegion(r0), "the people are abstract again");
        assert_eq!(c.online_on(A), None, "A is offline until a host of the new generation reports it");
        // A second host of the new generation attaching changes nothing of the first's.
        c.handle(h(5), HostMsg::RegionStarted { rc: r0, incarnation: 1 }, 30);
        let mut attach = claim_attach(h(6), 60, Vec::new(), Vec::new());
        if let HostMsg::Attach { generation, .. } = &mut attach {
            *generation = 1;
        }
        c.handle(h(6), attach, 40);
        assert_eq!(c.region_host(r0), Some(h(5)), "host 5's region stands");
    }
}

/// Elastic 2a fix round 1 (I1): the wanted rollback is in the projection with the generation it
/// was decided at (`rollback_from`), which the store's rollback is conditional on. The
/// coordinator dies after the rollback commits and before its transition is written: the next
/// process loads the projection still wanting it, asks the store again from the old generation,
/// is answered the world's generation (already rolled back, nothing written) and records it ONCE:
/// told to the hosts once — or, if hosts of the new generation attached first, not again at all
/// (they stay attached).
#[test]
fn a_rollback_committed_before_its_transition_is_recorded_once_after_a_restart() {
    for hosts_first in [false, true] {
        let mut c = succeeding(3);
        let _ = declare_one(&mut c, 3, h(2), 100);
        commit_routed(&mut c);
        read_leases(&mut c, 3, &[h(0)], &[h(2)], 200);
        c.maintain(200);
        assert_eq!(c.rollback_from(), Some(0), "wanted from the generation the hosts run");
        let t = c.take_transition().expect("the rollback is wanted");
        // The runtime's `rollback_world(fence, 0)` commits generation 1; the process dies before
        // `rolled_back` is applied and written.
        let (_, history) = c.encode_projection();
        let mut c = reload(&t, &history, ReplicaId(77), 300);
        c.set_crash_policy(game::CrashPolicy::Succession);
        assert_eq!(c.rollback_from(), Some(0), "the restarted process asks again from generation 0");
        if hosts_first {
            let mut attach = claim_attach(h(5), 50, Vec::new(), Vec::new());
            if let HostMsg::Attach { generation, .. } = &mut attach {
                *generation = 1;
            }
            c.handle(h(5), attach, 310);
            commit_routed(&mut c);
        }
        // The store answers the world's generation: 1 (rolled back already, nothing written).
        c.rolled_back(1);
        let out = commit_routed(&mut c);
        let told = out.iter().filter(|(_, m)| matches!(m, CoordMsg::Generation(1))).count();
        assert_eq!(told, usize::from(!hosts_first), "hosts first {hosts_first}: {out:?}");
        assert!(!c.rollback_wanted());
        assert_eq!(c.world_generation(), 1);
        if hosts_first {
            assert_eq!(c.gate_host(), Some(h(5)), "the new generation's host stays attached");
        }
    }
}

/// Elastic 2a fix round 1 (I2): while a transition's write is in flight nothing leaves by
/// `take_out`, not even what changed nothing durable — a replayed claim-granting reply whose
/// grant is only in the unwritten transition must not overtake it (an exit whose last write
/// timed out releases nothing). Once written, what waited leaves; a failed write keeps it.
#[test]
fn nothing_leaves_while_a_write_is_in_flight() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    let t = c.take_transition().expect("the grant is pending");
    // The host resends the request (a timeout): the memo answers, nothing durable changes.
    c.handle(h(0), adventure(1, A, site), 0);
    assert!(c.take_transition().is_none(), "nothing durable changed since");
    assert!(c.take_out().is_empty(), "the replayed grant waits for the grant's write");
    c.transition_failed(t);
    assert!(c.take_out().is_empty(), "a failed write releases nothing");
    let t = c.take_transition().expect("written again");
    assert!(c.take_out().is_empty());
    c.transition_written(c.version() + 1);
    let mut out: Vec<CoordMsg> = t.out.into_iter().map(|(_, m)| m).collect();
    out.extend(c.take_out().into_iter().map(|(_, m)| m));
    let granted = replies(&out).into_iter().filter(|(id, r)| *id == rid(1) && matches!(r, WorldReply::AdventureStarted { .. })).count();
    assert_eq!(granted, 2, "the grant and its replay, once written: {out:?}");
}

// ---- Elastic 2a final review fix wave ----------------------------------------------------

/// Elastic 2a final review (C1, conservation): the coordinator dies after the store committed a
/// decision about host 1 — its succession, or the end of its (landless) row — and before the
/// transition holding the core's answer was written. The next process loads the projection,
/// which still has host 1 running its region (the people embodied there) and player A online
/// on it: once the store's open successions are read (a new run) or a `hosts` read shows the
/// dead row nobody here declared (an end), host 1 is taken out of the view as a predecessor is
/// — its region parks (the people return to the abstract history), A goes offline.
#[test]
fn a_store_decision_lost_with_its_transition_still_takes_the_dead_host_out() {
    let (_, _, people) = world();
    let r0 = rc(0, 0);
    for ended in [false, true] {
        let mut c = succeeding(2);
        let here: Vec<(PersonId, [f32; 3])> = people[2..].iter().map(|p| (*p, [0.0; 3])).collect();
        c.handle(h(1), attach(h(1), 1, vec![RunningRegion { rc: r0, incarnation: 1, people: here }], vec![report(A, None, false)]), 0);
        commit_routed(&mut c);
        assert_eq!((c.region_host(r0), c.online_on(A)), (Some(h(1)), Some(h(1))));
        for p in &people[2..] {
            assert_eq!(c.history().person(*p).unwrap().location, Location::InRegion(r0));
        }
        // The projection as the store holds it: the last transition written.
        let (core, history) = c.encode_projection();
        let landless: &[HostId] = if ended { &[h(1)] } else { &[] };
        read_land(&mut c, 2, &[h(1)], &[], landless, 10);
        let run = if ended {
            let end = c.take_end_landless(10).expect("a landless lapse is ended");
            assert_eq!(end.host, h(1));
            None
        } else {
            let d = c.declare_wanted(10).expect("host 1 is declared");
            Some(store_declares(&d, 2, 1, game::CutId(0)).remove(0))
        };
        // The store committed it; the process dies before its answer is applied and written.
        let mut c = Coordinator::load(&core, &history, grid(), ReplicaId(2), 20).expect("the projection loads");
        // Its first input (Elastic 2b Task 4): it registered alone — the ticker.
        c.apply(Input::ReplicaJoined { me: ReplicaId(2), alone: true }, 20);
        c.set_cut_epoch(4);
        c.set_world_cuts(CUT_EVERY_MS);
        c.set_crash_policy(game::CrashPolicy::Succession);
        c.set_expected_hosts([h(0)].into_iter().collect());
        c.successions_unknown();
        c.successions_open(run.into_iter().collect(), 20);
        c.handle(h(0), attach(h(0), 2, Vec::new(), Vec::new()), 20);
        read_land(&mut c, 2, &[], &[h(1)], landless, 30);
        if c.take_successions_reread() {
            c.successions_open(Vec::new(), 31);
        }
        c.maintain(40);
        commit_routed(&mut c);
        assert_eq!(c.region_host(r0), None, "ended {ended}: host 1's region is no longer its");
        assert!(!c.history().live_regions().contains(&r0), "ended {ended}: {r0:?} parked");
        for p in &people[2..] {
            assert_ne!(
                c.history().person(*p).unwrap().location,
                Location::InRegion(r0),
                "ended {ended}: {p:?}'s body is back in the history"
            );
        }
        assert_eq!(c.online_on(A), None, "ended {ended}: A went offline with its host");
        assert!(c.host_mentions(h(1)).contains(&"succeeded"), "ended {ended}: host 1's boot is a dead one");
    }
}

/// Elastic 2a final review (I1): the calendar never moves backwards across a load. A running
/// coordinator writes no transition for a calendar advance alone (decision 7), so the stored
/// history's `now` lags the calendar the hosts were told (its day turns, its views). Loaded
/// after a long downtime, the calendar re-anchors where it last stood — never at the stale
/// history's `now` — and a view handed out between two day turns is durable before it leaves.
#[test]
fn the_calendar_never_runs_backwards_across_a_load() {
    const T0: u64 = 1_763_000_000_000;
    let (hist, _, _) = world();
    let mut c = Coordinator::new(hist, T0);
    let mut history = c.take_transition().expect("a new core writes").history.expect("its history");
    c.transition_written(c.version() + 1);
    let mut now = T0;
    let mut told = c.history().now();
    let mut last = None;
    // A minute of play, one second a step, every transition written: no history write.
    for _ in 0..60 {
        now += 1_000;
        c.maintain(now);
        if let Some(t) = c.take_transition() {
            if let Some(h) = &t.history {
                history = h.clone();
            }
            c.transition_written(c.version() + 1);
            for (_, m) in &t.out {
                if let CoordMsg::Calendar(at) = m {
                    told = told.max(*at);
                }
            }
            last = Some(t);
        }
        c.take_out();
    }
    // Loaded from the last day turn's transition an hour later: no earlier than that day turn.
    let turned = last.expect("days turned");
    assert!(turned.history.is_none(), "a calendar advance alone writes no history");
    let mut c1 = reload(&turned, &history, ReplicaId(1), now + 3_600_000);
    // The stored history lags the day turns told (below, `stored_now`); loaded in a gap, the
    // history stands where the calendar last stood at once (Elastic 2b), not one pass later.
    assert!(c1.history().now() >= told, "{:?} < {told:?}: the loaded history lags the calendar", c1.history().now());
    assert!(c1.calendar_now(now + 3_600_000) >= told, "{:?} < {told:?}: back to the stored history", c1.calendar_now(now + 3_600_000));
    c1.maintain(now + 3_600_000);
    assert!(c1.history().now() >= told);
    // A host attaches between two day turns: its view carries the calendar of now.
    now += 4_000;
    c.handle(h(0), attach(h(0), 1, Vec::new(), Vec::new()), now);
    let t = c.take_transition().expect("an attach changes the projection");
    c.transition_written(c.version() + 1);
    if let Some(h) = &t.history {
        history = h.clone();
    }
    for (_, m) in &t.out {
        if let CoordMsg::View { calendar, .. } = m {
            told = told.max(*calendar);
        }
    }
    let stored_now = game::coordinator::Coordinator::load(&t.core, &history, grid(), ReplicaId(1), now)
        .expect("loads")
        .history()
        .now();
    assert!(stored_now < told, "the stored history lags what the hosts were told ({stored_now:?} < {told:?})");
    // Loaded an hour later: re-anchored where the calendar last stood, not caught up.
    let later = now + 3_600_000;
    let mut c2 = reload(&t, &history, ReplicaId(2), later);
    assert!(c2.calendar_now(later) >= told, "{:?} < {told:?}: the calendar ran backwards", c2.calendar_now(later));
    c2.maintain(later);
    let at = c2.history().now();
    assert!(at >= told, "{at:?} < {told:?}: the calendar ran backwards");
    assert!(at.0 <= told.0 + TICKS_PER_DAY, "{at:?}: downtime is not caught up (told {told:?})");
    c2.handle(h(0), attach(h(0), 2, Vec::new(), Vec::new()), later + 10);
    let out = commit(&mut c2);
    for m in &out {
        if let CoordMsg::View { calendar, .. } | CoordMsg::Calendar(calendar) = m {
            assert!(*calendar >= told, "{calendar:?} < {told:?}: a host was told an earlier calendar");
        }
    }
}

/// Elastic 2a final review (I2): the first host attaching at a higher world generation finds
/// every host of an older one gone — those still attached (their links not dropped yet) are
/// detached: none is the gate host, an heir, or a participant a cut waits for.
#[test]
fn the_first_attach_of_a_new_generation_detaches_the_older_hosts() {
    let mut c = succeeding(3);
    assert_eq!(c.gate_host(), Some(h(0)));
    let mut attach = claim_attach(h(5), 50, Vec::new(), Vec::new());
    if let HostMsg::Attach { generation, .. } = &mut attach {
        *generation = 1;
    }
    c.handle(h(5), attach, 10);
    // A newer generation's attach reads the open successions again (none).
    assert!(c.take_successions_reread());
    c.successions_open(Vec::new(), 10);
    commit_routed(&mut c);
    assert_eq!(c.gate_host(), Some(h(5)), "no host of the older generation is the gate");
    c.set_expected_hosts([h(5)].into_iter().collect());
    c.maintain(10 + 10 * CUT_EVERY_MS);
    let out = commit_routed(&mut c);
    assert_eq!(begun(&out).iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), vec![h(5)], "a cut of the new generation's host alone");
    // Host 5 lapses: no old host inherits.
    let rows = vec![seen(h(5), 0)];
    c.observe_leases(&rows, 20 * CUT_EVERY_MS, 20 * CUT_EVERY_MS);
    assert_eq!(c.declare_wanted(20 * CUT_EVERY_MS), None, "no live host of the new generation: nothing to declare");
}

/// Elastic 2a final review (G1): no transition is taken while one is being written.
#[test]
fn no_second_transition_while_one_is_written() {
    let (mut c, _civ, _people) = coordinator();
    let site = site_of(&c);
    c.handle(h(0), adventure(1, A, site), 0);
    let t = c.take_transition().expect("the grant is pending");
    c.handle(h(0), adventure(2, B, site), 0);
    assert!(c.take_transition().is_none(), "one at a time");
    c.transition_written(c.version() + 1);
    let next = c.take_transition().expect("the second grant, after the first was answered");
    assert!(replies(&next.out.iter().map(|(_, m)| m.clone()).collect::<Vec<_>>()).iter().any(|(id, _)| *id == rid(2)));
    drop(t);
}

/// Elastic 2a final review (G2): what the projection keeps per host id — an attachment, the
/// boots its `Attach`es named — goes with the id once a `hosts` read no longer shows it (the
/// id is never issued again): a crash-looping pod across coordinator restarts never grows the
/// core.
#[test]
fn an_absent_id_leaves_no_attachment_or_boot_in_the_projection() {
    let mut c = succeeding(3);
    let (core, history) = c.encode_projection();
    let mut c = Coordinator::load(&core, &history, grid(), ReplicaId(9), 100).expect("loads");
    c.apply(Input::ReplicaJoined { me: ReplicaId(9), alone: true }, 100);
    c.set_crash_policy(game::CrashPolicy::Succession);
    c.handle(h(0), claim_attach(h(0), 70, Vec::new(), Vec::new()), 100);
    commit_routed(&mut c);
    // `{:#?}` prints a host id over three lines: compared without the whitespace.
    let names_host_2 = |c: &Coordinator| c.projection_debug().split_whitespace().collect::<String>().contains("HostId(2,)");
    assert!(names_host_2(&c), "host 2's attachment and boots are in the projection");
    let rows: Vec<HostSeen> = [0, 1].into_iter().map(h).map(|o| seen(o, 201)).collect();
    c.observe_leases(&rows, 200, 200);
    commit_routed(&mut c);
    assert!(!names_host_2(&c), "nothing of host 2 is kept: {}", c.projection_debug());
}

// ---- Elastic 2b: replicas over one row --------------------------------------------------

/// The `coordinator` row as replicas share it: the version a compare-and-set writes against.
struct Row {
    version: u64,
    core: Vec<u8>,
    history: Vec<u8>,
}

impl Row {
    /// The row `c` committed (its projection as it stands, at its version).
    fn of(c: &Coordinator) -> Row {
        let (core, history) = c.encode_projection();
        Row { version: c.version(), core, history }
    }

    /// A replica `id` reading the row.
    fn replica(&self, id: u64) -> Coordinator {
        let mut c = Coordinator::load(&self.core, &self.history, grid(), ReplicaId(id), 0).expect("the row loads");
        c.set_version(self.version);
        c
    }

    /// `c`'s pending transition written with a compare-and-set: what may leave once it
    /// committed (or, with nothing durable pending, what is free); `Err`: a conflict — nothing
    /// written, the transition the caller's to drop.
    fn write(&mut self, c: &mut Coordinator) -> Result<Vec<(Target, CoordMsg)>, Transition> {
        let Some(t) = c.take_transition() else { return Ok(c.take_out()) };
        if c.version() != self.version {
            return Err(t);
        }
        self.version += 1;
        self.core = t.core.clone();
        if let Some(h) = &t.history {
            self.history = h.clone();
        }
        c.transition_written(self.version);
        let mut out = t.out;
        out.extend(c.take_out());
        Ok(out)
    }

    /// `c` rebased onto the row.
    fn rebase(&self, c: &mut Coordinator) {
        c.rebase(&self.core, Some(&self.history), self.version).expect("the row loads");
    }
}

/// A `StartAdventure` of `player` at `site` from host 1.
fn adventure_on_1(seq: u64, player: PlayerId, site: SiteId) -> HostMsg {
    HostMsg::Request { id: RequestId { host: h(1), boot: 7, seq }, player, hop: PLACED, req: start_request(site) }
}

/// Elastic 2b (decision 1): replica A applies i1..i3 while replica B, from the same row,
/// applies j1 and commits; A's write conflicts. Rebased onto B's row, A holds exactly what one
/// core applying j1 and then i1..i3 holds — the projection's bytes and what leaves — and its
/// replay re-decided: B's claim, granted on A's stale cache, is refused on the newer row.
#[test]
fn a_rebase_replays_the_log_onto_the_newer_row() {
    let (base, _, _) = coordinator();
    let site = site_of(&base);
    let mut row = Row::of(&base);
    let (mut a, mut b, mut one) = (row.replica(1), row.replica(2), row.replica(1));
    let inputs: Vec<(u64, Input)> = vec![
        (20, Input::Host { host: h(0), msg: adventure(1, A, site) }),
        (30, Input::Maintain),
        (40, Input::Host { host: h(0), msg: adventure(2, B, site) }),
    ];
    for (now, input) in &inputs {
        a.apply(input.clone(), *now);
    }
    b.apply(Input::Host { host: h(1), msg: adventure_on_1(1, B, site) }, 10);
    row.write(&mut b).expect("B writes first");
    let _lost = row.write(&mut a).expect_err("A's write conflicts: the row moved");
    row.rebase(&mut a);
    one.apply(Input::Host { host: h(1), msg: adventure_on_1(1, B, site) }, 10);
    one.take_transition().expect("j1 is durable");
    one.transition_written(row.version);
    for (now, input) in inputs {
        one.apply(input, now);
    }
    assert!(a.encode_projection() == one.encode_projection(), "the projections differ:\n{}\n{}", a.projection_debug(), one.projection_debug());
    let (ta, to) = (a.take_transition().expect("A's inputs changed the newer row"), one.take_transition().expect("pending"));
    assert_eq!(ta.out, to.out, "the same messages leave");
    let msgs: Vec<CoordMsg> = ta.out.into_iter().map(|(_, m)| m).collect();
    assert!(reply_to(&msgs, 1).grants_claim(), "A's start is granted");
    assert_eq!(reply_to(&msgs, 2), WorldReply::AdventureRefused(AdventureRefusal::AlreadyPlaying), "B plays on host 1 already");
}

/// Elastic 2b (decision 3): what a host tells the login service is forwarded as it is and never
/// enters the log — a rebase does not tell it twice.
#[test]
fn a_login_message_is_never_replayed() {
    let (base, _, _) = coordinator();
    let site = site_of(&base);
    let mut row = Row::of(&base);
    let (mut a, mut b) = (row.replica(1), row.replica(2));
    let said = game::LoginOut::SessionEnded { player_token: "token".into(), session_epoch: 3 };
    a.apply(Input::Host { host: h(0), msg: HostMsg::Login(said) }, 10);
    a.apply(Input::Host { host: h(0), msg: adventure(1, A, site) }, 10);
    b.apply(Input::Host { host: h(1), msg: adventure_on_1(1, B, site) }, 10);
    row.write(&mut b).expect("B writes first");
    let _lost = row.write(&mut a).expect_err("A's write conflicts");
    row.rebase(&mut a);
    row.write(&mut a).expect("A writes after its rebase");
    let told = a.take_login_out();
    assert!(matches!(told.as_slice(), [game::LoginOut::SessionEnded { session_epoch: 3, .. }]), "told once: {told:?}");
}

/// Elastic 2b (Review Focus 3, decision 3's clock): two replicas a few milliseconds apart take
/// turns at the upkeep across two day turns — B 3 ms ahead, A 3 ms behind — each on its own
/// cache, rebased onto the other's row (its upkeep replayed) before it writes. Every day turns
/// once, in order, and no replica's calendar day ever goes back. (Task 6: the upkeep is the
/// ticker's — two processes that both believe they tick, as two caches across a handover can.)
#[test]
fn two_replicas_a_tick_apart_turn_each_day_once() {
    let (base, _, _) = coordinator();
    let mut row = Row::of(&base);
    let (mut a, mut b) = (row.replica(1), row.replica(2));
    for c in [&mut a, &mut b] {
        // The ticker of the row (replica 0), as each process believes.
        c.set_replica_for_test(ReplicaId(0));
    }
    let ((anchor_ms, anchor_tick), _) = base.calendar_anchor();
    let first = anchor_tick.0 / TICKS_PER_DAY + 1;
    let turn = |d: u64| anchor_ms + (d * TICKS_PER_DAY - anchor_tick.0) * game::TICK_RATE;
    let mut times: Vec<u64> = (0..).map(|s| s * 1_000).take_while(|t| *t < turn(first + 1) + 1_000).collect();
    for d in [first, first + 1] {
        times.extend((0..13).map(|k| turn(d) + k * 10 - 60));
    }
    times.sort_unstable();
    times.dedup();
    let (mut told, mut highest) = (Vec::new(), 0);
    for t in times {
        for (c, now) in [(&mut b, t + 3), (&mut a, t.saturating_sub(3))] {
            c.apply(Input::Maintain, now);
            if c.version() != row.version {
                row.rebase(c);
            }
            let out = row.write(c).expect("written on a cache as new as the row");
            told.extend(out.into_iter().filter_map(|(_, m)| match m {
                CoordMsg::Calendar(at) => Some(at.0 / TICKS_PER_DAY),
                _ => None,
            }));
            assert!(c.calendar_day() >= highest, "at {now}: the calendar day went back to {} from {highest}", c.calendar_day());
            highest = c.calendar_day();
        }
    }
    assert_eq!(told, vec![first, first + 1], "each day turned once, in order");
}

/// Elastic 2b (decision 3): a host's link to another replica going down changes nothing here —
/// its attachment there stands, the cut it takes part in runs on. On the replica it is
/// attached to, the same link down detaches it and aborts the cut.
#[test]
fn a_link_down_of_a_host_attached_elsewhere_changes_nothing() {
    let (base, _, _) = coordinator();
    let mut row = Row::of(&base);
    let configure = |c: &mut Coordinator| {
        c.set_cut_epoch(3);
        c.set_expected_hosts([0, 1].into_iter().map(h).collect());
        c.set_world_cuts(CUT_EVERY_MS);
        c.set_world_tick(100);
    };
    let mut b = row.replica(2);
    // B is the live replica (Elastic 2b Task 4: an attachment counts while its replica is live),
    // the ticker (Task 6: the cuts are its).
    b.apply(Input::ReplicaJoined { me: ReplicaId(2), alone: true }, 0);
    configure(&mut b);
    b.apply(Input::Host { host: h(0), msg: attach(h(0), 1, Vec::new(), Vec::new()) }, 0);
    b.apply(Input::Host { host: h(1), msg: attach(h(1), 1, Vec::new(), Vec::new()) }, 0);
    b.apply(Input::Maintain, 10 * CUT_EVERY_MS);
    assert!(b.world_cut_running().is_some(), "a cut of both hosts runs");
    row.write(&mut b).expect("B writes");
    let mut a = row.replica(1);
    configure(&mut a);
    let before = a.encode_projection();
    a.apply(Input::LinkDown { host: h(0) }, 10 * CUT_EVERY_MS);
    assert!(a.encode_projection() == before, "nothing changed: {}", a.projection_debug());
    assert_eq!(a.world_cut_progress().map(|(_, _, verdict)| verdict), Some(None), "the cut runs on");
    assert!(a.take_transition().is_none(), "nothing to write");
    b.apply(Input::LinkDown { host: h(0) }, 10 * CUT_EVERY_MS);
    assert_eq!(b.world_cut_progress().map(|(_, _, verdict)| verdict), Some(Some(false)), "aborted where it was attached");
}

// ---- store decisions in the transition (Elastic 2b Task 3) ---------------------------------

/// Elastic 2b Task 3: an answer counts only for the call it answers — its stamp is the mark's.
/// An answer with another stamp (another replica's call, or an older one of this replica's)
/// changes nothing: `declared`, a failed declaration, `landless_ended`, a failed end, and
/// `rolled_back`.
#[test]
fn an_answer_with_another_stamp_changes_nothing() {
    let other = CallStamp { replica: ReplicaId(77), version: 3 };
    let unchanged = |c: &mut Coordinator, a: DecisionAnswer, what: &str| {
        let before = c.encode_projection();
        c.apply(Input::Decision(a), 1_000);
        assert!(c.encode_projection() == before, "{what}: the projection changed: {}", c.projection_debug());
    };

    // A declaration in flight: neither its success nor its failure answered by another stamp.
    let mut c = succeeding(3);
    read_leases(&mut c, 3, &[h(2)], &[], 1_000);
    let d = c.declare_wanted(1_000).expect("a lapsed host is declared");
    let t = written(&mut c).expect("the declaration's transition");
    assert_eq!(t.decisions, vec![StoreDecision::Declare(d.clone())], "the declaration rides in its transition");
    let done = game::Declared::Done(store_declares(&d, 3, 1, game::CutId(0)));
    unchanged(&mut c, DecisionAnswer::Declared { stamp: other, outcome: Ok(done.clone()) }, "declared");
    unchanged(&mut c, DecisionAnswer::Declared { stamp: other, outcome: Err("lock timeout".into()) }, "declare_failed");
    read_leases(&mut c, 3, &[h(2)], &[], 1_100);
    assert_eq!(c.declare_wanted(1_100), None, "still in flight: nothing declared twice");
    let mine = CallStamp { replica: c.replica(), version: c.version() };
    c.apply(Input::Decision(DecisionAnswer::Declared { stamp: mine, outcome: Ok(done) }), 1_200);
    assert_eq!(c.successions_open_count(), 1, "its own answer opens the run");

    // An end in flight.
    let mut c = succeeding(3);
    read_land(&mut c, 3, &[h(2)], &[], &[h(2)], 10);
    let end = c.take_end_landless(10).expect("a landless lapse is ended");
    let t = written(&mut c).expect("the end's transition");
    assert_eq!(t.decisions, vec![StoreDecision::EndLandless(end)]);
    unchanged(&mut c, DecisionAnswer::Ended { stamp: other, end, outcome: Ok(game::LandlessEnd::Ended) }, "landless_ended");
    unchanged(&mut c, DecisionAnswer::Ended { stamp: other, end, outcome: Err("lock timeout".into()) }, "end_failed");
    assert_eq!(c.take_end_landless(20), None, "still in flight");

    // A rollback in flight.
    let mut c = succeeding(3);
    declare_one(&mut c, 3, h(2), 100);
    commit_routed(&mut c);
    c.apply(Input::Decision(DecisionAnswer::Declared { stamp: other, outcome: Ok(game::Declared::SuccessionOpen) }), 150);
    read_land(&mut c, 3, &[h(1)], &[h(2)], &[], 200);
    c.maintain(200);
    assert!(c.rollback_wanted(), "a second landed lapse before the settle");
    let from = c.take_rollback().expect("the rollback is handed out");
    let t = written(&mut c).expect("the rollback's transition");
    assert_eq!(t.decisions, vec![StoreDecision::Rollback { from }]);
    unchanged(&mut c, DecisionAnswer::RolledBack { stamp: other, outcome: Ok(from + 1) }, "rolled_back");
    unchanged(&mut c, DecisionAnswer::RolledBack { stamp: other, outcome: Err("lock timeout".into()) }, "a failed rollback");
    assert!(c.rollback_wanted() && c.take_rollback().is_none(), "the rollback is still in flight");
}

/// Elastic 2b Task 3: a declaration committed with its transition whose answer was lost (the
/// write's answer never came; the core then rebased onto the row) is looked up — the open
/// successions are read again — and never handed out a second time (a second declaration
/// would meet the first one's open run and roll the world back).
#[test]
fn a_mark_whose_answer_was_lost_is_looked_up_not_reissued() {
    let mut c = succeeding(3);
    read_leases(&mut c, 3, &[h(2)], &[], 1_000);
    let d = c.declare_wanted(1_000).expect("a lapsed host is declared");
    let t = c.take_transition().expect("the declaration's transition");
    assert_eq!(t.decisions, vec![StoreDecision::Declare(d.clone())]);
    // It committed; its answers were lost on the way back.
    let committed = c.version() + 1;
    c.transition_written(committed);
    assert!(!c.take_successions_reread(), "nothing is looked up while the answer may still come");
    // Another writer moves the row (its projection as written): the core rebases onto it.
    c.rebase(&t.core, t.history.as_deref(), committed + 1).expect("the row loads");
    assert!(c.take_successions_reread(), "the lost answer is looked up: the open successions are read again");
    read_leases(&mut c, 3, &[h(2)], &[], 2_000);
    assert_eq!(c.declare_wanted(2_000), None, "no second declaration before the re-read answers");
    let t = c.take_transition();
    assert!(t.as_ref().is_none_or(|t| t.decisions.is_empty()), "nothing re-issued: {t:?}");
    if t.is_some() {
        c.transition_written(c.version() + 1);
    }
    c.successions_open(store_declares(&d, 3, 1, game::CutId(0)), 2_100);
    read_leases(&mut c, 3, &[h(2)], &[h(2)], 3_000);
    assert_eq!(c.declare_wanted(3_000), None, "nor after it: the declared run is open");
    assert_eq!(c.successions_open_count(), 1, "one run");
    assert!(!c.rollback_wanted(), "never rolled back");
}

// ---- Elastic 2b Task 4: the live set of replicas -----------------------------------------

/// Two replicas joined over one row (`a` = replica 1 committed it first, `b` = replica 2), each
/// rebased onto the row's latest version. The row, `a`, `b`.
fn two_replicas() -> (Row, Coordinator, Coordinator) {
    let (base, _, _) = coordinator();
    let mut row = Row::of(&base);
    let mut a = row.replica(1);
    a.apply(Input::ReplicaJoined { me: ReplicaId(1), alone: true }, 0);
    row.write(&mut a).expect("A writes first");
    let mut b = row.replica(2);
    b.apply(Input::ReplicaJoined { me: ReplicaId(2), alone: false }, 0);
    row.write(&mut b).expect("B writes on A's row");
    row.rebase(&mut a);
    let both: std::collections::BTreeSet<ReplicaId> = [ReplicaId(1), ReplicaId(2)].into_iter().collect();
    assert_eq!(a.replicas_live(), &both, "A's cache holds both");
    assert_eq!(b.replicas_live(), &both, "B's cache holds both");
    (row, a, b)
}

/// Elastic 2b Task 4: an attachment counts while its replica is in the durable live set — on
/// every replica: a host attached to A counts on B too (the gate, the cuts, the heirs) — and
/// counts detached from the transition that drops A (`ReplicaLapsed`, which ends A's row in the
/// same transaction) on.
#[test]
fn an_attachment_of_a_lapsed_replica_counts_detached() {
    let (mut row, mut a, mut b) = two_replicas();
    a.set_crash_policy(game::CrashPolicy::Succession);
    b.set_crash_policy(game::CrashPolicy::Succession);
    a.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 0);
    row.write(&mut a).expect("A's attach commits");
    row.rebase(&mut b);
    b.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 0);
    row.write(&mut b).expect("B's attach commits");
    let one = |h: HostId| -> std::collections::BTreeSet<HostId> { [h].into_iter().collect() };
    assert_eq!(b.hosts_attached_to(ReplicaId(1)), one(h(0)), "host 0 is A's");
    assert_eq!(b.hosts_attached_to(ReplicaId(2)), one(h(1)), "host 1 is B's");
    assert_eq!(b.gate_host(), Some(h(0)), "host 0, attached to A, counts on B");
    assert_eq!(b.ticker(), Some(ReplicaId(1)), "the lowest live replica ticks");

    b.apply(Input::ReplicaLapsed(ReplicaId(1)), 100);
    let t = b.take_transition().expect("the lapse is durable");
    assert_eq!(t.decisions, vec![StoreDecision::EndReplica(ReplicaId(1))], "A's row is ended in the same transaction");
    b.transition_written(b.version() + 1);
    let only_b: std::collections::BTreeSet<ReplicaId> = [ReplicaId(2)].into_iter().collect();
    assert_eq!(b.replicas_live(), &only_b);
    assert_eq!(b.ticker(), Some(ReplicaId(2)), "B ticks now");
    assert_eq!(b.gate_host(), Some(h(1)), "host 0's attachment to the ended replica counts detached");
    assert!(b.hosts_attached_to(ReplicaId(1)).is_empty(), "nothing counts attached to an ended replica");
}

/// Elastic 2b Task 8: a replica relays the login service's orders only to a gate host attached
/// to IT (`gate_host_here`); with none it has no gate (`login_order` is `None`: the runtime
/// answers `NoGate` and the login service tries the next replica), even while another replica's
/// host is the world's gate.
#[test]
fn a_replica_relays_login_orders_only_to_its_own_hosts() {
    let (mut row, mut a, mut b) = two_replicas();
    a.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 0);
    row.write(&mut a).expect("A's attach commits");
    row.rebase(&mut b);
    assert_eq!(b.gate_host(), Some(h(0)), "host 0 is the world's gate");
    assert_eq!(b.gate_host_here(), None, "no host is attached to B");
    assert_eq!(b.login_order(spawn_order(1)), None, "B has no gate: the order is not relayed");
    assert_eq!(a.gate_host_here(), Some(h(0)));
    assert_eq!(a.login_order(spawn_order(1)), Some((h(0), CoordMsg::Login(spawn_order(1)))));

    b.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 0);
    row.write(&mut b).expect("B's attach commits");
    row.rebase(&mut a);
    assert_eq!(b.gate_host_here(), Some(h(1)), "B's own host, though host 0 is lower");
    assert_eq!(b.login_order(spawn_order(2)), Some((h(1), CoordMsg::Login(spawn_order(2)))));
    assert_eq!(a.gate_host_here(), Some(h(0)));
}

/// Elastic 2b Task 4 fix round 1 (I1): replica A dies and C registers beside B before B's watch
/// counted a lease — the registration marks A's row dead, no watch ever sees A standing still.
/// B's watch still names A gone (its row dead), and a replica the core holds whose row was
/// pruned (absent from the read) when its id is below the highest id the read returned; never a
/// replica registered after the read (above that id). Applied, A leaves the live set.
#[test]
fn a_replica_ended_by_a_registration_leaves_the_live_set() {
    let (mut row, _a, mut b) = two_replicas();
    let (a, b_id, c, pruned, newer) = (ReplicaId(1), ReplicaId(2), ReplicaId(3), ReplicaId(0), ReplicaId(9));
    let seen = |r: ReplicaId, live: bool| ReplicaRow { replica: r, seen_ms: 100, live, addr: String::new() };
    let mut watch = ReplicaWatch::default();
    watch.observe(&[seen(a, true), seen(b_id, true)], 0);
    // A dies; C registers beside B at once: A's row is dead (no lease counted by any watch).
    watch.observe(&[seen(a, false), seen(b_id, true), seen(c, true)], 1_000);
    assert!(watch.lapsed(1_000).is_empty(), "nobody stood still for a lease");
    assert!(watch.ends_lapsed(b_id, 1_000), "B is the lowest live");
    let core_live: std::collections::BTreeSet<ReplicaId> = [pruned, a, b_id, newer].into_iter().collect();
    assert_eq!(watch.gone(&core_live, b_id), vec![pruned, a], "A (dead) and the pruned id below the read's highest; never a newer one");
    b.apply(Input::ReplicaLapsed(a), 1_000);
    row.write(&mut b).expect("B ends A");
    assert!(!b.replicas_live().contains(&a), "A left the live set");
    assert_eq!(b.ticker(), Some(b_id), "B ticks");
}

/// Elastic 2b Task 4 fix round 1 (I2): a refresh never moves a cache backwards — a row read
/// before this cache's own later commit (an older version) changes nothing.
#[test]
fn a_refresh_onto_an_older_row_changes_nothing() {
    let (mut row, mut a, _b) = two_replicas();
    let older = Row { version: row.version, core: row.core.clone(), history: row.history.clone() };
    let (_, _, people) = world();
    a.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 100);
    let _ = people;
    row.write(&mut a).expect("A commits");
    let before = a.encode_projection();
    let moved = a.refresh(&older.core, Some(&older.history), older.version).expect("the row loads");
    assert!(!moved, "an older row is no refresh");
    assert!(a.encode_projection() == before, "the projection did not regress");
    assert_eq!(a.version(), row.version);
    let moved = a.refresh(&row.core, None, row.version).expect("the row loads");
    assert!(!moved, "nor the same version");
}

/// Elastic 2b Task 4: a replica whose rebased core lacks its own id (another replica ended it)
/// writes nothing and lets nothing leave — not even what it decided on its stale cache.
#[test]
fn a_replica_missing_from_its_own_rebased_core_writes_nothing() {
    let (mut row, mut a, mut b) = two_replicas();
    b.apply(Input::ReplicaLapsed(ReplicaId(1)), 100);
    row.write(&mut b).expect("B ends A");
    a.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 200);
    let _lost = row.write(&mut a).expect_err("A's write conflicts: B moved the row");
    row.rebase(&mut a);
    assert!(!a.replicas_live().contains(&ReplicaId(1)), "the row ended A");
    assert!(a.evicted(), "A knows it was ended");
    assert!(a.take_transition().is_none(), "A writes nothing");
    assert!(a.take_out().is_empty(), "nothing leaves A");
}

/// Elastic 2b Task 4 (departs 5): with two live replicas a query answered without any durable
/// change (free to leave) waits for the version check: it leaves once the row is read at this
/// cache's version, never before; a row read at another version leaves nothing (the replica
/// rebases first). With one live replica it leaves at once, as before.
#[test]
fn free_outputs_wait_for_the_version_check_with_two_replicas() {
    let (_row, mut a, _b) = two_replicas();
    let (_, _, people) = world();
    a.handle(h(0), request(1, B, WorldRequest::PersonInfo(people[2])), 300);
    assert!(a.take_transition().is_none(), "a query changes nothing durable");
    assert!(a.take_out().is_empty(), "with two replicas nothing leaves before the version check");
    let v = a.version_check_wanted().expect("a check is wanted");
    assert_eq!(v, a.version(), "of this cache's version");
    assert!(a.version_check_wanted().is_none(), "one at a time");
    assert!(!a.version_checked(v + 1), "the row moved: the cache is stale");
    assert!(a.take_out().is_empty(), "a stale cache releases nothing");
    let v = a.version_check_wanted().expect("checked again");
    assert!(a.version_checked(v), "the row is at this cache's version");
    let out: Vec<CoordMsg> = a.take_out().into_iter().map(|(_, m)| m).collect();
    assert!(matches!(reply_to(&out, 1), WorldReply::PersonInfo(_)), "the answer leaves after the check: {out:?}");

    // One replica: at once.
    let (mut c, _, people) = coordinator();
    c.handle(h(0), request(2, B, WorldRequest::PersonInfo(people[2])), 300);
    assert!(c.take_transition().is_none());
    assert!(c.version_check_wanted().is_none(), "no check with one replica");
    let out: Vec<CoordMsg> = c.take_out().into_iter().map(|(_, m)| m).collect();
    assert!(matches!(reply_to(&out, 2), WorldReply::PersonInfo(_)), "{out:?}");
}

// ---- Elastic 2b Task 5: the outbox ---------------------------------------------------------

/// [`two_replicas`] with hosts 0 and 1 attached to A (boots 7, 8) and host 2 to B (boot 9), every
/// attach committed, both caches at the row's version.
fn three_hosts_on_two_replicas() -> (Row, Coordinator, Coordinator) {
    let (mut row, mut a, mut b) = two_replicas();
    a.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 0);
    a.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 0);
    let t = a.take_transition().expect("the attaches are durable");
    assert_eq!(t.attached_now, vec![h(0), h(1)], "the transition names the hosts it attached");
    assert!(t.outbox.is_empty(), "nothing for another replica's host yet: {:?}", t.outbox);
    row.version += 1;
    row.core = t.core;
    a.transition_written(row.version);
    row.rebase(&mut b);
    b.handle(h(2), claim_attach(h(2), 9, Vec::new(), Vec::new()), 0);
    row.write(&mut b).expect("B's attach commits");
    row.rebase(&mut a);
    (row, a, b)
}

/// Elastic 2b Task 5 (decision 5): a message for every host leaves A on A's own links (its hosts
/// 0 and 1) and, for host 2 — attached to B — through the outbox, once, with host 2's boot; a
/// message for one of A's hosts stays direct.
#[test]
fn every_is_fanned_out_per_attached_host() {
    let (_row, mut a, _b) = three_hosts_on_two_replicas();
    let site = site_of(&a);
    a.handle(h(0), adventure(1, A, site), 100);
    let t = a.take_transition().expect("the grant is durable");
    let markers = |m: &CoordMsg| matches!(m, CoordMsg::SiteMarkers(_));
    assert!(t.out.iter().any(|(to, m)| *to == Target::Every && markers(m)), "the markers stay for A's links: {:?}", t.out);
    assert!(t.out.iter().any(|(to, m)| *to == Target::Host(h(0)) && matches!(m, CoordMsg::Reply { .. })), "the reply is A's host's");
    let routed: Vec<(HostId, u64)> = t.outbox.iter().map(|o| (o.host, o.boot)).collect();
    assert_eq!(routed, vec![(h(2), 9)], "one copy, for B's host, at its boot: {:?}", t.outbox);
    assert!(markers(&t.outbox[0].msg));
}

/// Elastic 2b Task 5: a host attached to a replica that is no longer live gets nothing — no
/// outbox row (no live replica relays it), no fan-out of a message for every host; its next
/// `Attach` to a live replica reconciles.
#[test]
fn a_host_of_no_live_replica_gets_nothing() {
    let (mut row, mut a, mut b) = three_hosts_on_two_replicas();
    // Host 2 has B online (its own report, hop 1).
    b.handle(h(2), HostMsg::PlayerOnline(report(B, None, false)), 10);
    row.write(&mut b).expect("B's report commits");
    row.rebase(&mut a);
    a.apply(Input::ReplicaLapsed(ReplicaId(2)), 50);
    // B moves to host 0 (hop 2): host 2 would be told (`HopIs`), and every host the markers.
    a.handle(h(0), HostMsg::PlayerOnline(OnlinePlayer { player: B, claim: None, claim_pending: false, report: 1, hop: 2 }), 60);
    let site = site_of(&a);
    a.handle(h(0), adventure(1, A, site), 100);
    let t = a.take_transition().expect("the lapse and the grant are durable");
    assert!(t.outbox.is_empty(), "nothing for host 2 through the outbox: {:?}", t.outbox);
    assert!(t.out.iter().all(|(to, _)| *to != Target::Host(h(2))), "nothing for host 2 on A's links: {:?}", t.out);
    assert!(t.out.iter().all(|(_, m)| !matches!(m, CoordMsg::HopIs { .. })), "the HopIs for host 2 is dropped: {:?}", t.out);
    assert!(t.out.iter().any(|(to, m)| *to == Target::Every && matches!(m, CoordMsg::SiteMarkers(_))), "A's own hosts get the markers");
}

/// Elastic 2b Task 5 (audit H3): host 1 — attached to B — prepares a cut A began, before B's cache
/// holds it. B never looks the cut up (the store would record it aborted): it asks for a refresh,
/// lets nothing leave meanwhile, and, rebased onto the row, counts the part. A cut the row does
/// not hold either (the refresh found nothing newer) is looked up as any unknown cut.
#[test]
fn a_cut_answer_for_an_unknown_future_cut_asks_a_refresh_not_a_lookup() {
    let (mut row, mut a, mut b) = two_replicas();
    a.set_cut_epoch(3);
    a.set_expected_hosts([0, 1].into_iter().map(h).collect());
    a.set_world_cuts(CUT_EVERY_MS);
    a.set_world_tick(100);
    a.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 0);
    row.write(&mut a).expect("A's attach");
    row.rebase(&mut b);
    b.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 0);
    row.write(&mut b).expect("B's attach");
    row.rebase(&mut a);
    a.apply(Input::Maintain, 10 * CUT_EVERY_MS);
    let (cut, _, _) = a.world_cut_progress().expect("A began a cut of both hosts");
    let t = a.take_transition().expect("the cut's begin is durable");
    let begun = |m: &CoordMsg| matches!(m, CoordMsg::BeginCut { cut: c, .. } if *c == cut);
    assert!(t.out.iter().any(|(to, m)| *to == Target::Host(h(0)) && begun(m)), "host 0's BeginCut on A's link");
    assert!(t.outbox.iter().any(|o| o.host == h(1) && o.boot == 8 && begun(&o.msg)), "host 1's through the outbox: {:?}", t.outbox);
    row.version += 1;
    row.core = t.core;
    a.transition_written(row.version);

    // Host 1's part reaches B, whose cache is a version behind.
    b.handle(h(1), HostMsg::CutPrepared { cut }, 10 * CUT_EVERY_MS + 5);
    assert!(b.take_cut_lookups().is_empty(), "never looked up");
    assert!(b.take_refresh_wanted(), "a refresh is asked");
    assert!(!b.take_refresh_wanted(), "once");
    assert!(b.take_out().is_empty(), "nothing leaves a cache known stale");
    assert!(b.refresh(&row.core, Some(&row.history), row.version).expect("the row loads"), "the row is newer");
    let (_, prepared, verdict) = b.world_cut_progress().expect("the refreshed cache holds the cut");
    assert_eq!((prepared, verdict), (vec![h(1)], None), "host 1's part counts, the cut runs on");
    assert!(!b.take_refresh_wanted(), "no longer stale");

    // A cut no row holds: looked up (by the ticker: replica 3 registered alone), once the refresh
    // found nothing newer.
    let mut c = row.replica(3);
    c.apply(Input::ReplicaJoined { me: ReplicaId(3), alone: true }, 20 * CUT_EVERY_MS);
    let bogus = game::CutId((3 << 32) | 99);
    c.handle(h(0), HostMsg::CutPrepared { cut: bogus }, 20 * CUT_EVERY_MS);
    assert!(c.take_refresh_wanted());
    assert!(c.take_cut_lookups().is_empty(), "not before the refresh");
    c.refresh_not_newer();
    assert_eq!(c.take_cut_lookups(), vec![(h(0), bogus)], "looked up as any unknown cut");
}

// ---- Elastic 2b Task 6: the ticker alone runs the timed and lease work --------------------

/// A `hosts` read of hosts `0..n` at `now` as the runtime hands it to the core: `dead` vouched
/// (lapsed at once), every other one beating, the next id to issue `n`.
fn hosts_read(n: u32, dead: &[HostId], now: u64) -> Input {
    let rows = (0..u64::from(n)).map(h).map(|o| seen(o, if dead.contains(&o) { 0 } else { now + 1 })).collect();
    Input::HostsRead { rows, next_host_id: u64::from(n), issued_mono: now, received_mono: now, world_tick: 100 }
}

/// Elastic 2b Task 6 (plan departs 2): a replica that is not the ticker runs none of the timed
/// and lease work — its upkeep, a `hosts` read, a successions read and an owners read are refused
/// and not logged, and it hands out no store call (no read, no lookup, no declaration, no end, no
/// cut decision). The ticker, fed the same, does that work.
#[test]
fn a_non_ticker_never_hands_out_timed_work() {
    let (mut row, mut a, mut b) = two_replicas();
    for c in [&mut a, &mut b] {
        c.set_cut_epoch(3);
        c.set_world_cuts(CUT_EVERY_MS);
        c.set_crash_policy(game::CrashPolicy::Succession);
        c.set_world_tick(100);
    }
    for n in 0..3 {
        a.handle(h(n), claim_attach(h(n), 100 + n, Vec::new(), Vec::new()), 0);
    }
    row.write(&mut a).expect("the attaches commit");
    row.rebase(&mut b);
    assert!(a.is_ticker() && !b.is_ticker(), "A, the lowest live replica, ticks");
    // A host wants land on B: an input any replica applies (queued in the projection).
    b.handle(h(1), HostMsg::LandWanted { sr: (7, 0) }, 10);
    let logged = b.logged();
    let before = b.encode_projection();
    let late = 10 * CUT_EVERY_MS;
    b.apply(Input::Maintain, late);
    b.apply(hosts_read(3, &[h(2)], late), late);
    b.apply(Input::SuccessionsOpen { list: Vec::new(), issued_at_version: b.version() }, late);
    b.apply(Input::LandOwners { owners: vec![h(5)] }, late);
    assert_eq!(b.logged(), logged, "nothing of the ticker's work is logged on B");
    assert!(b.encode_projection() == before, "nor applied: {}", b.projection_debug());
    assert!(b.world_cut_running().is_none(), "no cut begins on B");
    assert!(!b.take_successions_reread(), "B reads no successions");
    assert!(b.take_land_lookups().is_empty(), "B looks no land up");
    assert!(b.take_cut_lookups().is_empty(), "B looks no cut up");
    assert_eq!(b.declare_wanted(late), None, "B declares nobody");
    assert_eq!(b.take_end_landless(late), None, "B ends nobody");
    assert_eq!(b.take_decision(), None, "B decides no cut");
    let t = b.take_transition();
    assert!(t.as_ref().is_none_or(|t| t.decisions.is_empty()), "B issues no store decision: {t:?}");

    // The ticker, fed the same, does that work.
    assert!(a.take_successions_reread(), "A owes a read of the open successions");
    a.apply(Input::SuccessionsOpen { list: Vec::new(), issued_at_version: a.version() }, late);
    a.apply(hosts_read(3, &[h(2)], late), late);
    let d = a.declare_wanted(late).expect("A declares host 2");
    assert_eq!(d.dead, vec![(h(2), 102)]);
}

/// Elastic 2b Task 6 (audit H5): a successions read abandons a run it does not list only if the
/// run opened at or before the version the read was issued at — a run opened later (its store
/// row committed after the read began, perhaps by another replica's transition) is not in the read
/// and is no abandoned run.
#[test]
fn a_reread_abandons_only_runs_opened_before_it() {
    let mut c = succeeding(3);
    let issued = c.version();
    read_leases(&mut c, 3, &[h(2)], &[], 1_000);
    let d = c.declare_wanted(1_000).expect("host 2 is declared");
    written(&mut c).expect("the declaration commits with its transition");
    let mine = CallStamp { replica: c.replica(), version: c.version() };
    let list = store_declares(&d, 3, 1, game::CutId(0));
    c.apply(Input::Decision(DecisionAnswer::Declared { stamp: mine, outcome: Ok(game::Declared::Done(list)) }), 1_100);
    commit_routed(&mut c);
    assert_eq!(c.successions_open_count(), 1, "the run is open");
    c.apply(Input::SuccessionsOpen { list: Vec::new(), issued_at_version: issued }, 1_200);
    assert_eq!(c.successions_open_count(), 1, "a read issued before the run opened abandons nothing");
    assert!(succession_done(&commit_routed(&mut c)).is_empty(), "nobody is told");
    c.apply(Input::SuccessionsOpen { list: Vec::new(), issued_at_version: c.version() }, 1_300);
    assert_eq!(c.successions_open_count(), 0, "a read issued after it that does not list it: abandoned");
    assert_eq!(succession_done(&commit_routed(&mut c)), vec![(Target::Every, game::SuccessionId(1), false)]);
}

/// Elastic 2b Task 6 fix round 1: a successions read out across a rebase is stale — the rebased
/// row may have closed a run the read still lists. The ticker reads at v (run R open); another
/// replica closes R at v + 1; the ticker's write conflicts and it rebases. A new read is owed, and
/// the old read's answer, listing R, never opens R again.
#[test]
fn a_read_out_across_a_rebase_never_reopens_a_closed_run() {
    let mut c = succeeding(3);
    read_leases(&mut c, 3, &[h(2)], &[], 1_000);
    let d = c.declare_wanted(1_000).expect("host 2 is declared");
    written(&mut c).expect("the declaration commits");
    let mine = CallStamp { replica: c.replica(), version: c.version() };
    let list = store_declares(&d, 3, 1, game::CutId(0));
    c.apply(Input::Decision(DecisionAnswer::Declared { stamp: mine, outcome: Ok(game::Declared::Done(list.clone())) }), 1_100);
    commit_routed(&mut c);
    assert_eq!(c.successions_open_count(), 1, "run R is open");
    let mut row = Row::of(&c);
    // The ticker hands a read out at v.
    c.successions_reread();
    assert!(c.take_successions_reread(), "a read is out");
    let issued_at = c.version();
    // Another replica closes R at v + 1 (a rollback abandons it).
    let mut b = row.replica(2);
    b.rolled_back(c.world_generation());
    row.write(&mut b).expect("B closes R");
    // The ticker's write conflicts; it rebases onto the row that closed R.
    c.handle(h(1), HostMsg::LandWanted { sr: (7, 0) }, 1_200);
    let _lost = row.write(&mut c).expect_err("the ticker's write conflicts");
    row.rebase(&mut c);
    assert_eq!(c.successions_open_count(), 0, "the row closed R");
    assert!(c.take_successions_reread(), "a new read is owed: the one out may predate the close");
    // The old read's answer arrives anyway: it lists R.
    c.apply(Input::SuccessionsOpen { list, issued_at_version: issued_at }, 1_300);
    assert_eq!(c.successions_open_count(), 0, "a read issued before the rebase never re-opens R");
}

/// Elastic 2b Task 6 (audit H7): a `hosts` read forgets an absent id only if the read could have
/// shown it (below the read's next id: it was pruned) and no live replica's attachment holds it —
/// never a host attached to another replica, nor one registered after the read was issued.
#[test]
fn a_hosts_read_forgets_no_host_attached_to_another_replica() {
    let (mut row, mut a, mut b) = two_replicas();
    b.handle(h(0), claim_attach(h(0), 7, Vec::new(), Vec::new()), 0);
    row.write(&mut b).expect("host 0 attaches to B");
    row.rebase(&mut a);
    a.handle(h(1), claim_attach(h(1), 8, Vec::new(), Vec::new()), 0);
    a.handle(h(3), claim_attach(h(3), 9, Vec::new(), Vec::new()), 0);
    a.apply(Input::LinkDown { host: h(3) }, 0);
    row.write(&mut a).expect("hosts 1 and 3 attach to A, host 3's link drops");
    assert!(a.is_ticker());
    let read = |next: u64, at: u64| Input::HostsRead {
        rows: vec![seen(h(1), at + 1)],
        next_host_id: next,
        issued_mono: at,
        received_mono: at,
        world_tick: 100,
    };
    a.apply(read(3, 50), 50);
    assert!(a.host_mentions(h(3)).contains(&"boots"), "host 3 registered after the read was issued: kept");
    assert!(a.host_mentions(h(0)).contains(&"boots"), "host 0, attached to B, is kept");
    assert_eq!(a.hosts_attached_to(ReplicaId(2)), [h(0)].into_iter().collect(), "and stays B's");
    a.apply(read(4, 60), 60);
    assert_eq!(a.host_mentions(h(3)), Vec::<&str>::new(), "below the read's next id and attached nowhere: pruned");
    assert!(a.host_mentions(h(0)).contains(&"boots"), "host 0 is still B's");
    a.apply(Input::ReplicaLapsed(ReplicaId(2)), 70);
    a.apply(read(4, 80), 80);
    assert_eq!(a.host_mentions(h(0)), Vec::<&str>::new(), "attached to an ended replica: forgotten");
}

/// Elastic 2b Task 6 (plan decision 3, `Maintain`): B believed replica 0 lapsed and ticked on its
/// own cache — its upkeep turned the calendar's day. A (a lower id) committed first; B's write
/// conflicts and its rebase onto A's row finds A the ticker: the logged upkeep is dropped, its day
/// turn is not replayed, and nobody is told the day by B.
#[test]
fn a_rebase_that_loses_the_ticker_drops_its_maintain() {
    let (base, _, _) = coordinator();
    let mut row = Row::of(&base);
    let ((anchor_ms, anchor_tick), _) = base.calendar_anchor();
    let day = anchor_tick.0 / TICKS_PER_DAY + 1;
    let turn = anchor_ms + (day * TICKS_PER_DAY - anchor_tick.0) * game::TICK_RATE;
    let mut b = row.replica(2);
    b.apply(Input::ReplicaJoined { me: ReplicaId(2), alone: false }, 0);
    b.apply(Input::ReplicaLapsed(ReplicaId(0)), 0);
    assert!(b.is_ticker(), "B ticks on its own cache");
    b.apply(Input::Maintain, turn + 10);
    assert_eq!(b.calendar_day(), day, "B's upkeep turned the day");
    let logged = b.logged();
    let mut a = row.replica(1);
    a.apply(Input::ReplicaJoined { me: ReplicaId(1), alone: false }, 0);
    a.apply(Input::ReplicaLapsed(ReplicaId(0)), 0);
    row.write(&mut a).expect("A commits first");
    let _lost = row.write(&mut b).expect_err("B's write conflicts");
    row.rebase(&mut b);
    assert_eq!(b.ticker(), Some(ReplicaId(1)), "A ticks on the rebased row");
    assert!(!b.is_ticker());
    assert_eq!(b.logged(), logged - 1, "B's upkeep left its log");
    assert!(b.calendar_day() < day, "its day turn is not replayed");
    let out = row.write(&mut b).expect("B writes on A's row");
    assert!(out.iter().all(|(_, m)| !matches!(m, CoordMsg::Calendar(_))), "B tells nobody the day: {out:?}");
}
