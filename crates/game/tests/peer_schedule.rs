//! Entities crossing between region hosts (`world_manager/peers.rs`) under seeded
//! scheduling jitter and link delay ([`MultiRig`]): every scenario runs in lockstep with
//! every link delay 0..=3, then `SEEDS` seeds of each schedule mode (`shuffled`, `late`,
//! `jittery`, the link delay rolling through 0..=3 with the seed). Every round checks
//! conservation across hosts ([`MultiRig::assert_conserved_world`]); a failure names a
//! replayable `(mode, seed, delay)`. Run via `just test-threads`.
use game::{InlineSchedule, RegionCoords, RegionSpawner, FLIP_HYSTERESIS, REGION_SIZE};

use crate::common::cut_store::CutStore;
use crate::common::multi_rig::{four_host_corner, host_seam, rig_endpoint, MultiRig};
use crate::common::rig::{assert_same_streams, Streams};

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

/// [`sweep`] for a scenario that reports whether its case happened (a seed may miss its
/// timing window): every mode (lockstep at every link delay counts as one) must hit it at
/// least once.
fn sweep_hits(name: &str, hosts: u32, s: u32, seeds: u64, scenario: fn(&mut MultiRig) -> bool) {
    let run = |mode: &str, schedule: Option<fn(u64) -> InlineSchedule>, seed: u64, delay: u32| -> bool {
        std::panic::catch_unwind(|| {
            let mut rig = MultiRig::new(hosts, s, schedule, seed, delay);
            scenario(&mut rig)
        })
        .unwrap_or_else(|_| panic!("{name} broke under `{mode}`, seed {seed}, link delay {delay}"))
    };
    let hits: usize = (0..=MAX_LINK_DELAY).map(|d| usize::from(run("lockstep", None, 0, d))).sum();
    assert!(hits > 0, "{name}: its case never happened in lockstep");
    for (mode, schedule) in MODES {
        let hits: usize =
            (0..seeds).map(|seed| usize::from(run(mode, Some(schedule), seed, (seed % u64::from(MAX_LINK_DELAY + 1)) as u32))).sum();
        assert!(hits > 0, "{name}: its case never happened under `{mode}`");
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

/// Units inside its region's seam a [`two_sources_into_one`] slime stands.
const MIRROR_INSET: f32 = 4.0;

/// Two slimes on one host stand by the seams of two of its regions with one region of
/// the other host, so both mirror into it every tick: neighbour messages whose order
/// within a slot the sending host's schedule permutes. Then one of them crosses.
fn two_sources_into_one(rig: &mut MultiRig) {
    let steps = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    let mut near: Vec<RegionCoords> = (-8..=8).flat_map(|x| (-8..=8).map(move |z| RegionCoords::new(x, z))).collect();
    near.sort_by_key(|rc| (rc.x.abs() + rc.z.abs(), rc.x, rc.z));
    let (b, a, c) = near
        .iter()
        .find_map(|&b| {
            let other: Vec<RegionCoords> = steps
                .iter()
                .map(|(dx, dz)| RegionCoords::new(b.x + dx, b.z + dz))
                .filter(|&n| rig.owner(n) != rig.owner(b))
                .collect();
            let a = *other.first()?;
            let c = other[1..].iter().copied().find(|&c| rig.owner(c) == rig.owner(a))?;
            Some((b, a, c))
        })
        .expect("a region with two neighbours on one other host");
    for rc in [a, b, c] {
        rig.run(rc);
    }
    // Just inside `from`'s seam with `b`, mid-seam.
    let inside = |from: RegionCoords| {
        let across = |d: i32| match d {
            1 => REGION_SIZE - MIRROR_INSET,
            -1 => MIRROR_INSET,
            _ => REGION_SIZE / 2.0,
        };
        [across(b.x - from.x), crate::common::above_floor_y(), across(b.z - from.z)]
    };
    rig.spawn_slime(a, inside(a));
    let id = rig.spawn_slime(c, inside(c));
    // Both also drop piles on `b`'s blocks by the seam: two sources' intents in one slot.
    let n = game::REGION_BLOCKS as i32;
    for from in [a, c] {
        let span = |d: i32| match d {
            1 => [n, n + 1],
            -1 => [-1, -2],
            _ => [n / 2, n / 2 + 1],
        };
        let (xs, zs) = (span(b.x - from.x), span(b.z - from.z));
        rig.host_of(from).manager.spawner_mut().with_region(from, |region| {
            let tick = region.current_tick() as u64;
            region.with_data(|d| {
                for i in 0..2 {
                    let cell = [xs[i], crate::common::STAND, zs[i]];
                    let stack = game::ItemStack { item: game::ItemId(1), count: 3 };
                    assert_eq!(d.deposit_items_safe(cell, stack, tick, game::PICKUP_DELAY_TICKS), 0, "{from:?} {cell:?}");
                }
            });
        });
    }
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert!(rig.teleport(c, id, past_seam(c, b, REGION_SIZE / 2.0)), "the slime is still in {c:?}");
    until_settled_in(rig, id, b);
}

/// Every host's region fingerprints and event streams after `scenario` on a two-host
/// rig at link delay 0, under `schedule` (or lockstep).
fn two_host_record(
    schedule: Option<fn(u64) -> InlineSchedule>,
    seed: u64,
    scenario: fn(&mut MultiRig),
) -> Vec<(Vec<(RegionCoords, u64, u32)>, Streams)> {
    let mut rig = MultiRig::new(2, 1, schedule, seed, 0);
    for host in &mut rig.hosts {
        host.event_streams = Some(Default::default());
    }
    scenario(&mut rig);
    rig.hosts.iter_mut().map(|h| (h.fingerprint(), h.event_streams.take().expect("recording"))).collect()
}

/// Permuted tick order on both hosts only permutes the order neighbour messages
/// reach a region within a slot: applied in canonical order, every host ends
/// bit-identical to lockstep, with the same event streams.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn shuffled_two_hosts_match_lockstep() {
    let scenarios: [(&str, fn(&mut MultiRig)); 2] = [("cross_once", cross_once), ("two_sources_into_one", two_sources_into_one)];
    for (name, scenario) in scenarios {
        for seed in 0..SEEDS {
            let lockstep = two_host_record(None, seed, scenario);
            let shuffled = std::panic::catch_unwind(|| two_host_record(Some(InlineSchedule::shuffled), seed, scenario))
                .unwrap_or_else(|_| panic!("{name} broke under `shuffled`, seed {seed}"));
            for (h, ((lf, ls), (sf, ss))) in lockstep.iter().zip(&shuffled).enumerate() {
                let what = format!("{name}: host {h}, shuffled seed {seed} vs lockstep");
                assert_same_streams(&what, ls, ss);
                assert_eq!(lf, sf, "{what}: fingerprints differ");
            }
        }
    }
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

/// Round budget for a halo change to show across a host seam (a delta is applied
/// at the end of its slot, right before the target's next tick).
const HALO_ROUNDS: usize = 9;
/// Items a deposit test puts in.
const DEPOSIT: u32 = 5;
/// The dirt item of the block manifest.
const DIRT: game::ItemId = game::ItemId(1);

/// The block at `cell` of region `of` as running region `rc` sees it (own ∪ halo):
/// `None` is air, or outside own ∪ halo.
fn seen_from(rig: &mut MultiRig, rc: RegionCoords, of: RegionCoords, cell: [i32; 3]) -> Option<game::BlockId> {
    let mut m = None;
    let p = game::WorldBlockPos::from_local(of, game::BlockPos::new(cell[0], cell[1], cell[2]));
    rig.host_of(rc).manager.spawner_mut().with_region(rc, |r| {
        m = r.data().halo_block(p).map(|b| b.id).filter(|&id| id != game::BlockId::AIR)
    });
    m
}

/// The floor block of `a` on its edge facing the adjacent `b` (the second block in from
/// the seam), mid-way along that edge.
fn border_block(a: RegionCoords, b: RegionCoords) -> [i32; 3] {
    let n = game::REGION_BLOCKS as i32;
    let across = |d: i32| match d {
        1 => Some(n - 2),
        -1 => Some(1),
        _ => None,
    };
    let x = across(b.x - a.x).unwrap_or(n / 2);
    let z = across(b.z - a.z).unwrap_or(n / 2);
    [x, crate::common::STAND - 1, z]
}

/// The block beside `cell` along its edge (the next one in x, or in z on an x seam).
fn beside(a: RegionCoords, b: RegionCoords, cell: [i32; 3]) -> [i32; 3] {
    if b.x != a.x {
        [cell[0], cell[1], cell[2] + 1]
    } else {
        [cell[0] + 1, cell[1], cell[2]]
    }
}

/// Breaks block `cell` of running `rc` between ticks.
fn dig(rig: &mut MultiRig, rc: RegionCoords, cell: [i32; 3]) {
    rig.host_of(rc).manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| assert!(d.set_block_safe(cell, game::Block::new(game::BlockId::AIR)), "{rc:?} {cell:?} dug"));
    });
}

/// The mirror running `rc` holds of entity `id`, if any.
fn mirror_in(rig: &mut MultiRig, rc: RegionCoords, id: game::EntityId) -> bool {
    let mut m = false;
    rig.host_of(rc).manager.spawner_mut().with_region(rc, |r| m = r.data().halo_entity(id).is_some());
    m
}

/// A spot in `a`'s frame inside its edge band facing the adjacent `b` (1.5 blocks short
/// of the seam), mid-way along it.
fn in_band(a: RegionCoords, b: RegionCoords) -> [f32; 3] {
    let inset = 1.5 * game::BLOCK_VOXELS as f32;
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
    let block = border_block(a, b);
    assert!(until(rig, HALO_ROUNDS, |r| seen_from(r, b, a, block).is_some()), "b's halo holds a's ground");
    dig(rig, a, block);
    assert!(until(rig, HALO_ROUNDS, |r| seen_from(r, b, a, block).is_none()), "the dig never reached b's halo");
    // The one-block floor has nothing under the dug block: the floor beside it stands.
    assert!(seen_from(rig, b, a, beside(a, b, block)).is_some(), "the block beside it still stands");
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
    let block = border_block(a, b);
    dig(rig, a, block);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert!(seen_from(rig, b, a, block).is_some(), "nothing crosses a down link");
    rig.link_up(ha, hb);
    assert!(
        until(rig, CROSS_ROUNDS, |r| seen_from(r, b, a, block).is_none() && mirror_in(r, b, id)),
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
    dig(rig, a, border_block(a, b));
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
    let owner = |rc: RegionCoords| game::Static { ordinal: 0, hosts }.owner_of(rc, s);
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
    let block = border_block(a, b);
    dig(rig, a, block);
    rig.round();
    rig.park(a);
    rig.link_up(ha, hb);
    assert!(
        until(rig, CROSS_ROUNDS, |r| r.hosts[ha as usize].manager.foreign_running().contains(&b)),
        "host {ha} learns `b` runs"
    );
    assert!(seen_from(rig, b, a, block).is_some(), "b's halo of a is worldgen's");
    // The request `b`'s host would have sent had it still believed `a` ran.
    let link = rig.links[&(ha.min(hb), ha.max(hb))].clone();
    let end = if hb < ha { link.0 } else { link.1 };
    game::PeerLink::send(&mut *end.lock().unwrap(), game::PeerMsg::Volatile(game::Volatile::HaloRequest { from: b, of: a }));
    assert!(until(rig, HALO_ROUNDS, |r| seen_from(r, b, a, block).is_none()), "never answered from the cache");
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
    let y = crate::common::standing_y(crate::common::FLOOR_TOP);
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
    let owner = |rc: RegionCoords| game::Static { ordinal: 0, hosts }.owner_of(rc, s);
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
    rig.record_packets();
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
    // Each promotion's `PlayerRegion` came right behind the player's hop there, and the
    // later home's hop is the higher: a client reading the two hosts' packets in any
    // order can tell which home is the newer.
    let on_b = home_hops(&packets_to(rig, hb));
    let on_a = home_hops(&packets_to(rig, ha));
    assert_eq!(on_b.iter().map(|(_, r)| *r).collect::<Vec<_>>(), [b], "host {hb} became the home once");
    assert_eq!(on_a.iter().map(|(_, r)| *r).collect::<Vec<_>>(), [a], "host {ha} became the home again once");
    assert!(on_a[0].0 > on_b[0].0, "the newer home's hop is higher: {on_a:?} after {on_b:?}");
}

/// Every `HomeHop(n)` that is followed at once by a `PlayerRegion(Some(r))` in `packets`:
/// `(n, r)`.
fn home_hops(packets: &[&game::ServerPacket]) -> Vec<(u64, RegionCoords)> {
    packets
        .windows(2)
        .filter_map(|w| match (w[0], w[1]) {
            (game::ServerPacket::HomeHop(n), game::ServerPacket::PlayerRegion(Some(r), P_CLIENT)) => Some((*n, *r)),
            _ => None,
        })
        .collect()
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

/// The body is handed to B while the client has no session there; the client connects
/// (an observer ticket) within the grace: the awaited session becomes the home, and its
/// `PlayerRegion` comes right behind the player's hop there (`HomeHop`) — above the hop of
/// the host it left — so a client with both connections follows it.
fn awaited_session_home_hop(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.record_packets();
    rig.connect_home(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    assert_eq!(rig.hosts[hb as usize].manager.session_kind(P_CLIENT), None, "no session on host {hb} yet");
    assert!(home_hops(&packets_to(rig, hb)).is_empty(), "nothing told before the session came");
    rig.hosts[hb as usize].event(game::ServerEvent::ObserverConnected { client: P_CLIENT, player: P_PLAYER });
    assert_eq!(rig.hosts[hb as usize].manager.session_kind(P_CLIENT), Some(game::SessionKind::Home));
    rig.round();
    let hops = home_hops(&packets_to(rig, hb));
    assert_eq!(hops.iter().map(|(_, r)| *r).collect::<Vec<_>>(), [b], "HomeHop right before PlayerRegion({b:?})");
    assert!(hops[0].0 > 0, "a hand-off's hop is above the login's: {hops:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn awaited_session_gets_its_home_hop_before_player_region() {
    sweep("awaited_session_gets_its_home_hop_before_player_region", 2, 1, SEEDS / 4, awaited_session_home_hop);
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

// --- The coordinator serves every host (Task 10) ------------------------------------

/// The history's map: land from (-16, -16), 64 x 64 regions (the rig's regions near the
/// origin and its keepers' homes near (32, 32) all on it).
fn rig_world_map() -> game::WorldMap {
    let mut m = crate::history_handover::land_map(64, 64);
    m.origin = RegionCoords::new(-16, -16);
    m
}

/// A history with one site of `n` grown founders in `at`: (history, its civ, the founders).
fn history_with_site(at: RegionCoords, n: usize) -> (game::history::History, game::history::CivId, Vec<game::history::PersonId>) {
    use crate::history_handover::{start, years_before};
    use game::history::{History, Race, Sex, SocietyGrid};
    let s = start();
    let mut h = History::new(9, s, SocietyGrid::from_world_map(&rig_world_map()));
    let civ = h.add_civ(Race::Dwarf);
    let people: Vec<_> =
        (0..n).map(|i| h.add_founder(if i % 2 == 0 { Sex::Female } else { Sex::Male }, years_before(s, 20))).collect();
    h.found_site(civ, at, &people, None, false);
    (h, civ, people)
}

/// A site far from the regions the tests play in (near the origin).
const FAR_SITE: RegionCoords = RegionCoords { x: -14, z: 44 };

/// Every body of `person` in a running region of a host: `(host, region, id)`.
fn person_bodies(rig: &mut MultiRig, person: game::history::PersonId) -> Vec<(u32, RegionCoords, game::EntityId)> {
    let mut out = Vec::new();
    for (h, r) in rig.hosts.iter_mut().enumerate() {
        for rc in r.manager.running_regions() {
            r.manager.spawner_mut().with_region(rc, |region| {
                let data = region.data();
                for (id, &key) in data.id_index.iter() {
                    if matches!(data.ecs.kind.try_get(key), Some(game::EntityKind::Person { person: p }) if *p == person) {
                        out.push((h as u32, rc, *id));
                    }
                }
            });
        }
    }
    out
}

/// People conservation across hosts and the coordinator, for `people`: each living one is
/// either a body (in a running region, in a host's flight or mailbox, unsent or in transit
/// between hosts — or only sent and unacknowledged), or abstract in the history, or
/// absorbed at a seam with that report not handled by the coordinator yet — exactly one.
/// `seen` maps the bodies' ids to their people (learned in the regions).
fn assert_population(
    rig: &mut MultiRig,
    people: &[game::history::PersonId],
    seen: &mut std::collections::BTreeMap<game::EntityId, game::history::PersonId>,
) {
    use std::collections::{BTreeMap, BTreeSet};
    let mut places: BTreeMap<game::history::PersonId, usize> = BTreeMap::new();
    let mut shadows: BTreeSet<game::history::PersonId> = BTreeSet::new();
    for p in people {
        for (_, _, id) in person_bodies(rig, *p) {
            seen.insert(id, *p);
            *places.entry(*p).or_default() += 1;
        }
    }
    for r in &rig.hosts {
        let m = &r.manager;
        for (id, _) in m.in_flight_ids().into_iter().chain(m.pending_arrival_ids()) {
            if let Some(p) = seen.get(&id) {
                *places.entry(*p).or_default() += 1;
            }
        }
        for (_, id, sent) in m.peer_unreceived_ids() {
            if let Some(p) = seen.get(&id) {
                if sent {
                    shadows.insert(*p);
                } else {
                    *places.entry(*p).or_default() += 1;
                }
            }
        }
    }
    for (ea, eb) in rig.links.values() {
        for end in [ea, eb] {
            for msg in end.lock().unwrap().transit() {
                if let game::PeerMsg::Reliable { body, .. } = &msg {
                    let game::Reliable::Transfer { bundle, .. } = body.inner() else { continue };
                    if let game::EntityKind::Person { person } = bundle.kind {
                        *places.entry(person).or_default() += 1;
                    }
                }
            }
        }
    }
    let pending = rig.coordinator_pending();
    let core = rig.core();
    for p in people {
        let Some(q) = core.history().person(*p).filter(|q| q.alive()) else { continue };
        let bodies = places.get(p).copied().unwrap_or(0);
        let abstract_ = usize::from(!matches!(q.location, game::history::Location::InRegion(_)));
        let absorbed = pending
            .iter()
            .filter(|m| matches!(m, game::HostMsg::PersonCrossed { person, delivered: false, .. } if person == p))
            .count();
        let total = bodies + abstract_ + absorbed;
        // Only in a sent, unacknowledged transfer: the peer took it already (and may have
        // absorbed it), or it was lost with a connection and is sent again.
        let ok = if bodies == 0 && shadows.contains(p) { total <= 1 } else { total == 1 };
        assert!(
            ok,
            "{p:?}: {bodies} bodies (unacknowledged: {}), abstract {abstract_} ({:?}), {absorbed} absorbed reports pending",
            shadows.contains(p),
            q.location
        );
    }
}

/// Every `PersonCrossed` of `person` host `h` sent its coordinator.
fn crossings_reported(rig: &MultiRig, h: u32, person: game::history::PersonId) -> Vec<(RegionCoords, bool)> {
    rig.sent_to_coordinator(h)
        .into_iter()
        .filter_map(|m| match m {
            game::HostMsg::PersonCrossed { person: p, to, delivered } if p == person => Some((to, delivered)),
            _ => None,
        })
        .collect()
}

/// A resident of `a` (site there, on host A) is put past the seam into `b` on host B —
/// running (`running`) or parked: the body goes to host B, which alone reports the
/// crossing (`PersonCrossed`, delivered or absorbed); people are conserved every round;
/// the coordinator ends with the person embodied in `b`, or abstract.
fn person_crosses(rig: &mut MultiRig, running: bool) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let (history, _, people) = history_with_site(a, 2);
    rig.run(a);
    rig.run(b);
    if !running {
        rig.park(b);
    }
    rig.with_coordinator(history);
    let mut seen = std::collections::BTreeMap::new();
    let mut spawned = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if people.iter().all(|p| person_bodies(rig, *p).len() == 1) && rig.coordinator_pending().is_empty() {
            spawned = true;
            break;
        }
    }
    assert!(spawned, "the site's people never got their bodies in {a:?}");
    assert_population(rig, &people, &mut seen);
    let p = people[0];
    let (h, rc, id) = person_bodies(rig, p)[0];
    assert_eq!((h, rc), (ha, a));
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)), "{a:?} holds the body");
    let mut settled = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        assert_population(rig, &people, &mut seen);
        let bodies: Vec<(u32, RegionCoords)> = person_bodies(rig, p).into_iter().map(|(h, rc, _)| (h, rc)).collect();
        let location = rig.core().history().person(p).unwrap().location;
        let quiet = rig.coordinator_pending().is_empty() && rig.peer_unreceived() == 0;
        let done = if running {
            bodies == [(hb, b)] && location == game::history::Location::InRegion(b)
        } else {
            bodies.is_empty() && !matches!(location, game::history::Location::InRegion(_))
        };
        if quiet && done {
            settled = true;
            break;
        }
    }
    assert!(settled, "the crossing never settled (running {running}): bodies {:?}", person_bodies(rig, p));
    assert_eq!(crossings_reported(rig, hb, p), vec![(b, running)], "the receiving host reported the crossing once");
    assert!(crossings_reported(rig, ha, p).is_empty(), "the sending host reported nothing");
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        assert_population(rig, &people, &mut seen);
    }
    assert_eq!(person_bodies(rig, people[1]).iter().map(|(h, rc, _)| (*h, *rc)).collect::<Vec<_>>(), [(ha, a)]);
}

fn person_into_running(rig: &mut MultiRig) {
    person_crosses(rig, true);
}

