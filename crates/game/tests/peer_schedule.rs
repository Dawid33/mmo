//! Entities crossing between region hosts (`world_manager/peers.rs`) under seeded
//! scheduling jitter and link delay ([`MultiRig`]): every scenario runs in lockstep with
//! every link delay 0..=3, then `SEEDS` seeds of each schedule mode (`shuffled`, `late`,
//! `jittery`, the link delay rolling through 0..=3 with the seed). Every round checks
//! conservation across hosts ([`MultiRig::assert_conserved_world`]); a failure names a
//! replayable `(mode, seed, delay)`. Run via `just test-threads`.
use game::{InlineSchedule, RegionCoords, RegionSpawner, FLIP_HYSTERESIS, REGION_SIZE};

use crate::common::cut_store::CutStore;
use crate::common::multi_rig::{four_host_corner, host_seam, rig_endpoint, MultiRig};

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

// --- Players crossing between hosts (Task 7) ---------------------------------------

/// The test player's session id (the same on every host) and player.
const P_CLIENT: game::ClientId = 7;
const P_PLAYER: game::PlayerId = game::PlayerId(100);
/// Round budget for the test player's restore to give it a body.
const BODY_ROUNDS: usize = 20;
/// Round budget for the held-key walk to reach the other host.
const WALK_ROUNDS: usize = 240;
/// Rounds the held key stays down after the crossing (3 s).
const HELD_AFTER_ROUNDS: usize = (3000 / game::TICK_RATE) as usize;
/// How far ahead of the world tick a stamped input is predicted (as a client does).
const INPUT_LEAD: u64 = 2;
/// Where the held-key walk starts: this far (units) from the seam it walks to.
const WALK_START: f32 = 24.0;

/// The test player's record: standing at `xz` (`rc`'s frame) in `rc`.
fn record_at(rc: RegionCoords, xz: [f32; 2]) -> game::PlayerRecord {
    let y = crate::common::standing_y(crate::common::FLOOR_PLATES);
    game::PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(xz[0], y, xz[1])),
        region: rc,
        fortress: None,
        adventure: None,
    }
}

/// Rounds until the test player has a body; its host, region and id.
fn until_body(rig: &mut MultiRig) -> (u32, RegionCoords, game::EntityId) {
    for _ in 0..BODY_ROUNDS {
        if let Some(body) = rig.player_body(P_CLIENT) {
            return body;
        }
        rig.round();
    }
    panic!("the test player never got a body");
}

/// Rounds until the test player's body is in `rc` on its owner and nothing is left
/// unacknowledged between hosts.
fn until_player_settled_in(rig: &mut MultiRig, rc: RegionCoords) {
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if rig.player_owners(P_CLIENT) == [(rig.owner(rc), rc)] && rig.peer_unreceived() == 0 {
            return;
        }
    }
    panic!("the player never settled in {rc:?}: owned by {:?}, {} unacknowledged", rig.player_owners(P_CLIENT), rig.peer_unreceived());
}

/// The packets host `h` sent the test player since recording began.
fn packets_to(rig: &MultiRig, h: u32) -> Vec<&game::ServerPacket> {
    let seen = rig.hosts[h as usize].packets_seen.as_ref().expect("recording packets");
    seen.iter().filter(|(to, _)| *to == Some(P_CLIENT)).map(|(_, p)| p).collect()
}

/// The pair of regions nearest the origin whose owners differ, `b` one step -z of `a`
/// (W walks a body toward -z).
fn north_host_seam(hosts: u32, s: u32) -> (RegionCoords, RegionCoords) {
    let owner = |rc: RegionCoords| game::Ownership::Static { ordinal: 0, hosts }.owner_of(rc, s);
    for r in 0..64 {
        for x in -r..=r {
            for z in -r..=r {
                let a = RegionCoords::new(x, z);
                let b = RegionCoords::new(x, z - 1);
                if owner(a) != owner(b) {
                    return (a, b);
                }
            }
        }
    }
    panic!("no north host seam near the origin");
}

/// A body past the seam lands on the other host once; the session there becomes the home
/// (`PlayerRegion`), the one it left an observer; the first packet of each session was
/// `Host`.
fn player_crosses(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.record_packets();
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    for h in [ha, hb] {
        let first = packets_to(rig, h).first().map(|p| matches!(p, game::ServerPacket::Host(e) if *e == rig_endpoint(h)));
        assert_eq!(first, Some(true), "host {h}'s first packet is its Host");
    }
    let (h, rc, id) = until_body(rig);
    assert_eq!((h, rc), (ha, a));
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)), "{a:?} holds the body");
    until_player_settled_in(rig, b);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert_eq!(rig.player_owners(P_CLIENT), [(hb, b)], "one body, in {b:?}, staying");
    }
    let (ma, mb) = (&rig.hosts[ha as usize].manager, &rig.hosts[hb as usize].manager);
    assert_eq!((ma.home_of(P_CLIENT), mb.home_of(P_CLIENT)), (None, Some(b)), "the home followed the body");
    assert_eq!(ma.session_kind(P_CLIENT), Some(game::SessionKind::Observer), "the session it left observes");
    assert_eq!(mb.session_kind(P_CLIENT), Some(game::SessionKind::Home), "the session it reached is the home");
    assert_eq!((ma.player_of(P_CLIENT), mb.player_of(P_CLIENT)), (None, Some(P_PLAYER)));
    assert_eq!(ma.departed().get(&P_PLAYER), Some(&hb), "host {ha} handed the player to host {hb}");
    assert!(mb.departed().is_empty());
    let told = packets_to(rig, hb).iter().any(|p| matches!(p, game::ServerPacket::PlayerRegion(Some(r), P_CLIENT) if *r == b));
    assert!(told, "host {hb} told the client its new home");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_crosses_host_seam_once_and_home_follows() {
    sweep("player_crosses_host_seam_once_and_home_follows", 2, 1, SEEDS / 2, player_crosses);
}

/// A stamped input from the test player to host `h` (where the client believes its body
/// is), predicted `INPUT_LEAD` ticks ahead.
fn stamped_to(rig: &mut MultiRig, h: u32, input: game::InputEvent) {
    let rig = &mut rig.hosts[h as usize];
    let tick = rig.manager.spawner_mut().world_tick();
    let ev = game::GameEvent::new(game::GameEventKind::PlayerInput(P_CLIENT, input), 0, RegionCoords::new(0, 0));
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::GameEventAfterTick(ev, tick + INPUT_LEAD), P_CLIENT));
}

