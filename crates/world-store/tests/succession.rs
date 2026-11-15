//! Successions (R4): the coordinator declares a lapsed host dead in one fenced transaction
//! that moves all its super-regions to an heir; the next committed cut settles it; a world
//! rollback abandons an open one; the heir's part adopts the dead host's in-flight rows.
//! Postgres unless noted (several hosts need it); SQLite declares nothing.

use crate::common::hid;
use std::time::{Duration, Instant};

use game::{
    ChunkCoords, CrashPolicy, CutId, EntityBundle, EntityId, InFlightRow, PortableRegion, RegionCoords, WorldCut,
};
use world_store::{
    CoordinatorFence, Declared, Fence, HOST_DEAD, HOST_LIVE, HostRow, StoreError, StoreHandle, StoreUrl, Succession,
    SuccessionKind,
};

use crate::codec::{bundle, chunk, intent, meta, region_ground};
use crate::handle::{each_backend, open};
use crate::two_phase::{PgWorld, exec, pg_world, prepared, scalar_i64};

/// Host 0's region (super-region (0, 0)).
const A: RegionCoords = RegionCoords { x: 0, z: 0 };
/// Host 1's region (super-region (0, -1)).
const B: RegionCoords = RegionCoords { x: 1, z: -1 };
/// Host 2's region (super-region (1, 0) at the default side 16).
const C: RegionCoords = RegionCoords { x: 16, z: 0 };
/// A second super-region of host 1, with no region saved in it.
const FAR: (i32, i32) = (5, 5);

pub(crate) fn boot_of(ordinal: game::HostId) -> u64 {
    100 + ordinal.0
}

pub(crate) fn row(ordinal: game::HostId) -> HostRow {
    HostRow {
        id: ordinal,
        boot: boot_of(ordinal),
        generation: 0,
        clean: false,
        state: 0,
        seen_ms: 0,
        reg: 0,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    }
}

fn region_of_host(host: game::HostId) -> RegionCoords {
    match host.0 {
        0 => A,
        1 => B,
        2 => C,
        // Super-region (2, 0) at the default side 16.
        _ => RegionCoords { x: 32, z: 0 },
    }
}

fn region() -> PortableRegion {
    PortableRegion {
        meta: meta(),
        chunks: [(ChunkCoords::new(0, 0, 0), chunk())].into_iter().collect(),
        ground: Some(region_ground()),
        ..Default::default()
    }
}

/// Host `host`'s part of cut `id`: its region and `in_flight`.
pub(crate) fn part(id: u64, host: game::HostId, in_flight: Vec<InFlightRow>) -> WorldCut {
    WorldCut {
        id: CutId(id),
        host,
        tick: 100 * id,
        regions: vec![(region_of_host(host), region())],
        in_flight,
        players: vec![],
        history: None,
        adopts: vec![],
    }
}

/// An intent row aimed at `rc` that tells itself apart by its target's `z`.
pub(crate) fn in_flight(rc: RegionCoords) -> InFlightRow {
    InFlightRow::Intent { target: rc, msg: intent() }
}

/// A world of `n` hosts (0..n), each registered with its own fence (host 1 also holds
/// [`FAR`]), and a claimed coordinator. Each host gets its own store handle.
pub(crate) struct World {
    pub(crate) w: PgWorld,
    /// The coordinator's handle.
    pub(crate) h: StoreHandle,
    pub(crate) hosts: Vec<StoreHandle>,
    pub(crate) fences: Vec<Fence>,
    pub(crate) c: CoordinatorFence,
}

impl World {
    pub(crate) fn new(n: u32) -> Option<World> {
        let w = pg_world()?;
        let h = open(&w.url);
        let s = h.super_region_regions();
        let mut hosts = Vec::new();
        let mut fences = Vec::new();
        for o in (0..u64::from(n)).map(hid) {
            let mut srs = vec![game::super_region_of(region_of_host(o), s)];
            if o == hid(1) {
                srs.push(FAR);
            }
            let hh = open(&w.url);
            let f = hh.claim_super_regions(o, srs).unwrap();
            match hh.register_host(row(o), &f, CrashPolicy::Succession).unwrap() {
                world_store::BootVerdict::Registered(_) => {}
                other => panic!("host {o} registers: {other:?}"),
            }
            hosts.push(hh);
            fences.push(f);
        }
        let c = h.claim_coordinator().unwrap();
        Some(World { w, h, hosts, fences, c })
    }

    /// Host `o`'s row looks lapsed: its last beat lies two leases back.
    pub(crate) fn lapse(&self, o: game::HostId) {
        exec(&self.w, &format!("UPDATE hosts SET seen_ms = seen_ms - {} WHERE ordinal = {}", 2 * game::HOST_LEASE_MS, o.0));
    }

    /// Host `o`'s row is vouched dead (its next boot found it dirty while others ran).
    pub(crate) fn vouch(&self, o: game::HostId) {
        exec(&self.w, &format!("UPDATE hosts SET seen_ms = 0 WHERE ordinal = {}", o.0));
    }

