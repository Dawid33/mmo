//! Entities crossing between region hosts (`world_manager/peers.rs`) under seeded
//! scheduling jitter and link delay ([`MultiRig`]): every scenario runs in lockstep with
//! every link delay 0..=3, then `SEEDS` seeds of each schedule mode (`shuffled`, `late`,
//! `jittery`, the link delay rolling through 0..=3 with the seed). Every round checks
//! conservation across hosts ([`MultiRig::assert_conserved_world`]); a failure names a
//! replayable `(mode, seed, delay)`. Its tests are tagged slow: `just test-full`.
use crate::common::h;
use game::{HostId, InlineSchedule, RegionCoords, FLIP_HYSTERESIS, REGION_SIZE};

use crate::common::multi_rig::{host_ids, host_seam, MultiRig};

/// Seeds swept per schedule mode.
const SEEDS: u64 = 12;
/// The link delays swept (most pumps a message waits).
const MAX_LINK_DELAY: u32 = 3;
/// Round budget for one crossing to land and be acknowledged.
const CROSS_ROUNDS: usize = 60;
/// Rounds a link stays down, or a crossing waits, while a test watches it wait.
const WAIT_ROUNDS: usize = 20;
/// How far past a seam a test puts a body (units): past the flip hysteresis.
const PAST_SEAM: f32 = FLIP_HYSTERESIS + 2.0;

type Mode = (&'static str, fn(u64) -> InlineSchedule);
const MODES: [Mode; 3] =
    [("shuffled", InlineSchedule::shuffled), ("late", InlineSchedule::late), ("jittery", InlineSchedule::jittery)];

/// Run `scenario` on a fresh `hosts`-host rig (`s` regions per super-region side) in
/// lockstep at every link delay, then under every mode for every seed.
fn sweep(name: &str, hosts: u32, s: u32, seeds: u64, scenario: fn(&mut MultiRig)) {
    let run = |mode: &str, schedule: Option<fn(u64) -> InlineSchedule>, seed: u64, delay: u32| {
        let ok = std::panic::catch_unwind(|| {
            let mut rig = MultiRig::new(hosts, s, schedule, seed, delay);
            scenario(&mut rig);
        });
        if ok.is_err() {
            panic!("{name} broke under `{mode}`, seed {seed}, link delay {delay}");
        }
    };
    for delay in 0..=MAX_LINK_DELAY {
        run("lockstep", None, 0, delay);
    }
    for (mode, schedule) in MODES {
        for seed in 0..seeds {
            run(mode, Some(schedule), seed, (seed % u64::from(MAX_LINK_DELAY + 1)) as u32);
        }
    }
}

/// A spot in `from`'s frame just past its seam with the adjacent `to`, `along` units
/// along that seam.
fn past_seam(from: RegionCoords, to: RegionCoords, along: f32) -> [f32; 3] {
    let across = |d: i32| match d {
        1 => Some(REGION_SIZE + PAST_SEAM),
        -1 => Some(-PAST_SEAM),
        _ => None,
    };
    let x = across(to.x - from.x).unwrap_or(along);
    let z = across(to.z - from.z).unwrap_or(along);
    [x, crate::common::above_floor_y(), z]
}

// --- Halo and intents between hosts (Task 6) -------------------------------------

/// Rounds (at most `budget`) until `done`; whether it came.
fn until(rig: &mut MultiRig, budget: usize, mut done: impl FnMut(&mut MultiRig) -> bool) -> bool {
    for _ in 0..budget {
        rig.round();
        if done(rig) {
            return true;
        }
    }
    false
}

// --- Fix round 1 -----------------------------------------------------------------

// --- Players crossing between hosts (Task 7) ---------------------------------------

/// The test player's session id (the same on every host) and player.
const P_CLIENT: game::ClientId = 7;
const P_PLAYER: game::PlayerId = game::PlayerId(100);
/// Round budget for the test player's restore to give it a body.
const BODY_ROUNDS: usize = 20;
/// Round budget for the held-key walk to reach the other host.
const WALK_ROUNDS: usize = 240;

/// The test player's record: standing at `xz` (`rc`'s frame) in `rc`.
fn record_at(rc: RegionCoords, xz: [f32; 2]) -> game::PlayerRecord {
    let y = crate::common::standing_y(crate::common::FLOOR_TOP);
    game::PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(xz[0], y, xz[1])),
        region: rc,
        adventure: None,
    }
}

/// Rounds until the test player has a body; its host, region and id.
fn until_body(rig: &mut MultiRig) -> (HostId, RegionCoords, game::EntityId) {
    for _ in 0..BODY_ROUNDS {
        if let Some(body) = rig.player_body(P_CLIENT) {
            return body;
        }
        rig.round();
    }
    panic!("the test player never got a body");
}