/// W held across a host seam with a marker input every round, every input sent to the
/// host the walk began on: the body crosses once and stays; every marker is applied
/// exactly once — on the old host only in the region it left, on the new one only in the
/// region it reached (forwarded there).
fn held_key(rig: &mut MultiRig) {
    use game::{InputEvent, Key};
    let (a, b) = north_host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, WALK_START]));
    until_body(rig);
    for h in [ha, hb] {
        rig.hosts[h as usize].events_seen = Some(Vec::new());
    }
    stamped_to(rig, ha, InputEvent::Key { key: Key::KeyE, pressed: true });
    stamped_to(rig, ha, InputEvent::Key { key: Key::KeyE, pressed: false });
    stamped_to(rig, ha, InputEvent::Key { key: Key::KeyW, pressed: true });
    let mut seq = 0u32;
    let mut crossed = false;
    for _ in 0..WALK_ROUNDS {
        seq += 1;
        stamped_to(rig, ha, InputEvent::Resized { width: seq, height: 1 });
        rig.round();
        if rig.player_owners(P_CLIENT) == [(hb, b)] {
            crossed = true;
            break;
        }
    }
    assert!(crossed, "the player never reached {b:?}: owned by {:?}", rig.player_owners(P_CLIENT));
    for round in 0..HELD_AFTER_ROUNDS {
        seq += 1;
        stamped_to(rig, ha, InputEvent::Resized { width: seq, height: 1 });
        rig.round();
        let owners = rig.player_owners(P_CLIENT);
        assert!(owners.is_empty() || owners == [(hb, b)], "{round} rounds after the crossing the body is in {owners:?}");
    }
    until_player_settled_in(rig, b);
    let mut count = vec![0u32; seq as usize + 1];
    for (h, rc) in [(ha, a), (hb, b)] {
        for (at, kind) in rig.hosts[h as usize].events_seen.take().expect("recording") {
            if let game::GameEventKind::PlayerInput(P_CLIENT, InputEvent::Resized { width, .. }) = kind {
                assert_eq!(at, rc, "marker {width} applied on host {h} in {at:?}");
                count[width as usize] += 1;
            }
        }
    }
    for s in 1..=seq {
        assert_eq!(count[s as usize], 1, "marker {s} of {seq} applied {} times", count[s as usize]);
    }
    assert_eq!(rig.hosts[ha as usize].manager.departed().get(&P_PLAYER), Some(&hb), "one crossing, no way back");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn held_key_across_host_seam_settles() {
    sweep("held_key_across_host_seam_settles", 2, 1, SEEDS / 4, held_key);
}

/// A->B->A: the body is sent back the round it lands (the first hand-off may not be
/// acknowledged yet). One body, home on A; A (the host holding it) has no `departed`
/// entry, and B's names A — where the player is.
fn ping_pong(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    let mut back = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if let Some((h, rc, id)) = rig.player_body(P_CLIENT) {
            if (h, rc) == (hb, b) {
                assert!(rig.teleport(b, id, past_seam(b, a, REGION_SIZE / 2.0)));
                back = true;
                break;
            }
        }
    }
    assert!(back, "the body never reached {b:?}");
    until_player_settled_in(rig, a);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert_eq!(rig.player_owners(P_CLIENT), [(ha, a)], "one body, back in {a:?}");
    }
    let (ma, mb) = (&rig.hosts[ha as usize].manager, &rig.hosts[hb as usize].manager);
    assert_eq!((ma.home_of(P_CLIENT), mb.home_of(P_CLIENT)), (Some(a), None));
    assert_eq!(ma.session_kind(P_CLIENT), Some(game::SessionKind::Home));
    assert_eq!(mb.session_kind(P_CLIENT), Some(game::SessionKind::Observer));
    assert!(ma.departed().is_empty(), "the player is back on host {ha}: {:?}", ma.departed());
    assert_eq!(mb.departed().get(&P_PLAYER), Some(&ha), "host {hb} handed it back to host {ha}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn ping_pong_across_host_seam() {
    sweep("ping_pong_across_host_seam", 2, 1, SEEDS / 2, ping_pong);
}

