use game::prefab::{resolve, Cell, FileShape, Facing, Half, Shape, StairCorner, Trust};
use game::registry::BlockRegistry;
use mc_import::convert::{convert, AirMode, ConvertOptions};
use mc_import::{BlockEntity, BlockState, BlockVolume, Tag, Table};
use std::collections::BTreeMap;
use std::path::Path;

/// A volume from a size and a closure naming each cell's state, building the palette as it goes.
fn vol(size: [u32; 3], mut f: impl FnMut(u32, u32, u32) -> String) -> BlockVolume {
    let mut palette: Vec<BlockState> = Vec::new();
    let mut seen: BTreeMap<String, u16> = BTreeMap::new();
    let mut blocks = Vec::new();
    for y in 0..size[1] {
        for z in 0..size[2] {
            for x in 0..size[0] {
                let s = f(x, y, z);
                let id = *seen.entry(s.clone()).or_insert_with(|| {
                    palette.push(BlockState::parse(&s).unwrap());
                    (palette.len() - 1) as u16
                });
                blocks.push(id);
            }
        }
    }
    BlockVolume {
        size,
        palette,
        blocks,
        block_entities: vec![],
        data_version: 3463,
        name: String::new(),
        author: String::new(),
    }
}

fn opts(air: AirMode) -> ConvertOptions {
    ConvertOptions {
        name: "test".into(),
        air,
        region: None,
        author: "me".into(),
        source: "src".into(),
        licence: "lic".into(),
        source_hash: "abc".into(),
        tags: vec![],
    }
}

fn registry() -> BlockRegistry {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let src = std::fs::read_to_string(root.join("assets/blocks/blocks.ron")).unwrap();
    BlockRegistry::from_ron(&src).unwrap()
}

const BRICK: &str = "minecraft:stone_bricks";
const AIR: &str = "minecraft:air";

/// The 7x5x7 house at (2,1,2) of an 11x7x11 volume; `door` fills the doorway cell (house-local 0,1,3).
fn house(door: &'static str) -> BlockVolume {
    vol([11, 7, 11], move |x, y, z| {
        let (hx, hy, hz) = (x as i32 - 2, y as i32 - 1, z as i32 - 2);
        if !(0..7).contains(&hx) || !(0..5).contains(&hy) || !(0..7).contains(&hz) {
            return AIR.into();
        }
        if (hx, hy, hz) == (0, 1, 3) {
            return door.into();
        }
        let room = (1..6).contains(&hx) && (1..4).contains(&hy) && (1..6).contains(&hz);
        if room { AIR.into() } else { BRICK.into() }
    })
}

#[test]
fn a_hollow_house() {
    let t = Table::builtin();
    // An open doorway: everything inside is reachable from outside.
    let (f, r) = convert(&house(AIR), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.size, [7, 5, 7]);
    assert!(f.carve.is_empty());
    assert_eq!(r.carved, 0);
    // A door is a marker with no solid: still passable.
    let (f, _) = convert(&house("minecraft:oak_door[facing=east,half=lower]"), t, &opts(AirMode::Auto)).unwrap();
    assert!(f.carve.is_empty());
    assert_eq!(f.markers.len(), 1);
    // A glass block closes it: the room is enclosed.
    let (f, r) = convert(&house("minecraft:glass"), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.size, [7, 5, 7]);
    assert_eq!(f.carve.len(), 1);
    assert_eq!(f.carve[0].size, [5, 3, 5]);
    assert_eq!(r.carved, 75);
    let reg = registry();
    let res = resolve(&f, &|n| reg.id_of(n), Trust::Builtin).unwrap();
    assert!(matches!(res.cell([3, 2, 3]), Cell::Carve));
    assert!(matches!(res.cell([0, 2, 0]), Cell::Block(_)));
    assert!(matches!(res.cell([0, 1, 3]), Cell::Block(_)));
    // Carve mode carves all the air, Keep mode none.
    let (f, _) = convert(&house(AIR), t, &opts(AirMode::Carve)).unwrap();
    assert_eq!(f.carve.iter().map(|c| c.size.iter().product::<u32>()).sum::<u32>(), 76);
    let (f, _) = convert(&house("minecraft:glass"), t, &opts(AirMode::Keep)).unwrap();
    assert!(f.carve.is_empty());
}