    pub(crate) fn declare(&self, dead: game::HostId, heir: game::HostId, kind: SuccessionKind) -> Result<Declared, StoreError> {
        self.h.declare_succession(vec![(dead, boot_of(dead))], vec![heir], kind, self.c, 0)
    }

    /// Declares `dead` crashed with `heir` and returns the one succession.
    pub(crate) fn declared(&self, dead: game::HostId, heir: game::HostId) -> Succession {
        match self.declare(dead, heir, SuccessionKind::Crash).unwrap() {
            Declared::Done(mut s) => {
                assert_eq!(s.len(), 1);
                s.remove(0)
            }
            other => panic!("declared: {other:?}"),
        }
    }

    pub(crate) fn state_of(&self, o: game::HostId) -> (i64, bool) {
        let r = self.h.hosts().unwrap().into_iter().find(|r| r.id == o).unwrap();
        (r.state, r.clean)
    }

    pub(crate) fn shutdown(self) {
        for hh in self.hosts {
            hh.shutdown();
        }
        self.h.shutdown();
    }
}

#[test]
fn declare_moves_every_super_region_and_fences_the_dead_host() {
    let Some(w) = World::new(2) else { return };
    w.lapse(hid(1));
    let f1 = w.fences[1].clone();
    let s = w.declared(hid(1), hid(0));
    assert_eq!((s.predecessor, s.predecessor_boot, s.heir, s.kind), (hid(1), boot_of(hid(1)), hid(0), SuccessionKind::Crash));
    assert_eq!(s.base_cut, CutId(0), "no cut committed yet");
    assert_eq!(s.survivors, vec![hid(0)]);
    let mut want: Vec<_> = f1.epochs.iter().map(|(sr, e)| (*sr, e + 1)).collect();
    want.sort();
    assert_eq!(s.moved, want, "every super-region of host 1, epoch + 1");
    // The heir holds them at the new epochs, beside its own.
    let heir = w.h.owned_by(hid(0)).unwrap();
    for (sr, e) in &s.moved {
        assert_eq!(heir.epochs.get(sr), Some(e));
    }
    for (sr, e) in &w.fences[0].epochs {
        assert_eq!(heir.epochs.get(sr), Some(e), "the heir's own rows are untouched");
    }
    assert!(w.h.owned_by(hid(1)).unwrap().epochs.is_empty());
    // The dead host's fence is refused everywhere.
    let dead = &w.hosts[1];
    fn fenced<T>(r: Result<T, StoreError>) -> bool {
        matches!(r, Err(StoreError::Fenced { sr: Some(_) }))
    }
    assert!(fenced(dead.commit_cut(part(1, hid(1), vec![]), f1.clone()).recv().unwrap()), "commit");
    assert!(fenced(dead.prepare_cut(part(2, hid(1), vec![]), f1.clone(), 0).recv().unwrap()), "prepare");
    assert!(fenced(dead.load_region(B, &f1)), "load");
    assert_eq!(prepared(&w.w), 0);
    // Its lease is gone; its row is dead and still dirty.
    assert!(!dead.heartbeat(hid(1), boot_of(hid(1))).unwrap());
    assert_eq!(w.state_of(hid(1)), (HOST_DEAD, false));
    assert_eq!(w.h.open_successions().unwrap(), vec![s]);
    w.shutdown();
}

#[test]
fn declare_refuses_a_row_that_heartbeated() {
    let Some(w) = World::new(2) else { return };
    // Registered just now: not lapsed.
    assert_eq!(w.declare(hid(1), hid(0), SuccessionKind::Crash).unwrap(), Declared::NotLapsed(vec![hid(1)]));
    // Lapsed, then it beat again before the declaration.
    w.lapse(hid(1));
    assert!(w.hosts[1].heartbeat(hid(1), boot_of(hid(1))).unwrap());
    assert_eq!(w.declare(hid(1), hid(0), SuccessionKind::Crash).unwrap(), Declared::NotLapsed(vec![hid(1)]));
    // Another boot of the ordinal is not the one the caller judged dead.
    w.lapse(hid(1));
    let other = w.h.declare_succession(vec![(hid(1), 999)], vec![hid(0)], SuccessionKind::Crash, w.c, 0).unwrap();
    assert_eq!(other, Declared::Gone(vec![hid(1)]));
    // Nothing changed.
    assert_eq!(w.h.owned_by(hid(1)).unwrap(), w.fences[1]);
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, false));
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();
}

#[test]
fn a_vouched_row_needs_no_lapse() {
    let Some(w) = World::new(2) else { return };
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    assert_eq!(s.heir, hid(0));
    assert_eq!(w.state_of(hid(1)), (HOST_DEAD, false));
    w.shutdown();
}

#[test]
fn second_declare_while_one_is_open_says_so() {
    let Some(w) = World::new(3) else { return };
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    assert_eq!(s.survivors, vec![hid(0), hid(2)], "every live dirty host but the dead one");
    w.vouch(hid(2));
    let f2 = w.h.owned_by(hid(2)).unwrap();
    assert_eq!(w.declare(hid(2), hid(0), SuccessionKind::Crash).unwrap(), Declared::SuccessionOpen);
    assert_eq!(w.h.owned_by(hid(2)).unwrap(), f2, "nothing moved");
    assert_eq!(w.state_of(hid(2)), (HOST_LIVE, false));
    assert_eq!(w.h.open_successions().unwrap(), vec![s]);
    w.shutdown();
}

