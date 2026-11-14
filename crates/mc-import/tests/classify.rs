use game::prefab::{Facing, Half, Shape, Sides, StairCorner};
use game::registry::BlockRegistry;
use mc_import::{classify, BlockState, Class, Classified, Table};
use std::path::Path;

fn st(s: &str) -> Classified {
    classify(&BlockState::parse(s).unwrap(), Table::builtin())
}

fn block(b: &str) -> Class {
    Class::Block { block: b.into() }
}

fn shaped(shape: Shape, b: &str) -> Class {
    Class::Shaped { shape, block: b.into() }
}

fn sides(n: bool, e: bool, s: bool, w: bool) -> Sides {
    Sides { north: n, east: e, south: s, west: w }
}

#[test]
fn forms() {
    // stairs
    assert_eq!(
        st("minecraft:oak_stairs[facing=east,half=top,shape=outer_left]").class,
        shaped(Shape::Stairs { facing: Facing::East, half: Half::Top, corner: StairCorner::OuterLeft }, "oak_planks")
    );
    assert_eq!(
        st("minecraft:stone_brick_stairs[facing=north,half=bottom,shape=inner_right]").class,
        shaped(
            Shape::Stairs { facing: Facing::North, half: Half::Bottom, corner: StairCorner::InnerRight },
            "stone_bricks"
        )
    );
    // slabs
    assert_eq!(st("minecraft:smooth_sandstone_slab[type=double]").class, block("smooth_sandstone"));
    assert_eq!(
        st("minecraft:brick_slab[type=top,waterlogged=true]").class,
        shaped(Shape::Slab { half: Half::Top }, "bricks")
    );
    assert_eq!(
        st("minecraft:oak_slab[type=bottom]").class,
        shaped(Shape::Slab { half: Half::Bottom }, "oak_planks")
    );
    // walls
    assert_eq!(
        st("minecraft:sandstone_wall[east=low,north=none,south=tall,up=true,west=none]").class,
        shaped(Shape::Wall { sides: sides(false, true, true, false), post: true }, "sandstone")
    );
    assert_eq!(
        st("minecraft:cobblestone_wall[east=none,north=none,south=none,up=false,west=low]").class,
        shaped(Shape::Wall { sides: sides(false, false, false, true), post: false }, "cobblestone")
    );
    // fences, bars, panes
    assert_eq!(
        st("minecraft:spruce_fence[east=true,north=false,south=true,west=false]").class,
        shaped(Shape::Fence { sides: sides(false, true, true, false) }, "spruce_planks")
    );
    assert_eq!(
        st("minecraft:iron_bars[east=false,north=true,south=false,west=false]").class,
        shaped(Shape::Pane { sides: sides(true, false, false, false) }, "iron_block")
    );
    assert_eq!(
        st("minecraft:gray_stained_glass_pane[east=true,north=true,south=false,west=false]").class,
        shaped(Shape::Pane { sides: sides(true, true, false, false) }, "gray_stained_glass")
    );
    assert_eq!(st("minecraft:glass_pane").class, shaped(Shape::Pane { sides: Sides::default() }, "glass"));
    // trapdoors
    assert_eq!(
        st("minecraft:spruce_trapdoor[facing=west,half=top,open=true]").class,
        shaped(Shape::Trapdoor { facing: Facing::West, half: Half::Top, open: true }, "spruce_planks")
    );
    // carpets
    assert_eq!(st("minecraft:green_carpet").class, shaped(Shape::Carpet, "green_wool"));
    assert_eq!(st("minecraft:moss_carpet").class, shaped(Shape::Carpet, "green_terracotta"));
    // snow
    assert_eq!(st("minecraft:snow[layers=3]").class, shaped(Shape::Layer { layers: 3 }, "snow"));
    assert_eq!(st("minecraft:snow[layers=8]").class, block("snow"));
    // markers
    let door = Class::Marker { kind: "door".into(), turn: 2, solid: None };
    assert_eq!(st("minecraft:oak_door[facing=south,half=lower,hinge=left,open=false]").class, door);
    assert_eq!(
        st("minecraft:oak_fence_gate[facing=east,open=false]").class,
        Class::Marker { kind: "gate".into(), turn: 1, solid: None }
    );
    assert_eq!(
        st("minecraft:chest[facing=north,type=single]").class,
        Class::Marker { kind: "loot".into(), turn: 0, solid: Some("chest".into()) }
    );
    assert_eq!(
        st("minecraft:barrel[facing=up]").class,
        Class::Marker { kind: "loot".into(), turn: 0, solid: Some("chest".into()) }
    );
    assert_eq!(
        st("minecraft:wall_torch[facing=west]").class,
        Class::Marker { kind: "light".into(), turn: 3, solid: None }
    );
    assert_eq!(
        st("minecraft:sea_lantern").class,
        Class::Marker { kind: "light".into(), turn: 0, solid: Some("smooth_quartz".into()) }
    );
    assert_eq!(
        st("minecraft:lantern[hanging=true]").class,
        Class::Marker { kind: "light".into(), turn: 0, solid: None }
    );
    assert_eq!(
        st("minecraft:spruce_wall_sign[facing=east]").class,
        Class::Marker { kind: "sign".into(), turn: 1, solid: None }
    );
    // standing sign / banner: rotation 0 = south (turn 2), 8 = north (turn 0), 4 = west, 12 = east
    for (rot, turn) in [("0", 2), ("4", 3), ("8", 0), ("12", 1), ("10", 0), ("15", 1)] {
        assert_eq!(
            st(&format!("minecraft:white_banner[rotation={rot}]")).class,
            Class::Marker { kind: "banner".into(), turn, solid: None },
            "rotation {rot}"
        );
    }
    assert_eq!(
        st("minecraft:red_bed[facing=north,part=foot]").class,
        Class::Marker { kind: "bed".into(), turn: 0, solid: None }
    );
    assert_eq!(
        st("minecraft:ladder[facing=east]").class,
        Class::Marker { kind: "ladder".into(), turn: 1, solid: None }
    );
    for decor in ["stone_button[face=wall,facing=east]", "lever[face=floor]", "oak_pressure_plate", "rail[shape=north_south]",
        "flower_pot", "potted_oak_sapling", "zombie_head[rotation=3]", "skeleton_wall_skull[facing=west]"]
    {
        assert!(matches!(st(&format!("minecraft:{decor}")).class, Class::Marker { ref kind, solid: None, .. } if kind == "decor"), "{decor}");
    }
    // dropped
    assert_eq!(st("minecraft:water[level=0]").class, Class::Dropped { why: "liquid" });
    assert_eq!(st("minecraft:lava[level=0]").class, Class::Dropped { why: "liquid" });
    assert_eq!(st("minecraft:kelp[age=3]").class, Class::Dropped { why: "plant" });
    assert_eq!(st("minecraft:acacia_sapling[stage=0]").class, Class::Dropped { why: "plant" });
    assert_eq!(st("minecraft:fire_coral_fan[waterlogged=true]").class, Class::Dropped { why: "plant" });
    assert_eq!(st("minecraft:piston_head[facing=east]").class, Class::Dropped { why: "redstone" });
    assert_eq!(st("minecraft:fire").class, Class::Dropped { why: "fire" });
    // keep and air
    assert_eq!(st("minecraft:structure_void").class, Class::Keep);
    assert_eq!(st("minecraft:barrier").class, Class::Keep);
    assert_eq!(st("minecraft:light[level=15]").class, Class::Keep);
    for a in ["air", "cave_air", "void_air"] {
        assert_eq!(st(&format!("minecraft:{a}")).class, Class::Air);
    }
    // whole blocks, with and without namesake; waterlogged is ignored
    assert_eq!(st("minecraft:stripped_warped_hyphae[axis=y]").class, block("stripped_warped"));
    assert_eq!(st("minecraft:stripped_birch_wood[axis=x]").class, block("stripped_birch"));
    assert_eq!(st("minecraft:black_concrete_powder").class, block("black_concrete"));
    assert_eq!(st("minecraft:waxed_oxidized_cut_copper").class, block("copper_block"));
    assert_eq!(
        st("minecraft:waxed_oxidized_cut_copper_stairs[facing=north,half=bottom,shape=straight]").class,
        shaped(
            Shape::Stairs { facing: Facing::North, half: Half::Bottom, corner: StairCorner::Straight },
            "copper_block"
        )
    );
    assert_eq!(st("minecraft:dead_brain_coral_block").class, block("andesite"));
    assert_eq!(
        st("minecraft:smooth_sandstone_slab[type=top,waterlogged=true]").class,
        shaped(Shape::Slab { half: Half::Top }, "smooth_sandstone")
    );
    assert!(st("minecraft:smooth_sandstone").mapped);
}