fn person_into_parked(rig: &mut MultiRig) {
    person_crosses(rig, false);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn person_crosses_host_seam_into_a_running_region() {
    sweep("person_crosses_host_seam_into_a_running_region", 2, 1, SEEDS / 2, person_into_running);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn person_crosses_host_seam_into_a_parked_region() {
    sweep("person_crosses_host_seam_into_a_parked_region", 2, 1, SEEDS / 2, person_into_parked);
}

/// A second player's session id, and the player.
const P2_CLIENT: game::ClientId = 9;
const P2_PLAYER: game::PlayerId = game::PlayerId(101);

/// The `EmbarkResult`s host `h` sent `client` since recording began.
fn embark_results_to(rig: &MultiRig, h: u32, client: game::ClientId) -> Vec<Result<game::history::SiteId, game::history::EmbarkRefusal>> {
    let seen = rig.hosts[h as usize].packets_seen.as_ref().expect("recording packets");
    seen.iter()
        .filter_map(|(to, p)| match p {
            game::ServerPacket::EmbarkResult(r) if *to == Some(client) => Some(*r),
            _ => None,
        })
        .collect()
}

/// Rounds until `done` (at most [`CROSS_ROUNDS`]); panics naming `what`.
fn rounds_until(rig: &mut MultiRig, what: &str, mut done: impl FnMut(&mut MultiRig) -> bool) {
    for _ in 0..CROSS_ROUNDS {
        if done(rig) {
            return;
        }
        rig.round();
    }
    assert!(done(rig), "never: {what}");
}

/// The coordinator has handled everything the hosts sent, and they everything it sent.
fn coordinator_quiet(rig: &MultiRig) -> bool {
    rig.coordinator_pending().is_empty()
        && rig.hosts.iter().all(|r| r.manager.world_link().and_then(|l| l.as_inline()).is_none_or(|l| l.in_transit() == 0))
}

/// The test player embarks (through the real coordinator) into `a`, where it stands, then
/// walks over the host seam into `b`: its claim goes along — the coordinator has it online
/// on the NEW host (every seed: its `PlayerOnline` and the old host's `PlayerOffline`
/// arrive in either order) and another player's return to its fortress is refused
/// `InUse`; once it disconnects on the new host, that return succeeds.
fn claim_travels(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let (history, civ, founders) = history_with_site(FAR_SITE, 4);
    rig.run(a);
    rig.run(b);
    rig.with_coordinator(history);
    rig.record_packets();
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    let (_, _, id) = until_body(rig);
    let req = game::EmbarkRequest { civ, people: founders[0..2].to_vec(), region: a };
    rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::Embark(req), P_CLIENT));
    rounds_until(rig, "the embark is answered", |rig| !embark_results_to(rig, ha, P_CLIENT).is_empty());
    let site = embark_results_to(rig, ha, P_CLIENT)[0].expect("embarked");
    assert_eq!(rig.hosts[ha as usize].manager.client_claim(P_CLIENT), Some(game::Claim::Fortress(site)));
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)), "{a:?} holds the body");
    until_player_settled_in(rig, b);
    rounds_until(rig, "the coordinator settles", |rig| coordinator_quiet(rig));
    assert_eq!(rig.hosts[hb as usize].manager.client_claim(P_CLIENT), Some(game::Claim::Fortress(site)));
    assert_eq!(rig.hosts[ha as usize].manager.client_claim(P_CLIENT), None);
    {
        let core = rig.core();
        assert_eq!(core.online_on(P_PLAYER), Some(hb), "online on the new host");
        assert_eq!(core.claim_of(P_PLAYER), Some(game::Claim::Fortress(site)));
    }
    // Another player, on the old host, asks for the fortress: in use.
    rig.connect_home(ha, P2_CLIENT, P2_PLAYER, centre_of(a));
    rounds_until(rig, "the second player is placed", |rig| !rig.player_owners(P2_CLIENT).is_empty());
    let ask = |rig: &mut MultiRig| {
        rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::ReturnTo(site), P2_CLIENT))
    };
    ask(rig);
    rounds_until(rig, "the return is answered", |rig| !embark_results_to(rig, ha, P2_CLIENT).is_empty());
    assert_eq!(embark_results_to(rig, ha, P2_CLIENT), vec![Err(game::history::EmbarkRefusal::InUse)]);
    // The player leaves the new host: the fortress is free.
    rig.disconnect(hb, P_CLIENT);
    rounds_until(rig, "the player is offline", |rig| rig.core().online_on(P_PLAYER).is_none() && coordinator_quiet(rig));
    ask(rig);
    rounds_until(rig, "the second return is answered", |rig| embark_results_to(rig, ha, P2_CLIENT).len() == 2);
    assert_eq!(embark_results_to(rig, ha, P2_CLIENT)[1], Ok(site));
    rounds_until(rig, "the coordinator settles", |rig| coordinator_quiet(rig));
    assert_eq!(rig.core().claim_of(P2_PLAYER), Some(game::Claim::Fortress(site)));
    assert_eq!(rig.core().claim_of(P_PLAYER), None, "the offline claimant lost it");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn claim_travels_with_the_player_across_hosts() {
    sweep("claim_travels_with_the_player_across_hosts", 2, 1, SEEDS / 2, claim_travels);
}

/// The test player on host A embarks (through the real coordinator) into `b`, a region of
/// host B — running there already (`running`: the founding's spawn directives) or not
/// (its start's catch-up): the founding party's bodies stand in `b` on host B only, host
/// B knows the new site's region, and the claim is held on host B only.
fn embark_foreign_founders(rig: &mut MultiRig, running: bool) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let (history, civ, founders) = history_with_site(FAR_SITE, 4);
    rig.run(a);
    if running {
        rig.run(b);
    }
    rig.with_coordinator(history);
    rig.record_packets();
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    until_body(rig);
    let party = founders[0..2].to_vec();
    let req = game::EmbarkRequest { civ, people: party.clone(), region: b };
    rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::Embark(req), P_CLIENT));
    until_player_settled_in(rig, b);
    let site = embark_results_to(rig, ha, P_CLIENT).first().copied().expect("answered").expect("embarked");
    rounds_until(rig, "the founders stand in their fortress", |rig| {
        party.iter().all(|p| person_bodies(rig, *p).iter().map(|(h, rc, _)| (*h, *rc)).collect::<Vec<_>>() == [(hb, b)])
            && coordinator_quiet(rig)
    });
    assert_moved(rig, ha, hb, b, Some(game::Claim::Fortress(site)));
    for p in &party {
        let at: Vec<(u32, RegionCoords)> = person_bodies(rig, *p).iter().map(|(h, rc, _)| (*h, *rc)).collect();
        assert_eq!(at, [(hb, b)], "{p:?} stands in {b:?} on host {hb}, once");
        assert_eq!(rig.core().history().person(*p).unwrap().location, game::history::Location::InRegion(b));
    }
    assert_eq!(rig.hosts[hb as usize].manager.site_region(site), Some(b), "host {hb} knows the new site");
    assert_eq!(rig.core().online_on(P_PLAYER), Some(hb));
    assert_eq!(rig.core().claim_of(P_PLAYER), Some(game::Claim::Fortress(site)));
}

fn embark_foreign_running(rig: &mut MultiRig) {
    embark_foreign_founders(rig, true);
}

fn embark_foreign_parked(rig: &mut MultiRig) {
    embark_foreign_founders(rig, false);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn embark_into_a_foreign_host_spawns_the_founders_there() {
    sweep("embark_into_a_foreign_host_spawns_the_founders_there (running)", 2, 1, SEEDS / 4, embark_foreign_running);
    sweep("embark_into_a_foreign_host_spawns_the_founders_there (parked)", 2, 1, SEEDS / 4, embark_foreign_parked);
}

/// Fortress `site` founded by the test player (on host `ha`) in `a`, where it stands, with
/// a second player connected on `ha`: `(a, b, ha, hb, site, the player's body)`.
fn holder_and_rival(rig: &mut MultiRig) -> (RegionCoords, RegionCoords, u32, u32, game::history::SiteId, game::EntityId) {
    holder_and_rival_with(rig, false)
}

/// [`holder_and_rival`]; `b_unavailable`: `b` failed to load on its host.
fn holder_and_rival_with(
    rig: &mut MultiRig,
    b_unavailable: bool,
) -> (RegionCoords, RegionCoords, u32, u32, game::history::SiteId, game::EntityId) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let (history, civ, founders) = history_with_site(FAR_SITE, 4);
    rig.run(a);
    if b_unavailable {
        rig.host_of(b).manager.spawner_mut().fail_next_start(b);
        let keeper = crate::common::rig::CLIENT;
        rig.host_of(b).event(game::ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(b), keeper));
        rounds_until(rig, "b fails to load", |rig| rig.hosts[hb as usize].manager.unavailable_regions().contains(&b));
    } else {
        rig.run(b);
    }
    rig.with_coordinator(history);
    rig.record_packets();
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, centre_of(a));
    let (_, _, id) = until_body(rig);
    let req = game::EmbarkRequest { civ, people: founders[0..2].to_vec(), region: a };
    rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::Embark(req), P_CLIENT));
    rounds_until(rig, "the embark is answered", |rig| !embark_results_to(rig, ha, P_CLIENT).is_empty());
    let site = embark_results_to(rig, ha, P_CLIENT)[0].expect("embarked");
    rig.connect_home(ha, P2_CLIENT, P2_PLAYER, centre_of(a));
    rounds_until(rig, "the second player is placed", |rig| !rig.player_owners(P2_CLIENT).is_empty());
    rounds_until(rig, "the coordinator settles", |rig| coordinator_quiet(rig));
    (a, b, ha, hb, site, id)
}

/// The second player asks host `h` to return to `site`.
fn return_to(rig: &mut MultiRig, h: u32, site: game::history::SiteId) {
    rig.hosts[h as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::ReturnTo(site), P2_CLIENT));
}

/// While the fortress's holder walks over a host seam, a second player asks to return to
/// it on every round: never granted — the holder is online on the old host, in transit,
/// or online on the new one throughout.
fn claim_kept_while_crossing(rig: &mut MultiRig) {
    let (a, b, ha, hb, site, id) = holder_and_rival(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)), "{a:?} holds the body");
    let mut settled = false;
    for _ in 0..CROSS_ROUNDS {
        return_to(rig, ha, site);
        rig.round();
        let granted = embark_results_to(rig, ha, P2_CLIENT).into_iter().filter(|r| r.is_ok()).count();
        assert_eq!(granted, 0, "the fortress was taken while its holder crossed hosts");
        if rig.player_owners(P_CLIENT) == [(hb, b)] && rig.peer_unreceived() == 0 && coordinator_quiet(rig) {
            settled = true;
            break;
        }
    }
    assert!(settled, "the holder never settled in {b:?}");
    assert_eq!(rig.core().online_on(P_PLAYER), Some(hb));
    assert_eq!(rig.hosts[hb as usize].manager.client_claim(P_CLIENT), Some(game::Claim::Fortress(site)));
    assert_eq!(rig.core().claim_of(P2_PLAYER), None);
    if rig.lockstep && rig.link_delay == 0 {
        assert_eq!(rig.hop_is_received(), 0, "in order, a real hand-over tells nobody anything");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn claim_cannot_be_taken_while_its_holder_crosses_hosts() {
    sweep("claim_cannot_be_taken_while_its_holder_crosses_hosts", 2, 1, SEEDS / 2, claim_kept_while_crossing);
}

/// The fortress's holder is handed to a host on which its target region cannot be loaded:
/// that host parks it on arrival and reports it offline (the sending host reported it in
/// transit, not offline): the fortress is free then — another player's return succeeds.
fn handed_and_parked(rig: &mut MultiRig) {
    let (a, b, ha, hb, site, id) = holder_and_rival_with(rig, true);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)), "{a:?} holds the body");
    rounds_until(rig, "the holder is parked on the far host", |rig| {
        rig.parked_on(P_PLAYER) == [hb] && rig.peer_unreceived() == 0 && coordinator_quiet(rig)
    });
    assert_eq!(rig.core().online_on(P_PLAYER), None, "offline once parked on arrival");
    return_to(rig, ha, site);
    rounds_until(rig, "the return is answered", |rig| !embark_results_to(rig, ha, P2_CLIENT).is_empty());
    assert_eq!(embark_results_to(rig, ha, P2_CLIENT), vec![Ok(site)], "the fortress is free");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_parked_on_arrival_releases_the_online_state() {
    sweep("handoff_parked_on_arrival_releases_the_online_state", 2, 1, SEEDS / 4, handed_and_parked);
}

/// A `ClaimIs` answering the player's report on host A arrives (held back on A's link)
/// only after the player went A → B → A: it answers an old report and is not applied —
/// the player keeps the fortress the coordinator gave it meanwhile.
fn old_claim_is_after_round_trip(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let (history, _, _) = history_with_site(FAR_SITE, 2);
    let site = game::history::SiteId(0);
    let other = game::PlayerId(55);
    rig.run(a);
    rig.run(b);
    rig.with_coordinator(history);
    // Another player, online on a host outside the rig, plays the site.
    let held = game::OnlinePlayer { player: other, claim: Some(game::Claim::Fortress(site)), claim_pending: false, report: 1, hop: 1 };
    rig.core().handle(9, game::HostMsg::PlayerOnline(held), 0);
    // A's answers are held back: the coordinator's answer to the report below waits.
    rig.hold_world(ha, true);
    let record = game::PlayerRecord { fortress: Some(site), ..centre_of(a) };
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record);
    let (_, _, id) = until_body(rig);
    assert_eq!(rig.hosts[ha as usize].manager.client_claim(P_CLIENT), Some(game::Claim::Fortress(site)), "the early guess");
    rounds_until(rig, "the report is answered", |rig| rig.coordinator_pending().is_empty());
    // The other player leaves: the fortress is the test player's to take up.
    rig.core().handle(9, game::HostMsg::PlayerOffline { player: other, hop: 1 }, 0);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    rounds_until(rig, "host B's report is answered", |rig| rig.coordinator_pending().is_empty());
    assert_eq!(rig.core().claim_of(P_PLAYER), Some(game::Claim::Fortress(site)), "taken up from host B");
    let (_, _, id) = rig.player_body(P_CLIENT).expect("a body");
    assert!(rig.teleport(b, id, past_seam(b, a, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, a);
    rounds_until(rig, "host A's new report is handled", |rig| rig.coordinator_pending().is_empty());
    rig.hold_world(ha, false);
    rounds_until(rig, "the coordinator settles", |rig| coordinator_quiet(rig));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.hosts[ha as usize].manager.client_claim(P_CLIENT), Some(game::Claim::Fortress(site)), "kept");
    assert_eq!(rig.hosts[hb as usize].manager.client_claim(P_CLIENT), None);
    assert_eq!(rig.core().claim_of(P_PLAYER), Some(game::Claim::Fortress(site)));
    assert_eq!(rig.core().online_on(P_PLAYER), Some(ha));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn old_claim_is_after_a_round_trip_is_not_applied() {
    sweep("old_claim_is_after_a_round_trip_is_not_applied", 2, 1, SEEDS / 4, old_claim_is_after_round_trip);
}

// --- The coordinated world cut (Task 13) -------------------------------------------

/// The coordinator's world-cut period in the rig: one every 20 rounds of the hosts' clocks.
const WORLD_CUT_EVERY_MS: u64 = 20 * game::TICK_RATE;
/// Round budget for the world cuts a test waits for.
const WORLD_CUT_ROUNDS: usize = 600;
/// Most rounds a crossing that leaves after its source answered may take to land on the
/// other host while the cut is still pending (it is never held for the mark).
const LANDS_WITHIN_ROUNDS: usize = 12;
/// Rounds a test lags a region around the cut tick.
const LAG_AROUND_T: u32 = 10;

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

/// Cut rounds until world cut `cut` is decided; its decision.
fn until_decided(rig: &mut MultiRig, cut: game::CutId) -> bool {
    for _ in 0..WORLD_CUT_ROUNDS {
        if let Some(d) = rig.decided.get(&cut) {
            return *d;
        }
        rig.cut_round();
    }
    panic!("world cut {} never decided", cut.0);
}

/// Cut rounds until host `h` takes part in a world cut not decided yet: its id and tick.
fn until_world_cut_on(rig: &mut MultiRig, h: u32) -> (game::CutId, u64) {
    for _ in 0..WORLD_CUT_ROUNDS {
        let m = &rig.hosts[h as usize].manager;
        if let (Some(cut), Some((id, tick))) = (m.world_cut(), m.pending_cut()) {
            if cut == id && !rig.decided.contains_key(&cut) {
                return (id, tick);
            }
        }
        rig.cut_round();
    }
    panic!("host {h} never took part in a world cut");
}

fn world_tick(rig: &mut MultiRig) -> u64 {
    rig.hosts[0].manager.spawner_mut().world_tick()
}

/// `id`, standing in `a` or `b` (adjacent), pushed over their seam; `false` if neither
/// holds it now.
fn push_over(rig: &mut MultiRig, id: game::EntityId, a: RegionCoords, b: RegionCoords) -> bool {
    match rig.owners_of(id).as_slice() {
        [(_, rc)] if *rc == a => rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)),
        [(_, rc)] if *rc == b => rig.teleport(b, id, past_seam(b, a, REGION_SIZE / 2.0)),
        _ => false,
    }
}

/// How often `id` is in the running world: owned by a region, in flight or waiting on a
/// host, or never sent in a session.
fn live_count(rig: &mut MultiRig, id: game::EntityId) -> usize {
    let mut n = rig.owners_of(id).len();
    for r in &rig.hosts {
        let m = &r.manager;
        n += m.in_flight_ids().iter().filter(|(i, _)| *i == id).count();
        n += m.pending_arrival_ids().iter().filter(|(i, _)| *i == id).count();
        n += m.peer_unreceived_ids().iter().filter(|(_, i, sent)| *i == id && !sent).count();
    }
    n
}

/// Rounds until nothing is unacknowledged between `rig`'s hosts.
fn until_quiet(rig: &mut MultiRig) {
    for _ in 0..CROSS_ROUNDS {
        if rig.peer_unreceived() == 0 {
            return;
        }
        rig.round();
    }
    panic!("{} reliable messages never acknowledged", rig.peer_unreceived());
}

/// The test player with `n` stones in its inventory, standing in the middle of `rc`.
fn stocked_record(rc: RegionCoords, n: u32) -> game::PlayerRecord {
    let mut r = centre_of(rc);
    r.client.inventory.add(game::ItemStack { item: game::ItemId(2), count: n });
    r
}

/// Slimes and the test player shuttle over the host seam while world cuts run: every
/// committed cut holds each slime once (the store places each in exactly one region or
/// row), the last cut writes the player's record from one part only, and the world
/// restarted from the store has each slime and the player once.
fn cut_under_crossings(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let ha = rig.owner(a);
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, stocked_record(a, 7));
    until_body(rig);
    let y = crate::common::above_floor_y();
    let slimes: Vec<game::EntityId> =
        (0..4).map(|i| rig.spawn_slime(if i % 2 == 0 { a } else { b }, [REGION_SIZE / 2.0 + i as f32, y, REGION_SIZE / 3.0])).collect();
    let goal = committed(rig) + 2;
    let mut r = 0usize;
    while committed(rig) < goal {
        assert!(r < WORLD_CUT_ROUNDS, "two world cuts never committed: {:?}", rig.decided);
        if r % 3 == (rig.seed % 3) as usize {
            push_over(rig, slimes[(r / 3) % slimes.len()], a, b);
        }
        if r % 7 == 0 {
            if let Some((_, rc, id)) = rig.player_body(P_CLIENT) {
                let other = if rc == a { b } else { a };
                if rc == a || rc == b {
                    rig.teleport(rc, id, past_seam(rc, other, REGION_SIZE / 2.0));
                }
            }
        }
        rig.cut_round();
        r += 1;
    }
    for id in &slimes {
        assert_eq!(rig.stored_places(*id).len(), 1, "{id:?} stored once: {:?}", rig.stored_places(*id));
    }
    assert_eq!(rig.last_cut_writers(P_PLAYER).len(), 1, "the last cut writes the player once");
    let record = rig.committed_players[&P_PLAYER].clone();
    assert_eq!(record.client.inventory, stocked_record(a, 7).client.inventory, "the inventory it had");

    let mut again = rig.rebuild_from_last_cut();
    again.run(a);
    again.run(b);
    let home = again.owner(record.region);
    again.connect_home(home, P_CLIENT, P_PLAYER, record);
    until_quiet(&mut again);
    for id in &slimes {
        assert_eq!(live_count(&mut again, *id), 1, "{id:?} once in the restarted world");
    }
    assert_eq!(again.player_owners(P_CLIENT).len(), 1, "one body of the player");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_under_crossings_restores_each_entity_once() {
    sweep("cut_under_crossings_restores_each_entity_once", 2, 1, SEEDS, cut_under_crossings);
}

