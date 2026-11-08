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
