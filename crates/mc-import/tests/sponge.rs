use crate::common::*;
use mc_import::{read_schem, BlockState, ImportError};

const STATES: [&str; 3] = [
    "minecraft:air",
    "minecraft:stone",
    "minecraft:oak_stairs[facing=north,half=bottom]",
];

/// 3 x 2 x 4 cells (x + z*3 + y*12).
fn cells() -> Vec<u32> {
    let mut v = vec![0u32; 24];
    v[0] = 1; // (0,0,0)
    v[2 + 3] = 2; // (2,0,1)
    v[1 + 3 * 3 + 12] = 1; // (1,1,3)
    v[23] = 2; // (2,1,3)
    v[1 + 2 * 3 + 12] = 2; // (1,1,2)
    v
}

fn palette(states: &[String]) -> V {
    V::Compound(states.iter().enumerate().map(|(i, s)| (s.clone(), V::Int(i as i32))).collect())
}

fn v2_with(states: &[String], data: Vec<u8>, version: i32) -> Vec<u8> {
    let sign = c(vec![
        ("Pos", V::Ints(vec![1, 0, 2])),
        ("Id", s("minecraft:sign")),
        ("Text", s("hi")),
    ]);
    let root = c(vec![
        ("Version", V::Int(version)),
        ("DataVersion", V::Int(3700)),
        ("Width", V::Short(3)),
        ("Height", V::Short(2)),
        ("Length", V::Short(4)),
        ("PaletteMax", V::Int(states.len() as i32)),
        ("Palette", palette(states)),
        ("BlockData", V::Bytes(data)),
        ("BlockEntities", V::List(10, vec![sign])),
        ("Metadata", c(vec![("Name", s("Hut")), ("Author", s("Me"))])),
    ]);
    gzip(&nbt("Schematic", &root))
}

fn strs() -> Vec<String> {
    STATES.iter().map(|s| s.to_string()).collect()
}

fn v2() -> Vec<u8> {
    v2_with(&strs(), varints(&cells()), 2)
}

fn v3() -> Vec<u8> {
    let sign = c(vec![("Pos", V::Ints(vec![1, 0, 2])), ("Id", s("minecraft:sign")), ("Text", s("hi"))]);
    let inner = c(vec![
        ("Version", V::Int(3)),
        ("DataVersion", V::Int(3700)),
        ("Width", V::Short(3)),
        ("Height", V::Short(2)),
        ("Length", V::Short(4)),
        (
            "Blocks",
            c(vec![
                ("Palette", palette(&strs())),
                ("Data", V::Bytes(varints(&cells()))),
                ("BlockEntities", V::List(10, vec![sign])),
            ]),
        ),
        ("Metadata", c(vec![("Name", s("Hut")), ("Author", s("Me"))])),
    ]);
    gzip(&nbt("", &c(vec![("Schematic", inner)])))
}

#[test]
fn reads_a_v2_schematic() {
    let v = read_schem(&v2()).unwrap();
    assert_eq!(v.size, [3, 2, 4]);
    assert_eq!(v.blocks.len(), 24);
    assert_eq!(v.at(0, 0, 0).name, "minecraft:stone");
    assert_eq!(v.at(1, 0, 0).name, "minecraft:air");
    assert_eq!(v.at(2, 0, 1).name, "minecraft:oak_stairs");
    assert_eq!(v.at(2, 0, 1).props["facing"], "north");
    assert_eq!(v.at(1, 1, 3).name, "minecraft:stone");
    assert_eq!(v.at(2, 1, 3).name, "minecraft:oak_stairs");
    assert_eq!(v.at(1, 1, 2).props["half"], "bottom");
    assert_eq!(v.palette.len(), 3);
    assert_eq!(v.palette[2], BlockState::parse(STATES[2]).unwrap());
    assert_eq!(v.block_entities.len(), 1);
    assert_eq!(v.block_entities[0].pos, [1, 0, 2]);
    assert_eq!(v.block_entities[0].id, "minecraft:sign");
    assert_eq!(v.data_version, 3700);
    assert_eq!(v.name, "Hut");
    assert_eq!(v.author, "Me");
}

