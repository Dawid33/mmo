//! The host registry (Elastic 1): `register_host` issues every host a new id from
//! `world.next_host_id` and never issues one twice; a new host owns nothing and never waits.
//! The first registration after a total crash takes every dirty row's land and in-flight rows
//! and raises `world.generation`; a `static` registration beside a live row is refused; a
//! registration lapses the older dirty rows of its name; rows nothing refers to are pruned.

use crate::common::hid;
use game::{CrashPolicy, CutId, HostId, InFlightRow, RegionCoords, WorldCut};
use world_store::{HOST_DEAD, HOST_ID_MAX, HOST_LIVE, NewHost, Registered, StoreError, StoreHandle, StoreUrl};

use crate::codec::intent;
use crate::handle::{TEST_SUPER_REGIONS, each_backend, open, raw_exec};
use crate::stud_refusal::{old_store, scalar};

/// A process named `name` (boot `boot`) about to register.
pub(crate) fn new_host(name: &str, boot: u64) -> NewHost {
    NewHost {
        name: name.to_string(),
        boot,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    }
}

/// Registers `name` under `policy`.
fn reg(h: &StoreHandle, name: &str, boot: u64, policy: CrashPolicy) -> Registered {
    h.register_host(new_host(name, boot), policy).unwrap()
}

/// The ids of the stored rows.
fn ids(h: &StoreHandle) -> Vec<HostId> {
    h.hosts().unwrap().into_iter().map(|r| r.id).collect()
}

fn next_host_id(url: &StoreUrl) -> i64 {
    scalar(url, "SELECT next_host_id FROM world WHERE id = 1").expect("the world row")
}

/// An in-flight row of `host` aimed at `rc`: a cut that commits only it (and the world tick),
/// fenced by `fence`.
fn in_flight_cut(id: u64, host: HostId, rc: RegionCoords) -> WorldCut {
    WorldCut {
        id: CutId(id),
        host,
        tick: 100 * id,
        regions: vec![],
        in_flight: vec![InFlightRow::Intent { target: rc, msg: intent() }],
        players: vec![],
        history: None,
        adopts: vec![],
        releases: vec![],
    }
}

#[test]
fn ids_are_issued_in_order_from_the_world_row() {
    each_backend("hosts_ids", false, |url| {
        let h = open(url);
        assert_eq!(h.world_generation().unwrap(), 0);
        let a = reg(&h, "a", u64::MAX - 5, CrashPolicy::Succession);
        let b = reg(&h, "b", 7, CrashPolicy::Succession);
        assert_eq!((a.id, a.generation, a.bumped), (hid(1), 0, false));
        assert_eq!((b.id, b.generation, b.bumped), (hid(2), 0, false));
        let rows = h.hosts().unwrap();
        assert_eq!(
            rows.iter().map(|r| (r.id, r.name.as_str(), r.boot, r.generation, r.clean, r.state)).collect::<Vec<_>>(),
            vec![(hid(1), "a", u64::MAX - 5, 0, false, HOST_LIVE), (hid(2), "b", 7, 0, false, HOST_LIVE)]
        );
        assert!(rows.iter().all(|r| r.drain_in_ms.is_none()), "nobody drains");
        let now = h.db_now_ms().unwrap();
        for r in &rows {
            assert!(r.seen_ms.abs_diff(now) < 5_000, "seen_ms {} vs db now {now}", r.seen_ms);
        }
        assert!(h.owned_by(a.id).unwrap().epochs.is_empty(), "a new host owns nothing");
        h.shutdown();
    });
}

#[test]
fn an_id_is_never_issued_twice_after_a_prune() {
    each_backend("hosts_prune", true, |url| {
        let h = open(url);
        let a = reg(&h, "a", 1, CrashPolicy::Succession);
        h.shutdown();
        // A left clean and owns nothing: the next registration prunes its row.
        raw_exec(url, &format!("UPDATE hosts SET clean = 1 WHERE id = {}", a.id.0));
        let h = open(url);
        let b = reg(&h, "b", 2, CrashPolicy::Succession);
        assert_eq!(ids(&h), vec![b.id], "A's row is gone");
        let c = reg(&h, "c", 3, CrashPolicy::Succession);
        assert!(c.id > b.id && b.id > a.id, "{a:?} {b:?} {c:?}");
        h.shutdown();
    });
}

