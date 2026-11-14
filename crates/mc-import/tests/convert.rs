use game::prefab::{resolve, PrefabFile, Cell, FileShape, Facing, Half, Shape, StairCorner, Trust};
use game::registry::BlockRegistry;
use mc_import::convert::{convert as convert_raw, AirMode, ConvertOptions, CONVERT_MAX_RECORDS};
use mc_import::{ImportError, Report};
use mc_import::{BlockEntity, BlockVolume, Tag, Table};
use std::collections::BTreeMap;
use std::path::Path;

use crate::common::vol;

/// `convert` for a volume that is meant to be convertible.
fn convert(v: &BlockVolume, t: &Table, o: &ConvertOptions) -> Option<(PrefabFile, Report)> {
    convert_raw(v, t, o).unwrap()
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
        max_records: None,
    }
}

fn registry() -> BlockRegistry {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let src = std::fs::read_to_string(root.join("assets/blocks/blocks.ron")).unwrap();
    BlockRegistry::from_ron(&src).unwrap()
}

const BRICK: &str = "minecraft:stone_bricks";
const AIR: &str = "minecraft:air";

/// The 7x5x7 house at (2,1,2) of an 11x7x11 volume; `door` fills the doorway cell (house-local 0,1,3),
/// `upper` the cell above it (a door's upper half; the wall otherwise).
fn house_doors(door: &'static str, upper: &'static str) -> BlockVolume {
    vol([11, 7, 11], move |x, y, z| {
        let (hx, hy, hz) = (x as i32 - 2, y as i32 - 1, z as i32 - 2);
        if !(0..7).contains(&hx) || !(0..5).contains(&hy) || !(0..7).contains(&hz) {
            return AIR.into();
        }
        if (hx, hy, hz) == (0, 1, 3) {
            return door.into();
        }
        if (hx, hy, hz) == (0, 2, 3) && upper != BRICK {
            return upper.into();
        }
        let room = (1..6).contains(&hx) && (1..4).contains(&hy) && (1..6).contains(&hz);
        if room { AIR.into() } else { BRICK.into() }
    })
}

fn house(door: &'static str) -> BlockVolume {
    house_doors(door, BRICK)
}

