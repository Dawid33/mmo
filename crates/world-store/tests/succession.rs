//! Successions (R4): the coordinator declares a lapsed host dead in one fenced transaction
//! that moves all its super-regions to an heir; the next committed cut settles it; a world
//! rollback abandons an open one; the heir's part adopts the dead host's in-flight rows.
//! Postgres unless noted (several hosts need it); SQLite declares nothing.

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

fn boot_of(ordinal: u32) -> u64 {
    100 + u64::from(ordinal)
}

fn row(ordinal: u32) -> HostRow {
    HostRow {
        ordinal,
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

fn region_of_host(host: u32) -> RegionCoords {
    match host {
        0 => A,
        1 => B,
        _ => C,
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
fn part(id: u64, host: u32, in_flight: Vec<InFlightRow>) -> WorldCut {
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
fn in_flight(rc: RegionCoords) -> InFlightRow {
    InFlightRow::Intent { target: rc, msg: intent() }
}

/// A world of `n` hosts (0..n), each registered with its own fence (host 1 also holds
/// [`FAR`]), and a claimed coordinator. Each host gets its own store handle.
struct World {
    w: PgWorld,
    /// The coordinator's handle.
    h: StoreHandle,
    hosts: Vec<StoreHandle>,
    fences: Vec<Fence>,
    c: CoordinatorFence,
}

impl World {
    fn new(n: u32) -> Option<World> {
        let w = pg_world()?;
        let h = open(&w.url);
        let s = h.super_region_regions();
        let mut hosts = Vec::new();
        let mut fences = Vec::new();
        for o in 0..n {
            let mut srs = vec![game::super_region_of(region_of_host(o), s)];
            if o == 1 {
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
    fn lapse(&self, o: u32) {
        exec(&self.w, &format!("UPDATE hosts SET seen_ms = seen_ms - {} WHERE ordinal = {o}", 2 * game::HOST_LEASE_MS));
    }

    /// Host `o`'s row is vouched dead (its next boot found it dirty while others ran).
    fn vouch(&self, o: u32) {
        exec(&self.w, &format!("UPDATE hosts SET seen_ms = 0 WHERE ordinal = {o}"));
    }

    fn declare(&self, dead: u32, heir: u32, kind: SuccessionKind) -> Result<Declared, StoreError> {
        self.h.declare_succession(vec![(dead, boot_of(dead))], vec![heir], kind, self.c, 0)
    }

    /// Declares `dead` crashed with `heir` and returns the one succession.
    fn declared(&self, dead: u32, heir: u32) -> Succession {
        match self.declare(dead, heir, SuccessionKind::Crash).unwrap() {
            Declared::Done(mut s) => {
                assert_eq!(s.len(), 1);
                s.remove(0)
            }
            other => panic!("declared: {other:?}"),
        }
    }

    fn state_of(&self, o: u32) -> (i64, bool) {
        let r = self.h.hosts().unwrap().into_iter().find(|r| r.ordinal == o).unwrap();
        (r.state, r.clean)
    }

    fn shutdown(self) {
        for hh in self.hosts {
            hh.shutdown();
        }
        self.h.shutdown();
    }
}

#[test]
fn declare_moves_every_super_region_and_fences_the_dead_host() {
    let Some(w) = World::new(2) else { return };
    w.lapse(1);
    let f1 = w.fences[1].clone();
    let s = w.declared(1, 0);
    assert_eq!((s.predecessor, s.predecessor_boot, s.heir, s.kind), (1, boot_of(1), 0, SuccessionKind::Crash));
    assert_eq!(s.base_cut, CutId(0), "no cut committed yet");
    assert_eq!(s.survivors, vec![0]);
    let mut want: Vec<_> = f1.epochs.iter().map(|(sr, e)| (*sr, e + 1)).collect();
    want.sort();
    assert_eq!(s.moved, want, "every super-region of host 1, epoch + 1");
    // The heir holds them at the new epochs, beside its own.
    let heir = w.h.owned_by(0).unwrap();
    for (sr, e) in &s.moved {
        assert_eq!(heir.epochs.get(sr), Some(e));
    }
    for (sr, e) in &w.fences[0].epochs {
        assert_eq!(heir.epochs.get(sr), Some(e), "the heir's own rows are untouched");
    }
    assert!(w.h.owned_by(1).unwrap().epochs.is_empty());
    // The dead host's fence is refused everywhere.
    let dead = &w.hosts[1];
    fn fenced<T>(r: Result<T, StoreError>) -> bool {
        matches!(r, Err(StoreError::Fenced { sr: Some(_) }))
    }
    assert!(fenced(dead.commit_cut(part(1, 1, vec![]), f1.clone()).recv().unwrap()), "commit");
    assert!(fenced(dead.prepare_cut(part(2, 1, vec![]), f1.clone(), 0).recv().unwrap()), "prepare");
    assert!(fenced(dead.load_region(B, &f1)), "load");
    assert_eq!(prepared(&w.w), 0);
    // Its lease is gone; its row is dead and still dirty.
    assert!(!dead.heartbeat(1, boot_of(1)).unwrap());
    assert_eq!(w.state_of(1), (HOST_DEAD, false));
    assert_eq!(w.h.open_successions().unwrap(), vec![s]);
    w.shutdown();
}

#[test]
fn declare_refuses_a_row_that_heartbeated() {
    let Some(w) = World::new(2) else { return };
    // Registered just now: not lapsed.
    assert_eq!(w.declare(1, 0, SuccessionKind::Crash).unwrap(), Declared::NotLapsed(vec![1]));
    // Lapsed, then it beat again before the declaration.
    w.lapse(1);
    assert!(w.hosts[1].heartbeat(1, boot_of(1)).unwrap());
    assert_eq!(w.declare(1, 0, SuccessionKind::Crash).unwrap(), Declared::NotLapsed(vec![1]));
    // Another boot of the ordinal is not the one the caller judged dead.
    w.lapse(1);
    let other = w.h.declare_succession(vec![(1, 999)], vec![0], SuccessionKind::Crash, w.c, 0).unwrap();
    assert_eq!(other, Declared::Gone(vec![1]));
    // Nothing changed.
    assert_eq!(w.h.owned_by(1).unwrap(), w.fences[1]);
    assert_eq!(w.state_of(1), (HOST_LIVE, false));
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();
}

#[test]
fn a_vouched_row_needs_no_lapse() {
    let Some(w) = World::new(2) else { return };
    w.vouch(1);
    let s = w.declared(1, 0);
    assert_eq!(s.heir, 0);
    assert_eq!(w.state_of(1), (HOST_DEAD, false));
    w.shutdown();
}

#[test]
fn second_declare_while_one_is_open_says_so() {
    let Some(w) = World::new(3) else { return };
    w.vouch(1);
    let s = w.declared(1, 0);
    assert_eq!(s.survivors, vec![0, 2], "every live dirty host but the dead one");
    w.vouch(2);
    let f2 = w.h.owned_by(2).unwrap();
    assert_eq!(w.declare(2, 0, SuccessionKind::Crash).unwrap(), Declared::SuccessionOpen);
    assert_eq!(w.h.owned_by(2).unwrap(), f2, "nothing moved");
    assert_eq!(w.state_of(2), (HOST_LIVE, false));
    assert_eq!(w.h.open_successions().unwrap(), vec![s]);
    w.shutdown();
}

#[test]
fn stale_coordinator_cannot_declare() {
    let Some(w) = World::new(2) else { return };
    w.vouch(1);
    let newer = w.h.claim_coordinator().unwrap();
    let err = w.declare(1, 0, SuccessionKind::Crash).unwrap_err();
    assert_eq!(err, StoreError::Fenced { sr: None });
    let err = w.h.declare_succession(vec![(1, boot_of(1))], vec![0], SuccessionKind::Crash, newer, 7).unwrap_err();
    assert_eq!(err, StoreError::StaleGeneration { stored: 0, mine: 7 });
    assert_eq!(w.state_of(1), (HOST_LIVE, false));
    assert_eq!(w.h.owned_by(1).unwrap(), w.fences[1]);
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
    w.hosts[1].prepare_cut(part(1, 1, vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    w.vouch(1);
    let started = Instant::now();
    let err = w.declare(1, 0, SuccessionKind::Crash).unwrap_err();
    let waited = started.elapsed();
    assert!(matches!(&err, StoreError::Backend(m) if m.contains("lock_timeout")), "{err}");
    assert!(waited >= Duration::from_millis(WAIT_MS) && waited < Duration::from_millis(WAIT_MS * 10), "waited {waited:?}");
    // Nothing changed.
    assert_eq!(w.state_of(1), (HOST_LIVE, false));
    assert_eq!(w.h.owned_by(1).unwrap(), w.fences[1]);
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(prepared(&w.w), 1);
    // The cut is aborted: the declaration goes through.
    assert!(!w.h.abort_cut(CutId(1), 100).unwrap());
    assert_eq!(prepared(&w.w), 0);
    let s = w.declared(1, 0);
    assert_eq!(s.moved.len(), 2);
    w.shutdown();
}

#[test]
fn commit_without_naming_the_open_succession_becomes_an_abort() {
    let Some(w) = World::new(2) else { return };
    w.vouch(1);
    let s = w.declared(1, 0);
    let heir = w.h.owned_by(0).unwrap();
    w.hosts[0].prepare_cut(part(1, 0, vec![]), heir.clone(), 0).recv().unwrap().unwrap();
    assert!(!w.h.decide_cut(CutId(1), 100, true, vec![0], vec![], w.c, 0, vec![]).unwrap(), "settles nothing");
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(false));
    w.hosts[0].prepare_cut(part(2, 0, vec![]), heir, 0).recv().unwrap().unwrap();
    let wrong = world_store::SuccessionId(s.id.0 + 1);
    assert!(!w.h.decide_cut(CutId(2), 200, true, vec![0], vec![], w.c, 0, vec![s.id, wrong]).unwrap(), "settles one too many");
    assert_eq!(prepared(&w.w), 0);
    assert_eq!(w.h.open_successions().unwrap(), vec![s]);
    assert_eq!(w.state_of(1), (HOST_DEAD, false));
    w.shutdown();
}

#[test]
fn commit_settles_and_cleans_the_predecessor() {
    let Some(w) = World::new(2) else { return };
    // A committed cut first: the succession's base.
    w.hosts[0].prepare_cut(part(1, 0, vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, 1, vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![0, 1], vec![], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    w.vouch(1);
    let s = w.declared(1, 0);
    assert_eq!(s.base_cut, CutId(1));
    let heir = w.h.owned_by(0).unwrap();
    w.hosts[0].prepare_cut(part(2, 0, vec![]), heir, 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(2), 200, true, vec![0], vec![], w.c, 0, vec![s.id]).unwrap());
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(scalar_i64(&w.w, &format!("SELECT settled_cut FROM successions WHERE id = {}", s.id.0)), 2);
    assert_eq!(w.state_of(1), (HOST_DEAD, true), "the predecessor's row is clean");
    // A new boot of the ordinal registers at once.
    let back = w.hosts[1].register_host(HostRow { boot: 555, ..row(1) }, &Fence::default(), CrashPolicy::Succession);
    assert!(matches!(back, Ok(world_store::BootVerdict::Registered(_))), "{back:?}");
    w.shutdown();
}

#[test]
fn rollback_abandons_open_ones_and_keeps_the_land_with_the_heir() {
    let Some(w) = World::new(2) else { return };
    w.vouch(1);
    let s = w.declared(1, 0);
    let stale = CoordinatorFence { epoch: w.c.epoch - 1 };
    assert_eq!(w.h.rollback_world(stale).unwrap_err(), StoreError::Fenced { sr: None });
    assert_eq!(w.h.world_generation().unwrap(), 0);
    assert_eq!(w.h.rollback_world(w.c).unwrap(), 1);
    assert_eq!(w.h.world_generation().unwrap(), 1);
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(scalar_i64(&w.w, &format!("SELECT abandoned FROM successions WHERE id = {}", s.id.0)), 1);
    let heir = w.h.owned_by(0).unwrap();
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
    w.hosts[0].prepare_cut(part(1, 0, vec![own.clone()]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, 1, dead_rows.clone()), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![0, 1], vec![], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    w.vouch(1);
    let s = w.declared(1, 0);
    let heir = w.h.owned_by(0).unwrap();
    assert_eq!(w.hosts[0].load_in_flight_of(1, &heir).unwrap(), dead_rows, "the heir reads the dead host's rows");
    // The heir's part keeps its own row and two of the dead host's.
    let kept = vec![own.clone(), dead_rows[1].clone(), dead_rows[2].clone()];
    let adopting = WorldCut { adopts: vec![1], ..part(2, 0, kept.clone()) };
    w.hosts[0].prepare_cut(adopting, heir.clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(2), 200, true, vec![0], vec![], w.c, 0, vec![s.id]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(2)).unwrap(), Some(true));
    assert_eq!(w.h.load_in_flight(Some(0)).unwrap(), kept);
    assert!(w.h.load_in_flight(Some(1)).unwrap().is_empty(), "host 1 has no rows left");
    // Settled: adopting again is refused.
    let again = WorldCut { adopts: vec![1], ..part(3, 0, kept) };
    let err = w.hosts[0].prepare_cut(again, heir.clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
    assert!(matches!(w.hosts[0].load_in_flight_of(1, &heir), Err(StoreError::Unsupported(_))));
    assert_eq!(prepared(&w.w), 0);
    w.shutdown();
}

#[test]
fn adopts_without_a_succession_is_refused() {
    // Any backend: the one-phase commit and the read check the same way.
    each_backend("adopts_refused", false, |url| {
        let h = open(url);
        let f = crate::handle::fence(&h);
        let cut = WorldCut { adopts: vec![1], ..part(1, 0, vec![in_flight(A)]) };
        let err = h.commit_cut(cut, f.clone()).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
        assert!(h.load_in_flight(None).unwrap().is_empty(), "nothing written");
        assert!(matches!(h.load_in_flight_of(1, &f), Err(StoreError::Unsupported(_))));
        h.shutdown();
    });
    // Postgres: a succession whose heir is another host does not let host 2 adopt.
    let Some(w) = World::new(3) else { return };
    w.vouch(1);
    w.declared(1, 0);
    let cut = WorldCut { adopts: vec![1], ..part(1, 2, vec![]) };
    let err = w.hosts[2].prepare_cut(cut, w.fences[2].clone(), 0).recv().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
    assert!(matches!(w.hosts[2].load_in_flight_of(1, &w.fences[2]), Err(StoreError::Unsupported(_))));
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
        let cut = WorldCut { regions: vec![(A, ra), (B, rb)], ..part(1, 0, vec![]) };
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
    assert_eq!(w.declare(1, 0, SuccessionKind::Leave).unwrap(), Declared::NotLapsed(vec![1]), "dirty");
    // Host 1 leaves clean (its leave cut).
    w.hosts[0].prepare_cut(part(1, 0, vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, 1, vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![0, 1], vec![1], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    let s = match w.declare(1, 0, SuccessionKind::Leave).unwrap() {
        Declared::Done(mut s) => s.remove(0),
        other => panic!("{other:?}"),
    };
    assert_eq!((s.kind, s.base_cut, s.survivors.clone()), (SuccessionKind::Leave, CutId(1), vec![0]));
    assert_eq!(w.state_of(1), (HOST_LIVE, true), "a leave marks no row dead");
    assert_eq!(s.moved.len(), 2);
    let heir = w.h.owned_by(0).unwrap();
    w.hosts[0].prepare_cut(part(2, 0, vec![]), heir, 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(2), 200, true, vec![0], vec![], w.c, 0, vec![s.id]).unwrap());
    // Succeeded already.
    assert_eq!(w.declare(1, 0, SuccessionKind::Leave).unwrap(), Declared::Gone(vec![1]));
    w.shutdown();
}

/// SQLite: successions need several hosts, which need Postgres.
#[test]
fn sqlite_declares_nothing() {
    let h = open(&StoreUrl::Memory);
    let c = h.claim_coordinator().unwrap();
    let err = h.declare_succession(vec![(1, 1)], vec![0], SuccessionKind::Crash, c, 0).unwrap_err();
    assert!(matches!(err, StoreError::Unsupported(_)), "{err}");
    assert!(matches!(h.rollback_world(c), Err(StoreError::Unsupported(_))));
    assert!(h.open_successions().unwrap().is_empty());
    h.shutdown();
}