#[test]
fn stale_coordinator_cannot_declare() {
    let Some(w) = World::new(2) else { return };
    w.vouch(hid(1));
    let newer = w.h.claim_coordinator().unwrap();
    let err = w.declare(hid(1), hid(0), SuccessionKind::Crash).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None });
    let err = w.h.declare_succession(vec![(hid(1), boot_of(hid(1)))], vec![hid(0)], SuccessionKind::Crash, newer, 7).unwrap_err();
    assert_eq!(err, StoreError::StaleGeneration { stored: 0, mine: 7 });
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, false));
    assert_eq!(w.h.owned_by(hid(1)).unwrap(), w.fences[1]);
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn declare_waits_out_a_staged_part_with_a_timeout() {
    let Some(w) = World::new(2) else { return };
    const WAIT_MS: u64 = 400;
    w.h.set_cut_lock_timeout_ms(WAIT_MS);
    // Host 1's part of cut 1 is staged (it holds its super-regions FOR SHARE), then it dies.
    w.hosts[1].prepare_cut(part(1, hid(1), vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    w.vouch(hid(1));
    let started = Instant::now();
    let err = w.declare(hid(1), hid(0), SuccessionKind::Crash).unwrap_err();
    let waited = started.elapsed();
    assert!(matches!(&err, StoreError::LockConflict { sqlstate, detail } if sqlstate == "55P03" && detail.contains("lock_timeout")), "{err}");
    assert!(err.is_lock_conflict());
    assert!(waited >= Duration::from_millis(WAIT_MS) && waited < Duration::from_millis(WAIT_MS * 10), "waited {waited:?}");
    // Nothing changed.
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, false));
    assert_eq!(w.h.owned_by(hid(1)).unwrap(), w.fences[1]);
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(prepared(&w.w), 1);
    // The cut is aborted: the declaration goes through.
    assert!(!w.h.abort_cut(CutId(1), 100).unwrap());
    assert_eq!(prepared(&w.w), 0);
    let s = w.declared(hid(1), hid(0));
    assert_eq!(s.moved.len(), 2);
    w.shutdown();
}

