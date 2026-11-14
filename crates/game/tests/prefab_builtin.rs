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

fn temp_tree(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-prefab-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn named_hut(name: &str) -> String {
    let mut f = dev_hut();
    f.name = name.into();
    prefab_to_ron(&f)
}

#[test]
fn sub_directories_are_read() {
    let dir = temp_tree("subdirs");
    std::fs::create_dir_all(dir.join("more")).unwrap();
    std::fs::write(dir.join("a.prefab.ron"), named_hut("a")).unwrap();
    std::fs::write(dir.join("more").join("b.prefab.ron"), named_hut("b")).unwrap();
    let reg = registry();
    let set = load_builtin_prefabs(Some(&dir), &|n| reg.id_of(n));
    std::fs::remove_dir_all(&dir).unwrap();
    let set = set.unwrap();
    assert_eq!(set.len(), 2);
    assert!(set.named("a").is_some() && set.named("b").is_some());
}

#[test]
fn build_script_and_loader_take_the_same_files() {
    let dir = temp_tree("filter");
    std::fs::create_dir_all(dir.join(".hidden")).unwrap();
    std::fs::write(dir.join("a.prefab.ron"), named_hut("a")).unwrap();
    std::fs::write(dir.join(".#x.prefab.ron"), b"garbage {").unwrap();
    std::fs::write(dir.join("notes.txt"), b"garbage {").unwrap();
    std::fs::write(dir.join(".hidden").join("h.prefab.ron"), named_hut("h")).unwrap();
    let reg = registry();
    let set = load_builtin_prefabs(Some(&dir), &|n| reg.id_of(n));
    std::fs::remove_dir_all(&dir).unwrap();
    let set = set.unwrap();
    assert_eq!(set.len(), 1);
    assert!(set.named("a").is_some());
}

#[test]
fn name_keys_do_not_collide() {
    let reg = registry();
    let set = PrefabSet::from_sources(embedded(), &|n| reg.id_of(n)).unwrap();
    let keys: std::collections::BTreeSet<u32> = set.names().map(prefab_name_key).collect();
    assert_eq!(keys.len(), set.names().count());
    assert_eq!(keys.len(), set.len());
}
