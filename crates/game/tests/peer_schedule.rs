//! Entities crossing between region hosts (`world_manager/peers.rs`) under seeded
//! scheduling jitter and link delay ([`MultiRig`]): every scenario runs in lockstep with
//! every link delay 0..=3, then `SEEDS` seeds of each schedule mode (`shuffled`, `late`,
//! `jittery`, the link delay rolling through 0..=3 with the seed). Every round checks
//! conservation across hosts ([`MultiRig::assert_conserved_world`]); a failure names a
//! replayable `(mode, seed, delay)`. Run via `just test-threads`.
use game::{InlineSchedule, RegionCoords, RegionSpawner, FLIP_HYSTERESIS, REGION_SIZE};

use crate::common::cut_store::CutStore;
use crate::common::multi_rig::{four_host_corner, host_seam, MultiRig};

/// Seeds swept per schedule mode.
const SEEDS: u64 = 12;
/// The link delays swept (most pumps a message waits).
const MAX_LINK_DELAY: u32 = 3;
/// Round budget for one crossing to land and be acknowledged.
const CROSS_ROUNDS: usize = 60;
/// Rounds a link stays down, or a crossing waits, while a test watches it wait.
const WAIT_ROUNDS: usize = 20;
/// Rounds the source region sits out around the crossing.
const LAG_ROUNDS: u32 = 5;
/// Round budget for a save cut to complete (it may be missed and begun again).
const CUT_ROUNDS: usize = 120;
/// Ticks ahead of the world tick a test's cut is placed.
const CUT_LEAD: u64 = 8;
/// How far past a seam a test puts a body (units): past the flip hysteresis.
const PAST_SEAM: f32 = FLIP_HYSTERESIS + 2.0;
/// How far from a corner a body crossing near it stands (units).
const NEAR_CORNER: f32 = 4.0;

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

/// Rounds until `id` is owned by `rc` on its owner and nothing is left unacknowledged
/// between hosts; panics past [`CROSS_ROUNDS`].
fn until_settled_in(rig: &mut MultiRig, id: game::EntityId, rc: RegionCoords) {
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if rig.owners_of(id) == [(rig.owner(rc), rc)] && rig.peer_unreceived() == 0 {
            return;
        }
    }
    panic!("{id:?} never settled in {rc:?}: owned by {:?}, {} unacknowledged", rig.owners_of(id), rig.peer_unreceived());
}

/// A slime past the seam of the host pair: it lands on the other host, exactly once,
/// and nothing waits on the sending host.
fn cross_once(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    rig.run(a);
    rig.run(b);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    until_settled_in(rig, id, b);
    let source = rig.owner(a) as usize;
    assert_eq!(rig.hosts[source].manager.pending_arrival_total(), 0, "nothing waits on the sending host");
    assert_eq!(rig.hosts[source].manager.in_flight_count(), 0, "nothing in flight on the sending host");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn slime_crosses_host_seam_once() {
    sweep("slime_crosses_host_seam_once", 2, 1, SEEDS, cross_once);
}

/// The target is parked on its host: the transfer waits in the OWNER's mailbox (not the
/// sender's), and is delivered when the region next runs.
fn into_parked(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a) as usize, rig.owner(b) as usize);
    rig.run(a);
    rig.run(b);
    rig.park(b);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    let mut waiting = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if rig.hosts[hb].manager.pending_arrival_count(b) == 1 && rig.peer_unreceived() == 0 {
            waiting = true;
            break;
        }
    }
    assert!(waiting, "the slime never reached the owner's mailbox");
    assert_eq!(rig.hosts[ha].manager.pending_arrival_total(), 0, "the sender holds nothing");
    assert!(!rig.hosts[hb].manager.running_regions().contains(&b), "a slime does not wake its target");
    assert!(rig.owners_of(id).is_empty());
    rig.run(b);
    until_settled_in(rig, id, b);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn transfer_to_a_parked_foreign_region_waits_in_its_mailbox() {
    sweep("transfer_to_a_parked_foreign_region_waits_in_its_mailbox", 2, 1, SEEDS, into_parked);
}