/// Final review A-I1: a host FROZEN inside an open fenced transaction (stopped, its VM
/// paused, its machine gone without a FIN — in the middle of a cut part or a region load)
/// holds its super-regions `FOR SHARE` for as long as its session lives, and its own
/// succession's declaration needs those rows. The fence check bounds the transaction's idle
/// time (`FENCED_TX_IDLE_MS`, the lease): the database ends the session, the locks go, and
/// the declaration — retried by the coordinator — goes through.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_frozen_in_a_fenced_transaction_does_not_block_its_declaration_for_ever() {
    let Some(w) = World::new(2) else { return };
    const IDLE_MS: u64 = 600;
    const WAIT_MS: u64 = 200;
    w.h.set_cut_lock_timeout_ms(WAIT_MS);
    w.hosts[1].set_fenced_tx_idle_ms(IDLE_MS).unwrap();
    // Host 1 freezes holding its rows (and would for 5 s: far longer than the idle limit).
    w.hosts[1].freeze_in_fenced_tx(w.fences[1].clone(), 5_000).unwrap();
    w.vouch(hid(1));
    let err = w.declare(hid(1), hid(0), SuccessionKind::Crash).unwrap_err();
    assert!(err.is_lock_conflict(), "the frozen transaction holds the rows: {err}");
    // Past the idle limit the session is gone; the retried declaration succeeds.
    let deadline = Instant::now() + Duration::from_millis(IDLE_MS * 5);
    let s = loop {
        match w.declare(hid(1), hid(0), SuccessionKind::Crash) {
            Ok(Declared::Done(mut s)) => break s.remove(0),
            Ok(other) => panic!("declared: {other:?}"),
            Err(e) if e.is_lock_conflict() => {
                assert!(Instant::now() < deadline, "the frozen host's locks never went: {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("{e}"),
        }
    };
    assert_eq!(s.moved.len(), 2);
    assert_eq!(w.state_of(hid(1)), (HOST_DEAD, false));
    w.shutdown();
}

/// Final review A-M1: `claim_free` never locks a row it cannot win. A row another host holds
/// — here `FOR SHARE` in that host's staged cut part — is left alone at once (an upsert's
/// `ON CONFLICT DO UPDATE … WHERE` locked it before its `WHERE`, and the claim waited for that
/// cut's decision while holding this store's writer lock, which its own part needs).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn claim_free_does_not_wait_for_a_row_another_host_holds() {
    let Some(w) = World::new(2) else { return };
    const WAIT_MS: u64 = 500;
    w.hosts[0].set_claim_lock_timeout_ms(WAIT_MS);
    w.hosts[1].prepare_cut(part(1, hid(1), vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    let theirs: Vec<world_store::SuperRegion> = w.fences[1].epochs.keys().copied().collect();
    let started = Instant::now();
    let won = w.hosts[0].claim_free(hid(0), boot_of(hid(0)), theirs).expect("a claim that wins nothing is no error");
    let waited = started.elapsed();
    assert!(won.epochs.is_empty(), "host 1 holds them: {won:?}");
    assert!(waited < Duration::from_millis(WAIT_MS / 2), "the claim waited {waited:?} behind a row it could not win");
    assert_eq!(w.h.owned_by(hid(1)).unwrap(), w.fences[1], "host 1's rows are untouched");
    assert!(!w.h.abort_cut(CutId(1), 100).unwrap());
    w.shutdown();
}

/// Final review A-I4: the store cannot refuse a settle whose heir part did not adopt the
/// predecessor's in-flight rows (the pairing is the coordinator's), but it DETECTS it: the
/// rows that outlive the settle under the predecessor's ordinal are reported (an error line
/// and a counter). A settle whose heir part adopted them reports nothing.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_settle_whose_heir_part_did_not_adopt_is_reported() {
    for adopting in [false, true] {
        let Some(w) = World::new(2) else { return };
        w.hosts[0].prepare_cut(part(1, hid(0), vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
        w.hosts[1].prepare_cut(part(1, hid(1), vec![in_flight(A)]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
        assert!(w.h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1)], vec![], w.c, 0, vec![]).unwrap());
        assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
        w.vouch(hid(1));
        let s = w.declared(hid(1), hid(0));
        let heir = w.h.owned_by(hid(0)).unwrap();
        let adopts = if adopting { vec![hid(1)] } else { vec![] };
        w.hosts[0].prepare_cut(WorldCut { adopts, ..part(2, hid(0), vec![]) }, heir, 0).recv().unwrap().unwrap();
        assert!(w.h.decide_cut(CutId(2), 200, true, vec![hid(0)], vec![], w.c, 0, vec![s.id]).unwrap());
        // The post-commit checks run after the decision is answered.
        let deadline = Instant::now() + Duration::from_secs(10);
        while w.h.cuts_checked_for_entities_twice() < 2 {
            assert!(Instant::now() < deadline, "the post-commit checks of cut 2 ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(w.h.cuts_settled_unadopted(), u64::from(!adopting), "adopting: {adopting}");
        w.shutdown();
    }
}

#[test]
fn commit_without_naming_the_open_succession_becomes_an_abort() {
    let Some(w) = World::new(2) else { return };
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    let heir = w.h.owned_by(hid(0)).unwrap();
    w.hosts[0].prepare_cut(part(1, hid(0), vec![]), heir.clone(), 0).recv().unwrap().unwrap();
    assert!(!w.h.decide_cut(CutId(1), 100, true, vec![hid(0)], vec![], w.c, 0, vec![]).unwrap(), "settles nothing");
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(false));
    w.hosts[0].prepare_cut(part(2, hid(0), vec![]), heir, 0).recv().unwrap().unwrap();
    let wrong = world_store::SuccessionId(s.id.0 + 1);
    assert!(!w.h.decide_cut(CutId(2), 200, true, vec![hid(0)], vec![], w.c, 0, vec![s.id, wrong]).unwrap(), "settles one too many");
    assert_eq!(prepared(&w.w), 0);
    assert_eq!(w.h.open_successions().unwrap(), vec![s]);
    assert_eq!(w.state_of(hid(1)), (HOST_DEAD, false));
    w.shutdown();
}

#[test]
fn commit_settles_and_cleans_the_predecessor() {
    let Some(w) = World::new(2) else { return };
    // A committed cut first: the succession's base.
    w.hosts[0].prepare_cut(part(1, hid(0), vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, hid(1), vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1)], vec![], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    assert_eq!(s.base_cut, CutId(1));
    let heir = w.h.owned_by(hid(0)).unwrap();
    w.hosts[0].prepare_cut(part(2, hid(0), vec![]), heir, 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(2), 200, true, vec![hid(0)], vec![], w.c, 0, vec![s.id]).unwrap());
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(scalar_i64(&w.w, &format!("SELECT settled_cut FROM successions WHERE id = {}", s.id.0)), 2);
    assert_eq!(w.state_of(hid(1)), (HOST_DEAD, true), "the predecessor's row is clean");
    // A new boot of the ordinal registers at once.
    let back = w.hosts[1].register_host(HostRow { boot: 555, ..row(hid(1)) }, &Fence::default(), CrashPolicy::Succession);
    assert!(matches!(back, Ok(world_store::BootVerdict::Registered(_))), "{back:?}");
    w.shutdown();
}

#[test]
fn rollback_abandons_open_ones_and_keeps_the_land_with_the_heir() {
    let Some(w) = World::new(2) else { return };
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    let stale = CoordinatorFence { epoch: w.c.epoch - 1 };
    assert_eq!(w.h.rollback_world(stale).unwrap_err(), StoreError::Fenced { sr: None });
    assert_eq!(w.h.world_generation().unwrap(), 0);
    assert_eq!(w.h.rollback_world(w.c).unwrap(), 1);
    assert_eq!(w.h.world_generation().unwrap(), 1);
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(scalar_i64(&w.w, &format!("SELECT abandoned FROM successions WHERE id = {}", s.id.0)), 1);
    let heir = w.h.owned_by(hid(0)).unwrap();
    for (sr, e) in &s.moved {
        assert_eq!(heir.epochs.get(sr), Some(e), "the land stays with the heir");
    }
    w.shutdown();
}

#[test]
fn heir_adopts_the_dead_hosts_in_flight_rows_once() {
    let Some(w) = World::new(2) else { return };
    let own = in_flight(RegionCoords::new(0, 7));
    let dead_rows: Vec<InFlightRow> = (1..=3).map(|z| in_flight(RegionCoords::new(1, z))).collect();
    w.hosts[0].prepare_cut(part(1, hid(0), vec![own.clone()]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, hid(1), dead_rows.clone()), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1)], vec![], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    let heir = w.h.owned_by(hid(0)).unwrap();
    assert_eq!(w.hosts[0].load_in_flight_of(hid(1), &heir).unwrap(), dead_rows, "the heir reads the dead host's rows");
    // The heir's part keeps its own row and two of the dead host's.
    let kept = vec![own.clone(), dead_rows[1].clone(), dead_rows[2].clone()];
    let adopting = WorldCut { adopts: vec![hid(1)], ..part(2, hid(0), kept.clone()) };
    w.hosts[0].prepare_cut(adopting, heir.clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(2), 200, true, vec![hid(0)], vec![], w.c, 0, vec![s.id]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(2)).unwrap(), Some(true));
    assert_eq!(w.h.load_in_flight(Some(hid(0))).unwrap(), kept);
    assert!(w.h.load_in_flight(Some(hid(1))).unwrap().is_empty(), "host 1 has no rows left");
    // Settled: adopting again is refused.
    let again = WorldCut { adopts: vec![hid(1)], ..part(3, hid(0), kept) };
    let err = w.hosts[0].prepare_cut(again, heir.clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
    assert!(matches!(w.hosts[0].load_in_flight_of(hid(1), &heir), Err(StoreError::Unsupported(_))));
    assert_eq!(prepared(&w.w), 0);
    w.shutdown();
}

#[test]
fn adopts_without_a_succession_is_refused() {
    // Any backend: the one-phase commit and the read check the same way.
    each_backend("adopts_refused", false, |url| {
        let h = open(url);
        let f = crate::handle::fence(&h);
        let cut = WorldCut { adopts: vec![hid(1)], ..part(1, hid(0), vec![in_flight(A)]) };
        let err = h.commit_cut(cut, f.clone()).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
        assert!(h.load_in_flight(None).unwrap().is_empty(), "nothing written");
        assert!(matches!(h.load_in_flight_of(hid(1), &f), Err(StoreError::Unsupported(_))));
        h.shutdown();
    });
    // Postgres: a succession whose heir is another host does not let host 2 adopt.
    let Some(w) = World::new(3) else { return };
    w.vouch(hid(1));
    w.declared(hid(1), hid(0));
    let cut = WorldCut { adopts: vec![hid(1)], ..part(1, hid(2), vec![]) };
    let err = w.hosts[2].prepare_cut(cut, w.fences[2].clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
    assert!(matches!(w.hosts[2].load_in_flight_of(hid(1), &w.fences[2]), Err(StoreError::Unsupported(_))));
    assert_eq!(prepared(&w.w), 0);
    w.shutdown();
}

#[test]
fn regions_of_entities_finds_each_row() {
    each_backend("regions_of_entities", false, |url| {
        let h = open(url);
        let f = crate::handle::fence(&h);
        let with = |serial: u64| {
            let mut b: EntityBundle = bundle();
            b.id = EntityId { birth_region: A, serial };
            b
        };
        let mut ra = region();
        ra.entities = vec![with(1), with(2)];
        let mut rb = region();
        rb.entities = vec![with(3)];
        let cut = WorldCut { regions: vec![(A, ra), (B, rb)], ..part(1, hid(0), vec![]) };
        h.commit_cut(cut, f).recv().unwrap().unwrap();
        let id = |serial| EntityId { birth_region: A, serial };
        let found = h.regions_of_entities(vec![id(3), id(9), id(1)]).unwrap();
        assert_eq!(found, vec![(id(3), B), (id(1), A)], "each stored row, none for an unknown id");
        h.shutdown();
    });
}

#[test]
fn leave_kind_needs_a_clean_row() {
    let Some(w) = World::new(2) else { return };
    assert_eq!(w.declare(hid(1), hid(0), SuccessionKind::Leave).unwrap(), Declared::NotLapsed(vec![hid(1)]), "dirty");
    // Host 1 leaves clean (its leave cut).
    w.hosts[0].prepare_cut(part(1, hid(0), vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, hid(1), vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1)], vec![hid(1)], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    let s = match w.declare(hid(1), hid(0), SuccessionKind::Leave).unwrap() {
        Declared::Done(mut s) => s.remove(0),
        other => panic!("{other:?}"),
    };
    assert_eq!((s.kind, s.base_cut, s.survivors.clone()), (SuccessionKind::Leave, CutId(1), vec![hid(0)]));
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, true), "a leave marks no row dead");
    assert_eq!(s.moved.len(), 2);
    let heir = w.h.owned_by(hid(0)).unwrap();
    w.hosts[0].prepare_cut(part(2, hid(0), vec![]), heir, 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(2), 200, true, vec![hid(0)], vec![], w.c, 0, vec![s.id]).unwrap());
    // Succeeded already.
    assert_eq!(w.declare(hid(1), hid(0), SuccessionKind::Leave).unwrap(), Declared::Gone(vec![hid(1)]));
    w.shutdown();
}