/// The client has no session on the host its body is handed to (it cannot reach it): the
/// body waits there with no session, kept alive, and after `HANDOFF_SESSION_GRACE_MS` it
/// is parked there — its record on that host only, exactly once in the world.
fn no_session_there(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_home(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    assert_eq!(rig.hosts[hb as usize].manager.home_of(P_CLIENT), Some(b));
    assert_eq!(rig.hosts[hb as usize].manager.session_kind(P_CLIENT), None);
    // Not before the grace.
    rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS / 2);
    rig.round();
    assert_eq!(rig.player_owners(P_CLIENT), [(hb, b)], "still there within the grace");
    let mut parked = false;
    for _ in 0..CROSS_ROUNDS {
        rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS / 2);
        rig.round();
        if !rig.parked_on(P_PLAYER).is_empty() {
            parked = true;
            break;
        }
    }
    assert!(parked, "the player was never parked: body {:?}", rig.player_owners(P_CLIENT));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.parked_on(P_PLAYER), [hb], "parked on host {hb}, once");
    assert!(rig.player_owners(P_CLIENT).is_empty(), "no body left");
    for h in [ha, hb] {
        assert_eq!(rig.hosts[h as usize].manager.home_of(P_CLIENT), None);
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_without_session_parks_after_grace() {
    sweep("handoff_without_session_parks_after_grace", 2, 1, SEEDS / 2, no_session_there);
}

/// The client disconnects from both hosts while its body crosses (`early`: in the round
/// it is pushed past the seam, else the round host A hands it off; `lag`: the target
/// region sits out rounds meanwhile, so the body's arrival waits there behind its slot):
/// the player ends parked exactly once, on one host, with no body and no home or session
/// anywhere.
fn disconnect_crossing(rig: &mut MultiRig, early: bool, lag: bool) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    if lag {
        rig.host_of(b).manager.spawner_mut().hold(b, LAG_ROUNDS);
    }
    if !early {
        let mut handed = false;
        for _ in 0..CROSS_ROUNDS {
            rig.round();
            if rig.hosts[ha as usize].manager.home_of(P_CLIENT).is_none() {
                handed = true;
                break;
            }
        }
        assert!(handed, "host {ha} never handed the body off");
    }
    rig.disconnect(ha, P_CLIENT);
    rig.disconnect(hb, P_CLIENT);
    let mut parked = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS);
        if rig.parked_on(P_PLAYER).len() == 1 && rig.player_owners(P_CLIENT).is_empty() && rig.peer_unreceived() == 0 {
            parked = true;
            break;
        }
    }
    assert!(parked, "never parked: body {:?}, parked on {:?}", rig.player_owners(P_CLIENT), rig.parked_on(P_PLAYER));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.parked_on(P_PLAYER).len(), 1, "parked once");
    assert!(rig.player_owners(P_CLIENT).is_empty());
    for h in [ha, hb] {
        let m = &rig.hosts[h as usize].manager;
        assert_eq!((m.home_of(P_CLIENT), m.session_kind(P_CLIENT)), (None, None), "host {h} holds nothing of it");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn disconnect_mid_handoff_parks_once() {
    sweep("disconnect_mid_handoff_parks_once (early)", 2, 1, SEEDS / 2, |rig| disconnect_crossing(rig, true, false));
    sweep("disconnect_mid_handoff_parks_once (handed)", 2, 1, SEEDS / 2, |rig| disconnect_crossing(rig, false, false));
    sweep("disconnect_mid_handoff_parks_once (lagging target)", 2, 1, SEEDS / 2, |rig| disconnect_crossing(rig, false, true));
}

/// A subscribe to a region another host owns is answered with that host's endpoint and a
/// ticket; that host is told to expect it (`ObserverOrder`), once per ticket.
fn subscribe_elsewhere(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.connect_home(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    until_body(rig);
    rig.record_packets();
    let subscribe = game::ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(b), P_CLIENT);
    rig.hosts[ha as usize].event(subscribe);
    let tickets: Vec<game::ObserverTicket> = packets_to(rig, ha)
        .into_iter()
        .filter_map(|p| match p {
            game::ServerPacket::RegionElsewhere { region, endpoint, ticket } if *region == b && *endpoint == rig_endpoint(hb) => {
                Some(ticket.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(tickets.len(), 1, "one RegionElsewhere for {b:?}, naming host {hb}");
    let ticket = tickets[0].clone();
    assert_eq!(ticket.client, P_CLIENT);
    let mut orders = Vec::new();
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        orders.extend(rig.hosts[hb as usize].manager.take_observer_orders());
        if !orders.is_empty() && rig.peer_unreceived() == 0 {
            break;
        }
    }
    assert_eq!(orders.len(), 1, "one order: {orders:?}");
    let (client, player, nonce, ttl_secs) = orders[0];
    assert_eq!((client, player, nonce, ttl_secs), (P_CLIENT, P_PLAYER, ticket.nonce, game::OBSERVER_ORDER_TTL_SECS));
    // Asked again: the same ticket, no second order.
    let subscribe = game::ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(b), P_CLIENT);
    rig.hosts[ha as usize].event(subscribe);
    let again = packets_to(rig, ha)
        .into_iter()
        .filter(|p| matches!(p, game::ServerPacket::RegionElsewhere { ticket: t, .. } if *t == ticket))
        .count();
    assert_eq!(again, 2, "the same ticket again");
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert!(rig.hosts[hb as usize].manager.take_observer_orders().is_empty(), "no second order");
    }
    assert!(!rig.hosts[ha as usize].manager.running_regions().contains(&b), "never run here");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn subscribe_to_foreign_region_answers_region_elsewhere_and_orders_the_owner() {
    sweep("subscribe_to_foreign_region_answers_region_elsewhere_and_orders_the_owner", 2, 1, SEEDS / 4, subscribe_elsewhere);
}

/// The link to the other host is down: a body past the seam is handed back to the region
/// it left, as before — nothing queued, the home unchanged.
fn unreachable(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    rig.link_down(ha, hb);
    rig.round();
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.player_owners(P_CLIENT), [(ha, a)], "handed back to {a:?}");
    let m = &rig.hosts[ha as usize].manager;
    assert_eq!(m.home_of(P_CLIENT), Some(a));
    assert_eq!(m.session_kind(P_CLIENT), Some(game::SessionKind::Home));
    assert!(m.departed().is_empty());
    assert_eq!(m.peer_unreceived(), 0, "nothing queued to host {hb}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_cannot_enter_an_unreachable_host() {
    sweep("player_cannot_enter_an_unreachable_host", 2, 1, SEEDS / 4, unreachable);
}


// --- Fix round 1: a hand-off never loses or doubles a player ------------------------

/// Host `h` fails to load `rc` now (its store read failed): `rc` is unavailable there
/// for its retry window.
fn fail_load(rig: &mut MultiRig, h: u32, rc: RegionCoords) {
    let r = &mut rig.hosts[h as usize];
    r.manager.spawner_mut().fail_next_start(rc);
    r.event(game::ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(rc), crate::common::rig::CLIENT));
    r.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(rc), crate::common::rig::CLIENT));
    assert!(r.manager.unavailable_regions().contains(&rc), "{rc:?} is unavailable on host {h}");
}

/// Rounds until the test player is parked on some host and nothing is unacknowledged.
fn until_parked(rig: &mut MultiRig) {
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if !rig.parked_on(P_PLAYER).is_empty() && rig.peer_unreceived() == 0 {
            return;
        }
    }
    panic!("the player was never parked: body {:?}", rig.player_owners(P_CLIENT));
}

/// Neither host holds the test client as online or homed.
fn holds_nothing(rig: &MultiRig, h: u32) {
    let m = &rig.hosts[h as usize].manager;
    assert_eq!((m.home_of(P_CLIENT), m.player_of(P_CLIENT)), (None, None), "host {h} holds the player");
}

/// The region a body is handed into is unavailable on the receiving host (its load
/// failed and its retry has not come): the player is parked there from the hand-off —
/// one record, on that host, no body — and its session there is refused.
fn into_unavailable(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.record_packets();
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    fail_load(rig, hb, b);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_parked(rig);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.parked_on(P_PLAYER), [hb], "parked once, on host {hb}");
    assert!(rig.player_owners(P_CLIENT).is_empty(), "no body");
    holds_nothing(rig, ha);
    holds_nothing(rig, hb);
    assert_eq!(rig.hosts[hb as usize].manager.session_kind(P_CLIENT), None, "its session there ended");
    let refused = packets_to(rig, hb)
        .iter()
        .any(|p| matches!(p, game::ServerPacket::Refused(why) if why == game::REGION_UNAVAILABLE_REFUSAL));
    assert!(refused, "the session on host {hb} was told why");
    assert_eq!(rig.hosts[hb as usize].manager.parked_record(P_PLAYER).map(|r| r.region), Some(b));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_into_an_unavailable_region_parks_the_record_once() {
    sweep("handoff_into_an_unavailable_region_parks_the_record_once", 2, 1, SEEDS / 4, into_unavailable);
}