/// The link is down when the slime crosses: the transfer waits in the sender's session,
/// unsent, for as long as the link stays down, and lands once it is back.
fn across_a_down_link(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.link_down(ha, hb);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert!(rig.owners_of(id).is_empty(), "nothing holds the slime but the session: {:?}", rig.owners_of(id));
    let queued = rig.hosts[ha as usize].manager.peer_unreceived_ids();
    assert_eq!(queued, [(hb, id, false)], "the transfer waits unsent in the session to host {hb}");
    rig.link_up(ha, hb);
    until_settled_in(rig, id, b);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn link_down_keeps_transfer_and_delivers_after_up() {
    sweep("link_down_keeps_transfer_and_delivers_after_up", 2, 1, SEEDS, across_a_down_link);
}

/// The source region misses rounds around the crossing (its thread fell behind): it
/// catches up in a burst, the slime departing on the way, conserved every round.
fn lagging_source(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    rig.run(a);
    rig.run(b);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    rig.host_of(a).manager.spawner_mut().hold(a, LAG_ROUNDS);
    until_settled_in(rig, id, b);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lagging_source_host_transfer_conserved() {
    sweep("lagging_source_host_transfer_conserved", 2, 1, SEEDS, lagging_source);
}

/// A spot in `from`'s frame near the corner `corner` (the region whose -x/-z corner it
/// is), just past `from`'s seam with the adjacent `to`.
fn toward_near_corner(from: RegionCoords, to: RegionCoords, corner: RegionCoords) -> [f32; 3] {
    let near = |c: i32, f: i32| if c == f { NEAR_CORNER } else { REGION_SIZE - NEAR_CORNER };
    let [mut x, y, mut z] = past_seam(from, to, 0.0);
    if to.x == from.x {
        x = near(corner.x, from.x);
    }
    if to.z == from.z {
        z = near(corner.z, from.z);
    }
    [x, y, z]
}

/// Round a corner where four hosts meet: the slime is sent on to the next host the
/// round it lands (its last transfer not acknowledged yet), four times, back home.
fn round_the_corner(rig: &mut MultiRig) {
    let block = four_host_corner(1);
    let corner = block[2];
    for rc in block {
        rig.run(rc);
    }
    let id = rig.spawn_slime(block[0], toward_near_corner(block[0], block[1], corner));
    let mut at = 1;
    for _ in 0..4 * CROSS_ROUNDS {
        rig.round();
        let here = block[at % 4];
        if rig.owners_of(id) != [(rig.owner(here), here)] {
            continue;
        }
        if at == 4 {
            break;
        }
        let next = block[(at + 1) % 4];
        assert!(rig.teleport(here, id, toward_near_corner(here, next, corner)), "{here:?} owns the slime");
        at += 1;
    }
    assert_eq!(at, 4, "the slime stopped after {at} of 4 crossings: owned by {:?}", rig.owners_of(id));
    until_settled_in(rig, id, block[0]);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn double_crossing_at_a_four_host_corner() {
    sweep("double_crossing_at_a_four_host_corner", 4, 1, SEEDS / 2, round_the_corner);
}

/// One whole one-phase cut on host `h` (begun again if a region missed it), committed.
fn cut_on(rig: &mut MultiRig, h: usize) -> game::WorldCut {
    for _ in 0..CUT_ROUNDS {
        let m = &mut rig.hosts[h].manager;
        if let Some(cut) = m.take_cut() {
            m.cut_committed(cut.id, Vec::new());
            return cut;
        }
        if m.pending_cut().is_none() {
            let world = m.spawner_mut().world_tick();
            let lead = m.cut_lead();
            m.begin_cut((world + CUT_LEAD).saturating_sub(lead));
        }
        rig.round();
    }
    panic!("host {h}'s cut never completed; waiting for {:?}", rig.hosts[h].manager.awaiting_cut());
}

/// The link is down while the slime crosses: the sender's cut holds the transfer once,
/// as a row to its target (in no region snapshot), and a host restored from that cut
/// queues it to the owner again, which gets it once the link is up.
fn cut_and_restore(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.link_down(ha, hb);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    for _ in 0..WAIT_ROUNDS {
        if rig.hosts[ha as usize].manager.peer_unreceived() > 0 {
            break;
        }
        rig.round();
    }
    let cut = cut_on(rig, ha as usize);
    let mut store = CutStore::default();
    store.apply(&cut);
    assert_eq!(store.places(id), [(b, "row")], "the cut holds the unreceived transfer once, as a row");
    assert!(rig.owners_of(id).is_empty(), "still only in the session");

    // A host restarted from that cut, the link up.
    let mut restored = MultiRig::new(2, 1, None, 0, 0);
    restored.run(a);
    restored.run(b);
    restored.hosts[ha as usize].manager.restore_in_flight(cut.in_flight.clone());
    assert_eq!(
        restored.hosts[ha as usize].manager.peer_unreceived_ids(),
        [(hb, id, false)],
        "the row is queued to its owner again"
    );
    assert_eq!(restored.hosts[ha as usize].manager.pending_arrival_total(), 0, "not held on the sending host");
    until_settled_in(&mut restored, id, b);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn one_phase_cut_holds_unreceived_transfer_once_and_restore_requeues_it() {
    sweep("one_phase_cut_holds_unreceived_transfer_once_and_restore_requeues_it", 2, 1, SEEDS / 2, cut_and_restore);
}

/// Rounds the keeper's home region sits out so a cut stays pending after the source
/// region answered it.
const HOLD_HOME_ROUNDS: u32 = 1000;

/// The link is down; a slime standing in `a` at the cut tick departs to the other host
/// only after `a` answered the cut (another region of its host keeps the cut pending):
/// `a`'s snapshot holds it, so the cut writes no in-flight row for it — it is saved once —
/// and restoring that cut's rows queues nothing for it.
fn depart_after_the_answer(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let home = rig.homes[ha as usize];
    rig.run(a);
    rig.run(b);
    rig.link_down(ha, hb);
    let id = rig.spawn_slime(a, [REGION_SIZE / 2.0, crate::common::above_floor_y(), REGION_SIZE / 2.0]);
    rig.round();
    // A cut that `a` answers while `home` keeps it pending.
    let m = &mut rig.hosts[ha as usize].manager;
    m.spawner_mut().hold(home, HOLD_HOME_ROUNDS);
    let mut answered = false;
    for _ in 0..CUT_ROUNDS {
        let m = &mut rig.hosts[ha as usize].manager;
        if m.pending_cut().is_none() {
            // None yet, or `a` missed it: (again) a cut ahead of the world tick.
            let world = m.spawner_mut().world_tick();
            let lead = m.cut_lead();
            m.begin_cut((world + CUT_LEAD).saturating_sub(lead)).expect("no cut pending");
        }
        rig.round();
        let m = &rig.hosts[ha as usize].manager;
        if m.pending_cut().is_some() && !m.awaiting_cut().contains(&a) {
            answered = true;
            break;
        }
    }
    assert!(answered, "`a` never answered a pending cut");
    assert_eq!(rig.owners_of(id), [(ha, a)], "the slime stood in `a` at the cut tick");
    // Now it leaves for the other host.
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        if !rig.hosts[ha as usize].manager.peer_unreceived_ids().is_empty() {
            break;
        }
    }
    assert_eq!(rig.hosts[ha as usize].manager.peer_unreceived_ids(), [(hb, id, false)], "queued to host {hb}");
    rig.hosts[ha as usize].manager.spawner_mut().hold(home, 0);
    let cut = cut_on(rig, ha as usize);
    let mut store = CutStore::default();
    store.apply(&cut);
    assert_eq!(store.places(id), [(a, "state")], "saved once, in its source's snapshot");
    // Restoring the cut's rows queues nothing for it (the snapshot brings it back).
    let mut restored = MultiRig::new(2, 1, None, 0, 0);
    restored.hosts[ha as usize].manager.restore_in_flight(cut.in_flight.clone());
    assert!(
        restored.hosts[ha as usize].manager.peer_unreceived_ids().iter().all(|(_, q, _)| *q != id),
        "no row of it is queued again"
    );
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn transfer_departing_after_its_source_answered_the_cut_is_not_a_row() {
    sweep("transfer_departing_after_its_source_answered_the_cut_is_not_a_row", 2, 1, SEEDS / 2, depart_after_the_answer);
}

// --- Halo and intents between hosts (Task 6) -------------------------------------

/// Stud columns along a region's edge.
const REGION_STUDS: i32 = (game::REGION_CHUNKS * game::STUD_CHUNK_STUDS) as i32;
/// Round budget for a halo change to show across a host seam.
const HALO_ROUNDS: usize = 8;
/// Items a deposit test puts in.
const DEPOSIT: u32 = 5;
/// The dirt item of the block manifest.
const DIRT: game::ItemId = game::ItemId(1);

/// The stud cell `cell` of region `of`, in region `rc`'s frame.
fn cell_from(rc: RegionCoords, of: RegionCoords, cell: [i32; 3]) -> [i64; 3] {
    let (dx, dz) = (of.x - rc.x, of.z - rc.z);
    [(cell[0] + dx * REGION_STUDS) as i64, cell[1] as i64, (cell[2] + dz * REGION_STUDS) as i64]
}

/// The material at stud cell `cell` of region `of` as running region `rc` sees it (own ∪
/// halo): `None` is air, or outside own ∪ halo.
fn seen_from(rig: &mut MultiRig, rc: RegionCoords, of: RegionCoords, cell: [i32; 3]) -> Option<u16> {
    let mut m = None;
    rig.host_of(rc).manager.spawner_mut().with_region(rc, |r| m = r.data().stud_material_seen(cell_from(rc, of, cell)));
    m
}

/// The box of the top ground plate of `a` over the 2 x 2 studs on its edge facing the
/// adjacent `b`, mid-way along that edge.
fn border_plate(a: RegionCoords, b: RegionCoords) -> ([i32; 3], [i32; 3]) {
    let mid = REGION_STUDS / 2;
    let across = |d: i32| match d {
        1 => Some(REGION_STUDS - 4),
        -1 => Some(2),
        _ => None,
    };
    let x = across(b.x - a.x).unwrap_or(mid);
    let z = across(b.z - a.z).unwrap_or(mid);
    let plates = crate::common::FLOOR_PLATES;
    ([x, plates - 1, z], [x + 2, plates, z + 2])
}

/// Carves `lo .. hi` out of running `rc` between ticks.
fn carve(rig: &mut MultiRig, rc: RegionCoords, (lo, hi): ([i32; 3], [i32; 3])) {
    rig.host_of(rc).manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| assert!(!d.carve_stud_safe(lo, hi).is_empty(), "{rc:?} {lo:?} carved"));
    });
}

/// The mirror running `rc` holds of entity `id`, if any.
fn mirror_in(rig: &mut MultiRig, rc: RegionCoords, id: game::EntityId) -> bool {
    let mut m = false;
    rig.host_of(rc).manager.spawner_mut().with_region(rc, |r| m = r.data().halo_entity(id).is_some());
    m
}

/// A spot in `a`'s frame inside its edge band facing the adjacent `b` (1.5 tiles short
/// of the seam), mid-way along it.
fn in_band(a: RegionCoords, b: RegionCoords) -> [f32; 3] {
    let inset = 1.5 * crate::common::TILE_W;
    let across = |d: i32| match d {
        1 => Some(REGION_SIZE - inset),
        -1 => Some(inset),
        _ => None,
    };
    let x = across(b.x - a.x).unwrap_or(REGION_SIZE / 2.0);
    let z = across(b.z - a.z).unwrap_or(REGION_SIZE / 2.0);
    [x, crate::common::above_floor_y(), z]
}

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

/// A dig on `a`'s border (host A) shows in `b`'s halo mirror (host B) within
/// [`HALO_ROUNDS`].
fn halo_delta(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    rig.run(a);
    rig.run(b);
    let plate = border_plate(a, b);
    assert!(until(rig, HALO_ROUNDS, |r| seen_from(r, b, a, plate.0).is_some()), "b's halo holds a's ground");
    carve(rig, a, plate);
    assert!(until(rig, HALO_ROUNDS, |r| seen_from(r, b, a, plate.0).is_none()), "the dig never reached b's halo");
    let below = [plate.0[0], plate.0[1] - 1, plate.0[2]];
    assert!(seen_from(rig, b, a, below).is_some(), "the plate under it still stands");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_delta_crosses_host_seam() {
    sweep("halo_delta_crosses_host_seam", 2, 1, SEEDS, halo_delta);
}

/// A slime in `a`'s band facing `b` (another host's) is mirrored in `b`.
fn halo_entity(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    rig.run(a);
    rig.run(b);
    let id = rig.spawn_slime(a, in_band(a, b));
    assert!(until(rig, CROSS_ROUNDS, |r| mirror_in(r, b, id)), "b never mirrored a's slime");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_entity_crosses_host_seam() {
    sweep("halo_entity_crosses_host_seam", 2, 1, SEEDS, halo_entity);
}

/// The link between the two hosts drops: `b` (host B) ends its mirrors of `a` (host A),
/// and B's view of A's running regions empties. A dig on `a`'s border meanwhile is not
/// seen; once the link is back, `b`'s halo is seeded again (the dig shows), its mirror
/// of `a`'s slime is back, and B's view of A's running regions is what it was.
fn halo_link_flap(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    let id = rig.spawn_slime(a, in_band(a, b));
    assert!(until(rig, CROSS_ROUNDS, |r| mirror_in(r, b, id)), "b never mirrored a's slime");
    let known = rig.hosts[hb as usize].manager.foreign_running();
    assert!(known.contains(&a), "host {hb} knows `a` runs: {known:?}");
    rig.link_down(ha, hb);
    assert!(until(rig, WAIT_ROUNDS, |r| !mirror_in(r, b, id)), "the mirror outlived the link");
    assert!(rig.hosts[hb as usize].manager.foreign_running().is_empty(), "nothing of host {ha} is known to run");
    let plate = border_plate(a, b);
    carve(rig, a, plate);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert!(seen_from(rig, b, a, plate.0).is_some(), "nothing crosses a down link");
    rig.link_up(ha, hb);
    assert!(
        until(rig, CROSS_ROUNDS, |r| seen_from(r, b, a, plate.0).is_none() && mirror_in(r, b, id)),
        "b's halo was never seeded again"
    );
    assert_eq!(rig.hosts[hb as usize].manager.foreign_running(), known, "exactly what ran before");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_source_gone_when_peer_link_drops_and_reseeds_on_up() {
    sweep("halo_source_gone_when_peer_link_drops_and_reseeds_on_up", 2, 1, SEEDS, halo_link_flap);
}

/// Halo traffic from a running `a` toward `b`, which its owner does not run, never goes
/// on the link and never wakes `b`.
fn halo_no_wake(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let hb = rig.owner(b);
    rig.run(a);
    rig.spawn_slime(a, in_band(a, b));
    carve(rig, a, border_plate(a, b));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        for (ea, eb) in rig.links.values() {
            for end in [ea, eb] {
                for msg in end.lock().unwrap().transit() {
                    if let game::PeerMsg::Volatile(
                        game::Volatile::HaloUpdate { to, .. } | game::Volatile::HaloEntity { to, .. },
                    ) = msg
                    {
                        assert_ne!(to, b, "halo traffic to a region its owner does not run");
                    }
                }
            }
        }
        assert!(!rig.hosts[hb as usize].manager.running_regions().contains(&b), "halo woke {b:?}");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_never_wakes_a_foreign_region() {
    sweep("halo_never_wakes_a_foreign_region", 2, 1, SEEDS, halo_no_wake);
}

/// The world cell of `b` just past `a`'s seam with it, mid-way along, at standing level.
fn deposit_cell(a: RegionCoords, b: RegionCoords) -> [i32; 3] {
    let n = game::REGION_BLOCKS as i32;
    let across = |d: i32| match d {
        1 => Some(n + 1),
        -1 => Some(-2),
        _ => None,
    };
    [across(b.x - a.x).unwrap_or(n / 2), crate::common::STAND, across(b.z - a.z).unwrap_or(n / 2)]
}

/// [`DEPOSIT`] dirt put down by region `a` at a cell of `b` (`a`'s frame): a
/// region-sourced `DepositItems` intent to `b`'s owner. Item conservation is checked
/// every round from now on.
fn deposit_across(rig: &mut MultiRig, a: RegionCoords, b: RegionCoords) {
    crate::common::install_real_block_props();
    let at = deposit_cell(a, b);
    let stack = game::ItemStack { item: DIRT, count: DEPOSIT };
    rig.host_of(a).manager.spawner_mut().with_region(a, |r| {
        let now = r.current_tick();
        r.with_data(|d| assert_eq!(d.deposit_items_safe(at, stack, now, game::PICKUP_DELAY_TICKS), 0));
    });
    rig.items = Some(u64::from(DEPOSIT));
}

/// Items in running `rc`'s own piles.
fn pile_items(rig: &mut MultiRig, rc: RegionCoords) -> u32 {
    let mut n = 0;
    rig.host_of(rc).manager.spawner_mut().with_region(rc, |r| {
        n = r.data().item_piles().values().flat_map(|p| &p.stacks).map(|s| s.count).sum();
    });
    n
}

/// A deposit from `a` into `b` (another host's, running) lands in `b`'s piles, every
/// item conserved on the way.
fn deposit_to_peer(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    rig.run(a);
    rig.run(b);
    deposit_across(rig, a, b);
    assert!(
        until(rig, CROSS_ROUNDS, |r| pile_items(r, b) == DEPOSIT && r.peer_unreceived() == 0),
        "the deposit never landed in b: {} there, {} unacknowledged",
        pile_items(rig, b),
        rig.peer_unreceived()
    );
    assert_eq!(pile_items(rig, a), 0, "nothing stayed in a");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn deposit_intent_across_hosts_conserves_items() {
    sweep("deposit_intent_across_hosts_conserves_items", 2, 1, SEEDS, deposit_to_peer);
}

/// The link is down: a deposit from `a` into `b` is refused at once (`OwnerNotRunning`)
/// and its items come back to `a`, which puts them down on its own side of the seam.
fn deposit_to_unreachable(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.link_down(ha, hb);
    assert!(until(rig, WAIT_ROUNDS, |r| !r.hosts[ha as usize].manager.reaches(b)), "the link never went down");
    deposit_across(rig, a, b);
    assert!(until(rig, CROSS_ROUNDS, |r| pile_items(r, a) == DEPOSIT), "the items never came back to a");
    assert!(rig.hosts[ha as usize].manager.peer_unreceived_intents().is_empty(), "nothing queued to host {hb}");
    assert_eq!(pile_items(rig, b), 0);
    rig.link_up(ha, hb);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!((pile_items(rig, a), pile_items(rig, b)), (DEPOSIT, 0), "and they stay there");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn intent_to_unreachable_host_is_refused_and_items_return() {
    sweep("intent_to_unreachable_host_is_refused_and_items_return", 2, 1, SEEDS, deposit_to_unreachable);
}

/// `b` does not run on its host: the deposit is refused there and the refusal — carrying
/// the items — goes back to `a`'s host as a reliable reply. The link drops as soon as
/// the reply is queued; the reply waits, and once the link is back the items reach `a`,
/// conserved every round.
fn reply_across_a_flap(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    deposit_across(rig, a, b);
    let is = |r: &MultiRig, h: u32, reply: bool| {
        r.hosts[h as usize].manager.peer_unreceived_intents().iter().any(|(_, m, _)| matches!(m, game::IntentMsg::Reply(_)) == reply)
    };
    let mut went = false;
    let queued = until(rig, CROSS_ROUNDS, |r| {
        went |= is(r, ha, false);
        is(r, hb, true) || pile_items(r, a) == DEPOSIT
    });
    assert!(went, "the deposit never went to host {hb}");
    assert!(queued, "host {hb} never answered the deposit");
    rig.link_down(ha, hb);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    rig.link_up(ha, hb);
    assert!(
        until(rig, CROSS_ROUNDS, |r| pile_items(r, a) == DEPOSIT && r.peer_unreceived() == 0),
        "the refusal never brought the items back: {} in a, {} unacknowledged",
        pile_items(rig, a),
        rig.peer_unreceived()
    );
    assert!(!rig.hosts[hb as usize].manager.running_regions().contains(&b), "an intent never wakes its owner");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn intent_reply_survives_a_link_flap() {
    sweep("intent_reply_survives_a_link_flap", 2, 1, SEEDS, reply_across_a_flap);
}

/// The link is down (not noticed yet) when `a` sends a deposit to `b`'s host: it waits
/// unsent in the session. The sender's one-phase cut holds it once, as an intent row to
/// `b`; a host restored from that cut queues it to `b`'s host again, which (not running
/// `b`) refuses it, and the items come back to `a`.
fn intent_cut_and_restore(rig: &mut MultiRig) {
    crate::common::install_real_block_props();
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.link_down(ha, hb);
    // As `a`'s own `Intents` output, before its host noticed the link is down.
    let [x, y, z] = deposit_cell(a, b);
    let cell = game::WorldBlockPos::from_local(a, game::BlockPos::new(x, y, z));
    let due = rig.hosts[ha as usize].manager.spawner_mut().world_tick() + 1;
    let by = game::EntityId::of_region(a);
    let stack = game::ItemStack { item: DIRT, count: DEPOSIT };
    let intent = game::Intent::DepositItems {
        cell,
        stack,
        expire: due + game::PILE_DESPAWN_TICKS,
        pickup: due + game::PICKUP_DELAY_TICKS,
        by,
    };
    let msg = game::IntentMsg::Intent(game::SentIntent { target: game::Target::Pile(cell), intent, from: a, due });
    let host = &mut rig.hosts[ha as usize];
    let now = host.now_ms;
    host.manager.handle_region_output(a, game::RegionOutput::Intents(vec![msg]), now);
    rig.items = Some(u64::from(DEPOSIT));
    assert_eq!(rig.hosts[ha as usize].manager.peer_unreceived_intents(), [(hb, msg, false)], "queued to host {hb}");
    let cut = cut_on(rig, ha as usize);
    let rows: Vec<_> = cut.in_flight.iter().filter(|r| matches!(r, game::InFlightRow::Intent { .. })).cloned().collect();
    assert_eq!(rows, [game::InFlightRow::Intent { target: b, msg }], "the cut holds the unreceived intent once");

    // A host restarted from that cut, the link up.
    let mut restored = MultiRig::new(2, 1, None, 0, 0);
    restored.run(a);
    restored.hosts[ha as usize].manager.restore_in_flight(cut.in_flight.clone());
    assert_eq!(
        restored.hosts[ha as usize].manager.peer_unreceived_intents(),
        [(hb, msg, false)],
        "the row is queued to its owner again"
    );
    restored.items = Some(u64::from(DEPOSIT));
    assert!(
        until(&mut restored, CROSS_ROUNDS, |r| pile_items(r, a) == DEPOSIT && r.peer_unreceived() == 0),
        "the refused deposit never came back to a"
    );
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn one_phase_cut_holds_unreceived_intent_and_restore_requeues_it() {
    sweep("one_phase_cut_holds_unreceived_intent_and_restore_requeues_it", 2, 1, SEEDS / 2, intent_cut_and_restore);
}

// --- Fix round 1 -----------------------------------------------------------------

/// The `+x` pair of regions nearest the origin whose owners are the same host
/// (`same`) or differ, among `hosts` hosts (`s` regions per super-region side),
/// avoiding `taken`.
fn x_pair(hosts: u32, s: u32, same: bool, taken: &[RegionCoords]) -> (RegionCoords, RegionCoords) {
    let owner = |rc: RegionCoords| game::Ownership::Static { ordinal: 0, hosts }.owner_of(rc, s);
    let mut all: Vec<RegionCoords> = (-8..=8).flat_map(|x| (-8..=8).map(move |z| RegionCoords::new(x, z))).collect();
    all.sort_by_key(|rc| (rc.x.abs() + rc.z.abs(), rc.x, rc.z));
    for a in all {
        let b = RegionCoords::new(a.x + 1, a.z);
        if (owner(a) == owner(b)) == same && !taken.contains(&a) && !taken.contains(&b) {
            return (a, b);
        }
    }
    panic!("no such pair near the origin");
}

/// A stocked chest.
fn stocked_chest() -> game::Container {
    let mut c = game::Container::chest();
    c.slots[2] = Some(game::ItemStack { item: DIRT, count: 7 });
    c
}

/// A pile and a chest on `a`'s edge tiles facing `b = a + x`.
fn pile_and_chest(rig: &mut MultiRig, a: RegionCoords) {
    crate::common::install_real_block_props();
    let n = game::REGION_BLOCKS as i32;
    let stand = crate::common::STAND;
    rig.host_of(a).manager.spawner_mut().with_region(a, |r| {
        let now = r.current_tick();
        let stack = game::ItemStack { item: DIRT, count: DEPOSIT };
        r.with_data(|d| {
            assert_eq!(d.deposit_items_safe([n - 1, stand, 5], stack, now, game::PICKUP_DELAY_TICKS), 0);
            let chest = game::WorldBlockPos::from_local(a, game::BlockPos::new(n - 1, stand, 9));
            d.write_container_safe(chest, Some(stocked_chest()));
        });
    });
}

/// The pile and chest mirrors running `b` holds of region `a`, in `a`'s frame.
type Mirrors = (Vec<(game::BlockPos, Vec<game::ItemStack>)>, Vec<(game::BlockPos, game::Container)>);

fn mirrors_of(rig: &mut MultiRig, b: RegionCoords, a: RegionCoords) -> Mirrors {
    let mut out = (Vec::new(), Vec::new());
    rig.host_of(b).manager.spawner_mut().with_region(b, |r| {
        let d = r.data();
        out.0 = d
            .halo_piles()
            .iter()
            .filter(|(p, _)| p.to_local().0 == a)
            .map(|(p, pile)| (p.to_local().1, pile.stacks.clone()))
            .collect();
        out.1 = d
            .halo_containers()
            .iter()
            .filter(|(p, _)| p.to_local().0 == a)
            .map(|(p, c)| (p.to_local().1, c.clone()))
            .collect();
    });
    out
}

/// The same park across a region seam (one host) and across a host seam: a pile and a
/// chest on the parking region's edge; the neighbour's mirrors of them after the park
/// are equal either way (`RegionParked` is not a loss). Both seams end with those
/// mirrors cleared: a park re-seeds the running neighbours from the parked region's final
/// terrain, a `replace` seed without piles or chests.
fn park_keeps_mirrors(rig: &mut MultiRig) {
    let (ha, hb) = x_pair(2, 2, false, &[]);
    let (la, lb) = x_pair(2, 2, true, &[ha, hb]);
    for rc in [ha, hb, la, lb] {
        rig.run(rc);
    }
    pile_and_chest(rig, ha);
    pile_and_chest(rig, la);
    let mirrored = |r: &mut MultiRig, a, b| {
        let m = mirrors_of(r, b, a);
        !m.0.is_empty() && !m.1.is_empty()
    };
    assert!(until(rig, CROSS_ROUNDS, |r| mirrored(r, ha, hb) && mirrored(r, la, lb)), "both neighbours mirror");
    assert_eq!(mirrors_of(rig, hb, ha), mirrors_of(rig, lb, la), "the same mirrors either way before the park");
    rig.park(la);
    rig.park(ha);
    // (Across the host seam the final seed may lag by the link's delay.)
    for _ in 0..HALO_ROUNDS {
        rig.round();
    }
    assert_eq!(mirrors_of(rig, hb, ha), mirrors_of(rig, lb, la), "a park across a host seam: as across a region seam");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn park_across_a_host_seam_matches_a_region_seam() {
    sweep("park_across_a_host_seam_matches_a_region_seam", 2, 2, SEEDS / 2, park_keeps_mirrors);
}

/// `a` (host A) is lost — its thread died — with a pile, a chest and a slime mirrored in
/// `b` (host B): B drops every mirror of it (`RegionDown` → `HaloSourceGone`).
fn lost_drops_mirrors(rig: &mut MultiRig) {
    let (a, b) = x_pair(2, 1, false, &[]);
    rig.run(a);
    rig.run(b);
    pile_and_chest(rig, a);
    let id = rig.spawn_slime(a, in_band(a, b));
    assert!(
        until(rig, CROSS_ROUNDS, |r| mirror_in(r, b, id) && !mirrors_of(r, b, a).0.is_empty() && !mirrors_of(r, b, a).1.is_empty()),
        "b mirrors a"
    );
    let host = rig.host_of(a);
    host.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(a), crate::common::rig::CLIENT));
    host.manager.spawner_mut().kill(a);
    host.now_ms += game::UNLOAD_GRACE_MS;
    let now = host.now_ms;
    host.manager.maintain(now);
    host.settle();
    // (A message for `a` may find it dead first and respawn it, empty: also a loss.)
    let now = rig.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
    for r in &mut rig.hosts {
        r.now_ms = now;
    }
    // The slime died with `a`: no longer tracked.
    rig.tracked.remove(&id);
    assert!(
        until(rig, CROSS_ROUNDS, |r| !mirror_in(r, b, id) && mirrors_of(r, b, a) == (Vec::new(), Vec::new())),
        "b still mirrors lost a: {:?}",
        mirrors_of(rig, b, a)
    );
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lost_region_across_a_host_seam_drops_its_mirrors() {
    sweep("lost_region_across_a_host_seam_drops_its_mirrors", 2, 1, SEEDS / 2, lost_drops_mirrors);
}

/// A halo request for a region its owner has parked (the requester did not know yet) is
/// answered from the owner's terrain cache: a dig `a` made before parking reaches `b`,
/// which started while the link was down (seeded from worldgen).
fn request_answered_from_cache(rig: &mut MultiRig) {
    let (a, b) = x_pair(2, 1, false, &[]);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.link_down(ha, hb);
    rig.run(b);
    rig.run(a);
    let plate = border_plate(a, b);
    carve(rig, a, plate);
    rig.round();
    rig.park(a);
    rig.link_up(ha, hb);
    assert!(
        until(rig, CROSS_ROUNDS, |r| r.hosts[ha as usize].manager.foreign_running().contains(&b)),
        "host {ha} learns `b` runs"
    );
    assert!(seen_from(rig, b, a, plate.0).is_some(), "b's halo of a is worldgen's");
    // The request `b`'s host would have sent had it still believed `a` ran.
    let link = rig.links[&(ha.min(hb), ha.max(hb))].clone();
    let end = if hb < ha { link.0 } else { link.1 };
    game::PeerLink::send(&mut *end.lock().unwrap(), game::PeerMsg::Volatile(game::Volatile::HaloRequest { from: b, of: a }));
    assert!(until(rig, HALO_ROUNDS, |r| seen_from(r, b, a, plate.0).is_none()), "never answered from the cache");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_request_for_a_parked_foreign_region_is_answered_from_its_terrain_cache() {
    sweep(
        "halo_request_for_a_parked_foreign_region_is_answered_from_its_terrain_cache",
        2,
        1,
        SEEDS / 2,
        request_answered_from_cache,
    );
}

/// A region-sourced deposit reply from `a` to (foreign) `b`'s region, carrying `count` dirt.
fn refusal_to(a: RegionCoords, b: RegionCoords, due: u64) -> game::IntentMsg {
    let [x, y, z] = deposit_cell(b, a);
    let cell = game::WorldBlockPos::from_local(b, game::BlockPos::new(x, y, z));
    let stack = game::ItemStack { item: DIRT, count: DEPOSIT };
    let by = game::EntityId::of_region(b);
    let intent = game::Intent::DepositItems { cell, stack, expire: due + game::PILE_DESPAWN_TICKS, pickup: due, by };
    game::IntentMsg::Reply(game::SentReply {
        reply: game::IntentReply::Refused(intent, game::IntentRefusal::PileFull),
        hint: b,
        due,
        hops: 0,
    })
}

/// Replies carrying 1..=3 dirt.
const OWED: [u32; 3] = [1, 2, 3];

/// [`refusal_to`] carrying `count` dirt.
fn refusal_of(a: RegionCoords, b: RegionCoords, due: u64, count: u32) -> game::IntentMsg {
    let game::IntentMsg::Reply(mut r) = refusal_to(a, b, due) else { unreachable!() };
    if let game::IntentReply::Refused(game::Intent::DepositItems { stack, .. }, _) = &mut r.reply {
        stack.count = count;
    }
    game::IntentMsg::Reply(r)
}

/// Replies owed both ways — some sent and unacknowledged, some received and not
/// acknowledged yet — when both hosts are handed a fresh link (`set_peer` again, its
/// connection value the old one's): each keeps its session, and everything owed arrives
/// exactly once (item conservation every round, the exact totals at the end), within a
/// bounded number of rounds.
fn replace_link(rig: &mut MultiRig) {
    crate::common::install_real_block_props();
    let (a, b) = x_pair(2, 1, false, &[]);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    let due = rig.hosts[ha as usize].manager.spawner_mut().world_tick() + 1;
    for (from, to, h) in [(a, b, ha), (b, a, hb)] {
        let msgs: Vec<_> = OWED.iter().map(|&n| refusal_of(from, to, due, n)).collect();
        let host = &mut rig.hosts[h as usize];
        let now = host.now_ms;
        host.manager.handle_region_output(from, game::RegionOutput::Intents(msgs), now);
    }
    let total: u32 = OWED.iter().sum();
    rig.items = Some(u64::from(2 * total));
    rig.round();
    rig.replace_link(ha, hb, 0x5EED, 2);
    assert!(
        until(rig, CROSS_ROUNDS, |r| pile_items(r, a) == total && pile_items(r, b) == total && r.peer_unreceived() == 0),
        "not delivered exactly once: {} in a, {} in b, {} unacknowledged",
        pile_items(rig, a),
        pile_items(rig, b),
        rig.peer_unreceived()
    );
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!((pile_items(rig, a), pile_items(rig, b)), (total, total), "nothing came twice");
    assert!(rig.hosts[ha as usize].manager.reaches(b) && rig.hosts[hb as usize].manager.reaches(a), "the link is up");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn replacing_a_peer_link_keeps_the_session_and_delivers_exactly_once() {
    sweep("replacing_a_peer_link_keeps_the_session_and_delivers_exactly_once", 2, 1, SEEDS / 2, replace_link);
}

/// Two identical replies to another host in one cut window: the first sent after its
/// source answered the pending cut (in its snapshot: not a row) and acknowledged at once;
/// the second sent by a region that has not answered, owed over a down link. The cut
/// holds exactly that second one as a row.
fn identical_in_and_out_of_view(rig: &mut MultiRig) {
    crate::common::install_real_block_props();
    let (a, b) = x_pair(2, 1, false, &[]);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let home = rig.homes[ha as usize];
    rig.run(a);
    rig.run(b);
    rig.hosts[ha as usize].manager.spawner_mut().hold(home, HOLD_HOME_ROUNDS);
    let mut answered = false;
    for _ in 0..CUT_ROUNDS {
        let m = &mut rig.hosts[ha as usize].manager;
        if m.pending_cut().is_none() {
            let world = m.spawner_mut().world_tick();
            let lead = m.cut_lead();
            m.begin_cut((world + CUT_LEAD).saturating_sub(lead)).expect("no cut pending");
        }
        rig.round();
        let m = &rig.hosts[ha as usize].manager;
        if m.pending_cut().is_some() && !m.awaiting_cut().contains(&a) {
            answered = true;
            break;
        }
    }
    assert!(answered, "`a` never answered a pending cut");
    let due = rig.hosts[ha as usize].manager.spawner_mut().world_tick() + 1;
    let msg = refusal_to(a, b, due);
    let inject = |rig: &mut MultiRig, from: RegionCoords| {
        let host = &mut rig.hosts[ha as usize];
        let now = host.now_ms;
        host.manager.handle_region_output(from, game::RegionOutput::Intents(vec![msg]), now);
    };
    // From `a`, which answered: in view.
    inject(rig, a);
    rig.items = Some(u64::from(DEPOSIT));
    assert!(
        until(rig, CROSS_ROUNDS, |r| r.hosts[ha as usize].manager.peer_unreceived_intents().is_empty()),
        "the first reply is acknowledged"
    );
    rig.link_down(ha, hb);
    assert!(until(rig, WAIT_ROUNDS, |r| !r.hosts[ha as usize].manager.reaches(b)), "the link never went down");
    // From `home`, which has not answered (held): not in view.
    inject(rig, home);
    rig.items = Some(u64::from(2 * DEPOSIT));
    assert_eq!(rig.hosts[ha as usize].manager.peer_unreceived_intents(), [(hb, msg, false)]);
    assert!(rig.hosts[ha as usize].manager.pending_cut().is_some(), "still the same cut");
    rig.hosts[ha as usize].manager.spawner_mut().hold(home, 0);
    let cut = cut_on(rig, ha as usize);
    let rows: Vec<_> = cut.in_flight.iter().filter(|r| matches!(r, game::InFlightRow::Intent { .. })).cloned().collect();
    assert_eq!(rows, [game::InFlightRow::Intent { target: b, msg }], "one row: the one not in view");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn identical_intents_in_and_out_of_the_cut_view_make_one_row() {
    sweep("identical_intents_in_and_out_of_the_cut_view_make_one_row", 2, 1, SEEDS / 2, identical_in_and_out_of_view);
}