/// Task 12b fix round 1 (the benign case of the per-region entity key): a slime stored in
/// host A's region `a` by a committed cut crosses to host B's region `b`, then `a` parks
/// before the next cut. The next committed cut stores it in `b` only (the rig's store
/// checks no entity is in two regions after every commit), and the world restarted from
/// that cut has one live copy.
fn crossing_then_park(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let hb = rig.owner(b);
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    let y = crate::common::above_floor_y();
    let id = rig.spawn_slime(a, [REGION_SIZE / 2.0, y, REGION_SIZE / 3.0]);
    until_committed(rig, 1);
    assert_eq!(rig.stored_places(id).iter().map(|(_, rc, _)| *rc).collect::<Vec<_>>(), [a], "stored in a first");
    assert!(push_over(rig, id, a, b), "the slime stands in a");
    for _ in 0..CROSS_ROUNDS {
        if rig.owners_of(id) == [(hb, b)] {
            break;
        }
        rig.round();
    }
    assert_eq!(rig.owners_of(id), [(hb, b)], "the slime crossed to b");
    rig.park(a);
    until_committed(rig, 1);
    let places = rig.stored_places(id);
    assert_eq!(places, [(hb, b, "state")], "stored in b only: {places:?}");
    let mut again = rig.rebuild_from_last_cut();
    again.run(a);
    again.run(b);
    until_quiet(&mut again);
    assert_eq!(live_count(&mut again, id), 1, "one live copy in the restarted world");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn crossing_then_parking_the_source_region_stores_the_entity_once() {
    sweep("crossing_then_parking_the_source_region_stores_the_entity_once", 2, 1, SEEDS / 2, crossing_then_park);
}

/// Slimes leave both sides of the host seam one tick apart around a world cut's tick T
/// (T−3 .. T+2): whichever were on the wire at T, the cut holds each once.
fn on_the_wire(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let ha = rig.owner(a);
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    let y = crate::common::above_floor_y();
    let slimes: Vec<(game::EntityId, RegionCoords, RegionCoords)> = (0..6)
        .map(|i| {
            let (from, to) = if i % 2 == 0 { (a, b) } else { (b, a) };
            (rig.spawn_slime(from, [REGION_SIZE / 2.0, y, 8.0 + 4.0 * i as f32]), from, to)
        })
        .collect();
    rig.round();
    let (cut, t) = until_world_cut_on(rig, ha);
    let mut pushed = 0;
    while !rig.decided.contains_key(&cut) {
        let now = world_tick(rig);
        while pushed < slimes.len() && now + 3 >= t + pushed as u64 {
            let (id, from, to) = slimes[pushed];
            if rig.owners_of(id).iter().any(|(_, rc)| *rc == from) {
                rig.teleport(from, id, past_seam(from, to, 8.0 + 4.0 * pushed as f32));
            }
            pushed += 1;
        }
        rig.cut_round();
    }
    assert!(rig.decided[&cut], "the world cut at {t} committed");
    for (id, _, _) in &slimes {
        assert_eq!(rig.stored_places(*id).len(), 1, "{id:?} stored once: {:?}", rig.stored_places(*id));
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn transfer_on_the_wire_at_t_is_in_exactly_one_part() {
    sweep("transfer_on_the_wire_at_t_is_in_exactly_one_part", 2, 1, SEEDS, on_the_wire);
}

/// [`DEPOSIT`] dirt put down by region `from` at a cell of `to` (another host's): an
/// intent across the seam; `rig.items` counts it.
fn deposit_from(rig: &mut MultiRig, from: RegionCoords, to: RegionCoords) {
    crate::common::install_real_block_props();
    let at = deposit_cell(from, to);
    let stack = game::ItemStack { item: DIRT, count: DEPOSIT };
    rig.host_of(from).manager.spawner_mut().with_region(from, |r| {
        let now = r.current_tick();
        r.with_data(|d| assert_eq!(d.deposit_items_safe(at, stack, now, game::PICKUP_DELAY_TICKS), 0));
    });
    rig.items = Some(rig.items.unwrap_or(0) + u64::from(DEPOSIT));
}

/// Items `rig` holds once it is quiet (every item counted, none only in flight).
fn settled_items(rig: &mut MultiRig) -> u64 {
    until_quiet(rig);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    rig.items().0
}

/// Deposits cross the host seam one tick apart around a world cut's tick (T−3 .. T+2):
/// into the other side's running region (`running`: the intents on the wire), or into
/// its parked one (the owner's refusals carry the items back: replies on the wire). The
/// world restarted from the cut holds every item once — every item put down before its
/// region answered the cut (one put down after it is not in the cut: the test makes the
/// items out of nothing).
fn intents_on_the_wire(rig: &mut MultiRig, running: bool) {
    let (a, b) = host_seam(2, 1);
    let ha = rig.owner(a);
    rig.run(a);
    rig.run(b);
    if !running {
        rig.park(b);
    }
    world_cuts(rig);
    let (cut, t) = until_world_cut_on(rig, ha);
    let (mut sent, mut before) = (0u64, 0u64);
    while !rig.decided.contains_key(&cut) {
        let now = world_tick(rig);
        while sent < 6 && now + 3 >= t + sent {
            let (from, to) = if running && sent % 2 == 1 { (b, a) } else { (a, b) };
            let host = rig.owner(from) as usize;
            if rig.hosts[host].manager.pending_cut().is_some_and(|(c, _)| c == cut)
                && rig.hosts[host].manager.awaiting_cut().contains(&from)
            {
                before += 1;
            }
            deposit_from(rig, from, to);
            sent += 1;
        }
        rig.cut_round();
    }
    assert!(rig.decided[&cut], "the world cut at {t} committed");
    assert!(before > 0, "some deposits were put down before their region answered");
    let expected = u64::from(DEPOSIT) * before;
    let mut again = rig.rebuild_from_last_cut();
    again.run(a);
    if running {
        again.run(b);
    }
    assert_eq!(settled_items(&mut again), expected, "every item once in the restarted world");
}

fn intents_on_the_wire_running(rig: &mut MultiRig) {
    intents_on_the_wire(rig, true);
}

fn replies_on_the_wire_parked(rig: &mut MultiRig) {
    intents_on_the_wire(rig, false);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn intent_reply_on_the_wire_at_t_is_in_exactly_one_part() {
    sweep("intent_reply_on_the_wire_at_t_is_in_exactly_one_part (intents)", 2, 1, SEEDS, intents_on_the_wire_running);
    sweep("intent_reply_on_the_wire_at_t_is_in_exactly_one_part (replies)", 2, 1, SEEDS, replies_on_the_wire_parked);
}

/// The link between the hosts drops while a world cut is pending on both: the cut is
/// aborted; a slime crossing while the link is down
/// waits in the sender's session; once it is back the next cut commits and holds the
/// slime once.
fn link_down_aborts(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    let (cut, _) = until_world_cut_on(rig, ha);
    rig.link_down(ha, hb);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    assert!(!until_decided(rig, cut), "the cut with a link down is aborted");
    // The link drops before the marks, so no part of this cut is staged: criterion C (no
    // row for another participant's in-transit message, unless wrapped after the mark with
    // stamp / due ≥ T) is asserted on every part any test stages
    // (`MultiRig::prepare_part`).
    assert!(rig.hosts[ha as usize].manager.peer_unreceived() > 0 || live_count(rig, id) == 1);
    rig.link_up(ha, hb);
    until_committed(rig, 1);
    assert_eq!(rig.stored_places(id).len(), 1, "stored once: {:?}", rig.stored_places(id));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_with_a_link_down_aborts_and_the_next_cut_commits() {
    sweep("cut_with_a_link_down_aborts_and_the_next_cut_commits", 2, 1, SEEDS, link_down_aborts);
}

/// One host reads the coordinator only after the cut tick passed: it answers `Missed`,
/// the cut is aborted and the coordinator doubles the lead; the retry commits and the
/// lead is back to [`game::WORLD_CUT_LEAD_TICKS`].
fn missed_tick(rig: &mut MultiRig) {
    world_cuts(rig);
    rig.hold_world(1, true);
    for _ in 0..WORLD_CUT_ROUNDS {
        if rig.core().world_cut_running().is_some() {
            break;
        }
        rig.cut_round();
    }
    let (cut, t) = rig.core().world_cut_running().expect("a world cut began");
    while world_tick(rig) < t {
        rig.cut_round();
    }
    rig.hold_world(1, false);
    assert!(!until_decided(rig, cut), "the missed cut is aborted");
    assert_eq!(rig.core().world_cut_lead(), 2 * game::WORLD_CUT_LEAD_TICKS, "the lead doubled");
    until_committed(rig, 1);
    assert_eq!(rig.core().world_cut_lead(), game::WORLD_CUT_LEAD_TICKS, "a committed cut resets the lead");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn missed_tick_doubles_the_lead_and_the_retry_commits() {
    sweep("missed_tick_doubles_the_lead_and_the_retry_commits", 2, 1, SEEDS / 2, missed_tick);
}

/// A host the coordinator expects is not attached: no world cut begins (no `BeginCut`
/// on any host); once it is not expected any more, one commits.
fn no_cut_while_absent(rig: &mut MultiRig) {
    world_cuts(rig);
    rig.core().set_expected_hosts([0, 1, 2].into_iter().collect());
    for _ in 0..4 * (WORLD_CUT_EVERY_MS / game::TICK_RATE) as usize {
        rig.cut_round();
        assert_eq!(rig.core().world_cut_running(), None, "no cut while host 2 is absent");
        assert!(rig.hosts.iter().all(|r| r.manager.world_cut().is_none()));
    }
    for h in 0..2 {
        assert!(!rig.sent_to_coordinator(h).iter().any(|m| matches!(m, game::HostMsg::CutPrepared { .. } | game::HostMsg::CutFailed { .. })));
    }
    rig.core().set_expected_hosts([0, 1].into_iter().collect());
    until_committed(rig, 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_cut_while_an_expected_host_is_absent() {
    sweep("no_cut_while_an_expected_host_is_absent", 2, 1, SEEDS / 4, no_cut_while_absent);
}

/// Host 1's link to the coordinator drops while a world cut runs (the runtime tells the
/// core `host_detached`): the cut is aborted; once host 1 is back (its `Attach` names the
/// cut it took part in and is answered) the next cut commits.
fn host_detaching(rig: &mut MultiRig) {
    world_cuts(rig);
    let (cut, _) = until_world_cut_on(rig, 1);
    rig.hosts[1].manager.world_link_mut().unwrap().as_inline_mut().unwrap().set_up(false);
    rig.core().host_detached(1);
    assert!(!until_decided(rig, cut), "the cut is aborted");
    rig.hosts[1].manager.world_link_mut().unwrap().as_inline_mut().unwrap().set_up(true);
    for _ in 0..WORLD_CUT_ROUNDS {
        if rig.hosts[1].manager.world_attached() && rig.hosts[1].manager.world_cut() != Some(cut) {
            break;
        }
        rig.cut_round();
    }
    assert_ne!(rig.hosts[1].manager.world_cut(), Some(cut), "host 1 learned the outcome");
    until_committed(rig, 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_detaching_mid_cut_aborts_it() {
    sweep("host_detaching_mid_cut_aborts_it", 2, 1, SEEDS / 4, host_detaching);
}

/// The test player's inventory in its region on its host now.
fn inventory_now(rig: &mut MultiRig, client: game::ClientId) -> Option<game::Inventory> {
    let (h, rc, _) = rig.player_body(client)?;
    let mut inv = None;
    rig.hosts[h as usize].manager.spawner_mut().with_region(rc, |r| inv = r.data().clients.get(&client).map(|c| c.inventory.clone()));
    inv
}

/// Criterion A: the test player is moved across hosts (an embark through the real
/// coordinator into a region of the other host) right around a world cut's tick, while
/// a second player walks over the host seam. Before the move the player picks up two more
/// stones (a stale record would differ). When THAT cut commits: each player's record is
/// written by exactly one part of it, the moved player's with the inventory it held at
/// T, and the world restarted from it has each player once with that inventory. Whether
/// the mid-move cut commits depends on the seed; every mode must commit it at least once.
fn mid_move(rig: &mut MultiRig) -> bool {
    let (a, b) = host_seam(2, 1);
    let ha = rig.owner(a);
    let (history, civ, founders) = history_with_site(FAR_SITE, 4);
    rig.run(a);
    rig.run(b);
    world_cuts_over(rig, history);
    rig.record_packets();
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, stocked_record(a, 5));
    rig.connect_everywhere(ha, P2_CLIENT, P2_PLAYER, record_at(a, [REGION_SIZE / 2.0, 10.0]));
    until_body(rig);
    let (cut, t) = until_world_cut_on(rig, ha);
    // Two more stones, before the cut tick: what the record at T must hold.
    let (h, rc, _) = rig.player_body(P_CLIENT).expect("a body");
    rig.hosts[h as usize].manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| {
            d.clients.get_mut(&P_CLIENT).unwrap().inventory.add(game::ItemStack { item: game::ItemId(2), count: 2 });
        })
    });
    rig.cut_round();
    let at_t = inventory_now(rig, P_CLIENT).expect("the inventory");
    assert_ne!(at_t, stocked_record(a, 5).client.inventory, "the inventory changed");
    let fire = t.saturating_sub(2 + rig.seed % 4);
    let (mut embarked, mut walked) = (false, false);
    while !rig.decided.contains_key(&cut) {
        let now = world_tick(rig);
        if !embarked && now >= fire {
            let req = game::EmbarkRequest { civ, people: founders[0..2].to_vec(), region: b };
            rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::Embark(req), P_CLIENT));
            embarked = true;
        }
        if !walked && now + 1 >= t {
            if let Some((_, rc, id)) = rig.player_body(P2_CLIENT) {
                if rc == a {
                    rig.teleport(a, id, past_seam(a, b, 10.0));
                    walked = true;
                }
            }
        }
        rig.cut_round();
    }
    if !rig.decided[&cut] {
        return false;
    }
    assert_eq!(rig.last_committed().0, cut, "the mid-move cut is the last committed");
    for p in [P_PLAYER, P2_PLAYER] {
        assert_eq!(rig.last_cut_writers(p).len(), 1, "player {} written by one part of the mid-move cut: {:?}", p.0, rig.last_cut_writers(p));
    }
    let (_, parts) = rig.last_committed();
    let record = parts.values().flat_map(|p| &p.players).find(|(p, _)| *p == P_PLAYER).map(|(_, r)| r.clone()).unwrap();
    assert_eq!(record.client.inventory, at_t, "the record holds the inventory at the cut tick");
    let mut again = rig.rebuild_from_last_cut();
    for (c, p) in [(P_CLIENT, P_PLAYER), (P2_CLIENT, P2_PLAYER)] {
        let r = rig.committed_players[&p].clone();
        let home = again.owner(r.region);
        again.connect_home(home, c, p, r);
    }
    until_quiet(&mut again);
    assert_eq!(again.player_owners(P_CLIENT).len(), 1);
    assert_eq!(again.player_owners(P2_CLIENT).len(), 1);
    assert_eq!(inventory_now(&mut again, P_CLIENT), Some(at_t.clone()), "restored with the inventory it had at T");
    // World-wide: every item of the restarted world is what the cut held at T — the
    // players' inventories, and nothing else (no pile, intent or reply anywhere).
    let p2 = inventory_now(&mut again, P2_CLIENT).expect("the walker's inventory");
    let held = |inv: &game::Inventory| inv.slots.iter().flatten().map(|s| u64::from(s.count)).sum::<u64>();
    let at_t_p2 = parts.values().flat_map(|p| &p.players).find(|(p, _)| *p == P2_PLAYER).map(|(_, r)| held(&r.client.inventory)).unwrap();
    assert_eq!(again.items(), (0, 0), "no item outside the players' inventories");
    assert_eq!(held(&p2), at_t_p2, "the walker holds what it held at T");
    assert_eq!(held(&inventory_now(&mut again, P_CLIENT).unwrap()), held(&at_t), "the mover holds what it held at T");
    true
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_mid_move_holds_the_player_once() {
    sweep_hits("cut_mid_move_holds_the_player_once", 2, 1, SEEDS, mid_move);
}

/// Counter-example 1 of the ruling: region `a` has answered the cut while its host's
/// keeper region lags, so the host's mark is not out; a slime (or, `player`, the test
/// player) leaves `a` into the other host then. It crosses at once (nothing is held for
/// the mark: it lands within [`LANDS_WITHIN_ROUNDS`]), goes wrapped (`a`'s snapshot holds
/// it), and the cut holds it once — in `a`'s snapshot.
fn cut_lagging_source(rig: &mut MultiRig, player: bool) {
    let (a, b) = host_seam(2, 1);
    let ha = rig.owner(a);
    let home = rig.homes[ha as usize];
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    if player {
        rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, stocked_record(a, 3));
        until_body(rig);
    }
    let y = crate::common::above_floor_y();
    let slime = rig.spawn_slime(a, [REGION_SIZE / 2.0, y, REGION_SIZE / 2.0]);
    let (cut, t) = until_world_cut_on(rig, ha);
    while world_tick(rig) + 2 < t {
        rig.cut_round();
    }
    let lag = (t - world_tick(rig)) as u32 + LAG_AROUND_T;
    rig.hosts[ha as usize].manager.spawner_mut().hold(home, lag);
    for _ in 0..WORLD_CUT_ROUNDS {
        let m = &rig.hosts[ha as usize].manager;
        if m.pending_cut().is_some_and(|(c, _)| c == cut) && !m.awaiting_cut().contains(&a) {
            break;
        }
        rig.cut_round();
    }
    let m = &rig.hosts[ha as usize].manager;
    assert!(m.awaiting_cut().contains(&home), "the keeper region still lags");
    assert_eq!(m.world_cut_state().map(|s| s.1), Some("pending"), "the host's mark is not out");
    let id = if player { rig.player_body(P_CLIENT).expect("a body").2 } else { slime };
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)), "{a:?} holds it");
    let mut landed = false;
    for _ in 0..LANDS_WITHIN_ROUNDS {
        rig.cut_round();
        if rig.owners_of(id).iter().any(|(_, rc)| *rc == b) {
            landed = true;
            break;
        }
    }
    assert!(landed, "the crossing landed on the other host within {LANDS_WITHIN_ROUNDS} rounds");
    rig.hosts[ha as usize].manager.spawner_mut().hold(home, 0);
    assert!(until_decided(rig, cut), "the cut commits");
    if player {
        assert_eq!(rig.last_cut_writers(P_PLAYER), [ha], "the player is in {a:?}'s snapshot only");
    } else {
        assert_eq!(rig.stored_places(id), [(ha, a, "state")], "once, in {a:?}'s snapshot");
    }
}

fn lagging_source_slime(rig: &mut MultiRig) {
    cut_lagging_source(rig, false);
}

fn lagging_source_player(rig: &mut MultiRig) {
    cut_lagging_source(rig, true);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_with_a_lagging_source_region_holds_a_crossing_once() {
    sweep("cut_with_a_lagging_source_region_holds_a_crossing_once (slime)", 2, 1, SEEDS, lagging_source_slime);
    sweep("cut_with_a_lagging_source_region_holds_a_crossing_once (player)", 2, 1, SEEDS / 2, lagging_source_player);
}

/// Counter-example 2 of the ruling: the other host's mark is out; region `a` lags (it has
/// not answered) and puts dirt down into `b`, parked on the other host. That host refuses
/// it after its mark: the reply carrying the items back is its own part's row (due at
/// least T) and goes wrapped. Every item is once in the world restarted from the cut —
/// `a` answering before the reply (`short`) or after it.
fn refusal_after_mark(rig: &mut MultiRig, short: bool) -> bool {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.park(b);
    world_cuts(rig);
    let (cut, t) = until_world_cut_on(rig, ha);
    while world_tick(rig) + 3 < t {
        rig.cut_round();
    }
    let lag = if short { 6 } else { 6 + LAG_AROUND_T };
    rig.hosts[ha as usize].manager.spawner_mut().hold(a, lag);
    for _ in 0..WORLD_CUT_ROUNDS {
        if matches!(rig.hosts[hb as usize].manager.world_cut_state(), Some((c, "marked" | "complete" | "prepared")) if c == cut) {
            break;
        }
        rig.cut_round();
    }
    assert!(rig.hosts[ha as usize].manager.awaiting_cut().contains(&a), "`a` has not answered");
    deposit_from(rig, a, b);
    let decided = until_decided(rig, cut);
    let expected = u64::from(DEPOSIT);
    if decided {
        let (_, parts) = rig.last_committed();
        let replies: Vec<&game::InFlightRow> = parts[&hb]
            .in_flight
            .iter()
            .filter(|r| matches!(r, game::InFlightRow::Intent { msg: game::IntentMsg::Reply(s), .. } if s.due >= t))
            .collect();
        assert!(replies.len() <= 1, "the refusal is written once: {replies:?}");
        if !short {
            // `a` answers only after the reply came back: the refusal was made after the
            // other host's mark and is that host's row (case 3).
            assert_eq!(replies.len(), 1, "the refusal made after the mark is host {hb}'s row");
        }
        let mut again = rig.rebuild_from_last_cut();
        again.run(a);
        assert_eq!(settled_items(&mut again), expected, "every item once in the restarted world");
    } else {
        until_committed(rig, 1);
    }
    decided
}

fn refusal_after_mark_short(rig: &mut MultiRig) -> bool {
    refusal_after_mark(rig, true)
}

fn refusal_after_mark_long(rig: &mut MultiRig) -> bool {
    refusal_after_mark(rig, false)
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn refusal_generated_after_the_mark_is_in_exactly_one_part() {
    sweep_hits("refusal_generated_after_the_mark_is_in_exactly_one_part (short)", 2, 1, SEEDS, refusal_after_mark_short);
    sweep_hits("refusal_generated_after_the_mark_is_in_exactly_one_part (long)", 2, 1, SEEDS, refusal_after_mark_long);
}

/// A second test player (no body anywhere) and its session.
const P3_CLIENT: game::ClientId = 11;
const P3_PLAYER: game::PlayerId = game::PlayerId(102);

/// Important 1 (manager level): host A begins world cut K alone (its `BeginCut` is the
/// only one sent); once `a` answered, a slime and the test player leave `a` for host B —
/// wrapped in K (`a`'s snapshot holds them) — and reach B before B's `BeginCut { K }`.
/// B then refuses K as `Missed` (a peer's region is past the tick; B never takes part in
/// a cut it already saw wrappers of). The next cut, through the coordinator, holds the
/// slime and the player once.
fn wrapper_before_begin(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.with_coordinator(quiet_history());
    for h in 0..2 {
        rig.hosts[h].manager.set_world_map(rig_world_map());
    }
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, stocked_record(a, 3));
    until_body(rig);
    let y = crate::common::above_floor_y();
    let slime = rig.spawn_slime(a, [REGION_SIZE / 2.0, y, REGION_SIZE / 3.0]);
    let k = game::CutId(5);
    let t = world_tick(rig) + 20;
    rig.hosts[ha as usize].manager.begin_world_cut(k, t, &[0, 1], &[]).expect("host A begins K");
    for _ in 0..WORLD_CUT_ROUNDS {
        if !rig.hosts[ha as usize].manager.awaiting_cut().contains(&a) {
            break;
        }
        rig.round();
    }
    assert!(!rig.hosts[ha as usize].manager.awaiting_cut().contains(&a), "`a` answered K");
    assert!(rig.teleport(a, slime, past_seam(a, b, REGION_SIZE / 3.0)));
    rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::PlayRegion(b), P_CLIENT));
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        let player_there = rig.hosts[hb as usize].manager.player_of(P_CLIENT) == Some(P_PLAYER);
        if player_there && rig.owners_of(slime).iter().any(|(_, rc)| *rc == b) {
            break;
        }
    }
    assert_eq!(rig.hosts[hb as usize].manager.player_of(P_CLIENT), Some(P_PLAYER), "the player reached host {hb}");
    assert!(rig.owners_of(slime).iter().any(|(_, rc)| *rc == b), "the slime reached host {hb}");
    let now = world_tick(rig);
    assert_eq!(
        rig.hosts[hb as usize].manager.begin_world_cut(k, now + game::WORLD_CUT_LEAD_TICKS, &[0, 1], &[]),
        Err(game::CutFailure::Missed),
        "a wrapper of K came first: K is missed"
    );
    // K is over for host A too.
    rig.core().cut_outcome(ha, k, false);
    rig.with_world_cuts(WORLD_CUT_EVERY_MS);
    until_committed(rig, 1);
    assert_eq!(rig.stored_places(slime).len(), 1, "the slime stored once: {:?}", rig.stored_places(slime));
    assert_eq!(rig.last_cut_writers(P_PLAYER).len(), 1, "the player written once");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn in_cut_message_arriving_before_begin_cut_is_not_a_row() {
    sweep("in_cut_message_arriving_before_begin_cut_is_not_a_row", 2, 1, SEEDS / 2, wrapper_before_begin);
}