/// The client has no session on the receiving host, whose region fails to load only when
/// the hand-off wakes it: the player is parked there once, and that host does not keep
/// it online or homed (no body) until the grace.
fn fails_after_arrival(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.connect_home(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    rig.hosts[hb as usize].manager.spawner_mut().fail_next_start(b);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_parked(rig);
    assert_eq!(rig.parked_on(P_PLAYER), [hb], "parked once, on host {hb}");
    assert!(rig.player_owners(P_CLIENT).is_empty(), "no body");
    holds_nothing(rig, ha);
    holds_nothing(rig, hb);
    // Nothing more comes of it at the grace.
    rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.parked_on(P_PLAYER), [hb]);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_target_fails_to_load_after_arrival_parks_once() {
    sweep("handoff_target_fails_to_load_after_arrival_parks_once", 2, 1, SEEDS / 4, fails_after_arrival);
}

/// A login on the host the player's body left (the same session id once its observer
/// session ended, or a new one) is refused and places nothing: one body in the world.
fn login_where_it_left(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    rig.disconnect(ha, P_CLIENT);
    rig.record_packets();
    const NEW_CLIENT: game::ClientId = 8;
    for client in [P_CLIENT, NEW_CLIENT] {
        let record = record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]);
        rig.hosts[ha as usize].event(game::ServerEvent::ClientConnected { client, player: P_PLAYER, record: Some(record) });
        for _ in 0..WAIT_ROUNDS {
            rig.round();
        }
        let m = &rig.hosts[ha as usize].manager;
        assert_eq!((m.home_of(client), m.player_of(client), m.session_kind(client)), (None, None, None), "client {client}");
        let seen = rig.hosts[ha as usize].packets_seen.as_ref().expect("recording");
        let refused = seen.iter().any(|(to, p)| {
            *to == Some(client) && matches!(p, game::ServerPacket::Refused(why) if why == game::PLAYER_ELSEWHERE_REFUSAL)
        });
        assert!(refused, "client {client} was refused");
        assert_eq!(rig.player_owners(P_CLIENT), [(hb, b)], "the one body stays on host {hb}");
        assert!(rig.player_owners(NEW_CLIENT).is_empty(), "no second body");
        assert!(rig.parked_on(P_PLAYER).is_empty());
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn login_on_the_host_a_player_left_is_refused_and_places_nothing() {
    sweep("login_on_the_host_a_player_left_is_refused_and_places_nothing", 2, 1, SEEDS / 4, login_where_it_left);
}

/// A->B->A with no session on B: B no longer awaits the client once the body is gone, so
/// an observer connecting to B within the grace stays an observer.
fn ping_pong_sessionless(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_home(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    let mut back = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if let Some((h, rc, id)) = rig.player_body(P_CLIENT) {
            if (h, rc) == (hb, b) {
                assert!(rig.teleport(b, id, past_seam(b, a, REGION_SIZE / 2.0)));
                back = true;
                break;
            }
        }
    }
    assert!(back, "the body never reached {b:?}");
    until_player_settled_in(rig, a);
    rig.hosts[hb as usize].event(game::ServerEvent::ObserverConnected { client: P_CLIENT, player: P_PLAYER });
    let m = &rig.hosts[hb as usize].manager;
    assert_eq!(m.session_kind(P_CLIENT), Some(game::SessionKind::Observer), "no body here: an observer");
    assert_eq!(m.home_of(P_CLIENT), None);
    rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.player_owners(P_CLIENT), [(ha, a)], "still home on host {ha}");
    assert!(rig.parked_on(P_PLAYER).is_empty(), "nothing parked");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn ping_pong_without_a_session_on_the_far_host_leaves_no_awaiting_entry() {
    sweep("ping_pong_without_a_session_on_the_far_host_leaves_no_awaiting_entry", 2, 1, SEEDS / 4, ping_pong_sessionless);
}

/// A coordinator link that is never up: a world request waits for it (a claim pending).
struct NoCoordinator;

impl game::WorldLink for NoCoordinator {
    fn send(&mut self, _msg: game::HostMsg) {}
    fn recv(&mut self) -> Option<game::CoordMsg> {
        None
    }
    fn is_up(&self) -> bool {
        false
    }
}

/// While an embark of the player waits for the coordinator (its claim may change), the
/// body is handed back at the host seam; once the request is answered (here: it times
/// out) it crosses.
fn pending_claim(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    let now = rig.hosts[ha as usize].now_ms;
    rig.hosts[ha as usize].manager.set_world_link(Box::new(NoCoordinator), 1, now);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    let embark = game::EmbarkRequest { civ: game::history::CivId(0), people: Vec::new(), region: a };
    rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::Embark(embark), P_CLIENT));
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.player_owners(P_CLIENT), [(ha, a)], "handed back while the request is pending");
    assert!(rig.hosts[ha as usize].manager.departed().is_empty());
    // The request times out (never sent): nothing is pending any more.
    rig.maintain_all(game::WORLD_REQUEST_TIMEOUT_SECS * 1000);
    rig.round();
    let (_, _, id) = rig.player_body(P_CLIENT).expect("a body");
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    assert_eq!(rig.hosts[ha as usize].manager.departed().get(&P_PLAYER), Some(&hb));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_with_a_pending_world_request_is_clamped_back_until_it_is_answered() {
    sweep(
        "player_with_a_pending_world_request_is_clamped_back_until_it_is_answered",
        2,
        1,
        SEEDS / 4,
        pending_claim,
    );
}

// --- Moves into another host's region (Task 8) ----------------------------------------

/// The site the scripted coordinator founds for every embark.
const EMBARK_SITE: game::history::SiteId = game::history::SiteId(41);
/// The fortress the retiring test player holds.
const RETIRED_SITE: game::history::SiteId = game::history::SiteId(42);

/// What the scripted coordinator was sent, and what it answers.
#[derive(Default)]
struct Script {
    sent: Vec<game::HostMsg>,
    inbox: std::collections::VecDeque<game::CoordMsg>,
    /// Replies kept back while `hold` is set ([`ScriptedCoordinator::release`]).
    held: Vec<game::CoordMsg>,
    hold: bool,
}

/// A coordinator stand-in on one host's world link (the multi-host rig has no
/// coordinator): it echoes every `Attach` and answers every embark with
/// [`EMBARK_SITE`] founded in the region asked for.
#[derive(Clone, Default)]
struct ScriptedCoordinator(std::sync::Arc<std::sync::Mutex<Script>>);

impl ScriptedCoordinator {
    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The held replies go out (the next world pump delivers them).
    fn release(&self) {
        let mut s = self.script();
        s.hold = false;
        let held = std::mem::take(&mut s.held);
        s.inbox.extend(held);
    }

    /// How often the host gave `player`'s claim back.
    fn released_claims(&self, player: game::PlayerId) -> usize {
        self.script().sent.iter().filter(|m| matches!(m, game::HostMsg::ReleaseClaim { player: p } if *p == player)).count()
    }
}