/// The packets host `h` sent the test player since recording began.
fn packets_to(rig: &MultiRig, h: HostId) -> Vec<&game::ServerPacket> {
    let seen = rig.hosts[h.0 as usize].packets_seen.as_ref().expect("recording packets");
    seen.iter().filter(|(to, _)| *to == Some(P_CLIENT)).map(|(_, p)| p).collect()
}


// --- Fix round 1: a hand-off never loses or doubles a player ------------------------

// --- Moves into another host's region (Task 8) ----------------------------------------

// --- Fix round 1 ------------------------------------------------------------------------

// --- The coordinator serves every host (Task 10) ------------------------------------

/// The history's map: land from (-16, -16), 64 x 64 regions (the rig's regions near the
/// origin and its keepers' homes near (32, 32) all on it).
fn rig_world_map() -> game::WorldMap {
    let mut m = crate::history_handover::land_map(64, 64);
    m.origin = RegionCoords::new(-16, -16);
    m
}

// --- The coordinated world cut (Task 13) -------------------------------------------

/// The coordinator's world-cut period in the rig: one every 20 rounds of the hosts' clocks.
const WORLD_CUT_EVERY_MS: u64 = 20 * game::TICK_RATE;
/// Round budget for the world cuts a test waits for.
const WORLD_CUT_ROUNDS: usize = 600;

/// A history with nobody in it, on the rig's map.
fn quiet_history() -> game::history::History {
    use game::history::{History, SocietyGrid};
    History::new(9, crate::history_handover::start(), SocietyGrid::from_world_map(&rig_world_map()))
}

/// The rig's shared coordinator over `history`, world cuts every [`WORLD_CUT_EVERY_MS`].
fn world_cuts_over(rig: &mut MultiRig, history: game::history::History) {
    rig.with_coordinator(history);
    rig.with_world_cuts(WORLD_CUT_EVERY_MS);
}

fn world_cuts(rig: &mut MultiRig) {
    world_cuts_over(rig, quiet_history());
}

/// World cuts committed so far.
fn committed(rig: &MultiRig) -> usize {
    rig.decided.values().filter(|c| **c).count()
}

/// Cut rounds until `more` further world cuts committed.
fn until_committed(rig: &mut MultiRig, more: usize) {
    let goal = committed(rig) + more;
    for _ in 0..WORLD_CUT_ROUNDS {
        if committed(rig) >= goal {
            return;
        }
        rig.cut_round();
    }
    panic!("{} world cuts committed of {goal}; decisions {:?}", committed(rig), rig.decided);
}

// --- Every host leaving (Task 14b) ---------------------------------------------------

// --- Login placement across hosts (Task 15) ------------------------------------------

// --- Task 15 fix round 1 -----------------------------------------------------------


// --- R4 Task 4: a super-region with no owner this host knows ---------------------------

/// Directory rows laid out as `static` would for the rig's hosts over every super-region
/// within reach of the test regions and the keepers' homes (`s` = 1: a super-region is a
/// region), at epoch 1 — but `free` as free rows and `unknown` with no row at all.
fn rendezvous_rows(
    rig: &MultiRig,
    free: &[game::SuperRegion],
    unknown: &[game::SuperRegion],
) -> Vec<(game::SuperRegion, Option<HostId>, u64)> {
    let hosts = rig.hosts.len() as u32;
    let mut rows = Vec::new();
    for sx in -16..=48 {
        for sz in -16..=48 {
            let sr = (sx, sz);
            if unknown.contains(&sr) {
                continue;
            }
            let owner = (!free.contains(&sr)).then(|| crate::common::rendezvous_owner(sr, hosts));
            rows.push((sr, owner, 1));
        }
    }
    rows
}

/// The super-region of `rc` in the rig (`s` regions per side).
fn sr_of(rig: &MultiRig, rc: RegionCoords) -> game::SuperRegion {
    game::super_region_of(rc, rig.s)
}

// --- R4 Task 5 / Elastic 3: land assigned on demand, `Owns` --------------------------------

/// The host the coordinator assigns free `sr` to while it assigned nothing else lately: among
/// the live hosts that are not draining, the fewest players, then the fewest regions (the rig's
/// [`MultiRig::loads`]), then the rendezvous weight (`pick_assignee`).
fn least_loaded(rig: &MultiRig, sr: game::SuperRegion) -> HostId {
    let candidates: Vec<HostId> =
        host_ids(rig.hosts.len() as u32).filter(|h| !rig.dead.contains(h) && !rig.draining.contains(h)).collect();
    let key = |h: &HostId| {
        let l = rig.loads.get(h).cloned().unwrap_or_default();
        (l.players, l.regions)
    };
    let least = candidates.iter().map(key).min().expect("a candidate");
    game::rendezvous_among(sr, candidates.iter().copied().filter(|h| key(h) == least)).expect("a candidate")
}

