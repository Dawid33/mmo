//! `install_from_env` with a real registry, on procedural terrain. Its own `[[test]]` target: it
//! installs process globals (the prefab set, the structure source), so every assertion is in ONE test.

use game::prefab::Placement;
use game::{BlockId, ColumnCoords, RegionCoords, SubChunk, CHUNK_BLOCKS};
use server::dev_structures::{from_env, install_from_env, DevStructures, LandingOf, DEV_STRUCTURES_ENV, PREFAB_DIRS_ENV};
use worldgen::structures::{active, DEV_GAP_BLOCKS};

const CB: i32 = CHUNK_BLOCKS as i32;
const RB: i32 = game::REGION_BLOCKS as i32;

/// The block at world `p` in generated regions (copied from worldgen's `structure_support`, which a
/// different crate cannot import): stored blocks, else the column's implied ground.
fn read(cols: &[(ColumnCoords, game::Column)], [x, y, z]: [i32; 3]) -> BlockId {
    let cc = ColumnCoords::new((x / CB) as usize, (z / CB) as usize);
    let col = &cols.iter().find(|(c, _)| *c == cc).unwrap().1;
    let (lx, ly, lz) = ((x % CB) as usize, y.rem_euclid(CB) as usize, (z % CB) as usize);
    let sub_y = y.div_euclid(CB);
    if sub_y < col.base_sub_y() {
        return col.ground().unwrap().block_at(lx, lz, y).id;
    }
    match col.sub_chunk_at(sub_y) {
        Some(SubChunk::Dense(c)) => c.block_at(lx, ly, lz).id,
        _ => BlockId::AIR,
    }
}

#[test]
fn dev_structures_install_and_stamp() {
    let registry = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    game::install_block_props(registry.props());
    let mats = worldgen::material::Materials::from_names(|n| registry.id_of(n).unwrap());
    let seed = worldgen::WORLD_SEED;

    // Off: no variable, no source; the built-in prefabs are installed anyway.
    std::env::remove_var("VOXEL_DEV_STRUCTURES");
    let landing: LandingOf = std::sync::Arc::new(|_: RegionCoords| Some([20u32, 20u32]));
    let pure_landing = landing.clone();
    // (A second install in this process would conflict, so the "off" call is the same set.)
    assert_eq!(install_from_env(&registry, landing.clone(), seed, mats), Ok(DevStructures::Off));
    assert!(game::prefab::prefabs().named("dev-hut").is_some());
    assert!(active().placements_in(-100_000, -100_000, 100_000, 100_000, 0).is_empty());

    // On.
    std::env::set_var("VOXEL_DEV_STRUCTURES", "0,0=dev-hut");
    assert_eq!(
        install_from_env(&registry, landing, seed, mats),
        Ok(DevStructures::Region { x: 0, z: 0, placements: 1 })
    );

    let found = active().placements_in(0, 0, RB, RB, 0);
    assert_eq!(found.len(), 1);
    let sp = &found[0];
    assert_eq!(sp.prefab.name, "dev-hut");
    let ground = worldgen::ground_y(20 + DEV_GAP_BLOCKS, 20, seed, mats);
    assert!((-64..320).contains(&ground), "ground_y {ground}");
    let Placement { anchor, .. } = sp.at;
    assert_eq!(anchor[1], worldgen::ground_y(anchor[0], anchor[2], seed, mats) + 1);
    // Anchor (3, 0, 6) of the hut: its box starts DEV_GAP_BLOCKS east of landing column (20, 20).
    assert_eq!(sp.bounds().0[0], 20 + DEV_GAP_BLOCKS);
    assert_eq!(anchor[2], 20);

    let cols = worldgen::generate_region(RegionCoords { x: 0, z: 0 }, seed, mats);
    let bricks = registry.id_of("stone_bricks").unwrap();
    // The floor layer (y0 of the hut) is stone bricks.
    for (dx, dz) in [(0, 0), (3, 3), (6, 6)] {
        let p = [anchor[0] - 3 + dx, anchor[1], anchor[2] - 6 + dz];
        assert_eq!(read(&cols, p), bricks, "floor cell {p:?}");
    }

    // The pure step (`from_env`): what would be installed, built again and again in this process with
    // nothing installed. Every region's form first.
    std::env::set_var(DEV_STRUCTURES_ENV, "all=dev-hut");
    let setup = from_env(&registry, pure_landing.clone(), seed, mats).expect("all=dev-hut");
    assert_eq!(setup.what, DevStructures::EveryRegion { spec: "dev-hut".into() });
    assert!(setup.prefabs.named("dev-hut").is_some());
    let source = setup.source.expect("a source for all=");
    let row = source.placements_in(0, 0, 2 * RB, RB, 0);
    for rc_x in [0, 1] {
        let beside: Vec<_> =
            row.iter().filter(|p| p.bounds().0[0] == rc_x * RB + 20 + DEV_GAP_BLOCKS && p.at.anchor[2] == 20).collect();
        assert_eq!(beside.len(), 1, "one dev-hut beside region ({rc_x}, 0)'s landing: {row:?}");
        assert_eq!(beside[0].prefab.name, "dev-hut");
    }
    // The home form: no placement anywhere until a region is generated in full, then the row is beside
    // that region's landing only.
    std::env::set_var(DEV_STRUCTURES_ENV, "home=dev-hut");
    let setup = from_env(&registry, pure_landing.clone(), seed, mats).expect("home=dev-hut");
    assert_eq!(setup.what, DevStructures::Home { spec: "dev-hut".into() });
    let source = setup.source.expect("a source for home=");
    assert!(source.placements_in(-10 * RB, -10 * RB, 10 * RB, 10 * RB, 0).is_empty());
    source.region_requested(RegionCoords { x: 1, z: 0 });
    let row = source.placements_in(-10 * RB, -10 * RB, 10 * RB, 10 * RB, 0);
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].bounds().0[0], RB + 20 + DEV_GAP_BLOCKS);
    // Errors, each naming the offending text.
    for (value, names) in [
        ("garbage", "garbage"),
        ("0,0", "0,0"),
        ("x,y=dev-hut", "\"x\""),
        ("all=no-such-prefab", "no-such-prefab"),
        ("0,0=", "0,0="),
        ("home=", "home="),
        ("home=no-such-prefab", "no-such-prefab"),
    ] {
        std::env::set_var(DEV_STRUCTURES_ENV, value);
        let e = from_env(&registry, pure_landing.clone(), seed, mats).err().unwrap_or_else(|| panic!("{value} accepted"));
        assert!(e.contains(value) && e.contains(names), "{value}: {e}");
    }
    std::env::remove_var(DEV_STRUCTURES_ENV);
    let missing = std::env::temp_dir().join("voxel-no-such-prefab-dir");
    assert!(!missing.exists());
    std::env::set_var(PREFAB_DIRS_ENV, &missing);
    let e = from_env(&registry, pure_landing, seed, mats).err().expect("a missing prefab directory is an error");
    assert!(e.contains(&*missing.to_string_lossy()), "{e}");
    std::env::remove_var(PREFAB_DIRS_ENV);
}