/// SQLite: successions need several hosts, which need Postgres.
#[test]
fn sqlite_declares_nothing() {
    let h = open(&StoreUrl::Memory);
    let c = h.claim_coordinator().unwrap();
    let err = h.declare_succession(vec![(hid(1), 1)], vec![hid(0)], SuccessionKind::Crash, c, 0).unwrap_err();
    assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
    assert!(matches!(h.rollback_world(c), Err(StoreError::Unsupported(_))));
    assert!(h.open_successions().unwrap().is_empty());
    h.shutdown();
}

// ---- Fix round 1: a declared-dead host wins nothing; several predecessors; refusals ----------

/// Host 1 is declared dead while its process still runs: its `claim_free` of a free row is
/// refused as fenced and the row stays free.
#[test]
fn a_dead_hosts_claim_of_a_free_row_is_refused() {
    let Some(w) = World::new(2) else { return };
    w.vouch(hid(1));
    w.declared(hid(1), hid(0));
    let free = (9, 9);
    let err = w.hosts[1].claim_free(hid(1), boot_of(hid(1)), vec![free]).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None });
    assert!(w.h.owners().unwrap().iter().all(|(sr, _, _)| *sr != free), "the row was not created");
    // The heir claims it as usual.
    let won = w.hosts[0].claim_free(hid(0), boot_of(hid(0)), vec![free]).unwrap();
    assert_eq!(won.epochs.keys().copied().collect::<Vec<_>>(), vec![free]);
    w.shutdown();
}