/// The bound exists because the in-flight key packs the host id into the high 32 bits of a
/// signed 64-bit `seq`: an id above [`HOST_ID_MAX`] is never issued, and the refusal writes
/// nothing.
#[test]
fn an_id_above_the_bound_is_refused() {
    each_backend("hosts_bound", true, |url| {
        open(url).shutdown();
        raw_exec(url, &format!("UPDATE world SET next_host_id = {}", HOST_ID_MAX + 1));
        let h = open(url);
        let err = h.register_host(new_host("a", 1), CrashPolicy::Succession).unwrap_err();
        assert!(matches!(&err, StoreError::Corrupt(text) if text.contains(&HOST_ID_MAX.to_string())), "{err}");
        assert!(h.hosts().unwrap().is_empty(), "no row written");
        h.shutdown();
        assert_eq!(next_host_id(url), (HOST_ID_MAX + 1) as i64, "nothing written");
        // The bound itself is an id.
        raw_exec(url, &format!("UPDATE world SET next_host_id = {HOST_ID_MAX}"));
        let h = open(url);
        assert_eq!(reg(&h, "a", 1, CrashPolicy::Succession).id, HostId(HOST_ID_MAX));
        h.shutdown();
    });
}

#[test]
fn a_migrated_world_keeps_its_rows_and_issues_above_them() {
    each_backend("hosts_migrated", true, |url| {
        let row = |ordinal: u64| {
            format!(
                "INSERT INTO hosts (ordinal, boot, generation, clean, peer_addr, peer_cert, quic, webtransport, \
                 wt_cert_hash, updated_utc, state, seen_ms, reg) \
                 VALUES ({ordinal}, {}, 0, 1, NULL, NULL, NULL, NULL, NULL, '2026-11-01T00:00:00Z', 0, 1, {})",
                100 + ordinal,
                ordinal + 1
            )
        };
        let (sx, sz) = TEST_SUPER_REGIONS[0];
        let land = format!("INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ({sx}, {sz}, 1, 4)");
        old_store(url, 13, "0", &[row(0), row(1), land]);
        let h = open(url);
        assert_eq!(ids(&h), vec![hid(0), hid(1)], "the rows keep their numbers as ids");
        assert!(h.hosts().unwrap().iter().all(|r| r.name.is_empty() && r.drain_in_ms.is_none()));
        assert_eq!(h.owned_by(hid(1)).unwrap().epochs, [((sx, sz), 4)].into_iter().collect(), "the owner is unchanged");
        let next = reg(&h, "new", 9, CrashPolicy::Succession);
        assert_eq!((next.id, next.bumped), (hid(2), false));
        // Host 0 (clean, owning nothing) is pruned; host 1 keeps its row and its land.
        assert_eq!(ids(&h), vec![hid(1), hid(2)]);
        assert_eq!(h.owned_by(hid(1)).unwrap().epochs, [((sx, sz), 4)].into_iter().collect());
        h.shutdown();
    });
}

/// Review focus 1: a restarted process registers at once under a new id beside its old id's
/// open succession; it owns nothing, and the succession (and the old row it names) is untouched.
#[test]
fn a_restarted_host_registers_at_once_beside_its_open_succession() {
    let Some(w) = crate::succession::World::new(2) else { return };
    w.lapse(hid(1));
    let s = w.declared(hid(1), hid(0));
    let before = w.h.open_successions().unwrap();
    assert_eq!(before, vec![s]);
    let started = std::time::Instant::now();
    let back = w.hosts[1].register_host(new_host("host-1", 9001), CrashPolicy::Succession).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "registered at once");
    assert_eq!((back.id, back.bumped), (hid(2), false));
    assert!(w.h.owned_by(back.id).unwrap().epochs.is_empty(), "the new host owns nothing");
    assert_eq!(w.h.open_successions().unwrap(), before, "the succession is unchanged");
    assert_eq!(w.state_of(hid(1)), (HOST_DEAD, false), "the old row stays, dead and dirty");
    w.shutdown();
}

