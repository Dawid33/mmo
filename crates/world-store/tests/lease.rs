//! The lease on a `hosts` row (R4): `register_host` stamps `seen_ms`, `state` and a rising
//! `reg`; `heartbeat` moves `seen_ms` for the live boot only.

use game::CrashPolicy;
use world_store::{BootVerdict, Fence, HOST_LIVE, HostBoot, HostRow, WaitFor};

use crate::handle::{TEST_SUPER_REGIONS, each_backend, open, raw_exec, register};

fn row(ordinal: u32, boot: u64) -> HostRow {
    HostRow {
        ordinal,
        boot,
        // Ignored by `register_host`.
        generation: 99,
        clean: true,
        state: 7,
        seen_ms: 1,
        reg: 42,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    }
}

#[test]
fn register_stamps_seen_and_a_rising_reg() {
    each_backend("lease_register", false, |url| {
        let h = open(url);
        let a = h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        let b = h.claim_super_regions(1, vec![TEST_SUPER_REGIONS[1]]).unwrap();
        let boot0 = register(&h, row(0, 10), &a).unwrap();
        let boot1 = register(&h, row(1, 11), &b).unwrap();
        assert_eq!((boot0.reg, boot1.reg), (1, 2));
        let now = h.db_now_ms().unwrap();
        let rows = h.hosts().unwrap();
        assert_eq!(rows.iter().map(|r| (r.ordinal, r.reg, r.state)).collect::<Vec<_>>(), vec![(0, 1, HOST_LIVE), (1, 2, HOST_LIVE)]);
        for r in &rows {
            assert!(r.seen_ms.abs_diff(now) < 5_000, "seen_ms {} vs db now {now}", r.seen_ms);
        }
        // A re-registration takes the next number.
        assert_eq!(register(&h, row(0, 12), &a).unwrap().reg, 3);
        h.shutdown();
    });
}

#[test]
fn heartbeat_moves_seen_ms_for_the_live_boot_only() {
    each_backend("lease_heartbeat", true, |url| {
        let h = open(url);
        let a = h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        register(&h, row(0, 10), &a).unwrap();
        let seen = || h.hosts().unwrap()[0].seen_ms;
        let first = seen();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(h.heartbeat(0, 10).unwrap());
        let second = seen();
        assert!(second > first, "{second} > {first}");
        // Another boot of the ordinal, or an ordinal that never registered: replaced.
        assert!(!h.heartbeat(0, 11).unwrap());
        assert!(!h.heartbeat(5, 10).unwrap());
        assert_eq!(seen(), second);
        // A dead row no longer takes beats.
        h.shutdown();
        raw_exec(url, "UPDATE hosts SET state = 1");
        let h = open(url);
        let before = h.hosts().unwrap()[0].seen_ms;
        assert!(!h.heartbeat(0, 10).unwrap());
        assert_eq!(h.hosts().unwrap()[0].seen_ms, before);
        h.shutdown();
    });
}