/// A free row host 1 won just before its declaration moves to the heir with the rest.
#[test]
fn a_claim_committed_just_before_the_declaration_moves_with_the_rest() {
    let Some(w) = World::new(2) else { return };
    let late = (9, 9);
    let won = w.hosts[1].claim_free(hid(1), boot_of(hid(1)), vec![late]).unwrap();
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    assert_eq!(s.moved.len(), 3, "its two boot rows and the late one: {:?}", s.moved);
    assert!(s.moved.contains(&(late, won.epochs[&late] + 1)));
    assert!(w.h.owned_by(hid(1)).unwrap().epochs.is_empty(), "the dead ordinal owns nothing");
    w.shutdown();
}

/// A boot declared dead between its `owned_by` and its re-claim takes nothing back from the
/// heir; a live boot re-claims its own rows.
#[test]
fn a_dead_boots_reclaim_is_refused() {
    let Some(w) = World::new(2) else { return };
    let own: Vec<_> = w.fences[1].epochs.keys().copied().collect();
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    let err = w.hosts[1].claim_super_regions_live(hid(1), boot_of(hid(1)), own.clone()).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None });
    let heir = w.h.owned_by(hid(0)).unwrap();
    for (sr, e) in &s.moved {
        assert_eq!(heir.epochs.get(sr), Some(e), "still the heir's");
    }
    let mine: Vec<_> = w.fences[0].epochs.keys().copied().collect();
    let again = w.hosts[0].claim_super_regions_live(hid(0), boot_of(hid(0)), mine.clone()).unwrap();
    assert_eq!(again.epochs.keys().copied().collect::<Vec<_>>(), mine);
    w.shutdown();
}

/// Two predecessors in one call: one shared heir, then (another world) two heirs.
#[test]
fn several_predecessors_in_one_call() {
    let Some(w) = World::new(3) else { return };
    w.vouch(hid(1));
    w.vouch(hid(2));
    let done = w.h.declare_succession(vec![(hid(2), boot_of(hid(2))), (hid(1), boot_of(hid(1)))], vec![hid(0), hid(0)], SuccessionKind::Crash, w.c, 0).unwrap();
    let Declared::Done(s) = done else { panic!("{done:?}") };
    assert_eq!(s.iter().map(|s| (s.predecessor, s.heir)).collect::<Vec<_>>(), vec![(hid(2), hid(0)), (hid(1), hid(0))], "the call's order");
    assert!(s[0].id != s[1].id);
    for s in &s {
        assert_eq!(s.survivors, vec![hid(0)], "neither predecessor survives");
    }
    assert_eq!(w.h.owned_by(hid(0)).unwrap().epochs.len(), 1 + 1 + 2, "all of 1's and 2's land is 0's");
    assert_eq!((w.state_of(hid(1)).0, w.state_of(hid(2)).0), (HOST_DEAD, HOST_DEAD));
    assert_eq!(w.h.open_successions().unwrap(), s);
    // Settled together.
    w.hosts[0].prepare_cut(part(1, hid(0), vec![]), w.h.owned_by(hid(0)).unwrap(), 0).recv().unwrap().unwrap();
    let ids: Vec<_> = s.iter().map(|s| s.id).collect();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![hid(0)], vec![], w.c, 0, ids).unwrap());
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();

    let Some(w) = World::new(4) else { return };
    exec(&w.w, "UPDATE hosts SET seen_ms = 0 WHERE ordinal IN (1, 2)");
    let done = w.h.declare_succession(vec![(hid(1), boot_of(hid(1))), (hid(2), boot_of(hid(2)))], vec![hid(0), hid(3)], SuccessionKind::Crash, w.c, 0).unwrap();
    let Declared::Done(s) = done else { panic!("{done:?}") };
    assert_eq!(s.iter().map(|s| (s.predecessor, s.heir)).collect::<Vec<_>>(), vec![(hid(1), hid(0)), (hid(2), hid(3))]);
    assert_eq!(s[0].survivors, vec![hid(0), hid(3)]);
    assert_eq!(w.h.owned_by(hid(3)).unwrap().epochs.len(), 2, "3's own and 2's");
    assert_eq!(w.h.owned_by(hid(0)).unwrap().epochs.len(), 3, "0's own and 1's two");
    w.shutdown();
}