/// Important 1 (d): a wrapped hand-off that wakes a parked region with mail waiting there
/// keeps its `accounted_in` (the mail's delivery restores the lineage it found).
fn wrapped_wake_keeps_accounted(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.park(b);
    rig.with_coordinator(quiet_history());
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, stocked_record(a, 1));
    let (_, _, body) = until_body(rig);
    // Mail for `b` (a reply) waits on host B.
    let due = world_tick(rig);
    rig.hosts[hb as usize].manager.apply_peer_reliable(ha, game::Reliable::Intent(refusal_to(a, b, due)));
    rig.items = None;
    let k = game::CutId(6);
    let t = world_tick(rig) + 20;
    rig.hosts[ha as usize].manager.begin_world_cut(k, t, &[0, 1], &[]).expect("host A begins K");
    for _ in 0..WORLD_CUT_ROUNDS {
        if !rig.hosts[ha as usize].manager.awaiting_cut().contains(&a) {
            break;
        }
        rig.round();
    }
    // `b`'s inputs wait once it runs, so the arrival stays in flight.
    rig.hosts[hb as usize].manager.spawner_mut().hold_inputs(b, 40);
    assert!(rig.teleport(a, body, past_seam(a, b, REGION_SIZE / 2.0)));
    let mut seen = None;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        seen = rig.hosts[hb as usize].manager.in_flight_accounted().into_iter().find(|(id, _)| *id == body);
        if seen.is_some() {
            break;
        }
    }
    assert!(rig.hosts[hb as usize].manager.running_regions().contains(&b), "the hand-off woke `b`");
    assert_eq!(seen, Some((body, Some(k))), "the arrival keeps the cut its sender accounts for it in");
    rig.hosts[hb as usize].manager.spawner_mut().hold_inputs(b, 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn wrapped_hand_off_waking_a_region_with_mail_keeps_its_accounted_cut() {
    sweep("wrapped_hand_off_waking_a_region_with_mail_keeps_its_accounted_cut", 2, 1, 2, wrapped_wake_keeps_accounted);
}

/// The sending host breaks the rule once (a test link strips the wrapper of a crossing
/// sent after its mark): the receiver, still pending, fails the cut (`CutFailed {
/// Aborted }`), applies the crossing all the same, and the next cut commits with the
/// slime stored once.
fn bare_after_mark(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let home_b = rig.homes[hb as usize];
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    let y = crate::common::above_floor_y();
    let id = rig.spawn_slime(a, [REGION_SIZE / 2.0, y, REGION_SIZE / 2.0]);
    let (cut, t) = until_world_cut_on(rig, ha);
    rig.strip_next_wrapper(ha, hb);
    while world_tick(rig) + 2 < t {
        rig.cut_round();
    }
    let lag = (t - world_tick(rig)) as u32 + LAG_AROUND_T;
    rig.hosts[hb as usize].manager.spawner_mut().hold(home_b, lag);
    for _ in 0..WORLD_CUT_ROUNDS {
        if matches!(rig.hosts[ha as usize].manager.world_cut_state(), Some((c, "marked" | "complete"))if c == cut) {
            break;
        }
        rig.cut_round();
    }
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    for _ in 0..CROSS_ROUNDS {
        rig.cut_round();
        if rig.stripped(ha, hb) == 1 && rig.owners_of(id).iter().any(|(_, rc)| *rc == b) {
            break;
        }
    }
    assert_eq!(rig.stripped(ha, hb), 1, "one wrapper stripped");
    assert!(rig.owners_of(id).iter().any(|(_, rc)| *rc == b), "the bare crossing was applied");
    let failed = rig
        .sent_to_coordinator(hb)
        .into_iter()
        .any(|m| m == game::HostMsg::CutFailed { cut, why: game::CutFailure::Aborted });
    assert!(failed, "host {hb} failed the cut");
    rig.hosts[hb as usize].manager.spawner_mut().hold(home_b, 0);
    assert!(!until_decided(rig, cut), "the cut is aborted");
    until_committed(rig, 1);
    assert_eq!(rig.stored_places(id).len(), 1, "stored once: {:?}", rig.stored_places(id));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn bare_message_after_a_mark_fails_the_cut() {
    sweep("bare_message_after_a_mark_fails_the_cut", 2, 1, SEEDS / 2, bare_after_mark);
}

/// Criterion F on a host: a `BeginCut` while the host still has a world cut pending is
/// refused (`Aborted`) without touching the pending one, which commits.
fn second_begin(rig: &mut MultiRig) {
    world_cuts(rig);
    let (cut, t) = until_world_cut_on(rig, 0);
    let other = game::CutId(cut.0 + 1000);
    let refused = rig.hosts[0].manager.begin_world_cut(other, t + 40, &[0, 1], &[]);
    assert_eq!(refused, Err(game::CutFailure::Aborted), "a second cut is refused");
    assert_eq!(rig.hosts[0].manager.pending_cut(), Some((cut, t)), "the pending cut is untouched");
    assert_eq!(rig.hosts[0].manager.world_cut(), Some(cut));
    assert!(until_decided(rig, cut), "the pending cut commits");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_begun_while_one_is_pending_is_refused_and_the_first_commits() {
    sweep("cut_begun_while_one_is_pending_is_refused_and_the_first_commits", 2, 1, SEEDS / 4, second_begin);
}

/// How the test player goes over and back in [`returned_before_t`], and how it ends.
#[derive(Clone, Copy, PartialEq)]
enum Return {
    /// `PlayerMove` there and back (`PlayRegion`), then the client disconnects.
    MovesThenDisconnect,
    /// `PlayerMove` there and back, then a move to another region of host A.
    MovesThenLocalMove,
    /// Walking hand-offs there and back, then the client disconnects.
    WalksThenDisconnect,
}

/// Rounds (cut rounds) until the test player's body stands in `rc` on its owner.
fn until_player_in(rig: &mut MultiRig, rc: RegionCoords) -> bool {
    for _ in 0..CROSS_ROUNDS {
        if rig.player_owners(P_CLIENT) == [(rig.owner(rc), rc)] {
            return true;
        }
        rig.cut_round();
    }
    false
}

/// Critical 1: with world cut K pending, the test player leaves host A BARE (its region
/// had not answered: the receiver's) and comes back BARE (the same rule the other way)
/// before the cut tick, then ends on host A (a disconnect parks it, or a local move).
/// The cut must write its record from host A's part — the bare return makes it A's again.
/// For the moves, host B reads K's `BeginCut` only once the player is back (so B creates
/// its body at once, as with no cut pending). Returns whether everything happened before
/// T and K committed (the case under test).
fn returned_before_t(rig: &mut MultiRig, how: Return) -> bool {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let a2 = region_of(rig, ha);
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    for h in 0..2 {
        rig.hosts[h].manager.set_world_map(rig_world_map());
    }
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, stocked_record(a, 4));
    until_body(rig);
    let walks = how == Return::WalksThenDisconnect;
    if !walks {
        rig.hold_world(hb, true);
    }
    let (cut, t) = until_world_cut_on(rig, ha);
    if walks {
        // Both hosts must have the cut pending (each notes the player it sends bare).
        for _ in 0..CROSS_ROUNDS {
            if rig.hosts[hb as usize].manager.world_cut() == Some(cut) {
                break;
            }
            rig.cut_round();
        }
    }
    let over = |rig: &mut MultiRig, from: u32, to: RegionCoords| {
        if walks {
            let (_, rc, id) = rig.player_body(P_CLIENT).expect("a body");
            let other = if rc == a { b } else { a };
            rig.teleport(rc, id, past_seam(rc, other, REGION_SIZE / 2.0));
        } else {
            rig.hosts[from as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::PlayRegion(to), P_CLIENT));
        }
    };
    over(rig, ha, b);
    let there = until_player_in(rig, b);
    if there {
        over(rig, hb, a);
    }
    // Walking, the body is back in `a`. Moved, host A holds the player again: its body's
    // creation there is held to the slot after T while the cut is pending (`cut_hold_stamp`),
    // so its record is A's transit record at T.
    let back = there
        && if walks {
            until_player_in(rig, a)
        } else {
            (0..CROSS_ROUNDS).any(|_| {
                let home = rig.hosts[ha as usize].manager.player_of(P_CLIENT) == Some(P_PLAYER);
                if !home {
                    rig.cut_round();
                }
                home
            })
        };
    rig.hold_world(hb, false);
    if back {
        match how {
            Return::MovesThenLocalMove => {
                rig.hosts[ha as usize].event(game::ServerEvent::ClientPacket(game::ClientPacket::PlayRegion(a2), P_CLIENT));
            }
            _ => {
                rig.disconnect(ha, P_CLIENT);
                rig.disconnect(hb, P_CLIENT);
            }
        }
    }
    let in_time = back && world_tick(rig) + 2 < t;
    let committed = until_decided(rig, cut);
    if !(in_time && committed) {
        return false;
    }
    assert_eq!(rig.last_committed().0, cut);
    assert_eq!(rig.last_cut_writers(P_PLAYER), [ha], "the returned player is host {ha}'s, in exactly one part");
    let (_, parts) = rig.last_committed();
    let record = parts[&ha].players.iter().find(|(p, _)| *p == P_PLAYER).map(|(_, r)| r.clone()).unwrap();
    assert_eq!(record.client.inventory, stocked_record(a, 4).client.inventory, "the inventory it held at T");
    true
}

fn returned_moves_disconnect(rig: &mut MultiRig) -> bool {
    returned_before_t(rig, Return::MovesThenDisconnect)
}

fn returned_moves_local_move(rig: &mut MultiRig) -> bool {
    returned_before_t(rig, Return::MovesThenLocalMove)
}

fn returned_walks_disconnect(rig: &mut MultiRig) -> bool {
    returned_before_t(rig, Return::WalksThenDisconnect)
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_sent_and_returned_before_t_is_in_exactly_one_part() {
    sweep_hits("player_sent_and_returned_before_t (moves, disconnect)", 2, 1, SEEDS, returned_moves_disconnect);
    sweep_hits("player_sent_and_returned_before_t (moves, local move)", 2, 1, SEEDS, returned_moves_local_move);
    sweep_hits("player_sent_and_returned_before_t (walks, disconnect)", 2, 1, SEEDS, returned_walks_disconnect);
}

/// Important 5 (defensive since fix round 2: marks wait for T, so no region-sourced
/// transfer or move is ever case 3 — only a manager-made message could be, and none makes
/// a transfer or move after the mark today; the code path is kept, and tested here through
/// the manager's sender path): case 3 — a transfer and a player move made by host A after
/// its marks went out, in no snapshot of A (the slime is made after `a` answered, the player was on no
/// host), while host B keeps the cut pending (its keeper region lags). Sent through the
/// manager's state-message path with `view == false`: A writes them into its own part (a
/// row with stamp ≥ T, the player's record), wrapped; B's part has nothing of them; the
/// committed cut and the restarted world hold each once.
fn case_three(rig: &mut MultiRig) -> bool {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let home_b = rig.homes[hb as usize];
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    let (cut, t) = until_world_cut_on(rig, ha);
    while world_tick(rig) + 2 < t {
        rig.cut_round();
    }
    let lag = (t - world_tick(rig)) as u32 + LAG_AROUND_T;
    rig.hosts[hb as usize].manager.spawner_mut().hold(home_b, lag);
    for _ in 0..WORLD_CUT_ROUNDS {
        if matches!(rig.hosts[ha as usize].manager.world_cut_state(), Some((c, "marked"))if c == cut) {
            break;
        }
        rig.cut_round();
    }
    if !matches!(rig.hosts[ha as usize].manager.world_cut_state(), Some((c, "marked")) if c == cut) {
        return false;
    }
    // A slime made after `a` answered (in no snapshot), taken out of `a` as a transfer.
    let y = crate::common::above_floor_y();
    let id = rig.spawn_slime(a, [REGION_SIZE / 2.0, y, REGION_SIZE / 2.0]);
    let mut bundle = None;
    rig.host_of(a).manager.spawner_mut().with_region(a, |r| {
        let key = r.data().id_index[&id];
        bundle = Some(r.data().bundle_of(key));
        r.with_data(|d| d.remove_entity_safe(key, None));
    });
    let mut bundle = bundle.unwrap();
    bundle.rebase(a, b);
    let stamp = t - 2;
    let transfer = game::Reliable::Transfer { target: b, stamp, bundle: Box::new(bundle), player: None };
    rig.hosts[ha as usize].manager.send_reliable_for_test(hb, transfer, false);
    // A player on no host, moved to `b`.
    let handoff = game::PlayerHandoff { client: P3_CLIENT, player: P3_PLAYER, claim: None, login: None, park: false, hop: 1 };
    let record = stocked_record(b, 6);
    let mv = game::Reliable::PlayerMove { handoff, record: Box::new(record.clone()), target: b };
    rig.hosts[ha as usize].manager.send_reliable_for_test(hb, mv, false);
    rig.hosts[hb as usize].manager.spawner_mut().hold(home_b, 0);
    if !until_decided(rig, cut) {
        return false;
    }
    let (_, parts) = rig.last_committed();
    let row = parts[&ha].in_flight.iter().find_map(|r| match r {
        game::InFlightRow::Transfer { bundle, stamp, .. } if bundle.id == id => Some(*stamp),
        _ => None,
    });
    assert!(row.is_some_and(|s| s >= t), "host {ha}'s part has the transfer as a row at T or later: {row:?}");
    assert!(part_holds(&parts[&hb], id).is_none(), "host {hb}'s part has nothing of it");
    assert_eq!(rig.last_cut_writers(P3_PLAYER), [ha], "the moved player's record is host {ha}'s");
    assert_eq!(rig.stored_places(id).len(), 1, "stored once: {:?}", rig.stored_places(id));
    let mut again = rig.rebuild_from_last_cut();
    again.run(b);
    until_quiet(&mut again);
    assert_eq!(live_count(&mut again, id), 1, "the slime once in the restarted world");
    true
}

/// Where `part` holds entity `id`: in a region snapshot or a row.
fn part_holds(part: &game::WorldCut, id: game::EntityId) -> Option<RegionCoords> {
    for (rc, p) in &part.regions {
        if p.entities.iter().any(|e| e.id == id) {
            return Some(*rc);
        }
    }
    part.in_flight.iter().find_map(|r| match r {
        game::InFlightRow::Transfer { bundle, target, .. } if bundle.id == id => Some(*target),
        _ => None,
    })
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn case_three_crossings_stay_in_the_senders_part() {
    sweep_hits("case_three_crossings_stay_in_the_senders_part", 2, 1, SEEDS / 2, case_three);
}

/// Fix round 2 (I5): host B has no running region when `BeginCut { K }` arrives (its
/// keeper left and its home parked). During the lead the test player walks A→B, waking
/// `b` (it starts before T), then a slime follows it, and before T both walk back B→A.
/// B's marks must not be queued before T (a region woken in the lead window sends bare
/// before them: the receiver's); when K commits, each is in exactly one part and the
/// rebuild holds each once.
fn lead_window(rig: &mut MultiRig) -> bool {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let home_b = rig.homes[hb as usize];
    rig.run(a);
    // Host B idle: its keeper leaves (its body parks: not a tracked entity any more), its
    // home parks.
    let mut keeper = None;
    rig.hosts[hb as usize].manager.spawner_mut().with_region(home_b, |r| {
        let d = r.data();
        keeper = d.player_entites.get(&crate::common::rig::CLIENT).and_then(|k| *d.ecs.id.try_get(*k));
    });
    // Its body may end by parking (the keeper's player is the host's ordinal).
    if let Some(id) = keeper {
        rig.player_bodies.insert(id, game::PlayerId(u64::from(hb)));
    }
    rig.disconnect(hb, crate::common::rig::CLIENT);
    rig.park(home_b);
    assert!(rig.hosts[hb as usize].manager.running_regions().is_empty(), "host {hb} runs nothing");
    world_cuts(rig);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, stocked_record(a, 4));
    let (_, _, body) = until_body(rig);
    let y = crate::common::above_floor_y();
    let slime = rig.spawn_slime(a, [REGION_SIZE / 2.0, y, REGION_SIZE / 3.0]);
    let (cut, t) = until_world_cut_on(rig, ha);
    for _ in 0..CROSS_ROUNDS {
        if rig.hosts[hb as usize].manager.world_cut() == Some(cut) {
            break;
        }
        rig.cut_round();
    }
    assert_eq!(rig.hosts[hb as usize].manager.world_cut(), Some(cut), "host {hb} takes part");
    // Over: the player wakes `b`, then the slime follows.
    assert!(rig.teleport(a, body, past_seam(a, b, REGION_SIZE / 2.0)));
    let woke = until_player_in(rig, b);
    let slime_over = woke && rig.teleport(a, slime, past_seam(a, b, REGION_SIZE / 3.0)) && {
        let mut there = false;
        for _ in 0..CROSS_ROUNDS {
            if rig.owners_of(slime).iter().any(|(_, rc)| *rc == b) {
                there = true;
                break;
            }
            rig.cut_round();
        }
        there
    };
    // Back before T.
    if slime_over {
        rig.teleport(b, slime, past_seam(b, a, REGION_SIZE / 3.0));
        if let Some((_, rc, id)) = rig.player_body(P_CLIENT) {
            if rc == b {
                rig.teleport(b, id, past_seam(b, a, REGION_SIZE / 2.0));
            }
        }
    }
    let back_before_t = slime_over && world_tick(rig) + 1 < t;
    let committed = until_decided(rig, cut);
    let marked = rig.hosts[hb as usize].manager.marks_queued().iter().find(|(c, _)| *c == cut).map(|(_, at)| *at);
    assert!(marked.is_some_and(|at| at >= t), "host {hb} queued its marks of K at {marked:?}, before the cut tick {t}");
    if !(back_before_t && committed) {
        return false;
    }
    assert_eq!(rig.last_committed().0, cut);
    assert_eq!(rig.last_cut_writers(P_PLAYER).len(), 1, "the player in exactly one part: {:?}", rig.last_cut_writers(P_PLAYER));
    assert_eq!(rig.stored_places(slime).len(), 1, "the slime stored once: {:?}", rig.stored_places(slime));
    let record = rig.committed_players[&P_PLAYER].clone();
    let mut again = rig.rebuild_from_last_cut();
    again.run(a);
    again.run(b);
    let home = again.owner(record.region);
    again.connect_home(home, P_CLIENT, P_PLAYER, record);
    until_quiet(&mut again);
    assert_eq!(live_count(&mut again, slime), 1, "the slime once in the restarted world");
    assert_eq!(again.player_owners(P_CLIENT).len(), 1, "the player once in the restarted world");
    true
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn region_woken_in_the_lead_window_is_owned_by_exactly_one_part() {
    sweep_hits("region_woken_in_the_lead_window_is_owned_by_exactly_one_part", 2, 1, SEEDS, lead_window);
}

/// Fix round 2: a person's bundle is never a cut row — neither a one-phase cut's row for a
/// transfer to another host that is not acknowledged (`cut_peer_rows`), nor a world cut's
/// case-3 row (`keep_in_part`): the history respawns people (as `cut_crossing` drops them).
/// The case: host A's part of the cut was staged (committed or aborted, it is checked).
fn person_never_a_row(rig: &mut MultiRig) -> bool {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let home_b = rig.homes[hb as usize];
    let (history, _, people) = history_with_site(a, 3);
    rig.run(a);
    rig.run(b);
    world_cuts_over(rig, history);
    for _ in 0..CROSS_ROUNDS {
        if people.iter().all(|p| person_bodies(rig, *p).len() == 1) {
            break;
        }
        rig.cut_round();
    }
    let bodies: Vec<game::EntityId> = people.iter().map(|p| person_bodies(rig, *p)[0].2).collect();
    // `keep_in_part`: once host A's marks are out (host B lagging), a person taken out of `a`
    // goes to B through the manager's path with `view == false` (case 3).
    let (cut, t) = until_world_cut_on(rig, ha);
    while world_tick(rig) + 2 < t {
        rig.cut_round();
    }
    let lag = (t - world_tick(rig)) as u32 + LAG_AROUND_T;
    rig.hosts[hb as usize].manager.spawner_mut().hold(home_b, lag);
    for _ in 0..WORLD_CUT_ROUNDS {
        if matches!(rig.hosts[ha as usize].manager.world_cut_state(), Some((c, "marked")) if c == cut) {
            break;
        }
        rig.cut_round();
    }
    assert!(matches!(rig.hosts[ha as usize].manager.world_cut_state(), Some((c, "marked")) if c == cut), "host {ha} marked");
    let mut bundle = None;
    rig.host_of(a).manager.spawner_mut().with_region(a, |r| {
        let key = r.data().id_index[&bodies[0]];
        bundle = Some(r.data().bundle_of(key));
        r.with_data(|d| d.remove_entity_safe(key, None));
    });
    let mut bundle = bundle.unwrap();
    assert!(matches!(bundle.kind, game::EntityKind::Person { .. }));
    bundle.rebase(a, b);
    let transfer = game::Reliable::Transfer { target: b, stamp: t - 2, bundle: Box::new(bundle), player: None };
    rig.hosts[ha as usize].manager.send_reliable_for_test(hb, transfer, false);
    rig.hosts[hb as usize].manager.spawner_mut().hold(home_b, 0);
    until_decided(rig, cut);
    // Host A's part as it was staged, committed or aborted.
    let staged = match rig.last_cut.as_ref().filter(|(c, _)| *c == cut) {
        Some((_, parts)) => parts.get(&ha).cloned(),
        None => rig.aborted_parts.iter().find(|(c, h, _)| *c == cut && *h == ha).map(|(_, _, p)| p.clone()),
    };
    if let Some(part) = &staged {
        assert!(part_holds(part, bodies[0]).is_none(), "no row of the person in host {ha}'s part");
    }
    // `cut_peer_rows`: the link down, a person walks into the other host; host A's
    // one-phase cut (the other host is no participant of it) writes no row of it.
    rig.link_down(ha, hb);
    let id = bodies[1];
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)), "{a:?} holds the person");
    for _ in 0..WAIT_ROUNDS {
        if rig.hosts[ha as usize].manager.peer_unreceived_ids().iter().any(|(_, i, _)| *i == id) {
            break;
        }
        rig.cut_round();
    }
    assert!(rig.hosts[ha as usize].manager.peer_unreceived_ids().iter().any(|(_, i, _)| *i == id), "queued to host {hb}");
    let one_phase = cut_on(rig, ha as usize);
    assert!(part_holds(&one_phase, id).is_none(), "no row of the person in the one-phase cut");
    staged.is_some()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn person_bundle_is_never_a_cut_row() {
    sweep_hits("person_bundle_is_never_a_cut_row", 2, 1, 2, person_never_a_row);
}

/// Task 14: host B leaves in a world cut. A slime sent into B's parked region before the
/// leave is B's (its mailbox, a row of its leave part); one crossing toward B after host A
/// began the cut is held back on A (A's part) and never reaches B. Once the cut commits,
/// A's session with B is held and forgets its mark and everything before it — those all
/// reached B (B's part needed the mark) — while the held-back slime stays owed, a row of
/// A's next cut (B absent). A reconnect of B's same boot keeps the session held. The
/// scenario's case: A's mark was still unacknowledged when B went away (B's
/// acknowledgements are lost from the cut's begin, [`MultiRig::mute_acks`]).
fn committed_leave(rig: &mut MultiRig) -> bool {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let home_b = rig.homes[hb as usize];
    rig.run(a);
    world_cuts(rig);
    // B's keeper body parks with B's leave (the keeper's player is the host's ordinal).
    let mut keeper = None;
    rig.hosts[hb as usize].manager.spawner_mut().with_region(home_b, |r| {
        let d = r.data();
        keeper = d.player_entites.get(&crate::common::rig::CLIENT).and_then(|k| *d.ecs.id.try_get(*k));
    });
    rig.player_bodies.insert(keeper.expect("B's keeper body"), game::PlayerId(u64::from(hb)));
    let early = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 3.0));
    rig.hosts[hb as usize].manager.request_leave();
    let mut cut = None;
    for _ in 0..WORLD_CUT_ROUNDS {
        let leaving = rig.hosts[hb as usize].manager.leaving_cut();
        if leaving.is_some() && rig.hosts[ha as usize].manager.world_cut() == leaving {
            cut = leaving;
            break;
        }
        rig.cut_round();
    }
    let cut = cut.expect("the leave cut began on both hosts");
    // B's acknowledgements are lost from now on: what A sends B (its mark too) stays in
    // A's session, as with a leaver whose last acknowledgements never arrive.
    rig.mute_acks(hb, ha);
    // After A began the cut: held back on A.
    let late = rig.spawn_slime(a, past_seam(a, b, 2.0 * REGION_SIZE / 3.0));
    let mark = |r: &game::Reliable| matches!(r, game::Reliable::CutMark { cut: c } if *c == cut);
    let carries = |r: &game::Reliable, id: game::EntityId| matches!(r.inner(), game::Reliable::Transfer { bundle, .. } if bundle.id == id);
    let mut hit = false;
    for _ in 0..WORLD_CUT_ROUNDS {
        assert!(
            !rig.hosts[hb as usize].manager.pending_arrival_ids().iter().any(|(id, _)| *id == late),
            "the slime sent after the leave cut began reached the leaving host"
        );
        let prepared = |h: u32| matches!(rig.hosts[h as usize].manager.world_cut_state(), Some((c, "prepared")) if c == cut);
        if rig.decided.contains_key(&cut) || (prepared(ha) && prepared(hb)) {
            // B exits: its link goes.
            hit = rig.hosts[ha as usize].manager.peer_owed(hb).iter().any(mark);
            rig.link_down(ha, hb);
            break;
        }
        rig.cut_round();
    }
    if !until_decided(rig, cut) {
        return false;
    }
    for _ in 0..WAIT_ROUNDS {
        if rig.hosts[ha as usize].manager.peer_left(hb) {
            break;
        }
        rig.cut_round();
    }
    assert!(rig.hosts[ha as usize].manager.peer_left(hb), "host {ha} holds its session with the host that left");
    let owed = rig.hosts[ha as usize].manager.peer_owed(hb);
    assert!(!owed.iter().any(mark), "the mark is forgotten after the committed leave");
    assert!(!owed.iter().any(|r| carries(r, early)), "what went before the mark is forgotten");
    assert!(owed.iter().any(|r| carries(r, late)), "the slime held back is still owed");
    assert_eq!(rig.stored_places(early), [(hb, b, "row")], "the slime sent before the leave is in B's part");
    assert!(
        rig.stored_places(late).iter().all(|(h, _, _)| *h == ha) && rig.stored_places(late).len() == 1,
        "the slime held back is in A's part only: {:?}",
        rig.stored_places(late)
    );
    // B's process reconnects (the same boot): nothing goes out to it.
    rig.link_up(ha, hb);
    for _ in 0..WAIT_ROUNDS {
        rig.cut_round();
    }
    assert!(rig.hosts[ha as usize].manager.peer_left(hb), "still held for the same boot");
    assert!(
        rig.hosts[ha as usize].manager.peer_unreceived_ids().contains(&(hb, late, false)),
        "the held-back slime was never sent"
    );
    // A's next cut, B absent: the slime is A's row.
    until_committed(rig, 1);
    let (last, parts) = rig.last_committed();
    assert!(last > cut && !parts.contains_key(&hb), "the next cut is A's alone");
    assert_eq!(rig.stored_places(late), [(ha, b, "row")], "the slime owed to B is A's row");
    hit
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn held_session_entries_before_the_mark_are_dropped_after_a_committed_leave() {
    sweep_hits("held_session_entries_before_the_mark_are_dropped_after_a_committed_leave", 2, 1, SEEDS, committed_leave);
}