/// Postgres: a held lock on the host's own row makes a beat fail with an error after the
/// lock timeout — it neither hangs nor answers `false` (which means "replaced").
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_heartbeat_behind_a_held_row_lock_errors_instead_of_waiting() {
    use sqlx::Connection;
    use world_store::{HEARTBEAT_LOCK_TIMEOUT_MS, StoreUrl};

    if crate::common::pg_admin_url().is_none() {
        return crate::common::skip_msg();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(crate::common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let h = open(&StoreUrl::Postgres(db.url.clone()));
    let a = h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
    register(&h, row(0, 10), &a).unwrap();
    rt.block_on(async {
        let mut conn = sqlx::PgConnection::connect(&db.url).await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM hosts WHERE ordinal = 0 FOR UPDATE").execute(&mut *tx).await.unwrap();
        let started = std::time::Instant::now();
        let h2 = h.clone();
        let beat = tokio::task::spawn_blocking(move || h2.heartbeat(0, 10)).await.unwrap();
        let waited = started.elapsed();
        assert!(beat.is_err(), "a beat behind a row lock is an error, got {beat:?}");
        assert!(waited < std::time::Duration::from_millis(HEARTBEAT_LOCK_TIMEOUT_MS * 3), "waited {waited:?}");
        tx.rollback().await.unwrap();
        // Released: the beat goes through again.
        let h2 = h.clone();
        assert!(tokio::task::spawn_blocking(move || h2.heartbeat(0, 10)).await.unwrap().unwrap());
    });
    h.shutdown();
    drop(db);
}

/// Host 0's registration as a boot under `policy`: the boot after one of the same ordinal.
fn boot_as(h: &world_store::StoreHandle, boot: u64, policy: CrashPolicy) -> BootVerdict {
    h.register_host(row(0, boot), &Fence::default(), policy).unwrap()
}

/// Host 0 boots again (boot 20) under `Succession` after its previous boot (10) and host 1's
/// (11) registered, by what its prior row says: the verdict, the world generation after it,
/// and the prior row (boot, `seen_ms`) when it was left in place.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn boot_verdicts() {
    struct Case {
        what: &'static str,
        /// Run against the store after both boots registered.
        setup: &'static [&'static str],
        verdict: fn(&BootVerdict) -> bool,
        generation: u64,
        /// The prior row is left as it was (boot 10), its `seen_ms` 0 (`Some(true)`, vouched)
        /// or unchanged (`Some(false)`); `None`: replaced by boot 20.
        prior_vouched: Option<bool>,
    }
    let cases = [
        Case {
            what: "no prior row",
            setup: &["DELETE FROM hosts WHERE ordinal = 0"],
            verdict: |v| matches!(v, BootVerdict::Registered(HostBoot { generation: 0, bumped: false, .. })),
            generation: 0,
            prior_vouched: None,
        },
        Case {
            what: "a clean prior row",
            setup: &["UPDATE hosts SET clean = 1 WHERE ordinal = 0"],
            verdict: |v| matches!(v, BootVerdict::Registered(HostBoot { generation: 0, bumped: false, .. })),
            generation: 0,
            prior_vouched: None,
        },
        Case {
            what: "a dirty prior row of an older generation (R3's resume)",
            setup: &["UPDATE world SET generation = 1"],
            verdict: |v| matches!(v, BootVerdict::Registered(HostBoot { generation: 1, bumped: false, .. })),
            generation: 1,
            prior_vouched: None,
        },
        Case {
            what: "a dirty prior row marked dead (being inherited)",
            setup: &["UPDATE hosts SET state = 1 WHERE ordinal = 0"],
            verdict: |v| matches!(v, BootVerdict::Wait(WaitFor::Succession)),
            generation: 0,
            prior_vouched: Some(false),
        },
        Case {
            what: "a dirty live prior row, another host alive",
            setup: &[],
            verdict: |v| matches!(v, BootVerdict::Wait(WaitFor::Vouched)),
            generation: 0,
            prior_vouched: Some(true),
        },
        Case {
            what: "a dirty live prior row, the only other fresh row of an older generation",
            setup: &["UPDATE world SET generation = 1", "UPDATE hosts SET generation = 1 WHERE ordinal = 0"],
            verdict: |v| matches!(v, BootVerdict::Registered(HostBoot { generation: 2, bumped: true, .. })),
            generation: 2,
            prior_vouched: None,
        },
        Case {
            what: "a dirty live prior row, nobody alive",
            setup: &["UPDATE hosts SET seen_ms = 1 WHERE ordinal = 1"],
            verdict: |v| matches!(v, BootVerdict::Registered(HostBoot { generation: 1, bumped: true, .. })),
            generation: 1,
            prior_vouched: None,
        },
    ];
    each_backend("lease_verdicts", true, |url| {
        for case in &cases {
            raw_exec(url, "DELETE FROM hosts");
            raw_exec(url, "UPDATE world SET generation = 0");
            let h = open(url);
            boot_as(&h, 10, CrashPolicy::Rollback);
            h.register_host(row(1, 11), &Fence::default(), CrashPolicy::Rollback).unwrap();
            h.shutdown();
            for sql in case.setup.iter() {
                raw_exec(url, sql);
            }
            let h = open(url);
            let before = h.hosts().unwrap().into_iter().find(|r| r.ordinal == 0);
            let verdict = boot_as(&h, 20, CrashPolicy::Succession);
            assert!((case.verdict)(&verdict), "{}: {verdict:?}", case.what);
            assert_eq!(h.world_generation().unwrap(), case.generation, "{}: the world generation", case.what);
            let prior = h.hosts().unwrap().into_iter().find(|r| r.ordinal == 0).expect("host 0's row");
            match case.prior_vouched {
                None => assert_eq!((prior.boot, prior.state), (20, HOST_LIVE), "{}: registered", case.what),
                Some(vouched) => {
                    let before = before.expect("a prior row");
                    assert_eq!(prior.boot, 10, "{}: the prior row stays", case.what);
                    let seen = if vouched { 0 } else { before.seen_ms };
                    assert_eq!(prior.seen_ms, seen, "{}: the prior row's seen_ms", case.what);
                }
            }
            h.shutdown();
        }
    });
}