/// An heir whose lease is not fresh (lapsed, or vouched dead), and a predecessor already
/// dead: `Gone`, nothing written.
#[test]
fn a_stale_heir_or_a_dead_predecessor_is_gone() {
    let Some(w) = World::new(3) else { return };
    w.vouch(hid(1));
    w.lapse(hid(2));
    assert_eq!(w.declare(hid(1), hid(2), SuccessionKind::Crash).unwrap(), Declared::Gone(vec![hid(2)]), "a lapsed heir");
    w.vouch(hid(2));
    assert_eq!(w.declare(hid(1), hid(2), SuccessionKind::Crash).unwrap(), Declared::Gone(vec![hid(2)]), "a vouched heir");
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, false));
    assert_eq!(w.h.owned_by(hid(1)).unwrap(), w.fences[1]);
    exec(&w.w, &format!("UPDATE hosts SET state = {HOST_DEAD} WHERE ordinal = 1"));
    assert_eq!(w.declare(hid(1), hid(0), SuccessionKind::Crash).unwrap(), Declared::Gone(vec![hid(1)]), "already dead");
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();
}

/// Malformed calls are refused before anything is read or written.
#[test]
fn malformed_declarations_are_unsupported() {
    let Some(w) = World::new(3) else { return };
    let unsupported = |dead: Vec<(game::HostId, u64)>, heirs: Vec<game::HostId>| {
        let r = w.h.declare_succession(dead, heirs, SuccessionKind::Crash, w.c, 0);
        assert!(matches!(r, Err(StoreError::Unsupported(_))), "{r:?}");
    };
    unsupported(vec![], vec![]);
    unsupported(vec![(hid(1), boot_of(hid(1)))], vec![hid(0), hid(2)]);
    unsupported(vec![(hid(1), boot_of(hid(1))), (hid(1), boot_of(hid(1)))], vec![hid(0), hid(2)]);
    unsupported(vec![(hid(1), boot_of(hid(1))), (hid(2), boot_of(hid(2)))], vec![hid(2), hid(0)]);
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();
}

// ---- R4 Task 11: a cleanly-left boot claims nothing ------------------------------------------

/// Host 1 leaves clean (its leave cut committed): from then on a claim by THAT boot — of a
/// free row, or a re-claim of its own — is refused as fenced and writes nothing, before and
/// after its `Leave` succession is declared (the declaration does not mark the row dead).
/// Another boot of host 1 first WAITS (`Wait(Succession)`: an open succession names its
/// ordinal as predecessor) until the heir's cut settles that succession, then registers
/// normally (dirty, live) and claims normally.
#[test]
fn a_cleanly_left_boot_claims_nothing() {
    let Some(w) = World::new(2) else { return };
    let own: Vec<_> = w.fences[1].epochs.keys().copied().collect();
    w.hosts[0].prepare_cut(part(1, hid(0), vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, hid(1), vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1)], vec![hid(1)], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[1].cut_decided(CutId(1)).unwrap(), Some(true));
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, true), "left clean");
    let free = (9, 9);
    let refused = |w: &World| {
        assert_eq!(w.hosts[1].claim_free(hid(1), boot_of(hid(1)), vec![free]).unwrap_err(), StoreError::Fenced { sr: None });
        assert_eq!(w.hosts[1].claim_super_regions_live(hid(1), boot_of(hid(1)), own.clone()).unwrap_err(), StoreError::Fenced { sr: None });
        assert!(w.h.owners().unwrap().iter().all(|(sr, _, _)| *sr != free), "the free row was not created");
    };
    refused(&w);
    let before = w.h.owned_by(hid(1)).unwrap();
    assert_eq!(before.epochs, w.fences[1].epochs, "its own rows untouched by the refused re-claim");
    let s = match w.declare(hid(1), hid(0), SuccessionKind::Leave).unwrap() {
        Declared::Done(mut s) => s.remove(0),
        other => panic!("{other:?}"),
    };
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, true), "a leave marks no row dead");
    refused(&w);
    assert!(w.h.owned_by(hid(1)).unwrap().epochs.is_empty(), "every row is the heir's");
    assert_eq!(s.moved.len(), 2);
    // Fix round 2 of Task 14: while the `Leave` succession naming it is open its next boot
    // waits (the heir adopts the in-flight rows its leave cut left under its ordinal); once a
    // cut settles it, the next boot registers (dirty, live) and claims normally.
    let none = Fence { owner: hid(1), epochs: Default::default(), all_hosts: false };
    let waits = w.hosts[1].register_host(HostRow { boot: 9001, ..row(hid(1)) }, &none, CrashPolicy::Succession).unwrap();
    assert_eq!(waits, world_store::BootVerdict::Wait(world_store::WaitFor::Succession));
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, true), "nothing written");
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    let heir = w.h.owned_by(hid(0)).unwrap();
    w.hosts[0].prepare_cut(WorldCut { adopts: vec![hid(1)], ..part(2, hid(0), vec![]) }, heir, 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(2), 200, true, vec![hid(0)], vec![], w.c, 0, vec![s.id]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(2)).unwrap(), Some(true));
    let next = w.hosts[1].register_host(HostRow { boot: 9001, ..row(hid(1)) }, &none, CrashPolicy::Succession).unwrap();
    assert!(matches!(next, world_store::BootVerdict::Registered(_)), "{next:?}");
    assert_eq!(w.state_of(hid(1)), (HOST_LIVE, false));
    let won = w.hosts[1].claim_free(hid(1), 9001, vec![free]).unwrap();
    assert_eq!(won.epochs.keys().copied().collect::<Vec<_>>(), vec![free]);
    w.shutdown();
}