/// Task 14: a transfer arrives on host 0 from host 1 for a region host 2 owns (the hosts'
/// ownership disagreeing): it is bounced on to host 2, where it lands once — never dropped.
fn misrouted_transfer(rig: &mut MultiRig) {
    let home0 = rig.homes[0];
    let home2 = rig.homes[2];
    assert_eq!(rig.owner(home2), 2);
    let id = rig.spawn_slime(home0, [REGION_SIZE / 3.0, crate::common::above_floor_y(), REGION_SIZE / 3.0]);
    let mut bundle = None;
    rig.hosts[0].manager.spawner_mut().with_region(home0, |r| {
        let key = r.data().id_index[&id];
        bundle = Some(r.data().bundle_of(key));
        r.with_data(|d| d.remove_entity_safe(key, None));
    });
    let mut bundle = bundle.unwrap();
    // Standing in the middle of `home2`.
    let y = crate::common::above_floor_y();
    bundle.body.pose = game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, REGION_SIZE / 2.0));
    let stamp = world_tick(rig);
    let transfer = game::Reliable::Transfer { target: home2, stamp, bundle: Box::new(bundle), player: None };
    rig.hosts[0].manager.apply_peer_reliable(1, transfer);
    for _ in 0..CROSS_ROUNDS {
        if rig.owners_of(id) == [(2, home2)] && rig.peer_unreceived() == 0 {
            break;
        }
        rig.round();
    }
    assert_eq!(rig.owners_of(id), [(2, home2)], "bounced on to its owner, once");
    assert_eq!(live_count(rig, id), 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn transfer_for_a_region_of_a_third_host_is_bounced_to_its_owner() {
    sweep("transfer_for_a_region_of_a_third_host_is_bounced_to_its_owner", 3, 1, SEEDS / 4, misrouted_transfer);
}

/// Host B's keeper body may end by parking (B leaves): registered as a player body.
fn keeper_may_park(rig: &mut MultiRig, hb: u32) {
    let home_b = rig.homes[hb as usize];
    let mut keeper = None;
    rig.hosts[hb as usize].manager.spawner_mut().with_region(home_b, |r| {
        let d = r.data();
        keeper = d.player_entites.get(&crate::common::rig::CLIENT).and_then(|k| *d.ecs.id.try_get(*k));
    });
    rig.player_bodies.insert(keeper.expect("B's keeper body"), game::PlayerId(u64::from(hb)));
}

/// Cut rounds until host `hb`'s leave cut has begun on it and on host `ha`: its id.
fn until_leave_begun(rig: &mut MultiRig, ha: u32, hb: u32) -> game::CutId {
    for _ in 0..WORLD_CUT_ROUNDS {
        let leaving = rig.hosts[hb as usize].manager.leaving_cut();
        if leaving.is_some() && rig.hosts[ha as usize].manager.world_cut() == leaving {
            return leaving.unwrap();
        }
        rig.cut_round();
    }
    panic!("the leave cut never began on both hosts; decisions {:?}", rig.decided);
}

/// Host `h`'s coordinator link drops (the runtime tells the core it detached) and, after
/// some rounds, comes back (a new connection: the host attaches again).
fn flap_world_link(rig: &mut MultiRig, h: u32) {
    rig.hosts[h as usize].manager.world_link_mut().unwrap().as_inline_mut().unwrap().set_up(false);
    rig.core().host_detached(h);
    for _ in 0..WAIT_ROUNDS {
        rig.cut_round();
    }
    rig.hosts[h as usize].manager.world_link_mut().unwrap().as_inline_mut().unwrap().set_up(true);
}

/// Fix round 1 (I1): host B asks to leave while attached, and its coordinator link drops
/// before the leave cut begins (the coordinator forgets the request): B asks again with its
/// `Attach`, and the leave cut still begins and commits.
fn leave_after_a_link_flap(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    world_cuts(rig);
    keeper_may_park(rig, hb);
    rig.hosts[hb as usize].manager.request_leave();
    rig.cut_round();
    flap_world_link(rig, hb);
    let cut = until_leave_begun(rig, ha, hb);
    assert!(until_decided(rig, cut), "the leave cut commits");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn leave_request_survives_a_coordinator_link_flap() {
    sweep("leave_request_survives_a_coordinator_link_flap", 2, 1, SEEDS / 4, leave_after_a_link_flap);
}

/// Fix round 1 (I2): host B's leave cut is ABORTED (B's coordinator link drops while it
/// runs). The two slimes host A held back for B go out once the abort is known, in order,
/// each exactly once (B's mailbox; conservation every round). A following ordinary cut of
/// both hosts commits, and B's retried leave commits with the slimes as B's rows.
fn aborted_leave(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    world_cuts(rig);
    keeper_may_park(rig, hb);
    rig.hosts[hb as usize].manager.request_leave();
    let cut = until_leave_begun(rig, ha, hb);
    let first = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 3.0));
    rig.cut_round();
    let second = rig.spawn_slime(a, past_seam(a, b, 2.0 * REGION_SIZE / 3.0));
    let held = |rig: &MultiRig, id| rig.hosts[ha as usize].manager.peer_unreceived_ids().contains(&(hb, id, false));
    for _ in 0..WAIT_ROUNDS {
        if held(rig, first) && held(rig, second) {
            break;
        }
        rig.cut_round();
    }
    assert!(held(rig, first) && held(rig, second), "both slimes held back on host {ha}");
    // B's link goes while the leave cut runs: it is aborted.
    rig.hosts[hb as usize].manager.world_link_mut().unwrap().as_inline_mut().unwrap().set_up(false);
    rig.core().host_detached(hb);
    assert!(!until_decided(rig, cut), "the leave cut is aborted");
    rig.hosts[hb as usize].manager.world_link_mut().unwrap().as_inline_mut().unwrap().set_up(true);
    for _ in 0..CROSS_ROUNDS {
        let waiting: Vec<game::EntityId> = rig.hosts[hb as usize].manager.pending_arrival_ids().into_iter().map(|(id, _)| id).collect();
        if waiting.contains(&first) && waiting.contains(&second) && rig.peer_unreceived() == 0 {
            break;
        }
        rig.cut_round();
    }
    let waiting: Vec<game::EntityId> = rig.hosts[hb as usize].manager.pending_arrival_ids().into_iter().map(|(id, _)| id).collect();
    let at = |id| waiting.iter().position(|w| *w == id);
    assert!(at(first).is_some() && at(second).is_some(), "both slimes released to host {hb}: {waiting:?}");
    assert!(at(first) < at(second), "in the order they were sent");
    assert_eq!(live_count(rig, first), 1);
    assert_eq!(live_count(rig, second), 1);
    assert!(!rig.hosts[ha as usize].manager.peer_left(hb), "an aborted leave holds nothing");
    // An ordinary cut of both hosts, then the retried leave.
    until_committed(rig, 1);
    let (_, parts) = rig.last_committed();
    assert!(parts.contains_key(&ha) && parts.contains_key(&hb), "both hosts in the next cut");
    assert_eq!(rig.hosts[hb as usize].manager.leave_outcome(), Some(false));
    assert!(rig.hosts[hb as usize].manager.retry_leave(), "the leave is asked for again");
    let again = until_leave_begun(rig, ha, hb);
    assert!(again > cut);
    assert!(until_decided(rig, again), "the retried leave commits");
    for id in [first, second] {
        assert_eq!(rig.stored_places(id), [(hb, b, "row")], "{id:?} once, in B's mailbox row");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn aborted_leave_releases_what_was_held_and_the_retry_commits() {
    sweep("aborted_leave_releases_what_was_held_and_the_retry_commits", 2, 1, SEEDS / 4, aborted_leave);
}

/// Fix round 1 (I2): a player handed (walking) and one moved to host B after B parked for
/// its leave cut (each sent before host A's mark: B's) are parked on B, never woken into a
/// region, and each is in exactly one part — B's — of the committed leave.
fn players_toward_a_leaver(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    world_cuts(rig);
    keeper_may_park(rig, hb);
    // A body bundle to carry the handed player (a copy of a slime's, under a new id).
    let slime = rig.spawn_slime(a, [REGION_SIZE / 2.0, crate::common::above_floor_y(), REGION_SIZE / 2.0]);
    let mut bundle = None;
    rig.hosts[ha as usize].manager.spawner_mut().with_region(a, |r| {
        let key = r.data().id_index[&slime];
        bundle = Some(r.data().bundle_of(key));
    });
    let mut bundle = bundle.unwrap();
    bundle.id = game::EntityId { birth_region: a, serial: 999_999 };
    bundle.rebase(a, b);
    bundle.body.pose = game::Pose::from_translation(game::rapier3d::math::Vector::new(1.0, crate::common::above_floor_y(), REGION_SIZE / 2.0));
    let (walker, mover) = ((60, game::PlayerId(600)), (61, game::PlayerId(610)));
    bundle.client = Some((walker.0, game::Client::default()));
    rig.hosts[hb as usize].manager.request_leave();
    let mut cut = None;
    for _ in 0..WORLD_CUT_ROUNDS {
        // On B, right after its BeginCut (no mark of A's yet: A marks at the cut tick).
        if let Some(c) = rig.hosts[hb as usize].manager.leaving_cut() {
            cut = Some(c);
            break;
        }
        rig.cut_round();
    }
    let cut = cut.expect("the leave cut began on B");
    assert!(rig.hosts[hb as usize].manager.leave_parked());
    let handoff = |(client, player): (game::ClientId, game::PlayerId)| game::PlayerHandoff {
        client,
        player,
        claim: None,
        login: None,
        park: false,
        hop: 1,
    };
    let stamp = world_tick(rig);
    let walk = game::Reliable::Transfer { target: b, stamp, bundle: Box::new(bundle), player: Some(handoff(walker)) };
    rig.hosts[hb as usize].manager.apply_peer_reliable(ha, walk);
    let record = centre_of(b);
    let mv = game::Reliable::PlayerMove { handoff: handoff(mover), record: Box::new(record), target: b };
    rig.hosts[hb as usize].manager.apply_peer_reliable(ha, mv);
    for (_, p) in [walker, mover] {
        assert!(rig.hosts[hb as usize].manager.parked_record(p).is_some(), "player {} parked on B", p.0);
    }
    assert!(!rig.hosts[hb as usize].manager.running_regions().contains(&b), "nothing woke {b:?}");
    assert!(until_decided(rig, cut), "the leave cut commits");
    let (_, parts) = rig.last_committed();
    for (_, p) in [walker, mover] {
        assert!(parts[&hb].players.iter().any(|(q, _)| *q == p), "player {} in B's part", p.0);
        assert!(!parts[&ha].players.iter().any(|(q, _)| *q == p), "player {} not in A's part", p.0);
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn players_handed_or_moved_to_a_leaver_park_there_in_one_part() {
    sweep("players_handed_or_moved_to_a_leaver_park_there_in_one_part", 2, 1, SEEDS / 4, players_toward_a_leaver);
}

// --- Every host leaving (Task 14b) ---------------------------------------------------

/// Slimes and deposits that cross the host seam `a` | `b` both ways in the rounds around
/// the hosts' leave requests (so some are on the wire, in a mailbox or held back when a
/// leave cut parks everything), and a player standing on each side.
struct LeaveTraffic {
    a: RegionCoords,
    b: RegionCoords,
    slimes: Vec<(game::EntityId, RegionCoords, RegionCoords)>,
    pushed: usize,
    /// Items put down so far (each deposit before its region parked for a leave).
    items: u64,
}

/// Slimes standing in `a` and `b`, a player on each of their hosts, every keeper body
/// allowed to park, world cuts on. Entities whose region parks for the leave leave live
/// tracking ([`MultiRig::parked_hold`]): they are checked on the stored state.
fn leave_traffic(rig: &mut MultiRig) -> LeaveTraffic {
    let (a, b) = host_seam(rig.hosts.len() as u32, rig.s);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    for h in 0..rig.hosts.len() as u32 {
        keeper_may_park(rig, h);
    }
    rig.parked_hold = true;
    rig.connect_home(ha, P_CLIENT, P_PLAYER, centre_of(a));
    rig.connect_home(hb, P2_CLIENT, P2_PLAYER, centre_of(b));
    let y = crate::common::above_floor_y();
    let slimes = (0..6)
        .map(|i| {
            let (from, to) = if i % 2 == 0 { (a, b) } else { (b, a) };
            (rig.spawn_slime(from, [REGION_SIZE / 2.0, y, 8.0 + 4.0 * i as f32]), from, to)
        })
        .collect();
    rig.round();
    LeaveTraffic { a, b, slimes, pushed: 0, items: 0 }
}

/// Whether `rc` runs on its owner, which has not parked for a leave.
fn runs_unparked(rig: &mut MultiRig, rc: RegionCoords) -> bool {
    let h = rig.owner(rc) as usize;
    !rig.hosts[h].manager.leave_parked() && rig.hosts[h].manager.running_regions().contains(&rc)
}

/// One cut round; before it (from round `seed % 4` on) the next slime is pushed over the
/// seam and a deposit goes across, each only while its source region still runs.
fn traffic_round(rig: &mut MultiRig, t: &mut LeaveTraffic, round: usize) {
    if t.pushed < t.slimes.len() && round >= (rig.seed % 4) as usize {
        let (id, from, to) = t.slimes[t.pushed];
        if runs_unparked(rig, from) {
            rig.teleport(from, id, past_seam(from, to, 8.0 + 4.0 * t.pushed as f32));
        }
        let (from, to) = if t.pushed % 2 == 0 { (t.a, t.b) } else { (t.b, t.a) };
        if runs_unparked(rig, from) {
            crate::common::install_real_block_props();
            let at = deposit_cell(from, to);
            let stack = game::ItemStack { item: DIRT, count: DEPOSIT };
            rig.host_of(from).manager.spawner_mut().with_region(from, |r| {
                let now = r.current_tick();
                r.with_data(|d| assert_eq!(d.deposit_items_safe(at, stack, now, game::PICKUP_DELAY_TICKS), 0));
            });
            t.items += u64::from(DEPOSIT);
        }
        t.pushed += 1;
    }
    rig.cut_round();
}

/// Traffic rounds until every host in `leavers` is named leaving in one world cut and
/// that cut has begun on every host in `on` too: its id.
fn until_leave_of(rig: &mut MultiRig, t: &mut LeaveTraffic, leavers: &[u32], on: &[u32]) -> game::CutId {
    for round in 0..WORLD_CUT_ROUNDS {
        let first = rig.hosts[leavers[0] as usize].manager.leaving_cut();
        if let Some(cut) = first {
            let all_leave = leavers.iter().all(|h| rig.hosts[*h as usize].manager.leaving_cut() == Some(cut));
            let begun = on.iter().all(|h| rig.hosts[*h as usize].manager.world_cut() == Some(cut));
            if all_leave && begun {
                return cut;
            }
        }
        traffic_round(rig, t, round);
    }
    panic!("hosts {leavers:?} never left in one cut begun on {on:?}; decisions {:?}", rig.decided);
}

/// Cut rounds until every host in `leavers` has its leave settled (its `CutDone` came and
/// the server settled it): what its exit loop waits for.
fn until_leaves_settled(rig: &mut MultiRig, leavers: &[u32]) {
    for _ in 0..WAIT_ROUNDS {
        if leavers.iter().all(|h| rig.hosts[*h as usize].manager.leave_outcome().is_some()) {
            return;
        }
        rig.cut_round();
    }
    panic!("the leaves of {leavers:?} were never settled");
}

/// After every host left: each slime is stored exactly once, each player record is
/// written once (by the host it stood on), the coordinator expects nobody; then every
/// host "returns" — fresh managers restored from the store, in-flight rows on their way
/// again — and the world is whole: each slime once, every item once, one body per player.
fn assert_whole_after_leaves(rig: &mut MultiRig, t: &LeaveTraffic) {
    let n = rig.hosts.len() as u32;
    assert_eq!(rig.core().left_hosts(), (0..n).collect::<Vec<_>>(), "every host left");
    for (id, _, _) in &t.slimes {
        assert_eq!(rig.stored_places(*id).len(), 1, "{id:?} stored once: {:?}", rig.stored_places(*id));
    }
    for (player, rc) in [(P_PLAYER, t.a), (P2_PLAYER, t.b)] {
        assert_eq!(rig.committed_players[&player].region, rc, "player {} stored where it stood", player.0);
    }
    let mut again = rig.rebuild_from_last_cut();
    again.run(t.a);
    again.run(t.b);
    for (client, player) in [(P_CLIENT, P_PLAYER), (P2_CLIENT, P2_PLAYER)] {
        let record = rig.committed_players[&player].clone();
        let home = again.owner(record.region);
        again.connect_home(home, client, player, record);
    }
    until_quiet(&mut again);
    for (id, _, _) in &t.slimes {
        assert_eq!(live_count(&mut again, *id), 1, "{id:?} once in the returned world");
    }
    assert!(t.items > 0, "some deposits went across");
    assert_eq!(settled_items(&mut again), t.items, "every item once in the returned world");
    for client in [P_CLIENT, P2_CLIENT] {
        assert_eq!(again.player_owners(client).len(), 1, "one body of client {client}");
    }
}

/// Task 14b: EVERY host asks to leave in the same batch (what the dev stack's shutdown
/// does): one leave cut names them all leaving, each holds back toward every other (no
/// stayer), every part still gets every other's mark and the cut commits — with
/// slimes, deposits (and refusals of them, a leaver's regions being parked) in flight
/// across the seam both ways. The stored world holds everything exactly once, and the
/// returned world is whole.
fn every_host_leaves(rig: &mut MultiRig) {
    let mut t = leave_traffic(rig);
    let all: Vec<u32> = (0..rig.hosts.len() as u32).collect();
    for &h in &all {
        rig.hosts[h as usize].manager.request_leave();
    }
    let cut = until_leave_of(rig, &mut t, &all, &all);
    assert!(until_decided(rig, cut), "the leave cut of every host commits");
    let (last, parts) = rig.last_committed();
    assert_eq!(last, cut);
    assert_eq!(parts.keys().copied().collect::<Vec<_>>(), all, "one part of every host");
    until_leaves_settled(rig, &all);
    for &h in &all {
        assert_eq!(rig.hosts[h as usize].manager.leave_outcome(), Some(true), "host {h} left cleanly");
    }
    assert_whole_after_leaves(rig, &t);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn both_hosts_leave_in_one_cut() {
    sweep("both_hosts_leave_in_one_cut", 2, 1, SEEDS, every_host_leaves);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn three_hosts_leave_in_one_cut() {
    sweep("three_hosts_leave_in_one_cut", 3, 1, SEEDS / 4, every_host_leaves);
}

/// Task 14b: host A asks to leave; host B asks only once A's leave cut K has begun (its
/// request misses K's batch): A leaves in K with B a stayer, then B leaves ALONE in the
/// next cut, its only peer gone. What B held back for A in K (and sent A after it) is B's
/// row of its own leave; the world is whole once both return.
fn consecutive_leaves(rig: &mut MultiRig) {
    let mut t = leave_traffic(rig);
    let (ha, hb) = (rig.owner(t.a), rig.owner(t.b));
    rig.hosts[ha as usize].manager.request_leave();
    let first = until_leave_of(rig, &mut t, &[ha], &[ha, hb]);
    rig.hosts[hb as usize].manager.request_leave();
    assert!(until_decided(rig, first), "host {ha}'s leave cut commits");
    assert_ne!(rig.hosts[hb as usize].manager.leaving_cut(), Some(first), "host {hb} stayed in it");
    let second = until_leave_of(rig, &mut t, &[hb], &[hb]);
    assert!(second > first);
    assert!(rig.hosts[hb as usize].manager.peer_left(ha), "host {hb} saw host {ha} leave");
    assert!(until_decided(rig, second), "host {hb}'s leave cut, alone, commits");
    let (_, parts) = rig.last_committed();
    assert_eq!(parts.keys().copied().collect::<Vec<_>>(), [hb], "the second cut is host {hb}'s alone");
    until_leaves_settled(rig, &[ha, hb]);
    assert_eq!(rig.hosts[hb as usize].manager.leave_outcome(), Some(true));
    assert_eq!(rig.hosts[ha as usize].manager.leave_outcome(), Some(true));
    assert_whole_after_leaves(rig, &t);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn hosts_leave_in_consecutive_cuts() {
    sweep("hosts_leave_in_consecutive_cuts", 2, 1, SEEDS, consecutive_leaves);
}

// --- Login placement across hosts (Task 15) ------------------------------------------

/// The test login's player token (any string: the manager never resolves it).
const LOGIN_TOKEN: &str = "5ca1ab1e5ca1ab1e";
/// The session id of the test player's newer login.
const NEW_LOGIN_CLIENT: game::ClientId = 12;
/// A player no test connects.
const FRESH_PLAYER: game::PlayerId = game::PlayerId(103);

/// What each host told its login service so far, appended to `out` as `(host, message)`.
fn collect_login_out(rig: &mut MultiRig, out: &mut Vec<(u32, game::LoginOut)>) {
    for (h, r) in rig.hosts.iter_mut().enumerate() {
        out.extend(r.manager.take_login_out().into_iter().map(|o| (h as u32, o)));
    }
}

/// The host of a 3-host rig that owns neither `a` nor `b` (the entry host of the login
/// tests: the login service is linked to it only), set as every host's entry host.
fn third_host_as_entry(rig: &mut MultiRig, a: RegionCoords, b: RegionCoords) -> u32 {
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let entry = (0..rig.hosts.len() as u32).find(|h| *h != ha && *h != hb).expect("a third host");
    for r in &mut rig.hosts {
        r.manager.set_entry_host(entry);
    }
    entry
}

/// `place_spawn`'s rules in order, each with the earlier ones absent: no record → the
/// owner of the spawn region; a record → its region's owner; a player handed away
/// (`departed`) → where it went, whatever its record says; a player held here (online,
/// then parked) → here, whatever its record says.
fn place_spawn_rules(rig: &mut MultiRig) {
    use game::Placement::{Here, Host};
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let at = |h: u32, owner: u32| if h == owner { Here } else { Host(owner) };
    let spawn_owner = rig.owner(game::SPAWN_REGION);
    for h in [ha, hb] {
        let m = &rig.hosts[h as usize].manager;
        assert_eq!(m.place_spawn(FRESH_PLAYER, None), at(h, spawn_owner), "no record, on host {h}");
        assert_eq!(m.place_spawn(FRESH_PLAYER, Some(a)), at(h, ha), "a record in {a:?}, on host {h}");
        assert_eq!(m.place_spawn(FRESH_PLAYER, Some(b)), at(h, hb), "a record in {b:?}, on host {h}");
    }
    rig.run(a);
    rig.run(b);
    rig.connect_login(ha, P_CLIENT, P_PLAYER, Some(record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0])), LOGIN_TOKEN, 1);
    let (_, _, id) = until_body(rig);
    assert_eq!(rig.hosts[ha as usize].manager.place_spawn(P_PLAYER, Some(b)), Here, "online here");
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    // Its stored record still says `a` (no cut since).
    assert_eq!(rig.hosts[ha as usize].manager.place_spawn(P_PLAYER, Some(a)), Host(hb), "handed away: where it went");
    assert_eq!(rig.hosts[hb as usize].manager.place_spawn(P_PLAYER, Some(a)), Here, "online here (handed here)");
    // Its session never comes to hb: parked there after the grace.
    for _ in 0..CROSS_ROUNDS {
        if !rig.parked_on(P_PLAYER).is_empty() {
            break;
        }
        rig.maintain_all(game::HANDOFF_SESSION_GRACE_MS / 2);
        rig.round();
    }
    assert_eq!(rig.parked_on(P_PLAYER), [hb]);
    assert_eq!(rig.hosts[hb as usize].manager.place_spawn(P_PLAYER, Some(a)), Here, "parked here");
    assert_eq!(rig.hosts[ha as usize].manager.place_spawn(P_PLAYER, Some(a)), Host(hb), "parked where it went");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn place_spawn_order_of_rules() {
    sweep("place_spawn_order_of_rules", 2, 1, SEEDS / 4, place_spawn_rules);
}

/// One login of the test player through the entry host: the order (stored record in `a`)
/// at `entry`, `walk` called before the rounds it names; the server answers taken orders
/// every `serve_every` rounds. Then the session (`client`, `epoch`) connects where the
/// acknowledgement said. The host that took the order, and whether that host refused the
/// session (its player left it meanwhile: `PLAYER_ELSEWHERE_REFUSAL`), and the round the
/// server answered the order in.
fn login_through_entry(
    rig: &mut MultiRig,
    entry: u32,
    (client, epoch): (game::ClientId, u64),
    a: RegionCoords,
    serve_every: u64,
    out: &mut Vec<(u32, game::LoginOut)>,
    mut walk: impl FnMut(&mut MultiRig, u64),
) -> (u32, bool, u64) {
    let order = game::ServerEvent::SpawnOrder {
        origin: entry,
        hops: 0,
        player_token: LOGIN_TOKEN.into(),
        session_epoch: epoch,
        player: P_PLAYER,
        record_region: Some(a),
    };
    rig.hosts[entry as usize].event(order);
    let mut taken = Vec::new();
    let is_ack = |o: &game::LoginOut| matches!(o, game::LoginOut::SpawnAck { session_epoch, .. } if *session_epoch == epoch);
    for round in 0..CROSS_ROUNDS as u64 {
        walk(rig, round);
        rig.round();
        if round % serve_every == serve_every - 1 {
            for t in rig.serve_logins() {
                taken.push((round, t));
            }
        }
        collect_login_out(rig, out);
        if out.iter().any(|(_, o)| is_ack(o)) {
            break;
        }
    }
    assert_eq!(taken.len(), 1, "one host took the order of epoch {epoch}: {taken:?}");
    let (answered, (placed, _)) = taken[0];
    let acks: Vec<&(u32, game::LoginOut)> = out.iter().filter(|(_, o)| is_ack(o)).collect();
    let want = game::LoginOut::SpawnAck {
        player_token: LOGIN_TOKEN.into(),
        session_epoch: epoch,
        ok: true,
        endpoint: Some(rig_endpoint(placed)),
    };
    assert_eq!(acks, [&(entry, want)], "one acknowledgement, at the entry host, naming host {placed}");
    let centre = [REGION_SIZE / 2.0, REGION_SIZE / 2.0];
    rig.record_packets();
    rig.connect_login(placed, client, P_PLAYER, Some(record_at(a, centre)), LOGIN_TOKEN, epoch);
    let seen = rig.hosts[placed as usize].packets_seen.as_ref().expect("recording");
    let refused = seen.iter().any(|(to, p)| {
        *to == Some(client) && matches!(p, game::ServerPacket::Refused(why) if why == game::PLAYER_ELSEWHERE_REFUSAL)
    });
    (placed, refused, answered)
}

/// 3 hosts: the player walks from host A to host B; around the same round a newer login
/// of it is ordered at the entry host E (neither), with the stored record still in A's
/// region. E sends the order to A; A either still holds the player — it takes the order,
/// and the player stays while online (no hand-off while its new session is awaited) — or
/// relays it to B on the stream that carries the hand-off: behind it, so B holds the
/// player when the order comes. One acknowledgement reaches E, naming the host that took
/// the order, and the new session connects there. Only if A's supersede parked the player
/// in the very tick its body left (the park then goes on with it to B) is that session
/// refused (`PLAYER_ELSEWHERE_REFUSAL`, nothing placed); the client logs in again and that
/// order is relayed to B. Either way the player ends as one body, and each login's end
/// reaches E once.
fn spawn_behind_handoff(rig: &mut MultiRig) {
    let (a, b) = host_seam(3, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    let entry = third_host_as_entry(rig, a, b);
    rig.run(a);
    rig.run(b);
    let centre = [REGION_SIZE / 2.0, REGION_SIZE / 2.0];
    rig.connect_login(ha, P_CLIENT, P_PLAYER, Some(record_at(a, centre)), LOGIN_TOKEN, 1);
    let (_, _, id) = until_body(rig);
    // The walk starts 0..=2 rounds after the order, and the server answers a taken order
    // at once or only every third round (per seed and delay): the order meets the player
    // on A before, during or after its hand-off, and a player A took the order for may try
    // to walk on before its supersede comes. (Superseded first, it has no body to push.)
    let mix = rig.seed + u64::from(rig.link_delay);
    let (lead, serve_every) = (mix % 3, if (mix / 3) % 2 == 0 { 3 } else { 1 });
    let mut out = Vec::new();
    let walk = |rig: &mut MultiRig, round: u64| {
        if round == lead {
            rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0));
        }
    };
    let (mut placed, refused, answered) =
        login_through_entry(rig, entry, (NEW_LOGIN_CLIENT, 2), a, serve_every, &mut out, walk);
    assert!(placed == ha || placed == hb, "placed on host {placed}, which never held the player");
    let mut client = NEW_LOGIN_CLIENT;
    if refused {
        assert_eq!(placed, ha, "only A can lose the player after taking the order");
        // Online, it was kept on A (no hand-off while its new session is awaited): only a
        // player already parking when its body left — superseded before the walk — goes.
        assert!(answered < lead, "the player left A online after A took its order (answered in round {answered}, walk in {lead})");
        rig.players.remove(&NEW_LOGIN_CLIENT);
        client = NEW_LOGIN_CLIENT + 1;
        let again;
        (again, _, _) = login_through_entry(rig, entry, (client, 3), a, 1, &mut out, |_, _| {});
        assert_eq!(again, hb, "the login again lands where the player went");
        placed = again;
    }
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        collect_login_out(rig, &mut out);
        if rig.player_owners(client).len() == 1 && rig.peer_unreceived() == 0 {
            break;
        }
    }
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        collect_login_out(rig, &mut out);
    }
    let owners = rig.player_owners(client);
    assert_eq!(owners.len(), 1, "the new login's one body: {owners:?}");
    assert_eq!(owners[0].0, placed, "on the host that took the order");
    assert!(rig.player_owners(P_CLIENT).is_empty(), "the old session's body is gone");
    assert!(rig.parked_on(P_PLAYER).is_empty(), "no parked copy beside the body");
    let ended_once = |epoch: u64| {
        let end = game::LoginOut::SessionEnded { player_token: LOGIN_TOKEN.into(), session_epoch: epoch };
        let ends: Vec<&(u32, game::LoginOut)> = out.iter().filter(|(_, o)| *o == end).collect();
        assert_eq!(ends, [&(entry, end.clone())], "login {epoch} ended once, told to the entry host: {out:?}");
    };
    ended_once(1);
    if refused {
        ended_once(2);
    }
    let live = if refused { 3 } else { 2 };
    assert!(
        !out.iter().any(|(_, o)| matches!(o, game::LoginOut::SessionEnded { session_epoch, .. } if *session_epoch == live)),
        "the live login did not end"
    );
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn spawn_order_relayed_behind_a_hand_off_places_one_body() {
    sweep("spawn_order_relayed_behind_a_hand_off_places_one_body", 3, 1, SEEDS / 2, spawn_behind_handoff);
}

/// 3 hosts: the player walks from host A to host B and the login service kicks its login
/// at the entry host E in the same round. Wherever the kick meets the player (on A before
/// the hand-off, on B after it, or on B before the hand-off arrives — remembered there), it
/// is parked once; E acknowledges the kick once, after every peer applied it, and the
/// login's end reaches E once.
fn kick_crossing(rig: &mut MultiRig) {
    let (a, b) = host_seam(3, 1);
    let entry = third_host_as_entry(rig, a, b);
    rig.run(a);
    rig.run(b);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.connect_login(ha, P_CLIENT, P_PLAYER, Some(record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0])), LOGIN_TOKEN, 1);
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    let kick = game::ServerEvent::Kick { player_token: LOGIN_TOKEN.into(), session_epoch: 1, reason: "signed out".into() };
    rig.hosts[entry as usize].event(kick);
    let mut out = Vec::new();
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        rig.serve_logins();
        collect_login_out(rig, &mut out);
        if !rig.parked_on(P_PLAYER).is_empty() && rig.peer_unreceived() == 0 && rig.player_owners(P_CLIENT).is_empty() {
            break;
        }
    }
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        rig.serve_logins();
        collect_login_out(rig, &mut out);
    }
    assert!(rig.player_owners(P_CLIENT).is_empty(), "the kicked session has no body");
    let parked = rig.parked_on(P_PLAYER);
    assert!(parked == [ha] || parked == [hb], "parked once, on A or B: {parked:?}");
    let ack = game::LoginOut::KickAck { player_token: LOGIN_TOKEN.into(), session_epoch: 1 };
    let ended = game::LoginOut::SessionEnded { player_token: LOGIN_TOKEN.into(), session_epoch: 1 };
    let acks: Vec<&(u32, game::LoginOut)> = out.iter().filter(|(_, o)| *o == ack).collect();
    assert_eq!(acks, [&(entry, ack.clone())], "acknowledged once, by the entry host: {out:?}");
    let ends: Vec<&(u32, game::LoginOut)> = out.iter().filter(|(_, o)| *o == ended).collect();
    assert_eq!(ends, [&(entry, ended.clone())], "the login ended once, told to the entry host: {out:?}");
    // A resend (the login service's sweep) is acknowledged again and changes nothing.
    rig.hosts[entry as usize].event(game::ServerEvent::Kick {
        player_token: LOGIN_TOKEN.into(),
        session_epoch: 1,
        reason: "signed out".into(),
    });
    out.clear();
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        rig.serve_logins();
        collect_login_out(rig, &mut out);
    }
    assert_eq!(out, [(entry, ack)], "a resend: one more acknowledgement, nothing else");
    assert_eq!(rig.parked_on(P_PLAYER), parked);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn kick_reaches_a_crossing_login_and_is_acked_once() {
    sweep("kick_reaches_a_crossing_login_and_is_acked_once", 3, 1, SEEDS / 2, kick_crossing);
}