/// `Rollback` is R3's rule whatever the lease says: a dirty boot at the world's generation
/// raises it once (another host alive or not, the prior row dead or not), the others adopt.
#[test]
fn rollback_policy_keeps_the_r3_rule() {
    each_backend("lease_rollback", true, |url| {
        let h = open(url);
        let reg = |ordinal: u32, boot: u64| -> HostBoot {
            match h.register_host(row(ordinal, boot), &Fence::default(), CrashPolicy::Rollback).unwrap() {
                BootVerdict::Registered(b) => b,
                other => panic!("Rollback never waits: {other:?}"),
            }
        };
        assert!(!reg(0, 1).bumped);
        assert!(!reg(1, 2).bumped);
        // A crashed and reboots while B is alive: its row was dirty at the world's generation.
        let again = reg(0, 3);
        assert_eq!((again.generation, again.bumped), (1, true));
        // B restarts because it saw generation 1: its row says 0, so it adopts.
        let adopted = reg(1, 4);
        assert_eq!((adopted.generation, adopted.bumped), (1, false));
        assert_eq!(h.world_generation().unwrap(), 1);
        h.shutdown();
        // A row marked dead changes nothing under Rollback: a second crash of B bumps again.
        raw_exec(url, "UPDATE hosts SET state = 1 WHERE ordinal = 1");
        let h = open(url);
        match h.register_host(row(1, 5), &Fence::default(), CrashPolicy::Rollback).unwrap() {
            BootVerdict::Registered(b) => assert_eq!((b.generation, b.bumped), (2, true)),
            other => panic!("Rollback never waits: {other:?}"),
        }
        let rows = h.hosts().unwrap();
        assert_eq!(rows.iter().map(|r| (r.ordinal, r.generation)).collect::<Vec<_>>(), vec![(0, 1), (1, 2)]);
        h.shutdown();
    });
}

/// Every host crashed (every dirty row lapsed): the first boot raises the generation, once;
/// the next resumes at it.
#[test]
fn total_crash_bumps_once_and_everyone_resumes() {
    each_backend("lease_total_crash", true, |url| {
        let h = open(url);
        h.register_host(row(0, 10), &Fence::default(), CrashPolicy::Rollback).unwrap();
        h.register_host(row(1, 11), &Fence::default(), CrashPolicy::Rollback).unwrap();
        h.shutdown();
        raw_exec(url, "UPDATE hosts SET seen_ms = 1");
        let h = open(url);
        let a = h.register_host(row(0, 20), &Fence::default(), CrashPolicy::Succession).unwrap();
        assert!(matches!(a, BootVerdict::Registered(HostBoot { generation: 1, bumped: true, .. })), "{a:?}");
        let b = h.register_host(row(1, 21), &Fence::default(), CrashPolicy::Succession).unwrap();
        assert!(matches!(b, BootVerdict::Registered(HostBoot { generation: 1, bumped: false, .. })), "{b:?}");
        assert_eq!(h.world_generation().unwrap(), 1);
        h.shutdown();
    });
}

