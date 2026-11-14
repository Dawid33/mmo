//! `worldctl` (`world_store::ctl`, the logic `src/bin/worldctl.rs` wraps):
//! `forge` forges in process into a store as tiles, `export` writes the
//! archived forge file back out, and a store another process holds is refused
//! with the lock message instead of waiting for it.

use world_store::{StoreHandle, StoreUrl, ctl, load_forged};

use crate::handle::open;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mmo_ws_ctl_{tag}_{}_{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A small forge (2 km / 64 m) through the CLI's own argument parsing: the
/// store then serves it, and `export` gives back a file that loads as the
/// same world.
#[test]
fn worldctl_forge_writes_store() {
    let dir = temp_dir("forge");
    let db = dir.join("w.sqlite");
    let db_arg = db.to_str().unwrap();
    ctl::run(&args(&["forge", "--db", db_arg, "--size", "2048", "--cell", "64", "--seed", "4", "--set", "iters=50"]))
        .expect("forged");
    let url = StoreUrl::Sqlite(db.clone());
    let h = open(&url);
    let (world, map) = load_forged(&h).unwrap().expect("a tiled world in the store");
    assert!(world.tile_cache().is_some());
    assert_eq!(world.globals().size_m, 2048.0);
    assert_eq!(world.globals().cell_m, 64.0);
    assert_eq!(world.globals().seed, 4);
    assert!(map.width > 0 && map.height > 0);
    // Held by this handle: a writing worldctl is refused at once.
    let err = ctl::run(&args(&["retile", "--db", db_arg])).err().expect("locked");
    assert!(err.contains("already in use"), "{err}");
    // Export only reads (read-only open): it works beside the holder. It
    // gives the archive row, byte for byte; it loads as a forge file.
    let out = dir.join("out.world");
    let archive = h.forged_archive().unwrap().expect("the archive row");
    ctl::run(&args(&["export", "--db", db_arg, out.to_str().unwrap()])).expect("exported");
    assert_eq!(std::fs::read(&out).unwrap(), archive);
    // A second forge into a store that holds a world is refused too (once free).
    drop(world);
    h.shutdown();
    let err = ctl::run(&args(&["forge", "--db", db_arg, "--size", "2048", "--cell", "64"])).err().expect("refused");
    assert!(err.contains("already holds a world"), "{err}");
    let file = worldgen::forged::ForgedWorld::load(&out).unwrap();
    assert_eq!(file.globals().seed, 4);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn worldctl_rejects_bad_arguments() {
    for (list, want) in [
        (&["frobnicate"][..], "usage"),
        (&["forge"][..], "--db"),
        (&["forge", "--db", "x.sqlite", "--preset", "Nowhere"][..], "no preset named"),
        (&["forge", "--db", "x.sqlite", "--set", "nope=1"][..], "no knob named"),
        (&["forge", "--db", "x.sqlite", "--size"][..], "needs a value"),
        (&["export", "--db", "x.sqlite"][..], "usage"),
    ] {
        let err = ctl::run(&args(list)).err().unwrap_or_else(|| panic!("{list:?} accepted"));
        assert!(err.contains(want), "{list:?}: {err}");
    }
}

/// Export from a store without a forged world says so.
#[test]
fn worldctl_export_without_world() {
    let dir = temp_dir("empty");
    let db = dir.join("w.sqlite");
    StoreHandle::open(&StoreUrl::Sqlite(db.clone()), world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap().shutdown();
    let err = ctl::run(&args(&["export", "--db", db.to_str().unwrap(), dir.join("o.world").to_str().unwrap()]))
        .err()
        .expect("nothing to export");
    assert!(err.contains("no forged world"), "{err}");
    assert!(!dir.join("o.world").exists());
    let _ = std::fs::remove_dir_all(dir);
}

/// `super-region <url> <n>` sets the world's super-region side, but only on a world no
/// host has claimed yet (an empty `super_regions`): afterwards the fences already name
/// super-regions of the old side. A bad `n` is refused, and no message echoes the URL
/// (a Postgres URL carries a password).
#[test]
fn super_region_only_before_the_first_claim() {
    crate::handle::each_backend("ctl_super_region", true, |url| {
        let url_arg = match url {
            StoreUrl::Sqlite(path) => path.to_str().unwrap().to_string(),
            StoreUrl::Postgres(u) => u.clone(),
            StoreUrl::Memory => unreachable!("reopen runs on a file"),
        };
        let h = open(url);
        assert_eq!(h.super_region_regions(), game::DEFAULT_SUPER_REGION_REGIONS);
        h.shutdown();
        for bad in ["0", "-1", "many", ""] {
            let err = ctl::run(&args(&["super-region", &url_arg, bad])).err().unwrap_or_else(|| panic!("{bad:?} accepted"));
            assert!(err.contains(">= 1"), "{bad:?}: {err}");
            assert!(!err.contains(&url_arg), "the URL is echoed: {err}");
        }
        let err = ctl::run(&args(&["super-region", &url_arg])).err().expect("no n");
        assert!(err.contains("usage"), "{err}");
        ctl::run(&args(&["super-region", &url_arg, "1"])).expect("set on an unclaimed world");
        let h = open(url);
        assert_eq!(h.super_region_regions(), 1, "stored, and read back at open");
        // A host claims: the side is fixed from now on.
        h.claim_super_regions(0, vec![(0, 0)]).unwrap();
        h.shutdown();
        let err = ctl::run(&args(&["super-region", &url_arg, "4"])).err().expect("refused once claimed");
        assert!(err.contains("claimed"), "{err}");
        assert!(!err.contains(&url_arg), "the URL is echoed: {err}");
        let h = open(url);
        assert_eq!(h.super_region_regions(), 1, "unchanged");
        h.shutdown();
    });
}

/// Postgres has no file lock: a host may open the store, then `super-region` changes the
/// side before that host claims. The claim must not go through with super-regions of the
/// old side — it re-reads the side in its own transaction and is refused.
#[test]
fn super_region_refuses_a_claim_opened_at_the_old_side() {
    if crate::common::pg_admin_url().is_none() {
        return crate::common::skip_msg();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(crate::common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let url = StoreUrl::Postgres(db.url.clone());
    let early = open(&url);
    assert_eq!(early.super_region_regions(), game::DEFAULT_SUPER_REGION_REGIONS);
    ctl::run(&args(&["super-region", &db.url, "2"])).expect("nobody has claimed yet");
    let err = early.claim_super_regions(0, vec![(0, 0)]).err().expect("a claim at the old side is refused");
    assert!(matches!(err, world_store::StoreError::OtherSuperRegionSide { stored: 2, opened } if opened == game::DEFAULT_SUPER_REGION_REGIONS), "{err}");
    early.shutdown();
    // A process opened at the new side claims; from then on the side is fixed.
    let late = open(&url);
    assert_eq!(late.super_region_regions(), 2);
    late.claim_super_regions(0, vec![(0, 0)]).unwrap();
    let err = ctl::run(&args(&["super-region", &db.url, "1"])).err().expect("refused after a claim");
    assert!(err.contains("claimed"), "{err}");
    late.shutdown();
    drop(db);
}

/// No worldctl command shows a Postgres URL's credentials, on success or failure
/// (`Ok` is exactly what the binary prints; `Err` what it prints to stderr).
#[test]
fn worldctl_never_prints_the_postgres_password() {
    if crate::common::pg_admin_url().is_none() {
        return crate::common::skip_msg();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(crate::common::PgTestDb::create_async("mmo_test")).expect("pg env var set");
    let userinfo = db.url.split("://").nth(1).and_then(|rest| rest.split_once('@')).map(|(u, _)| u.to_string());
    let userinfo = userinfo.expect("the test URL carries user:password@");
    let dir = temp_dir("pg_redact");
    let out = dir.join("o.world");
    let out = out.to_str().unwrap();
    let missing = db.url.rsplit_once('/').map(|(base, _)| format!("{base}/mmo_no_such_database")).unwrap();
    let mut said = Vec::new();
    for list in [
        vec!["export", "--db", &db.url, out],
        vec!["retile", "--db", &db.url],
        vec!["clock-reset", &db.url],
        vec!["super-region", &db.url, "0"],
        vec!["forge", "--db", &db.url, "--size", "2048", "--cell", "64", "--seed", "4", "--set", "iters=50"],
        vec!["forge", "--db", &db.url, "--size", "2048", "--cell", "64"],
        vec!["retile", "--db", &db.url],
        vec!["export", "--db", &db.url, out],
        vec!["super-region", &db.url, "1"],
        vec!["clock-reset", &missing],
        vec!["super-region", &missing, "1"],
    ] {
        let result = ctl::run(&args(&list));
        let text = match &result {
            Ok(s) | Err(s) => s.clone(),
        };
        assert!(!text.contains(&userinfo), "{list:?} printed the credentials: {text}");
        said.push((list[0].to_string(), result.is_ok()));
    }
    // The run went through both paths of each command (so every message was looked at).
    for cmd in ["export", "retile", "forge", "clock-reset", "super-region"] {
        assert!(said.iter().any(|(c, ok)| c == cmd && *ok), "{cmd} never succeeded: {said:?}");
        assert!(said.iter().any(|(c, ok)| c == cmd && !*ok), "{cmd} never failed: {said:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
    drop(db);
}

// ---- R4 Task 14: `retire-host` and `rollback` ------------------------------------------------

use game::{CutId, RegionCoords};
use world_store::{Declared, HOST_DEAD, HOST_LIVE, SuccessionKind};

use crate::succession::{World, boot_of, in_flight, part};
use crate::two_phase::{exec, prepared, scalar_i64};

/// The `postgres://` URL of a test world, as an operator passes it.
fn url_of(w: &World) -> String {
    match &w.w.url {
        StoreUrl::Postgres(u) => u.clone(),
        other => unreachable!("a Postgres world: {other:?}"),
    }
}

/// `user:password` of a test URL: no command may print it.
fn credentials(url: &str) -> String {
    url.split("://").nth(1).and_then(|rest| rest.split_once('@')).map(|(u, _)| u.to_string()).expect("user:password@")
}

/// `ctl::run`'s text either way, checked never to show the credentials.
fn said(url: &str, list: &[&str]) -> Result<String, String> {
    let result = ctl::run(&args(list));
    let text = match &result {
        Ok(s) | Err(s) => s.clone(),
    };
    assert!(!text.contains(&credentials(url)), "{list:?} printed the credentials: {text}");
    result
}

/// Every host of the world lapsed (their last beats two leases back): nobody is alive.
fn lapse_all(w: &World) {
    exec(&w.w, &format!("UPDATE hosts SET seen_ms = seen_ms - {}", 2 * game::HOST_LEASE_MS));
}

/// A host that beat within the lease is running: `retire-host` refuses it (nothing written),
/// whether other hosts run or not — and an ordinal with no row, or a bad one, is refused too.
#[test]
fn retire_refuses_a_heartbeating_host() {
    let Some(w) = World::new(2) else { return };
    let url = url_of(&w);
    let err = said(&url, &["retire-host", &url, "1"]).expect_err("host 1 beats");
    assert!(err.contains("host 1 is running: stop it first"), "{err}");
    // Host 0 lapsed: host 1 still beats — refused all the same.
    w.lapse(0);
    let err = said(&url, &["retire-host", &url, "1"]).expect_err("host 1 beats");
    assert!(err.contains("host 1 is running: stop it first"), "{err}");
    for (list, want) in [
        (vec!["retire-host", &url, "7"], "no hosts row"),
        (vec!["retire-host", &url, "x"], "ordinal"),
        (vec!["retire-host", &url], "usage"),
    ] {
        let err = said(&url, &list).expect_err("refused");
        assert!(err.contains(want), "{list:?}: {err}");
    }
    assert_eq!(w.state_of(1), (HOST_LIVE, false));
    assert!(w.h.hosts().unwrap().iter().all(|r| r.seen_ms != 0), "nothing vouched");
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();
}

/// With another host alive, `retire-host` only vouches for the lapsed one (`seen_ms = 0`):
/// the coordinator's next declaration takes it at once (no whole lease); a vouched host that
/// beats right after is no longer lapsed — the declaration re-checks and writes nothing.
#[test]
fn retire_vouches_while_others_live() {
    let Some(w) = World::new(3) else { return };
    let url = url_of(&w);
    // Host 2 lapsed, then vouched by the operator; it beats again (it was only slow).
    w.lapse(2);
    let out = said(&url, &["retire-host", &url, "2"]).expect("vouched");
    assert!(out.contains("vouched"), "{out}");
    assert_eq!(w.h.hosts().unwrap()[2].seen_ms, 0, "vouched: seen_ms = 0");
    assert!(w.h.open_successions().unwrap().is_empty(), "the command declares nothing itself");
    assert!(w.hosts[2].heartbeat(2, boot_of(2)).unwrap());
    assert_eq!(w.declare(2, 0, SuccessionKind::Crash).unwrap(), Declared::NotLapsed(vec![2]), "it beat: harmless");
    assert_eq!(w.state_of(2), (HOST_LIVE, false));
    // Running again: a second vouch is refused.
    let err = said(&url, &["retire-host", &url, "2"]).expect_err("host 2 beats");
    assert!(err.contains("host 2 is running: stop it first"), "{err}");
    // Host 1 is gone for good: vouched, and declared at the coordinator's next pass.
    w.lapse(1);
    said(&url, &["retire-host", &url, "1"]).expect("vouched");
    let s = w.declared(1, 0);
    assert_eq!((s.predecessor, s.heir, s.survivors.clone()), (1, 0, vec![0, 2]), "an ordinary declaration");
    assert_eq!(w.state_of(1), (HOST_DEAD, false));
    w.shutdown();
}

/// Nobody alive (every dirty row lapsed): `retire-host` opens the succession itself — kind
/// crash, heir the lowest other ordinal with a live `hosts` row, no survivors, based on the
/// last committed cut — marks the row dead and moves its land at `epoch + 1`, in one
/// transaction under the `world` row's lock. The heir reads the dead host's in-flight rows,
/// and only a commit that settles it commits. A dead row, or one an open succession names,
/// is refused.
#[test]
fn retire_offline_opens_a_succession_to_the_lowest_other_host() {
    let Some(w) = World::new(3) else { return };
    let url = url_of(&w);
    // A committed cut first (host 0 carries an in-flight row): the succession's base.
    let rows0 = vec![in_flight(RegionCoords::new(0, 9))];
    w.hosts[0].prepare_cut(part(1, 0, rows0.clone()), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, 1, vec![]), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    w.hosts[2].prepare_cut(part(1, 2, vec![]), w.fences[2].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![0, 1, 2], vec![], w.c, 0, vec![]).unwrap());
    for h in &w.hosts {
        assert_eq!(h.cut_decided(CutId(1)).unwrap(), Some(true));
    }
    // A staged part a crash left behind (cut 2, host 0's): the command resolves it first.
    w.hosts[0].prepare_cut(part(2, 0, vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    assert_eq!(prepared(&w.w), 1);
    lapse_all(&w);
    // Host 0 is retired: the lowest OTHER ordinal (1) inherits.
    let out = said(&url, &["retire-host", &url, "0"]).expect("retired offline");
    assert!(out.contains("heir 1"), "{out}");
    assert_eq!(prepared(&w.w), 0, "the staged part was resolved (aborted)");
    let open = w.h.open_successions().unwrap();
    assert_eq!(open.len(), 1);
    let s = &open[0];
    assert_eq!((s.predecessor, s.predecessor_boot, s.heir, s.kind), (0, boot_of(0), 1, SuccessionKind::Crash));
    assert_eq!(s.survivors, Vec::<u32>::new(), "nobody ran: no survivor");
    assert_eq!(s.base_cut, CutId(1), "the last committed cut");
    let mut want: Vec<_> = w.fences[0].epochs.iter().map(|(sr, e)| (*sr, e + 1)).collect();
    want.sort();
    assert_eq!(s.moved, want, "every super-region of host 0, epoch + 1");
    assert!(w.h.owned_by(0).unwrap().epochs.is_empty());
    let heir = w.h.owned_by(1).unwrap();
    for (sr, e) in &s.moved {
        assert_eq!(heir.epochs.get(sr), Some(e));
    }
    assert_eq!(w.state_of(0), (HOST_DEAD, false), "dead, dirty until settled");
    assert_eq!(w.hosts[1].load_in_flight_of(0, &heir).unwrap(), rows0, "the heir reads the dead host's rows");
    // Refused now: host 0 is dead already; host 1 is named by the open succession.
    let err = said(&url, &["retire-host", &url, "0"]).expect_err("dead already");
    assert!(err.contains("host 0"), "{err}");
    let err = said(&url, &["retire-host", &url, "1"]).expect_err("named by the open succession");
    assert!(err.contains("succession"), "{err}");
    assert_eq!(w.h.open_successions().unwrap().len(), 1);
    // The settle rule holds: only a commit naming it commits; it cleans host 0's row.
    let heir_part = |id: u64| game::WorldCut { adopts: vec![0], ..part(id, 1, rows0.clone()) };
    w.hosts[1].prepare_cut(part(3, 1, vec![]), heir.clone(), 0).recv().unwrap().unwrap();
    w.hosts[2].prepare_cut(part(3, 2, vec![]), w.fences[2].clone(), 0).recv().unwrap().unwrap();
    assert!(!w.h.decide_cut(CutId(3), 300, true, vec![1, 2], vec![], w.c, 0, vec![]).unwrap(), "settles nothing");
    for h in [1, 2] {
        assert_eq!(w.hosts[h].cut_decided(CutId(3)).unwrap(), Some(false));
    }
    w.hosts[1].prepare_cut(heir_part(4), heir.clone(), 0).recv().unwrap().unwrap();
    w.hosts[2].prepare_cut(part(4, 2, vec![]), w.fences[2].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(4), 400, true, vec![1, 2], vec![], w.c, 0, vec![s.id]).unwrap());
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(w.state_of(0), (HOST_DEAD, true));
    assert_eq!(w.h.load_in_flight(Some(1)).unwrap(), rows0, "adopted under the heir");
    assert!(w.h.load_in_flight(Some(0)).unwrap().is_empty());
    assert_eq!(prepared(&w.w), 0);
    w.shutdown();
}

/// Nobody alive and no other `hosts` row to inherit: refused, nothing written. A SQLite
/// world (one host) has no hosts to retire to either.
#[test]
fn retire_offline_with_no_other_host_is_refused() {
    let Some(w) = World::new(2) else { return };
    let url = url_of(&w);
    exec(&w.w, "DELETE FROM hosts WHERE ordinal = 0");
    lapse_all(&w);
    let err = said(&url, &["retire-host", &url, "1"]).expect_err("nobody to inherit");
    assert!(
        err.contains("nothing to give host 1's land to: start a host of another ordinal once, or start host 1"),
        "{err}"
    );
    assert_eq!(w.state_of(1), (HOST_LIVE, false));
    assert_eq!(w.h.owned_by(1).unwrap(), w.fences[1], "nothing moved");
    assert!(w.h.open_successions().unwrap().is_empty());
    w.shutdown();
    let dir = temp_dir("retire_sqlite");
    let db = dir.join("w.sqlite");
    StoreHandle::open(&StoreUrl::Sqlite(db.clone()), world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap().shutdown();
    let err = ctl::run(&args(&["retire-host", db.to_str().unwrap(), "1"])).expect_err("SQLite");
    assert!(err.contains("Postgres"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

/// `rollback` is refused without `--yes`; with it the world generation goes up exactly once,
/// every open succession is abandoned (the land stays with its heir; the predecessor's
/// in-flight rows go to the heir, who restarts owning the land), and it is learned by the
/// running processes at their next store write: a host's part at the old generation and the
/// coordinator's decision at it are refused (`StaleGeneration` → the host exits 5), while the
/// coordinator's fence still holds.
#[test]
fn rollback_needs_yes_and_bumps_once() {
    let Some(w) = World::new(2) else { return };
    let url = url_of(&w);
    let rows1 = vec![in_flight(RegionCoords::new(1, 5))];
    w.hosts[0].prepare_cut(part(1, 0, vec![]), w.fences[0].clone(), 0).recv().unwrap().unwrap();
    w.hosts[1].prepare_cut(part(1, 1, rows1.clone()), w.fences[1].clone(), 0).recv().unwrap().unwrap();
    assert!(w.h.decide_cut(CutId(1), 100, true, vec![0, 1], vec![], w.c, 0, vec![]).unwrap());
    assert_eq!(w.hosts[0].cut_decided(CutId(1)).unwrap(), Some(true));
    w.vouch(1);
    let s = w.declared(1, 0);
    let err = said(&url, &["rollback", &url]).expect_err("needs --yes");
    assert!(err.contains("--yes") && err.contains("every host restarts from the last save"), "{err}");
    assert_eq!(w.h.world_generation().unwrap(), 0, "nothing written");
    assert_eq!(w.h.open_successions().unwrap().len(), 1);
    let out = said(&url, &["rollback", &url, "--yes"]).expect("rolled back");
    assert!(out.contains("generation 1"), "{out}");
    assert_eq!(w.h.world_generation().unwrap(), 1, "once");
    assert!(w.h.open_successions().unwrap().is_empty());
    assert_eq!(scalar_i64(&w.w, &format!("SELECT abandoned FROM successions WHERE id = {}", s.id.0)), 1);
    let heir = w.h.owned_by(0).unwrap();
    for (sr, e) in &s.moved {
        assert_eq!(heir.epochs.get(sr), Some(e), "the land stays with the heir");
    }
    assert!(w.h.load_in_flight(Some(1)).unwrap().is_empty(), "no row left under the dead host");
    assert_eq!(w.h.load_in_flight(Some(0)).unwrap(), rows1, "its rows are the heir's");
    // The running processes learn it at their next write.
    let err = w.hosts[0].prepare_cut(part(2, 0, vec![]), heir.clone(), 0).recv().unwrap().unwrap_err();
    assert_eq!(err, world_store::StoreError::StaleGeneration { stored: 1, mine: 0 });
    let err = w.h.decide_cut(CutId(2), 200, true, vec![0], vec![], w.c, 0, vec![]).unwrap_err();
    assert_eq!(err, world_store::StoreError::StaleGeneration { stored: 1, mine: 0 });
    assert_eq!(w.h.abort_cut(CutId(2), 200), Ok(false), "the coordinator still records its abort");
    // A second run bumps once more (each run is one rollback).
    said(&url, &["rollback", &url, "--yes"]).expect("rolled back again");
    assert_eq!(w.h.world_generation().unwrap(), 2);
    assert_eq!(prepared(&w.w), 0);
    w.shutdown();
}
