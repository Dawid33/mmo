//! Block properties (`assets/blocks/blocks.ron`): the item / material name
//! table — the manifest's tree, biome and water entries and their flags. What
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
    let id = |n: &str| reg.id_of(n).unwrap();
    for sp in ["oak", "pine", "birch"] {
        assert!(!props.is_solid(id(&format!("{sp}_leaves"))));
        assert!(props.is_cutout(id(&format!("{sp}_leaves"))));
        for part in ["log", "branch"] {
            assert!(props.is_solid(id(&format!("{sp}_{part}"))));
            assert!(!props.is_cutout(id(&format!("{sp}_{part}"))));
        }
    }
    // Trunks: every `_log`, and the one-block cactus; never branches or leaves.
    for sp in ["oak", "pine", "birch", "spruce", "acacia", "palm", "jungle"] {
        assert!(props.is_trunk(id(&format!("{sp}_log"))), "{sp}");
        assert!(!props.is_trunk(id(&format!("{sp}_branch"))) && !props.is_trunk(id(&format!("{sp}_leaves"))), "{sp}");
    }
    assert!(props.is_trunk(id("cactus")) && props.is_solid(id("cactus")));
    assert!(!props.is_trunk(id("stone")));
    assert!(props.is_solid(id("stone")) && !props.is_cutout(id("stone")));
    assert!(props.is_solid(BlockId(9999)) && !props.is_cutout(BlockId(9999)), "unknown = solid, opaque");
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
        if n.ends_with("_leaves") {
            assert!(!props.is_solid(id) && props.is_cutout(id), "{n}");
        } else {
            assert!(props.is_solid(id), "{n}");
        }
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
fn water_is_liquid_and_not_solid() {
    let reg = registry();
    let props = game::block_props();
    let water = reg.id_of("water").unwrap();
    assert_eq!(water, BlockId(5));
    assert!(props.is_liquid(water));
    assert!(!props.is_solid(water), "a liquid is always non-solid");
    assert!(!props.blocks_ray(water));
    assert!(!props.blocks_ray(BlockId::AIR));
    let ice = reg.id_of("ice").unwrap();
    assert!(props.blocks_ray(ice) && props.is_solid(ice) && !props.is_liquid(ice));
    assert!(props.blocks_ray(reg.id_of("stone").unwrap()));
}
