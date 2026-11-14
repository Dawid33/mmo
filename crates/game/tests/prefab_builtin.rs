use game::prefab::*;
use game::{BlockId, BlockRegistry};
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn registry() -> BlockRegistry {
    BlockRegistry::from_ron(&std::fs::read_to_string(repo().join("assets/blocks/blocks.ron")).unwrap()).unwrap()
}

fn embedded() -> Vec<(&'static str, &'static [u8])> {
    embedded_prefab_sources().to_vec()
}

fn dev_hut() -> PrefabFile {
    let (_, bytes) = embedded().into_iter().find(|(n, _)| *n == "dev-hut.prefab.ron").expect("dev-hut is embedded");
    prefab_from_named_bytes("dev-hut.prefab.ron", bytes, None).unwrap().unwrap()
}

fn one(name: &str, file: &PrefabFile) -> Result<PrefabSet, String> {
    let reg = registry();
    let text = prefab_to_ron(file);
    PrefabSet::from_sources([(name, text.as_bytes())], &|n| reg.id_of(n))
}

#[test]
fn the_embedded_set_resolves() {
    let reg = registry();
    let set = PrefabSet::from_sources(embedded(), &|n| reg.id_of(n)).unwrap();
    assert!(set.named("dev-hut").unwrap().solid_count() > 100);
    assert!(set.names().any(|n| n == "dev-hut"));
}

#[test]
fn disk_and_embedded_agree() {
    let reg = registry();
    let disk = load_builtin_prefabs(Some(&repo().join(BUILTIN_PREFAB_DIR)), &|n| reg.id_of(n)).unwrap();
    let emb = PrefabSet::from_sources(embedded(), &|n| reg.id_of(n)).unwrap();
    assert_eq!(disk, emb);
    let none = load_builtin_prefabs(None, &|n| reg.id_of(n)).unwrap();
    assert_eq!(none, emb);
}

#[test]
fn the_hut_has_a_door_and_a_roof() {
    let reg = registry();
    let set = PrefabSet::from_sources(embedded(), &|n| reg.id_of(n)).unwrap();
    let hut = set.named("dev-hut").unwrap();
    assert!(hut.markers.iter().any(|m| m.kind == "door"));
    let above = [hut.anchor[0], hut.anchor[1] + 1, hut.anchor[2]];
    assert!(matches!(hut.cell(above), Cell::Carve | Cell::Keep));
    let (cx, cz) = (hut.size[0] as i32 / 2, hut.size[2] as i32 / 2);
    let spruce: BlockId = reg.id_of("spruce_planks").unwrap();
    assert_eq!(hut.column_top(cx, cz).map(|(_, b)| b), Some(spruce));
}

#[test]
fn a_stem_must_match_the_name() {
    let err = one("other.prefab.ron", &dev_hut()).unwrap_err();
    assert!(err.contains("other.prefab.ron"), "{err}");
}

#[test]
fn a_source_needs_a_licence() {
    let mut f = dev_hut();
    f.meta.source = "somewhere".into();
    f.meta.licence = String::new();
    let err = one("dev-hut.prefab.ron", &f).unwrap_err();
    assert!(err.contains("dev-hut.prefab.ron"), "{err}");
}

#[test]
fn an_unknown_block_names_the_file() {
    let mut f = dev_hut();
    f.palette[0] = "no_such_block".into();
    let err = one("dev-hut.prefab.ron", &f).unwrap_err();
    assert!(err.contains("dev-hut.prefab.ron"), "{err}");
}

#[test]
fn skipped_directories_are_not_read() {
    let dir = std::env::temp_dir().join(format!("mmo-prefab-skip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["imported", "sources"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
        std::fs::write(dir.join(d).join("b.prefab.ron"), b"garbage {").unwrap();
    }
    let mut f = dev_hut();
    f.name = "a".into();
    std::fs::write(dir.join("a.prefab.ron"), prefab_to_ron(&f)).unwrap();
    let reg = registry();
    let set = load_builtin_prefabs(Some(&dir), &|n| reg.id_of(n));
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(set.unwrap().len(), 1);
}

#[test]
fn name_keys_do_not_collide() {
    let reg = registry();
    let set = PrefabSet::from_sources(embedded(), &|n| reg.id_of(n)).unwrap();
    let keys: std::collections::BTreeSet<u32> = set.names().map(prefab_name_key).collect();
    assert_eq!(keys.len(), set.names().count());
    assert_eq!(keys.len(), set.len());
}