#[test]
fn reads_a_v3_schematic() {
    let a = read_schem(&v2()).unwrap();
    let b = read_schem(&v3()).unwrap();
    assert_eq!(a.size, b.size);
    assert_eq!(a.palette, b.palette);
    assert_eq!(a.blocks, b.blocks);
    assert_eq!(a.block_entities[0].pos, b.block_entities[0].pos);
    assert_eq!(a.block_entities[0].id, b.block_entities[0].id);
    assert_eq!((a.data_version, &a.name, &a.author), (b.data_version, &b.name, &b.author));
}

#[test]
fn varints_above_127() {
    let states: Vec<String> = (0..200).map(|i| format!("minecraft:block_{i}")).collect();
    let cells: Vec<u32> = (0..24).map(|i| 199 - i * 8).collect();
    let v = read_schem(&v2_with(&states, varints(&cells), 2)).unwrap();
    for (i, &c) in cells.iter().enumerate() {
        assert_eq!(v.blocks[i] as u32, c);
    }
    assert_eq!(v.at(0, 0, 0).name, "minecraft:block_199");
}

#[test]
fn refuses_bad_input() {
    assert!(matches!(read_schem(&[]), Err(ImportError::NotGzip)));
    assert!(matches!(read_schem(b"just some plain text"), Err(ImportError::NotGzip)));

    // A valid file cut at every tenth length: an error, never a panic.
    let good = v2();
    for n in (0..good.len()).step_by(10) {
        assert!(read_schem(&good[..n]).is_err(), "cut at {n}");
    }

    let mut short = cells();
    short.pop();
    assert!(matches!(
        read_schem(&v2_with(&strs(), varints(&short), 2)),
        Err(ImportError::NotASchematic(_))
    ));
    let mut long = cells();
    long.push(0);
    assert!(matches!(
        read_schem(&v2_with(&strs(), varints(&long), 2)),
        Err(ImportError::NotASchematic(_))
    ));
    let mut bad = cells();
    bad[5] = 3;
    assert!(matches!(
        read_schem(&v2_with(&strs(), varints(&bad), 2)),
        Err(ImportError::NotASchematic(_))
    ));
    assert!(matches!(
        read_schem(&v2_with(&strs(), varints(&cells()), 1)),
        Err(ImportError::Unsupported(_))
    ));

    // A byte array declaring i32::MAX in a tiny file: Truncated, no allocation.
    let mut raw = vec![7u8, 0, 0];
    raw.extend(i32::MAX.to_be_bytes());
    raw.resize(40, 0);
    assert!(matches!(mc_import::nbt::parse_nbt(&raw), Err(ImportError::Truncated)));

    // 100 lists deep.
    let mut deep = V::List(0, vec![]);
    for _ in 0..100 {
        deep = V::List(9, vec![deep]);
    }
    let bytes = nbt("", &deep);
    assert!(matches!(mc_import::nbt::parse_nbt(&bytes), Err(ImportError::BadNbt(_))));
    assert!(matches!(read_schem(&gzip(&bytes)), Err(ImportError::BadNbt(_))));
}

#[test]
fn block_state_parsing() {
    let s = BlockState::parse("minecraft:stone").unwrap();
    assert_eq!((s.name.as_str(), s.props.len()), ("minecraft:stone", 0));
    let s = BlockState::parse("minecraft:oak_stairs[facing=north,half=bottom]").unwrap();
    assert_eq!(s.name, "minecraft:oak_stairs");
    assert_eq!(s.props.len(), 2);
    assert_eq!(s.props["half"], "bottom");
    for bad in ["a[b", "a[b=c", ""] {
        assert!(BlockState::parse(bad).is_err(), "{bad}");
    }
}