impl game::WorldLink for ScriptedCoordinator {
    fn send(&mut self, msg: game::HostMsg) {
        let mut s = self.script();
        match &msg {
            game::HostMsg::Attach { epoch, .. } => s.inbox.push_back(game::CoordMsg::Attached { epoch: *epoch }),
            game::HostMsg::Request { id, req: game::WorldRequest::Embark(r), .. } => {
                let reply = game::WorldReply::Embarked { site: EMBARK_SITE, region: r.region };
                let msg = game::CoordMsg::Reply { id: *id, reply };
                if s.hold {
                    s.held.push(msg);
                } else {
                    s.inbox.push_back(msg);
                }
            }
            _ => {}
        }
        s.sent.push(msg);
    }
    fn recv(&mut self) -> Option<game::CoordMsg> {
        self.script().inbox.pop_front()
    }
    fn is_up(&self) -> bool {
        true
    }
}

/// Host `h`'s world link is a [`ScriptedCoordinator`] (replies held back if `hold`).
fn install_coordinator(rig: &mut MultiRig, h: u32, hold: bool) -> ScriptedCoordinator {
    let link = ScriptedCoordinator::default();
    link.script().hold = hold;
    let now = rig.hosts[h as usize].now_ms;
    rig.hosts[h as usize].manager.set_world_link(Box::new(link.clone()), 1, now);
    link
}

/// The test player asks host `h` to embark in `region`.
fn embark_into(rig: &mut MultiRig, h: u32, region: RegionCoords) {
    let req = game::EmbarkRequest { civ: game::history::CivId(0), people: Vec::new(), region };
    rig.hosts[h as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::Embark(req), P_CLIENT));
}

/// The test player standing in the middle of `rc`.
fn centre_of(rc: RegionCoords) -> game::PlayerRecord {
    record_at(rc, [REGION_SIZE / 2.0, REGION_SIZE / 2.0])
}

/// The test player's records in `cut`.
fn records_in(cut: &game::WorldCut) -> Vec<&game::PlayerRecord> {
    cut.players.iter().filter(|(p, _)| *p == P_PLAYER).map(|(_, r)| r).collect()
}

/// Whether `cut` has an in-flight row carrying a player's body.
fn has_player_row(cut: &game::WorldCut) -> bool {
    cut.in_flight.iter().any(|row| matches!(row, game::InFlightRow::Transfer { bundle, .. } if bundle.client.is_some()))
}

/// Rounds until host `h` has handed the test player away (`departed`).
fn until_departed(rig: &mut MultiRig, h: u32) {
    for _ in 0..CROSS_ROUNDS {
        if rig.hosts[h as usize].manager.departed().contains_key(&P_PLAYER) {
            return;
        }
        rig.round();
    }
    panic!("host {h} never handed the player away");
}

/// The test player moved from host `ha` into `b` on host `hb` and stays there: one body,
/// the home and the claim (`claim`) on `hb` only, an observer left on `ha`, which sent
/// `RegionElsewhere` for `b` (never `PlayerRegion`), while `hb` sent `PlayerRegion`.
fn assert_moved(rig: &mut MultiRig, ha: u32, hb: u32, b: RegionCoords, claim: Option<game::Claim>) {
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert_eq!(rig.player_owners(P_CLIENT), [(hb, b)], "one body, in {b:?}, staying");
    }
    assert!(rig.parked_on(P_PLAYER).is_empty(), "nothing parked");
    let (ma, mb) = (&rig.hosts[ha as usize].manager, &rig.hosts[hb as usize].manager);
    assert_eq!((ma.home_of(P_CLIENT), mb.home_of(P_CLIENT)), (None, Some(b)), "the home moved");
    assert_eq!(ma.session_kind(P_CLIENT), Some(game::SessionKind::Observer));
    assert_eq!(mb.session_kind(P_CLIENT), Some(game::SessionKind::Home));
    assert_eq!((ma.player_of(P_CLIENT), mb.player_of(P_CLIENT)), (None, Some(P_PLAYER)));
    assert_eq!((ma.client_claim(P_CLIENT), mb.client_claim(P_CLIENT)), (None, claim), "the claim went along");
    assert_eq!(ma.departed().get(&P_PLAYER), Some(&hb), "host {ha} handed the player to host {hb}");
    assert!(mb.departed().is_empty());
    let region = |p: &&game::ServerPacket| matches!(p, game::ServerPacket::PlayerRegion(Some(r), P_CLIENT) if *r == b);
    assert!(packets_to(rig, hb).iter().any(region), "host {hb} told the client its new home");
    assert!(!packets_to(rig, ha).iter().any(region), "host {ha} never claims the body is in {b:?}");
    let elsewhere = packets_to(rig, ha)
        .iter()
        .filter(|p| matches!(p, game::ServerPacket::RegionElsewhere { region, endpoint, .. } if *region == b && *endpoint == rig_endpoint(hb)))
        .count();
    assert!(elsewhere >= 1, "host {ha} sent the client on to host {hb}");
}

/// Embark into a region another host owns: the player is parked out of its region here
/// and handed over with its claim ([`game::Reliable::PlayerMove`]); its body is created
/// there once.
fn embark_foreign(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    install_coordinator(rig, ha, false);
    rig.record_packets();
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    until_body(rig);
    embark_into(rig, ha, b);
    until_player_settled_in(rig, b);
    assert_moved(rig, ha, hb, b, Some(game::Claim::Fortress(EMBARK_SITE)));
    let ok = packets_to(rig, ha).iter().any(|p| matches!(p, game::ServerPacket::EmbarkResult(Ok(s)) if *s == EMBARK_SITE));
    assert!(ok, "the embark was granted");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn embark_into_a_foreign_host_moves_the_player_once() {
    sweep("embark_into_a_foreign_host_moves_the_player_once", 2, 1, SEEDS / 4, embark_foreign);
}

/// A region near the origin host `h` owns.
fn region_of(rig: &MultiRig, h: u32) -> RegionCoords {
    (1..64).map(|x| RegionCoords::new(x, 0)).find(|rc| rig.owner(*rc) == h).expect("a region of the host near the origin")
}

/// The test player holding [`RETIRED_SITE`] in a region of the host that does NOT own
/// the spawn region: `(its region, its host, the spawn host)`.
fn retire_setup(rig: &mut MultiRig) -> (RegionCoords, u32, u32) {
    let hs = rig.owner(game::SPAWN_REGION);
    let ha = 1 - hs;
    let a = region_of(rig, ha);
    rig.run(a);
    rig.record_packets();
    let record = game::PlayerRecord { fortress: Some(RETIRED_SITE), ..centre_of(a) };
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record);
    until_body(rig);
    assert_eq!(rig.hosts[ha as usize].manager.client_claim(P_CLIENT), Some(game::Claim::Fortress(RETIRED_SITE)));
    (a, ha, hs)
}

fn retire(rig: &mut MultiRig, h: u32) {
    rig.hosts[h as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::Retire, P_CLIENT));
}