// ---- R4 Task 14: a rollback strands no in-flight row -----------------------------------------

/// After a world rollback (the coordinator's, fenced, or the operator's) the abandoned
/// succession's land stays with its heir, and so do the predecessor's in-flight rows: they are
/// re-keyed to the heir in the same transaction. The heir restarts from the cut owning the
/// land and reads them as its own (`load_in_flight(Some(heir))`); the predecessor's ordinal,
/// should it ever boot again, owns nothing and reads none — every row has exactly one reader.
#[test]
fn rollback_leaves_no_in_flight_row_without_a_reader() {
    for operator in [false, true] {
        let Some(w) = World::new(3) else { return };
        let own0 = vec![in_flight(RegionCoords::new(0, 3))];
        let dead1: Vec<InFlightRow> = (1..=2).map(|z| in_flight(RegionCoords::new(1, z))).collect();
        let own2 = vec![in_flight(RegionCoords::new(16, 4))];
        for (h, rows) in [(0, &own0), (1, &dead1), (2, &own2)] {
            w.hosts[h].prepare_cut(part(1, hid(h as u64), rows.clone()), w.fences[h].clone(), 0).recv().unwrap().unwrap();
        }
        assert!(w.h.decide_cut(CutId(1), 100, true, vec![hid(0), hid(1), hid(2)], vec![], w.c, 0, vec![]).unwrap());
        for h in &w.hosts {
            assert_eq!(h.cut_decided(CutId(1)).unwrap(), Some(true));
        }
        w.vouch(hid(1));
        let s = w.declared(hid(1), hid(0));
        let generation = if operator { w.h.rollback_world_operator().unwrap().0 } else { w.h.rollback_world(w.c).unwrap() };
        assert_eq!(generation, 1, "operator {operator}");
        assert!(w.h.open_successions().unwrap().is_empty());
        assert!(w.h.load_in_flight(Some(hid(1))).unwrap().is_empty(), "operator {operator}: no row without a reader under host 1");
        let mut heir_rows = w.h.load_in_flight(Some(hid(0))).unwrap();
        heir_rows.sort_by_key(|r| format!("{r:?}"));
        let mut want: Vec<InFlightRow> = own0.iter().chain(&dead1).cloned().collect();
        want.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(heir_rows, want, "operator {operator}: the heir reads its own and the predecessor's rows");
        assert_eq!(w.h.load_in_flight(Some(hid(2))).unwrap(), own2, "operator {operator}: another host's rows untouched");
        let heir = w.h.owned_by(hid(0)).unwrap();
        for (sr, e) in &s.moved {
            assert_eq!(heir.epochs.get(sr), Some(e), "operator {operator}: the land stays with the heir");
        }
        // The heir's first part (after its restart at the new generation) replaces them all.
        w.hosts[0].prepare_cut(part(2, hid(0), own0.clone()), heir, 1).recv().unwrap().unwrap();
        w.hosts[2].prepare_cut(part(2, hid(2), own2.clone()), w.fences[2].clone(), 1).recv().unwrap().unwrap();
        let c = w.h.claim_coordinator().unwrap();
        assert!(w.h.decide_cut(CutId(2), 200, true, vec![hid(0), hid(2)], vec![], c, 1, vec![]).unwrap(), "operator {operator}");
        assert_eq!(w.h.load_in_flight(Some(hid(0))).unwrap(), own0);
        assert_eq!(prepared(&w.w), 0);
        w.shutdown();
    }
}