/// A spawn order placed from the STORE waits for every peer's first sync: the test
/// player's move from A into B (an embark) is queued on A while the A–B link is down; a
/// login of it is ordered at B with the stored record already in B's region. B holds the
/// player not yet, so the order would place it from the store — it waits instead, until the
/// link is back and A's backlog (the move, before A's `Running`) has come. Then B takes it
/// from the live player: the new login gets the moved record (its fortress), not the stored
/// one.
fn store_spawn_waits_for_sync(rig: &mut MultiRig) {
    let (b, ha, hb) = move_queued_link_down(rig, true);
    rig.hosts[hb as usize].manager.set_entry_host(hb);
    let order = game::ServerEvent::SpawnOrder {
        origin: hb,
        hops: 0,
        player_token: LOGIN_TOKEN.into(),
        session_epoch: 2,
        player: P_PLAYER,
        record_region: Some(b),
    };
    rig.hosts[hb as usize].event(order);
    let (mut taken, mut out) = (Vec::new(), Vec::new());
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        taken.extend(rig.serve_logins());
        collect_login_out(rig, &mut out);
    }
    assert!(taken.is_empty() && out.is_empty(), "nothing placed while host {ha} has not synced: {taken:?} {out:?}");
    assert_eq!(rig.hosts[hb as usize].manager.spawn_orders_waiting(), 1, "the order waits");
    rig.link_up(ha, hb);
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        taken.extend(rig.serve_logins());
        collect_login_out(rig, &mut out);
        if !out.is_empty() {
            break;
        }
    }
    assert_eq!(taken.iter().map(|(h, _)| *h).collect::<Vec<_>>(), [hb], "host {hb} took it");
    let ack = game::LoginOut::SpawnAck { player_token: LOGIN_TOKEN.into(), session_epoch: 2, ok: true, endpoint: Some(rig_endpoint(hb)) };
    assert_eq!(out, [(hb, ack)]);
    // The new login connects with the store's (older) record: the live one wins.
    rig.connect_login(hb, NEW_LOGIN_CLIENT, P_PLAYER, Some(centre_of(b)), LOGIN_TOKEN, 2);
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if rig.player_owners(NEW_LOGIN_CLIENT).len() == 1 && rig.peer_unreceived() == 0 {
            break;
        }
    }
    assert_eq!(rig.player_owners(NEW_LOGIN_CLIENT), [(hb, b)], "one body, where the move took it");
    assert!(rig.parked_on(P_PLAYER).is_empty());
    let claim = rig.hosts[hb as usize].manager.client_claim(NEW_LOGIN_CLIENT);
    assert_eq!(claim, Some(game::Claim::Fortress(EMBARK_SITE)), "the moved record's fortress, not the stored record's none");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn spawn_from_the_store_waits_for_every_peer_sync() {
    sweep("spawn_from_the_store_waits_for_every_peer_sync", 2, 1, SEEDS / 4, store_spawn_waits_for_sync);
}

// --- Task 15 fix round 1 -----------------------------------------------------------

/// A spawn order for a player no host holds, its stored record in `rc`, at `h` (its own
/// entry host); rounds (the server answering) until host `h` told its login service
/// something, or `budget` rounds. What it told.
fn store_order_at(rig: &mut MultiRig, h: u32, rc: RegionCoords, epoch: u64, budget: usize) -> Vec<(u32, game::LoginOut)> {
    rig.hosts[h as usize].manager.set_entry_host(h);
    let order = game::ServerEvent::SpawnOrder {
        origin: h,
        hops: 0,
        player_token: LOGIN_TOKEN.into(),
        session_epoch: epoch,
        player: FRESH_PLAYER,
        record_region: Some(rc),
    };
    rig.hosts[h as usize].event(order);
    let mut out = Vec::new();
    for _ in 0..budget {
        rig.serve_logins();
        collect_login_out(rig, &mut out);
        if !out.is_empty() {
            break;
        }
        rig.round();
    }
    out
}