/// Retire to a spawn region another host owns: the fortress is given up here, the player
/// moves to the spawn host with no claim.
fn retire_foreign(rig: &mut MultiRig) {
    let (_, ha, hs) = retire_setup(rig);
    retire(rig, ha);
    until_player_settled_in(rig, game::SPAWN_REGION);
    assert_moved(rig, ha, hs, game::SPAWN_REGION, None);
    let told = packets_to(rig, ha).iter().any(|p| matches!(p, game::ServerPacket::Fortress(None)));
    assert!(told, "the client was told it holds no fortress");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn retire_to_a_foreign_spawn_region() {
    sweep("retire_to_a_foreign_spawn_region", 2, 1, SEEDS / 4, retire_foreign);
}

/// The other host becomes unreachable while the embark is with the coordinator: the
/// granted embark is refused when it is answered — before anything is parked — and the
/// claim it granted is given back; the body stays where it is.
fn move_to_unreachable(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    let coordinator = install_coordinator(rig, ha, true);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    let (_, _, id) = until_body(rig);
    rig.record_packets();
    embark_into(rig, ha, b);
    assert_eq!(rig.hosts[ha as usize].manager.pending_world_requests(), 1, "asked while host {hb} is reachable");
    rig.link_down(ha, hb);
    for _ in 0..WAIT_ROUNDS {
        if !rig.hosts[ha as usize].manager.reaches(b) {
            break;
        }
        rig.round();
    }
    assert!(!rig.hosts[ha as usize].manager.reaches(b), "host {hb} is unreachable");
    coordinator.release();
    rig.round();
    let refused = packets_to(rig, ha).iter().any(|p| {
        matches!(p, game::ServerPacket::EmbarkResult(Err(game::history::EmbarkRefusal::RegionUnavailable)))
    });
    assert!(refused, "the embark was refused: {:?}", packets_to(rig, ha));
    assert_eq!(coordinator.released_claims(P_PLAYER), 1, "the claim the answer granted is given back");
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert_eq!(rig.player_owners(P_CLIENT), [(ha, a)], "the body stays in {a:?}");
    }
    assert_eq!(rig.player_body(P_CLIENT).map(|(_, _, i)| i), Some(id), "the same body");
    let m = &rig.hosts[ha as usize].manager;
    assert_eq!((m.home_of(P_CLIENT), m.session_kind(P_CLIENT)), (Some(a), Some(game::SessionKind::Home)));
    assert_eq!(m.client_claim(P_CLIENT), None, "no fortress held");
    assert!(m.departed().is_empty());
    assert_eq!(m.peer_unreceived(), 0, "nothing queued to host {hb}");
    assert!(rig.parked_on(P_PLAYER).is_empty());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn move_to_unreachable_host_is_refused_before_parking() {
    sweep("move_to_unreachable_host_is_refused_before_parking", 2, 1, SEEDS / 4, move_to_unreachable);
}

/// The test player (on host `ha`, every host observing it if `everywhere`, else a session
/// on `ha` only) embarks into `b` on host `hb`; the link drops once the move's park went
/// out, and the move is queued on `ha`. `(b, ha, hb)`.
fn move_queued_link_down(rig: &mut MultiRig, everywhere: bool) -> (RegionCoords, u32, u32) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    let coordinator = install_coordinator(rig, ha, true);
    rig.record_packets();
    if everywhere {
        rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    } else {
        rig.connect_home(ha, P_CLIENT, P_PLAYER, centre_of(a));
    }
    until_body(rig);
    embark_into(rig, ha, b);
    coordinator.release();
    // The answer is applied (the park goes to `a`), then the link drops.
    let now = rig.hosts[ha as usize].now_ms;
    rig.hosts[ha as usize].manager.pump_world(now);
    rig.link_down(ha, hb);
    until_departed(rig, ha);
    (b, ha, hb)
}