/// A load that makes a host the busiest of the rig's.
const BUSY: game::HostLoad = game::HostLoad { players: 5, regions: 5, tick_permille: 0, land: Vec::new() };

/// A rig whose free land is assigned by a coordinator: the directory `rows`, the rig's
/// `super_regions` table behind it, the coordinator over a quiet history.
fn assigning_world(rig: &mut MultiRig, rows: Vec<(game::SuperRegion, Option<HostId>, u64)>) {
    rig.with_directory(rows);
    rig.with_assignment();
    rig.with_coordinator(quiet_history());
}

/// A client whose only connection is its home walks from `a` into `b`, whose super-region
/// is free, having asked its home for `b` while nobody owned it (the subscribe waits).
/// Another host is assigned `b` and the body is handed there: the home must have named that
/// host for `b` (`RegionElsewhere`) by then — the client has no other way to find the host
/// its player is on now, and would be parked there for want of a session.
fn told_where_its_body_went(rig: &mut MultiRig) {
    let hosts = rig.hosts.len() as u32;
    let (a, b) = host_seam(hosts, 1);
    let sr = sr_of(rig, b);
    assigning_world(rig, rendezvous_rows(rig, &[sr], &[]));
    let ha = rig.owner(a);
    // The home is the busiest: the land goes to another host.
    rig.loads.insert(ha, BUSY);
    let owner = least_loaded(rig, sr);
    assert_ne!(owner, ha);
    rig.run(a);
    rig.connect_home(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    until_body(rig);
    rig.record_packets();
    rig.hosts[ha.0 as usize].event(subscribe_packet(b));
    // Pushed over the seam until its body is handed to the owner of `b` — in `b` there, or
    // parked there for want of a session (the rig's client follows no `RegionElsewhere`).
    let mut handed = false;
    for _ in 0..WALK_ROUNDS {
        match rig.player_body(P_CLIENT) {
            Some((h, rc, _)) if rc == b && h == owner => handed = true,
            Some((_, rc, id)) if rc == a => {
                rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0));
            }
            None if rig.parked_on(P_PLAYER) == [owner] => handed = true,
            _ => {}
        }
        if handed {
            break;
        }
        rig.round();
    }
    assert!(handed, "the body was never handed to host {owner}: {:?}, parked {:?}", rig.player_body(P_CLIENT), rig.parked_on(P_PLAYER));
    let named: Vec<HostId> = elsewhere_for(rig, ha, b).into_iter().map(|(o, _)| o).collect();
    assert!(
        named.contains(&owner),
        "host {ha} handed the body to host {owner} and never named it for {b:?} (named: {named:?}; seed {}, link delay {})",
        rig.seed,
        rig.link_delay
    );
}

/// Regression: client handed into just-claimed land never told the new host, parked after grace; seen in the first live go-nogo-r3 run under claim (fix bdeb8c78 message)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_client_whose_body_is_handed_into_just_claimed_land_is_told_the_host() {
    for hosts in [2, 3] {
        sweep("a_client_whose_body_is_handed_into_just_claimed_land_is_told_the_host", hosts, 1, SEEDS / 4, told_where_its_body_went);
    }
}

// --- Elastic 1 Task 7 / Elastic 3: a draining host is assigned no land -----------------------

// --- Elastic 3 Task 4: assignment races, lost answers, dead assignees, no replica -------------

// --- The peer journal (R4 Task 7) --------------------------------------------------

/// `hosts` (2 or 3) regions in a chain, each adjacent to the next and every one owned by
/// another host (one region per super-region).
fn host_chain(hosts: u32) -> Vec<RegionCoords> {
    let (a, b) = host_seam(hosts, 1);
    if hosts == 2 {
        return vec![a, b];
    }
    let owner = |rc: RegionCoords| crate::common::fixed(HostId::SOLO, hosts).owner_of(rc, 1);
    for (x, y) in [(a, b), (b, a)] {
        for c in [(1, 0), (-1, 0), (0, 1), (0, -1)].map(|(dx, dz)| RegionCoords::new(y.x + dx, y.z + dz)) {
            if owner(c) != owner(x) && owner(c) != owner(y) {
                return vec![x, y, c];
            }
        }
    }
    panic!("no chain of {hosts} hosts at the seam {a:?} | {b:?}");
}

// --- Successions: the survivor's half (R4 Task 8) -------------------------------------

/// A chain of one region per host (`host_chain`), every one running, the hosts answering
/// ownership from a claim directory laid out as `static` would (the rig's `super_regions`
/// table behind it), world cuts running.
fn succession_world(rig: &mut MultiRig) -> Vec<RegionCoords> {
    let chain = host_chain(rig.hosts.len() as u32);
    let rows = rendezvous_rows(rig, &[], &[]);
    rig.with_directory(rows);
    rig.with_assignment();
    for rc in &chain {
        rig.run(*rc);
    }
    world_cuts(rig);
    chain
}