#[test]
fn a_hollow_house() {
    let t = Table::builtin();
    // An open doorway: everything inside is reachable from outside.
    let (f, r) = convert(&house(AIR), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.size, [7, 5, 7]);
    assert!(f.carve.is_empty());
    assert_eq!(r.carved, 0);
    // A door is a barrier for the flood fill and is itself carved (an opening: carve + marker); with both
    // halves in the doorway the 75-cell room is carved plus the two door cells.
    let doored = house_doors(
        "minecraft:oak_door[facing=east,half=lower]",
        "minecraft:oak_door[facing=east,half=upper]",
    );
    let (f, r) = convert(&doored, t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(r.carved, 77);
    assert_eq!(f.carve.iter().map(|c| c.size.iter().product::<u32>()).sum::<u32>(), 77);
    assert_eq!(f.markers.len(), 1);
    assert_eq!(f.markers[0].at, [0, 1, 3]);
    assert_eq!(f.markers[0].kind, "door");
    let reg = registry();
    let res = resolve(&f, &|n| reg.id_of(n), Trust::Builtin).unwrap();
    assert!(matches!(res.cell([3, 2, 3]), Cell::Carve));
    assert!(matches!(res.cell([0, 1, 3]), Cell::Carve));
    assert!(matches!(res.cell([0, 2, 3]), Cell::Carve));
    // Keep mode carves nothing, door or not.
    let (f, _) = convert(&doored, t, &opts(AirMode::Keep)).unwrap();
    assert!(f.carve.is_empty());
    // A door block with no upper half: the lower cell is the barrier, the room is still sealed.
    let (_, r) = convert(&house("minecraft:oak_door[facing=east,half=lower]"), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(r.carved, 76);
    // Other markers without a solid (a torch in the doorway) stay passable.
    let (f, _) = convert(&house("minecraft:wall_torch[facing=east]"), t, &opts(AirMode::Auto)).unwrap();
    assert!(f.carve.is_empty());
    // A glass block closes it: the room is enclosed.
    let (f, r) = convert(&house("minecraft:glass"), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.size, [7, 5, 7]);
    assert_eq!(f.carve.len(), 1);
    assert_eq!(f.carve[0].size, [5, 3, 5]);
    assert_eq!(r.carved, 75);
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
                    assert_eq!(got.map(|g| g.as_str()), Some(s.name.strip_prefix("minecraft:").unwrap()));
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

/// Every cell of the trimmed box is in at most one of {box, shaped, carve box}; solids and shaped cells in
/// exactly one; no carve box holds a solid, shaped or keep cell.
#[test]
fn boxes_shaped_and_carve_partition_the_cells() {
    use mc_import::{classify, Class};
    let t = Table::builtin();
    let names = [
        AIR, AIR, AIR, AIR, BRICK, "minecraft:oak_planks", "minecraft:glass", "minecraft:barrier",
        "minecraft:oak_stairs[facing=north,half=bottom,shape=straight]", "minecraft:oak_door[facing=east,half=lower]",
    ];
    let mut seed = 777u32;
    let v = vol([12, 12, 12], |_, _, _| {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        names[(seed >> 16) as usize % names.len()].into()
    });
    for mode in [AirMode::Auto, AirMode::Carve] {
        let (f, _) = convert(&v, t, &opts(mode)).unwrap();
        let src = f.meta.import.as_ref().unwrap().source_box.unwrap().0;
        let sz = f.size.map(|n| n as usize);
        let idx = |c: [i32; 3]| c[0] as usize + c[2] as usize * sz[0] + c[1] as usize * sz[0] * sz[2];
        let mut boxed = vec![0u8; sz[0] * sz[1] * sz[2]];
        let mut shaped = boxed.clone();
        let mut carved = boxed.clone();
        let each = |min: [i32; 3], size: [u32; 3], f: &mut dyn FnMut([i32; 3])| {
            for z in 0..size[2] as i32 {
                for y in 0..size[1] as i32 {
                    for x in 0..size[0] as i32 {
                        f([min[0] + x, min[1] + y, min[2] + z]);
                    }
                }
            }
        };
        for b in &f.boxes {
            each(b.min, b.size, &mut |c| boxed[idx(c)] += 1);
        }
        for c in &f.carve {
            each(c.min, c.size, &mut |p| carved[idx(p)] += 1);
        }
        for s in &f.shaped {
            shaped[idx(s.at)] += 1;
        }
        for z in 0..sz[2] as i32 {
            for y in 0..sz[1] as i32 {
                for x in 0..sz[0] as i32 {
                    let i = idx([x, y, z]);
                    let state = v.at((src[0] + x) as u32, (src[1] + y) as u32, (src[2] + z) as u32);
                    let class = classify(state, t).class;
                    assert!(boxed[i] + shaped[i] + carved[i] <= 1, "{mode:?}: cell {x},{y},{z} claimed twice");
                    match class {
                        Class::Block { .. } => assert_eq!((boxed[i], shaped[i], carved[i]), (1, 0, 0)),
                        Class::Shaped { .. } => assert_eq!((boxed[i], shaped[i], carved[i]), (0, 1, 0)),
                        Class::Keep => assert_eq!((boxed[i], shaped[i], carved[i]), (0, 0, 0)),
                        Class::Air | Class::Marker { solid: None, .. } | Class::Dropped { .. } => {
                            assert_eq!((boxed[i], shaped[i]), (0, 0));
                            if mode == AirMode::Carve {
                                assert_eq!(carved[i], 1, "Carve mode carves all air");
                            }
                        }
                        Class::Marker { .. } => unreachable!(),
                    }
                }
            }
        }
    }
}

fn sign_entity(pos: [i32; 3], lines: [&str; 4]) -> BlockEntity {
    let msgs = Tag::List(lines.iter().map(|l| Tag::String(l.to_string())).collect());
    let front = Tag::Compound([("messages".to_string(), msgs)].into_iter().collect());
    BlockEntity {
        pos,
        id: "minecraft:sign".into(),
        nbt: Tag::Compound([("front_text".to_string(), front)].into_iter().collect()),
    }
}

#[test]
fn wall_and_hanging_signs_carry_text() {
    let t = Table::builtin();
    let states = ["minecraft:oak_wall_sign[facing=north]", "minecraft:oak_hanging_sign[rotation=0]", BRICK];
    let mut v = vol([3, 1, 1], |x, _, _| states[x as usize].into());
    v.block_entities = vec![
        sign_entity([0, 0, 0], [r#"{"text":"Wall"}"#, r#""""#, r#""""#, r#""""#]),
        sign_entity([1, 0, 0], [r#"{"text":"","extra":["Hang"]}"#, r#"["ing"]"#, r#""""#, r#""""#]),
    ];
    let (f, r) = convert(&v, t, &opts(AirMode::Auto)).unwrap();
    let texts: Vec<_> = f.markers.iter().map(|m| (m.kind.as_str(), m.props.get("text").cloned())).collect();
    assert_eq!(texts, vec![("sign", Some("Wall".to_string())), ("sign", Some("Hang ing".to_string()))]);
    assert_eq!(r.signs_unparsed, 0);
}

#[test]
fn unparsed_sign_lines_are_counted() {
    let t = Table::builtin();
    let mut v = vol([2, 1, 1], |x, _, _| if x == 0 { BRICK.into() } else { "minecraft:oak_sign[rotation=0]".into() });
    // Empty forms are fine; `{{` and a number are not.
    v.block_entities = vec![sign_entity([1, 0, 0], [r#"{"text":""}"#, "{}", "{{", "7"])];
    let (f, r) = convert(&v, t, &opts(AirMode::Auto)).unwrap();
    assert!(!f.markers[0].props.contains_key("text"));
    assert_eq!(r.signs_unparsed, 2);
    assert!(r.to_text("x").contains("unparsed"));
}

/// An L-shaped house: its notch lies inside the trimmed box and outside the house, so it stays KEEP while
/// the room is carved.
#[test]
fn an_l_shaped_house_keeps_its_notch() {
    let t = Table::builtin();
    // Footprint over x, z in 0..8 without the 4x4 corner x, z in 4..8; the volume has a 1-cell margin.
    let inl = |x: i32, z: i32| (0..8).contains(&x) && (0..8).contains(&z) && !(x >= 4 && z >= 4);
    let v = vol([10, 7, 10], |x, y, z| {
        let (hx, hy, hz) = (x as i32 - 1, y as i32 - 1, z as i32 - 1);
        if !inl(hx, hz) || !(0..5).contains(&hy) {
            return AIR.into();
        }
        let edge = !(inl(hx - 1, hz) && inl(hx + 1, hz) && inl(hx, hz - 1) && inl(hx, hz + 1));
        if hy == 0 || hy == 4 || edge { BRICK.into() } else { AIR.into() }
    });
    let room = (0..8).flat_map(|x| (0..8).map(move |z| (x, z))).filter(|&(x, z)| {
        inl(x, z) && inl(x - 1, z) && inl(x + 1, z) && inl(x, z - 1) && inl(x, z + 1)
    }).count() as u64
        * 3;
    assert!(room > 0);
    let (f, r) = convert(&v, t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.size, [8, 5, 8]);
    assert_eq!(r.carved, room);
    let reg = registry();
    let res = resolve(&f, &|n| reg.id_of(n), Trust::Builtin).unwrap();
    for y in 0..5 {
        for z in 4..8 {
            for x in 4..8 {
                assert!(matches!(res.cell([x, y, z]), Cell::Keep), "notch cell {x},{y},{z} must stay");
            }
        }
    }
    assert!(matches!(res.cell([1, 2, 1]), Cell::Carve));
}

/// A volume holding one of every shaped form the classifier can emit: all named shapes are valid and
/// non-empty, and the prefab resolves.
#[test]
fn every_shaped_form_is_valid_and_resolves() {
    let t = Table::builtin();
    let mut states: Vec<String> = Vec::new();
    for facing in ["north", "east", "south", "west"] {
        for half in ["top", "bottom"] {
            for shape in ["straight", "inner_left", "inner_right", "outer_left", "outer_right"] {
                states.push(format!("minecraft:oak_stairs[facing={facing},half={half},shape={shape}]"));
            }
        }
        states.push(format!("minecraft:spruce_trapdoor[facing={facing},half=top,open=true]"));
        states.push(format!("minecraft:spruce_trapdoor[facing={facing},half=bottom,open=false]"));
        states.push(format!("minecraft:oak_fence_gate[facing={facing},open=false]"));
        states.push(format!("minecraft:oak_fence_gate[facing={facing},open=true]"));
    }
    for ty in ["top", "bottom", "double"] {
        states.push(format!("minecraft:oak_slab[type={ty}]"));
    }
    for up in ["true", "false"] {
        for side in ["none", "low", "tall"] {
            states.push(format!("minecraft:cobblestone_wall[east={side},north=none,south=none,up={up},west={side}]"));
            states.push(format!("minecraft:cobblestone_wall[east=none,north={side},south={side},up={up},west=none]"));
        }
    }
    states.push("minecraft:cobblestone_wall[east=none,north=none,south=none,up=false,west=none]".into());
    for (a, b) in [("true", "false"), ("false", "false"), ("true", "true")] {
        states.push(format!("minecraft:oak_fence[east={a},north={b},south={a},west={b}]"));
        states.push(format!("minecraft:glass_pane[east={a},north={b},south={a},west={b}]"));
        states.push(format!("minecraft:iron_bars[east={a},north={b},south={a},west={b}]"));
    }
    states.push("minecraft:red_carpet".into());
    for l in 1..=8 {
        states.push(format!("minecraft:snow[layers={l}]"));
    }
    states.push("minecraft:barrel[facing=up]".into());
    let n = states.len() as u32;
    let v = vol([n, 1, 1], |x, _, _| states[x as usize].clone());
    let (f, _) = convert(&v, t, &opts(AirMode::Keep)).unwrap();
    assert!(!f.shaped.is_empty());
    for s in &f.shaped {
        match &s.shape {
            FileShape::Named { shape, .. } => assert!(shape.is_valid() && !shape.is_empty(), "{shape:?}"),
            other => panic!("unexpected {other:?}"),
        }
    }
    let reg = registry();
    resolve(&f, &|n| reg.id_of(n), Trust::Builtin).unwrap();
}

#[test]
fn barrels_are_plain_planks_and_gates_are_fences() {
    let t = Table::builtin();
    let v = vol([3, 1, 1], |x, _, _| {
        ["minecraft:barrel[facing=north]", "minecraft:oak_fence_gate[facing=north,open=false]", "minecraft:oak_fence_gate[facing=north,open=true]"]
            [x as usize]
            .into()
    });
    let (f, r) = convert(&v, t, &opts(AirMode::Keep)).unwrap();
    assert_eq!(f.boxes.len(), 1);
    assert_eq!(f.palette[f.boxes[0].mat as usize], "spruce_planks");
    assert_eq!(f.shaped.len(), 1);
    assert_eq!(r.markers.get("gate"), Some(&1));
    assert_eq!(r.markers.get("loot"), None);
}

fn sign_state_volume(data_version: i32, lines: Vec<Tag>) -> BlockVolume {
    let mut v = vol([2, 1, 1], |x, _, _| if x == 0 { BRICK.into() } else { "minecraft:oak_sign[rotation=0]".into() });
    v.data_version = data_version;
    let front = comp(vec![("messages", Tag::List(lines))]);
    v.block_entities =
        vec![BlockEntity { pos: [1, 0, 0], id: "minecraft:sign".into(), nbt: comp(vec![("front_text", front)]) }];
    v
}

#[test]
fn plain_string_sign_lines_from_1_21_5() {
    let t = Table::builtin();
    let lines = || vec![s("Zeus"), s("  Temple "), s(""), s(r#"{"text":"x"}"#)];
    // From data version 4325 a message may be a plain string.
    let (f, r) = convert(&sign_state_volume(4325, lines()), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.markers[0].props.get("text").map(String::as_str), Some("Zeus Temple x"));
    assert_eq!(r.signs_unparsed, 0);
    // Before it, such a line is unparsed (and counted); the JSON one still reads.
    let (f, r) = convert(&sign_state_volume(4324, lines()), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.markers[0].props.get("text").map(String::as_str), Some("x"));
    assert_eq!(r.signs_unparsed, 2);
    // Compound messages stay unparsed and counted, at any version.
    let (f, r) = convert(&sign_state_volume(4325, vec![comp(vec![("text", s("Hi"))]), s("Ok")]), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.markers[0].props.get("text").map(String::as_str), Some("Ok"));
    assert_eq!(r.signs_unparsed, 1);
}

#[test]
fn whitespace_only_sign_text_is_not_a_prop() {
    let t = Table::builtin();
    let (f, _) = convert(&sign_state_volume(3463, vec![s(r#"{"text":" "}"#), s(r#""  ""#)]), t, &opts(AirMode::Auto)).unwrap();
    assert!(!f.markers[0].props.contains_key("text"));
    let (f, _) = convert(&sign_state_volume(4325, vec![s("   ")]), t, &opts(AirMode::Auto)).unwrap();
    assert!(!f.markers[0].props.contains_key("text"));
}

#[test]
fn the_record_cap_is_an_error() {
    let t = Table::builtin();
    let stairs = "minecraft:oak_stairs[facing=north,half=bottom,shape=straight]";
    let v = vol([3, 3, 3], |_, _, _| stairs.into());
    let mut o = opts(AirMode::Auto);
    o.max_records = Some(10);
    assert_eq!(convert_raw(&v, t, &o).unwrap_err(), ImportError::TooLarge);
    // At the cap it still converts.
    o.max_records = Some(27);
    assert_eq!(convert_raw(&v, t, &o).unwrap().unwrap().0.shaped.len(), 27);
    // The default is the constant.
    assert_eq!(CONVERT_MAX_RECORDS, 1 << 23);
    assert!(convert_raw(&v, t, &opts(AirMode::Auto)).unwrap().is_some());
}

#[test]
fn nothing_solid_is_none_and_an_inconsistent_volume_is_an_error() {
    let t = Table::builtin();
    let air = vol([3, 3, 3], |_, _, _| AIR.into());
    assert!(convert_raw(&air, t, &opts(AirMode::Auto)).unwrap().is_none());
    let mut bad = vol([3, 3, 3], |_, _, _| BRICK.into());
    bad.blocks.pop();
    assert!(matches!(convert_raw(&bad, t, &opts(AirMode::Auto)), Err(ImportError::NotASchematic(_))));
    let mut bad = vol([3, 3, 3], |_, _, _| BRICK.into());
    bad.blocks.push(0);
    assert!(matches!(convert_raw(&bad, t, &opts(AirMode::Auto)), Err(ImportError::NotASchematic(_))));
}