#[test]
fn greedy_boxes() {
    let t = Table::builtin();
    let slab = vol([4, 3, 2], |_, _, _| BRICK.into());
    let (f, r) = convert(&slab, t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.boxes.len(), 1);
    assert_eq!(f.boxes[0].size, [4, 3, 2]);
    assert_eq!(r.boxes, 1);

    let two = vol([4, 1, 1], |x, _, _| if x < 2 { BRICK.into() } else { "minecraft:sandstone".into() });
    assert_eq!(convert(&two, t, &opts(AirMode::Auto)).unwrap().0.boxes.len(), 2);

    // L-shape: a 3-wide row and a 1x1x2 stem.
    let l = vol([3, 1, 3], |x, _, z| if z == 0 || x == 0 { BRICK.into() } else { AIR.into() });
    let (f, _) = convert(&l, t, &opts(AirMode::Keep)).unwrap();
    assert_eq!(f.boxes.len(), 2);

    // Brute force: every solid cell covered exactly once, by its own block.
    let mut seed = 12345u32;
    let names = [AIR, BRICK, "minecraft:oak_planks", "minecraft:sandstone"];
    let v = vol([12, 12, 12], |_, _, _| {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        names[(seed >> 16) as usize % 4].into()
    });
    let (f, _) = convert(&v, t, &opts(AirMode::Keep)).unwrap();
    let src = f.meta.import.as_ref().unwrap().source_box.unwrap().0;
    let mut cover = BTreeMap::new();
    for b in &f.boxes {
        for z in 0..b.size[2] as i32 {
            for y in 0..b.size[1] as i32 {
                for x in 0..b.size[0] as i32 {
                    let c = [b.min[0] + x + src[0], b.min[1] + y + src[1], b.min[2] + z + src[2]];
                    assert!(cover.insert(c, &f.palette[b.mat as usize]).is_none(), "cell covered twice");
                }
            }
        }
    }
    let mut solid = 0;
    for z in 0..12u32 {
        for y in 0..12u32 {
            for x in 0..12u32 {
                let s = v.at(x, y, z);
                let got = cover.get(&[x as i32, y as i32, z as i32]);
                if s.name == AIR {
                    assert!(got.is_none());
                } else {
                    solid += 1;
                    let want = s.name.strip_prefix("minecraft:").unwrap();
                    assert_eq!(got.map(|g| g.as_str()), Some(want).map(|w| if w == "stone_bricks" { "stone_bricks" } else { w }));
                }
            }
        }
    }
    assert_eq!(cover.len(), solid);
}

fn s(x: &str) -> Tag {
    Tag::String(x.to_string())
}