// --- Successions: the heir's half (R4 Task 9) ----------------------------------------

/// Seeds swept per schedule mode by the heir's other tests.
const HEIR_SWEEP_SEEDS: u64 = SEEDS / 2;

/// [`sweep`] over 2 and 3 hosts.
fn sweep_hosts(name: &str, scenario: fn(&mut MultiRig)) {
    for hosts in [2, 3] {
        sweep(&format!("{name} ({hosts} hosts)"), hosts, 1, HEIR_SWEEP_SEEDS, scenario);
    }
}

// --- Fix round 1 -------------------------------------------------------------------

// --- Fix round 2: the heir holds every spawn order until it has adopted ---------------

// --- Drain: a departed host's land goes to an heir when someone needs it (R4 Task 11) ---

// --- Task 11 fix round 1 -----------------------------------------------------------

// --- Task 11 fix round 2 -----------------------------------------------------------

// --- The home answers for regions whose owner changed (R4 Task 16) ---------------------

/// The `RegionElsewhere`s host `h` sent the test player for `rc` since recording began:
/// the host each names, and its ticket.
fn elsewhere_for(rig: &MultiRig, h: HostId, rc: RegionCoords) -> Vec<(HostId, game::ObserverTicket)> {
    packets_to(rig, h)
        .into_iter()
        .filter_map(|p| match p {
            game::ServerPacket::RegionElsewhere { region, endpoint, ticket } if *region == rc => {
                Some((endpoint.id, ticket.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The test player's subscribe of `rc`, as its client sends it.
fn subscribe_packet(rc: RegionCoords) -> game::ServerEvent {
    game::ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(rc), P_CLIENT)
}

// --- Elastic 1 Task 4: peers come and go -------------------------------------------------

/// Elastic 2b Task 10b (the twin's phase 7): a landless host the gate linked to dies, and its
/// row is pruned (another host's registration) before any read of the gate showed it dead.
/// While the gate keeps the peer, every spawn order waits for its sync; a read that does not
/// show it — its id below the read's `next_host_id` — forgets it, and the gate places logins
/// again. An id at or above `next_host_id` (registered after the read was issued) is kept.
fn absent_peer_forgotten(rig: &mut MultiRig) {
    succession_world(rig);
    until_committed(rig, 1);
    let gate = h(0);
    let x = rig.start("x");
    assert!(
        until(rig, CROSS_ROUNDS, |r| {
            let m = &r.hosts[gate.0 as usize].manager;
            m.peer_is_reachable(x) && m.peers_synced()
        }),
        "the gate synced with the new host (seed {}, link delay {})",
        rig.seed,
        rig.link_delay
    );
    rig.kill(x);
    rig.pruned.insert(x);
    for _ in 0..WAIT_ROUNDS {
        rig.cut_round();
    }
    assert!(!rig.hosts[gate.0 as usize].manager.peers_synced(), "a dead peer the gate still keeps is never synced");
    // A read issued before `x` registered: absent, but at the read's next id — kept.
    let before: std::collections::BTreeSet<HostId> = host_ids(x.0 as u32).collect();
    rig.hosts[gate.0 as usize].manager.forget_absent_hosts(&before, x.0);
    assert!(!rig.hosts[gate.0 as usize].manager.is_forgotten(x), "an id at the read's next id may have registered after it");
    // The gate's next read: `x` is absent below its next id — pruned, forgotten.
    rig.read_directory(gate);
    let m = &rig.hosts[gate.0 as usize].manager;
    assert!(m.is_forgotten(x), "the pruned peer is forgotten (seed {}, link delay {})", rig.seed, rig.link_delay);
    assert_eq!(m.mentions(x), Vec::<&str>::new(), "nothing of it is left");
    assert!(m.peers_synced(), "the gate awaits nobody's sync any more");
    assert_eq!(m.peer_state_to_forgotten(), 0, "no state message was owed to it");
}

/// Regression: pruned peer row never forgotten, gate refused every join; seen in the k3d twin's phase 7 run (twin_check on a deployed world, fix b58d2379 message)
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn an_absent_peer_below_the_next_host_id_is_forgotten_and_the_gate_places_again() {
    sweep_hosts("an_absent_peer_below_the_next_host_id_is_forgotten_and_the_gate_places_again", absent_peer_forgotten);
}

// --- Task 4 fix round 1: what makes a cut without the newcomer safe ------------------------

// --- Elastic 1 Task 6: the login service's orders through the coordinator's gate host ---------

// --- Elastic 1 Task 8: the host drains with a warning --------------------------------

// --- Elastic 2a: recipients own land; a landless death is ended ----------------------------

// --- Elastic 3 Task 6: idle land is released in a committed cut --------------------------------