/// 3 hosts; host C is gone and host A's connection to it is new (as after A restarted
/// while C was away). C left cleanly — A's server read C's clean `hosts` row at boot and
/// told the manager (`note_left_peers`): a store placement on A is taken at once. Then C
/// returns as a new boot: it is expected again (and synced by its `Running`).
fn left_host_not_awaited(rig: &mut MultiRig) {
    let (ha, hc) = (0, 2);
    let a = rig.homes[ha as usize];
    rig.link_down(ha, hc);
    rig.link_down(1, hc);
    rig.round();
    assert!(!rig.hosts[ha as usize].manager.peers_synced(), "host {hc} is gone: not synced");
    // The boot C left as (before the one that comes back).
    rig.hosts[ha as usize].manager.note_left_peers([(hc, 1)]);
    assert!(rig.hosts[ha as usize].manager.peers_synced(), "a host that left holds nobody");
    let out = store_order_at(rig, ha, a, 2, 1);
    let ack = game::LoginOut::SpawnAck { player_token: LOGIN_TOKEN.into(), session_epoch: 2, ok: true, endpoint: Some(rig_endpoint(ha)) };
    assert_eq!(out, [(ha, ack)], "taken without waiting");
    // C comes back (a new boot's hello): expected again, synced once its `Running` came.
    rig.link_up(ha, hc);
    rig.link_up(1, hc);
    let mut synced = false;
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        if !rig.hosts[ha as usize].manager.peer_left(hc) && rig.hosts[ha as usize].manager.peers_synced() {
            synced = true;
            break;
        }
    }
    assert!(synced, "host {hc} is expected again and synced");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_that_left_cleanly_is_not_awaited_by_a_store_placement() {
    sweep("a_host_that_left_cleanly_is_not_awaited_by_a_store_placement", 3, 1, SEEDS / 4, left_host_not_awaited);
}

/// The same with host C merely down (crashed, not left): the store placement waits for it
/// and is refused once `SPAWN_SYNC_WAIT_MS` passed.
fn down_host_awaited(rig: &mut MultiRig) {
    let (ha, hc) = (0, 2);
    let a = rig.homes[ha as usize];
    rig.link_down(ha, hc);
    rig.link_down(1, hc);
    rig.round();
    let out = store_order_at(rig, ha, a, 2, WAIT_ROUNDS);
    assert!(out.is_empty(), "nothing while host {hc} may hold the player: {out:?}");
    assert_eq!(rig.hosts[ha as usize].manager.spawn_orders_waiting(), 1);
    rig.maintain_all(game::SPAWN_SYNC_WAIT_MS);
    let mut out = Vec::new();
    rig.round();
    collect_login_out(rig, &mut out);
    let refused = game::LoginOut::SpawnAck { player_token: LOGIN_TOKEN.into(), session_epoch: 2, ok: false, endpoint: None };
    assert_eq!(out, [(ha, refused)], "refused after the wait");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_crashed_host_is_still_awaited_by_a_store_placement() {
    sweep("a_crashed_host_is_still_awaited_by_a_store_placement", 3, 1, SEEDS / 4, down_host_awaited);
}

/// An order host A took for its online player is answered `ok: false` (the server refused
/// it, e.g. stale): the player is not kept on A — it crosses the host seam as before.
fn refused_order_unpins(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let (ha, hb) = (rig.owner(a), rig.owner(b));
    rig.run(a);
    rig.run(b);
    rig.hosts[ha as usize].manager.set_entry_host(ha);
    rig.connect_login(ha, P_CLIENT, P_PLAYER, Some(record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0])), LOGIN_TOKEN, 2);
    let (_, _, id) = until_body(rig);
    let order = game::ServerEvent::SpawnOrder {
        origin: ha,
        hops: 0,
        player_token: LOGIN_TOKEN.into(),
        session_epoch: 1,
        player: P_PLAYER,
        record_region: Some(a),
    };
    rig.hosts[ha as usize].event(order);
    let taken = rig.hosts[ha as usize].manager.take_spawn_here();
    assert_eq!(taken.len(), 1, "held here: taken");
    rig.hosts[ha as usize].manager.spawn_answered(taken[0].clone(), false);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    until_player_settled_in(rig, b);
    assert_eq!(rig.player_owners(P_CLIENT), [(hb, b)], "it crossed");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_refused_order_does_not_keep_the_player() {
    sweep("a_refused_order_does_not_keep_the_player", 2, 1, SEEDS / 4, refused_order_unpins);
}

/// A login whose session the manager refuses while placing it (its record in another
/// host's region, no relay in between): its end is reported exactly once, and no login is
/// kept for the dead session (a later close of its connection reports nothing more).
fn refused_login_ends_once(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let ha = rig.owner(a);
    rig.hosts[ha as usize].manager.set_entry_host(ha);
    rig.connect_login(ha, P_CLIENT, P_PLAYER, Some(record_at(b, [REGION_SIZE / 2.0, REGION_SIZE / 2.0])), LOGIN_TOKEN, 1);
    let mut out = Vec::new();
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        collect_login_out(rig, &mut out);
    }
    let end = game::LoginOut::SessionEnded { player_token: LOGIN_TOKEN.into(), session_epoch: 1 };
    assert_eq!(out, [(ha, end.clone())], "its end reported when it was refused, not at a later close");
    rig.disconnect(ha, P_CLIENT);
    for _ in 0..WAIT_ROUNDS {
        rig.round();
        collect_login_out(rig, &mut out);
    }
    assert_eq!(out, [(ha, end)], "and only once");
    assert!(rig.player_owners(P_CLIENT).is_empty());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_login_refused_while_placed_ends_once() {
    sweep("a_login_refused_while_placed_ends_once", 2, 1, SEEDS / 4, refused_login_ends_once);
}


// --- R4 Task 4: a super-region with no owner this host knows ---------------------------

/// Directory rows laid out as `static` would for the rig's hosts over every super-region
/// within reach of the test regions and the keepers' homes (`s` = 1: a super-region is a
/// region), at epoch 1 — but `free` as free rows and `unknown` with no row at all.
fn rendezvous_rows(
    rig: &MultiRig,
    free: &[game::SuperRegion],
    unknown: &[game::SuperRegion],
) -> Vec<(game::SuperRegion, Option<u32>, u64)> {
    let hosts = rig.hosts.len() as u32;
    let mut rows = Vec::new();
    for sx in -16..=48 {
        for sz in -16..=48 {
            let sr = (sx, sz);
            if unknown.contains(&sr) {
                continue;
            }
            let owner = (!free.contains(&sr)).then(|| game::rendezvous_owner(sr, hosts));
            rows.push((sr, owner, 1));
        }
    }
    rows
}

/// The super-region of `rc` in the rig (`s` regions per side).
fn sr_of(rig: &MultiRig, rc: RegionCoords) -> game::SuperRegion {
    game::super_region_of(rc, rig.s)
}

/// A slime walks from `a` into `b`, whose super-region is free: it waits on `a`'s host
/// (`unowned_waiting`, nowhere else), the next committed world cut holds it once — as a
/// row of the sender's part — and the world restarted from that cut holds it once again,
/// waiting on the same host.
fn into_a_free_super_region(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let rows = rendezvous_rows(rig, &[sr_of(rig, b)], &[]);
    rig.with_directory(rows);
    let ha = rig.owner(a);
    assert_eq!(rig.hosts[ha as usize].manager.owner_of(b), game::Owner::Free);
    rig.run(a);
    world_cuts(rig);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    let waiting = |rig: &MultiRig| -> Vec<(u32, game::SuperRegion, game::EntityId)> {
        let mut out = Vec::new();
        for (h, r) in rig.hosts.iter().enumerate() {
            out.extend(r.manager.unowned_waiting_ids().into_iter().map(|(sr, id)| (h as u32, sr, id)));
        }
        out
    };
    let held = [(ha, sr_of(rig, b), id)];
    assert!(until(rig, CROSS_ROUNDS, |rig| waiting(rig) == held), "the slime never waited: {:?}", waiting(rig));
    for _ in 0..WAIT_ROUNDS {
        rig.cut_round();
        assert_eq!(waiting(rig), held, "it waits on host {ha} for an owner");
        assert!(rig.owners_of(id).is_empty(), "in no region");
        assert_eq!(rig.peer_unreceived(), 0, "sent to no host");
    }
    until_committed(rig, 1);
    assert_eq!(rig.stored_places(id), [(ha, b, "row")], "a row of the sender's part, once");
    assert_eq!(waiting(rig), held, "still waiting after the cut");

    let mut again = rig.rebuild_from_last_cut();
    assert_eq!(waiting(&again), held, "the restarted world holds it once, waiting on host {ha}");
    again.run(a);
    for _ in 0..WAIT_ROUNDS {
        again.round();
    }
    assert_eq!(waiting(&again), held, "and keeps it waiting");
    assert!(again.owners_of(id).is_empty());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn transfer_into_a_free_super_region_waits_and_is_a_row_of_the_senders_cut() {
    sweep(
        "transfer_into_a_free_super_region_waits_and_is_a_row_of_the_senders_cut",
        2,
        1,
        SEEDS / 2,
        into_a_free_super_region,
    );
}

/// A player walks from `a` toward `b`, whose super-region this host has no row for
/// (`Unknown`): never guessed — its body is handed back to `a`, its home and session stay,
/// nothing is queued to any host or waits for an owner.
fn clamped_at_an_unknown_seam(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let rows = rendezvous_rows(rig, &[], &[sr_of(rig, b)]);
    rig.with_directory(rows);
    let ha = rig.owner(a);
    assert_eq!(rig.hosts[ha as usize].manager.owner_of(b), game::Owner::Unknown);
    rig.run(a);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    let (_, _, id) = until_body(rig);
    assert!(rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0)));
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.player_owners(P_CLIENT), [(ha, a)], "handed back to {a:?}");
    let m = &rig.hosts[ha as usize].manager;
    assert_eq!(m.home_of(P_CLIENT), Some(a));
    assert_eq!(m.session_kind(P_CLIENT), Some(game::SessionKind::Home));
    assert!(m.departed().is_empty());
    assert_eq!(rig.peer_unreceived(), 0, "nothing queued to any host");
    assert!(rig.hosts.iter().all(|r| r.manager.unowned_waiting_ids().is_empty()), "nothing waits for an owner");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_is_clamped_at_a_seam_whose_owner_is_unknown() {
    sweep("player_is_clamped_at_a_seam_whose_owner_is_unknown", 2, 1, SEEDS / 4, clamped_at_an_unknown_seam);
}

// --- R4 Task 5: claiming on demand, `Owns`, `ClaimWanted` ------------------------------

/// The test player, standing in `a`, is pushed over its seam with `b` every round until its
/// body stands in `b` (it is clamped back while `b` has no owner its host knows); `false`
/// if it never got there within `rounds`.
fn push_player_into(rig: &mut MultiRig, a: RegionCoords, b: RegionCoords, rounds: usize) -> bool {
    for _ in 0..rounds {
        match rig.player_body(P_CLIENT) {
            Some((_, rc, _)) if rc == b => return true,
            Some((_, rc, id)) if rc == a => {
                rig.teleport(a, id, past_seam(a, b, REGION_SIZE / 2.0));
            }
            _ => {}
        }
        rig.round();
    }
    matches!(rig.player_body(P_CLIENT), Some((_, rc, _)) if rc == b)
}

/// Every host's answer for who owns `rc`.
fn owners_seen(rig: &MultiRig, rc: RegionCoords) -> Vec<Option<u32>> {
    rig.hosts.iter().map(|r| r.manager.owner_host(rc)).collect()
}

/// Nothing waits for an owner on any host.
fn nothing_unowned(rig: &MultiRig) -> bool {
    rig.hosts.iter().all(|r| r.manager.unowned_waiting_ids().is_empty())
}

