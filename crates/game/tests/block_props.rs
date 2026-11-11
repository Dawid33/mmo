//! Block properties (`assets/blocks/blocks.ron`): the item / material name
//! table — the manifest's tree, biome and water entries. What
//! the names mean as stud terrain (materials, break times, drops) is
//! `stud_tiles::break_time_and_yield_follow_the_material` and `stud_gameplay::`.
use game::{install_block_props, BlockId, BlockRegistry};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");

/// The shipped registry, with its props installed (idempotent: every test
/// in this binary shares the process-global).
fn registry() -> BlockRegistry {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    reg
}

#[test]
fn manifest_tree_blocks_parse() {
    let reg = registry();
    let names = [
        "oak_log", "oak_branch", "oak_leaves", "pine_log", "pine_branch", "pine_leaves",
        "birch_log", "birch_branch", "birch_leaves",
    ];
    for (i, n) in names.iter().enumerate() {
        assert_eq!(reg.id_of(n), Some(BlockId(13 + i as u16)), "{n}");
    }
    let props = reg.props();
    assert_eq!(props.id_named("oak_log"), reg.id_of("oak_log"));
    assert_eq!(props.name_of(BlockId(9999)), None, "unknown ids have no name");
    assert_eq!(props, BlockRegistry::from_ron(MANIFEST).unwrap().props(), "equal manifests, equal props");
}

#[test]
fn manifest_has_biome_blocks() {
    let reg = registry();
    let mut names: Vec<String> = [
        "snow", "ice", "glacier_ice", "mud", "peat", "gravel", "red_sand", "dry_grass", "podzol",
        "frozen_dirt", "salt", "cactus",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for sp in ["spruce", "acacia", "palm", "jungle"] {
        for part in ["log", "branch", "leaves"] {
            names.push(format!("{sp}_{part}"));
        }
    }
    let props = reg.props();
    for n in &names {
        let id = reg.id_of(n).unwrap_or_else(|| panic!("{n} missing"));
        assert_ne!(id, BlockId(0), "{n} is air");
        assert_eq!(props.name_of(id), Some(n.as_str()));
    }
    // Every texture file named in the manifest exists.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks");
    let mut files = 0;
    for tok in MANIFEST.split('"').skip(1).step_by(2) {
        if tok.ends_with(".png") {
            assert!(dir.join(tok).is_file(), "texture {tok} missing");
            files += 1;
        }
    }
    assert!(files > 30);
}

#[test]
fn water_keeps_its_id() {
    let reg = registry();
    assert_eq!(reg.id_of("water"), Some(BlockId(5)));
    assert!(reg.id_of("ice").is_some());
}