/// R4 (one host under `claim`): the host's crash policy is `Succession` although its
/// coordinator runs `Rollback` (one host: nobody to inherit). A dirty reboot of the lone
/// host — its own row beat a moment ago — finds no OTHER live host: it raises the generation
/// and registers at once, never waiting for a declaration nobody will make.
#[test]
fn a_lone_claim_host_restarts_after_a_crash_without_waiting() {
    each_backend("lease_lone_claim", false, |url| {
        let h = open(url);
        assert!(matches!(boot_as(&h, 10, CrashPolicy::Succession), BootVerdict::Registered(HostBoot { bumped: false, .. })));
        // It crashed right after a beat: its row is dirty, live and fresh.
        assert!(h.heartbeat(0, 10).unwrap());
        let started = std::time::Instant::now();
        let again = boot_as(&h, 20, CrashPolicy::Succession);
        assert!(matches!(again, BootVerdict::Registered(HostBoot { generation: 1, bumped: true, .. })), "{again:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "registered at once");
        let rows = h.hosts().unwrap();
        assert_eq!(rows.iter().map(|r| (r.ordinal, r.boot, r.state, r.clean)).collect::<Vec<_>>(), vec![(0, 20, HOST_LIVE, false)]);
        h.shutdown();
    });
}

/// Fix round 2: an offline retirement raises the generation in the same transaction that marks
/// the retired row dead, so that row is of an OLDER generation from then on — its next boot must
/// still wait for the open succession naming it (under any crash policy), writing nothing; after
/// the heir's settle it registers owning nothing; after a rollback (the succession abandoned) it
/// registers owning nothing and reads no in-flight row (they went to the heir).
#[test]
fn a_retired_ordinal_waits_for_its_succession_whatever_the_generation() {
    use crate::succession::{World, boot_of, in_flight, part, row as host_row};
    use game::{CutId, RegionCoords};
    for settle in [true, false] {
        let Some(w) = World::new(2) else { return };
        let rows1 = vec![in_flight(RegionCoords::new(1, 5))];
        w.hosts[0].prepare_cut(part(1, 0, vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
        w.hosts[1].prepare_cut(part(1, 1, rows1.clone()), w.fences[1].clone(), 0).recv().unwrap().unwrap();
        assert!(w.h.decide_cut(CutId(1), 100, true, vec![0, 1], vec![], w.c, 0, vec![]).unwrap());
        for h in &w.hosts {
            assert_eq!(h.cut_decided(CutId(1)).unwrap(), Some(true));
        }
        crate::two_phase::exec(&w.w, &format!("UPDATE hosts SET seen_ms = seen_ms - {}", 2 * game::HOST_LEASE_MS));
        let s = match w.h.retire_offline(1).unwrap() {
            world_store::Retired::Opened(s) => s,
            other => panic!("{other:?}"),
        };
        assert_eq!(w.h.world_generation().unwrap(), 1);
        let before = w.h.hosts().unwrap();
        for policy in [CrashPolicy::Succession, CrashPolicy::Rollback] {
            let v = w.hosts[1].register_host(HostRow { boot: 900, ..host_row(1) }, &Fence::default(), policy).unwrap();
            assert_eq!(v, BootVerdict::Wait(WaitFor::Succession), "settle {settle}, {policy:?}: the retired ordinal waits");
        }
        assert_eq!(w.h.hosts().unwrap(), before, "nothing written");
        assert_eq!(w.h.world_generation().unwrap(), 1);
        if settle {
            let heir = w.h.owned_by(0).unwrap();
            let adopting = game::WorldCut { adopts: vec![1], ..part(2, 0, rows1.clone()) };
            w.hosts[0].prepare_cut(adopting, heir, 1).recv().unwrap().unwrap();
            assert!(w.h.decide_cut(CutId(2), 200, true, vec![0], vec![], w.c, 1, vec![s.id]).unwrap());
            assert_eq!(w.hosts[0].cut_decided(CutId(2)).unwrap(), Some(true));
        } else {
            w.h.rollback_world_operator().unwrap();
        }
        let v = w.hosts[1].register_host(HostRow { boot: 901, ..host_row(1) }, &Fence::default(), CrashPolicy::Succession).unwrap();
        assert!(matches!(v, BootVerdict::Registered(_)), "settle {settle}: {v:?}");
        assert!(w.h.owned_by(1).unwrap().epochs.is_empty(), "settle {settle}: it owns nothing");
        assert!(w.h.load_in_flight(Some(1)).unwrap().is_empty(), "settle {settle}: no in-flight row to restore");
        assert_ne!(boot_of(1), 901);
        w.shutdown();
    }
}