/// A player walks from `a` toward `b`, whose super-region is free: its host asks for the
/// super-region to be claimed — by the preferred live host (`rendezvous_owner` with every
/// host live), which claims it — every directory ends naming that host, and the player
/// crosses into `b` there, once.
fn claimed_when_a_player_walks_to_it(rig: &mut MultiRig) {
    let hosts = rig.hosts.len() as u32;
    let (a, b) = host_seam(hosts, 1);
    let sr = sr_of(rig, b);
    rig.with_directory(rendezvous_rows(rig, &[sr], &[]));
    rig.with_claims();
    let ha = rig.owner(a);
    let preferred = game::rendezvous_owner(sr, hosts);
    assert_eq!(rig.hosts[ha as usize].manager.owner_of(b), game::Owner::Free);
    rig.run(a);
    rig.connect_everywhere(ha, P_CLIENT, P_PLAYER, record_at(a, [REGION_SIZE / 2.0, REGION_SIZE / 2.0]));
    until_body(rig);
    assert!(push_player_into(rig, a, b, WALK_ROUNDS), "the player never crossed into {b:?}: {:?}", rig.player_body(P_CLIENT));
    assert_eq!(rig.stored_owner(sr), Some(preferred), "the preferred host claimed it");
    assert!(
        until(rig, CROSS_ROUNDS, |rig| owners_seen(rig, b).iter().all(|o| *o == Some(preferred))),
        "every directory names host {preferred}: {:?}",
        owners_seen(rig, b)
    );
    assert_eq!(rig.player_owners(P_CLIENT), [(preferred, b)], "the player is in {b:?} once, on host {preferred}");
    assert!(nothing_unowned(rig), "nothing waits for an owner");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_free_super_region_is_claimed_by_its_preferred_host_when_a_player_walks_to_it() {
    for hosts in [2, 3] {
        sweep(
            "a_free_super_region_is_claimed_by_its_preferred_host_when_a_player_walks_to_it",
            hosts,
            1,
            SEEDS / 4,
            claimed_when_a_player_walks_to_it,
        );
    }
}

/// A slime walks from `a` into free `b`, whose preferred host is not `a`'s: `a`'s host
/// never claims it — it asks the preferred host (`ClaimWanted`), which claims it — and
/// the slime, waiting meanwhile, ends in `b` on that host, once.
fn claim_wanted_reaches_preferred(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let sr = sr_of(rig, b);
    rig.with_directory(rendezvous_rows(rig, &[sr], &[]));
    rig.with_claims();
    let ha = rig.owner(a);
    let preferred = game::rendezvous_owner(sr, 2);
    assert_ne!(ha, preferred, "the needing host is not the preferred one");
    rig.run(a);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    assert!(
        until(rig, CROSS_ROUNDS, |rig| owners_seen(rig, b).iter().all(|o| *o == Some(preferred))),
        "every directory names host {preferred}: {:?}",
        owners_seen(rig, b)
    );
    assert_eq!(rig.stored_owner(sr), Some(preferred), "claimed by the preferred host, not by host {ha}");
    rig.run(b);
    until_settled_in(rig, id, b);
    assert!(nothing_unowned(rig), "nothing waits for an owner");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn claim_wanted_reaches_the_preferred_host() {
    sweep("claim_wanted_reaches_the_preferred_host", 2, 1, SEEDS / 2, claim_wanted_reaches_preferred);
}

/// A slime waits on `a`'s host for free `b`; then the other host claims `b`'s super-region
/// (its server's `claimed`, no claim loop): its `Owns` tells `a`'s host, which sends the
/// slime on — it ends in `b` once, and a committed world cut afterwards holds it once.
fn owns_delivers_what_waited(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let sr = sr_of(rig, b);
    rig.with_directory(rendezvous_rows(rig, &[sr], &[]));
    let (ha, hb) = (rig.owner(a), 1 - rig.owner(a));
    rig.run(a);
    world_cuts(rig);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    assert!(
        until(rig, CROSS_ROUNDS, |rig| rig.hosts[ha as usize].manager.unowned_waiting_ids() == [(sr, id)]),
        "the slime never waited for an owner"
    );
    rig.hosts[hb as usize].manager.claimed(vec![(sr, 2)]);
    assert!(
        until(rig, CROSS_ROUNDS, |rig| rig.hosts[ha as usize].manager.owner_of(b) == game::Owner::Host(hb)),
        "host {ha} never learned host {hb}'s claim"
    );
    rig.run(b);
    until_settled_in(rig, id, b);
    assert!(nothing_unowned(rig), "nothing waits for an owner");
    until_committed(rig, 1);
    assert_eq!(rig.stored_places(id).len(), 1, "stored once: {:?}", rig.stored_places(id));
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn owns_from_a_peer_delivers_what_waited() {
    sweep("owns_from_a_peer_delivers_what_waited", 2, 1, SEEDS / 2, owns_delivers_what_waited);
}

/// Host `ha` has no row for `b`'s super-region (`Unknown`); the other host holds it in its
/// directory (read, never claimed in this session: no `Owns` was sent). A slime waits on
/// `ha` for it. Across a reconnect the other host's hello answer (its whole `Owns`) tells
/// `ha`, which sends the slime on: it ends in `b` once.
fn late_peer_learns(rig: &mut MultiRig) {
    let (a, b) = host_seam(2, 1);
    let sr = sr_of(rig, b);
    rig.with_directory(rendezvous_rows(rig, &[], &[sr]));
    let (ha, hb) = (rig.owner(a), 1 - rig.owner(a));
    let mut theirs = rendezvous_rows(rig, &[], &[]);
    for row in &mut theirs {
        if row.0 == sr {
            row.1 = Some(hb);
        }
    }
    rig.set_directory(hb, theirs);
    rig.run(a);
    let id = rig.spawn_slime(a, past_seam(a, b, REGION_SIZE / 2.0));
    assert!(
        until(rig, CROSS_ROUNDS, |rig| rig.hosts[ha as usize].manager.unowned_waiting_ids() == [(sr, id)]),
        "the slime never waited for an owner"
    );
    for _ in 0..WAIT_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.hosts[ha as usize].manager.owner_of(b), game::Owner::Unknown, "nothing told host {ha} yet");
    rig.link_down(ha, hb);
    rig.round();
    rig.link_up(ha, hb);
    assert!(
        until(rig, CROSS_ROUNDS, |rig| rig.hosts[ha as usize].manager.owner_of(b) == game::Owner::Host(hb)),
        "host {ha} never learned host {hb}'s rows at its hello"
    );
    rig.run(b);
    until_settled_in(rig, id, b);
    assert!(nothing_unowned(rig), "nothing waits for an owner");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_late_peer_learns_every_claim_at_its_hello() {
    sweep("a_late_peer_learns_every_claim_at_its_hello", 2, 1, SEEDS / 2, late_peer_learns);
}

// --- The peer journal (R4 Task 7) --------------------------------------------------

/// Round budget for [`journal_explains`]: three committed world cuts and an aborted one.
const JOURNAL_ROUNDS: usize = 900;
/// Rounds a participant's link stays down under a pending world cut (it aborts the cut).
const JOURNAL_DOWN_ROUNDS: usize = 6;

/// `hosts` (2 or 3) regions in a chain, each adjacent to the next and every one owned by
/// another host (one region per super-region).
fn host_chain(hosts: u32) -> Vec<RegionCoords> {
    let (a, b) = host_seam(hosts, 1);
    if hosts == 2 {
        return vec![a, b];
    }
    let owner = |rc: RegionCoords| game::Static { ordinal: 0, hosts }.owner_of(rc, 1);
    for (x, y) in [(a, b), (b, a)] {
        for c in [(1, 0), (-1, 0), (0, 1), (0, -1)].map(|(dx, dz)| RegionCoords::new(y.x + dx, y.z + dz)) {
            if owner(c) != owner(x) && owner(c) != owner(y) {
                return vec![x, y, c];
            }
        }
    }
    panic!("no chain of {hosts} hosts at the seam {a:?} | {b:?}");
}

/// World cuts decided aborted so far.
fn aborted(rig: &MultiRig) -> usize {
    rig.decided.values().filter(|c| !**c).count()
}

/// Slimes and the test player shuttle along a chain of regions of every host (a slime
/// may pass through the middle host to the far one) while world cuts run, and a link
/// drops under some pending cuts so they abort. Every round, every host's peer journal
/// explains every difference between the running world and the last committed cut
/// ([`MultiRig::assert_journals_explain_the_cut`]).
fn journal_explains(rig: &mut MultiRig) {
    let chain = host_chain(rig.hosts.len() as u32);
    for rc in &chain {
        rig.run(*rc);
    }
    world_cuts(rig);
    let home = rig.owner(chain[0]);
    rig.connect_everywhere(home, P_CLIENT, P_PLAYER, stocked_record(chain[0], 7));
    until_body(rig);
    let y = crate::common::above_floor_y();
    let slimes: Vec<game::EntityId> = (0..4)
        .map(|i| rig.spawn_slime(chain[i % chain.len()], [REGION_SIZE / 2.0 + i as f32, y, REGION_SIZE / 3.0]))
        .collect();
    let (x, z) = (rig.owner(chain[0]), rig.owner(chain[1]));
    let goal = committed(rig) + 3;
    let mut down_until = None;
    let mut r = 0usize;
    while committed(rig) < goal || aborted(rig) == 0 || down_until.is_some() {
        assert!(r < JOURNAL_ROUNDS, "{} committed, {} aborted world cuts: {:?}", committed(rig), aborted(rig), rig.decided);
        let roll = crate::common::multi_rig::mix(rig.seed, r as u64);
        if r % 3 == 0 {
            let id = slimes[(roll % slimes.len() as u64) as usize];
            let i = ((roll >> 16) % (chain.len() as u64 - 1)) as usize;
            push_over(rig, id, chain[i], chain[i + 1]);
        }
        if r % 7 == 0 {
            if let Some((_, rc, id)) = rig.player_body(P_CLIENT) {
                if let Some(i) = chain.iter().position(|c| *c == rc) {
                    let j = if i == 0 || (i + 1 < chain.len() && (roll >> 32) % 2 == 0) { i + 1 } else { i - 1 };
                    rig.teleport(rc, id, past_seam(rc, chain[j], REGION_SIZE / 2.0));
                }
            }
        }
        match down_until {
            Some(until) if r >= until => {
                rig.link_up(x, z);
                down_until = None;
            }
            // About one cut in three aborts, at a seeded moment of its pending part.
            None if aborted(rig) * 2 < committed(rig)
                && (roll >> 40) % 4 == 0
                && matches!(rig.hosts[x as usize].manager.world_cut_state(), Some((_, "pending" | "marked"))) =>
            {
                rig.link_down(x, z);
                down_until = Some(r + JOURNAL_DOWN_ROUNDS);
            }
            _ => {}
        }
        rig.cut_round();
        let checked = rig.assert_journals_explain_the_cut();
        JOURNAL_CHECKS.fetch_add(checked, std::sync::atomic::Ordering::SeqCst);
        r += 1;
    }
}

/// Moves [`journal_explains`] checked against the journals (not exempted as passing
/// through a third host), over the sweep running now: only that test touches it.
static JOURNAL_CHECKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn journal_explains_every_difference_from_the_last_cut() {
    use std::sync::atomic::Ordering::SeqCst;
    for hosts in [2, 3] {
        JOURNAL_CHECKS.store(0, SeqCst);
        sweep(&format!("journal_explains_every_difference_from_the_last_cut ({hosts} hosts)"), hosts, 1, SEEDS / 4, journal_explains);
        assert!(JOURNAL_CHECKS.load(SeqCst) > 0, "{hosts} hosts: no move was ever checked against the journals");
    }
}

/// Rounds that push a random slime over the seam of `a` and `b` every third round.
fn shuttle_round(rig: &mut MultiRig, slimes: &[game::EntityId], a: RegionCoords, b: RegionCoords, r: usize) {
    if r % 3 == 0 {
        let roll = crate::common::multi_rig::mix(rig.seed, r as u64);
        push_over(rig, slimes[(roll % slimes.len() as u64) as usize], a, b);
    }
    rig.cut_round();
}

/// A host misses the coordinator's `CutDone` of a committed world cut and learns the
/// commit from the store's poll instead (`cut_committed` with no participants, as the
/// server's `settle_world` does): its journal truncates toward the cut's participants all
/// the same (the property, both bounds), and the `CutDone` delivered afterwards changes
/// nothing it holds. `false` if the cut aborted (the seed missed the case).
fn polled_commit(rig: &mut MultiRig) -> bool {
    let (a, b) = host_seam(2, 1);
    rig.run(a);
    rig.run(b);
    world_cuts(rig);
    let y = crate::common::above_floor_y();
    let slimes: Vec<game::EntityId> =
        (0..4).map(|i| rig.spawn_slime(if i % 2 == 0 { a } else { b }, [REGION_SIZE / 2.0 + i as f32, y, REGION_SIZE / 3.0])).collect();
    let h = rig.owner(b);
    let goal = committed(rig) + 1;
    let mut r = 0usize;
    while committed(rig) < goal {
        assert!(r < WORLD_CUT_ROUNDS, "no world cut committed: {:?}", rig.decided);
        shuttle_round(rig, &slimes, a, b, r);
        r += 1;
    }
    // The next cut: once host `h` takes part in it, the coordinator's messages to it are held.
    let cut = loop {
        assert!(r < 2 * WORLD_CUT_ROUNDS, "host {h} never took part in another cut");
        if let Some((c, "pending" | "marked")) = rig.hosts[h as usize].manager.world_cut_state() {
            if !rig.decided.contains_key(&c) {
                break c;
            }
        }
        shuttle_round(rig, &slimes, a, b, r);
        r += 1;
    };
    rig.hold_world(h, true);
    while !rig.decided.contains_key(&cut) {
        assert!(r < 3 * WORLD_CUT_ROUNDS, "cut {} never decided", cut.0);
        shuttle_round(rig, &slimes, a, b, r);
        r += 1;
    }
    if !rig.decided[&cut] {
        rig.hold_world(h, false);
        return false;
    }
    assert_eq!(rig.hosts[h as usize].manager.world_cut(), Some(cut), "host {h} has not heard the decision");
    assert!(rig.hosts[h as usize].manager.peer_journal().base() < cut);
    // The server's poll of the store finds the commit.
    rig.hosts[h as usize].manager.cut_committed(cut, Vec::new());
    assert_eq!(rig.hosts[h as usize].manager.peer_journal().base(), cut);
    rig.assert_journals_explain_the_cut();
    let other = 1 - h;
    let held: Vec<game::JournalEntry> = rig.hosts[h as usize].manager.peer_journal().entries(other).map(|(e, _)| e.clone()).collect();
    rig.hold_world(h, false);
    while rig.hosts[h as usize].manager.world_cut() == Some(cut) {
        assert!(r < 4 * WORLD_CUT_ROUNDS, "host {h} never heard the decision of cut {}", cut.0);
        rig.cut_round();
        r += 1;
    }
    let now: Vec<game::JournalEntry> = rig.hosts[h as usize].manager.peer_journal().entries(other).map(|(e, _)| e.clone()).collect();
    assert!(now.starts_with(&held), "the late CutDone changed what the journal held: {held:?} → {now:?}");
    rig.assert_journals_explain_the_cut();
    true
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn journal_truncates_on_a_commit_found_by_polling() {
    sweep_hits("journal_truncates_on_a_commit_found_by_polling", 2, 1, SEEDS / 4, polled_commit);
}

// --- Successions: the survivor's half (R4 Task 8) -------------------------------------

/// Round budget for a succession's orders to be answered.
const SUCCESSION_ROUNDS: usize = 200;

/// A chain of one region per host (`host_chain`), every one running, the hosts answering
/// ownership from a claim directory laid out as `static` would (the rig's `super_regions`
/// table behind it), world cuts running.
fn succession_world(rig: &mut MultiRig) -> Vec<RegionCoords> {
    let chain = host_chain(rig.hosts.len() as u32);
    let rows = rendezvous_rows(rig, &[], &[]);
    rig.with_directory(rows);
    rig.with_claims();
    for rc in &chain {
        rig.run(*rc);
    }
    world_cuts(rig);
    chain
}

/// The heir of `dead` among the rig's other live hosts, and a survivor that is not it.
fn heir_and_survivor(rig: &MultiRig, dead: u32) -> (u32, Option<u32>) {
    let live: std::collections::BTreeSet<u32> =
        (0..rig.hosts.len() as u32).filter(|h| *h != dead && !rig.dead.contains(h)).collect();
    let heir = game::heir_of(dead, &live).expect("a live host");
    (heir, live.into_iter().find(|h| *h != heir))
}

/// The coordinator's order of succession `s`.
fn order_of(s: &game::Succession) -> game::CoordMsg {
    game::CoordMsg::Succession {
        id: s.id,
        predecessor: s.predecessor,
        predecessor_boot: s.predecessor_boot,
        heir: s.heir,
        kind: s.kind,
        base_cut: s.base_cut,
        moved: s.moved.clone(),
        survivors: s.survivors.clone(),
    }
}

/// Cut rounds until every survivor of `s` answered `SuccessionApplied` at the coordinator
/// and the heir holds every survivor's report.
fn until_applied(rig: &mut MultiRig, s: &game::Succession) {
    for _ in 0..SUCCESSION_ROUNDS {
        let applied = rig.core().succession_progress().into_iter().find(|(id, _, _)| *id == s.id).map(|(_, a, _)| a);
        let reports: Vec<u32> =
            rig.hosts[s.heir as usize].manager.succession_reports(s.id).map(|r| r.keys().copied().collect()).unwrap_or_default();
        if applied.as_deref() == Some(&s.survivors[..]) && reports == s.survivors {
            return;
        }
        rig.cut_round();
    }
    panic!(
        "succession {} never answered: {:?} (seed {}, link delay {})",
        s.id.0,
        rig.core().succession_progress(),
        rig.seed,
        rig.link_delay
    );
}

/// The survivor's order arrives BEFORE the `CutDone` of the succession's base cut (its
/// world link held across that decision): it waits — no report leaves, its journal stays —
/// and once the `CutDone` comes the report leaves, against the base cut, with the journal
/// that cut truncated. Every survivor answers; the heir holds every report. `false` when
/// the seed missed the case (the cut aborted, or truncated nothing).
fn reports_against_the_base(rig: &mut MultiRig) -> bool {
    let chain = succession_world(rig);
    let dead = rig.owner(chain[1]);
    let (heir, s) = heir_and_survivor(rig, dead);
    let s = s.expect("three hosts");
    let y = crate::common::above_floor_y();
    let slimes: Vec<game::EntityId> =
        (0..4).map(|i| rig.spawn_slime(chain[i % 3], [REGION_SIZE / 2.0 + i as f32, y, REGION_SIZE / 3.0])).collect();
    let shuttle = |rig: &mut MultiRig, r: usize| {
        let roll = crate::common::multi_rig::mix(rig.seed, r as u64);
        if r % 3 == 0 {
            let i = ((roll >> 16) % 2) as usize;
            push_over(rig, slimes[(roll % slimes.len() as u64) as usize], chain[i], chain[i + 1]);
        }
        rig.cut_round();
    };
    let mut r = 0usize;
    while committed(rig) < 1 {
        assert!(r < WORLD_CUT_ROUNDS, "no world cut committed: {:?}", rig.decided);
        shuttle(rig, r);
        r += 1;
    }
    // The next cut host `s` takes part in: its decision is held from `s`.
    let cut = loop {
        assert!(r < 2 * WORLD_CUT_ROUNDS, "host {s} never took part in another cut");
        if let Some((c, "pending" | "marked")) = rig.hosts[s as usize].manager.world_cut_state() {
            if !rig.decided.contains_key(&c) {
                break c;
            }
        }
        shuttle(rig, r);
        r += 1;
    };
    rig.hold_world(s, true);
    while !rig.decided.contains_key(&cut) {
        assert!(r < 3 * WORLD_CUT_ROUNDS, "cut {} never decided", cut.0);
        shuttle(rig, r);
        r += 1;
    }
    if !rig.decided[&cut] {
        rig.hold_world(s, false);
        return false;
    }
    let journal = rig.hosts[s as usize].manager.peer_journal().clone();
    assert!(journal.base() < cut, "host {s} has not heard the commit");
    let before: Vec<game::JournalEntry> = journal.entries(dead).map(|(e, _)| e.clone()).collect();
    let truncated = {
        let mut j = journal.clone();
        j.committed(cut, &std::collections::BTreeSet::new());
        j.take(dead)
    };
    rig.kill(dead);
    let succession = rig.run_succession(dead).remove(0);
    assert_eq!((succession.base_cut, succession.heir), (cut, heir), "declared from the cut, to the heir");
    // The order reaches `s` before the cut's decision.
    rig.hosts[s as usize].manager.apply_coordinator_message(order_of(&succession));
    for _ in 0..WAIT_ROUNDS {
        let m = &rig.hosts[s as usize].manager;
        assert_eq!(m.successions(), vec![(succession.id, false)], "host {s} waits for its base cut");
        assert!(
            !m.peer_owed(heir).iter().any(|msg| matches!(msg, game::Reliable::SuccessionReport { .. })),
            "no report against another base"
        );
        rig.cut_round();
    }
    rig.hold_world(s, false);
    until_applied(rig, &succession);
    let report = &rig.hosts[heir as usize].manager.succession_reports(succession.id).expect("reports")[&s];
    assert_eq!(report.base_cut, cut);
    assert!(
        report.entries.starts_with(&truncated),
        "host {s}'s report holds its journal as cut {} truncated it: {} entries, {} expected first (seed {})",
        cut.0,
        report.entries.len(),
        truncated.len(),
        rig.seed
    );
    assert!(rig.hosts[s as usize].manager.peer_journal().entries(dead).next().is_none(), "taken");
    // The heir filed its own; once it adopts (driven here: Task 9's), the next cut settles it.
    let now = rig.hosts.iter().map(|h| h.now_ms).max().unwrap_or(0);
    rig.core().handle(heir, game::HostMsg::SuccessionAdopted { id: succession.id }, now);
    for _ in 0..WORLD_CUT_ROUNDS {
        if rig.successions.iter().all(|(_, settled)| settled.is_some()) && rig.hosts[s as usize].manager.successions().is_empty() {
            break;
        }
        rig.cut_round();
    }
    assert!(rig.successions.iter().all(|(_, settled)| settled.is_some()), "the next commit settles it: {:?}", rig.decided);
    for h in [s, heir] {
        assert!(rig.hosts[h as usize].manager.successions().is_empty(), "host {h} heard the settle");
    }
    truncated.len() < before.len()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn survivor_reports_only_against_the_declared_base_cut() {
    sweep_hits("survivor_reports_only_against_the_declared_base_cut", 3, 1, SEEDS / 4, reports_against_the_base);
}

/// What a survivor owed the dead host: intents and replies go to the heir in its report
/// (`owed`, in order, unwrapped) — their items held exactly once all along — a transfer is
/// a `Sent` journal entry (never owed), a spawn order is refused to its origin, and every
/// other order is dropped: nothing stays owed to the dead host.
fn owed_to_the_heir(rig: &mut MultiRig) {
    crate::common::install_real_block_props();
    let chain = succession_world(rig);
    let dead = rig.owner(chain[1]);
    let (heir, s) = heir_and_survivor(rig, dead);
    let s = s.expect("three hosts");
    let (rs, rd) = (chain.iter().copied().find(|rc| rig.owner(*rc) == s).expect("s's region"), chain[1]);
    rig.link_down(s, dead);
    let due = world_tick(rig) + 5;
    let (first, second) = (refusal_of(rs, rd, due, OWED[1]), refusal_of(rs, rd, due + 1, OWED[2]));
    let player = FRESH_PLAYER;
    let token = || LOGIN_TOKEN.to_string();
    let orders = [
        game::Reliable::Intent(first),
        game::Reliable::SpawnOrder { origin: s, hops: 1, player_token: token(), session_epoch: 3, player, record_region: Some(rd) },
        game::Reliable::ObserverOrder { client: P_CLIENT, player, nonce: game::Nonce(5), ttl_secs: 30 },
        game::Reliable::Kick { player_token: token(), session_epoch: 2, reason: "test".into() },
        game::Reliable::KickAck { player_token: token(), session_epoch: 2 },
        game::Reliable::SessionEnded { player_token: token(), session_epoch: 1 },
        game::Reliable::PlayerParked { client: P_CLIENT, player },
        game::Reliable::Owns { rows: Vec::new() },
        game::Reliable::Intent(second),
    ];
    for msg in orders {
        rig.hosts[s as usize].manager.send_reliable_for_test(dead, msg, false);
    }
    // A slime crossing into the dead host's region while the link is down: owed too.
    let y = crate::common::above_floor_y();
    let slime = rig.spawn_slime(rs, [REGION_SIZE / 2.0, y, REGION_SIZE / 2.0]);
    rig.round();
    assert!(push_over(rig, slime, rs, rd), "the slime is in {rs:?}");
    for _ in 0..CROSS_ROUNDS {
        if rig.hosts[s as usize].manager.peer_unreceived_ids().iter().any(|(p, id, _)| *p == dead && *id == slime) {
            break;
        }
        rig.round();
    }
    assert!(rig.hosts[s as usize].manager.peer_unreceived_ids().iter().any(|(p, id, _)| *p == dead && *id == slime));
    rig.items = Some(u64::from(OWED[1] + OWED[2]));
    rig.assert_conserved_world();
    rig.hosts[s as usize].manager.take_login_out();
    rig.kill(dead);
    let succession = rig.run_succession(dead).remove(0);
    until_applied(rig, &succession);
    let report = &rig.hosts[heir as usize].manager.succession_reports(succession.id).expect("reports")[&s];
    assert_eq!(report.owed, vec![game::Reliable::Intent(first), game::Reliable::Intent(second)], "the intents, in order");
    assert!(
        report.entries.iter().any(|e| e.subject == game::Subject::Entity(slime) && matches!(e.event, game::JournalEvent::Sent(_))),
        "the transfer is a Sent entry of the journal"
    );
    assert!(rig.hosts[s as usize].manager.peer_owed(dead).is_empty(), "nothing is owed to the dead host");
    let out = rig.hosts[s as usize].manager.take_login_out();
    assert!(
        out.iter().any(|o| matches!(o, game::LoginOut::SpawnAck { ok: false, session_epoch: 3, .. })),
        "the spawn order is refused to its origin: {out:?}"
    );
    // Items stay held exactly once (checked every round) while the report sits on the heir.
    for _ in 0..WAIT_ROUNDS {
        rig.cut_round();
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn owed_state_messages_go_to_the_heir_and_orders_are_dropped() {
    sweep("owed_state_messages_go_to_the_heir_and_orders_are_dropped", 3, 1, SEEDS / 4, owed_to_the_heir);
}

/// Rounds until host `from`'s `departed` points `player` at `to`.
fn until_handed_to(rig: &mut MultiRig, from: u32, player: game::PlayerId, to: u32) {
    for _ in 0..CROSS_ROUNDS {
        if rig.hosts[from as usize].manager.departed().get(&player) == Some(&to) {
            return;
        }
        rig.round();
    }
    panic!("host {from} never handed player {} to host {to}", player.0);
}

/// Two players walk from a survivor's region into the dead host's — one before a committed
/// cut, one after the last one — then the host dies: the survivor's `departed` pointer of a
/// player its report carries (a `Sent` entry) points at the heir, the other one's is gone
/// (the store's record places it). `false` when the seed's cuts left no case of one kind.
fn departed_pointers(rig: &mut MultiRig) -> bool {
    let chain = succession_world(rig);
    let dead = rig.owner(chain[1]);
    let (heir, s) = heir_and_survivor(rig, dead);
    let s = s.expect("three hosts");
    let (rs, rd) = (chain.iter().copied().find(|rc| rig.owner(*rc) == s).expect("s's region"), chain[1]);
    rig.connect_everywhere(s, P_CLIENT, P_PLAYER, centre_of(rs));
    let (_, _, body) = until_body(rig);
    assert!(rig.teleport(rs, body, past_seam(rs, rd, REGION_SIZE / 2.0)));
    until_handed_to(rig, s, P_PLAYER, dead);
    until_committed(rig, 1);
    rig.connect_everywhere(s, P2_CLIENT, P2_PLAYER, record_at(rs, [REGION_SIZE / 2.0, REGION_SIZE / 3.0]));
    let mut second = None;
    for _ in 0..BODY_ROUNDS {
        second = rig.player_body(P2_CLIENT);
        if second.is_some() {
            break;
        }
        rig.round();
    }
    let (_, _, body2) = second.expect("the second player has a body");
    assert!(rig.teleport(rs, body2, past_seam(rs, rd, REGION_SIZE / 3.0)));
    until_handed_to(rig, s, P2_PLAYER, dead);
    // Which of them the survivor's journal still carries (a cut may have committed meanwhile).
    let carried: std::collections::BTreeSet<game::PlayerId> = rig.hosts[s as usize]
        .manager
        .peer_journal()
        .entries(dead)
        .filter(|(e, _)| matches!(e.event, game::JournalEvent::Sent(_)))
        .filter_map(|(e, _)| match e.subject {
            game::Subject::Player(p) => Some(p),
            game::Subject::Entity(_) => None,
        })
        .collect();
    rig.kill(dead);
    let succession = rig.run_succession(dead).remove(0);
    until_applied(rig, &succession);
    let departed = rig.hosts[s as usize].manager.departed().clone();
    for p in [P_PLAYER, P2_PLAYER] {
        if carried.contains(&p) {
            assert_eq!(departed.get(&p), Some(&heir), "player {} (carried) points at the heir", p.0);
        } else {
            assert_eq!(departed.get(&p), None, "player {} (not carried) is forgotten", p.0);
        }
    }
    assert!(!departed.values().any(|h| *h == dead), "nothing points at the dead host: {departed:?}");
    carried.contains(&P2_PLAYER) && !carried.contains(&P_PLAYER)
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn departed_pointers_follow_the_report_or_are_dropped() {
    sweep_hits("departed_pointers_follow_the_report_or_are_dropped", 3, 1, SEEDS / 4, departed_pointers);
}

/// A super-region nobody claims in the rig's layout.
const FAR_SR: game::SuperRegion = (90, 90);

/// After the succession a process of the dead boot comes back (a zombie): every survivor
/// refuses its hello — it is never reachable and gets nothing, not even what waits for its
/// ordinal. Then a NEW boot of that ordinal starts: it is welcome — its session numbering
/// starts fresh and what waited for the ordinal is delivered to it.
fn zombie_refused(rig: &mut MultiRig) {
    let chain = succession_world(rig);
    let dead = rig.owner(chain[1]);
    let (heir, _) = heir_and_survivor(rig, dead);
    let old_boot = rig.boots[dead as usize];
    rig.kill(dead);
    let succession = rig.run_succession(dead).remove(0);
    until_applied(rig, &succession);
    // The heir tells the dead ordinal of a claim: it waits for that ordinal's next boot.
    rig.hosts[heir as usize].manager.send_reliable_for_test(dead, game::Reliable::Owns { rows: vec![(FAR_SR, 9)] }, false);
    let far = RegionCoords::new(FAR_SR.0, FAR_SR.1);
    rig.start_as(dead, old_boot);
    for _ in 0..WAIT_ROUNDS {
        rig.cut_round();
        assert!(!rig.hosts[heir as usize].manager.peer_is_reachable(dead), "the dead boot is never reachable");
        assert_eq!(rig.hosts[dead as usize].manager.owner_of(far), game::Owner::Free, "the zombie learns nothing");
    }
    rig.kill(dead);
    let boot = rig.start(dead);
    assert_ne!(boot, old_boot);
    for _ in 0..CROSS_ROUNDS {
        let m = &rig.hosts[heir as usize].manager;
        if m.peer_is_reachable(dead) && m.peers_synced() && rig.hosts[dead as usize].manager.owner_of(far) == game::Owner::Host(heir) {
            break;
        }
        rig.cut_round();
    }
    assert!(rig.hosts[heir as usize].manager.peer_is_reachable(dead), "the new boot is welcome");
    assert_eq!(rig.hosts[dead as usize].manager.owner_of(far), game::Owner::Host(heir), "and gets what waited for its ordinal");
    assert!(rig.hosts[heir as usize].manager.peers_synced(), "the new boot synced: a store placement waits for nobody");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_zombie_hello_of_the_dead_boot_is_refused_and_a_new_boot_is_welcome() {
    sweep("a_zombie_hello_of_the_dead_boot_is_refused_and_a_new_boot_is_welcome", 2, 1, SEEDS / 4, zombie_refused);
}