#[test]
fn unknown_block_falls_back() {
    let c = st("minecraft:definitely_not_a_block");
    assert_eq!(c, Classified { class: block("smooth_stone"), mapped: false });
    let c = st("minecraft:definitely_not_stairs[facing=north,half=bottom,shape=straight]");
    assert_eq!(
        c,
        Classified {
            class: shaped(
                Shape::Stairs { facing: Facing::North, half: Half::Bottom, corner: StairCorner::Straight },
                "smooth_stone"
            ),
            mapped: false
        }
    );
}

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn every_target_block_exists() {
    let src = std::fs::read_to_string(repo().join("assets/blocks/blocks.ron")).unwrap();
    let registry = BlockRegistry::from_ron(&src).unwrap();
    let table = Table::builtin();
    let missing: Vec<&str> = table.target_blocks().into_iter().filter(|b| registry.id_of(b).is_none()).collect();
    assert!(missing.is_empty(), "blocks not in blocks.ron: {missing:?}");
}

/// A state with the first listed value of each property of a census row (`N values` -> "0").
fn default_state(name: &str, properties: &str) -> BlockState {
    let mut props = std::collections::BTreeMap::new();
    for part in properties.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').unwrap();
        let first = if v.ends_with(" values") { "0" } else { v.split('|').next().unwrap() };
        props.insert(k.to_string(), first.to_string());
    }
    BlockState { name: name.to_string(), props }
}

#[test]
fn the_census_is_covered() {
    let table = Table::builtin();
    let census = include_str!("../data/census.tsv");
    let (mut total, mut unmapped_count) = (0u64, 0u64);
    let mut unmapped: Vec<(u64, String)> = Vec::new();
    for line in census.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        let count: u64 = cols[1].parse().unwrap();
        let state = default_state(cols[0], cols.get(4).copied().unwrap_or(""));
        let c = classify(&state, table);
        if c.class != Class::Air {
            total += count;
        }
        if !c.mapped {
            unmapped_count += count;
            unmapped.push((count, cols[0].to_string()));
        }
    }
    unmapped.sort_by(|a, b| b.cmp(a));
    assert!(total > 0);
    assert!(
        (unmapped_count as f64) < total as f64 * 0.005,
        "unmapped {unmapped_count} of {total} non-air cells; by count: {unmapped:?}"
    );
    println!("unmapped names: {} ({unmapped_count}/{total}): {unmapped:?}", unmapped.len());
}

#[test]
fn builtin_table_parses() {
    assert_eq!(Table::builtin().fallback(), "smooth_stone");
    assert!(Table::from_ron("(").is_err());
}