#[test]
fn the_first_host_after_a_total_crash_takes_every_dirty_row() {
    each_backend("hosts_total_crash", true, |url| {
        let h = open(url);
        let a = reg(&h, "a", 1, CrashPolicy::Succession);
        let b = reg(&h, "b", 2, CrashPolicy::Succession);
        let fa = h.claim_super_regions(a.id, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        let fb = h.claim_super_regions(b.id, vec![TEST_SUPER_REGIONS[1], TEST_SUPER_REGIONS[2]]).unwrap();
        h.commit_cut(in_flight_cut(1, a.id, RegionCoords::new(0, 0)), fa.clone()).recv().unwrap().unwrap();
        h.commit_cut(in_flight_cut(2, b.id, RegionCoords::new(1, -1)), fb.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_in_flight(Some(a.id)).unwrap().len(), 1);
        assert_eq!(h.load_in_flight(Some(b.id)).unwrap().len(), 1);
        h.shutdown();
        // Both leases lapsed: nobody is live.
        raw_exec(url, "UPDATE hosts SET seen_ms = 1");
        let h = open(url);
        let c = reg(&h, "c", 3, CrashPolicy::Succession);
        assert_eq!((c.id, c.generation, c.bumped), (hid(3), 1, true));
        assert_eq!(h.world_generation().unwrap(), 1);
        let mut want: std::collections::BTreeMap<_, _> = fa.epochs.iter().map(|(sr, e)| (*sr, e + 1)).collect();
        want.extend(fb.epochs.iter().map(|(sr, e)| (*sr, e + 1)));
        assert_eq!(h.owned_by(c.id).unwrap().epochs, want, "every dirty row's land, at epoch + 1");
        assert!(h.owned_by(a.id).unwrap().epochs.is_empty() && h.owned_by(b.id).unwrap().epochs.is_empty());
        assert_eq!(h.load_in_flight(Some(c.id)).unwrap().len(), 2, "both in-flight rows are C's");
        assert!(h.load_in_flight(Some(a.id)).unwrap().is_empty() && h.load_in_flight(Some(b.id)).unwrap().is_empty());
        // A and B were marked dead and, owning nothing, pruned in the same transaction: their
        // old processes are refused at their next beat.
        assert_eq!(ids(&h), vec![c.id]);
        assert!(!h.heartbeat(a.id, 1, Default::default()).unwrap().live && !h.heartbeat(b.id, 2, Default::default()).unwrap().live);
        // The old fences are refused.
        let err = h.commit_cut(in_flight_cut(3, a.id, RegionCoords::new(0, 0)), fa).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Fenced { .. }), "{err}");
        let d = reg(&h, "d", 4, CrashPolicy::Succession);
        assert_eq!((d.id, d.generation, d.bumped), (hid(4), 1, false));
        assert!(h.owned_by(d.id).unwrap().epochs.is_empty(), "D owns nothing");
        h.shutdown();
    });
}