/// The link drops after the move's park went out: the player's record waits in the peer
/// queue (no record parked here), a cut of this host holds it once — region the target,
/// no in-flight row — and once the link is back the player arrives once and this host's
/// cuts write it no more.
fn drop_after_park(rig: &mut MultiRig) {
    let (b, ha, hb) = move_queued_link_down(rig, true);
    assert!(rig.player_owners(P_CLIENT).is_empty(), "no body: the record is on its way");
    assert!(rig.parked_on(P_PLAYER).is_empty(), "nothing parked on either host");
    assert!(rig.hosts[ha as usize].manager.peer_unreceived() > 0, "the move waits for host {hb}");
    let cut = cut_on(rig, ha as usize);
    let records = records_in(&cut);
    assert_eq!(records.len(), 1, "the cut holds the player once");
    assert_eq!((records[0].region, records[0].fortress), (b, Some(EMBARK_SITE)), "where it is headed, with its claim");
    assert!(!has_player_row(&cut), "no in-flight row for it");
    rig.link_up(ha, hb);
    until_player_settled_in(rig, b);
    assert_moved(rig, ha, hb, b, Some(game::Claim::Fortress(EMBARK_SITE)));
    let later = cut_on(rig, ha as usize);
    assert!(records_in(&later).is_empty(), "host {hb} holds it now; host {ha} writes it no more");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn peer_drops_after_the_park_record_waits_and_the_cut_holds_it_once() {
    sweep("peer_drops_after_the_park_record_waits_and_the_cut_holds_it_once", 2, 1, SEEDS / 4, drop_after_park);
}

/// After the move, the client's session on the host it left (an observer) ends: nothing
/// is parked there or anywhere — the body on the other host stays.
fn observer_disconnect(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    install_coordinator(rig, ha, false);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    until_body(rig);
    embark_into(rig, ha, b);
    until_player_settled_in(rig, b);
    rig.disconnect(ha, P_CLIENT);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert_eq!(rig.player_owners(P_CLIENT), [(hb, b)], "the body stays on host {hb}");
    }
    assert!(rig.parked_on(P_PLAYER).is_empty(), "nothing parked");
    assert_eq!(rig.hosts[ha as usize].manager.session_kind(P_CLIENT), None);
    assert_eq!(rig.hosts[hb as usize].manager.session_kind(P_CLIENT), Some(game::SessionKind::Home));
    assert_eq!(rig.peer_unreceived(), 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn observer_disconnect_parks_nothing() {
    sweep("observer_disconnect_parks_nothing", 2, 1, SEEDS / 4, observer_disconnect);
}

/// A retire to a foreign spawn region whose link drops after the park: this host's cut
/// holds a record in a super-region it does not own, and nothing else of another host's
/// — the store's commit check is its own claim (`world_store::fence`: the super-regions
/// the cut's regions touch and every one it claimed), so the cut commits; once the
/// player is across, later cuts do not write it.
fn foreign_record_cut(rig: &mut MultiRig) {
    let (_, ha, hs) = retire_setup(rig);
    let spawn = game::SPAWN_REGION;
    retire(rig, ha);
    rig.link_down(ha, hs);
    until_departed(rig, ha);
    let cut = cut_on(rig, ha as usize);
    let records = records_in(&cut);
    assert_eq!(records.len(), 1, "the cut holds the player once");
    assert_eq!((records[0].region, records[0].fortress), (spawn, None));
    assert_ne!(rig.owner(spawn), ha, "the record's region is another host's");
    assert!(
        cut.regions.iter().all(|(rc, _)| rig.owner(*rc) == ha),
        "the cut writes only host {ha}'s regions: its commit check is host {ha}'s own claim (no Fenced)"
    );
    rig.link_up(ha, hs);
    until_player_settled_in(rig, spawn);
    let later = cut_on(rig, ha as usize);
    assert!(records_in(&later).is_empty(), "host {hs} holds it now");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_with_a_foreign_record_commits() {
    sweep("cut_with_a_foreign_record_commits", 2, 1, SEEDS / 4, foreign_record_cut);
}

/// One [`MultiRig::round`] with `between` run after every host handled its regions'
/// outputs and before any peer session is pumped.
fn round_with(rig: &mut MultiRig, between: impl FnOnce(&mut MultiRig)) {
    for r in &mut rig.hosts {
        r.now_ms += game::TICK_RATE;
        r.manager.spawner_mut().tick_all();
    }
    for r in &mut rig.hosts {
        r.settle();
    }
    between(rig);
    for r in &mut rig.hosts {
        let now = r.now_ms;
        r.manager.pump_peers(now);
    }
    for r in &mut rig.hosts {
        r.settle();
    }
    rig.assert_conserved_world();
}

/// A walking hand-off queued and never received (the link drops before it is sent): the
/// sender's one-phase cut holds the player as a record, once, in the target region — not
/// as an in-flight row — and a host restored from that cut queues no hand-off.
fn handoff_in_transit_cut(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    let mut handed = false;
    for _ in 0..CROSS_ROUNDS {
        round_with(rig, |rig| {
            if rig.hosts[ha as usize].manager.departed().contains_key(&P_PLAYER) {
                handed = true;
                rig.link_down(ha, hb);
            }
        });
        if handed {
            break;
        }
    }
    assert!(handed, "host {ha} never handed the body off");
    let cut = cut_on(rig, ha as usize);
    let records = records_in(&cut);
    assert_eq!(records.len(), 1, "the cut holds the player once");
    assert_eq!(records[0].region, b, "in the region it is headed for");
    assert!(!has_player_row(&cut), "no in-flight row for the body");
    let mut store = CutStore::default();
    store.apply(&cut);
    assert!(store.places(id).is_empty(), "the body is in no stored region or row");

    let mut restored = MultiRig::new(2, 1, None, 0, 0);
    restored.run(a);
    restored.run(b);
    restored.hosts[ha as usize].manager.restore_in_flight(cut.in_flight.clone());
    assert!(restored.hosts[ha as usize].manager.peer_unreceived_ids().is_empty(), "no hand-off queued again");
    for _ in 0..WAIT_ROUNDS {
        restored.round();
    }
    assert!(restored.player_body(P_CLIENT).is_none(), "no body: the stored record places it at the next login");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_holds_a_player_in_transit_as_a_record_once() {
    sweep("cut_holds_a_player_in_transit_as_a_record_once", 2, 1, SEEDS / 4, handoff_in_transit_cut);
}

/// A body handed to a host the client never connects to is parked there after the grace;
/// that host tells the host it came from, which ends its observer session of the client
/// (`PLAYER_PARKED_ELSEWHERE`) exactly once and keeps pointing at the far host.
fn far_park_told(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.record_packets();
    rig.connect_home(ha, P_CLIENT, P_PLAYER, centre_of(a));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    assert_eq!(rig.hosts[ha as usize].manager.session_kind(P_CLIENT), Some(game::SessionKind::Observer));
    let mut parked = false;
    for _ in 0..CROSS_ROUNDS {
        rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS / 2);
        rig.round();
        if !rig.parked_on(P_PLAYER).is_empty() {
            parked = true;
            break;
        }
    }
    assert!(parked, "never parked on host {hb}");
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if rig.peer_unreceived() == 0 {
            break;
        }
    }
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    let told = packets_to(rig, ha)
        .iter()
        .filter(|p| matches!(p, game::ServerPacket::Refused(why) if why == game::PLAYER_PARKED_ELSEWHERE))
        .count();
    assert_eq!(told, 1, "host {ha}'s observer session was told once");
    let m = &rig.hosts[ha as usize].manager;
    assert_eq!(m.session_kind(P_CLIENT), None, "and ended");
    assert_eq!(m.departed().get(&P_PLAYER), Some(&hb), "still pointing at host {hb}");
    assert_eq!(rig.parked_on(P_PLAYER), [hb], "parked once, on host {hb}");
    assert!(rig.player_owners(P_CLIENT).is_empty());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn client_is_told_when_the_far_host_parks_its_body() {
    sweep("client_is_told_when_the_far_host_parks_its_body", 2, 1, SEEDS / 4, far_park_told);
}

// --- Fix round 1 ------------------------------------------------------------------------

/// A second session id of the test player: a fresh login.
const LOGIN_CLIENT: game::ClientId = 8;

/// Every body of the test player, under either session id: `(host, region)`.
fn player_bodies_any(rig: &mut MultiRig) -> Vec<(u32, RegionCoords)> {
    let mut out = rig.player_owners(P_CLIENT);
    out.extend(rig.player_owners(LOGIN_CLIENT));
    out
}

/// After a login placed the player on host `hb` from the record host `ha`'s cut wrote,
/// the link comes back and the copy queued on `ha` arrives: it is dropped — the body the
/// login made is the only one, and nothing is parked.
fn after_login_the_copy_is_dropped(rig: &mut MultiRig, cut: game::WorldCut, b: RegionCoords, ha: u32, hb: u32) {
    let record = records_in(&cut).first().map(|r| (*r).clone()).expect("the cut holds the player");
    assert_eq!(record.region, b);
    rig.connect_home(hb, LOGIN_CLIENT, P_PLAYER, record);
    for _ in 0..BODY_ROUNDS {
        if !rig.player_owners(LOGIN_CLIENT).is_empty() {
            break;
        }
        rig.round();
    }
    assert_eq!(rig.player_owners(LOGIN_CLIENT), [(hb, b)], "the login placed the player");
    rig.link_up(ha, hb);
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if rig.peer_unreceived() == 0 {
            break;
        }
    }
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert_eq!(player_bodies_any(rig), [(hb, b)], "one body, the one the login made");
    }
    assert!(rig.parked_on(P_PLAYER).is_empty(), "nothing parked");
    let mb = &rig.hosts[hb as usize].manager;
    assert_eq!(mb.player_of(LOGIN_CLIENT), Some(P_PLAYER));
    assert_eq!(mb.player_of(P_CLIENT), None, "the arriving copy placed nothing");
    assert_eq!(mb.home_of(P_CLIENT), None);
    assert!(mb.departed().is_empty());
}

fn move_after_login(rig: &mut MultiRig) {
    let (b, ha, hb) = move_queued_link_down(rig, true);
    let cut = cut_on(rig, ha as usize);
    after_login_the_copy_is_dropped(rig, cut, b, ha, hb);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn move_arriving_after_a_login_placed_the_player_is_dropped() {
    sweep("move_arriving_after_a_login_placed_the_player_is_dropped", 2, 1, SEEDS / 4, move_after_login);
}

fn handoff_after_login(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    let mut handed = false;
    for _ in 0..CROSS_ROUNDS {
        round_with(rig, |rig| {
            if rig.hosts[ha as usize].manager.departed().contains_key(&P_PLAYER) {
                handed = true;
                rig.link_down(ha, hb);
            }
        });
        if handed {
            break;
        }
    }
    assert!(handed, "host {ha} never handed the body off");
    let cut = cut_on(rig, ha as usize);
    after_login_the_copy_is_dropped(rig, cut, b, ha, hb);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_arriving_for_a_player_already_here_is_dropped() {
    sweep("handoff_arriving_for_a_player_already_here_is_dropped", 2, 1, SEEDS / 4, handoff_after_login);
}

/// The target region could not be loaded on its owner: the move arriving there is parked
/// on that host, and with no session of the client there the sender is told
/// (`PlayerParked`): its observer session of the client ends with
/// `PLAYER_PARKED_ELSEWHERE`, once.
fn move_into_unavailable(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.host_of(b).manager.spawner_mut().fail_next_start(b);
    rig.host_of(b).event(game::ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(b), crate::common::rig::CLIENT));
    for _ in 0..WAIT_ROUNDS {
        if rig.hosts[hb as usize].manager.unavailable_regions().contains(&b) {
            break;
        }
        rig.round();
    }
    assert!(rig.hosts[hb as usize].manager.unavailable_regions().contains(&b), "{b:?} failed to load");
    install_coordinator(rig, ha, false);
    rig.record_packets();
    rig.connect_home(ha, P_CLIENT, P_PLAYER, centre_of(a));
    until_body(rig);
    embark_into(rig, ha, b);
    let mut parked = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if !rig.parked_on(P_PLAYER).is_empty() && rig.peer_unreceived() == 0 {
            parked = true;
            break;
        }
    }
    assert!(parked, "never parked: bodies {:?}", rig.player_owners(P_CLIENT));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.parked_on(P_PLAYER), [hb], "parked once, on host {hb}");
    assert_eq!(rig.hosts[hb as usize].manager.parked_record(P_PLAYER).map(|r| r.region), Some(b));
    assert!(rig.player_owners(P_CLIENT).is_empty());
    let told = packets_to(rig, ha)
        .iter()
        .filter(|p| matches!(p, game::ServerPacket::Refused(why) if why == game::PLAYER_PARKED_ELSEWHERE))
        .count();
    assert_eq!(told, 1, "host {ha}'s observer session was told once");
    assert_eq!(rig.hosts[ha as usize].manager.session_kind(P_CLIENT), None);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn move_into_an_unavailable_region_parks_and_tells_the_sender() {
    sweep("move_into_an_unavailable_region_parks_and_tells_the_sender", 2, 1, SEEDS / 4, move_into_unavailable);
}

/// Ending an adventure moves the player to a spawn region another host owns: back in
/// Fortress mode there, no claim anywhere.
fn end_adventure_foreign(rig: &mut MultiRig) {
    let hs = rig.owner(game::SPAWN_REGION);
    let ha = 1 - hs;
    let a = region_of(rig, ha);
    rig.run(a);
    rig.record_packets();
    let client = game::Client { mode: game::PlayMode::Adventure, ..game::Client::default() };
    let record = game::PlayerRecord { client, adventure: Some(RETIRED_SITE), ..centre_of(a) };
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record);
    until_body(rig);
    assert_eq!(rig.hosts[ha as usize].manager.client_claim(P_CLIENT), Some(game::Claim::Adventure(RETIRED_SITE)));
    rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::EndAdventure, P_CLIENT));
    until_player_settled_in(rig, game::SPAWN_REGION);
    assert_moved(rig, ha, hs, game::SPAWN_REGION, None);
    let mut mode = None;
    rig.hosts[hs as usize].manager.spawner_mut().with_region(game::SPAWN_REGION, |region| {
        mode = region.data().clients.get(&P_CLIENT).map(|c| c.mode);
    });
    assert_eq!(mode, Some(game::PlayMode::Fortress), "back in Fortress mode");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn end_adventure_to_a_foreign_spawn_region() {
    sweep("end_adventure_to_a_foreign_spawn_region", 2, 1, SEEDS / 4, end_adventure_foreign);
}

