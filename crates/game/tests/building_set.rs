//! The building set (ids 47..=167): names, order, glass cutout, textures on disk.
use game::{BlockDrop, BlockId, BlockRegistry, RenderMode, TextureSpec};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");

const NAMES: &[&str] = &[
    "smooth_sandstone",
    "cut_sandstone",
    "chiseled_sandstone",
    "red_sandstone",
    "smooth_red_sandstone",
    "quartz",
    "smooth_quartz",
    "quartz_bricks",
    "quartz_pillar",
    "cobblestone",
    "stone_bricks",
    "cracked_stone_bricks",
    "mossy_stone_bricks",
    "smooth_stone",
    "andesite",
    "polished_andesite",
    "diorite",
    "polished_diorite",
    "polished_granite",
    "tuff",
    "calcite",
    "deepslate",
    "cobbled_deepslate",
    "deepslate_bricks",
    "polished_deepslate",
    "bricks",
    "mud_bricks",
    "clay",
    "packed_mud",
    "prismarine",
    "dark_prismarine",
    "bone_block",
    "oak_planks",
    "spruce_planks",
    "birch_planks",
    "acacia_planks",
    "jungle_planks",
    "dark_oak_planks",
    "mangrove_planks",
    "warped_planks",
    "stripped_oak",
    "stripped_spruce",
    "stripped_birch",
    "stripped_acacia",
    "stripped_jungle",
    "stripped_dark_oak",
    "stripped_mangrove",
    "stripped_warped",
    "white_concrete",
    "light_gray_concrete",
    "gray_concrete",
    "black_concrete",
    "brown_concrete",
    "red_concrete",
    "orange_concrete",
    "yellow_concrete",
    "lime_concrete",
    "green_concrete",
    "cyan_concrete",
    "light_blue_concrete",
    "blue_concrete",
    "purple_concrete",
    "magenta_concrete",
    "pink_concrete",
    "terracotta",
    "white_terracotta",
    "light_gray_terracotta",
    "gray_terracotta",
    "black_terracotta",
    "brown_terracotta",
    "red_terracotta",
    "orange_terracotta",
    "yellow_terracotta",
    "lime_terracotta",
    "green_terracotta",
    "cyan_terracotta",
    "light_blue_terracotta",
    "blue_terracotta",
    "purple_terracotta",
    "magenta_terracotta",
    "pink_terracotta",
    "white_wool",
    "light_gray_wool",
    "gray_wool",
    "black_wool",
    "brown_wool",
    "red_wool",
    "orange_wool",
    "yellow_wool",
    "lime_wool",
    "green_wool",
    "cyan_wool",
    "light_blue_wool",
    "blue_wool",
    "purple_wool",
    "magenta_wool",
    "pink_wool",
    "glass",
    "white_stained_glass",
    "light_gray_stained_glass",
    "gray_stained_glass",
    "black_stained_glass",
    "brown_stained_glass",
    "red_stained_glass",
    "orange_stained_glass",
    "yellow_stained_glass",
    "lime_stained_glass",
    "green_stained_glass",
    "cyan_stained_glass",
    "light_blue_stained_glass",
    "blue_stained_glass",
    "purple_stained_glass",
    "magenta_stained_glass",
    "pink_stained_glass",
    "gold_block",
    "iron_block",
    "copper_block",
    "hay_bale",
    "bookshelf",
    "thatch",
    "cobbled_road",
];

fn registry() -> BlockRegistry {
    BlockRegistry::from_ron(MANIFEST).expect("blocks.ron parses")
}

#[test]
fn old_ids_are_unchanged() {
    let reg = registry();
    for (name, id) in [("air", 0), ("stone", 2), ("sandstone", 7), ("oak_log", 13), ("cactus", 45), ("chest", 46)] {
        assert_eq!(reg.id_of(name), Some(BlockId(id)), "{name}");
    }
}

#[test]
fn building_set_is_complete() {
    let reg = registry();
    assert_eq!(NAMES.len(), 121);
    for (i, name) in NAMES.iter().enumerate() {
        assert_eq!(reg.id_of(name), Some(BlockId(47 + i as u16)), "{name}");
    }
    assert_eq!(reg.iter().count(), 168);
}

#[test]
fn glass_is_cutout_and_drops_nothing() {
    let reg = registry();
    let props = reg.props();
    for (i, name) in NAMES.iter().enumerate() {
        let id = BlockId(47 + i as u16);
        let glass = *name == "glass" || name.ends_with("_stained_glass");
        assert_eq!(props.is_cutout(id), glass, "{name}");
        assert_eq!(reg.def(id).unwrap().render == RenderMode::Cutout, glass, "{name}");
        if glass {
            assert_eq!(props.drop(id), BlockDrop::Nothing, "{name}");
        }
    }
}

#[test]
fn every_texture_file_exists() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks");
    let reg = registry();
    for (_, def) in reg.iter() {
        let files: Vec<&str> = match &def.textures {
            TextureSpec::All(p) => vec![p.as_str()],
            TextureSpec::Faces { top, side, bottom } => vec![top.as_str(), side.as_str(), bottom.as_str()],
            TextureSpec::Untextured => vec![],
        };
        for f in files {
            let bytes = std::fs::read(dir.join(f)).unwrap_or_else(|e| panic!("{} texture {f}: {e}", def.name));
            assert!(bytes.len() >= 24 && bytes[..8] == [0x89, b'P', b'N', b'G', 13, 10, 26, 10], "{f} is not a PNG");
            let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
            assert_eq!((w, h), (16, 16), "{f}");
        }
    }
}