fn comp(fields: Vec<(&str, Tag)>) -> Tag {
    Tag::Compound(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

#[test]
fn shaped_and_markers() {
    let t = Table::builtin();
    let states = [
        "minecraft:oak_stairs[facing=east,half=bottom,shape=straight]",
        "minecraft:cobblestone_wall[east=none,north=none,south=none,up=true,west=none]",
        "minecraft:oak_slab[type=top]",
        "minecraft:chest[facing=south]",
    ];
    let v = vol([4, 1, 1], |x, _, _| states[x as usize].into());
    let (f, r) = convert(&v, t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.shaped.len(), 3);
    assert_eq!(r.shaped, 3);
    let named: Vec<(&Shape, &str)> = f
        .shaped
        .iter()
        .map(|s| match &s.shape {
            FileShape::Named { shape, mat } => (shape, f.palette[*mat as usize].as_str()),
            _ => panic!("not named"),
        })
        .collect();
    assert_eq!(
        named[0],
        (&Shape::Stairs { facing: Facing::East, half: Half::Bottom, corner: StairCorner::Straight }, "oak_planks")
    );
    assert!(matches!(named[1], (Shape::Wall { .. }, "cobblestone")));
    assert_eq!(named[2], (&Shape::Slab { half: Half::Top }, "oak_planks"));
    // The chest: a block plus a loot marker.
    assert_eq!(f.markers.len(), 1);
    assert_eq!(f.markers[0].kind, "loot");
    assert_eq!(f.markers[0].at, [3, 0, 0]);
    assert_eq!(f.markers[0].props.get("mc").map(String::as_str), Some("minecraft:chest"));
    assert_eq!(f.markers[0].props.get("facing").map(String::as_str), Some("south"));
    assert_eq!(f.boxes.len(), 1);
    assert_eq!(f.palette[f.boxes[0].mat as usize], "chest");
    assert_eq!(f.boxes[0].min, [3, 0, 0]);
    assert_eq!(r.markers.get("loot"), Some(&1));

    // Signs: the 1.20 form and the old one.
    let mut v = vol([3, 1, 1], |x, _, _| if x == 0 { BRICK.into() } else { "minecraft:oak_sign[rotation=0]".into() });
    let new = comp(vec![(
        "front_text",
        comp(vec![("messages", Tag::List(vec![s("{\"text\":\"Zeus\"}"), s("\"\""), s("\"Temple\""), s("{\"text\":\"\"}")]))]),
    )]);
    let old = comp(vec![
        ("Text1", s("{\"text\":\"Hera\"}")),
        ("Text2", s("{\"text\":\"\"}")),
        ("Text3", s("{\"text\":\"Shrine\"}")),
        ("Text4", s("")),
    ]);
    v.block_entities = vec![
        BlockEntity { pos: [1, 0, 0], id: "minecraft:sign".into(), nbt: new },
        BlockEntity { pos: [2, 0, 0], id: "minecraft:sign".into(), nbt: old },
    ];
    let (f, _) = convert(&v, t, &opts(AirMode::Auto)).unwrap();
    let texts: Vec<_> = f.markers.iter().map(|m| m.props.get("text").cloned()).collect();
    assert_eq!(texts, vec![Some("Zeus Temple".to_string()), Some("Hera Shrine".to_string())]);
    // A sign without text carries no text prop.
    let v2 = vol([2, 1, 1], |x, _, _| if x == 0 { BRICK.into() } else { "minecraft:oak_sign[rotation=0]".into() });
    let (f, _) = convert(&v2, t, &opts(AirMode::Auto)).unwrap();
    assert!(!f.markers[0].props.contains_key("text"));
}

#[test]
fn unmapped_and_dropped_are_counted() {
    let t = Table::builtin();
    let states = [BRICK, "minecraft:frobnicator", "minecraft:water[level=0]", "minecraft:poppy", BRICK];
    let v = vol([5, 1, 1], |x, _, _| states[x as usize].into());
    let (f, r) = convert(&v, t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(r.unmapped.get("minecraft:frobnicator"), Some(&1));
    assert!(f.palette.iter().any(|p| p == "smooth_stone"));
    assert_eq!(r.dropped.values().sum::<u64>(), 2);
    assert_eq!(r.dropped.len(), 2);
    let text = r.to_text("temple");
    assert!(text.contains("temple"));
    assert!(text.contains("minecraft:frobnicator"));
    assert!(text.contains("liquid"));
    assert!(text.contains("plant"));
}

#[test]
fn region_and_trim() {
    let t = Table::builtin();
    // Two builds: a 2x2x2 at (1,1,1) and a 3x1x3 at (10,2,10).
    let v = vol([16, 6, 16], |x, y, z| {
        if (1..3).contains(&x) && (1..3).contains(&y) && (1..3).contains(&z) {
            BRICK.into()
        } else if (10..13).contains(&x) && y == 2 && (10..13).contains(&z) {
            "minecraft:sandstone".into()
        } else {
            AIR.into()
        }
    });
    let mut o = opts(AirMode::Auto);
    o.region = Some(([8, 0, 8], [16, 6, 16]));
    let (f, r) = convert(&v, t, &o).unwrap();
    assert_eq!(f.size, [3, 1, 3]);
    assert_eq!(f.palette, vec!["sandstone".to_string()]);
    assert_eq!(f.anchor, [1, 0, 1]);
    assert_eq!(f.meta.import.as_ref().unwrap().source_box, Some(([10, 2, 10], [13, 3, 13])));
    assert_eq!(r.blocks, 9);
    o.region = Some(([4, 0, 4], [8, 6, 8]));
    assert!(convert(&v, t, &o).is_none());
    // Without a region the whole volume is the build.
    let (f, _) = convert(&v, t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.size, [12, 2, 12]);
}

#[test]
fn deterministic() {
    let t = Table::builtin();
    let v = house("minecraft:glass");
    let a = game::prefab::prefab_to_bin(&convert(&v, t, &opts(AirMode::Auto)).unwrap().0);
    let b = game::prefab::prefab_to_bin(&convert(&v, t, &opts(AirMode::Auto)).unwrap().0);
    assert_eq!(a, b);
}