/// The client disconnects from both hosts while its move waits for a down link: once the
/// link is back the move arrives with no session, and the player is parked once, on the
/// target's host.
fn disconnect_while_queued(rig: &mut MultiRig) {
    let (b, ha, hb) = move_queued_link_down(rig, true);
    rig.disconnect(ha, P_CLIENT);
    rig.disconnect(hb, P_CLIENT);
    rig.round();
    assert!(rig.parked_on(P_PLAYER).is_empty(), "nothing parked while the move waits");
    rig.link_up(ha, hb);
    let mut parked = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS);
        if !rig.parked_on(P_PLAYER).is_empty() && rig.player_owners(P_CLIENT).is_empty() && rig.peer_unreceived() == 0 {
            parked = true;
            break;
        }
    }
    assert!(parked, "never parked: bodies {:?}, parked on {:?}", rig.player_owners(P_CLIENT), rig.parked_on(P_PLAYER));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.parked_on(P_PLAYER), [hb], "parked once, on host {hb}");
    assert_eq!(rig.hosts[hb as usize].manager.parked_record(P_PLAYER).map(|r| r.region), Some(b));
    assert!(rig.player_owners(P_CLIENT).is_empty());
    for h in [ha, hb] {
        let m = &rig.hosts[h as usize].manager;
        assert_eq!((m.home_of(P_CLIENT), m.session_kind(P_CLIENT)), (None, None), "host {h} holds nothing of it");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn disconnect_while_the_move_is_queued_parks_once_on_the_target() {
    sweep("disconnect_while_the_move_is_queued_parks_once_on_the_target", 2, 1, SEEDS / 4, disconnect_while_queued);
}