/// The total-crash rule under `static` too (ruling 4): no live row, a dirty one → taken.
#[test]
fn a_static_host_after_a_crash_takes_the_dirty_row() {
    each_backend("hosts_static_crash", true, |url| {
        let h = open(url);
        let a = reg(&h, "a", 1, CrashPolicy::Rollback);
        let fa = h.claim_super_regions(a.id, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        h.shutdown();
        raw_exec(url, "UPDATE hosts SET seen_ms = 1");
        let h = open(url);
        let b = reg(&h, "a", 2, CrashPolicy::Rollback);
        assert_eq!((b.id, b.generation, b.bumped), (hid(2), 1, true));
        let want: std::collections::BTreeMap<_, _> = fa.epochs.iter().map(|(sr, e)| (*sr, e + 1)).collect();
        assert_eq!(h.owned_by(b.id).unwrap().epochs, want);
        h.shutdown();
    });
}

#[test]
fn a_clean_stop_keeps_the_leavers_land() {
    each_backend("hosts_clean_stop", true, |url| {
        let h = open(url);
        let a = reg(&h, "a", 1, CrashPolicy::Succession);
        let fa = h.claim_super_regions(a.id, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        h.shutdown();
        raw_exec(url, &format!("UPDATE hosts SET clean = 1, seen_ms = 1 WHERE id = {}", a.id.0));
        let h = open(url);
        let b = reg(&h, "b", 2, CrashPolicy::Succession);
        assert_eq!((b.generation, b.bumped), (0, false));
        assert_eq!(h.owned_by(a.id).unwrap(), fa, "the leaver keeps its land");
        assert_eq!(ids(&h), vec![a.id, b.id], "a row that owns land is not pruned");
        h.shutdown();
    });
}

#[test]
fn a_static_host_beside_a_live_row_is_refused() {
    each_backend("hosts_not_alone", true, |url| {
        let h = open(url);
        let a = reg(&h, "a", 1, CrashPolicy::Succession);
        h.shutdown();
        let next = next_host_id(url);
        let h = open(url);
        let err = h.register_host(new_host("b", 2), CrashPolicy::Rollback).unwrap_err();
        assert_eq!(err, StoreError::NotAlone { live: vec![a.id] });
        assert!(err.to_string().contains("VOXEL_OWNERSHIP=claim"), "{err}");
        assert_eq!(ids(&h), vec![a.id], "nothing written");
        h.shutdown();
        assert_eq!(next_host_id(url), next, "no id issued: the transaction rolled back");
    });
}

/// A restarted process beside another live host: its old row (same name) lapses and nothing
/// else — it is not a total crash (B is live), the new host owns nothing, A keeps its land
/// until the coordinator declares it; B is untouched. An empty name vouches for nothing.
#[test]
fn a_same_named_registration_lapses_the_old_row() {
    each_backend("hosts_same_name", false, |url| {
        let h = open(url);
        let a = reg(&h, "a", 1, CrashPolicy::Succession);
        let b = reg(&h, "b", 2, CrashPolicy::Succession);
        let fa = h.claim_super_regions(a.id, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        let seen = |id: HostId| h.hosts().unwrap().into_iter().find(|r| r.id == id).unwrap().seen_ms;
        let b_seen = seen(b.id);
        let again = reg(&h, "a", 3, CrashPolicy::Succession);
        assert_eq!((again.id, again.generation, again.bumped), (hid(3), 0, false));
        assert!(h.owned_by(again.id).unwrap().epochs.is_empty(), "the new host owns nothing");
        assert_eq!(h.owned_by(a.id).unwrap(), fa, "A's land is still A's");
        assert_eq!(seen(a.id), 0, "the older row named a lapsed");
        assert_eq!(seen(b.id), b_seen, "a row of another name is untouched");
        assert_ne!(seen(again.id), 0, "the new row is seen now");
        // An empty name vouches for nothing.
        let (b_seen, again_seen) = (seen(b.id), seen(again.id));
        reg(&h, "", 4, CrashPolicy::Succession);
        reg(&h, "", 5, CrashPolicy::Succession);
        assert_eq!((seen(b.id), seen(again.id)), (b_seen, again_seen));
        assert!(h.hosts().unwrap().iter().filter(|r| r.name.is_empty()).all(|r| r.seen_ms != 0));
        h.shutdown();
    });
}

/// A lone host named `a` (live, dirty, its lease NOT lapsed, owning land and an in-flight row)
/// restarted under `policy` by a process of the same name: the vouch comes first, so its old
/// row is not live and the registration is a total crash — the new id takes the land at
/// `epoch + 1` and the in-flight row; the old row is gone.
fn lone_named_restart_takes_its_land_back(url: &StoreUrl, policy: CrashPolicy) {
    let h = open(url);
    let a = reg(&h, "a", 1, policy);
    let fa = h.claim_super_regions(a.id, vec![TEST_SUPER_REGIONS[0]]).unwrap();
    h.commit_cut(in_flight_cut(1, a.id, RegionCoords::new(0, 0)), fa.clone()).recv().unwrap().unwrap();
    assert!(h.heartbeat(a.id, 1, Default::default()).unwrap().live, "A's lease is fresh");
    let back = h.register_host(new_host("a", 2), policy).unwrap();
    assert_eq!((back.id, back.generation, back.bumped), (hid(2), 1, true), "{policy:?}");
    let want: std::collections::BTreeMap<_, _> = fa.epochs.iter().map(|(sr, e)| (*sr, e + 1)).collect();
    assert_eq!(h.owned_by(back.id).unwrap().epochs, want, "{policy:?}: A's land, epoch + 1");
    assert_eq!(h.load_in_flight(Some(back.id)).unwrap().len(), 1, "{policy:?}: A's in-flight row");
    assert_eq!(ids(&h), vec![back.id], "{policy:?}: A's row is gone");
    assert!(!h.heartbeat(a.id, 1, Default::default()).unwrap().live, "{policy:?}: the old process is replaced");
    h.shutdown();
}

#[test]
fn a_lone_named_host_restarted_within_the_lease_takes_its_land_back() {
    each_backend("hosts_lone_named", false, |url| lone_named_restart_takes_its_land_back(url, CrashPolicy::Succession));
}

#[test]
fn a_static_host_restarted_under_its_name_is_not_refused() {
    each_backend("hosts_static_named", false, |url| lone_named_restart_takes_its_land_back(url, CrashPolicy::Rollback));
}

/// Without a name nothing is vouched: a `static` restart beside its own still-fresh old row
/// is refused (why a host needs a name to restart at once).
#[test]
fn an_unnamed_static_restart_within_the_lease_is_refused() {
    each_backend("hosts_static_unnamed", false, |url| {
        let h = open(url);
        let a = reg(&h, "", 1, CrashPolicy::Rollback);
        let err = h.register_host(new_host("", 2), CrashPolicy::Rollback).unwrap_err();
        assert_eq!(err, StoreError::NotAlone { live: vec![a.id] });
        assert_eq!(ids(&h), vec![a.id]);
        h.shutdown();
    });
}

/// A row an open succession names — predecessor, heir or survivor — is kept although it is
/// dead or clean and owns nothing; once the succession is over, the next registration prunes it.
#[test]
fn a_row_named_by_an_open_succession_is_not_pruned() {
    use crate::two_phase::exec;
    let Some(w) = crate::succession::World::new(4) else { return };
    w.vouch(hid(1));
    let s = w.declared(hid(1), hid(0));
    assert_eq!(s.survivors, vec![hid(0), hid(2), hid(3)]);
    // The heir (0) and a survivor (2) look clean and landless, the predecessor (1) is dead and
    // landless: only the open succession names them. Host 3 runs on.
    exec(&w.w, "UPDATE super_regions SET owner = -1 WHERE owner IN (0, 2)");
    exec(&w.w, "UPDATE hosts SET clean = 1 WHERE id IN (0, 2)");
    let first = w.h.register_host(new_host("x", 1), CrashPolicy::Succession).unwrap();
    assert!(!first.bumped);
    assert_eq!(ids(&w.h), vec![hid(0), hid(1), hid(2), hid(3), first.id], "nothing pruned while the succession is open");
    // Abandoned (a world rollback): the next registration prunes all three.
    w.h.rollback_world_operator().unwrap();
    let second = w.h.register_host(new_host("y", 2), CrashPolicy::Succession).unwrap();
    let left = ids(&w.h);
    assert!(left.iter().all(|id| ![hid(0), hid(1), hid(2)].contains(id)), "{left:?}");
    assert!(left.contains(&second.id));
    w.shutdown();
}
