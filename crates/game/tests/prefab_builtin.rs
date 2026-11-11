//! The built-in prefab set: embedded sources, disk override, name keys.

use crate::common::prefabs::{arch, TempDir};
use game::{
    embedded_prefab_sources, load_builtin_prefabs, prefab_name_key, prefab_to_ron, PrefabKindFile, PrefabSet,
};

const ARCH_RON: &str = include_str!("../../../assets/prefabs/dev-arch.prefab.ron");

#[test]
fn embedded_set_contains_the_sample() {
    let s = PrefabSet::from_sources(embedded_prefab_sources().iter().copied()).unwrap();
    assert!(s.named("dev-arch").is_some());
    assert_eq!(s.get(prefab_name_key("dev-arch")).unwrap().name, "dev-arch");
    assert!(s.names().any(|n| n == "dev-arch"));
    assert!(s.named("nope").is_none());
}

#[test]
fn stem_must_match_name() {
    assert!(PrefabSet::from_sources([("other", ARCH_RON)]).unwrap_err().contains("other"));
}

#[test]
fn bad_prefab_fails_the_set_with_its_name() {
    assert!(PrefabSet::from_sources([("x", "(nope")]).unwrap_err().contains("x"));
    // Parses, does not resolve.
    let mut f = arch();
    f.name = "unknown-part".into();
    let PrefabKindFile::Build(b) = &mut f.kind else { unreachable!() };
    b.parts[0].part = "99999".into();
    let err = PrefabSet::from_sources([("unknown-part", prefab_to_ron(&f).as_str())]).unwrap_err();
    assert!(err.contains("unknown-part") && err.contains("99999"), "{err}");
}

#[test]
fn the_same_name_twice_fails_the_set() {
    assert!(PrefabSet::from_sources([("dev-arch", ARCH_RON), ("dev-arch", ARCH_RON)]).unwrap_err().contains("dev-arch"));
}

#[test]
fn disk_overrides_embedded() {
    let tmp = TempDir::new("builtin");
    let mut f = arch();
    let PrefabKindFile::Build(b) = &mut f.kind else { unreachable!() };
    b.parts[0].min[0] += 1;
    std::fs::write(tmp.path().join("dev-arch.prefab.ron"), prefab_to_ron(&f)).unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "not a prefab, ignored").unwrap();
    let disk = load_builtin_prefabs(Some(tmp.path())).unwrap();
    let embedded = load_builtin_prefabs(None).unwrap();
    assert_ne!(disk.named("dev-arch").unwrap().key(), embedded.named("dev-arch").unwrap().key());
    // A directory that does not exist falls back to the embedded set.
    assert_eq!(load_builtin_prefabs(Some(&tmp.path().join("missing"))).unwrap(), embedded);
}

#[test]
fn a_bad_file_on_disk_fails_the_load() {
    let tmp = TempDir::new("builtin-bad");
    std::fs::write(tmp.path().join("junk.prefab.ron"), "junk").unwrap();
    assert!(load_builtin_prefabs(Some(tmp.path())).unwrap_err().contains("junk"));
}

#[test]
fn no_name_key_collisions_in_assets() {
    let s = load_builtin_prefabs(None).unwrap();
    assert_eq!(s.names().count(), embedded_prefab_sources().len());
}

#[test]
fn nothing_installed_reads_as_some_set() {
    // Other tests of this binary may install the embedded set; either way this never panics.
    let _ = game::prefabs().named("dev-arch");
}
